//! The link handshake after TLS (PROTOCOL.md 5.3): both sides prove their pinned relay key over a
//! transcript bound to the TLS session, and each side refuses a key it has not paired with.
//!
//! The transcript has its own domain tag and exporter label, so a relay proof is never a fleet
//! proof, and a side byte (`G` game PC, `K` companion), so a proof is never accepted back by the
//! side that made it.

use std::fmt;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use fleet_proto::{Identity, Nonce, PublicKey, Signature};

use super::wire::{self, Msg, PROTOCOL, Reader, Reason};

const LINK_TAG: &[u8] = b"hardpoint relay link v1\0";
/// The TLS exporter label both the link and pairing proofs are bound with.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-hardpoint-relay-v1";
/// Each handshake message must come within this.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Game,
    Companion,
}

impl Side {
    pub fn byte(self) -> u8 {
        match self {
            Self::Game => b'G',
            Self::Companion => b'K',
        }
    }
}

/// The two parties of one link, in a fixed order.
pub struct Parties<'a> {
    pub game_key: &'a PublicKey,
    pub game_nonce: &'a Nonce,
    pub companion_key: &'a PublicKey,
    pub companion_nonce: &'a Nonce,
    pub binding: &'a [u8; 32],
}

pub fn transcript(side: Side, p: &Parties) -> Vec<u8> {
    let mut t = Vec::with_capacity(LINK_TAG.len() + 1 + 4 * 32 + 32);
    t.extend_from_slice(LINK_TAG);
    t.push(side.byte());
    t.extend_from_slice(&nonce_bytes(p.game_nonce));
    t.extend_from_slice(&nonce_bytes(p.companion_nonce));
    t.extend_from_slice(p.game_key.as_bytes());
    t.extend_from_slice(p.companion_key.as_bytes());
    t.extend_from_slice(p.binding);
    t
}

/// A nonce's bytes, from its base64url text (fleet-proto keeps the bytes private).
fn nonce_bytes(nonce: &Nonce) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(nonce.to_string())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The other side refused; with both versions when they differ.
    Refused {
        reason: Reason,
        game: Option<String>,
        companion: Option<String>,
    },
    /// The companion answered with a key that is not the paired one.
    WrongKey,
    BadProof,
    /// Nothing, or something unexpected, came.
    Broken(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Refused {
                reason: Reason::Version,
                game,
                companion,
            } => write!(
                f,
                "this game PC runs Hardpoint {} and the companion {}; update both to the same version",
                game.as_deref().unwrap_or("?"),
                companion.as_deref().unwrap_or("?")
            ),
            Self::Refused { reason, .. } => f.write_str(reason.text()),
            Self::WrongKey => f.write_str("the companion's key is not the paired one"),
            Self::BadProof => f.write_str("the other side's proof does not verify"),
            Self::Broken(why) => f.write_str(why),
        }
    }
}

impl LinkError {
    /// True when trying again cannot help until someone acts (pairs again, updates).
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Refused { .. } | Self::WrongKey | Self::BadProof)
    }
}

pub async fn recv(reader: &mut Reader<impl AsyncRead + Unpin>) -> Result<Msg, LinkError> {
    match tokio::time::timeout(STEP_TIMEOUT, reader.next()).await {
        Err(_) => Err(LinkError::Broken("the other side did not answer".into())),
        Ok(Err(e)) => Err(LinkError::Broken(e.to_string())),
        Ok(Ok(None)) => Err(LinkError::Broken("the other side closed the link".into())),
        Ok(Ok(Some(msg))) => Ok(msg),
    }
}

pub async fn send(out: &mut (impl AsyncWrite + Unpin), msg: &Msg) -> Result<(), LinkError> {
    wire::write(out, msg)
        .await
        .map_err(|e| LinkError::Broken(e.to_string()))
}

fn refusal(msg: Msg) -> LinkError {
    match msg {
        Msg::Refused {
            reason,
            game,
            companion,
        } => LinkError::Refused {
            reason,
            game,
            companion,
        },
        _ => LinkError::Broken("the other side sent something unexpected".into()),
    }
}

fn random_nonce() -> Result<Nonce, LinkError> {
    Nonce::random().map_err(|e| LinkError::Broken(e.to_string()))
}

