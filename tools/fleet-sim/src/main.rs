//! `fleet-sim`: demo fleet members against a CIC relay, for internal testing. Not part of the
//! shipped app.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use fleet_sim::invite::{self, HostedInvite};
use fleet_sim::scenario::Ship;
use fleet_sim::{MAX_MEMBERS, Options, Sim, recording};

const USAGE: &str = "\
fleet-sim: demo fleet members for testing a CIC relay (not part of Hardpoint)

Usage: fleet-sim [options]

Peer to peer (a host player's app), one invite for every member:
  --invite <cic://...&code=...>
                             the event's invite (or FLEET_SIM_INVITE)
  --count <n>                members, 1 to 64 (or FLEET_SIM_COUNT; default 8)

Hosted (a CIC relay), one connection code per member, each for its own CIC account:
  --connect-file <path>      one cic://...&connect=... per line, member 1 first; blank lines
                             and lines starting with # are skipped (or FLEET_SIM_CONNECT_FILE)
  --connect <cic://...>      one member's connection code; repeat for more (a file keeps the
                             codes out of the process list)
  --event <id>               the event to choose when the relay offers a list; else the first

Either way:
  --wait                     with no invite, idle and wait instead of exiting
  --recordings <path>...     recording files (.jsonl) or folders of them (or FLEET_SIM_RECORDINGS,
                             separated by ':'); read at start, never built in
  --seed <n>                 spreads recording offsets and picks the scripted members
                             (or FLEET_SIM_SEED; default 1)
  --handle-prefix <text>     handles are '<prefix> Vega', '<prefix> Rook', ...
                             (or FLEET_SIM_HANDLE_PREFIX; default Demo)
  --data <dir>               each member's key and files (or FLEET_SIM_DATA; default fleet-sim-data)
  --cycle-secs <n>           the scripted downed, dead and respawn cycle (default 180)
  --status-secs <n>          how often to log the counts (default 30)
  --ship <CLASS:seats>       a ship and its seats in order, such as
                             RSI_Constellation_Andromeda:pilot,copilot,crew; repeat for more
                             ships. Members take the seats from member 1; the rest keep their
                             recording's. Seat them the same way in the CIC portal
  --scripted <n>             members on the downed, dead and respawn script (default a third)
  --pilot-drop-secs <n>      each seated pilot goes off line for n seconds halfway through
                             every cycle, so the relay falls back to the copilot or crew

Peer to peer tests:
  --consent <fields>         what every member sends: all (the default), none, or field ids
                             separated by commas, such as ship.hull,ship.name
  --received-log <path>      at every status tick, one JSON line: per member its link, its
                             refusal code, and the cards it receives as member ids with their
                             field ids (never handles or values)
  --step <SECS:ACTION:N>     at SECS after start member N (from 1) does ACTION: drop (off line,
                             not leaving), rejoin (by key), leave; or SECS:join:N:<invite> joins
                             with that invite (an old code, to see the refusal). Repeat for more

Exit codes: 0 stopped, 1 an error, 2 bad arguments; when every member is refused, 3 if any was
banned (kicked by the host), 4 if any was removed (kicked), else 5.

Logs counts and states only, never an invite or a token. SIGTERM or Ctrl+C disconnects every
member without leaving, so a restart rejoins the event by key.
";

