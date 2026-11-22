use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

/// Milliseconds on a monotonic-enough clock; `Date.now()` by default. Signed, as in the TS, so a
/// restored section can be older than the clock's zero.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// The section arrived within the stale window.
    Fresh,
    /// The section arrived before, but not recently: the game may have closed or the stream stalled.
    Stale,
    /// The section never arrived, or was dropped.
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading<'a> {
    pub freshness: Freshness,
    /// The value when the section is fresh or stale; `None` when absent.
    pub value: Option<&'a Value>,
    /// Milliseconds since the section last arrived; `None` when absent.
    pub age_ms: Option<i64>,
}

impl Reading<'_> {
    const ABSENT: Self = Reading {
        freshness: Freshness::Absent,
        value: None,
        age_ms: None,
    };
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionSnapshot<'a> {
    pub name: &'a str,
    pub value: &'a Value,
    pub age_ms: i64,
}

#[derive(Clone)]
pub struct StateModelOptions {
    /// Age after which a section is stale.
    pub stale_ms: i64,
    pub now: Clock,
}

impl Default for StateModelOptions {
    fn default() -> Self {
        Self {
            stale_ms: 3000,
            now: Arc::new(unix_ms),
        }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Returned by [`StateModel::on_change`]; pass it to [`StateModel::remove_listener`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerId(u64);

type Listener = Box<dyn Fn() + Send + Sync>;

struct Section {
    value: Value,
    at: i64,
}

/// The latest state of the game, one entry per top-level frame section with its arrival time.
/// A frame that carries only some sections updates only those. Inside a section that is present,
/// a missing scalar is its proto3 default (0, false, ""), since the game does not send defaults;
/// readers pass the default they expect.
pub struct StateModel {
    // In arrival order, as the TS Map; a frame has at most six sections, so a Vec is enough.
    sections: Vec<(String, Section)>,
    listeners: Vec<(ListenerId, Listener)>,
    next_listener: u64,
    stale_ms: i64,
    now: Clock,
}

impl Default for StateModel {
    fn default() -> Self {
        Self::new(StateModelOptions::default())
    }
}

impl fmt::Debug for StateModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateModel")
            .field(
                "sections",
                &self
                    .sections
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("listeners", &self.listeners.len())
            .field("stale_ms", &self.stale_ms)
            .finish_non_exhaustive()
    }
}

impl StateModel {
    pub fn new(opts: StateModelOptions) -> Self {
        Self {
            sections: Vec::new(),
            listeners: Vec::new(),
            next_listener: 0,
            stale_ms: opts.stale_ms,
            now: opts.now,
        }
    }

    /// Merge a frame: each object-valued top-level field replaces that section; others are ignored.
    pub fn ingest(&mut self, frame: &Value) {
        let at = (self.now)();
        if let Value::Object(fields) = frame {
            for (name, value) in fields {
                if matches!(value, Value::Object(_) | Value::Array(_)) {
                    self.set_section(name, value.clone(), at);
                }
            }
        }
        self.notify();
    }

    /// Every section with its age, e.g. to start a bus client with the current state.
    pub fn snapshot(&self) -> Vec<SectionSnapshot<'_>> {
        let now = (self.now)();
        self.sections
            .iter()
            .map(|(name, s)| SectionSnapshot {
                name,
                value: &s.value,
                age_ms: now - s.at,
            })
            .collect()
    }

    /// Set one section as it was `age_ms` ago (a bus client restoring the server's snapshot).
    pub fn restore(&mut self, name: &str, value: Value, age_ms: i64) {
        let at = (self.now)() - age_ms;
        self.set_section(name, value, at);
        self.notify();
    }

    /// Forget everything, e.g. when the game connection is lost for good.
    pub fn clear(&mut self) {
        self.sections.clear();
        self.notify();
    }

    pub fn on_change(&mut self, listener: impl Fn() + Send + Sync + 'static) -> ListenerId {
        let id = ListenerId(self.next_listener);
        self.next_listener += 1;
        self.listeners.push((id, Box::new(listener)));
        id
    }

    /// Returns false when the listener was already removed.
    pub fn remove_listener(&mut self, id: ListenerId) -> bool {
        let before = self.listeners.len();
        self.listeners.retain(|(l, _)| *l != id);
        self.listeners.len() != before
    }

    pub fn section(&self, name: &str) -> Reading<'_> {
        let Some((_, s)) = self.sections.iter().find(|(n, _)| n == name) else {
            return Reading::ABSENT;
        };
        let age_ms = (self.now)() - s.at;
        let freshness = if age_ms < self.stale_ms {
            Freshness::Fresh
        } else {
            Freshness::Stale
        };
        Reading {
            freshness,
            value: Some(&s.value),
            age_ms: Some(age_ms),
        }
    }

    /// Read a dotted path, e.g. "vehicle.state.flight.state.gear". `fallback` is the value of a
    /// field missing inside a section that is there: the proto3 default for scalars. Pass `None`
    /// for fields where "missing" must stay distinguishable (optional messages).
    pub fn read<'a>(&'a self, path: &str, fallback: Option<&'a Value>) -> Reading<'a> {
        let mut keys = path.split('.');
        let section = self.section(keys.next().unwrap_or_default());
        if section.freshness == Freshness::Absent {
            return Reading::ABSENT;
        }
        let node = keys.try_fold(section.value.expect("present section has a value"), child);
        Reading {
            value: node.or(fallback),
            ..section
        }
    }

