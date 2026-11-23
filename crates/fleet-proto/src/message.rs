//! The messages on a fleet connection, one JSON object per line over TLS. The first three are
//! the handshake (see [`crate::identity`]); nothing else is accepted before it completes. Then
//! `join` (peer to peer) or `connect` (hosted) attaches the member to an event.

use serde::{Deserialize, Serialize};

use crate::card::Card;
use crate::consent::Subscription;
use crate::hello::Hello;
use crate::identity::{Challenge, Nonce, PublicKey, Signature};
use crate::roles::{Grant, Role};
use crate::structure::{EventStructure, MemberId, StructureChange};

/// How often a member sends its card unless the server says otherwise.
pub const CARD_EVERY_MS: u64 = 500;
/// The fastest card rate a server may ask for.
pub const MIN_CARD_EVERY_MS: u64 = 200;

/// The `bye` reason a member sends to leave the event, not just to disconnect.
pub const BYE_LEAVE: &str = "leave";

/// One member in a [`Message::Roster`]. Listed whether or not they are connected, so leaders
/// see who has not connected yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// The user id on a relay, the key's text form peer to peer; cards carry the same id.
    pub member: MemberId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Their highest role; none while Unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The ship or squad they sit in, else the unit they lead; none for the admiral with no
    /// seat, or while Unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The fleet key, only while connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<PublicKey>,
}

impl RosterEntry {
    pub fn connected(&self) -> bool {
        self.key.is_some()
    }
}

/// One event in [`Message::Events`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventChoice {
    pub id: String,
    pub name: String,
    /// The user's unit and highest role there, for showing the choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

/// Why a server refused, for the app to act on; `reason` stays the text to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// The connection token was used already.
    TokenSpent,
    /// The connection token, or the binding it made, is past its 12 hours.
    TokenExpired,
    /// No account has this token.
    TokenUnknown,
    /// A `connect` with no token from a key no account is bound to.
    Unbound,
    /// Kicked from the event, or from every event a general token offers.
    Kicked,
    /// The account is revoked.
    Revoked,
    /// `choose` named an event the user is not in, or one not open.
    NoSuchEvent,
    /// A per-event token was sent with another event.
    WrongEvent,
    /// Peer to peer: the host kicked this key, which bans it (0.4.2).
    Banned,
    /// Peer to peer: the code is wrong or no longer current, or the address sent too many wrong
    /// codes (0.4.2).
    BadCode,
    /// The event has its most members already (0.4.2).
    Full,
    /// Peer to peer: a rejoin by key after the host's session ended, such as a host restart
    /// (0.4.2).
    SessionEnded,
    /// A newer server's code.
    #[serde(other)]
    Other,
}

