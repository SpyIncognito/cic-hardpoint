//! Sequences: named lists of steps the engine runs one after another, and the store that keeps
//! the user's sequences in a JSON file the TypeScript store reads and writes too.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use super::catalog::{MasterMode, needs_confirm, toggle_by_id};
use crate::js;
use crate::listeners::lock;

pub const MAX_PAUSE_S: f64 = 60.0;
pub const MAX_TIMEOUT_S: f64 = 60.0;
pub const MAX_STEPS: usize = 20;
/// In UTF-16 code units, as the TS counts `name.length`.
pub const MAX_NAME: usize = 40;
const MAX_ID: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

impl OnOff {
    pub fn is_on(self) -> bool {
        self == Self::On
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

impl fmt::Display for OnOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A master mode the game has a set action for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TargetMode {
    Nav,
    Scm,
}

impl TargetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nav => "NAV",
            Self::Scm => "SCM",
        }
    }
}

impl From<TargetMode> for MasterMode {
    fn from(mode: TargetMode) -> Self {
        match mode {
            TargetMode::Nav => MasterMode::Nav,
            TargetMode::Scm => MasterMode::Scm,
        }
    }
}

impl fmt::Display for TargetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Step {
    /// Bring a toggle to a state; done when the game reports it (within `timeout_s`).
    Set {
        toggle: String,
        to: OnOff,
        #[serde(
            rename = "timeoutS",
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "js_number_opt"
        )]
        timeout_s: Option<f64>,
    },
    /// Bring the master mode to NAV or SCM; done when the game reports it (within `timeout_s`).
    Mode {
        to: TargetMode,
        #[serde(
            rename = "timeoutS",
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "js_number_opt"
        )]
        timeout_s: Option<f64>,
    },
    /// Fire a game action; done when the game accepts it.
    Action { action: String },
    /// Wait, e.g. for the ship to power up.
    Pause {
        #[serde(serialize_with = "js_number")]
        seconds: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub id: String,
    pub name: String,
    /// Shipped with the plugin; can be edited and reset to its default, not removed for good.
    /// Kept as an option because the file tells absent from false.
    #[serde(rename = "builtIn", default, skip_serializing_if = "Option::is_none")]
    pub built_in: Option<bool>,
    pub steps: Vec<Step>,
}

/// Integral numbers are written without a fraction, as `JSON.stringify` writes them.
fn js_number<S: Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
    const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
    if x.fract() == 0.0 && x.abs() <= MAX_SAFE {
        s.serialize_i64(*x as i64)
    } else {
        s.serialize_f64(*x)
    }
}

fn js_number_opt<S: Serializer>(x: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match x {
        Some(x) => js_number(x, s),
        None => s.serialize_none(),
    }
}

fn set(toggle: &str, to: OnOff) -> Step {
    Step::Set {
        toggle: toggle.to_owned(),
        to,
        timeout_s: None,
    }
}

fn mode(to: TargetMode) -> Step {
    Step::Mode {
        to,
        timeout_s: None,
    }
}

fn built_in(id: &str, name: &str, steps: Vec<Step>) -> Sequence {
    Sequence {
        id: id.to_owned(),
        name: name.to_owned(),
        built_in: Some(true),
        steps,
    }
}

pub static DEFAULT_SEQUENCES: LazyLock<Vec<Sequence>> = LazyLock::new(|| {
    use OnOff::{Off, On};
    vec![
        built_in(
            "flight_ready",
            "Flight ready",
            vec![
                Step::Action {
                    action: "v_flightready".to_owned(),
                },
                // The ship needs a moment to power up before it reports and takes the next steps.
                Step::Pause { seconds: 3.0 },
                set("door_locks", On),
                set("lights", On),
            ],
        ),
        built_in(
            "landing_prep",
            "Landing prep",
            vec![
                mode(TargetMode::Scm),
                set("gear", On),
                set("proximity", On),
                set("lights", On),
            ],
        ),
        built_in(
            "combat_ready",
            "Combat ready",
            vec![
                mode(TargetMode::Scm),
                set("power.shields", On),
                set("power.weapons", On),
            ],
        ),
        // Power off is in the confirm group, so it is its own hold-to-confirm key, not a step here.
        built_in("shutdown", "Shutdown", vec![set("lights", Off)]),
    ]
});

