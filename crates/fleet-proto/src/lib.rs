//! The fleet wire protocol, shared by Hardpoint (the player app) and CIC (the hosted relay), so
//! both read and write the same messages. Pure types and logic: no transport, no clock, no files.
//! Callers pass the time in as milliseconds since the Unix epoch.
//!
//! - [`hello`]: protocol major and capabilities; a different major is refused.
//! - [`identity`]: ed25519 keys and the challenge both ways (nonce, timestamp, replay window).
//! - [`card`]: the card schema, its fields and their consent groups.
//! - [`consent`]: what a sender agrees to send (per field) and what a viewer subscribes to.
//! - [`rollup`]: member states and group roll-ups.
//! - [`roles`] and [`structure`]: event, wings, groups, and who sees which groups.
//! - [`message`]: the messages on the wire and their framing.

pub mod card;
pub mod consent;
pub mod hello;
pub mod identity;
pub mod message;
pub mod roles;
pub mod rollup;
pub mod structure;

pub use card::{CARD_SCHEMA, Card, Consent, FIELDS, FieldDef, FieldValue, Fields, Life, Sharing};
pub use consent::{SendConsent, Subscription, sendable};
pub use hello::{Hello, PROTOCOL_MAJOR, PROTOCOL_MINOR, Version};
pub use identity::{Challenge, Identity, IdentityError, Nonce, PublicKey, Signature};
pub use message::Message;
pub use roles::{Grant, Role};
pub use rollup::{Rollup, Status, rollup};
pub use structure::{EventState, EventStructure};