impl RefusalCode {
    /// What the app tells the player, when the code needs more than the reason.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Self::TokenSpent | Self::TokenExpired | Self::TokenUnknown | Self::Unbound => {
                Some("Make a new connection code in the CIC portal")
            }
            Self::Revoked => Some("The account is disabled: ask a CIC admin"),
            Self::Banned => Some("The host banned this fleet key: only the host can lift it"),
            Self::BadCode => Some("Ask the host for the current invite"),
            Self::Full => Some("The fleet is full: ask the host to make room"),
            Self::SessionEnded => {
                Some("The host's session ended: ask the host for a new invite once they host again")
            }
            _ => None,
        }
    }
}

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<RefusalCode>,
    },
    /// Viewer to relay or sender: the fields wanted.
    Subscribe {
        subscription: Subscription,
    },
    Card {
        card: Card,
    },
    /// Server: the structure as this member sees it, after `joined` and on every change.
    Structure {
        structure: EventStructure,
    },
    /// Client: a change it asks for; answered by `structure` and `roster`, or `changeRefused`.
    Change {
        change: StructureChange,
    },
    /// Peer to peer, after `welcome`: join the event the code names. A member the event knows
    /// may leave the code out and rejoin by key.
    Join {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        handle: String,
    },
    /// Hosted, after `welcome`: bind this key to the account the token names (spending it), or
    /// with no token reconnect a key already bound. `event` is the per-event invite's event, or
    /// the event this app was in when it reconnects. Answered by `joined`, `events` or `refused`.
    Connect {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event: Option<String>,
    },
    /// Server, after `connect`: the user's open events, when it cannot pick one (none open yet,
    /// or more than one with no event named). Sent again when the list changes, until `choose`.
    Events {
        events: Vec<EventChoice>,
    },
    /// Client: the event picked from `events`.
    Choose {
        event: String,
    },
    /// Server, in answer to `join`, `connect` or `choose`: attached to the event. `member` is the
    /// id the event knows this member by (the user id, or the key's text form peer to peer), so
    /// the app finds its own card and roster entry; `handle` is unique in the event. The roles
    /// come with `roster`; `grants` is for a server that sends them here.
    #[serde(rename_all = "camelCase")]
    Joined {
        /// The event's id.
        event: String,
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<MemberId>,
        handle: String,
        /// The ship or squad this member sits in; none while Unassigned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grants: Vec<Grant>,
        /// The card rate the server wants; [`CARD_EVERY_MS`] when it does not say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_every_ms: Option<u64>,
    },
    /// Server, with every structure push: this member's own roles, and everyone in
    /// [`EventStructure::roster_for`], connected or not.
    Roster {
        you: Vec<Grant>,
        #[serde(default)]
        members: Vec<RosterEntry>,
    },
    /// Server: a change this member sent was not applied, and why. The connection stays.
    ChangeRefused {
        reason: String,
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
    use crate::roles::Scope;

    fn round_trips(m: &Message) {
        let line = m.encode();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(&Message::decode(&line).unwrap(), m);
    }

    /// Decodes, and encodes back to the same line.
    fn exact(line: &str) -> Message {
        let m = Message::decode(line.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(m.encode()).unwrap().trim_end(), line);
        m
    }

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
            source: None,
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
            Message::Join {
                code: Some("abc".to_owned()),
                handle: "Vega".to_owned(),
            },
            Message::Connect {
                token: Some("t".repeat(43)),
                event: Some("op-1".to_owned()),
            },
            Message::Roster {
                you: vec![Grant {
                    role: Role::SquadronCommander,
                    scope: Scope::Unit("red".to_owned()),
                }],
                members: vec![RosterEntry {
                    member: "u-1".to_owned(),
                    handle: Some("Vega".to_owned()),
                    role: Some(Role::SquadronCommander),
                    unit: Some("red-1".to_owned()),
                    key: Some(id.public()),
                }],
            },
            Message::ChangeRefused {
                reason: "no".to_owned(),
            },
        ] {
            round_trips(&m);
        }
        assert_eq!(
            String::from_utf8(
                Message::Refused {
                    reason: "no".to_owned(),
                    code: None,
                }
                .encode(),
            )
            .unwrap(),
            "{\"type\":\"refused\",\"reason\":\"no\"}\n"
        );
    }

    #[test]
    fn join_leaves_out_a_spent_code_and_joined_defaults_the_rate() {
        let rejoin = Message::Join {
            code: None,
            handle: "Vega".to_owned(),
        };
        assert_eq!(
            String::from_utf8(rejoin.encode()).unwrap(),
            "{\"type\":\"join\",\"handle\":\"Vega\"}\n"
        );
        let Message::Joined {
            grants,
            card_every_ms,
            member,
            unit,
            ..
        } = Message::decode(br#"{"type":"joined","event":"e","name":"E","handle":"V"}"#).unwrap()
        else {
            panic!("not joined");
        };
        assert!(grants.is_empty());
        assert_eq!((card_every_ms, member, unit), (None, None, None));
        exact(r#"{"type":"join","code":"ABCD-EFGH-JKLM","handle":"Vega"}"#);
    }

    #[test]
    fn connect_shapes_are_exact() {
        let token = "q".repeat(43);
        assert_eq!(
            exact(&format!(
                r#"{{"type":"connect","token":"{token}","event":"op-1"}}"#
            )),
            Message::Connect {
                token: Some(token.clone()),
                event: Some("op-1".to_owned()),
            }
        );
        // A bound key reconnecting: no token.
        assert_eq!(
            exact(r#"{"type":"connect"}"#),
            Message::Connect {
                token: None,
                event: None,
            }
        );
        exact(r#"{"type":"connect","event":"op-1"}"#);

        let Message::Events { events } = exact(
            r#"{"type":"events","events":[{"id":"op-1","name":"Op One","unit":"red-2","role":"pilot"},{"id":"op-2","name":"Op Two"}]}"#,
        ) else {
            panic!("not events");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].role, Some(Role::Pilot));
        assert_eq!(events[1].unit, None);
        exact(r#"{"type":"events","events":[]}"#);
        assert_eq!(
            exact(r#"{"type":"choose","event":"op-2"}"#),
            Message::Choose {
                event: "op-2".to_owned()
            }
        );

        let joined = exact(
            r#"{"type":"joined","event":"op-1","name":"Op One","member":"u-7","handle":"Vega","unit":"red-2","grants":[{"role":"pilot","scope":{"kind":"unit","id":"red-2"}}],"cardEveryMs":500}"#,
        );
        let Message::Joined { member, unit, .. } = joined else {
            panic!("not joined");
        };
        assert_eq!(
            (member.as_deref(), unit.as_deref()),
            (Some("u-7"), Some("red-2"))
        );
        // Unassigned: no unit, no grants.
        exact(r#"{"type":"joined","event":"op-1","name":"Op One","member":"u-8","handle":"Kite"}"#);
    }

    #[test]
    fn refusals_carry_a_code_the_app_can_act_on() {
        for (code, wire) in [
            (RefusalCode::TokenSpent, "token_spent"),
            (RefusalCode::TokenExpired, "token_expired"),
            (RefusalCode::TokenUnknown, "token_unknown"),
            (RefusalCode::Unbound, "unbound"),
            (RefusalCode::Kicked, "kicked"),
            (RefusalCode::Revoked, "revoked"),
            (RefusalCode::NoSuchEvent, "no_such_event"),
            (RefusalCode::WrongEvent, "wrong_event"),
            (RefusalCode::Banned, "banned"),
            (RefusalCode::BadCode, "bad_code"),
            (RefusalCode::Full, "full"),
            (RefusalCode::SessionEnded, "session_ended"),
        ] {
            let line = format!(r#"{{"type":"refused","reason":"r","code":"{wire}"}}"#);
            assert_eq!(
                exact(&line),
                Message::Refused {
                    reason: "r".to_owned(),
                    code: Some(code),
                }
            );
        }
        assert_eq!(
            RefusalCode::TokenExpired.hint(),
            Some("Make a new connection code in the CIC portal")
        );
        assert_eq!(RefusalCode::Kicked.hint(), None);
        assert_eq!(
            RefusalCode::BadCode.hint(),
            Some("Ask the host for the current invite")
        );
        // A restart is not a wrong code: no hunting for a code that does not exist yet.
        assert_ne!(
            RefusalCode::SessionEnded.hint(),
            RefusalCode::BadCode.hint()
        );
        for code in [RefusalCode::Banned, RefusalCode::Full] {
            assert!(code.hint().is_some(), "{code:?}");
        }
        // A newer server's code still reads.
        let Message::Refused { code, .. } =
            Message::decode(br#"{"type":"refused","reason":"r","code":"later"}"#).unwrap()
        else {
            panic!("not refused");
        };
        assert_eq!(code, Some(RefusalCode::Other));
    }

    #[test]
    fn roster_entries_say_whether_a_member_is_connected() {
        let key = Identity::from_seed([5; 32]).public();
        let line = format!(
            r#"{{"type":"roster","you":[{{"role":"admiral","scope":{{"kind":"event"}}}}],"members":[{{"member":"u-1","handle":"Rook","role":"pilot","unit":"red-1","key":"{key}"}},{{"member":"u-2","handle":"Wren","role":"crew","unit":"carrack"}},{{"member":"u-3","handle":"New"}}]}}"#
        );
        let Message::Roster { you, members } = exact(&line) else {
            panic!("not roster");
        };
        assert_eq!(you[0].role, Role::Admiral);
        let connected: Vec<bool> = members.iter().map(RosterEntry::connected).collect();
        assert_eq!(connected, [true, false, false]);
        assert_eq!(members[2].unit, None);
    }

    #[test]
    fn structure_and_change_shapes_are_exact() {
        exact(
            r#"{"type":"structure","structure":{"event":{"id":"op-1","name":"Op One","state":"open"},"admiral":"u-1","units":[{"id":"air","kind":"wing","name":"Air","lead":["u-2"]},{"id":"red","kind":"squadron","name":"Red","parent":"air"},{"id":"red-1","kind":"ship","name":"Red 1","parent":"red","seats":[{"member":"u-3","role":"pilot"}]}],"unassigned":["u-4"]}}"#,
        );
        exact(
            r#"{"type":"change","change":{"op":"place","member":"u-4","unit":"red-1","role":"copilot"}}"#,
        );
        exact(
            r#"{"type":"change","change":{"op":"place","member":"u-4","unit":null,"role":null}}"#,
        );
        exact(
            r#"{"type":"change","change":{"op":"addUnit","id":"red-2","kind":"ship","name":"Red 2","parent":"red"}}"#,
        );
        exact(
            r#"{"type":"change","change":{"op":"setLead","unit":"red","member":"u-3","on":true}}"#,
        );
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