/// Why a sequence given as JSON cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceError {
    /// The reasons to show the user, the same strings the TS core gives.
    Invalid(Vec<String>),
    /// Not shaped like a sequence at all (the TS core throws on these).
    Malformed(String),
}

impl fmt::Display for SequenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(problems) => f.write_str(&problems.join("; ")),
            Self::Malformed(reason) => write!(f, "not a sequence: {reason}"),
        }
    }
}

impl std::error::Error for SequenceError {}

/// Why a sequence cannot be saved; empty when it can.
pub fn validate(seq: &Sequence) -> Vec<String> {
    let value = serde_json::to_value(seq).expect("a sequence serializes");
    validate_value(&value).expect("a typed sequence is well shaped")
}

/// A sequence from JSON (a bus or portal message), with the TS core's reasons when it is not valid.
pub fn parse_sequence(value: &Value) -> Result<Sequence, SequenceError> {
    let problems = validate_value(value).map_err(SequenceError::Malformed)?;
    if !problems.is_empty() {
        return Err(SequenceError::Invalid(problems));
    }
    Sequence::deserialize(value).map_err(|e| SequenceError::Malformed(e.to_string()))
}

/// The TS `validate` over raw JSON, with its JavaScript coercions, so any input gets the same
/// reasons in the same order. `Err` where the TS would throw.
fn validate_value(seq: &Value) -> Result<Vec<String>, String> {
    let mut problems = Vec::new();
    if seq.is_null() {
        return Err("null".to_owned());
    }
    if !is_id(&js_string(prop(seq, "id"))) {
        problems.push("The id must be 1 to 40 lower-case letters, digits, - or _".to_owned());
    }
    let Some(Value::String(name)) = prop(seq, "name") else {
        return Err("the name is not a string".to_owned());
    };
    let name = js::trim(name);
    if name.is_empty() || js::utf16_len(name) > MAX_NAME {
        problems.push(format!("The name must be 1 to {MAX_NAME} characters"));
    }
    let Some(Value::Array(steps)) = prop(seq, "steps") else {
        return Err("the steps are not a list".to_owned());
    };
    if steps.is_empty() {
        problems.push("A sequence needs at least one step".to_owned());
    }
    if steps.len() > MAX_STEPS {
        problems.push(format!("A sequence can have at most {MAX_STEPS} steps"));
    }
    for (i, s) in steps.iter().enumerate() {
        if s.is_null() {
            return Err(format!("step {} is null", i + 1));
        }
        validate_step(s, &format!("Step {}", i + 1), &mut problems);
    }
    Ok(problems)
}

fn validate_step(s: &Value, at: &str, problems: &mut Vec<String>) {
    let kind = prop(s, "kind").and_then(Value::as_str);
    let text = |key| prop(s, key).and_then(Value::as_str);
    if kind == Some("set") {
        if text("toggle").and_then(toggle_by_id).is_none() {
            problems.push(format!(
                "{at}: unknown toggle \"{}\"",
                js_string(prop(s, "toggle"))
            ));
        }
        if !matches!(text("to"), Some("on" | "off")) {
            problems.push(format!("{at}: set must be on or off"));
        }
    }
    if kind == Some("mode") && !matches!(text("to"), Some("NAV" | "SCM")) {
        problems.push(format!("{at}: mode must be NAV or SCM"));
    }
    if kind == Some("action") {
        let action = js_string(prop(s, "action"));
        if !is_action_name(&action) {
            problems.push(format!("{at}: \"{action}\" is not an action name"));
        }
        if text("action").is_some_and(needs_confirm) {
            problems.push(format!(
                "{at}: \"{action}\" needs a hold to confirm, so it cannot be a step"
            ));
        }
    }
    if kind == Some("pause") {
        let seconds = js_number_of(prop(s, "seconds"));
        if !(seconds > 0.0 && seconds <= MAX_PAUSE_S) {
            problems.push(format!(
                "{at}: a pause must be more than 0 and at most {MAX_PAUSE_S} seconds"
            ));
        }
    }
    let timeout = prop(s, "timeoutS").filter(|_| matches!(kind, Some("set" | "mode")));
    if let Some(timeout) = timeout.map(|t| js_number_of(Some(t)))
        && !(timeout > 0.0 && timeout <= MAX_TIMEOUT_S)
    {
        problems.push(format!(
            "{at}: the wait must be more than 0 and at most {MAX_TIMEOUT_S} seconds"
        ));
    }
    if !matches!(kind, Some("set" | "mode" | "action" | "pause")) {
        problems.push(format!("{at}: unknown step kind"));
    }
}

