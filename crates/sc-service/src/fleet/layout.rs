//! The Fleet tab's layouts: where unit objects (each its own people and its own grid of child
//! objects), member and ship cards, roll-ups, alerts and Unassigned sit, and the square cell size.
//! The format is the CIC Live board's (cic-panel `layouts.rs`, layout schema 3), checked by the
//! same rules, but saved apart from
//! CIC's: one per CIC event, and one for peer-to-peer sessions, in fleet-layout.json in the
//! settings folder. A layout holds ids, kinds, positions, binds and short labels, never card data.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use sc_core::js;

use crate::json::to_pretty_text;

pub const FILE: &str = "fleet-layout.json";
pub const FILE_SCHEMA: u32 = 1;
/// The layout format the page writes. Versions 1 (no nesting) and 2 (54 px rows, children on 24
/// columns) are still read and upgraded.
pub const LAYOUT_SCHEMA: u32 = 3;
/// Columns on the board. A unit's own grid has as many columns as the unit is wide.
pub const COLUMNS: u32 = 24;
/// The square cell sizes in px a layout may use, and the one older layouts get.
pub const CELLS: [u32; 5] = [48, 64, 72, 80, 96];
pub const DEFAULT_CELL: u32 = 72;
/// Version 2's row height in px.
const V2_ROW_PX: u32 = 54;
/// Objects in a layout, counted at every level.
pub const MAX_OBJECTS: usize = 100;
/// How deep a unit holding objects may sit: board objects are depth 0, their children depth 1.
pub const MAX_CONTAINER_DEPTH: usize = 2;
/// How deep any object may sit.
pub const MAX_DEPTH: usize = 3;
pub const MAX_LABEL: usize = 40;
pub const MAX_LAYOUT_BYTES: usize = 64 * 1024;
pub const MAX_Y: u32 = 999;
pub const MAX_H: u32 = 99;
/// Layouts kept; saving one more drops the one saved longest ago.
pub const MAX_SAVED: usize = 50;
/// A larger fleet-layout.json is not read.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The key of the layout for peer-to-peer sessions; a CIC event's is `event:<id>`.
pub const P2P_KEY: &str = "p2p";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Header,
    Unit,
    Member,
    Ship,
    Rollup,
    Alerts,
    Unassigned,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Look {
    Cards,
    Condensed,
    Full,
    Compact,
}

/// How an object's own title sits. An object without it: a unit on the board centered, the rest
/// left. Older layouts said it once for every unit ([`Layout::titles`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Titles {
    Center,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binds {
    Nothing,
    Unit,
    /// A unit, or nothing for the whole fleet.
    UnitOrEvent,
    Member,
    Ship,
}

impl Kind {
    fn binds(self) -> Binds {
        match self {
            Self::Header | Self::Unassigned | Self::Timeline => Binds::Nothing,
            Self::Unit => Binds::Unit,
            Self::Rollup | Self::Alerts => Binds::UnitOrEvent,
            Self::Member => Binds::Member,
            Self::Ship => Binds::Ship,
        }
    }

