//! Pairing (PROTOCOL.md 5.2): the companion shows an 8-digit code, the game PC's user types it,
//! and both run SPAKE2 on it over TLS. With a password-authenticated exchange a party in the
//! middle gets one online guess per attempt, never an offline search of the code space; three
//! wrong codes burn the code. The SPAKE2 identities carry the TLS binding and the side bytes, so
//! the shared key exists only for these two ends of this one TLS session.
//!
//! Inside the exchange each side sends its relay key with a MAC under the shared key (proving the
//! code) and a signature (proving the key). The companion checks the game PC's MAC before it sends
//! its own, so a wrong guess learns nothing.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;

use fleet_proto::{Identity, PublicKey, Signature};

use super::handshake::{LinkError, Side, recv, send};
use super::wire::{Msg, PROTOCOL, Reader, Reason};

const PAIR_TAG: &[u8] = b"hardpoint relay pair v1\0";
/// A code is good for this long after it is shown.
pub const CODE_LIFETIME: Duration = Duration::from_secs(120);
/// Wrong codes before the code is burned.
pub const MAX_FAILURES: u32 = 3;
/// One pairing attempt per source address in this long.
pub const ATTEMPT_EVERY: Duration = Duration::from_secs(5);
pub const CODE_DIGITS: usize = 8;

/// A pairing code: 8 decimal digits.
#[derive(Clone, PartialEq, Eq)]
pub struct Code(String);

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("Code(..)")
    }
}

impl Code {
    pub fn generate() -> std::io::Result<Self> {
        // Below the largest multiple of 10^8 that fits, so every code is equally likely.
        const LIMIT: u32 = u32::MAX - u32::MAX % 100_000_000;
        loop {
            let mut bytes = [0u8; 4];
            getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
            let n = u32::from_le_bytes(bytes);
            if n < LIMIT {
                return Ok(Self(format!("{:08}", n % 100_000_000)));
            }
        }
    }

    /// What the user typed, with any spaces or dashes; `None` unless 8 digits remain.
    pub fn parse(typed: &str) -> Option<Self> {
        let digits: String = typed
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        (digits.len() == CODE_DIGITS && digits.chars().all(|c| c.is_ascii_digit()))
            .then_some(Self(digits))
    }

    pub fn digits(&self) -> &str {
        &self.0
    }

    /// As shown: `4821 7730`.
    pub fn shown(&self) -> String {
        format!("{} {}", &self.0[..4], &self.0[4..])
    }
}

/// What a side learns of the other when pairing succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    pub name: String,
    pub key: PublicKey,
}

fn spake_ids(binding: &[u8; 32]) -> (SpakeId, SpakeId) {
    let id = |side: Side| {
        let mut id = PAIR_TAG.to_vec();
        id.push(side.byte());
        id.extend_from_slice(binding);
        SpakeId::new(&id)
    };
    (id(Side::Game), id(Side::Companion))
}

/// What each side's MAC and signature cover.
fn proof_input(
    side: Side,
    binding: &[u8; 32],
    game_msg: &[u8],
    companion_msg: &[u8],
    key: &PublicKey,
) -> Vec<u8> {
    let mut t = PAIR_TAG.to_vec();
    t.push(side.byte());
    t.extend_from_slice(binding);
    for part in [game_msg, companion_msg] {
        t.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
        t.extend_from_slice(part);
    }
    t.extend_from_slice(key.as_bytes());
    t
}

fn mac(shared: &[u8], input: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(shared).expect("HMAC takes any key length");
    mac.update(input);
    mac
}

fn broken(why: &str) -> LinkError {
    LinkError::Broken(why.to_owned())
}

fn refused(reason: Reason) -> LinkError {
    LinkError::Refused {
        reason,
        game: None,
        companion: None,
    }
}

/// Checks the other side's proof: the MAC with the shared key, then the signature by its key.
fn check_proof(
    shared: &[u8],
    input: &[u8],
    their_mac: &str,
    key: &PublicKey,
    sig: &Signature,
) -> Result<(), LinkError> {
    let their_mac = URL_SAFE_NO_PAD
        .decode(their_mac)
        .map_err(|_| refused(Reason::PairingFailed))?;
    mac(shared, input)
        .verify_slice(&their_mac)
        .map_err(|_| refused(Reason::PairingFailed))?;
    key.verify(input, sig).map_err(|_| LinkError::BadProof)
}

