//! The CIC portal's address, from the `portal` parameter a CIC relay adds to its connection
//! strings (`cic://...&portal=<percent-encoded https base>`), so the panel can link to an event's
//! page there. fleet-proto's invite parser skips the parameter; it is read here.

/// The longest portal address taken, as the relay caps it.
pub const MAX_PORTAL: usize = 200;
/// The longest address the app opens.
pub const MAX_OPEN_URL: usize = 512;

/// The portal address in a pasted connection string, checked; `None` when absent or unsafe.
pub fn portal_of(invite: &str) -> Option<String> {
    let (_, query) = invite.trim().split_once('?')?;
    let mut found = query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("portal="));
    let raw = found.next()?;
    if found.next().is_some() {
        return None;
    }
    portal_url(&percent_decode(raw)?)
}

/// A portal base address: `https://host[:port][/path]`, without a trailing `/`.
pub fn portal_url(text: &str) -> Option<String> {
    let text = text.trim_end_matches('/');
    (text.len() <= MAX_PORTAL && is_https(text)).then(|| text.to_owned())
}

/// An address the app may open in the system browser.
pub fn is_openable(url: &str) -> bool {
    url.len() <= MAX_OPEN_URL && is_https(url)
}

/// The event's page in the portal.
pub fn event_url(portal: &str, event: &str) -> Option<String> {
    fleet_proto::structure::is_id(event)
        .then(|| format!("{portal}/events/{event}"))
        .filter(|u| is_openable(u))
}

/// `https://` with a plain host and optional port, then a path of plain characters: no
/// credentials, query, fragment, spaces or quotes.
fn is_https(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.ends_with(']') => {
            if !port.parse::<u16>().is_ok_and(|p| p > 0) {
                return false;
            }
            host
        }
        _ => authority,
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .map_or(Some(host), |v6| {
            v6.parse::<std::net::Ipv6Addr>().is_ok().then_some(v6)
        });
    host.is_some_and(fleet_proto::invite::is_host)
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~/%".contains(&b))
        && valid_escapes(path)
}

fn valid_escapes(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.iter().enumerate().all(|(i, b)| {
        *b != b'%'
            || bytes
                .get(i + 1..i + 3)
                .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))
    })
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "cic://cic.example.org:7443?key=k&connect=t";

    #[test]
    fn reads_the_portal_from_a_connection_string() {
        let with = |p: &str| portal_of(&format!("{BASE}&portal={p}"));
        assert_eq!(
            with("https%3A%2F%2Fcic.example.org").as_deref(),
            Some("https://cic.example.org")
        );
        assert_eq!(
            with("https%3A%2F%2Fcic.example.org%3A8443%2Fportal%2F").as_deref(),
            Some("https://cic.example.org:8443/portal")
        );
        assert_eq!(
            with("https%3A%2F%2F%5B%3A%3A1%5D%3A8443").as_deref(),
            Some("https://[::1]:8443")
        );
        assert_eq!(portal_of(BASE), None, "an older string has none");
        assert_eq!(portal_of("cic://h:1"), None);
    }

    #[test]
    fn refuses_a_portal_that_is_not_plain_https() {
        let with = |p: &str| portal_of(&format!("{BASE}&portal={p}"));
        for bad in [
            "http%3A%2F%2Fcic.example.org",
            "javascript%3Aalert(1)",
            "https%3A%2F%2Fuser%3Apass%40cic.example.org",
            "https%3A%2F%2Fcic.example.org%2F%3Fq%3D1",
            "https%3A%2F%2Fcic.example.org%2F%23x",
            "https%3A%2F%2Fcic.example.org%2Fa%20b",
            "https%3A%2F%2Fcic.example.org%2F%22",
            "https%3A%2F%2F",
            "https%3A%2F%2Fcic.example.org%3A0",
            "https%3A%2F%2Fcic.example.org%3Ax",
            "https%3A%2F%2Fbad_host",
            "%ZZ",
            "%FF",
            "junk",
            "",
        ] {
            assert_eq!(with(bad), None, "{bad}");
        }
        let long = format!("https%3A%2F%2Fcic.example.org%2F{}", "a".repeat(MAX_PORTAL));
        assert_eq!(with(&long), None, "too long");
        let twice = format!("{BASE}&portal=https%3A%2F%2Fa.org&portal=https%3A%2F%2Fb.org");
        assert_eq!(portal_of(&twice), None, "named twice");
    }

    #[test]
    fn builds_the_event_page_address() {
        assert_eq!(
            event_url("https://cic.example.org", "op-1").as_deref(),
            Some("https://cic.example.org/events/op-1")
        );
        assert_eq!(event_url("https://cic.example.org", "../x"), None);
        assert!(is_openable("https://cic.example.org/events/op-1"));
        assert!(!is_openable("http://cic.example.org/events/op-1"));
        assert!(!is_openable("https://cic.example.org/events/op-1 x"));
        assert!(!is_openable("file:///c:/windows"));
    }
}
