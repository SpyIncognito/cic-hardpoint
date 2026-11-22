//! The messages on a fleet connection, one JSON object per line over TLS. The first three are
//! the handshake (see [`crate::identity`]); nothing else is accepted before it completes.

use serde::{Deserialize, Serialize};

use crate::card::Card;
use crate::consent::Subscription;
use crate::hello::Hello;
use crate::identity::{Challenge, Nonce, PublicKey, Signature};
use crate::structure::{EventStructure, StructureChange};

/// A longer line is refused unread.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Message {
    /// Client, first.
    ClientHello {
        hello: Hello,
        key: PublicKey,
        nonce: Nonce,
    },
    /// Server, in answer: its key, a challenge, and its proof.
    ServerHello {
        hello: Hello,
        /// The server's configured name, for display.
        server: String,
        key: PublicKey,
        challenge: Challenge,
        proof: Signature,
    },
    /// Client: its proof.
    ClientProof {
        proof: Signature,
    },
    /// Server: the handshake is done.
    Welcome {
        event: String,
        capabilities: Vec<String>,
    },
    /// Either side, before closing: why.
    Refused {
        reason: String,
    },
    /// Viewer to relay or sender: the fields wanted.
    Subscribe {
        subscription: Subscription,
    },
    Card {
        card: Card,
    },
    Structure {
        structure: EventStructure,
    },
    Change {
        change: StructureChange,
    },
    /// Either side, closing politely.
    Bye {
        #[serde(default)]
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("message over {MAX_FRAME_BYTES} bytes")]
    TooLong,
    #[error("not a fleet message: {0}")]
    Bad(#[from] serde_json::Error),
}

impl Message {
    /// One line, ending in a newline.
    pub fn encode(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }

    /// One line, with or without its newline.
    pub fn decode(line: &[u8]) -> Result<Self, FrameError> {
        if line.len() > MAX_FRAME_BYTES + 1 {
            return Err(FrameError::TooLong);
        }
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        Ok(serde_json::from_slice(line)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{CARD_SCHEMA, FieldValue, Fields, Sharing, Tone};
    use crate::identity::Identity;

    #[test]
    fn messages_round_trip_as_lines() {
        let id = Identity::from_seed([3; 32]);
        let card = Card {
            schema: CARD_SCHEMA,
            member: id.public().to_string(),
            handle: "Vega".to_owned(),
            sharing: Sharing {
                ship: true,
                ..Sharing::default()
            },
            state: None,
            fields: Fields(vec![("ship.hull", FieldValue::text("90%", Tone::Ok))]),
        };
        for m in [
            Message::ClientHello {
                hello: Hello::new("t", &["cards"]),
                key: id.public(),
                nonce: Nonce::random().unwrap(),
            },
            Message::ClientProof {
                proof: id.sign(b"x"),
            },
            Message::Card { card },
            Message::Subscribe {
                subscription: Subscription::of(["ship.hull"]),
            },
            Message::Bye {
                reason: String::new(),
            },
        ] {
            let line = m.encode();
            assert_eq!(line.last(), Some(&b'\n'));
            assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
            assert_eq!(Message::decode(&line).unwrap(), m);
        }
        let text = String::from_utf8(
            Message::Refused {
                reason: "no".to_owned(),
            }
            .encode(),
        )
        .unwrap();
        assert_eq!(text, "{\"type\":\"refused\",\"reason\":\"no\"}\n");
    }

    #[test]
    fn a_frame_over_the_cap_or_unknown_is_refused() {
        let long = vec![b' '; MAX_FRAME_BYTES + 2];
        assert!(matches!(Message::decode(&long), Err(FrameError::TooLong)));
        assert!(matches!(
            Message::decode(br#"{"type":"nope"}"#),
            Err(FrameError::Bad(_))
        ));
    }
}
