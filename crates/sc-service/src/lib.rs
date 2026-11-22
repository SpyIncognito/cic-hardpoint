//! The Hardpoint service: one control core for every device. It serves the core to other
//! processes over the local bus (the Stream Deck plugin), and serves the portal and dashboard
//! pages on 127.0.0.1. Ported from packages/sc-control-service; the wire contract is PROTOCOL.md.
//!
//! The MFD pages (`/mfd`, `/mip`) are not ported: the dashboard replaces them.

pub mod bus;
pub mod busfile;
pub mod dashboard;
pub mod fleet;
pub mod json;
pub mod pages;
pub mod paths;
pub mod portal;
mod service;
pub mod ship;
pub mod token;

pub use service::{DEFAULT_PORT, Service, ServiceOptions};
