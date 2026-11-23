//! The invite a member pastes, in one of two forms:
//!
//! - hosted (CIC relay): `cic://<host>:<port>?key=<server key>&connect=<connection token>`, with
//!   `&event=<event id>` when the token is the user's for one event;
//! - peer to peer (a host player's app): `cic://<host>:<port>?key=<server key>&code=<join code>`.
//!
//! It pins the server's identity key, never a TLS certificate (see [`crate::identity`]). The
//! token or code is spent on first use; an app keeps the address and key, never either.

use std::fmt;
use std::net::Ipv6Addr;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::identity::{IdentityError, PublicKey};

pub const SCHEME: &str = "cic";
/// A longer invite is refused unread.
pub const MAX_INVITE: usize = 512;
/// A join code is 1 to 64 of `A-Z a-z 0-9 _ -`.
pub const MAX_CODE: usize = 64;
const MAX_HOST: usize = 253;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InviteError {
    #[error("an invite starts with cic://")]
    Scheme,
    #[error("the invite is too long")]
    TooLong,
    #[error("the invite has no valid host")]
    Host,
    #[error("the invite has no valid port (1 to 65535)")]
    Port,
    #[error("the invite has no valid server key")]
    Key,
    #[error("the invite has no valid join code")]
    Code,
    #[error("the invite has no valid connection code")]
    Connect,
    #[error("the invite has no valid event id")]
    Event,
    #[error("the invite has a join code and a connection code")]
    Both,
    #[error("the invite names {0} twice")]
    Repeated(&'static str),
}

/// A host name, an IPv4 address, or an IPv6 address (written in brackets in the invite).
pub fn is_host(s: &str) -> bool {
    if s.parse::<Ipv6Addr>().is_ok() {
        return true;
    }
    (1..=MAX_HOST).contains(&s.len())
        && s.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

pub fn is_code(s: &str) -> bool {
    (1..=MAX_CODE).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A peer-to-peer join code's random characters; written in groups of four (`ABCD-EFGH-...`).
pub const JOIN_CODE_CHARS: usize = 16;
const JOIN_CODE_ALPHABET: &[u8; 62] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// A new join code for a host: 16 characters of `A-Z a-z 0-9` (about 95 bits) from the system's
/// CSPRNG, as the identity key, in groups of four joined by `-`.
pub fn new_join_code() -> Result<String, IdentityError> {
    let mut out = String::with_capacity(JOIN_CODE_CHARS + JOIN_CODE_CHARS / 4);
    let mut taken = 0;
    while taken < JOIN_CODE_CHARS {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| IdentityError::Random(e.to_string()))?;
        // 248 is 4 * 62: below it every character is equally likely.
        for b in bytes.into_iter().filter(|b| *b < 248) {
            if taken == JOIN_CODE_CHARS {
                break;
            }
            if taken > 0 && taken % 4 == 0 {
                out.push('-');
            }
            out.push(char::from(JOIN_CODE_ALPHABET[usize::from(b % 62)]));
            taken += 1;
        }
    }
    Ok(out)
}

/// A connection token is 32 random bytes, base64url without padding (43 characters).
pub const CONNECT_TOKEN_LEN: usize = 43;

pub fn is_connect_token(s: &str) -> bool {
    s.len() == CONNECT_TOKEN_LEN
        && URL_SAFE_NO_PAD
            .decode(s)
            .is_ok_and(|b| b.len() == 32 && URL_SAFE_NO_PAD.encode(&b) == s)
}

/// A new connection token, for the relay to issue.
pub fn new_connect_token() -> Result<String, IdentityError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| IdentityError::Random(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// What the relay stores instead of the token.
pub fn connect_token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub host: String,
    pub port: u16,
    /// The server's identity key, pinned for every connection after.
    pub key: PublicKey,
    /// Exactly one of `code` (peer to peer) and `connect` (hosted).
    pub code: Option<String>,
    pub connect: Option<String>,
    /// With `connect` only: the event a per-event token binds to.
    pub event: Option<String>,
}

impl FromStr for Invite {
    type Err = InviteError;

    fn from_str(text: &str) -> Result<Self, InviteError> {
        let text = text.trim();
        if text.len() > MAX_INVITE {
            return Err(InviteError::TooLong);
        }
        let rest = text
            .split_once("://")
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
            .map(|(_, rest)| rest)
            .ok_or(InviteError::Scheme)?;
        let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, port) = v6.split_once("]:").ok_or(InviteError::Port)?;
            host.parse::<Ipv6Addr>().map_err(|_| InviteError::Host)?;
            (host, port)
        } else {
            let (host, port) = authority.rsplit_once(':').ok_or(InviteError::Port)?;
            // An IPv6 address goes in brackets.
            if host.contains(':') {
                return Err(InviteError::Host);
            }
            (host, port)
        };
        if !is_host(host) {
            return Err(InviteError::Host);
        }
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0 && !port.starts_with('+'))
            .ok_or(InviteError::Port)?;

        let (mut key, mut code, mut connect, mut event) = (None, None, None, None);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let slot = match name {
                "key" if key.is_some() => return Err(InviteError::Repeated("key")),
                "key" => {
                    key = Some(value.parse().map_err(|_| InviteError::Key)?);
                    continue;
                }
                "code" => (&mut code, "code"),
                "connect" => (&mut connect, "connect"),
                "event" => (&mut event, "event"),
                // A newer invite may say more; an older app ignores it.
                _ => continue,
            };
            if slot.0.replace(value).is_some() {
                return Err(InviteError::Repeated(slot.1));
            }
        }
        let key = key.ok_or(InviteError::Key)?;
        let (code, connect, event) = match (code, connect) {
            (Some(_), Some(_)) => return Err(InviteError::Both),
            (Some(c), None) if event.is_none() => (
                Some(Some(c).filter(|c| is_code(c)).ok_or(InviteError::Code)?),
                None,
                None,
            ),
            (Some(_), None) => return Err(InviteError::Event),
            (None, Some(t)) => (
                None,
                Some(
                    Some(t)
                        .filter(|t| is_connect_token(t))
                        .ok_or(InviteError::Connect)?,
                ),
                event
                    .map(|e| {
                        Some(e)
                            .filter(|e| crate::structure::is_id(e))
                            .ok_or(InviteError::Event)
                    })
                    .transpose()?,
            ),
            (None, None) => return Err(InviteError::Code),
        };
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
            key,
            code: code.map(str::to_owned),
            connect: connect.map(str::to_owned),
            event: event.map(str::to_owned),
        })
    }
}