fn my_proof(me: &Identity, shared: &[u8], input: &[u8]) -> Msg {
    Msg::PairProof {
        key: me.public(),
        mac: URL_SAFE_NO_PAD.encode(mac(shared, input).finalize().into_bytes()),
        sig: me.sign(input),
    }
}

fn version_refusal(game: &str, companion: &str) -> Msg {
    Msg::Refused {
        reason: Reason::Version,
        game: Some(game.to_owned()),
        companion: Some(companion.to_owned()),
    }
}

fn as_error(msg: Msg) -> LinkError {
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
        _ => broken("the companion sent something unexpected"),
    }
}

/// The game PC's side, on a fresh TLS connection to the companion. Waits for the companion's
/// user to press Pair (up to the code's lifetime).
pub async fn dial(
    reader: &mut Reader<impl AsyncRead + Unpin>,
    out: &mut (impl AsyncWrite + Unpin),
    me: &Identity,
    my_name: &str,
    version: &str,
    binding: &[u8; 32],
    code: &Code,
) -> Result<Paired, LinkError> {
    let (id_g, id_k) = spake_ids(binding);
    let (spake, game_msg) =
        Spake2::<Ed25519Group>::start_a(&Password::new(code.digits()), &id_g, &id_k);
    send(
        out,
        &Msg::PairHello {
            protocol: PROTOCOL,
            app: version.to_owned(),
            name: my_name.to_owned(),
            spake: URL_SAFE_NO_PAD.encode(&game_msg),
        },
    )
    .await?;
    let (name, companion_msg) = match recv(reader).await? {
        Msg::PairHello {
            protocol,
            app,
            name,
            spake,
        } => {
            if protocol != PROTOCOL || app != version {
                let refusal = version_refusal(version, &app);
                let _ = send(out, &refusal).await;
                return Err(as_error(refusal));
            }
            let msg = URL_SAFE_NO_PAD
                .decode(spake)
                .map_err(|_| broken("the companion's pairing message is not base64"))?;
            (name, msg)
        }
        other => return Err(as_error(other)),
    };
    let shared = spake
        .finish(&companion_msg)
        .map_err(|_| refused(Reason::PairingFailed))?;
    let mine = proof_input(Side::Game, binding, &game_msg, &companion_msg, &me.public());
    send(out, &my_proof(me, &shared, &mine)).await?;
    let (key, their_mac, sig) = match recv(reader).await? {
        Msg::PairProof { key, mac, sig } => (key, mac, sig),
        other => return Err(as_error(other)),
    };
    let theirs = proof_input(Side::Companion, binding, &game_msg, &companion_msg, &key);
    check_proof(&shared, &theirs, &their_mac, &key, &sig)?;
    match tokio::time::timeout(CODE_LIFETIME, reader.next()).await {
        Ok(Ok(Some(Msg::PairDone {}))) => Ok(Paired { name, key }),
        Ok(Ok(Some(other))) => Err(as_error(other)),
        Ok(Ok(None) | Err(_)) => Err(broken("the companion closed before Pair was pressed")),
        Err(_) => Err(refused(Reason::Cancelled)),
    }
}

