//! Fleet identity: an ed25519 key pair per member and per server, and the challenge both ways.
//!
//! An invite pins the server's long-lived identity key, not a TLS certificate, so it survives
//! certificate renewal and a move to another host. TLS only carries the connection.
//!
//! The exchange, after TLS:
//!
//! 1. Client: its key and a fresh client nonce ([`crate::Message::ClientHello`]).
//! 2. Server: its key, a [`Challenge`] (server nonce and timestamp), and a proof: the server's
//!    signature over the [`transcript`], which binds the client's nonce, so it cannot be replayed
//!    to another client ([`crate::Message::ServerHello`]).
//! 3. Client: checks the server key against the pin and the proof, then signs the same
//!    transcript for its own side ([`crate::Message::ClientProof`]).
//! 4. Server: [`ChallengeIssuer::accept`] checks the proof, that the challenge is one it issued
//!    and has not been answered, and that the answer came within [`CHALLENGE_WINDOW_MS`].

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// How long a challenge may be answered, and how far a server's timestamp may be from the
/// client's clock.
pub const CHALLENGE_WINDOW_MS: i64 = 30_000;
pub const NONCE_LEN: usize = 32;
/// Challenges a server holds open at once; past this, the oldest are dropped.
pub const MAX_PENDING: usize = 4096;
const TRANSCRIPT_TAG: &[u8] = b"fleet-proto identity v1\0";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("not a valid key")]
    BadKey,
    #[error("the server's identity key is not the one in the invite")]
    WrongServerKey,
    #[error("the signature does not match the key")]
    BadSignature,
    #[error("the challenge was not issued here or was already answered")]
    Replayed,
    #[error("the challenge is too old or its time is too far off")]
    Stale,
    #[error("no randomness available: {0}")]
    Random(String),
}

fn random<const N: usize>() -> Result<[u8; N], IdentityError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| IdentityError::Random(e.to_string()))?;
    Ok(bytes)
}

/// Bytes written as unpadded base64url, which is also a valid member id.
macro_rules! b64_bytes {
    ($name:ident, $len:expr) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = IdentityError;

            fn from_str(s: &str) -> Result<Self, IdentityError> {
                let bytes = URL_SAFE_NO_PAD
                    .decode(s)
                    .map_err(|_| IdentityError::BadKey)?;
                let bytes: [u8; $len] = bytes.try_into().map_err(|_| IdentityError::BadKey)?;
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = String::deserialize(d)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

/// A public identity key. Its text form is the member id on cards.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; 32]);
b64_bytes!(PublicKey, 32);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl PublicKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn verifying_key(&self) -> Result<VerifyingKey, IdentityError> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| IdentityError::BadKey)
    }

    /// A short fingerprint for people to compare, such as `3f2a-91c0-7d4e`: the first six bytes
    /// of the key's SHA-256.
    pub fn fingerprint(&self) -> String {
        let hash = Sha256::digest(self.0);
        hash[..6]
            .chunks(2)
            .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), IdentityError> {
        let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
        self.verifying_key()?
            .verify(message, &sig)
            .map_err(|_| IdentityError::BadSignature)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Nonce([u8; NONCE_LEN]);
b64_bytes!(Nonce, NONCE_LEN);

impl Nonce {
    pub fn random() -> Result<Self, IdentityError> {
        random().map(Self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; 64]);
b64_bytes!(Signature, 64);

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Signature({self})")
    }
}

/// A key pair. The secret half never leaves the side that made it.
pub struct Identity {
    key: SigningKey,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Identity({})", self.public())
    }
}

impl Identity {
    pub fn generate() -> Result<Self, IdentityError> {
        Ok(Self::from_seed(random()?))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// The secret seed, for the owner to store with user-only permissions.
    pub fn seed(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.key.verifying_key().to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.key.sign(message).to_bytes())
    }
}

/// What the server asks the client to sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub nonce: Nonce,
    /// When the server issued it, in milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Server,
    Client,
}

