//! The names the user gives ships, by class name, e.g. "Old Betsy" for DRAK_Corsair. Kept as
//! `"shipNames"` in `app-settings.json` in the settings folder. The core's state holds a copy,
//! so the page, the fleet card and the portal all show the same name; clearing one brings back
//! the derived name.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use tokio::sync::watch;

use sc_core::js;
use sc_core::state::StateModel;

use crate::app_settings::{FILE, write_field};
use crate::dashboard::{MAX_CLASS_NAME, is_plain};

pub const FIELD: &str = "shipNames";
/// About what the status strip fits.
pub const MAX_NAME: usize = 40;
/// Keeps the file small; far more ships than the game has.
pub const MAX_SHIPS: usize = 500;

pub type Names = BTreeMap<String, String>;

fn check_class(v: Option<&Value>) -> Result<String, String> {
    match v.and_then(Value::as_str) {
        Some(c)
            if !c.is_empty()
                && c.len() <= MAX_CLASS_NAME
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
        {
            Ok(c.to_owned())
        }
        _ => Err("The ship class name must be letters, numbers and underscores".to_owned()),
    }
}

/// A name, or `None` to clear it (null or blank).
fn check_name(v: Option<&Value>) -> Result<Option<String>, String> {
    let name = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => js::trim(s),
        Some(_) => return Err("The ship name must be text".to_owned()),
    };
    if js::utf16_len(name) > MAX_NAME || !is_plain(name) {
        return Err(format!(
            "The ship name must be at most {MAX_NAME} characters of plain text"
        ));
    }
    Ok((!name.is_empty()).then(|| name.to_owned()))
}

/// The `"shipNames"` field of the file, read leniently: a bad entry is left out.
pub fn from_file(text: &str) -> (Names, Vec<String>) {
    let mut names = Names::new();
    let mut problems = vec![];
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
        return (names, problems);
    };
    let Some(field) = obj.get(FIELD) else {
        return (names, problems);
    };
    let Value::Object(entries) = field else {
        problems.push(format!("{FIELD} must be an object"));
        return (names, problems);
    };
    for (class, name) in entries {
        match (check_class(Some(&json!(class))), check_name(Some(name))) {
            (Ok(class), Ok(Some(name))) if names.len() < MAX_SHIPS => {
                names.insert(class, name);
            }
            (Ok(_), Ok(None)) => {}
            _ => problems.push(format!("the name for {class} is left out")),
        }
    }
    (names, problems)
}

/// The ship names, kept in the file when there is one.
pub struct ShipNames {
    file: Option<PathBuf>,
    names: Mutex<Names>,
    state: Arc<Mutex<StateModel>>,
    changes: watch::Sender<u64>,
}

impl ShipNames {
    pub fn new(file: Option<PathBuf>, state: Arc<Mutex<StateModel>>) -> Self {
        let text = file.as_deref().and_then(|f| fs::read_to_string(f).ok());
        let (names, problems) = text.as_deref().map(from_file).unwrap_or_default();
        for p in problems {
            tracing::warn!("[settings] {FILE}: {p}");
        }
        let store = Self {
            file,
            names: Mutex::new(Names::new()),
            state,
            changes: watch::channel(0).0,
        };
        store.apply(names);
        store
    }

