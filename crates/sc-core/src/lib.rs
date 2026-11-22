//! The Hardpoint core: telemetry state in, game actions out. Ported from
//! `packages/sc-control-core`; frames are `serde_json::Value` in the TypeScript client's shape.

mod control;
pub mod engine;
pub mod js;
mod listeners;
pub mod state;

pub use control::{ControlCore, ControlCoreOptions};
pub use listeners::ListenerGuard;