    fn looks(self) -> &'static [Look] {
        match self {
            Self::Unit => &[Look::Cards, Look::Condensed],
            Self::Member | Self::Ship => &[Look::Full, Look::Compact],
            _ => &[],
        }
    }

    /// Whether an object of this kind may sit inside a unit, not only on the board.
    pub fn may_nest(self) -> bool {
        !matches!(self, Self::Header | Self::Unassigned | Self::Timeline)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Object {
    pub id: String,
    pub kind: Kind,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<Look>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How this object's own title sits; never its children's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<Titles>,
    /// Objects on this unit's own grid; only a unit has them. None: the unit's default grid;
    /// empty: a grid emptied on purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Object>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layout {
    pub schema_version: u32,
    pub columns: u32,
    /// Square cells, this many px a side.
    #[serde(default = "default_cell")]
    pub cell: u32,
    #[serde(default)]
    pub rows: u32,
    /// Read from older layouts only: how every unit's title sat. [`Layout::upgrade`] moves it
    /// onto each unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titles: Option<Titles>,
    pub objects: Vec<Object>,
}

fn default_cell() -> u32 {
    DEFAULT_CELL
}

/// Rounds `n * num / den` half up, as the page's `Math.round` does.
fn scaled(n: u32, num: u32, den: u32) -> u32 {
    let n = u64::from(n) * u64::from(num) + u64::from(den / 2);
    u32::try_from(n / u64::from(den)).unwrap_or(u32::MAX)
}

/// Version 2 objects in version 3's terms: rows of 54 px in cells of 72, and children from their
/// unit's 24 columns to as many as the unit is wide (as unit-grid.js migrateLayout).
fn upgrade_objects(objects: &mut [Object], cols: u32) {
    for o in objects {
        o.y = scaled(o.y, V2_ROW_PX, DEFAULT_CELL);
        o.h = scaled(o.h, V2_ROW_PX, DEFAULT_CELL).max(1);
        if cols != COLUMNS {
            o.w = scaled(o.w, cols, COLUMNS).clamp(1, cols);
            o.x = scaled(o.x, cols, COLUMNS).min(cols - o.w);
        }
        let inner = o.w.clamp(1, COLUMNS);
        if let Some(children) = &mut o.children {
            upgrade_objects(children, inner);
        }
    }
}

/// A layout-wide title alignment as each unit's own, where the unit has none (as unit-grid.js
/// migrateTitles); other objects kept their titles left.
fn align_units(objects: &mut [Object], titles: Titles) {
    for o in objects {
        if o.kind == Kind::Unit && o.align.is_none() {
            o.align = Some(titles);
        }
        if let Some(children) = &mut o.children {
            align_units(children, titles);
        }
    }
}

/// 1 to 64 of `A-Z a-z 0-9 _ -`: object ids, and the ids binds name.
fn is_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `p2p`, or `event:<event id>`.
pub fn is_key(s: &str) -> bool {
    s == P2P_KEY || s.strip_prefix("event:").is_some_and(is_id)
}

impl Layout {
    /// Brings an older layout up to [`LAYOUT_SCHEMA`]. A unit without children gets its default
    /// grid, as a version 2 auto list showed its child units. A layout-wide `titles` becomes each
    /// unit's own `align`.
    pub fn upgrade(&mut self) {
        if matches!(self.schema_version, 1 | 2) {
            upgrade_objects(&mut self.objects, COLUMNS);
            self.schema_version = LAYOUT_SCHEMA;
            self.cell = DEFAULT_CELL;
        }
        if let Some(titles) = self.titles.take() {
            align_units(&mut self.objects, titles);
        }
    }

    /// What is wrong with it, in plain words; empty when it may be saved.
    pub fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if self.schema_version != LAYOUT_SCHEMA {
            p.push(format!("schemaVersion must be {LAYOUT_SCHEMA}"));
        }
        if self.columns != COLUMNS {
            p.push(format!("columns must be {COLUMNS}"));
        }
        if !CELLS.contains(&self.cell) {
            p.push("cells are 48, 64, 72, 80 or 96 px".to_owned());
        }
        if self.rows > MAX_Y + MAX_H {
            p.push("too many rows".to_owned());
        }
        let mut walk = Walk::default();
        walk.check(&self.objects, 0, COLUMNS, &mut p);
        if walk.count > MAX_OBJECTS {
            p.push(format!(
                "at most {MAX_OBJECTS} objects, counting those inside units"
            ));
        }
        p.sort();
        p.dedup();
        p
    }
}

/// What [`Layout::problems`] carries across the levels of the tree.
#[derive(Default)]
struct Walk<'a> {
    ids: std::collections::BTreeSet<&'a str>,
    count: usize,
}

