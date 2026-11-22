//! The app's own settings file, `app-settings.json` in the settings folder, beside the service's
//! files. Read leniently: a missing or unreadable file, or a field that is not understood, falls
//! back to the default for that field.

use std::path::Path;

use serde_json::Value;

pub const FILE: &str = "app-settings.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    /// Show the splash at launch (`"splash": false` turns it off).
    pub splash: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { splash: true }
    }
}

impl AppSettings {
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut problems = vec![];
        let mut settings = Self::default();
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
            problems.push("not a JSON object; the defaults are used".to_owned());
            return (settings, problems);
        };
        match obj.get("splash") {
            None => {}
            Some(Value::Bool(b)) => settings.splash = *b,
            Some(_) => problems.push("splash must be true or false".to_owned()),
        }
        (settings, problems)
    }

    /// The file in `dir`; the defaults when it is not there. Problems are logged.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let (settings, problems) = Self::parse(&text);
        for p in problems {
            tracing::warn!("[settings] {}: {p}", path.display());
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splash_is_on_unless_the_file_turns_it_off() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AppSettings::load(dir.path()).splash);
        std::fs::write(dir.path().join(FILE), r#"{ "splash": false, "future": 1 }"#).unwrap();
        assert!(!AppSettings::load(dir.path()).splash);
    }

    #[test]
    fn a_bad_file_or_field_falls_back_with_a_reason() {
        assert_eq!(
            AppSettings::parse("[1]"),
            (
                AppSettings::default(),
                vec!["not a JSON object; the defaults are used".to_owned()]
            )
        );
        assert_eq!(
            AppSettings::parse(r#"{ "splash": "no" }"#),
            (
                AppSettings::default(),
                vec!["splash must be true or false".to_owned()]
            )
        );
    }
}
