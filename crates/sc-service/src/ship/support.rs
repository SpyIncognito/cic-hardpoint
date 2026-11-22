//! Which functions the current ship has, learned from the stream (design notes, section 1).
//!
//! proto3 leaves zero, false and empty values off the wire, so a missing plain field cannot be
//! told from a real 0 or OFF. Absence counts only for the fields in [`FIELDS`]: an `optional`
//! field or a sub-message whose zero value is UNSPECIFIED, checked against
//! sc_game_telemetry_v1.proto. Plain scalars and bools are never greyed by absence.
//!
//! - An IFCS `Availability` message that arrives with `enabled` not true: "not available".
//! - A field that has not arrived after 10 s of fresh frames in this ship: "this ship does not
//!   report it", learned for the ship's class name and game build.
//! - A field that arrives: supported at once, and learned so.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};

use sc_core::js;
use sc_core::state::StateModel;

/// Fresh time in a ship before a field that has not arrived counts as absent.
pub const ABSENT_AFTER_MS: i64 = 10_000;
/// A gap between two looks longer than this is not counted as time in the ship.
const MAX_STEP_MS: i64 = 1_000;
pub const MAX_SHIPS: usize = 500;
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub const SUPPORT_SCHEMA_VERSION: u32 = 1;
pub const ABSENT_REASON: &str = "this ship does not report it";
pub const UNAVAILABLE_REASON: &str = "not available";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// An optional enum or a sub-message: arrived when present, and not the UNSPECIFIED zero.
    Present,
    /// An IFCS `Availability` message: arrived when present; `enabled` not true is "not
    /// available".
    Availability,
}

/// A field that can be greyed, and the value, toggle, widget or configuration row ids it greys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDef {
    pub id: &'static str,
    pub path: &'static str,
    pub rule: Rule,
    pub greys: &'static [&'static str],
}

const fn present(id: &'static str, path: &'static str, greys: &'static [&'static str]) -> FieldDef {
    FieldDef {
        id,
        path,
        rule: Rule::Present,
        greys,
    }
}

const fn availability(
    id: &'static str,
    path: &'static str,
    greys: &'static [&'static str],
) -> FieldDef {
    FieldDef {
        id,
        path,
        rule: Rule::Availability,
        greys,
    }
}

/// Every greyable field. Each is `optional` or a sub-message in the proto, with an UNSPECIFIED
/// zero where it is an enum (plan section 1 table). `greys`: value ids, toggle ids, `mode` (the
/// master mode widget) and configuration row ids.
#[rustfmt::skip]
pub const FIELDS: &[FieldDef] = &[
    present("gear", "vehicle.state.flight.state.gear", &["gear", "ship.gear"]),
    present("vtol", "vehicle.state.flight.state.vtol", &["vtol"]),
    present("transformation", "vehicle.state.flight.state.transformation", &["transform"]),
    present("g_meter", "vehicle.state.flight.instruments.g_meter", &["ship.g_force", "ship.g_force_max"]),
    // doors is optional; its lock_state is an enum with an UNSPECIFIED zero, so a ship whose
    // doors arrive without a lock state reports no locks.
    present("doors.lock_state", "vehicle.state.doors.lock_state", &["door_locks"]),
    present("master_mode.mode", "vehicle.state.master_mode.mode", &["ship.master_mode", "mode"]),
    present("annunciators.pwr", "vehicle.state.annunciators.pwr", &["caution.pwr"]),
    present("annunciators.wpn", "vehicle.state.annunciators.wpn", &["caution.wpn"]),
    present("annunciators.thr", "vehicle.state.annunciators.thr", &["caution.thr"]),
    present("annunciators.shld", "vehicle.state.annunciators.shld", &["caution.shld"]),
    present("annunciators.cool", "vehicle.state.annunciators.cool", &["caution.cool"]),
    present("annunciators.qdrv", "vehicle.state.annunciators.qdrv", &["caution.qdrv"]),
    present("annunciators.radr", "vehicle.state.annunciators.radr", &["caution.radr"]),
    present("annunciators.prox", "vehicle.state.annunciators.prox", &["caution.prox"]),
    present("annunciators.hit", "vehicle.state.annunciators.hit", &["caution.hit"]),
    present("annunciators.misl", "vehicle.state.annunciators.misl", &["caution.misl"]),
    availability("ifcs_coupled", "vehicle.state.flight.settings.ifcs_coupled", &["decoupled", "coupled"]),
    availability("ifcs_esp", "vehicle.state.flight.settings.ifcs_esp", &["esp"]),
    availability("ifcs_g_safe", "vehicle.state.flight.settings.ifcs_g_safe", &["gsafe"]),
    availability("ifcs_proximity_assist", "vehicle.state.flight.settings.ifcs_proximity_assist", &["proximity"]),
    availability("ifcs_cruise_mode", "vehicle.state.flight.settings.ifcs_cruise_mode", &["flight.cruise"]),
    availability("ifcs_gravity_compensation", "vehicle.state.flight.settings.ifcs_gravity_compensation", &["flight.gravity_compensation"]),
    availability("ifcs_speed_limiter", "vehicle.state.flight.settings.ifcs_speed_limiter", &["flight.speed_limiter"]),
];

