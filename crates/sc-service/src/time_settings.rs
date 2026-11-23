//! How the dashboard shows times: the time zone (the computer's own, or an IANA zone) and a 12 or
//! 24 hour clock. Kept as `"time"` in `app-settings.json` in the settings folder, the desktop
//! app's own file; its other fields are left as they are. The page does the formatting.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;

pub use crate::app_settings::FILE;
use crate::app_settings::write_field;
use crate::time_zones::ZONES;
/// The zone the computer is set to.
pub const AUTO: &str = "auto";
/// Longer than any IANA name.
pub const MAX_ZONE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Clock {
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "12h")]
    H12,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeSettings {
    /// [`AUTO`] or a name in [`ZONES`].
    pub zone: String,
    pub clock: Clock,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            zone: AUTO.to_owned(),
            clock: Clock::H24,
        }
    }
}

fn check_zone(v: Option<&Value>) -> Result<String, String> {
    let Some(Value::String(zone)) = v else {
        return Err("The time zone must be auto or a time zone name".to_owned());
    };
    if zone.len() > MAX_ZONE {
        return Err(format!("The time zone name is over {MAX_ZONE} characters"));
    }
    if zone == AUTO || ZONES.contains(&zone.as_str()) {
        Ok(zone.clone())
    } else {
        Err(format!(
            "Unknown time zone \"{zone}\": pick one from the list"
        ))
    }
}

fn check_clock(v: Option<&Value>) -> Result<Clock, String> {
    match v.and_then(Value::as_str) {
        Some("24h") => Ok(Clock::H24),
        Some("12h") => Ok(Clock::H12),
        _ => Err("The clock must be 12h or 24h".to_owned()),
    }
}

impl TimeSettings {
    /// What a page asked for; every problem with it, or the settings.
    pub fn check(v: &Value) -> Result<Self, Vec<String>> {
        let zone = check_zone(v.get("zone"));
        let clock = check_clock(v.get("clock"));
        match (zone, clock) {
            (Ok(zone), Ok(clock)) => Ok(Self { zone, clock }),
            (zone, clock) => Err([zone.err(), clock.err()].into_iter().flatten().collect()),
        }
    }

    /// The `"time"` field of the file, read leniently: a missing or bad field is the default.
    pub fn from_file(text: &str) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut problems = vec![];
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
            return (settings, problems);
        };
        let Some(time) = obj.get("time") else {
            return (settings, problems);
        };
        if time.get("zone").is_some() {
            match check_zone(time.get("zone")) {
                Ok(zone) => settings.zone = zone,
                Err(p) => problems.push(p),
            }
        }
        if time.get("clock").is_some() {
            match check_clock(time.get("clock")) {
                Ok(clock) => settings.clock = clock,
                Err(p) => problems.push(p),
            }
        }
        (settings, problems)
    }
}

/// The time settings, kept in the file when there is one.
pub struct TimeStore {
    file: Option<PathBuf>,
    current: Mutex<TimeSettings>,
    changes: watch::Sender<u64>,
}

impl TimeStore {
    pub fn new(file: Option<PathBuf>) -> Self {
        let text = file.as_deref().and_then(|f| fs::read_to_string(f).ok());
        let (current, problems) = text
            .as_deref()
            .map(TimeSettings::from_file)
            .unwrap_or_default();
        for p in problems {
            tracing::warn!("[settings] {FILE}: {p}; the default is used");
        }
        Self {
            file,
            current: Mutex::new(current),
            changes: watch::channel(0).0,
        }
    }

