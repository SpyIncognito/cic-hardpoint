//! The fleet panel (draft): member cards, the viewer's groups and the model the dashboard draws.
//! Rust service only. No fleet transport exists yet; `--fleet-demo` feeds the panel from mock
//! peers replayed from recordings ([`demo`], demo only). The card builder, the hub, the roll-ups
//! and the viewer's store are what a real fleet transport will use. The wire types (card,
//! consent, roll-ups, identity) are in `fleet-proto`, shared with the CIC relay.

pub mod card;
pub mod demo;
pub mod hub;
pub mod view;

pub use card::{CARD_SCHEMA, Card, FIELDS, Life, LifeTracker, SendConsent, Sender, build_card};
pub use fleet_proto::rollup::{Rollup, Status, rollup};
pub use hub::{FleetHub, FleetModel, STALE_AFTER};
pub use view::{FleetView, FleetViewStore, ViewChange};

#[cfg(test)]
mod tests;