impl<'a> Walk<'a> {
    /// Checks `objects` on a grid `cols` wide, `depth` levels inside others.
    fn check(&mut self, objects: &'a [Object], depth: usize, cols: u32, p: &mut Vec<String>) {
        for o in objects {
            self.count += 1;
            self.check_one(o, depth, cols, p);
            let Some(children) = &o.children else {
                continue;
            };
            if o.kind != Kind::Unit {
                p.push("only a unit may hold other objects".to_owned());
            }
            if !children.is_empty() && depth > MAX_CONTAINER_DEPTH {
                p.push(format!(
                    "a unit holding objects may sit at most {MAX_CONTAINER_DEPTH} levels inside others"
                ));
            }
            self.check(children, depth + 1, o.w, p);
        }
    }

    fn check_one(&mut self, o: &'a Object, depth: usize, cols: u32, p: &mut Vec<String>) {
        if !is_id(&o.id) || !self.ids.insert(o.id.as_str()) {
            p.push("object ids must be unique and plain".to_owned());
        }
        if depth > MAX_DEPTH {
            p.push(format!(
                "objects may sit at most {MAX_DEPTH} levels inside others"
            ));
        }
        if depth > 0 && !o.kind.may_nest() {
            p.push(
                "a header, unassigned list or timeline goes on the board, not inside a unit"
                    .to_owned(),
            );
        }
        let fits = o.w >= 1 && o.x < cols && o.x.saturating_add(o.w) <= cols;
        if !fits || o.y > MAX_Y || !(1..=MAX_H).contains(&o.h) {
            p.push("an object is outside the grid".to_owned());
        }
        let bind_ok = match (o.kind.binds(), o.bind.as_deref()) {
            (_, None) => true,
            (Binds::Nothing, Some(_)) => false,
            (Binds::Unit | Binds::UnitOrEvent | Binds::Member, Some(b)) => is_id(b),
            (Binds::Ship, Some(b)) => b.strip_prefix("ship:").is_some_and(is_id),
        };
        if !bind_ok {
            p.push("binds are ids of the kind the object shows".to_owned());
        }
        if o.look.is_some_and(|l| !o.kind.looks().contains(&l)) {
            p.push("a look the object does not have".to_owned());
        }
        if let Some(label) = &o.label {
            let plain = !label
                .chars()
                .any(|c| c.is_control() || c == '<' || c == '>');
            if js::utf16_len(label) > MAX_LABEL || !plain {
                p.push(format!(
                    "labels are plain text of up to {MAX_LABEL} characters"
                ));
            }
        }
    }
}

