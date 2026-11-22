//! The viewer's own arrangement of the fleet panel: named groups of members, the card fields to
//! show, and the admiral view switch. Kept as fleet-view.json in the settings folder, keyed by
//! each member's fleet identity, and never sent to anyone.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

use sc_core::js;

pub use fleet_proto::card::is_member_id;

use super::card::{Consent, card_fields, field_def};
use crate::json::{int_in, to_pretty_text};

pub const VIEW_SCHEMA: u32 = 1;
pub const MAX_GROUPS: usize = 20;
pub const MAX_GROUP_NAME: usize = 40;
pub const MAX_GROUP_MEMBERS: usize = 64;
/// A larger fleet-view.json is not read.
pub const MAX_VIEW_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
    /// Fleet identities, in the viewer's order.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetView {
    pub schema_version: u32,
    pub groups: Vec<Group>,
    /// Card field ids, in [`super::card::FIELDS`] order.
    pub fields: Vec<String>,
    pub admiral: bool,
}

impl Default for FleetView {
    fn default() -> Self {
        Self {
            schema_version: VIEW_SCHEMA,
            groups: vec![],
            fields: card_fields().map(|f| f.id.to_owned()).collect(),
            admiral: false,
        }
    }
}

impl FleetView {
    pub fn group_of(&self, member: &str) -> Option<&Group> {
        self.groups
            .iter()
            .find(|g| g.members.iter().any(|m| m == member))
    }
}

/// What a change answers: the reasons it was refused (none when done), and a group it made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ViewChange {
    pub problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<Group>,
}

impl ViewChange {
    fn refused(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
            group: None,
        }
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(&js::utf16_prefix(s, 60)).expect("a string serializes")
}

/// A fleet identity: 1 to 64 of `A-Z a-z 0-9 _ -`.
fn is_group_id(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A group name: 1 to 40 characters of plain text, trimmed.
pub fn check_group_name(raw: Option<&Value>) -> Result<String, String> {
    let name = raw
        .and_then(Value::as_str)
        .map(js::trim)
        .unwrap_or_default();
    if name.is_empty() || js::utf16_len(name) > MAX_GROUP_NAME {
        return Err(format!(
            "The group name must be 1 to {MAX_GROUP_NAME} characters"
        ));
    }
    if name.chars().any(|c| c == '<' || c == '>' || c.is_control()) {
        return Err("The group name must be plain text (no < or >)".to_owned());
    }
    Ok(name.to_owned())
}

/// Card fields from a page or the file, checked: only viewer-choosable field ids, each once, kept
/// in display order.
fn check_fields(raw: Option<&Value>) -> Result<Vec<String>, Vec<String>> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Err(vec!["fields must be a list of field ids".to_owned()]);
    };
    let mut problems = vec![];
    let mut wanted = vec![];
    for item in items {
        match item.as_str().and_then(field_def) {
            Some(def) if def.consent != Consent::Detail => wanted.push(def.id),
            _ => problems.push(format!(
                "unknown card field {}",
                quote(&crate::json::js_string(Some(item)))
            )),
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(card_fields()
        .filter(|f| wanted.contains(&f.id))
        .map(|f| f.id.to_owned())
        .collect())
}

/// Read a fleet-view.json: keeps what it understands and lists what it left out.
pub fn read_view(raw: &Value) -> (FleetView, Vec<String>) {
    let mut view = FleetView::default();
    let mut skipped = vec![];
    let Some(obj) = raw.as_object() else {
        return (view, vec!["the file is not a JSON object".to_owned()]);
    };
    for key in obj.keys() {
        if !["schemaVersion", "groups", "fields", "admiral"].contains(&key.as_str()) {
            skipped.push(format!("field {} is not allowed", quote(key)));
        }
    }
    if let Some(fields) = obj.get("fields") {
        match check_fields(Some(fields)) {
            Ok(fields) => view.fields = fields,
            Err(problems) => {
                // Keep the known ones; a field from a newer app is left out.
                let known: Vec<Value> = fields
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|f| {
                        f.as_str()
                            .and_then(field_def)
                            .is_some_and(|d| d.consent != Consent::Detail)
                    })
                    .cloned()
                    .collect();
                view.fields = check_fields(Some(&Value::Array(known))).unwrap_or_default();
                skipped.extend(problems);
            }
        }
    }
    view.admiral = obj.get("admiral") == Some(&Value::Bool(true));
    let groups = obj.get("groups").and_then(Value::as_array);
    for raw in groups.into_iter().flatten() {
        if view.groups.len() >= MAX_GROUPS {
            skipped.push(format!("groups past the first {MAX_GROUPS}"));
            break;
        }
        let id = raw.get("id").and_then(Value::as_str).unwrap_or_default();
        if !is_group_id(id) || view.groups.iter().any(|g| g.id == id) {
            skipped.push(format!("a group with a bad or repeated id {}", quote(id)));
            continue;
        }
        let name = match check_group_name(raw.get("name")) {
            Ok(name) => name,
            Err(problem) => {
                skipped.push(format!("group {}: {problem}", quote(id)));
                continue;
            }
        };
        let mut members: Vec<String> = vec![];
        for m in raw
            .get("members")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let m = m.as_str().unwrap_or_default();
            let taken = members.iter().any(|x| x == m) || view.group_of(m).is_some();
            if !is_member_id(m) || taken || members.len() >= MAX_GROUP_MEMBERS {
                skipped.push(format!("group {}: member {} left out", quote(id), quote(m)));
                continue;
            }
            members.push(m.to_owned());
        }
        view.groups.push(Group {
            id: id.to_owned(),
            name,
            collapsed: raw.get("collapsed") == Some(&Value::Bool(true)),
            members,
        });
    }
    (view, skipped)
}