/// `v.key`, `None` standing for undefined.
fn prop<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?.get(key)
}

/// `String(v)`, as a regex test or a template literal coerces it.
fn js_string(v: Option<&Value>) -> String {
    v.map_or_else(|| "undefined".to_owned(), js::to_display_string)
}

/// The number a relational comparison (`v > 0`) coerces `v` to.
fn js_number_of(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(v @ (Value::Array(_) | Value::Object(_))) => {
            js::to_number(&Value::String(js::to_display_string(v)))
        }
        Some(v) => js::to_number(v),
    }
}

/// `/^[a-z0-9_-]{1,40}$/`
fn is_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `/^[a-z0-9_]{1,80}$/`
fn is_action_name(s: &str) -> bool {
    (1..=80).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(Serialize)]
struct FileFormat<'a> {
    version: u32,
    sequences: &'a [Sequence],
}

/// The user's sequences: the built-in ones (editable, resettable) plus their own. Every sequence
/// can be added, edited, duplicated, reordered within its steps and removed; a removed built-in
/// comes back with `reset`.
///
/// Changes are kept in memory first; an `Err` from a change means only the file write failed.
#[derive(Debug)]
pub struct SequenceStore {
    items: Mutex<Vec<Sequence>>,
    file: Option<PathBuf>,
}

impl Default for SequenceStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SequenceStore {
    /// Kept in `file` when given, in memory only otherwise.
    pub fn new(file: Option<PathBuf>) -> Self {
        let items = file.as_deref().and_then(load).unwrap_or_else(defaults);
        Self {
            items: Mutex::new(items),
            file,
        }
    }

    pub fn list(&self) -> Vec<Sequence> {
        lock(&self.items).clone()
    }

    pub fn get(&self, id: &str) -> Option<Sequence> {
        lock(&self.items).iter().find(|s| s.id == id).cloned()
    }

    /// Add a new sequence or replace the one with the same id; returns the problems that stopped
    /// the save, empty when saved.
    pub fn save(&self, seq: &Sequence) -> io::Result<Vec<String>> {
        let problems = validate(seq);
        if !problems.is_empty() {
            return Ok(problems);
        }
        let next = Sequence {
            name: js::trim(&seq.name).to_owned(),
            built_in: Some(DEFAULT_SEQUENCES.iter().any(|d| d.id == seq.id)),
            ..seq.clone()
        };
        let mut items = lock(&self.items);
        match items.iter_mut().find(|s| s.id == seq.id) {
            Some(existing) => *existing = next,
            None => items.push(next),
        }
        self.persist(&items)?;
        Ok(Vec::new())
    }

    pub fn remove(&self, id: &str) -> io::Result<bool> {
        let mut items = lock(&self.items);
        let before = items.len();
        items.retain(|s| s.id != id);
        if items.len() == before {
            return Ok(false);
        }
        self.persist(&items)?;
        Ok(true)
    }

    /// A copy under a new id, named "<name> copy".
    pub fn duplicate(&self, id: &str) -> io::Result<Option<Sequence>> {
        let mut items = lock(&self.items);
        let Some(src) = items.iter().find(|s| s.id == id) else {
            return Ok(None);
        };
        let mut new_id = format!("{}-copy", src.id);
        let mut n = 2;
        while items.iter().any(|s| s.id == new_id) {
            new_id = format!("{}-copy-{n}", src.id);
            n += 1;
        }
        // As in the TS, the cut id is not checked again, so a 40-character id duplicates to itself.
        let copy = Sequence {
            id: js::utf16_prefix(&new_id, MAX_ID),
            name: js::utf16_prefix(&format!("{} copy", src.name), MAX_NAME),
            built_in: None,
            steps: src.steps.clone(),
        };
        items.push(copy.clone());
        self.persist(&items)?;
        Ok(Some(copy))
    }