    /// True when the player is in a vehicle the game reports on, and that report is fresh.
    pub fn in_vehicle(&self) -> bool {
        let v = self.read("vehicle.state", None);
        v.freshness == Freshness::Fresh && v.value.is_some()
    }

    fn set_section(&mut self, name: &str, value: Value, at: i64) {
        let section = Section { value, at };
        match self.sections.iter_mut().find(|(n, _)| n == name) {
            Some((_, s)) => *s = section,
            None => self.sections.push((name.to_owned(), section)),
        }
    }

    fn notify(&self) {
        for (_, listener) in &self.listeners {
            listener();
        }
    }
}

/// `node[key]` for an object or array node; `None` (JS undefined) for anything else.
fn child<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    match node {
        Value::Object(fields) => fields.get(key),
        Value::Array(items) => key
            .parse::<usize>()
            .ok()
            .filter(|i| i.to_string() == key)
            .and_then(|i| items.get(i)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    fn clocked(stale_ms: i64) -> (StateModel, Arc<AtomicI64>) {
        let now = Arc::new(AtomicI64::new(0));
        let clock = Arc::clone(&now);
        let model = StateModel::new(StateModelOptions {
            stale_ms,
            now: Arc::new(move || clock.load(Ordering::SeqCst)),
        });
        (model, now)
    }

    #[test]
    fn keeps_each_section_with_its_own_age_and_reads_missing_scalars_as_their_default() {
        let (mut s, now) = clocked(3000);
        s.ingest(&json!({ "actor": { "vitals": { "heart_rate": 72 } } }));
        now.store(2000, Ordering::SeqCst);
        s.ingest(&json!({ "vehicle": { "state": { "name": "Cutter" } } }));
        now.store(4000, Ordering::SeqCst);

        let hr = s.read("actor.vitals.heart_rate", None);
        assert_eq!(
            (hr.freshness, hr.value),
            (Freshness::Stale, Some(&json!(72)))
        );
        let lights = s.read("vehicle.state.lights_enabled", Some(&Value::Bool(false)));
        assert_eq!(
            (lights.freshness, lights.value),
            (Freshness::Fresh, Some(&json!(false)))
        );
        let preset = s.read("weapons.preset_name", None);
        assert_eq!(
            (preset.freshness, preset.value, preset.age_ms),
            (Freshness::Absent, None, None)
        );
        assert!(s.in_vehicle());
        now.store(6000, Ordering::SeqCst);
        assert!(!s.in_vehicle());
    }

    #[test]
    fn notifies_on_every_frame_and_clears() {
        let mut s = StateModel::default();
        let n = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&n);
        let off = s.on_change(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        s.ingest(&json!({ "actor": {} }));
        s.clear();
        assert!(s.remove_listener(off));
        assert!(!s.remove_listener(off));
        s.ingest(&json!({ "actor": {} }));
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert_eq!(s.section("actor").freshness, Freshness::Fresh);
    }

    #[test]
    fn a_frame_replaces_only_the_sections_it_carries() {
        let (mut s, _) = clocked(3000);
        s.ingest(&json!({ "actor": { "a": 1 }, "vehicle": { "state": { "name": "C2" } } }));
        s.ingest(&json!({ "vehicle": { "state": { "lights_enabled": true } }, "turret": 5, "weapons": null }));
        assert_eq!(s.read("actor.a", None).value, Some(&json!(1)));
        // The whole section is replaced, not deep-merged.
        assert_eq!(s.read("vehicle.state.name", None).value, None);
        assert_eq!(s.read("turret", None).freshness, Freshness::Absent);
        assert_eq!(s.read("weapons", None).freshness, Freshness::Absent);
        assert_eq!(
            s.snapshot().iter().map(|x| x.name).collect::<Vec<_>>(),
            ["actor", "vehicle"]
        );
    }

    #[test]
    fn present_null_is_not_replaced_by_the_fallback_and_scalars_have_no_children() {
        let (mut s, _) = clocked(3000);
        s.ingest(&json!({ "vehicle": { "state": { "name": "C2", "health": null } } }));
        let fallback = json!(0);
        assert_eq!(
            s.read("vehicle.state.health", Some(&fallback)).value,
            Some(&Value::Null)
        );
        assert_eq!(
            s.read("vehicle.state.name.length", Some(&fallback)).value,
            Some(&fallback)
        );
        assert_eq!(
            s.read("vehicle", None).value,
            Some(&json!({ "state": { "name": "C2", "health": null } }))
        );
    }

    #[test]
    fn snapshot_and_restore_keep_ages() {
        let (mut s, now) = clocked(3000);
        s.ingest(&json!({ "actor": { "a": 1 } }));
        now.store(1500, Ordering::SeqCst);
        let snap: Vec<_> = s
            .snapshot()
            .into_iter()
            .map(|x| (x.name.to_owned(), x.value.clone(), x.age_ms))
            .collect();
        assert_eq!(snap, [("actor".to_owned(), json!({ "a": 1 }), 1500)]);

        let (mut other, other_now) = clocked(3000);
        let calls = Arc::new(Mutex::new(0));
        let c = Arc::clone(&calls);
        other.on_change(move || *c.lock().unwrap() += 1);
        for (name, value, age) in snap {
            other.restore(&name, value, age);
        }
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(other.section("actor").age_ms, Some(1500));
        other_now.store(1500, Ordering::SeqCst);
        let r = other.section("actor");
        assert_eq!((r.freshness, r.age_ms), (Freshness::Stale, Some(3000)));
    }
}