/// The bytes each side signs: which side, both nonces, the time, and both keys. A proof made for
/// one connection, side or key pair verifies for no other.
pub fn transcript(
    side: Side,
    challenge: &Challenge,
    client_nonce: &Nonce,
    server: &PublicKey,
    client: &PublicKey,
) -> Vec<u8> {
    let mut t = Vec::with_capacity(TRANSCRIPT_TAG.len() + 1 + 2 * NONCE_LEN + 8 + 64);
    t.extend_from_slice(TRANSCRIPT_TAG);
    t.push(match side {
        Side::Server => b'S',
        Side::Client => b'C',
    });
    t.extend_from_slice(&challenge.nonce.0);
    t.extend_from_slice(&client_nonce.0);
    t.extend_from_slice(&challenge.timestamp_ms.to_be_bytes());
    t.extend_from_slice(&server.0);
    t.extend_from_slice(&client.0);
    t
}

/// The client's side of step 3: checks the server, then makes the client's proof.
pub struct ClientHandshake<'a> {
    pub identity: &'a Identity,
    pub nonce: Nonce,
    /// The server key from the invite. `None` only for a first contact the user confirms by
    /// fingerprint.
    pub pinned: Option<PublicKey>,
}

impl ClientHandshake<'_> {
    pub fn answer(
        &self,
        server: &PublicKey,
        challenge: &Challenge,
        proof: &Signature,
        now_ms: i64,
    ) -> Result<Signature, IdentityError> {
        if self.pinned.is_some_and(|p| p != *server) {
            return Err(IdentityError::WrongServerKey);
        }
        if (now_ms - challenge.timestamp_ms).abs() > CHALLENGE_WINDOW_MS {
            return Err(IdentityError::Stale);
        }
        let client = self.identity.public();
        server.verify(
            &transcript(Side::Server, challenge, &self.nonce, server, &client),
            proof,
        )?;
        Ok(self.identity.sign(&transcript(
            Side::Client,
            challenge,
            &self.nonce,
            server,
            &client,
        )))
    }
}

/// The server's side: issues challenges and accepts each answer once, within the window.
#[derive(Default)]
pub struct ChallengeIssuer {
    pending: HashMap<Nonce, i64>,
}

