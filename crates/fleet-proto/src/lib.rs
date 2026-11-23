//! The fleet wire protocol, shared by Hardpoint (the player app) and CIC (the hosted relay), so
//! both read and write the same messages. Pure types and logic: no transport, no clock, no files.
//! Callers pass the time in as milliseconds since the Unix epoch.
//!
//! - [`hello`]: protocol major and capabilities; a different major is refused.
//! - [`identity`]: ed25519 keys and the challenge both ways (nonce, timestamp, replay window,
//!   TLS channel binding).
//! - [`invite`]: the `cic://` invite a member pastes: a connection token (hosted) or a join code
//!   (peer to peer).
//! - [`card`]: the card schema, its fields and their consent groups.
//! - [`consent`]: what a sender agrees to send (per field) and what a viewer subscribes to.
//! - [`rollup`]: member states and unit roll-ups.
//! - [`roles`] and [`structure`]: the unit tree (fleet, wing, ground, squadron, ship, squad), the
//!   roles, and who sees which cards.
//! - [`ship`]: the relay-authored ship card and its source.
//! - [`message`]: the messages on the wire and their framing.

pub mod card;
pub mod consent;
pub mod hello;
pub mod identity;
pub mod invite;
pub mod message;
pub mod roles;
pub mod rollup;
pub mod ship;
pub mod structure;

pub use card::{CARD_SCHEMA, Card, Consent, FIELDS, FieldDef, FieldValue, Fields, Life, Sharing};
pub use consent::{SendConsent, Subscription, sendable};
pub use hello::{Hello, PROTOCOL_MAJOR, PROTOCOL_MINOR, Version};
pub use identity::{
    Challenge, Identity, IdentityError, Nonce, PublicKey, Signature, TLS_BINDING_LABEL,
    TLS_BINDING_LEN, TlsBinding,
};
pub use invite::{Invite, InviteError};
pub use message::{EventChoice, Message, RefusalCode, RosterEntry};
pub use roles::{Grant, Role, Scope};
pub use rollup::{Rollup, Status, rollup, rollup_as};
pub use ship::{ShipSource, SourceKind, ship_card};
pub use structure::{EventState, EventStructure, MemberId, StructureChange, Unit, UnitKind};