/// The game PC's side, on a fresh TLS connection to the companion pinned as `pinned`.
pub async fn dial(
    reader: &mut Reader<impl AsyncRead + Unpin>,
    out: &mut (impl AsyncWrite + Unpin),
    me: &Identity,
    pinned: &PublicKey,
    version: &str,
    binding: &[u8; 32],
) -> Result<(), LinkError> {
    let game_nonce = random_nonce()?;
    let game_key = me.public();
    send(
        out,
        &Msg::RelayHello {
            protocol: PROTOCOL,
            app: version.to_owned(),
            key: game_key,
            nonce: game_nonce,
            proof: None,
        },
    )
    .await?;
    let (companion_key, companion_nonce, proof) = match recv(reader).await? {
        Msg::RelayHello {
            protocol,
            app,
            key,
            nonce,
            proof: Some(proof),
        } => {
            if protocol != PROTOCOL || app != version {
                let refused = Msg::Refused {
                    reason: Reason::Version,
                    game: Some(version.to_owned()),
                    companion: Some(app),
                };
                let _ = send(out, &refused).await;
                return Err(refusal(refused));
            }
            (key, nonce, proof)
        }
        other => return Err(refusal(other)),
    };
    if companion_key != *pinned {
        let _ = send(out, &Msg::refused(Reason::NotPaired)).await;
        return Err(LinkError::WrongKey);
    }
    let parties = Parties {
        game_key: &game_key,
        game_nonce: &game_nonce,
        companion_key: &companion_key,
        companion_nonce: &companion_nonce,
        binding,
    };
    if companion_key
        .verify(&transcript(Side::Companion, &parties), &proof)
        .is_err()
    {
        let _ = send(out, &Msg::refused(Reason::NotPaired)).await;
        return Err(LinkError::BadProof);
    }
    let proof = me.sign(&transcript(Side::Game, &parties));
    send(out, &Msg::RelayProof { proof }).await?;
    match recv(reader).await? {
        Msg::RelayWelcome {} => Ok(()),
        other => Err(refusal(other)),
    }
}

