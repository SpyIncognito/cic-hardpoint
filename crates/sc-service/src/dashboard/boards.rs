//! Reading dashboards.json: version 2 (a list of dashboards, the generic one and one per ship)
//! and version 1 (one dashboard), which becomes the generic dashboard. Every dashboard and tab is
//! rebuilt from known fields as an import is; what cannot be read is left out and listed.

use std::collections::HashSet;

use serde_json::{Map, Value};

use sc_core::js;

use super::model::{
    Board, DashFile, FILE_SCHEMA_VERSION, GENERIC_ID, MAX_BOARDS, MAX_CLASS_NAME, MAX_SHIP_TEXT,
    MAX_TABS, ShipRef, Tab, generic_board, starter, starter_tab,
};
use super::validate::{
    Known, check_cell, check_name, check_tab, check_theme_ref, extra_keys, is_id, is_plain,
    quote_str, read_dashboard,
};
use crate::json::is_integer;

const FILE_FIELDS: &[&str] = &[
    "schemaVersion",
    "active",
    "switchOnShipChange",
    "dashboards",
];
const BOARD_FIELDS: &[&str] = &["id", "name", "ship", "theme", "cell", "tabs"];
const SHIP_FIELDS: &[&str] = &["manufacturer", "model", "className"];

/// What reading the file gave.
#[derive(Debug)]
pub enum FileRead {
    /// Read; `from_v1` holds a version 1 file, migrated in memory.
    Read {
        file: DashFile,
        from_v1: bool,
        skipped: Vec<String>,
    },
    /// A newer version than this build knows: never read, never written over.
    Newer(String),
    /// Not a dashboards file at all.
    Unreadable(String),
}

/// `base`, else `base-2`, `base-3` and up: the first id `taken` refuses. Ids are ASCII, so a byte
/// cut is a character cut.
pub fn free_id(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    let stem = &base[..base.len().min(36)];
    (2..)
        .map(|n| format!("{stem}-{n}"))
        .find(|id| !taken(id))
        .expect("an unused id exists")
}

/// A dashboard id from free text: lower case, `a-z 0-9 _ -`, at most 32 characters.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "ship".to_owned()
    } else {
        out
    }
}

fn ship_text(v: Option<&Value>, max: usize, required: bool) -> Result<Option<String>, ()> {
    match v {
        None | Some(Value::Null) if !required => Ok(None),
        Some(Value::String(s)) => {
            let s = js::trim(s);
            if js::utf16_len(s) > max || !is_plain(s) || (required && s.is_empty()) {
                Err(())
            } else if s.is_empty() {
                Ok(None)
            } else {
                Ok(Some(s.to_owned()))
            }
        }
        _ => Err(()),
    }
}

/// A ship reference: `manufacturer` (0 to 60), `model` (1 to 60) and `className` (0 to 100),
/// plain text.
pub fn check_ship(raw: Option<&Value>) -> Result<ShipRef, Vec<String>> {
    let Some(obj) = raw.and_then(Value::as_object) else {
        return Err(vec![
            "the ship must be { manufacturer, model, className }".to_owned(),
        ]);
    };
    let mut problems: Vec<String> = extra_keys(obj, SHIP_FIELDS)
        .into_iter()
        .map(|k| format!("ship field {} is not allowed", quote_str(k)))
        .collect();
    let manufacturer = ship_text(obj.get("manufacturer"), MAX_SHIP_TEXT, false);
    if manufacturer.is_err() {
        problems.push(format!(
            "the manufacturer must be at most {MAX_SHIP_TEXT} characters of plain text"
        ));
    }
    let model = ship_text(obj.get("model"), MAX_SHIP_TEXT, true);
    if model.is_err() {
        problems.push(format!(
            "the model must be 1 to {MAX_SHIP_TEXT} characters of plain text"
        ));
    }
    let class_name = ship_text(obj.get("className"), MAX_CLASS_NAME, false);
    if class_name.is_err() {
        problems.push(format!(
            "the class name must be at most {MAX_CLASS_NAME} characters of plain text"
        ));
    }
    match (manufacturer, model, class_name) {
        (Ok(manufacturer), Ok(Some(model)), Ok(class_name)) if problems.is_empty() => Ok(ShipRef {
            manufacturer: manufacturer.unwrap_or_default(),
            model,
            class_name,
        }),
        _ => Err(problems),
    }
}