pub fn field(id: &str) -> Option<&'static FieldDef> {
    FIELDS.iter().find(|f| f.id == id)
}

/// What one frame says about a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Arrived,
    /// An Availability that arrived with `enabled` not true.
    Unavailable,
    Missing,
}

fn is_unspecified(v: &Value) -> bool {
    match v {
        Value::String(s) => s.ends_with("_UNSPECIFIED"),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Null => true,
        _ => false,
    }
}

/// The rule for one field applied to the value at its path (`None` when it is not there).
pub fn seen(def: &FieldDef, value: Option<&Value>) -> Seen {
    match (def.rule, value) {
        (_, None) => Seen::Missing,
        (Rule::Present, Some(v)) if is_unspecified(v) => Seen::Missing,
        (Rule::Present, Some(_)) => Seen::Arrived,
        (Rule::Availability, Some(v)) if !v.is_object() => Seen::Missing,
        (Rule::Availability, Some(v)) => {
            if v.get("enabled") == Some(&Value::Bool(true)) {
                Seen::Arrived
            } else {
                Seen::Unavailable
            }
        }
    }
}

/// One look at the state: the ship's class name while the player is in it and the report is
/// fresh, and what each field says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub class_name: Option<String>,
    pub fields: Vec<(&'static str, Seen)>,
}

impl Observation {
    pub fn of(state: &StateModel) -> Self {
        let class_name = state
            .in_vehicle()
            .then(|| state.read("vehicle.state.class_name", None).value)
            .flatten()
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|c| !c.is_empty());
        let fields = if class_name.is_some() {
            FIELDS
                .iter()
                .map(|f| (f.id, seen(f, state.read(f.path, None).value)))
                .collect()
        } else {
            vec![]
        };
        Self { class_name, fields }
    }
}

/// The game build the learned list is for. The telemetry stream has no build number, so the
/// service uses a fingerprint of the game's own action list (see [`build_of_actions`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Build {
    /// Not asked yet: nothing is learned or used until it is known.
    Pending,
    /// `None`: the game listed no actions, so the list is keyed by class name only.
    Known(Option<String>),
}