    pub fn get(&self) -> Names {
        self.names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn name_of(&self, class_name: &str) -> Option<String> {
        self.names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(class_name)
            .cloned()
    }

    /// Bumps whenever a name changes.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Names (or, with a null or blank name, clears) the ship of `className`; the problems,
    /// empty when it was saved.
    pub fn set(&self, msg: &Value) -> io::Result<Vec<String>> {
        let (class, name) = match (
            check_class(msg.get("className")),
            check_name(msg.get("name")),
        ) {
            (Ok(class), Ok(name)) => (class, name),
            (class, name) => {
                return Ok([class.err(), name.err()].into_iter().flatten().collect());
            }
        };
        let mut next = self.get();
        match name {
            Some(name) => {
                if next.len() >= MAX_SHIPS && !next.contains_key(&class) {
                    return Ok(vec![format!("At most {MAX_SHIPS} ship names")]);
                }
                next.insert(class, name);
            }
            None => {
                next.remove(&class);
            }
        }
        if let Some(file) = &self.file {
            write_field(file, FIELD, json!(next))?;
        }
        self.apply(next);
        self.changes.send_modify(|n| *n += 1);
        Ok(vec![])
    }

    fn apply(&self, names: Names) {
        let copy: HashMap<String, String> = names.clone().into_iter().collect();
        *self.names.lock().unwrap_or_else(PoisonError::into_inner) = names;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_ship_names(copy);
    }

    /// The push to dashboard pages.
    pub fn push(&self) -> Value {
        json!({ "type": "shipNames", "names": self.get(), "max": MAX_NAME })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_core::state::read_value;

    fn store(file: Option<PathBuf>) -> (ShipNames, Arc<Mutex<StateModel>>) {
        let state = Arc::new(Mutex::new(StateModel::default()));
        (ShipNames::new(file, Arc::clone(&state)), state)
    }

    fn shown(state: &Mutex<StateModel>) -> String {
        read_value(&state.lock().unwrap(), "ship.name")
            .unwrap()
            .text
    }

    #[test]
    fn a_name_is_saved_shown_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        fs::write(
            &file,
            r#"{ "splash": false, "time": { "zone": "UTC", "clock": "24h" } }"#,
        )
        .unwrap();
        let (names, state) = store(Some(file.clone()));
        state
            .lock()
            .unwrap()
            .ingest(&json!({ "vehicle": { "state": {
            "name": "DRAK_Corsair_877723283935", "class_name": "DRAK_Corsair" } } }));
        assert_eq!(shown(&state), "Drake Corsair");
        let changes = names.subscribe();

        let set = json!({ "className": "DRAK_Corsair", "name": "  Old Betsy " });
        assert_eq!(names.set(&set).unwrap(), Vec::<String>::new());
        assert!(changes.has_changed().unwrap());
        assert_eq!(shown(&state), "Old Betsy");
        let saved: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({ "splash": false, "time": { "zone": "UTC", "clock": "24h" },
                    "shipNames": { "DRAK_Corsair": "Old Betsy" } })
        );

        let (again, again_state) = store(Some(file.clone()));
        assert_eq!(again.name_of("DRAK_Corsair").as_deref(), Some("Old Betsy"));
        assert_eq!(
            again_state.lock().unwrap().ship_name_for("DRAK_Corsair"),
            Some("Old Betsy")
        );

        let clear = json!({ "className": "DRAK_Corsair", "name": "" });
        assert_eq!(names.set(&clear).unwrap(), Vec::<String>::new());
        assert_eq!(shown(&state), "Drake Corsair");
        assert!(names.get().is_empty());
        let null = json!({ "className": "DRAK_Corsair", "name": null });
        assert_eq!(names.set(&null).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_bad_name_or_class_is_refused_and_changes_nothing() {
        let (names, _) = store(None);
        let changes = names.subscribe();
        let long = "A".repeat(MAX_NAME + 1);
        for msg in [
            json!({ "className": "DRAK_Corsair", "name": long }),
            json!({ "className": "DRAK_Corsair", "name": "<b>Betsy</b>" }),
            json!({ "className": "DRAK_Corsair", "name": "Old\nBetsy" }),
            json!({ "className": "DRAK_Corsair", "name": 5 }),
            json!({ "className": "DRAK Corsair", "name": "Betsy" }),
            json!({ "className": "", "name": "Betsy" }),
            json!({ "name": "Betsy" }),
        ] {
            assert_eq!(names.set(&msg).unwrap().len(), 1, "{msg}");
        }
        assert_eq!(
            names.set(&json!({ "name": 5 })).unwrap().len(),
            2,
            "both problems"
        );
        assert!(!changes.has_changed().unwrap());
        assert!(names.get().is_empty());
        let max = "B".repeat(MAX_NAME);
        assert!(
            names
                .set(&json!({ "className": "ANVL_Arrow", "name": max }))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_file_is_read_leniently() {
        let (names, problems) = from_file(
            r#"{ "shipNames": { "DRAK_Corsair": "Betsy", "bad class": "X", "ANVL_Arrow": "<i>", "AEGS_Gladius": "" } }"#,
        );
        assert_eq!(
            names,
            Names::from([("DRAK_Corsair".into(), "Betsy".into())])
        );
        assert_eq!(problems.len(), 2);
        assert_eq!(from_file(r#"{ "shipNames": [] }"#).1.len(), 1);
        assert_eq!(from_file("not json"), (Names::new(), vec![]));
        assert_eq!(from_file(r#"{ "splash": true }"#), (Names::new(), vec![]));
    }
}
