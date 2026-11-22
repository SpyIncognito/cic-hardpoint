use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tokio::sync::watch;

use crate::{
    lock,
    types::{BoxFuture, Frame, FrameStream, GameAction, GameApi, TriggerResult, TriggerStatus},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedFrame {
    /// Milliseconds on the recorder's clock.
    pub t: f64,
    pub frame: Frame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerCall {
    pub name: String,
    pub hold_ms: Option<u32>,
}

/// What [`ReplayGame::trigger_action`](GameApi::trigger_action) answers.
#[derive(Clone)]
pub enum TriggerResponder {
    Fixed(TriggerResult),
    With(Arc<dyn Fn(&TriggerCall) -> TriggerResult + Send + Sync>),
}

impl TriggerResponder {
    pub fn with(respond: impl Fn(&TriggerCall) -> TriggerResult + Send + Sync + 'static) -> Self {
        Self::With(Arc::new(respond))
    }

    fn respond(&self, call: &TriggerCall) -> TriggerResult {
        match self {
            Self::Fixed(result) => result.clone(),
            Self::With(respond) => respond(call),
        }
    }
}

impl fmt::Debug for TriggerResponder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(result) => f.debug_tuple("Fixed").field(result).finish(),
            Self::With(_) => f.write_str("With(..)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    /// Playback speed: 2 plays twice as fast.
    pub speed: f64,
    /// Start over after the last frame instead of ending the stream.
    pub loop_playback: bool,
    pub actions: Vec<GameAction>,
    pub trigger_result: TriggerResponder,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            speed: 1.0,
            loop_playback: false,
            actions: Vec::new(),
            trigger_result: TriggerResponder::Fixed(TriggerResult::new(TriggerStatus::Ok, "")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("speed must be positive, got {0}")]
pub struct ReplayError(pub f64);

/// Gap between the last frame and the first when looping, if the recording has one frame.
const DEFAULT_FRAME_GAP_MS: f64 = 1000.0 / 17.0;

/// A [`GameApi`] that plays a recording on its original timing: for tests and for running
/// without the game.
pub struct ReplayGame {
    recording: Arc<Recording>,
    actions: Vec<GameAction>,
    trigger_result: TriggerResponder,
    trigger_calls: Mutex<Vec<TriggerCall>>,
    /// Each send ends the streams open at the time.
    close_signal: watch::Sender<()>,
}

struct Recording {
    frames: Vec<RecordedFrame>,
    speed: f64,
    loop_playback: bool,
    loop_gap_ms: f64,
}

impl Recording {
    /// The frame to play after `index`, and how long to wait for it.
    fn after(&self, index: usize) -> Option<(usize, Duration)> {
        let current = &self.frames[index];
        match self.frames.get(index + 1) {
            Some(next) => Some((index + 1, self.scaled((next.t - current.t).max(0.0)))),
            None if self.loop_playback => Some((0, self.scaled(self.loop_gap_ms))),
            None => None,
        }
    }

    fn scaled(&self, ms: f64) -> Duration {
        Duration::try_from_secs_f64(ms / self.speed / 1000.0).unwrap_or(Duration::MAX)
    }
}

impl ReplayGame {
    pub fn new(frames: Vec<RecordedFrame>, options: ReplayOptions) -> Result<Self, ReplayError> {
        if options.speed.is_nan() || options.speed <= 0.0 {
            return Err(ReplayError(options.speed));
        }
        let loop_gap_ms = match (frames.first(), frames.last()) {
            (Some(first), Some(last)) if frames.len() > 1 => {
                (last.t - first.t) / (frames.len() - 1) as f64
            }
            _ => DEFAULT_FRAME_GAP_MS,
        };
        let recording = Recording {
            frames,
            speed: options.speed,
            loop_playback: options.loop_playback,
            loop_gap_ms,
        };
        Ok(Self {
            recording: Arc::new(recording),
            actions: options.actions,
            trigger_result: options.trigger_result,
            trigger_calls: Mutex::new(Vec::new()),
            close_signal: watch::Sender::new(()),
        })
    }

    /// Every trigger call so far, in order.
    pub fn trigger_calls(&self) -> Vec<TriggerCall> {
        lock(&self.trigger_calls).clone()
    }
}

impl GameApi for ReplayGame {
    fn open_stream(&self) -> FrameStream {
        let start = (
            self.recording.clone(),
            Some((0, Duration::ZERO)),
            self.close_signal.subscribe(),
        );
        Box::pin(stream::unfold(
            start,
            |(recording, next, mut closed)| async move {
                let (index, delay) = next?;
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    // A dropped game is not a close: its streams keep playing.
                    Ok(()) = closed.changed() => return None,
                }
                let frame = recording.frames.get(index)?.frame.clone();
                let next = recording.after(index);
                Some((Ok(frame), (recording, next, closed)))
            },
        ))
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async move { self.actions.clone() })
    }

    fn trigger_action(&self, name: &str, hold_ms: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        let call = TriggerCall {
            name: name.to_owned(),
            hold_ms,
        };
        let result = self.trigger_result.respond(&call);
        lock(&self.trigger_calls).push(call);
        Box::pin(async move { result })
    }

    fn close(&self) {
        self.close_signal.send_replace(());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("recording line {line}: not JSON ({source})")]
    NotJson {
        line: usize,
        source: serde_json::Error,
    },
    #[error("recording line {line}: expected {{ \"t\": number, \"frame\": object }}")]
    Shape { line: usize },
}

/// Parses a recording made by tools/record.ts: one `{ "t": ms, "frame": {...} }` per line.
pub fn parse_recording(jsonl: &str) -> Result<Vec<RecordedFrame>, RecordingError> {
    let mut frames = Vec::new();
    for (index, text) in jsonl.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let line = index + 1;
        let entry: Json = serde_json::from_str(text)
            .map_err(|source| RecordingError::NotJson { line, source })?;
        let t = entry.get("t").and_then(Json::as_f64);
        let frame = entry.get("frame").filter(|frame| frame.is_object());
        let (Some(t), Some(frame)) = (t, frame) else {
            return Err(RecordingError::Shape { line });
        };
        frames.push(RecordedFrame {
            t,
            frame: frame.clone(),
        });
    }
    Ok(frames)
}
