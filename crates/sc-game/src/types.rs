use std::{future::Future, pin::Pin};

use futures_util::Stream;
use serde::{Deserialize, Serialize};

/// One decoded `TelemetryStreamResponse` as a JSON object: field names as in the schema
/// (snake_case), enums as their names (e.g. "VEHICLE_MASTER_MODE_SCM"), 64-bit integers as
/// strings. Absent fields are absent: proto3 does not send zeros, false or empty values, and a
/// frame may carry only some of its top-level sections.
pub type Frame = serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameAction {
    /// The game's action name, e.g. "v_toggle_landing_system".
    pub name: String,
    /// The game's category, e.g. "spaceship_movement".
    pub category: String,
    pub holdable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TriggerStatus {
    Ok,
    UnknownAction,
    MapInactive,
    RateLimited,
    Failed,
    HoldNotAllowed,
    Filtered,
    Unspecified,
    /// Not from the game: the call did not reach it (game closed, telemetry off, timeout).
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerResult {
    pub status: TriggerStatus,
    pub message: String,
}

impl TriggerResult {
    pub fn new(status: TriggerStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// Why a telemetry stream ended other than cleanly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub message: String,
}

impl StreamError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<tonic::Status> for StreamError {
    fn from(status: tonic::Status) -> Self {
        Self::new(format!("{:?}: {}", status.code(), status.message()))
    }
}

/// Frames until the stream ends: `None` is a clean end, an `Err` item is the last one.
/// Dropping the stream cancels it.
pub type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame, StreamError>> + Send>>;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The game, live or recorded. Calls never fail: an unreachable game lists no actions and
/// answers triggers with [`TriggerStatus::Unreachable`].
pub trait GameApi: Send + Sync {
    fn open_stream(&self) -> FrameStream;
    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>>;
    fn trigger_action(&self, name: &str, hold_ms: Option<u32>) -> BoxFuture<'_, TriggerResult>;
    fn close(&self);
}

/// Whether the core currently hears the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    /// Frames are arriving.
    Live,
    /// The stream is open or being reopened, but no frame has arrived recently.
    Waiting,
    /// Nothing is listening for frames (no subscribers).
    Idle,
}
