//! The per-launch secrets: 32 random bytes as 64 hex digits, compared in constant time.

use std::fmt;

use subtle::ConstantTimeEq;

#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// # Panics
    /// When the operating system has no random source.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Only the length leaks, and every token has the same length.
    pub fn matches(&self, given: &str) -> bool {
        given.len() == self.0.len() && bool::from(given.as_bytes().ct_eq(self.0.as_bytes()))
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_64_hex_digits_and_matches_only_itself() {
        let t = Token::generate();
        assert_eq!(t.as_str().len(), 64);
        assert!(
            t.as_str()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert!(t.matches(t.as_str()));
        assert!(!t.matches(&"0".repeat(64)));
        assert!(!t.matches("short"));
        assert_ne!(t, Token::generate());
    }
}