    /// Move a step within a sequence.
    pub fn move_step(&self, id: &str, from: usize, to: usize) -> io::Result<bool> {
        let mut items = lock(&self.items);
        let Some(s) = items.iter_mut().find(|s| s.id == id) else {
            return Ok(false);
        };
        if from >= s.steps.len() || to >= s.steps.len() {
            return Ok(false);
        }
        let step = s.steps.remove(from);
        s.steps.insert(to, step);
        self.persist(&items)?;
        Ok(true)
    }

    /// Put a built-in sequence back to its default (also brings a removed one back).
    pub fn reset(&self, id: &str) -> io::Result<bool> {
        let Some(def) = DEFAULT_SEQUENCES.iter().find(|d| d.id == id) else {
            return Ok(false);
        };
        let mut items = lock(&self.items);
        match items.iter_mut().find(|s| s.id == id) {
            Some(existing) => *existing = def.clone(),
            None => items.push(def.clone()),
        }
        self.persist(&items)?;
        Ok(true)
    }

    /// Temp file and rename, so a crash never leaves a half-written file.
    fn persist(&self, items: &[Sequence]) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = OsString::from(file.as_os_str());
        tmp.push(".tmp");
        let json = serde_json::to_string_pretty(&FileFormat {
            version: 1,
            sequences: items,
        })?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, file)
    }
}

fn defaults() -> Vec<Sequence> {
    DEFAULT_SEQUENCES.clone()
}