    pub fn get(&self) -> TimeSettings {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Bumps whenever the settings change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Checks and saves what a page asked for; the problems, empty when it was saved.
    pub fn set(&self, v: &Value) -> io::Result<Vec<String>> {
        let next = match TimeSettings::check(v) {
            Ok(next) => next,
            Err(problems) => return Ok(problems),
        };
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = &self.file {
            write_field(file, "time", json!(next))?;
        }
        *current = next;
        drop(current);
        self.changes.send_modify(|n| *n += 1);
        Ok(vec![])
    }

    /// The push to dashboard pages: the settings and the zones to pick from.
    pub fn push(&self) -> Value {
        json!({ "type": "time", "time": self.get(), "zones": ZONES })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_24h_unless_the_file_says_otherwise() {
        assert_eq!(
            TimeSettings::from_file("{}"),
            (TimeSettings::default(), vec![])
        );
        assert_eq!(TimeSettings::default().zone, "auto");
        let (s, problems) = TimeSettings::from_file(
            r#"{ "splash": false, "time": { "zone": "America/Denver", "clock": "12h" } }"#,
        );
        assert_eq!(
            (s.zone.as_str(), s.clock, problems.len()),
            ("America/Denver", Clock::H12, 0)
        );
    }

    #[test]
    fn a_bad_field_in_the_file_falls_back_alone() {
        let (s, problems) =
            TimeSettings::from_file(r#"{ "time": { "zone": "Mars/Olympus", "clock": "12h" } }"#);
        assert_eq!((s.zone.as_str(), s.clock), ("auto", Clock::H12));
        assert_eq!(
            problems,
            ["Unknown time zone \"Mars/Olympus\": pick one from the list"]
        );
        assert_eq!(
            TimeSettings::from_file("not json").0,
            TimeSettings::default()
        );
    }

    #[test]
    fn a_page_may_name_only_known_zones_and_clocks() {
        for zone in [
            "auto",
            "UTC",
            "America/Denver",
            "Europe/London",
            "Asia/Kolkata",
        ] {
            assert!(
                TimeSettings::check(&json!({ "zone": zone, "clock": "24h" })).is_ok(),
                "{zone}"
            );
        }
        let long = "A".repeat(MAX_ZONE + 1);
        for zone in [
            json!("america/denver"),
            json!(""),
            json!(5),
            json!(long),
            json!("<b>"),
        ] {
            assert_eq!(
                TimeSettings::check(&json!({ "zone": zone, "clock": "24h" }))
                    .unwrap_err()
                    .len(),
                1,
                "{zone}"
            );
        }
        assert_eq!(
            TimeSettings::check(&json!({ "zone": "Nowhere", "clock": "13h" })).unwrap_err(),
            [
                "Unknown time zone \"Nowhere\": pick one from the list",
                "The clock must be 12h or 24h"
            ]
        );
    }

    #[test]
    fn saving_keeps_the_other_fields_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        fs::write(&file, r#"{ "splash": false }"#).unwrap();
        let store = TimeStore::new(Some(file.clone()));
        let mut changes = store.subscribe();
        assert_eq!(store.get(), TimeSettings::default());
        assert_eq!(
            store
                .set(&json!({ "zone": "Pacific/Auckland", "clock": "12h" }))
                .unwrap(),
            Vec::<String>::new()
        );
        assert!(changes.has_changed().unwrap());
        let saved: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({ "splash": false, "time": { "zone": "Pacific/Auckland", "clock": "12h" } })
        );
        assert_eq!(
            TimeStore::new(Some(file.clone())).get().zone,
            "Pacific/Auckland"
        );

        changes.mark_unchanged();
        assert_eq!(
            store
                .set(&json!({ "zone": "Nowhere", "clock": "12h" }))
                .unwrap()
                .len(),
            1
        );
        assert!(!changes.has_changed().unwrap());
        assert_eq!(store.get().zone, "Pacific/Auckland");
    }

    #[test]
    fn the_zone_list_is_sorted_iana_names() {
        assert!(ZONES.windows(2).all(|w| w[0] < w[1]));
        assert!(
            ZONES
                .iter()
                .all(|z| z.len() <= MAX_ZONE && !z.contains(' '))
        );
        assert!(ZONES.contains(&"America/Denver") && ZONES.contains(&"UTC"));
    }
}