/// The companion's side after the game PC's `pairHello` came, up to the point where its user
/// decides: then [`finish`] answers on the same connection.
#[allow(clippy::too_many_arguments)]
pub async fn accept(
    reader: &mut Reader<impl AsyncRead + Unpin>,
    out: &mut (impl AsyncWrite + Unpin),
    hello: (u32, String, String, String),
    me: &Identity,
    my_name: &str,
    version: &str,
    binding: &[u8; 32],
    code: &Code,
) -> Result<Paired, LinkError> {
    let (protocol, app, name, spake) = hello;
    if protocol != PROTOCOL || app != version {
        let refusal = version_refusal(&app, version);
        let _ = send(out, &refusal).await;
        return Err(as_error(refusal));
    }
    let game_msg = URL_SAFE_NO_PAD
        .decode(spake)
        .map_err(|_| broken("the game PC's pairing message is not base64"))?;
    let (id_g, id_k) = spake_ids(binding);
    let (state, companion_msg) =
        Spake2::<Ed25519Group>::start_b(&Password::new(code.digits()), &id_g, &id_k);
    send(
        out,
        &Msg::PairHello {
            protocol: PROTOCOL,
            app: version.to_owned(),
            name: my_name.to_owned(),
            spake: URL_SAFE_NO_PAD.encode(&companion_msg),
        },
    )
    .await?;
    let (key, their_mac, sig) = match recv(reader).await? {
        Msg::PairProof { key, mac, sig } => (key, mac, sig),
        other => return Err(as_error(other)),
    };
    let theirs = proof_input(Side::Game, binding, &game_msg, &companion_msg, &key);
    let checked = state
        .finish(&game_msg)
        .map_err(|_| refused(Reason::PairingFailed))
        .and_then(|shared| check_proof(&shared, &theirs, &their_mac, &key, &sig).map(|()| shared));
    let shared = match checked {
        Ok(shared) => shared,
        Err(e) => {
            let _ = send(out, &Msg::refused(Reason::PairingFailed)).await;
            return Err(e);
        }
    };
    let mine = proof_input(
        Side::Companion,
        binding,
        &game_msg,
        &companion_msg,
        &me.public(),
    );
    send(out, &my_proof(me, &shared, &mine)).await?;
    Ok(Paired { name, key })
}

/// The companion's user pressed Pair (`true`) or Cancel.
pub async fn finish(out: &mut (impl AsyncWrite + Unpin), paired: bool) -> Result<(), LinkError> {
    let msg = if paired {
        Msg::PairDone {}
    } else {
        Msg::refused(Reason::Cancelled)
    };
    send(out, &msg).await
}

/// The companion's pairing code while it is shown, and what it has been through.
#[derive(Debug)]
pub struct Window {
    pub code: Code,
    pub opened: Instant,
    pub failures: u32,
}

impl Window {
    pub fn open() -> std::io::Result<Self> {
        Ok(Self {
            code: Code::generate()?,
            opened: Instant::now(),
            failures: 0,
        })
    }

    pub fn expired(&self) -> bool {
        self.opened.elapsed() >= CODE_LIFETIME
    }

    pub fn burned(&self) -> bool {
        self.failures >= MAX_FAILURES
    }

    pub fn left(&self) -> Duration {
        CODE_LIFETIME.saturating_sub(self.opened.elapsed())
    }

    /// Counts a wrong code; true when that burned it.
    pub fn failed(&mut self) -> bool {
        self.failures += 1;
        self.burned()
    }
}

/// One pairing attempt per address per [`ATTEMPT_EVERY`].
#[derive(Debug, Default)]
pub struct Attempts(HashMap<IpAddr, Instant>);

