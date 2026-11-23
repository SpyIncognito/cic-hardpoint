//! Hardpoint (hardpoint.exe): the control service with its dashboard in a native window.
//! Without Windows it runs the service and the app's session headless, for testing.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod gui;

use std::process::ExitCode;

use sc_service::{DEFAULT_PORT, ServiceOptions};

const USAGE: &str = "\
usage: hardpoint [--port N] [--page-bezel] [--record] [--no-splash]

  --port N        pages on 127.0.0.1:N (default 47520, or SC_MIP_PORT; 0 picks a free port)
  --page-bezel    leave bezel buttons to the kiosk pages (the browser Gamepad API) instead of
                  reading them natively
  --record        record the game's telemetry (scrubbed) from launch, into the recordings
                  folder in the settings folder, until stopped from the tray or the app quits
  --no-splash     no splash at launch (also \"splash\": false in app-settings.json in the
                  settings folder)";

#[derive(Debug)]
pub struct Args {
    pub port: u16,
    pub native_bezel: bool,
    pub record: bool,
    /// Show the splash at launch (Windows only; the settings file can also turn it off).
    pub splash: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut port = None;
    let mut native_bezel = true;
    let mut record = false;
    let mut splash = true;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                let v = it.next().ok_or("--port needs a value")?;
                port = Some(v.parse().map_err(|_| "--port must be a port number")?);
            }
            "--page-bezel" => native_bezel = false,
            "--record" => record = true,
            "--no-splash" => splash = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let port = port.unwrap_or_else(|| {
        std::env::var("SC_MIP_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .filter(|&p| p != 0)
            .unwrap_or(DEFAULT_PORT)
    });
    Ok(Args {
        port,
        native_bezel,
        record,
        splash,
    })
}

/// The service's options.
pub fn service_options(args: &Args) -> Result<ServiceOptions, String> {
    Ok(ServiceOptions {
        port: args.port,
        ..ServiceOptions::standard()
    })
}

fn main() -> ExitCode {
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
    #[cfg(windows)]
    return gui::run(args);
    #[cfg(not(windows))]
    return headless::run(args);
}

#[cfg(not(windows))]
mod headless {
    use std::process::ExitCode;
    use std::sync::Arc;

    use sc_app::client;
    use sc_app::instance::{self, Launch};
    use sc_app::recorder::{self, DEFAULT_CAP, Recorder, StopReason};
    use sc_service::Service;
    use sc_service::paths::{self, Env};

    use crate::Args;

    #[tokio::main]
    pub async fn run(args: Args) -> ExitCode {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_target(false)
            .init();
        let env = Env::current();
        paths::log_migration(&env, &paths::migrate_default_settings(&env));
        let settings = paths::config_dir(&env);
        let guard = match instance::launch(&settings) {
            Ok(Launch::First(guard)) => guard,
            Ok(Launch::Second) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        guard.serve(|| tracing::info!("another launch asked for the dashboard"));
        let options = match crate::service_options(&args) {
            Ok(options) => options,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        let service = match Service::start(options).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("control service could not start: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "Hardpoint running headless: http://127.0.0.1:{}/dash (windows need Windows)",
            service.port()
        );
        let state = client::Shared::default();
        let (_out, outgoing) = tokio::sync::mpsc::unbounded_channel();
        let (close, closing) = tokio::sync::watch::channel(false);
        let watched = Arc::clone(&state);
        let session = tokio::spawn(client::run(
            service.port(),
            service.token().as_str().to_owned(),
            Arc::clone(&state),
            outgoing,
            move |e| {
                if e == client::Event::Dashboard {
                    let placed = client::lock(&watched)
                        .tabs
                        .iter()
                        .filter(|t| t.placement.is_some())
                        .count();
                    tracing::info!("dashboard: {placed} tab(s) with a display");
                }
            },
            closing,
        ));
        let recording = if args.record {
            let core = service.core();
            let actions = core.actions().await;
            let folder = recorder::folder(&settings);
            match Recorder::start(core.telemetry(), &folder, &actions, DEFAULT_CAP, |s| {
                tracing::warn!("telemetry recording stopped: {}", s.why.text());
            }) {
                Ok(r) => {
                    service.set_recording(true);
                    tracing::info!("recording telemetry to {}", r.path().display());
                    Some(r)
                }
                Err(e) => {
                    tracing::error!("telemetry recording could not start: {e}");
                    None
                }
            }
        } else {
            None
        };
        let _ = tokio::signal::ctrl_c().await;
        if let Some(r) = recording {
            let s = r.stop(StopReason::Quit);
            tracing::info!(
                "wrote {} frames to {} ({})",
                s.frames,
                s.path.display(),
                s.why.text()
            );
        }
        let _ = close.send(true);
        let _ = session.await;
        service.close().await;
        ExitCode::SUCCESS
    }
}
