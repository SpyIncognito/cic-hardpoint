//! The first message each side sends: the protocol version and what it can do. Sides with a
//! different major refuse each other, with a message naming the version needed; the minor and
//! the capabilities only add things an older side can ignore.

use std::fmt;

use serde::{Deserialize, Serialize};

/// 2 since fleet-proto 0.4.0: the unit tree and member ids replace wings, groups and keys.
pub const PROTOCOL_MAJOR: u32 = 2;
pub const PROTOCOL_MINOR: u32 = 0;

/// Capabilities a side may offer. A side ignores ones it does not know.
pub mod capability {
    /// Sends and receives cards.
    pub const CARDS: &str = "cards";
    /// Honours viewers' subscriptions.
    pub const SUBSCRIBE: &str = "subscribe";
    /// Sends or receives event structure (the unit tree and roles).
    pub const STRUCTURE: &str = "structure";
    /// Takes `join` after the handshake: join codes, peer to peer. A server without it or
    /// `connect` puts every connection in one event.
    pub const JOIN: &str = "join";
    /// Takes `connect` after the handshake: CIC accounts, connection tokens and the event
    /// choice (fleet-proto 0.4.0).
    pub const CONNECT: &str = "connect";
    /// Signs the identity transcript with the TLS session's exported keying material appended
    /// ([`crate::identity::TlsBinding`]). Hardpoint refuses a server without it.
    pub const TLS_BINDING: &str = "tls-binding";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: Version,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The software and its version, for logs only, such as `cic-relay 0.1.0`.
    #[serde(default)]
    pub software: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "fleet protocol {theirs} is not supported: this side needs protocol {}.x (it speaks {ours})",
    ours.major
)]
pub struct VersionMismatch {
    pub ours: Version,
    pub theirs: Version,
}

impl Hello {
    pub fn new(software: impl Into<String>, capabilities: &[&str]) -> Self {
        Self {
            protocol: Version::CURRENT,
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            software: software.into(),
        }
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    /// Refuses a peer with a different major.
    pub fn check(&self, theirs: &Hello) -> Result<(), VersionMismatch> {
        if self.protocol.major == theirs.protocol.major {
            Ok(())
        } else {
            Err(VersionMismatch {
                ours: self.protocol,
                theirs: theirs.protocol,
            })
        }
    }

    /// The capabilities both sides offer, in this side's order.
    pub fn common(&self, theirs: &Hello) -> Vec<String> {
        self.capabilities
            .iter()
            .filter(|c| theirs.has(c))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_major_is_accepted_and_capabilities_intersect() {
        let ours = Hello::new("a", &[capability::CARDS, capability::STRUCTURE]);
        let mut theirs = Hello::new("b", &[capability::CARDS, "later"]);
        theirs.protocol.minor += 3;
        assert!(ours.check(&theirs).is_ok());
        assert_eq!(ours.common(&theirs), [capability::CARDS]);
    }

    #[test]
    fn a_different_major_is_refused_naming_the_version_needed() {
        let ours = Hello::new("a", &[]);
        let mut theirs = Hello::new("b", &[]);
        theirs.protocol.major = PROTOCOL_MAJOR + 1;
        let err = ours.check(&theirs).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(&format!("needs protocol {PROTOCOL_MAJOR}.x")),
            "{text}"
        );
        assert!(
            text.contains(&format!("{}.0", PROTOCOL_MAJOR + 1)),
            "{text}"
        );
    }
}