impl ChallengeIssuer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh challenge, with the server's proof for this client (step 2).
    pub fn issue(
        &mut self,
        server: &Identity,
        client: &PublicKey,
        client_nonce: &Nonce,
        now_ms: i64,
    ) -> Result<(Challenge, Signature), IdentityError> {
        self.prune(now_ms);
        if self.pending.len() >= MAX_PENDING
            && let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, t)| **t)
                .map(|(n, _)| *n)
        {
            self.pending.remove(&oldest);
        }
        let challenge = Challenge {
            nonce: Nonce::random()?,
            timestamp_ms: now_ms,
        };
        self.pending.insert(challenge.nonce, now_ms);
        let proof = server.sign(&transcript(
            Side::Server,
            &challenge,
            client_nonce,
            &server.public(),
            client,
        ));
        Ok((challenge, proof))
    }

    /// The client's answer (step 4). A challenge is spent by its first answer, right or wrong.
    pub fn accept(
        &mut self,
        server: &PublicKey,
        client: &PublicKey,
        client_nonce: &Nonce,
        challenge: &Challenge,
        proof: &Signature,
        now_ms: i64,
    ) -> Result<(), IdentityError> {
        let issued = self
            .pending
            .remove(&challenge.nonce)
            .ok_or(IdentityError::Replayed)?;
        if issued != challenge.timestamp_ms || now_ms - issued > CHALLENGE_WINDOW_MS {
            return Err(IdentityError::Stale);
        }
        client.verify(
            &transcript(Side::Client, challenge, client_nonce, server, client),
            proof,
        )
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn prune(&mut self, now_ms: i64) {
        self.pending
            .retain(|_, issued| now_ms - *issued <= CHALLENGE_WINDOW_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    struct Setup {
        server: Identity,
        client: Identity,
        issuer: ChallengeIssuer,
        client_nonce: Nonce,
        challenge: Challenge,
        server_proof: Signature,
    }

    fn setup() -> Setup {
        let server = Identity::generate().unwrap();
        let client = Identity::generate().unwrap();
        let mut issuer = ChallengeIssuer::new();
        let client_nonce = Nonce::random().unwrap();
        let (challenge, server_proof) = issuer
            .issue(&server, &client.public(), &client_nonce, NOW)
            .unwrap();
        Setup {
            server,
            client,
            issuer,
            client_nonce,
            challenge,
            server_proof,
        }
    }

    impl Setup {
        fn client_answer(&self) -> Result<Signature, IdentityError> {
            ClientHandshake {
                identity: &self.client,
                nonce: self.client_nonce,
                pinned: Some(self.server.public()),
            }
            .answer(
                &self.server.public(),
                &self.challenge,
                &self.server_proof,
                NOW + 10,
            )
        }

        fn accept(&mut self, proof: &Signature, now: i64) -> Result<(), IdentityError> {
            self.issuer.accept(
                &self.server.public(),
                &self.client.public(),
                &self.client_nonce,
                &self.challenge,
                proof,
                now,
            )
        }
    }

    #[test]
    fn a_good_exchange_is_accepted_both_ways() {
        let mut s = setup();
        let proof = s.client_answer().unwrap();
        assert_eq!(s.accept(&proof, NOW + 20), Ok(()));
        assert_eq!(s.issuer.pending(), 0);
    }

    #[test]
    fn a_proof_from_the_wrong_key_is_refused() {
        let mut s = setup();
        let other = Identity::generate().unwrap();
        // The right transcript, claiming `client`'s key, signed by another key.
        let proof = other.sign(&transcript(
            Side::Client,
            &s.challenge,
            &s.client_nonce,
            &s.server.public(),
            &s.client.public(),
        ));
        assert_eq!(s.accept(&proof, NOW + 20), Err(IdentityError::BadSignature));
    }

    #[test]
    fn a_server_that_is_not_the_pinned_one_is_refused() {
        let s = setup();
        let impostor = Identity::generate().unwrap();
        let hs = ClientHandshake {
            identity: &s.client,
            nonce: s.client_nonce,
            pinned: Some(s.server.public()),
        };
        assert_eq!(
            hs.answer(&impostor.public(), &s.challenge, &s.server_proof, NOW),
            Err(IdentityError::WrongServerKey)
        );
        // An impostor that copies the real server's key cannot sign for it.
        let forged = impostor.sign(&transcript(
            Side::Server,
            &s.challenge,
            &s.client_nonce,
            &s.server.public(),
            &s.client.public(),
        ));
        assert_eq!(
            hs.answer(&s.server.public(), &s.challenge, &forged, NOW),
            Err(IdentityError::BadSignature)
        );
    }

    #[test]
    fn a_replayed_answer_is_refused() {
        let mut s = setup();
        let proof = s.client_answer().unwrap();
        assert_eq!(s.accept(&proof, NOW + 20), Ok(()));
        assert_eq!(s.accept(&proof, NOW + 30), Err(IdentityError::Replayed));
    }

    #[test]
    fn a_stale_answer_is_refused() {
        let mut s = setup();
        let proof = s.client_answer().unwrap();
        assert_eq!(
            s.accept(&proof, NOW + CHALLENGE_WINDOW_MS + 1),
            Err(IdentityError::Stale)
        );
        // A challenge whose timestamp was altered is stale too.
        let mut s = setup();
        let proof = s.client_answer().unwrap();
        s.challenge.timestamp_ms += 1;
        assert_eq!(s.accept(&proof, NOW + 20), Err(IdentityError::Stale));
    }

    #[test]
    fn a_client_refuses_a_server_clock_far_off() {
        let s = setup();
        let hs = ClientHandshake {
            identity: &s.client,
            nonce: s.client_nonce,
            pinned: Some(s.server.public()),
        };
        assert_eq!(
            hs.answer(
                &s.server.public(),
                &s.challenge,
                &s.server_proof,
                NOW - CHALLENGE_WINDOW_MS - 1
            ),
            Err(IdentityError::Stale)
        );
    }

    #[test]
    fn expired_challenges_are_pruned() {
        let mut s = setup();
        let n = Nonce::random().unwrap();
        s.issuer
            .issue(
                &s.server,
                &s.client.public(),
                &n,
                NOW + CHALLENGE_WINDOW_MS + 1,
            )
            .unwrap();
        assert_eq!(s.issuer.pending(), 1);
    }

    #[test]
    fn keys_round_trip_as_text_and_are_member_ids() {
        let id = Identity::from_seed([7; 32]);
        let text = id.public().to_string();
        assert!(crate::card::is_member_id(&text));
        assert_eq!(text.parse::<PublicKey>().unwrap(), id.public());
        assert_eq!(Identity::from_seed(id.seed()).public(), id.public());
        assert_eq!("short".parse::<PublicKey>(), Err(IdentityError::BadKey));
        let fp = id.public().fingerprint();
        assert_eq!(fp.len(), 14);
        assert_eq!(fp.matches('-').count(), 2);
    }
}