impl fmt::Display for Invite {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        write!(f, "{SCHEME}://{host}:{}?key={}", self.port, self.key)?;
        if let Some(code) = &self.code {
            write!(f, "&code={code}")?;
        }
        if let Some(token) = &self.connect {
            write!(f, "&connect={token}")?;
        }
        if let Some(event) = &self.event {
            write!(f, "&event={event}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn key() -> PublicKey {
        Identity::from_seed([9; 32]).public()
    }

    #[test]
    fn a_good_invite_round_trips() {
        let text = format!("cic://relay.example.org:7443?key={}&code=Op-1_abc", key());
        let invite: Invite = text.parse().unwrap();
        assert_eq!(invite.host, "relay.example.org");
        assert_eq!(invite.port, 7443);
        assert_eq!(invite.key, key());
        assert_eq!(invite.code.as_deref(), Some("Op-1_abc"));
        assert_eq!(invite.connect, None);
        assert_eq!(invite.to_string(), text);

        let v6: Invite = format!("  CIC://[::1]:9000/?code=x&key={}&later=1 ", key())
            .parse()
            .unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(
            v6.to_string(),
            format!("cic://[::1]:9000?key={}&code=x", key())
        );
        let v4: Invite = format!("cic://127.0.0.1:1?key={}&code=a", key())
            .parse()
            .unwrap();
        assert_eq!(v4.host, "127.0.0.1");
    }

    #[test]
    fn a_connection_invite_round_trips_with_or_without_an_event() {
        let token = new_connect_token().unwrap();
        assert!(is_connect_token(&token));
        assert_ne!(token, new_connect_token().unwrap());
        let text = format!("cic://cic.example.org:7443?key={}&connect={token}", key());
        let invite: Invite = text.parse().unwrap();
        assert_eq!(invite.connect.as_deref(), Some(token.as_str()));
        assert_eq!(
            (invite.code.as_deref(), invite.event.as_deref()),
            (None, None)
        );
        assert_eq!(invite.to_string(), text);

        let text = format!("{text}&event=op-1");
        let invite: Invite = text.parse().unwrap();
        assert_eq!(invite.event.as_deref(), Some("op-1"));
        assert_eq!(invite.to_string(), text);

        assert_eq!(connect_token_digest(&token), connect_token_digest(&token));
        assert_ne!(connect_token_digest(&token), connect_token_digest("other"));
    }

    #[test]
    fn a_join_code_is_sixteen_characters_in_groups_of_four() {
        let a = new_join_code().unwrap();
        let b = new_join_code().unwrap();
        assert_ne!(a, b);
        for code in [&a, &b] {
            assert!(is_code(code), "{code}");
            let groups: Vec<&str> = code.split('-').collect();
            assert_eq!(groups.len(), 4, "{code}");
            assert!(
                groups
                    .iter()
                    .all(|g| g.len() == 4 && g.bytes().all(|c| c.is_ascii_alphanumeric()))
            );
        }
        let invite: Invite = format!("cic://h:1?key={}&code={a}", key()).parse().unwrap();
        assert_eq!(invite.code.as_deref(), Some(a.as_str()));
    }

    #[test]
    fn a_connection_token_is_256_bits_of_base64url() {
        let good = URL_SAFE_NO_PAD.encode([7u8; 32]);
        assert!(is_connect_token(&good));
        assert!(!is_connect_token(&good[1..]));
        assert!(!is_connect_token(&format!("{good}A")));
        assert!(!is_connect_token(&good.replace('H', "+")));
        assert!(!is_connect_token(&format!("{}=", &good[..42])));
        // The last character carries 2 spare bits, which must be zero.
        assert!(!is_connect_token(&format!("{}B", &good[..42])));
    }

    #[test]
    fn a_bad_invite_says_what_is_wrong() {
        let t = URL_SAFE_NO_PAD.encode([1u8; 32]);
        let k = key();
        for (text, err) in [
            (format!("https://h:1?key={k}&code=a"), InviteError::Scheme),
            (format!("h:1?key={k}&code=a"), InviteError::Scheme),
            (format!("cic://:1?key={k}&code=a"), InviteError::Host),
            (
                format!("cic://bad_host:1?key={k}&code=a"),
                InviteError::Host,
            ),
            (format!("cic://-h:1?key={k}&code=a"), InviteError::Host),
            (format!("cic://::1:5?key={k}&code=a"), InviteError::Host),
            (format!("cic://h?key={k}&code=a"), InviteError::Port),
            (format!("cic://h:0?key={k}&code=a"), InviteError::Port),
            (format!("cic://h:70000?key={k}&code=a"), InviteError::Port),
            (format!("cic://h:+5?key={k}&code=a"), InviteError::Port),
            ("cic://h:1?key=short&code=a".to_owned(), InviteError::Key),
            ("cic://h:1?code=a".to_owned(), InviteError::Key),
            (format!("cic://h:1?key={k}"), InviteError::Code),
            (format!("cic://h:1?key={k}&code="), InviteError::Code),
            (format!("cic://h:1?key={k}&code=a%20b"), InviteError::Code),
            (
                format!("cic://h:1?key={k}&key={k}&code=a"),
                InviteError::Repeated("key"),
            ),
            (
                format!("cic://h:1?key={k}&code=a&code=b"),
                InviteError::Repeated("code"),
            ),
            (
                format!("cic://h:1?key={k}&code={}", "a".repeat(600)),
                InviteError::TooLong,
            ),
            (
                format!("cic://h:1?key={k}&connect=abc"),
                InviteError::Connect,
            ),
            (format!("cic://h:1?key={k}&connect="), InviteError::Connect),
            (
                format!("cic://h:1?key={k}&code=a&connect={t}"),
                InviteError::Both,
            ),
            (
                format!("cic://h:1?key={k}&connect={t}&event=Op!"),
                InviteError::Event,
            ),
            (
                format!("cic://h:1?key={k}&code=a&event=op"),
                InviteError::Event,
            ),
            (
                format!("cic://h:1?key={k}&connect={t}&connect={t}"),
                InviteError::Repeated("connect"),
            ),
            (
                format!("cic://h:1?key={k}&connect={t}&event=a&event=b"),
                InviteError::Repeated("event"),
            ),
        ] {
            assert_eq!(text.parse::<Invite>(), Err(err.clone()), "{text}");
        }
    }
}
