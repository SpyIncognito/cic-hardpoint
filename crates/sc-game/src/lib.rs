//! The game side of the Hardpoint core: the local telemetry API (a stream of frames, a list of
//! actions, a trigger call) behind one trait, so the state model never touches gRPC and tests
//! can swap in a fake or a recording. Ported from packages/sc-control-core/src/game.

mod client;
mod link;
mod replay;
pub mod schema;
mod scrub;
mod types;

pub use client::{GameClient, GameClientOptions, InvalidAddress};
pub use link::{LinkOptions, ListenerError, Subscription, TelemetryLink};
pub use replay::{
    RecordedFrame, RecordingError, ReplayError, ReplayGame, ReplayOptions, TriggerCall,
    TriggerResponder, parse_recording,
};
pub use schema::DEFAULT_ADDRESS;
pub use scrub::scrub_frame;
pub use types::{
    BoxFuture, Frame, FrameStream, GameAction, GameApi, LinkStatus, StreamError, TriggerResult,
    TriggerStatus,
};

/// The guarded values stay consistent even if a holder panicked, so poisoning is ignored.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