struct Args {
    invite: Option<String>,
    connect: Vec<HostedInvite>,
    connect_file: Option<PathBuf>,
    choose: Option<String>,
    ships: Vec<Ship>,
    scripted: Option<usize>,
    pilot_drop: Option<Duration>,
    wait: bool,
    recordings: Vec<PathBuf>,
    count: usize,
    seed: u64,
    handle_prefix: String,
    data: PathBuf,
    cycle: Duration,
    status: Duration,
    consent: Option<fleet_proto::SendConsent>,
    received_log: Option<PathBuf>,
    steps: Vec<fleet_sim::steps::Step>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn number<T: std::str::FromStr>(name: &str, text: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("{name} must be a number, not {text:?}"))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        invite: env("FLEET_SIM_INVITE"),
        connect: Vec::new(),
        connect_file: env("FLEET_SIM_CONNECT_FILE").map(PathBuf::from),
        choose: None,
        ships: Vec::new(),
        scripted: None,
        pilot_drop: None,
        wait: false,
        recordings: env("FLEET_SIM_RECORDINGS")
            .map(|v| v.split(':').map(PathBuf::from).collect())
            .unwrap_or_default(),
        count: env("FLEET_SIM_COUNT")
            .map(|v| number("FLEET_SIM_COUNT", &v))
            .transpose()?
            .unwrap_or(8),
        seed: env("FLEET_SIM_SEED")
            .map(|v| number("FLEET_SIM_SEED", &v))
            .transpose()?
            .unwrap_or(1),
        handle_prefix: env("FLEET_SIM_HANDLE_PREFIX").unwrap_or_else(|| "Demo".to_owned()),
        data: env("FLEET_SIM_DATA").map_or_else(|| PathBuf::from("fleet-sim-data"), PathBuf::from),
        cycle: Duration::from_secs(180),
        status: Duration::from_secs(30),
        consent: None,
        received_log: None,
        steps: Vec::new(),
    };
    let mut from_cli = Vec::new();
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--invite" => args.invite = Some(value()?),
            "--connect" => args.connect.push(
                value()?
                    .parse()
                    .map_err(|e| format!("--connect {}: {e}", args.connect.len() + 1))?,
            ),
            "--connect-file" => args.connect_file = Some(PathBuf::from(value()?)),
            "--event" => {
                let event = value()?;
                if !fleet_proto::structure::is_id(&event) {
                    return Err(format!("--event {event:?} is not an event id"));
                }
                args.choose = Some(event);
            }
            "--ship" => args
                .ships
                .push(value()?.parse().map_err(|e| format!("--ship: {e}"))?),
            "--scripted" => args.scripted = Some(number("--scripted", &value()?)?),
            "--pilot-drop-secs" => {
                args.pilot_drop =
                    Some(Duration::from_secs(number("--pilot-drop-secs", &value()?)?));
            }
            "--wait" => args.wait = true,
            "--recordings" => {
                from_cli.push(PathBuf::from(value()?));
                while let Some(more) = it.next_if(|a| !a.starts_with("--")) {
                    from_cli.push(PathBuf::from(more));
                }
            }
            "--count" => args.count = number("--count", &value()?)?,
            "--seed" => args.seed = number("--seed", &value()?)?,
            "--handle-prefix" => args.handle_prefix = value()?.trim().to_owned(),
            "--data" => args.data = PathBuf::from(value()?),
            "--cycle-secs" => args.cycle = Duration::from_secs(number("--cycle-secs", &value()?)?),
            "--status-secs" => {
                args.status = Duration::from_secs(number("--status-secs", &value()?)?);
            }
            "--consent" => args.consent = Some(fleet_sim::steps::parse_consent(&value()?)?),
            "--received-log" => args.received_log = Some(PathBuf::from(value()?)),
            "--step" => args
                .steps
                .push(value()?.parse().map_err(|e| format!("--step {e}"))?),
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if !from_cli.is_empty() {
        args.recordings = from_cli;
    }
    if !(1..=MAX_MEMBERS).contains(&args.count) {
        return Err(format!("--count must be 1 to {MAX_MEMBERS}"));
    }
    if args.cycle.is_zero() || args.status.is_zero() {
        return Err("--cycle-secs and --status-secs must be above 0".to_owned());
    }
    if args
        .pilot_drop
        .is_some_and(|d| d.is_zero() || d >= args.cycle / 2)
    {
        return Err("--pilot-drop-secs must be above 0 and under half of --cycle-secs".to_owned());
    }
    if let Some(file) = &args.connect_file {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("--connect-file {}: {e}", file.display()))?;
        let mut from_file = invite::parse_lines(&text)
            .map_err(|e| format!("--connect-file {}, {e}", file.display()))?;
        from_file.append(&mut args.connect);
        args.connect = from_file;
    }
    if !args.connect.is_empty() {
        if args.invite.is_some() {
            return Err(
                "pass either --invite or connection codes (--connect, --connect-file), not both"
                    .to_owned(),
            );
        }
        if args.connect.len() > MAX_MEMBERS {
            return Err(format!(
                "at most {MAX_MEMBERS} connection codes, one per member"
            ));
        }
    } else if let Some(i) = args.invite.as_deref().filter(|i| invite::is_hosted(i)) {
        args.connect
            .push(i.parse().map_err(|e| format!("--invite: {e}"))?);
        args.invite = None;
    }
    Ok(args)
}

/// SIGTERM (a container stopping) or Ctrl+C. The handlers are installed on the call, before
/// the future is first polled, so a signal during start-up is not missed.
fn shutdown() -> impl Future<Output = ()> {
    #[cfg(unix)]
    let term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let ctrl_c = tokio::signal::ctrl_c();
    async move {
        #[cfg(unix)]
        if let Some(mut term) = term {
            tokio::select! {
                _ = term.recv() => {}
                _ = ctrl_c => {}
            }
            return;
        }
        let _ = ctrl_c.await;
    }
}