/// Give every tab an id no other dashboard's tab has, so a detached window names one tab. The
/// renamed ones are listed.
pub fn unique_tab_ids(boards: &mut [Board], skipped: &mut Vec<String>) {
    let mut seen: HashSet<String> = HashSet::new();
    for board in boards.iter_mut() {
        for tab in &mut board.tabs {
            if !seen.contains(&tab.id) {
                seen.insert(tab.id.clone());
                continue;
            }
            let id = free_id(&tab.id, |id| seen.contains(id));
            skipped.push(format!(
                "Dashboard {}, tab {}: its id is used by another dashboard; now {}",
                quote_str(&board.name),
                quote_str(&tab.name),
                quote_str(&id)
            ));
            seen.insert(id.clone());
            tab.id = id;
        }
    }
}

fn read_board(raw: &Value, known: &Known, skipped: &mut Vec<String>) -> Result<Board, String> {
    let Some(obj) = raw.as_object() else {
        return Err("It is not a dashboard".to_owned());
    };
    let name = obj.get("name").and_then(Value::as_str).unwrap_or_default();
    let place = format!("Dashboard {}", quote_str(name));
    let id = obj.get("id").and_then(Value::as_str).filter(|id| is_id(id));
    let Some(id) = id else {
        return Err(format!(
            "{place}: the id must be 1 to 40 lower-case letters, digits, - or _"
        ));
    };
    let problems = check_name(obj.get("name"), &format!("{place}: the name"));
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    skipped.extend(
        extra_keys(obj, BOARD_FIELDS)
            .into_iter()
            .map(|k| format!("{place}: ignored field {}", quote_str(k))),
    );
    let ship = match obj.get("ship") {
        None | Some(Value::Null) => None,
        raw => match check_ship(raw) {
            Ok(ship) => Some(ship),
            Err(p) => return Err(format!("{place}: {}", p.join("; "))),
        },
    };
    if id == GENERIC_ID && ship.is_some() {
        return Err(format!("{place}: the generic dashboard is for no one ship"));
    }
    let mut theme = None;
    if let Some(raw) = obj.get("theme") {
        let t = check_theme_ref(raw, known);
        if t.is_empty() {
            theme = raw.as_str().map(str::to_owned);
        } else {
            skipped.push(format!("{place}: theme left out, {}", t.join("; ")));
        }
    }
    let cell = match obj.get("cell") {
        None | Some(Value::Null) => None,
        Some(raw) => match check_cell(raw) {
            Ok(c) => Some(c),
            Err(p) => {
                skipped.push(format!("{place}: cell size left out, {p}"));
                None
            }
        },
    };
    let list = obj.get("tabs").and_then(Value::as_array);
    let Some(list) = list else {
        return Err(format!("{place}: it has no tab list"));
    };
    if list.len() > MAX_TABS {
        skipped.push(format!(
            "{place}: {} tabs over the cap of {MAX_TABS} left out",
            list.len() - MAX_TABS
        ));
    }
    let mut tabs: Vec<Tab> = Vec::new();
    for raw in list.iter().take(MAX_TABS) {
        let c = check_tab(raw, known, false);
        skipped.extend(c.skipped.iter().map(|s| format!("{place}, {s}")));
        match c.tab {
            None => skipped.extend(
                c.problems
                    .iter()
                    .map(|p| format!("{place}, {p}; tab skipped")),
            ),
            Some(tab) if tabs.iter().any(|t| t.id == tab.id) => skipped.push(format!(
                "{place}, tab {}: its id is used twice; tab skipped",
                quote_str(&tab.name)
            )),
            Some(tab) => tabs.push(tab),
        }
    }
    if tabs.is_empty() {
        return Err(format!("{place}: no tab could be read"));
    }
    Ok(Board {
        id: id.to_owned(),
        name: js::trim(name).to_owned(),
        ship,
        theme,
        cell,
        tabs,
    })
}

