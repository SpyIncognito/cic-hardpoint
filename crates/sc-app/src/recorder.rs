//! The telemetry recorder: the game's frames to a JSONL file in the same format as the Node
//! recorder (packages/sc-control-core/tools/record.ts), one `{"t": ms since the first frame,
//! "frame": {...}}` per line, plus the game's action list beside it as `<name>.actions.json`.
//! Frames are always scrubbed. It is one more subscriber on the app's telemetry link.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sc_game::{GameAction, Subscription, TelemetryLink, scrub_frame};
use sc_service::json::{to_pretty_text, to_text};
use serde_json::json;

/// The largest recording, in bytes; recording stops before a frame would go over it.
pub const DEFAULT_CAP: u64 = 1 << 30;
const FLUSH_EVERY: Duration = Duration::from_secs(1);
const BUFFER: usize = 64 * 1024;

/// The recordings folder inside the app's settings folder.
pub fn folder(settings: &Path) -> PathBuf {
    settings.join("recordings")
}

/// `telemetry-<ISO time with : and . replaced by ->.jsonl`, as the Node recorder names its files.
pub fn file_name(at: SystemTime) -> String {
    format!("telemetry-{}.jsonl", iso_time(at).replace([':', '.'], "-"))
}

/// `Date.prototype.toISOString`: UTC with milliseconds, e.g. `2026-11-22T09:05:03.042Z`.
fn iso_time(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// The date `days` after 1970-01-01 (Howard Hinnant's civil_from_days).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Stopped from the tray.
    Asked,
    /// The app quit.
    Quit,
    /// The next frame would have gone over the cap.
    Cap(u64),
    /// The file could not be written.
    Failed(String),
}

impl StopReason {
    /// True when the recorder stopped by itself, not because it was told to.
    pub fn is_unasked(&self) -> bool {
        matches!(self, Self::Cap(_) | Self::Failed(_))
    }

    pub fn text(&self) -> String {
        match self {
            Self::Asked => "stopped".to_owned(),
            Self::Quit => "the app quit".to_owned(),
            Self::Cap(cap) => format!("it reached the {} MiB cap", cap / (1024 * 1024)),
            Self::Failed(e) => format!("the file could not be written ({e})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub path: PathBuf,
    pub frames: u64,
    pub bytes: u64,
    pub why: StopReason,
}

enum Msg {
    Line(String),
    Stop(StopReason),
}

/// A recording in progress; dropping it stops it as [`StopReason::Quit`].
pub struct Recorder {
    path: PathBuf,
    subscription: Option<Subscription>,
    tx: mpsc::Sender<Msg>,
    writer: Option<JoinHandle<Summary>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Writes the action list, then records every frame from `link` into a new file in
    /// `folder`. Waits for the game if it is not streaming yet; `t` starts at the first frame.
    /// `on_end` hears of a stop the recorder made by itself (the cap, a failed write).
    ///
    /// # Panics
    /// Outside a tokio runtime (the link's listeners run on it).
    pub fn start(
        link: &TelemetryLink,
        folder: &Path,
        actions: &[GameAction],
        cap: u64,
        on_end: impl FnOnce(&Summary) + Send + 'static,
    ) -> io::Result<Self> {
        fs::create_dir_all(folder)?;
        let path = folder.join(file_name(SystemTime::now()));
        let actions_path = path.with_extension("actions.json");
        fs::write(&actions_path, to_pretty_text(&actions) + "\n")?;
        let file = File::create(&path)?;

        let (tx, rx) = mpsc::channel();
        let frames = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let writer = {
            let (path, frames, bytes) = (path.clone(), Arc::clone(&frames), Arc::clone(&bytes));
            std::thread::Builder::new()
                .name("telemetry recorder".to_owned())
                .spawn(move || {
                    let why = write_lines(file, &rx);
                    let summary = Summary {
                        path,
                        frames: frames.load(Ordering::Relaxed),
                        bytes: bytes.load(Ordering::Relaxed),
                        why,
                    };
                    if summary.why.is_unasked() {
                        on_end(&summary);
                    }
                    summary
                })?
        };

        let started = OnceLock::<Instant>::new();
        let stopped = AtomicBool::new(false);
        let sender = tx.clone();
        let subscription = link.subscribe(move |frame| {
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            let t = started.get_or_init(Instant::now).elapsed().as_millis();
            let line = to_text(&json!({ "t": t, "frame": scrub_frame(frame) }));
            let size = line.len() as u64 + 1;
            if bytes.load(Ordering::Relaxed) + size > cap {
                stopped.store(true, Ordering::Relaxed);
                let _ = sender.send(Msg::Stop(StopReason::Cap(cap)));
                return;
            }
            bytes.fetch_add(size, Ordering::Relaxed);
            frames.fetch_add(1, Ordering::Relaxed);
            if sender.send(Msg::Line(line)).is_err() {
                stopped.store(true, Ordering::Relaxed);
            }
        });
        Ok(Self {
            path,
            subscription: Some(subscription),
            tx,
            writer: Some(writer),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops, flushes and closes the file.
    pub fn stop(mut self, why: StopReason) -> Summary {
        self.finish(why).expect("a recorder finishes once")
    }

    fn finish(&mut self, why: StopReason) -> Option<Summary> {
        let writer = self.writer.take()?;
        self.subscription = None;
        let _ = self.tx.send(Msg::Stop(why.clone()));
        Some(writer.join().unwrap_or_else(|_| Summary {
            path: self.path.clone(),
            frames: 0,
            bytes: 0,
            why: StopReason::Failed("the recorder thread panicked".to_owned()),
        }))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish(StopReason::Quit);
    }
}

/// Writes lines until told to stop, flushing at least every [`FLUSH_EVERY`].
fn write_lines(file: File, rx: &mpsc::Receiver<Msg>) -> StopReason {
    let mut out = BufWriter::with_capacity(BUFFER, file);
    let mut next_flush = Instant::now() + FLUSH_EVERY;
    let why = loop {
        let wait = next_flush.saturating_duration_since(Instant::now());
        let written = match rx.recv_timeout(wait) {
            Ok(Msg::Line(line)) => out
                .write_all(line.as_bytes())
                .and_then(|()| out.write_all(b"\n")),
            Ok(Msg::Stop(why)) => break why,
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => break StopReason::Quit,
        };
        if let Err(e) = written {
            break StopReason::Failed(e.to_string());
        }
        if Instant::now() >= next_flush {
            if let Err(e) = out.flush() {
                break StopReason::Failed(e.to_string());
            }
            next_flush = Instant::now() + FLUSH_EVERY;
        }
    };
    match out.flush() {
        Err(e) if !matches!(why, StopReason::Failed(_)) => StopReason::Failed(e.to_string()),
        _ => why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_files_as_the_node_recorder() {
        let at = UNIX_EPOCH + Duration::from_millis(1_795_338_303_042);
        assert_eq!(iso_time(at), "2026-11-22T09:05:03.042Z");
        assert_eq!(file_name(at), "telemetry-2026-11-22T09-05-03-042Z.jsonl");
        assert_eq!(iso_time(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(iso_time(leap), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn the_actions_file_sits_beside_the_recording() {
        let path = Path::new("r").join(file_name(UNIX_EPOCH));
        assert_eq!(
            path.with_extension("actions.json"),
            Path::new("r").join("telemetry-1970-01-01T00-00-00-000Z.actions.json")
        );
    }
}
