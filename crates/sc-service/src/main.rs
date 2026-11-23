//! hardpoint-service: the control service without the desktop app. Starts the core on the game's local telemetry API, the bus the
//! Stream Deck plugin uses, and the portal and dashboard pages, then runs until Ctrl+C.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use sc_game::{GameApi, GameClient, GameClientOptions, ReplayGame, ReplayOptions, parse_recording};
use sc_service::paths;
use sc_service::{DEFAULT_PORT, Service, ServiceOptions};

const USAGE: &str = "\
usage: hardpoint-service [--port N] [--game HOST:PORT | --replay FILE.jsonl]

  --port N        pages on 127.0.0.1:N (default 47520, or SC_MIP_PORT; 0 picks a free port)
  --game ADDRESS  the game's telemetry server (default the local game, or SC_GAME_ADDRESS)
  --replay FILE   play a recording in a loop instead of the game

Settings and the bus file are where the TypeScript service keeps them (SC_CONTROL_BUS_FILE
overrides the bus file).";

#[derive(Debug, Default)]
struct Args {
    port: Option<u16>,
    game: Option<String>,
    replay: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--port" => {
                args.port = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--port must be a port number".to_owned())?,
                )
            }
            "--game" => args.game = Some(value()?),
            "--replay" => args.replay = Some(value()?.into()),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

/// The game to follow: a recording, another telemetry server, or the local game.
fn game(args: &Args) -> Result<Option<Arc<dyn GameApi>>, String> {
    if let Some(file) = &args.replay {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let frames = parse_recording(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        let options = ReplayOptions {
            loop_playback: true,
            ..ReplayOptions::default()
        };
        let replay = ReplayGame::new(frames, options).map_err(|e| e.to_string())?;
        return Ok(Some(Arc::new(replay)));
    }
    let address = args.game.clone().or_else(|| {
        std::env::var("SC_GAME_ADDRESS")
            .ok()
            .filter(|a| !a.is_empty())
    });
    let Some(address) = address else {
        return Ok(None);
    };
    let client = GameClient::new(GameClientOptions {
        address,
        ..GameClientOptions::default()
    })
    .map_err(|e| e.to_string())?;
    Ok(Some(Arc::new(client)))
}

/// As the TS service: SC_MIP_PORT when it is a port other than 0, the default otherwise.
fn port(args: &Args) -> u16 {
    args.port.unwrap_or_else(|| {
        std::env::var("SC_MIP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .filter(|&p| p != 0)
            .unwrap_or(DEFAULT_PORT)
    })
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let env = paths::Env::current();
    paths::log_migration(&env, &paths::migrate_default_settings(&env));
    let args = match parse_args() {
        Ok(args) => args,
        Err(problem) => {
            if !problem.is_empty() {
                eprintln!("{problem}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let game = match game(&args) {
        Ok(game) => game,
        Err(problem) => {
            eprintln!("{problem}");
            return ExitCode::FAILURE;
        }
    };
    let options = ServiceOptions {
        port: port(&args),
        game,
        ..ServiceOptions::standard()
    };
    let service = match Service::start(options).await {
        Ok(service) => service,
        Err(e) => {
            eprintln!("control service could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let port = service.port();
    println!(
        "Hardpoint service running (bus port {}, pages on 127.0.0.1:{port})",
        service.bus_port()
    );
    println!("  portal: http://127.0.0.1:{port}/portal");
    println!("  dashboard: http://127.0.0.1:{port}/dash");

    shutdown_signal().await;
    service.close().await;
    ExitCode::SUCCESS
}