async fn run(args: Args) -> Result<ExitCode, String> {
    let stop = shutdown();
    tokio::pin!(stop);
    if args.invite.is_none() && args.connect.is_empty() {
        if !args.wait {
            return Err("no invite: pass --invite, --connect or --connect-file".to_owned());
        }
        tracing::info!(
            "waiting for an invite (set FLEET_SIM_INVITE or FLEET_SIM_CONNECT_FILE and restart)"
        );
        stop.await;
        return Ok(ExitCode::SUCCESS);
    }
    if args.recordings.is_empty() {
        return Err("no recordings: pass --recordings or set FLEET_SIM_RECORDINGS".to_owned());
    }
    let mut options = match args.invite {
        Some(invite) => {
            let mut o = Options::new(invite, Vec::new(), args.data);
            o.count = args.count;
            o
        }
        None => Options::hosted(args.connect, Vec::new(), args.data),
    };
    let frame_ms = options.frame_every.as_secs_f64() * 1000.0;
    let paths = args.recordings;
    let loading = tokio::task::spawn_blocking(move || recording::load(&paths, frame_ms));
    let recordings = tokio::select! {
        () = &mut stop => return Ok(ExitCode::SUCCESS),
        loaded = loading => loaded.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?,
    };
    let frames: usize = recordings.iter().map(|r| r.len()).sum();
    tracing::info!("{} recordings read, {frames} frames kept", recordings.len());
    options.recordings = recordings;
    options.choose = args.choose;
    options.ships = args.ships;
    options.scripted = args.scripted;
    options.pilot_drop = args.pilot_drop;
    options.seed = args.seed;
    options.handle_prefix = args.handle_prefix;
    options.cycle = args.cycle;
    if let Some(consent) = args.consent {
        options.consent = consent;
    }
    let mut sim = Sim::start(options).map_err(|e| e.to_string())?;
    let scripted = sim.members().iter().filter(|m| m.scripted).count();
    let seated = sim.members().iter().filter(|m| m.seating.is_some()).count();
    tracing::info!(
        "{} members started ({}), {scripted} on the downed, dead and respawn script, {seated} seated by --ship",
        sim.members().len(),
        if sim.members().iter().any(|m| m.hosted().is_some()) {
            "hosted"
        } else {
            "peer to peer"
        },
    );
    let started = std::time::Instant::now();
    let mut steps = args.steps;
    steps.sort_by_key(|s| s.at);
    let mut steps = steps.into_iter().peekable();
    let mut log = match &args.received_log {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("--received-log {}: {e}", path.display()))?,
        ),
        None => None,
    };
    let mut status = tokio::time::interval(args.status);
    let mut tend = tokio::time::interval(Duration::from_secs(1));
    let code = loop {
        tokio::select! {
            () = &mut stop => break ExitCode::SUCCESS,
            _ = status.tick() => {
                tracing::info!("{}", sim.counts());
                if let Some(file) = log.as_mut() {
                    use std::io::Write;
                    let line = sim.received_line(started.elapsed().as_millis());
                    if let Err(e) = writeln!(file, "{line}") {
                        tracing::warn!("the received-cards log: {e}");
                    }
                }
            }
            _ = tend.tick() => {
                while let Some(step) = steps.next_if(|s| s.at <= started.elapsed()) {
                    use fleet_sim::steps::Action;
                    let n = step.member;
                    let done = match &step.action {
                        Action::Drop => sim.drop_member(n).await,
                        Action::Rejoin => sim.rejoin_member(n),
                        Action::Leave => sim.leave_member(n).await,
                        Action::Join(invite) => sim.join_member(n, invite),
                    };
                    match done {
                        Ok(()) => tracing::info!("step at {} s: member {} {}", step.at.as_secs(), n + 1, step.action_name()),
                        Err(e) => tracing::warn!("step at {} s, member {}: {e}", step.at.as_secs(), n + 1),
                    }
                }
                sim.tend().await;
                if steps.peek().is_none() && let Some(why) = sim.all_refused() {
                    tracing::info!("every member is refused ({why:?})");
                    break match why {
                        Some(fleet_proto::RefusalCode::Banned) => ExitCode::from(3),
                        Some(fleet_proto::RefusalCode::Kicked) => ExitCode::from(4),
                        _ => ExitCode::from(5),
                    };
                }
            }
        }
    };
    tracing::info!("stopping: disconnecting {} members", sim.members().len());
    sim.close().await;
    Ok(code)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,sc_service=warn,sc_core=warn,sc_game=warn")
            }),
        )
        .init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("fleet-sim: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fleet-sim: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}