/// The companion's side, after the game PC's `relayHello` came: its key must be `pinned`.
#[allow(clippy::too_many_arguments)]
pub async fn accept(
    reader: &mut Reader<impl AsyncRead + Unpin>,
    out: &mut (impl AsyncWrite + Unpin),
    hello: (u32, String, PublicKey, Nonce),
    me: &Identity,
    pinned: Option<&PublicKey>,
    version: &str,
    binding: &[u8; 32],
) -> Result<(), LinkError> {
    let (protocol, app, game_key, game_nonce) = hello;
    if pinned != Some(&game_key) {
        let refused = Msg::refused(Reason::NotPaired);
        let _ = send(out, &refused).await;
        return Err(refusal(refused));
    }
    if protocol != PROTOCOL || app != version {
        let refused = Msg::Refused {
            reason: Reason::Version,
            game: Some(app),
            companion: Some(version.to_owned()),
        };
        let _ = send(out, &refused).await;
        return Err(refusal(refused));
    }
    let companion_key = me.public();
    let companion_nonce = random_nonce()?;
    let parties = Parties {
        game_key: &game_key,
        game_nonce: &game_nonce,
        companion_key: &companion_key,
        companion_nonce: &companion_nonce,
        binding,
    };
    send(
        out,
        &Msg::RelayHello {
            protocol: PROTOCOL,
            app: version.to_owned(),
            key: companion_key,
            nonce: companion_nonce,
            proof: Some(me.sign(&transcript(Side::Companion, &parties))),
        },
    )
    .await?;
    let proof: Signature = match recv(reader).await? {
        Msg::RelayProof { proof } => proof,
        other => return Err(refusal(other)),
    };
    if game_key
        .verify(&transcript(Side::Game, &parties), &proof)
        .is_err()
    {
        let _ = send(out, &Msg::refused(Reason::NotPaired)).await;
        return Err(LinkError::BadProof);
    }
    send(out, &Msg::RelayWelcome {}).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf, duplex, split};

    type Half = (Reader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>);

    fn pipe() -> (Half, Half) {
        let (a, b) = duplex(1 << 16);
        let (ar, aw) = split(a);
        let (br, bw) = split(b);
        ((Reader::new(ar), aw), (Reader::new(br), bw))
    }

    struct Case {
        game_pins: PublicKey,
        companion_pins: Option<PublicKey>,
        game_version: &'static str,
        companion_version: &'static str,
        companion_binding: [u8; 32],
    }

    async fn run(
        game: &Identity,
        companion: &Identity,
        case: Case,
    ) -> (Result<(), LinkError>, Result<(), LinkError>) {
        let ((mut gr, mut gw), (mut kr, mut kw)) = pipe();
        let binding = [1u8; 32];
        let dial = dial(
            &mut gr,
            &mut gw,
            game,
            &case.game_pins,
            case.game_version,
            &binding,
        );
        let accept = async {
            let hello = match recv(&mut kr).await? {
                Msg::RelayHello {
                    protocol,
                    app,
                    key,
                    nonce,
                    proof: None,
                } => (protocol, app, key, nonce),
                other => return Err(refusal(other)),
            };
            accept(
                &mut kr,
                &mut kw,
                hello,
                companion,
                case.companion_pins.as_ref(),
                case.companion_version,
                &case.companion_binding,
            )
            .await
        };
        tokio::join!(dial, accept)
    }

    fn ids() -> (Identity, Identity) {
        (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]))
    }

    #[tokio::test]
    async fn paired_sides_with_one_version_link_up() {
        let (g, k) = ids();
        let case = Case {
            game_pins: k.public(),
            companion_pins: Some(g.public()),
            game_version: "0.1.0",
            companion_version: "0.1.0",
            companion_binding: [1; 32],
        };
        assert_eq!(run(&g, &k, case).await, (Ok(()), Ok(())));
    }

    #[tokio::test]
    async fn a_companion_that_never_paired_the_game_pc_refuses_it() {
        let (g, k) = ids();
        let case = Case {
            game_pins: k.public(),
            companion_pins: Some(Identity::from_seed([9; 32]).public()),
            game_version: "0.1.0",
            companion_version: "0.1.0",
            companion_binding: [1; 32],
        };
        let (game, companion) = run(&g, &k, case).await;
        let not_paired = LinkError::Refused {
            reason: Reason::NotPaired,
            game: None,
            companion: None,
        };
        assert_eq!(game, Err(not_paired.clone()));
        assert_eq!(companion, Err(not_paired));
    }

    #[tokio::test]
    async fn the_game_pc_refuses_a_companion_whose_key_is_not_the_pin() {
        let (g, k) = ids();
        let case = Case {
            game_pins: Identity::from_seed([9; 32]).public(),
            companion_pins: Some(g.public()),
            game_version: "0.1.0",
            companion_version: "0.1.0",
            companion_binding: [1; 32],
        };
        let (game, companion) = run(&g, &k, case).await;
        assert_eq!(game, Err(LinkError::WrongKey));
        assert!(companion.is_err());
    }

    #[tokio::test]
    async fn different_versions_are_refused_with_both_named() {
        let (g, k) = ids();
        let case = Case {
            game_pins: k.public(),
            companion_pins: Some(g.public()),
            game_version: "0.1.0",
            companion_version: "0.2.0",
            companion_binding: [1; 32],
        };
        let (game, companion) = run(&g, &k, case).await;
        let refused = LinkError::Refused {
            reason: Reason::Version,
            game: Some("0.1.0".into()),
            companion: Some("0.2.0".into()),
        };
        assert_eq!(game, Err(refused.clone()));
        assert_eq!(companion, Err(refused.clone()));
        assert_eq!(
            refused.to_string(),
            "this game PC runs Hardpoint 0.1.0 and the companion 0.2.0; update both to the same version"
        );
    }

    #[tokio::test]
    async fn a_proof_from_another_tls_session_does_not_verify() {
        let (g, k) = ids();
        let case = Case {
            game_pins: k.public(),
            companion_pins: Some(g.public()),
            game_version: "0.1.0",
            companion_version: "0.1.0",
            companion_binding: [2; 32],
        };
        let (game, companion) = run(&g, &k, case).await;
        assert_eq!(game, Err(LinkError::BadProof));
        assert!(companion.is_err());
    }

    #[test]
    fn the_side_byte_tells_the_two_proofs_apart() {
        let (g, k) = ids();
        let nonce = Nonce::random().unwrap();
        let parties = Parties {
            game_key: &g.public(),
            game_nonce: &nonce,
            companion_key: &k.public(),
            companion_nonce: &nonce,
            binding: &[0; 32],
        };
        let proof = g.sign(&transcript(Side::Game, &parties));
        assert!(
            g.public()
                .verify(&transcript(Side::Game, &parties), &proof)
                .is_ok()
        );
        assert!(
            g.public()
                .verify(&transcript(Side::Companion, &parties), &proof)
                .is_err()
        );
    }
}