/// Changes are made in memory first; an `Err` means only the file write failed.
#[derive(Debug)]
pub struct FleetViewStore {
    view: Mutex<FleetView>,
    file: Option<PathBuf>,
    changes: watch::Sender<u64>,
}

impl FleetViewStore {
    /// Kept in `file` when given, in memory only otherwise.
    pub fn new(file: Option<PathBuf>) -> Self {
        let view = file.as_deref().and_then(load).unwrap_or_default();
        Self {
            view: Mutex::new(view),
            file,
            changes: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, FleetView> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self) -> FleetView {
        self.lock().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn add_group(&self, name: Option<&Value>) -> io::Result<ViewChange> {
        let mut view = self.lock();
        if view.groups.len() >= MAX_GROUPS {
            return Ok(ViewChange::refused(format!("At most {MAX_GROUPS} groups")));
        }
        let name = match check_group_name(name) {
            Ok(name) => name,
            Err(problem) => return Ok(ViewChange::refused(problem)),
        };
        let mut n = view.groups.len() + 1;
        while view.groups.iter().any(|g| g.id == format!("group-{n}")) {
            n += 1;
        }
        let group = Group {
            id: format!("group-{n}"),
            name,
            collapsed: false,
            members: vec![],
        };
        view.groups.push(group.clone());
        self.commit(view)?;
        Ok(ViewChange {
            problems: vec![],
            group: Some(group),
        })
    }

    fn with_group(
        &self,
        id: &str,
        change: impl FnOnce(&mut FleetView, usize) -> Result<(), String>,
    ) -> io::Result<ViewChange> {
        let mut view = self.lock();
        let Some(i) = view.groups.iter().position(|g| g.id == id) else {
            return Ok(ViewChange::refused(format!("No group {}", quote(id))));
        };
        if let Err(problem) = change(&mut view, i) {
            return Ok(ViewChange::refused(problem));
        }
        self.commit(view)?;
        Ok(ViewChange::default())
    }

    pub fn rename_group(&self, id: &str, name: Option<&Value>) -> io::Result<ViewChange> {
        self.with_group(id, |view, i| {
            view.groups[i].name = check_group_name(name)?;
            Ok(())
        })
    }

    pub fn move_group(&self, id: &str, to: Option<&Value>) -> io::Result<ViewChange> {
        self.with_group(id, |view, from| {
            let last = i64::try_from(view.groups.len()).unwrap_or(i64::MAX) - 1;
            let to = int_in(to, 0, last)
                .and_then(|t| usize::try_from(t).ok())
                .ok_or("No such group position")?;
            let group = view.groups.remove(from);
            view.groups.insert(to, group);
            Ok(())
        })
    }

    pub fn collapse_group(&self, id: &str, collapsed: Option<&Value>) -> io::Result<ViewChange> {
        self.with_group(id, |view, i| {
            view.groups[i].collapsed = collapsed
                .and_then(Value::as_bool)
                .ok_or("collapsed must be true or false")?;
            Ok(())
        })
    }

    /// Remove a group; its members go back to Ungrouped.
    pub fn remove_group(&self, id: &str) -> io::Result<ViewChange> {
        self.with_group(id, |view, i| {
            view.groups.remove(i);
            Ok(())
        })
    }

    /// Put a member in a group (at `index` when given, else last), or back to Ungrouped (`None`).
    /// A member is in at most one group.
    pub fn assign(
        &self,
        member: &str,
        group: Option<&str>,
        index: Option<&Value>,
    ) -> io::Result<ViewChange> {
        if !is_member_id(member) {
            return Ok(ViewChange::refused(format!("No member {}", quote(member))));
        }
        let mut view = self.lock();
        let target = match group {
            None => None,
            Some(id) => match view.groups.iter().position(|g| g.id == id) {
                Some(i) => Some(i),
                None => return Ok(ViewChange::refused(format!("No group {}", quote(id)))),
            },
        };
        if let Some(t) = target {
            let full = view.groups[t].members.len() >= MAX_GROUP_MEMBERS
                && !view.groups[t].members.iter().any(|m| m == member);
            if full {
                return Ok(ViewChange::refused(format!(
                    "At most {MAX_GROUP_MEMBERS} members in a group"
                )));
            }
        }
        for g in &mut view.groups {
            g.members.retain(|m| m != member);
        }
        if let Some(t) = target {
            let members = &mut view.groups[t].members;
            let last = i64::try_from(members.len()).unwrap_or(i64::MAX);
            let at = int_in(index, 0, last)
                .and_then(|i| usize::try_from(i).ok())
                .unwrap_or(members.len());
            members.insert(at, member.to_owned());
        }
        self.commit(view)?;
        Ok(ViewChange::default())
    }

    pub fn set_fields(&self, raw: Option<&Value>) -> io::Result<ViewChange> {
        let fields = match check_fields(raw) {
            Ok(fields) => fields,
            Err(problems) => {
                return Ok(ViewChange {
                    problems,
                    group: None,
                });
            }
        };
        let mut view = self.lock();
        view.fields = fields;
        self.commit(view)?;
        Ok(ViewChange::default())
    }

    pub fn set_admiral(&self, on: Option<&Value>) -> io::Result<ViewChange> {
        let Some(on) = on.and_then(Value::as_bool) else {
            return Ok(ViewChange::refused("on must be true or false"));
        };
        let mut view = self.lock();
        view.admiral = on;
        self.commit(view)?;
        Ok(ViewChange::default())
    }

    fn commit(&self, view: MutexGuard<'_, FleetView>) -> io::Result<()> {
        if let Some(file) = &self.file {
            save(file, &view)?;
        }
        drop(view);
        self.changes.send_modify(|n| *n += 1);
        Ok(())
    }
}

fn load(file: &Path) -> Option<FleetView> {
    let size = fs::metadata(file).ok()?.len();
    if size > MAX_VIEW_FILE_BYTES {
        tracing::warn!(
            "[fleet] {} is over {} KiB; not read",
            file.display(),
            MAX_VIEW_FILE_BYTES / 1024
        );
        return None;
    }
    let text = fs::read_to_string(file).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let (view, skipped) = read_view(&value);
    if !skipped.is_empty() {
        tracing::warn!(
            "[fleet] {}: left out {}:\n  {}",
            file.display(),
            skipped.len(),
            skipped.join("\n  ")
        );
    }
    Some(view)
}

fn save(file: &Path, view: &FleetView) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(".tmp");
    fs::write(&tmp, to_pretty_text(view))?;
    fs::rename(&tmp, file)
}
