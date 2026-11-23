//! Hosted invites, one per member: `cic://host:port?key=K&connect=<token>[&event=<id>]`, as the
//! CIC portal's "Connect Hardpoint" makes them. The token is a secret: it never reaches a log,
//! an error message or a file (only its SHA-256 digest is kept, to know it was spent).

use std::fmt;
use std::str::FromStr;

use fleet_proto::invite::connect_token_digest;
use fleet_proto::{Invite, PublicKey};

#[derive(Clone, PartialEq, Eq)]
pub struct HostedInvite(Invite);

impl HostedInvite {
    pub fn host(&self) -> &str {
        &self.0.host
    }

    pub fn port(&self) -> u16 {
        self.0.port
    }

    pub fn key(&self) -> PublicKey {
        self.0.key
    }

    /// The event of a per-event token.
    pub fn event(&self) -> Option<&str> {
        self.0.event.as_deref()
    }

    pub(crate) fn token(&self) -> &str {
        self.0.connect.as_deref().unwrap_or_default()
    }

    pub fn digest(&self) -> [u8; 32] {
        connect_token_digest(self.token())
    }
}

impl fmt::Debug for HostedInvite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = if self.0.host.contains(':') {
            format!("[{}]", self.0.host)
        } else {
            self.0.host.clone()
        };
        write!(
            f,
            "cic://{host}:{}?key={}&connect=<hidden>",
            self.0.port, self.0.key
        )?;
        if let Some(event) = &self.0.event {
            write!(f, "&event={event}")?;
        }
        Ok(())
    }
}

impl FromStr for HostedInvite {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invite: Invite = text.parse().map_err(|e| format!("{e}"))?;
        if invite.connect.is_none() {
            return Err(
                "a peer-to-peer invite (code=), not a connection code: pass it with --invite"
                    .to_owned(),
            );
        }
        Ok(Self(invite))
    }
}

/// Whether an invite is a hosted one (`connect=`), without reading it further.
pub fn is_hosted(text: &str) -> bool {
    text.split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|p| p.starts_with("connect=")))
}

/// One invite per line; blank lines and lines starting with `#` are skipped. A problem names
/// its line, never its text.
pub fn parse_lines(text: &str) -> Result<Vec<HostedInvite>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(line.parse().map_err(|e| format!("line {}: {e}", n + 1))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_proto::Identity;
    use fleet_proto::invite::new_connect_token;

    fn key() -> PublicKey {
        Identity::from_seed([3; 32]).public()
    }

    #[test]
    fn a_hosted_invite_is_read_and_its_token_never_shown() {
        let token = new_connect_token().unwrap();
        let text = format!("cic://cic.example.org:7443?key={}&connect={token}", key());
        let invite: HostedInvite = text.parse().unwrap();
        assert_eq!(invite.host(), "cic.example.org");
        assert_eq!(invite.port(), 7443);
        assert_eq!(invite.key(), key());
        assert_eq!(invite.event(), None);
        assert_eq!(invite.token(), token);
        assert_eq!(invite.digest(), connect_token_digest(&token));
        let shown = format!("{invite:?}");
        assert!(
            !shown.contains(&token) && shown.contains("<hidden>"),
            "{shown}"
        );

        let per_event: HostedInvite = format!("{text}&event=op-1").parse().unwrap();
        assert_eq!(per_event.event(), Some("op-1"));
        assert!(format!("{per_event:?}").ends_with("&event=op-1"));
        assert!(is_hosted(&text));
    }

    #[test]
    fn a_code_invite_or_a_bad_token_is_refused() {
        let code = format!("cic://cic.example.org:7443?key={}&code=abc", key());
        assert!(!is_hosted(&code));
        assert!(
            code.parse::<HostedInvite>()
                .unwrap_err()
                .contains("--invite")
        );
        let short = format!("cic://cic.example.org:7443?key={}&connect=abc", key());
        let e = short.parse::<HostedInvite>().unwrap_err();
        assert!(e.contains("connection code") && !e.contains("abc"), "{e}");
    }

    #[test]
    fn lines_skip_comments_and_a_bad_line_is_named_without_its_text() {
        let good = |t: &str| format!("cic://cic.example.org:7443?key={}&connect={t}", key());
        let (a, b) = (new_connect_token().unwrap(), new_connect_token().unwrap());
        let text = format!("# demo members\n\n{}\n  {}  \n", good(&a), good(&b));
        let invites = parse_lines(&text).unwrap();
        assert_eq!(invites.len(), 2);
        assert_eq!(invites[1].token(), b);

        let secret = "x".repeat(43);
        let bad = format!("{}\n{}\n", good(&a), good(&secret));
        let e = parse_lines(&bad).unwrap_err();
        assert!(e.starts_with("line 2:") && !e.contains(&secret), "{e}");
        assert!(parse_lines("# nothing\n").unwrap().is_empty());
    }
}