impl Attempts {
    /// True when `from` may try now (and counts it).
    pub fn admit(&mut self, from: IpAddr) -> bool {
        let now = Instant::now();
        self.0.retain(|_, at| now - *at < ATTEMPT_EVERY);
        if self.0.contains_key(&from) {
            return false;
        }
        self.0.insert(from, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, split};

    #[test]
    fn a_code_is_8_digits_and_reads_back_with_spaces() {
        let code = Code::generate().unwrap();
        assert_eq!(code.digits().len(), 8);
        assert!(code.digits().chars().all(|c| c.is_ascii_digit()));
        assert_eq!(Code::parse(&code.shown()), Some(code.clone()));
        assert_eq!(Code::parse("4821-7730").unwrap().shown(), "4821 7730");
        assert_eq!(Code::parse("4821 773"), None);
        assert_eq!(Code::parse("4821 773x"), None);
    }

    struct Outcome {
        game: Result<Paired, LinkError>,
        companion: Result<Paired, LinkError>,
    }

    async fn pair(
        typed: &Code,
        shown: &Code,
        versions: (&str, &str),
        bindings: ([u8; 32], [u8; 32]),
        press_pair: bool,
    ) -> Outcome {
        let (a, b) = duplex(1 << 16);
        let (gr, mut gw) = split(a);
        let (kr, mut kw) = split(b);
        let (mut gr, mut kr) = (Reader::new(gr), Reader::new(kr));
        let game_id = Identity::from_seed([1; 32]);
        let companion_id = Identity::from_seed([2; 32]);
        let game = dial(
            &mut gr,
            &mut gw,
            &game_id,
            "gamepc",
            versions.0,
            &bindings.0,
            typed,
        );
        let companion = async {
            let hello = match recv(&mut kr).await? {
                Msg::PairHello {
                    protocol,
                    app,
                    name,
                    spake,
                } => (protocol, app, name, spake),
                other => return Err(as_error(other)),
            };
            let paired = accept(
                &mut kr,
                &mut kw,
                hello,
                &companion_id,
                "laptop",
                versions.1,
                &bindings.1,
                shown,
            )
            .await?;
            finish(&mut kw, press_pair).await?;
            Ok(paired)
        };
        let (game, companion) = tokio::join!(game, companion);
        Outcome { game, companion }
    }

    #[tokio::test]
    async fn the_right_code_pairs_and_each_side_learns_the_others_key() {
        let code = Code::generate().unwrap();
        let o = pair(&code, &code, ("0.1.0", "0.1.0"), ([5; 32], [5; 32]), true).await;
        assert_eq!(
            o.game,
            Ok(Paired {
                name: "laptop".into(),
                key: Identity::from_seed([2; 32]).public()
            })
        );
        assert_eq!(
            o.companion,
            Ok(Paired {
                name: "gamepc".into(),
                key: Identity::from_seed([1; 32]).public()
            })
        );
    }

    #[tokio::test]
    async fn a_wrong_code_fails_on_both_sides_without_saying_which() {
        let typed = Code::parse("1111 1111").unwrap();
        let shown = Code::parse("1111 1112").unwrap();
        let o = pair(&typed, &shown, ("0.1.0", "0.1.0"), ([5; 32], [5; 32]), true).await;
        assert_eq!(o.game, Err(refused(Reason::PairingFailed)));
        assert_eq!(o.companion, Err(refused(Reason::PairingFailed)));
    }

    #[tokio::test]
    async fn a_party_in_the_middle_of_tls_cannot_pair_even_with_the_code() {
        let code = Code::generate().unwrap();
        let o = pair(&code, &code, ("0.1.0", "0.1.0"), ([5; 32], [6; 32]), true).await;
        assert!(o.game.is_err());
        assert_eq!(o.companion, Err(refused(Reason::PairingFailed)));
    }

    #[tokio::test]
    async fn cancel_on_the_companion_refuses_the_game_pc() {
        let code = Code::generate().unwrap();
        let o = pair(&code, &code, ("0.1.0", "0.1.0"), ([5; 32], [5; 32]), false).await;
        assert_eq!(o.game, Err(refused(Reason::Cancelled)));
        assert!(o.companion.is_ok());
    }

    #[tokio::test]
    async fn pairing_refuses_another_version_with_both_named() {
        let code = Code::generate().unwrap();
        let o = pair(&code, &code, ("0.1.0", "0.3.0"), ([5; 32], [5; 32]), true).await;
        let refusal = LinkError::Refused {
            reason: Reason::Version,
            game: Some("0.1.0".into()),
            companion: Some("0.3.0".into()),
        };
        assert_eq!(o.game, Err(refusal.clone()));
        assert_eq!(o.companion, Err(refusal));
    }

    #[tokio::test(start_paused = true)]
    async fn three_wrong_codes_burn_the_code_and_it_expires() {
        let mut window = Window::open().unwrap();
        assert!(!window.failed());
        assert!(!window.failed());
        assert!(window.failed());
        assert!(window.burned());
        assert!(!window.expired());
        tokio::time::advance(CODE_LIFETIME).await;
        assert!(window.expired());
        assert_eq!(window.left(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn one_attempt_per_address_every_5_s() {
        let mut attempts = Attempts::default();
        let a: IpAddr = [192, 168, 1, 5].into();
        let b: IpAddr = [192, 168, 1, 6].into();
        assert!(attempts.admit(a));
        assert!(!attempts.admit(a));
        assert!(attempts.admit(b));
        tokio::time::advance(ATTEMPT_EVERY).await;
        assert!(attempts.admit(a));
    }
}