/// `actions-<16 hex>`: FNV-1a 64 of the sorted, distinct action names. It changes when a patch
/// adds, removes or renames an action; a patch that changes only telemetry keeps it.
pub fn build_of_actions<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let set: BTreeSet<&str> = names.into_iter().collect();
    if set.is_empty() {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for name in set {
        for b in name.bytes().chain([0]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    Some(format!("actions-{h:016x}"))
}

/// What a class of ship was seen to report, under one build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Learned {
    pub class_name: String,
    pub build: Option<String>,
    pub present: BTreeSet<String>,
    pub absent: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SupportState {
    Absent,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupportEntry {
    pub state: SupportState,
    pub reason: &'static str,
}

#[derive(Debug, Default)]
struct Session {
    class_name: Option<String>,
    fresh_ms: i64,
    last_at: Option<i64>,
    arrived: BTreeSet<&'static str>,
    now: Vec<(&'static str, Seen)>,
}

/// The learned list (kept in ship-support.json, most recently seen last) and the current ship's
/// session.
#[derive(Debug, Default)]
pub struct Tracker {
    learned: Vec<Learned>,
    session: Session,
    /// The learned list changed since the last [`Tracker::take_dirty`].
    dirty: bool,
}

impl Tracker {
    pub fn new(learned: Vec<Learned>) -> Self {
        Self {
            learned,
            ..Self::default()
        }
    }

    pub fn learned(&self) -> &[Learned] {
        &self.learned
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Forget what was learned for one class, or for every ship; learning starts again.
    pub fn reset(&mut self, class_name: Option<&str>) -> usize {
        let before = self.learned.len();
        match class_name {
            Some(c) => self.learned.retain(|l| l.class_name != c),
            None => self.learned.clear(),
        }
        if class_name.is_none_or(|c| self.session.class_name.as_deref() == Some(c)) {
            self.session = Session::default();
        }
        let removed = before - self.learned.len();
        self.dirty |= removed > 0;
        removed
    }

    /// The entry for this class under this build: a learned "absent" from another build
    /// expires, and the entry moves to the end (most recently seen).
    fn entry(&mut self, class_name: &str, build: &Option<String>) -> &mut Learned {
        let at = self.learned.iter().position(|l| l.class_name == class_name);
        let mut entry = at.map_or_else(
            || Learned {
                class_name: class_name.to_owned(),
                build: build.clone(),
                present: BTreeSet::new(),
                absent: BTreeSet::new(),
            },
            |i| self.learned.remove(i),
        );
        if &entry.build != build {
            entry.build.clone_from(build);
            entry.absent.clear();
            self.dirty = true;
        }
        if at.is_none() {
            self.dirty = true;
        }
        self.learned.push(entry);
        if self.learned.len() > MAX_SHIPS {
            self.learned.remove(0);
        }
        self.learned.last_mut().expect("just pushed")
    }

    /// Take one look at the state, at `now_ms`.
    pub fn observe(&mut self, now_ms: i64, obs: &Observation, build: &Build) {
        let Some(class_name) = obs.class_name.as_deref() else {
            // Out of the ship, or the report is stale: the clock stops, the ship is kept.
            self.session.last_at = None;
            self.session.now.clear();
            return;
        };
        if self.session.class_name.as_deref() != Some(class_name) {
            self.session = Session {
                class_name: Some(class_name.to_owned()),
                ..Session::default()
            };
        }
        let s = &mut self.session;
        if let Some(last) = s.last_at {
            s.fresh_ms += (now_ms - last).clamp(0, MAX_STEP_MS);
        }
        s.last_at = Some(now_ms);
        s.now.clone_from(&obs.fields);
        for (id, seen) in &obs.fields {
            if *seen != Seen::Missing {
                s.arrived.insert(id);
            }
        }
        let Build::Known(build) = build else {
            return;
        };
        let arrived: Vec<&'static str> = s.arrived.iter().copied().collect();
        let timed_out = s.fresh_ms >= ABSENT_AFTER_MS;
        let entry = self.entry(class_name, build);
        let mut changed = false;
        for f in FIELDS {
            if arrived.contains(&f.id) {
                changed |= entry.present.insert(f.id.to_owned());
                changed |= entry.absent.remove(f.id);
            } else if timed_out {
                changed |= entry.absent.insert(f.id.to_owned());
                changed |= entry.present.remove(f.id);
            }
        }
        self.dirty |= changed;
    }

    /// The class name of the ship the player is in, while the report is fresh.
    pub fn ship(&self) -> Option<&str> {
        self.session.last_at.and(self.session.class_name.as_deref())
    }

    /// Each greyed id with its state and reason, while in a ship. A field that arrived in this
    /// ship is never greyed by absence; one that has not is greyed once 10 s have passed or the
    /// ship is known not to report it.
    pub fn support(&self, build: &Build) -> Vec<(&'static str, SupportEntry)> {
        let Some(class_name) = self.ship() else {
            return vec![];
        };
        let s = &self.session;
        let learned = match build {
            Build::Known(b) => self
                .learned
                .iter()
                .find(|l| l.class_name == class_name && &l.build == b),
            Build::Pending => None,
        };
        let mut out = Vec::new();
        for f in FIELDS {
            let now = s.now.iter().find(|(id, _)| *id == f.id).map(|(_, v)| *v);
            let state = match now {
                Some(Seen::Unavailable) => Some(SupportState::Unavailable),
                Some(Seen::Arrived) => None,
                _ if s.arrived.contains(f.id) => None,
                _ if s.fresh_ms >= ABSENT_AFTER_MS => Some(SupportState::Absent),
                _ if learned.is_some_and(|l| l.absent.contains(f.id)) => Some(SupportState::Absent),
                _ => None,
            };
            if let Some(state) = state {
                let reason = match state {
                    SupportState::Absent => ABSENT_REASON,
                    SupportState::Unavailable => UNAVAILABLE_REASON,
                };
                out.extend(
                    f.greys
                        .iter()
                        .map(|id| (*id, SupportEntry { state, reason })),
                );
            }
        }
        out
    }
}

/// ship-support.json in the settings folder.
#[derive(Debug, Clone)]
pub struct SupportFile {
    path: Option<PathBuf>,
}

impl SupportFile {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// The learned list; a bad entry or field is left out and logged.
    pub fn load(&self) -> Vec<Learned> {
        let Some(path) = &self.path else {
            return vec![];
        };
        if fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            tracing::warn!("[ship-support] {}: over 256 KiB, not read", path.display());
            return vec![];
        }
        let Ok(text) = fs::read_to_string(path) else {
            return vec![];
        };
        let (learned, skipped) = read_support(serde_json::from_str(&text).ok().as_ref());
        if !skipped.is_empty() {
            tracing::warn!(
                "[ship-support] {}: left out {}:\n  {}",
                path.display(),
                skipped.len(),
                skipped.join("\n  ")
            );
        }
        learned
    }

    pub fn save(&self, learned: &[Learned]) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let text = crate::json::to_pretty_text(
            &json!({ "schemaVersion": SUPPORT_SCHEMA_VERSION, "ships": learned }),
        );
        let mut tmp = OsString::from(path.as_os_str());
        tmp.push(".tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

fn plain(s: &str, max: usize) -> bool {
    !s.is_empty()
        && js::utf16_len(s) <= max
        && !s.chars().any(|c| c == '<' || c == '>' || c.is_control())
}

fn field_set(v: Option<&Value>, at: &str, skipped: &mut Vec<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in v.and_then(Value::as_array).into_iter().flatten() {
        match f.as_str().and_then(field) {
            Some(def) => {
                out.insert(def.id.to_owned());
            }
            None => skipped.push(format!("{at}: unknown field {f}")),
        }
    }
    out
}

/// Read ship-support.json: only known fields, plain class names and builds, at most
/// [`MAX_SHIPS`] ships (the most recent).
pub fn read_support(input: Option<&Value>) -> (Vec<Learned>, Vec<String>) {
    let mut skipped = Vec::new();
    let obj = input.and_then(Value::as_object);
    let Some(ships) = obj
        .filter(|o| o.get("schemaVersion") == Some(&json!(SUPPORT_SCHEMA_VERSION)))
        .and_then(|o| o.get("ships"))
        .and_then(Value::as_array)
    else {
        return (vec![], vec!["not a version 1 ship-support file".to_owned()]);
    };
    let mut out: Vec<Learned> = Vec::new();
    let empty = Map::new();
    for (i, raw) in ships.iter().enumerate() {
        let at = format!("ship {}", i + 1);
        let o = raw.as_object().unwrap_or(&empty);
        let Some(class_name) = o
            .get("className")
            .and_then(Value::as_str)
            .filter(|c| plain(c, 100))
        else {
            skipped.push(format!("{at}: no readable class name"));
            continue;
        };
        let build = match o.get("build") {
            None | Some(Value::Null) => None,
            Some(Value::String(b)) if plain(b, 80) => Some(b.clone()),
            Some(_) => {
                skipped.push(format!("{at}: bad build"));
                continue;
            }
        };
        let present = field_set(o.get("present"), &at, &mut skipped);
        let mut absent = field_set(o.get("absent"), &at, &mut skipped);
        absent.retain(|f| !present.contains(f));
        out.retain(|l| l.class_name != class_name);
        out.push(Learned {
            class_name: class_name.to_owned(),
            build,
            present,
            absent,
        });
    }
    if out.len() > MAX_SHIPS {
        skipped.push(format!(
            "{} ships over the cap of {MAX_SHIPS} left out",
            out.len() - MAX_SHIPS
        ));
        out.drain(..out.len() - MAX_SHIPS);
    }
    (out, skipped)
}