/// A layout from a page: parsed, upgraded from version 1 and checked.
pub fn check_layout(raw: &Value) -> Result<Layout, Vec<String>> {
    if serde_json::to_string(raw).map_or(usize::MAX, |t| t.len()) > MAX_LAYOUT_BYTES {
        return Err(vec![format!(
            "a layout is at most {} KiB",
            MAX_LAYOUT_BYTES / 1024
        )]);
    }
    let mut layout: Layout =
        serde_json::from_value(raw.clone()).map_err(|e| vec![format!("not a layout: {e}")])?;
    layout.upgrade();
    let problems = layout.problems();
    if problems.is_empty() {
        Ok(layout)
    } else {
        Err(problems)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub key: String,
    pub saved_ms: i64,
    pub layout: Layout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutsFile {
    pub schema_version: u32,
    pub layouts: Vec<Saved>,
}

impl Default for LayoutsFile {
    fn default() -> Self {
        Self {
            schema_version: FILE_SCHEMA,
            layouts: Vec::new(),
        }
    }
}

/// Read a fleet-layout.json: keeps the layouts that pass the checks (version 1 ones upgraded)
/// and lists what it left out.
pub fn read_file(raw: &Value) -> (LayoutsFile, Vec<String>) {
    let mut file = LayoutsFile::default();
    let mut skipped = Vec::new();
    for (i, entry) in raw
        .get("layouts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let key = entry.get("key").and_then(Value::as_str).unwrap_or_default();
        if !is_key(key) || file.layouts.iter().any(|s| s.key == key) {
            skipped.push(format!("layout {i}: no key, or a key used twice"));
            continue;
        }
        match check_layout(entry.get("layout").unwrap_or(&Value::Null)) {
            Ok(layout) => file.layouts.push(Saved {
                key: key.to_owned(),
                saved_ms: entry.get("savedMs").and_then(Value::as_i64).unwrap_or(0),
                layout,
            }),
            Err(p) => skipped.push(format!("layout {key}: {}", p.join("; "))),
        }
    }
    file.layouts.truncate(MAX_SAVED);
    (file, skipped)
}

/// What a save answers: the reasons it was refused, none when saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LayoutChange {
    pub problems: Vec<String>,
}

/// The saved layouts. Changes are made in memory first; an `Err` means only the file write
/// failed.
#[derive(Debug)]
pub struct FleetLayoutStore {
    file_data: Mutex<LayoutsFile>,
    file: Option<PathBuf>,
    changes: watch::Sender<u64>,
}

impl FleetLayoutStore {
    /// Kept in `file` when given, in memory only otherwise.
    pub fn new(file: Option<PathBuf>) -> Self {
        let data = file.as_deref().and_then(load).unwrap_or_default();
        Self {
            file_data: Mutex::new(data),
            file,
            changes: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LayoutsFile> {
        self.file_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn get(&self, key: &str) -> Option<Layout> {
        self.lock()
            .layouts
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.layout.clone())
    }

    pub fn file(&self) -> LayoutsFile {
        self.lock().clone()
    }

    /// Saves `raw` as the layout for `key`, or clears it when `raw` is null.
    pub fn save(&self, key: &str, raw: Option<&Value>, now_ms: i64) -> io::Result<LayoutChange> {
        if !is_key(key) {
            return Ok(LayoutChange {
                problems: vec!["the key is p2p or event:<event id>".to_owned()],
            });
        }
        let layout = match raw {
            None | Some(Value::Null) => None,
            Some(raw) => match check_layout(raw) {
                Ok(layout) => Some(layout),
                Err(problems) => return Ok(LayoutChange { problems }),
            },
        };
        let mut data = self.lock();
        let at = data.layouts.iter().position(|s| s.key == key);
        match (at, layout) {
            (None, None) => return Ok(LayoutChange::default()),
            (Some(i), None) => {
                data.layouts.remove(i);
            }
            (Some(i), Some(layout)) if data.layouts[i].layout == layout => {
                return Ok(LayoutChange::default());
            }
            (at, Some(layout)) => {
                if let Some(i) = at {
                    data.layouts.remove(i);
                }
                if data.layouts.len() >= MAX_SAVED {
                    let oldest = data
                        .layouts
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, s)| s.saved_ms)
                        .map_or(0, |(i, _)| i);
                    data.layouts.remove(oldest);
                }
                data.layouts.push(Saved {
                    key: key.to_owned(),
                    saved_ms: now_ms,
                    layout,
                });
            }
        }
        if let Some(file) = &self.file {
            save(file, &data)?;
        }
        drop(data);
        self.changes.send_modify(|n| *n += 1);
        Ok(LayoutChange::default())
    }
}

fn load(file: &Path) -> Option<LayoutsFile> {
    let size = fs::metadata(file).ok()?.len();
    if size > MAX_FILE_BYTES {
        tracing::warn!(
            "[fleet] {} is over {} KiB; not read",
            file.display(),
            MAX_FILE_BYTES / 1024
        );
        return None;
    }
    let text = fs::read_to_string(file).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let (data, skipped) = read_file(&value);
    if !skipped.is_empty() {
        tracing::warn!(
            "[fleet] {}: left out {}:\n  {}",
            file.display(),
            skipped.len(),
            skipped.join("\n  ")
        );
    }
    Some(data)
}

fn save(file: &Path, data: &LayoutsFile) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(".tmp");
    fs::write(&tmp, to_pretty_text(data))?;
    fs::rename(&tmp, file)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
