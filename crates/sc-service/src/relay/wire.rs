//! The relay's messages (PROTOCOL.md section 5) and their framing: one JSON object per line, as
//! the fleet wire protocol.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use fleet_proto::{Nonce, PublicKey, Signature};

use crate::pages::MAX_PAGE_MESSAGE_BYTES;

pub const PROTOCOL: u32 = 1;
/// A line longer than this ends the link: a page message at the cap, escaped, fits easily.
pub const MAX_LINE: usize = 4 * MAX_PAGE_MESSAGE_BYTES;
/// Pages a companion may have open over one link.
pub const MAX_STREAMS: usize = 8;

/// Why a side ends the handshake or pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// The other side's relay key is not the one paired here.
    NotPaired,
    /// The two apps are not the same version; both are named.
    Version,
    /// No pairing code is shown on the companion.
    PairingClosed,
    /// The pairing exchange failed: a wrong code, or someone in the middle.
    PairingFailed,
    /// The code was refused three times and is spent.
    PairingBurned,
    /// Another pairing attempt from this address came less than 5 s ago.
    Busy,
    /// The companion's user cancelled, or did not answer in time.
    Cancelled,
    /// A message out of order or malformed.
    Protocol,
}

impl Reason {
    pub fn text(self) -> &'static str {
        match self {
            Self::NotPaired => "not paired",
            Self::Version => "the two Hardpoint versions differ",
            Self::PairingClosed => "the companion is not showing a pairing code",
            Self::PairingFailed => "pairing failed: check the code",
            Self::PairingBurned => "pairing stopped after 3 wrong codes",
            Self::Busy => "too many pairing attempts; wait 5 s",
            Self::Cancelled => "pairing was cancelled on the companion",
            Self::Protocol => "the other side sent something unexpected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Msg {
    /// Pairing, both ways: the game PC first, then the companion. `spake` is this side's SPAKE2
    /// message, base64url.
    PairHello {
        protocol: u32,
        app: String,
        name: String,
        spake: String,
    },
    /// Pairing, both ways after the hellos: this side's relay key, a MAC with the shared key that
    /// proves the code, and a signature that proves the key.
    PairProof {
        key: PublicKey,
        mac: String,
        sig: Signature,
    },
    /// Pairing, companion to game PC: its user pressed Pair.
    PairDone {},
    /// The link, both ways: the game PC's hello has no proof; the companion's has its proof.
    RelayHello {
        protocol: u32,
        app: String,
        key: PublicKey,
        nonce: Nonce,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof: Option<Signature>,
    },
    /// The link, game PC to companion.
    RelayProof { proof: Signature },
    /// The link, companion to game PC: the link is up.
    RelayWelcome {},
    /// Either side, then it closes. With [`Reason::Version`], both versions.
    Refused {
        reason: Reason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        game: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        companion: Option<String>,
    },
    /// Companion to game PC: a page opened; the game PC runs a session for it.
    Open {
        stream: u32,
        kind: StreamKind,
        client: String,
    },
    /// Both ways: one page message on a stream.
    Data { stream: u32, text: String },
    /// Both ways: the stream ended.
    Close {
        stream: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Game PC to companion, every second; `t` is the game PC's clock in ms.
    Ping { t: i64 },
    /// Companion to game PC: the ping's `t`, and the companion's clock when it answered.
    Pong { t: i64, at: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamKind {
    Page,
}

impl Msg {
    pub fn refused(reason: Reason) -> Self {
        Self::Refused {
            reason,
            game: None,
            companion: None,
        }
    }
}

/// Reads messages, one per line.
pub struct Reader<R> {
    inner: BufReader<R>,
    line: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            line: Vec::new(),
        }
    }

    /// The next message; `None` at the end of the stream. A line that is not a message is an
    /// error, and so is one longer than [`MAX_LINE`].
    pub async fn next(&mut self) -> io::Result<Option<Msg>> {
        self.line.clear();
        let cap = u64::try_from(MAX_LINE + 1).unwrap_or(u64::MAX);
        let n = (&mut self.inner)
            .take(cap)
            .read_until(b'\n', &mut self.line)
            .await?;
        if n == 0 {
            return Ok(None);
        }
        if self.line.last() != Some(&b'\n') {
            let why = if n > MAX_LINE {
                "a relay message is too long"
            } else {
                "the relay link ended inside a message"
            };
            return Err(io::Error::new(io::ErrorKind::InvalidData, why));
        }
        serde_json::from_slice(&self.line)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

pub async fn write(out: &mut (impl AsyncWrite + Unpin), msg: &Msg) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line).await?;
    out.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_proto::Identity;
    use serde_json::{Value, json};

    fn wire(msg: &Msg) -> Value {
        serde_json::to_value(msg).unwrap()
    }

    fn round_trip(msg: Msg, expected: Value) {
        assert_eq!(wire(&msg), expected);
        let back: Msg = serde_json::from_value(expected).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn every_message_has_its_wire_shape() {
        let id = Identity::from_seed([7; 32]);
        let key = id.public();
        let sig = id.sign(b"x");
        let nonce: Nonce = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            .parse()
            .unwrap();
        round_trip(
            Msg::PairHello {
                protocol: 1,
                app: "0.1.0".into(),
                name: "gamepc".into(),
                spake: "c3Bha2U".into(),
            },
            json!({ "type": "pairHello", "protocol": 1, "app": "0.1.0", "name": "gamepc", "spake": "c3Bha2U" }),
        );
        round_trip(
            Msg::PairProof {
                key,
                mac: "bWFj".into(),
                sig,
            },
            json!({ "type": "pairProof", "key": key.to_string(), "mac": "bWFj", "sig": sig.to_string() }),
        );
        round_trip(Msg::PairDone {}, json!({ "type": "pairDone" }));
        round_trip(
            Msg::RelayHello {
                protocol: 1,
                app: "0.1.0".into(),
                key,
                nonce,
                proof: None,
            },
            json!({ "type": "relayHello", "protocol": 1, "app": "0.1.0", "key": key.to_string(), "nonce": nonce.to_string() }),
        );
        round_trip(
            Msg::RelayHello {
                protocol: 1,
                app: "0.1.0".into(),
                key,
                nonce,
                proof: Some(sig),
            },
            json!({ "type": "relayHello", "protocol": 1, "app": "0.1.0", "key": key.to_string(), "nonce": nonce.to_string(), "proof": sig.to_string() }),
        );
        round_trip(
            Msg::RelayProof { proof: sig },
            json!({ "type": "relayProof", "proof": sig.to_string() }),
        );
        round_trip(Msg::RelayWelcome {}, json!({ "type": "relayWelcome" }));
        round_trip(
            Msg::refused(Reason::NotPaired),
            json!({ "type": "refused", "reason": "not-paired" }),
        );
        round_trip(
            Msg::Refused {
                reason: Reason::Version,
                game: Some("0.1.0".into()),
                companion: Some("0.2.0".into()),
            },
            json!({ "type": "refused", "reason": "version", "game": "0.1.0", "companion": "0.2.0" }),
        );
        for (reason, text) in [
            (Reason::PairingClosed, "pairing-closed"),
            (Reason::PairingFailed, "pairing-failed"),
            (Reason::PairingBurned, "pairing-burned"),
            (Reason::Busy, "busy"),
            (Reason::Cancelled, "cancelled"),
            (Reason::Protocol, "protocol"),
        ] {
            assert_eq!(wire(&Msg::refused(reason))["reason"], text);
        }
        round_trip(
            Msg::Open {
                stream: 3,
                kind: StreamKind::Page,
                client: "dash".into(),
            },
            json!({ "type": "open", "stream": 3, "kind": "page", "client": "dash" }),
        );
        round_trip(
            Msg::Data {
                stream: 3,
                text: "{\"type\":\"status\"}".into(),
            },
            json!({ "type": "data", "stream": 3, "text": "{\"type\":\"status\"}" }),
        );
        round_trip(
            Msg::Close {
                stream: 3,
                reason: None,
            },
            json!({ "type": "close", "stream": 3 }),
        );
        round_trip(
            Msg::Close {
                stream: 3,
                reason: Some("too many streams".into()),
            },
            json!({ "type": "close", "stream": 3, "reason": "too many streams" }),
        );
        round_trip(Msg::Ping { t: 5 }, json!({ "type": "ping", "t": 5 }));
        round_trip(
            Msg::Pong { t: 5, at: 9 },
            json!({ "type": "pong", "t": 5, "at": 9 }),
        );
    }

    #[tokio::test]
    async fn lines_are_read_back_and_a_bad_or_long_line_is_an_error() {
        let mut out = Vec::new();
        write(&mut out, &Msg::Ping { t: 1 }).await.unwrap();
        write(&mut out, &Msg::PairDone {}).await.unwrap();
        let mut reader = Reader::new(out.as_slice());
        assert_eq!(reader.next().await.unwrap(), Some(Msg::Ping { t: 1 }));
        assert_eq!(reader.next().await.unwrap(), Some(Msg::PairDone {}));
        assert_eq!(reader.next().await.unwrap(), None);

        let mut reader = Reader::new(&b"{\"type\":\"nope\"}\n"[..]);
        assert!(reader.next().await.is_err());
        let long = vec![b' '; MAX_LINE + 10];
        let mut reader = Reader::new(long.as_slice());
        assert!(reader.next().await.is_err());
        let mut reader = Reader::new(&b"{\"type\":\"ping\",\"t\":1}"[..]);
        assert!(reader.next().await.is_err());
    }
}
