//! The fleet: this player's identity, the fleets joined, the send consent, the client that keeps
//! the connection to a fleet server, and the panel's model. The wire types (card, consent,
//! roll-ups, identity, invite, structure) are in `fleet-proto`, shared with the CIC relay.

pub mod bans;
pub mod card;
pub mod cic;
pub mod client;
pub mod host;
pub mod hub;
pub mod identity;
pub mod layout;
pub mod store;
pub mod tree;
pub mod view;

pub use card::{CARD_SCHEMA, Card, FIELDS, Life, LifeTracker, SendConsent, Sender, build_card};
pub use client::{Backoff, Connected, Connector, FleetClient, TlsConnector, tls_binding};
pub use fleet_proto::rollup::{Rollup, Status, rollup};
pub use host::{FleetHost, HostView};
pub use hub::{FleetHub, FleetModel, LinkInfo, LinkState, STALE_AFTER};
pub use layout::{FleetLayoutStore, LayoutChange};
pub use store::{ConsentStore, FleetsStore, JoinedFleet};
pub use view::{FleetView, FleetViewStore, ViewChange};

#[cfg(test)]
mod tests;