/// The valid sequences in the file; `None` (use the defaults) when it is missing, unreadable,
/// malformed or holds none. The next change rewrites it.
fn load(file: &Path) -> Option<Vec<Sequence>> {
    let data: Value = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    let mut valid = Vec::new();
    for value in data.get("sequences")?.as_array()? {
        // A shape the TS validate throws on discards the whole file there, so here too.
        if validate_value(value).ok()?.is_empty() {
            valid.extend(Sequence::deserialize(value).ok());
        }
    }
    (!valid.is_empty()).then_some(valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sc-core-seq-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("nested").join("sequences.json")
    }

    fn ids(store: &SequenceStore) -> Vec<String> {
        store.list().into_iter().map(|s| s.id).collect()
    }

    #[test]
    fn starts_from_the_built_in_sequences_all_valid() {
        let store = SequenceStore::default();
        let default_ids: Vec<_> = DEFAULT_SEQUENCES.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids(&store), default_ids);
        for s in DEFAULT_SEQUENCES.iter() {
            assert_eq!(validate(s), Vec::<String>::new());
        }
    }

    #[test]
    fn adds_edits_duplicates_moves_removes_resets_and_persists() {
        let f = temp_file("ops");
        let store = SequenceStore::new(Some(f.clone()));
        let mine = |name: &str, steps| Sequence {
            id: "mine".into(),
            name: name.into(),
            built_in: None,
            steps,
        };
        let saved = store
            .save(&mine(
                "My checks",
                vec![set("lights", OnOff::On), mode(TargetMode::Scm)],
            ))
            .unwrap();
        assert!(saved.is_empty());
        let saved = store
            .save(&mine(
                " My checks v2 ",
                vec![Step::Action {
                    action: "v_invoke_ping".into(),
                }],
            ))
            .unwrap();
        assert!(saved.is_empty());
        let got = store.get("mine").unwrap();
        assert_eq!(
            (got.name.as_str(), got.built_in),
            ("My checks v2", Some(false))
        );

        let copy = store.duplicate("landing_prep").unwrap().unwrap();
        assert_eq!(
            (copy.id.as_str(), copy.name.as_str(), copy.built_in),
            ("landing_prep-copy", "Landing prep copy", None)
        );
        assert!(store.move_step("landing_prep-copy", 0, 2).unwrap());
        assert_eq!(
            store.get("landing_prep-copy").unwrap().steps[2],
            mode(TargetMode::Scm)
        );
        assert!(!store.move_step("landing_prep-copy", 0, 4).unwrap());
        assert!(store.remove("shutdown").unwrap());
        assert!(!store.remove("shutdown").unwrap());
        assert!(store.get("shutdown").is_none());
        assert!(store.reset("shutdown").unwrap());
        assert_eq!(store.get("shutdown").unwrap().built_in, Some(true));
        assert!(!store.reset("mine").unwrap());

        let reloaded = SequenceStore::new(Some(f.clone()));
        let mut got = ids(&reloaded);
        got.sort();
        assert_eq!(
            got,
            [
                "combat_ready",
                "flight_ready",
                "landing_prep",
                "landing_prep-copy",
                "mine",
                "shutdown"
            ]
        );
        let file: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(file["version"], 1);
        assert!(!f.with_extension("json.tmp").exists());
    }

    #[test]
    fn duplicate_picks_a_free_id() {
        let store = SequenceStore::default();
        store.duplicate("shutdown").unwrap();
        let second = store.duplicate("shutdown").unwrap().unwrap();
        assert_eq!(second.id, "shutdown-copy-2");
        assert!(store.duplicate("nope").unwrap().is_none());
    }

    #[test]
    fn refuses_invalid_sequences_with_reasons_including_confirm_group_steps() {
        let store = SequenceStore::default();
        let problems = store
            .save(&Sequence {
                id: "Bad Id".into(),
                name: String::new(),
                built_in: None,
                steps: vec![
                    Step::Action {
                        action: "v_self_destruct".into(),
                    },
                    set("nope", OnOff::On),
                ],
            })
            .unwrap();
        assert!(problems.len() >= 4, "{problems:?}");
        assert!(problems.join(" ").contains("hold to confirm"));
        let empty = Sequence {
            id: "empty".into(),
            name: "Empty".into(),
            built_in: None,
            steps: vec![],
        };
        assert_eq!(
            store.save(&empty).unwrap(),
            ["A sequence needs at least one step"]
        );
    }

    #[test]
    fn parse_gives_shape_reasons_and_flags_what_the_ts_throws_on() {
        let bad = json!({ "id": "x", "name": "X", "steps": [{ "kind": "set", "toggle": "lights", "to": "maybe" }, { "kind": "jump" }] });
        assert_eq!(
            parse_sequence(&bad),
            Err(SequenceError::Invalid(vec![
                "Step 1: set must be on or off".into(),
                "Step 2: unknown step kind".into()
            ]))
        );
        assert!(matches!(
            parse_sequence(&json!({ "id": "x", "name": 5, "steps": [] })),
            Err(SequenceError::Malformed(_))
        ));
        let ok = json!({ "id": "x", "name": "X", "steps": [{ "kind": "pause", "seconds": 1.5 }] });
        assert_eq!(
            parse_sequence(&ok).unwrap().steps,
            [Step::Pause { seconds: 1.5 }]
        );
    }

    #[test]
    fn unreadable_or_empty_files_fall_back_to_the_defaults() {
        let f = temp_file("bad");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        for text in [
            "not json",
            "null",
            "{}",
            r#"{"sequences": "x"}"#,
            r#"{"sequences": [{"id": "a", "name": 1, "steps": []}, {"id": "b", "name": "B", "steps": [{"kind": "pause", "seconds": 1}]}]}"#,
        ] {
            fs::write(&f, text).unwrap();
            assert_eq!(
                SequenceStore::new(Some(f.clone())).list(),
                *DEFAULT_SEQUENCES,
                "{text}"
            );
        }
        fs::write(&f, r#"{"sequences": [{"id": "Bad", "name": "A", "steps": []}, {"id": "b", "name": "B", "steps": [{"kind": "pause", "seconds": 1}]}]}"#).unwrap();
        assert_eq!(ids(&SequenceStore::new(Some(f))), ["b"]);
    }

    #[test]
    fn name_trim_and_length_follow_js() {
        assert_eq!(js::trim("\u{feff} a \u{3000}"), "a");
        assert_eq!(js::trim("\u{85}a"), "\u{85}a");
        assert_eq!(js::utf16_prefix("ab\u{1f600}c", 3), "ab");
        assert_eq!(js::utf16_prefix("ab\u{1f600}c", 4), "ab\u{1f600}");
    }
}