fn read_v2(obj: &Map<String, Value>, known: &Known) -> FileRead {
    let Some(list) = obj.get("dashboards").and_then(Value::as_array) else {
        return FileRead::Unreadable("This dashboards file has no dashboard list".to_owned());
    };
    let mut skipped: Vec<String> = extra_keys(obj, FILE_FIELDS)
        .into_iter()
        .map(|k| format!("Ignored field {}", quote_str(k)))
        .collect();
    if list.len() > MAX_BOARDS {
        skipped.push(format!(
            "{} dashboards over the cap of {MAX_BOARDS} left out",
            list.len() - MAX_BOARDS
        ));
    }
    let mut boards: Vec<Board> = Vec::new();
    for raw in list.iter().take(MAX_BOARDS) {
        match read_board(raw, known, &mut skipped) {
            Ok(b) if boards.iter().any(|o| o.id == b.id) => skipped.push(format!(
                "Dashboard {}: its id is used twice; dashboard skipped",
                quote_str(&b.name)
            )),
            Ok(b) => boards.push(b),
            Err(p) => skipped.push(format!("{p}; dashboard skipped")),
        }
    }
    match boards.iter().position(Board::is_generic) {
        Some(0) => {}
        Some(i) => {
            let generic = boards.remove(i);
            boards.insert(0, generic);
        }
        None => {
            skipped.push("No generic dashboard could be read; the starter is used".to_owned());
            if boards.len() >= MAX_BOARDS {
                boards.pop();
            }
            boards.insert(0, generic_board(starter()));
        }
    }
    unique_tab_ids(&mut boards, &mut skipped);
    let active = obj
        .get("active")
        .and_then(Value::as_str)
        .filter(|a| boards.iter().any(|b| b.id == *a))
        .unwrap_or(GENERIC_ID)
        .to_owned();
    let switch = obj.get("switchOnShipChange") == Some(&Value::Bool(true));
    FileRead::Read {
        file: DashFile {
            schema_version: FILE_SCHEMA_VERSION,
            active,
            switch_on_ship_change: switch,
            dashboards: boards,
        },
        from_v1: false,
        skipped,
    }
}

/// Read dashboards.json leniently. A version 1 file becomes the generic dashboard; a version
/// newer than this build's is refused, so it is never written over.
pub fn read_file(input: &Value, known: &Known) -> FileRead {
    let Some(obj) = input.as_object() else {
        return FileRead::Unreadable("This is not a dashboards file".to_owned());
    };
    let version = obj
        .get("schemaVersion")
        .filter(|v| is_integer(v))
        .and_then(Value::as_f64)
        .filter(|v| *v >= 1.0);
    match version {
        None => FileRead::Unreadable("This is not a dashboards file (no schema version)".into()),
        Some(v) if v > f64::from(FILE_SCHEMA_VERSION) => FileRead::Newer(format!(
            "dashboards.json is from a newer version of the app (schema {v}); this version \
             reads up to {FILE_SCHEMA_VERSION}. It was not read and is not changed: changes \
             made now are not saved."
        )),
        Some(v) if v >= 2.0 => read_v2(obj, known),
        Some(_) => {
            let (dashboard, summary) = read_dashboard(Some(input), known);
            match dashboard {
                None => FileRead::Unreadable(summary.problem.unwrap_or_default()),
                Some(d) => FileRead::Read {
                    file: DashFile {
                        dashboards: vec![generic_board(d)],
                        ..super::model::starter_file()
                    },
                    from_v1: true,
                    skipped: summary.skipped,
                },
            }
        }
    }
}

/// The starter tab for a dashboard: in place of a tab with its id (on the generic dashboard) or
/// its name (on a ship's, whose copied tabs have their own ids).
pub fn starter_for(board: &Board, taken: impl Fn(&str) -> bool) -> (Option<usize>, Tab) {
    let mut tab = starter_tab();
    let at = board.tabs.iter().position(|t| t.id == tab.id).or_else(|| {
        (!board.is_generic())
            .then(|| board.tabs.iter().position(|t| t.name == tab.name))
            .flatten()
    });
    match at {
        Some(i) => tab.id.clone_from(&board.tabs[i].id),
        None => tab.id = free_id(&tab.id, taken),
    }
    (at, tab)
}
