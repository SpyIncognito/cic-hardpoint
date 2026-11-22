//! Reading dashboards from JSON a page or a file sent. An imported file is someone else's content
//! reaching this screen, so every tab and widget is rebuilt from known fields only, every bind is
//! checked against the lists the service knows, and looks are names from the kind table. The
//! reasons are the TypeScript service's own strings, in the same order.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::{Map, Number, Value};

use sc_core::engine::needs_confirm;
use sc_core::engine::toggle_by_id;
use sc_core::js;
use sc_core::state::value_def;

use super::model::{
    Bezel, BezelBind, BezelButton, Bind, BindKind, COLUMNS, DEFAULT_ROWS, Dashboard,
    KEYBIND_REMOVED, KindDef, MAX_BEZEL_BUTTONS, MAX_BUTTON_INDEX, MAX_DISPLAY_LABEL,
    MAX_GAMEPAD_ID, MAX_LABEL, MAX_ROWS, MAX_TABS, MAX_WIDGETS, MIN_ROWS, Mode, OnOff, Placement,
    REMOVED_KIND, REMOVED_LOOKS, SCHEMA_VERSION, Tab, Widget, is_lamp_value, kind_def,
    look_only_for,
};
use crate::json::{int_in, is_integer, js_keys, js_string};

const WIDGET_FIELDS: &[&str] = &[
    "id",
    "kind",
    "x",
    "y",
    "w",
    "h",
    "bind",
    "look",
    "label",
    "hideUnsupported",
];
const TAB_FIELDS: &[&str] = &[
    "id",
    "name",
    "rows",
    "widgets",
    "placement",
    "bezel",
    "theme",
];
const PLACEMENT_FIELDS: &[&str] = &["displayLabel", "fullscreen"];
const BEZEL_FIELDS: &[&str] = &["gamepadId", "buttons"];
const BUTTON_FIELDS: &[&str] = &["button", "bind", "label"];
const BEZEL_BIND_FIELDS: &[&str] = &["kind", "id", "to"];
const BIND_FIELDS: &[&str] = &["kind", "id", "invert"];

/// Why a keybind part of an older dashboard is refused, or left out when read leniently.
fn removed(what: &str) -> String {
    format!("the {what} was removed, {KEYBIND_REMOVED}")
}

/// The lists a bind is checked against. `None` checks the form but not membership: the service's
/// own file is read before the game lists its actions.
#[derive(Debug, Clone, Default)]
pub struct Known {
    pub actions: Option<HashSet<String>>,
    pub sequences: Option<HashSet<String>>,
    /// The built-in and user theme names.
    pub themes: Option<HashSet<String>>,
}

/// `JSON.stringify(String(v).slice(0, 60))`.
fn quote(v: Option<&Value>) -> String {
    quote_str(&js_string(v))
}

pub(super) fn quote_str(s: &str) -> String {
    serde_json::to_string(&js::utf16_prefix(s, 60)).expect("a string serializes")
}

/// `/^[a-z0-9_-]{1,40}$/`
pub(super) fn is_id(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `/^[a-z0-9_]{1,80}$/`
fn is_action(s: &str) -> bool {
    (1..=80).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Plain text only: no markup, no control characters.
pub(super) fn is_plain(s: &str) -> bool {
    !s.chars().any(|c| c == '<' || c == '>' || c.is_control())
}

fn text(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

pub(super) fn extra_keys<'a>(map: &'a Map<String, Value>, known: &[&str]) -> Vec<&'a str> {
    js_keys(map)
        .into_iter()
        .filter(|k| !known.contains(k))
        .collect()
}

/// A theme reference: a name from the known themes, never a definition.
pub fn check_theme_ref(raw: &Value, known: &Known) -> Vec<String> {
    match raw.as_str().filter(|s| is_id(s)) {
        None => vec![
            "the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)"
                .to_owned(),
        ],
        Some(id) if known.themes.as_ref().is_some_and(|t| !t.contains(id)) => {
            vec![format!("unknown theme {}", quote_str(id))]
        }
        Some(_) => vec![],
    }
}

pub fn check_name(name: Option<&Value>, what: &str) -> Vec<String> {
    match text(name) {
        Some(s) if !js::trim(s).is_empty() && js::utf16_len(s) <= MAX_LABEL => {
            if is_plain(s) {
                vec![]
            } else {
                vec![format!("{what} must be plain text (no < or >)")]
            }
        }
        _ => vec![format!("{what} must be 1 to {MAX_LABEL} characters")],
    }
}

fn check_bind(def: &KindDef, raw: Option<&Value>, known: &Known) -> Result<Bind, Vec<String>> {
    let Some(raw) = raw.and_then(Value::as_object) else {
        return Err(vec!["it has no bind".to_owned()]);
    };
    let mut problems: Vec<String> = extra_keys(raw, BIND_FIELDS)
        .into_iter()
        .map(|k| format!("bind field {} is not allowed", quote_str(k)))
        .collect();
    let bk = raw.get("kind");
    let Some(kind) = text(bk)
        .and_then(BindKind::from_name)
        .filter(|k| def.binds.contains(k))
    else {
        return Err(vec![format!(
            "a {} cannot bind to {}",
            def.name.to_lowercase(),
            quote(bk)
        )]);
    };
    if let Some(fixed) = kind.fixed() {
        return if problems.is_empty() {
            Ok(fixed)
        } else {
            Err(problems)
        };
    }

    let Some(id) = text(raw.get("id")) else {
        problems.push("the bind has no id".to_owned());
        return Err(problems);
    };
    let on_off = matches!(kind, BindKind::Value | BindKind::Toggle);
    match kind {
        BindKind::Value if value_def(id).is_none() => {
            problems.push(format!("unknown value {}", quote_str(id)))
        }
        BindKind::Value if def.kind == "lamp" && !is_lamp_value(id) => {
            problems.push(format!(
                "a lamp shows a caution or an on/off value, not {}",
                quote_str(id)
            ));
        }
        BindKind::Toggle if toggle_by_id(id).is_none() => {
            problems.push(format!("unknown toggle {}", quote_str(id)))
        }
        BindKind::Action
            if !is_action(id) || known.actions.as_ref().is_some_and(|a| !a.contains(id)) =>
        {
            problems.push(format!("unknown game action {}", quote_str(id)));
        }
        BindKind::Sequence
            if !is_id(id) || known.sequences.as_ref().is_some_and(|s| !s.contains(id)) =>
        {
            problems.push(format!("unknown sequence {}", quote_str(id)));
        }
        _ => {}
    }
    let invert = raw.get("invert");
    if invert.is_some_and(|v| !v.is_boolean() || !on_off) {
        problems.push("invert must be true or false, on a value or toggle".to_owned());
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let id = id.to_owned();
    let invert = invert == Some(&Value::Bool(true));
    Ok(match kind {
        BindKind::Value => Bind::Value { id, invert },
        BindKind::Toggle => Bind::Toggle { id, invert },
        BindKind::Action => Bind::Action { id },
        BindKind::Sequence => Bind::Sequence { id },
        _ => unreachable!("fixed binds are handled above"),
    })
}

#[derive(Debug, Default)]
pub struct WidgetCheck {
    pub widget: Option<Widget>,
    /// What stops the widget.
    pub problems: Vec<String>,
    pub unknown_kind: Option<String>,
    /// Fields left out.
    pub ignored: Vec<String>,
}

/// Rebuild one widget from known fields.
pub fn check_widget(raw: &Value, rows: u32, known: &Known) -> WidgetCheck {
    let Some(obj) = raw.as_object() else {
        return WidgetCheck {
            problems: vec!["it is not a widget".to_owned()],
            ..WidgetCheck::default()
        };
    };
    let ignored: Vec<String> = extra_keys(obj, WIDGET_FIELDS)
        .into_iter()
        .map(str::to_owned)
        .collect();
    if text(obj.get("kind")) == Some(REMOVED_KIND) {
        return WidgetCheck {
            problems: vec![removed("keybind button")],
            ignored,
            ..WidgetCheck::default()
        };
    }
    let Some(def) = text(obj.get("kind")).and_then(kind_def) else {
        let kind = obj.get("kind");
        return WidgetCheck {
            problems: vec![format!("unknown widget kind {}", quote(kind))],
            unknown_kind: Some(js::utf16_prefix(&js_string(kind), 40)),
            ignored,
            ..WidgetCheck::default()
        };
    };
    let mut problems = Vec::new();
    let id = text(obj.get("id")).filter(|id| is_id(id));
    if id.is_none() {
        problems.push("its id must be 1 to 40 lower-case letters, digits, - or _".to_owned());
    }
    let rows_i = i64::from(rows);
    let cols = i64::from(COLUMNS);
    let x = int_in(obj.get("x"), 0, cols - 1);
    let y = int_in(obj.get("y"), 0, rows_i - 1);
    let w = int_in(obj.get("w"), 1, cols);
    let h = int_in(obj.get("h"), 1, rows_i);
    let rect = match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h)) if x + w <= cols && y + h <= rows_i => {
            Some((x, y, w, h))
        }
        _ => None,
    };
    match rect {
        None => problems.push(format!("it does not fit the {COLUMNS} x {rows} grid")),
        Some((_, _, w, h)) if w < def.min.w.into() || h < def.min.h.into() => {
            problems.push(format!(
                "the {} widget is at least {} x {} cells",
                def.name.to_lowercase(),
                def.min.w,
                def.min.h
            ));
        }
        Some(_) => {}
    }
    let look = text(obj.get("look")).and_then(|l| def.looks.iter().copied().find(|k| *k == l));
    if let Some(gone) = text(obj.get("look")).filter(|l| REMOVED_LOOKS.contains(&(def.kind, l))) {
        problems.push(removed(&format!("{gone} look")));
    } else if look.is_none() {
        problems.push(format!(
            "look {} is not one of {}",
            quote(obj.get("look")),
            def.looks.join(", ")
        ));
    }
    let label = obj.get("label");
    match label {
        None => {}
        Some(Value::String(s)) if js::utf16_len(s) <= MAX_LABEL => {
            if !is_plain(s) {
                problems.push("its label must be plain text (no < or >)".to_owned());
            }
        }
        Some(_) => problems.push(format!("its label must be at most {MAX_LABEL} characters")),
    }
    let hide = obj.get("hideUnsupported");
    if hide.is_some_and(|h| !h.is_boolean()) {
        problems.push("hideUnsupported must be true or false".to_owned());
    }
    let bind = check_bind(def, obj.get("bind"), known);
    let bind = match bind {
        Ok(bind) => Some(bind),
        Err(reasons) => {
            problems.extend(reasons);
            None
        }
    };
    if let (Some(raw_look), Some(bind)) = (text(obj.get("look")), &bind)
        && let Some(only) = look_only_for(raw_look)
        && !matches!(bind, Bind::Toggle { id, .. } if id == only)
    {
        problems.push(format!("the {raw_look} look is for the {only} toggle only"));
    }
    let (Some(id), Some((x, y, w, h)), Some(look), Some(bind), true) =
        (id, rect, look, bind, problems.is_empty())
    else {
        return WidgetCheck {
            problems,
            ignored,
            ..WidgetCheck::default()
        };
    };
    let label = text(label)
        .map(js::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    let cell = |n: i64| u32::try_from(n).expect("checked to be on the grid");
    WidgetCheck {
        widget: Some(Widget {
            id: id.to_owned(),
            kind: def.kind,
            x: cell(x),
            y: cell(y),
            w: cell(w),
            h: cell(h),
            bind,
            look,
            label,
            hide_unsupported: hide == Some(&Value::Bool(true)),
        }),
        problems,
        unknown_kind: None,
        ignored,
    }
}

#[derive(Debug, Default)]
pub struct PlacementCheck {
    pub placement: Option<Placement>,
    pub problems: Vec<String>,
    pub ignored: Vec<String>,
}

pub fn check_placement(raw: Option<&Value>) -> PlacementCheck {
    let Some(obj) = raw.and_then(Value::as_object) else {
        return PlacementCheck {
            problems: vec!["the placement is not a display placement".to_owned()],
            ..PlacementCheck::default()
        };
    };
    let ignored: Vec<String> = extra_keys(obj, PLACEMENT_FIELDS)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut problems = Vec::new();
    let label = text(obj.get("displayLabel"));
    match label {
        Some(l) if !js::trim(l).is_empty() && js::utf16_len(l) <= MAX_DISPLAY_LABEL => {
            if !is_plain(l) {
                problems.push("the display name must be plain text (no < or >)".to_owned());
            }
        }
        _ => problems.push(format!(
            "the display name must be 1 to {MAX_DISPLAY_LABEL} characters"
        )),
    }
    let fullscreen = obj.get("fullscreen").and_then(Value::as_bool);
    if fullscreen.is_none() {
        problems.push("full screen must be true or false".to_owned());
    }
    let (Some(label), Some(fullscreen), true) = (label, fullscreen, problems.is_empty()) else {
        return PlacementCheck {
            placement: None,
            problems,
            ignored,
        };
    };
    PlacementCheck {
        placement: Some(Placement {
            display_label: js::trim(label).to_owned(),
            fullscreen,
        }),
        problems,
        ignored,
    }
}

fn check_bezel_bind(raw: Option<&Value>, known: &Known) -> Result<BezelBind, Vec<String>> {
    let Some(raw) = raw.and_then(Value::as_object) else {
        return Err(vec!["it has no bind".to_owned()]);
    };
    let mut problems: Vec<String> = extra_keys(raw, BEZEL_BIND_FIELDS)
        .into_iter()
        .map(|k| format!("bind field {} is not allowed", quote_str(k)))
        .collect();
    let kind = raw.get("kind");
    let id = raw.get("id");
    let to = raw.get("to");
    match text(kind) {
        Some("keybind") => return Err(vec![removed("keybind bind")]),
        Some("mode") => {
            let mode = match text(to) {
                Some("NAV") => Some(Mode::Nav),
                Some("SCM") => Some(Mode::Scm),
                _ => None,
            };
            if mode.is_none() {
                problems.push("master mode must be set to NAV or SCM".to_owned());
            }
            if id.is_some() {
                problems.push("a master mode bind has no id".to_owned());
            }
            return match mode {
                Some(to) if problems.is_empty() => Ok(BezelBind::Mode { to }),
                _ => Err(problems),
            };
        }
        Some("action" | "sequence" | "toggle") => {}
        _ => {
            return Err(vec![format!(
                "a bezel button cannot bind to {}",
                quote(kind)
            )]);
        }
    }
    let kind = text(kind).unwrap_or_default();
    let Some(id) = text(id) else {
        problems.push("the bind has no id".to_owned());
        return Err(problems);
    };
    match kind {
        "action" if !is_action(id) || known.actions.as_ref().is_some_and(|a| !a.contains(id)) => {
            problems.push(format!("unknown game action {}", quote_str(id)));
        }
        "sequence" if !is_id(id) || known.sequences.as_ref().is_some_and(|s| !s.contains(id)) => {
            problems.push(format!("unknown sequence {}", quote_str(id)));
        }
        "toggle" if toggle_by_id(id).is_none() => {
            problems.push(format!("unknown toggle {}", quote_str(id)));
        }
        _ => {}
    }
    let to = match (kind, to.map(|v| text(Some(v)))) {
        (_, None) => None,
        ("toggle", Some(Some("on"))) => Some(OnOff::On),
        ("toggle", Some(Some("off"))) => Some(OnOff::Off),
        _ => {
            problems.push("to must be on or off, on a toggle".to_owned());
            None
        }
    };
    if !problems.is_empty() {
        return Err(problems);
    }
    let id = id.to_owned();
    Ok(match kind {
        "action" => BezelBind::Action { id },
        "sequence" => BezelBind::Sequence { id },
        _ => BezelBind::Toggle { id, to },
    })
}

#[derive(Debug, Default)]
pub struct BezelCheck {
    pub bezel: Option<Bezel>,
    pub problems: Vec<String>,
    pub skipped: Vec<String>,
}

/// Rebuild a tab's bezel mapping. Strict: any problem refuses it. Lenient: a bad button is left
/// out and reported; a mapping with no usable controller id is left out whole.
pub fn check_bezel(raw: Option<&Value>, known: &Known, strict: bool) -> BezelCheck {
    let refused = |problems| BezelCheck {
        problems,
        ..BezelCheck::default()
    };
    let Some(obj) = raw.and_then(Value::as_object) else {
        return refused(vec!["the bezel mapping is not a mapping".to_owned()]);
    };
    let mut problems = Vec::new();
    let pad = text(obj.get("gamepadId"));
    match pad {
        Some(p) if !js::trim(p).is_empty() && js::utf16_len(p) <= MAX_GAMEPAD_ID => {
            if !is_plain(p) {
                problems.push("the controller id must be plain text (no < or >)".to_owned());
            }
        }
        _ => problems.push(format!(
            "the controller id must be 1 to {MAX_GAMEPAD_ID} characters"
        )),
    }
    let list = obj.get("buttons").and_then(Value::as_array);
    if list.is_none() {
        problems.push("it has no button list".to_owned());
    }
    let (Some(pad), Some(list), true) = (pad, list, problems.is_empty()) else {
        return refused(problems);
    };
    let extra = extra_keys(obj, BEZEL_FIELDS);
    let mut issues = Vec::new();
    let mut left = Vec::new();
    if strict {
        issues.extend(
            extra
                .iter()
                .map(|k| format!("field {} is not allowed", quote_str(k))),
        );
    } else {
        left.extend(
            extra
                .iter()
                .map(|k| format!("ignored field {}", quote_str(k))),
        );
    }
    if list.len() > MAX_BEZEL_BUTTONS {
        if strict {
            issues.push(format!("at most {MAX_BEZEL_BUTTONS} bezel buttons"));
        } else {
            left.push(format!(
                "{} buttons over the cap of {MAX_BEZEL_BUTTONS} left out",
                list.len() - MAX_BEZEL_BUTTONS
            ));
        }
    }
    let mut buttons: Vec<BezelButton> = Vec::new();
    for (i, b) in list.iter().take(MAX_BEZEL_BUTTONS).enumerate() {
        let index = b
            .get("button")
            .filter(|v| b.is_object() && is_integer(v))
            .map(|v| format!(" (index {})", js_string(Some(v))))
            .unwrap_or_default();
        let at = format!("button {}{index}", i + 1);
        let mut reasons = Vec::new();
        let mut made = None;
        if let Some(obj) = b.as_object() {
            let button = int_in(obj.get("button"), 0, MAX_BUTTON_INDEX.into())
                .map(|n| u32::try_from(n).expect("in range"));
            match button {
                None => reasons.push(format!(
                    "the button index must be a whole number from 0 to {MAX_BUTTON_INDEX}"
                )),
                Some(n) if buttons.iter().any(|o| o.button == n) => {
                    reasons.push("its index is mapped twice".to_owned());
                }
                Some(_) => {}
            }
            let label = obj.get("label");
            match label {
                None => {}
                Some(Value::String(s)) if js::utf16_len(s) <= MAX_LABEL => {
                    if !is_plain(s) {
                        reasons.push("its label must be plain text (no < or >)".to_owned());
                    }
                }
                Some(_) => {
                    reasons.push(format!("its label must be at most {MAX_LABEL} characters"))
                }
            }
            let ignored = extra_keys(obj, BUTTON_FIELDS);
            if strict {
                reasons.extend(
                    ignored
                        .iter()
                        .map(|k| format!("field {} is not allowed", quote_str(k))),
                );
            } else if !ignored.is_empty() {
                let names: Vec<String> = ignored.iter().map(|k| quote_str(k)).collect();
                left.push(format!("{at}: ignored {}", names.join(", ")));
            }
            match check_bezel_bind(obj.get("bind"), known) {
                Ok(bind) => made = button.map(|button| (button, bind, label)),
                Err(bind_problems) => reasons.extend(bind_problems),
            }
        } else {
            reasons.push("it is not a button".to_owned());
        }
        if let (Some((button, bind, label)), true) = (made, reasons.is_empty()) {
            let label = text(label)
                .map(js::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned);
            buttons.push(BezelButton {
                button,
                bind,
                label,
            });
            continue;
        }
        if strict {
            issues.extend(reasons.iter().map(|r| format!("{at}: {r}")));
        } else {
            left.push(format!("{at}: skipped, {}", reasons.join("; ")));
        }
    }
    if !issues.is_empty() {
        return refused(issues);
    }
    BezelCheck {
        bezel: Some(Bezel {
            gamepad_id: js::trim(pad).to_owned(),
            buttons,
        }),
        problems: vec![],
        skipped: left,
    }
}

fn overlaps(a: &Widget, b: &Widget) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

#[derive(Debug, Default)]
pub struct TabCheck {
    pub tab: Option<Tab>,
    pub problems: Vec<String>,
    pub skipped: Vec<String>,
    pub unknown_kinds: Vec<String>,
}

/// Rebuild one tab. Strict (a save from a page): any problem refuses the tab. Lenient (an import
/// or the service's own file): a bad widget is skipped and reported, the rest is kept.
pub fn check_tab(raw: &Value, known: &Known, strict: bool) -> TabCheck {
    let mut out = TabCheck::default();
    let Some(obj) = raw.as_object() else {
        out.problems.push("It is not a tab".to_owned());
        return out;
    };
    let name = text(obj.get("name")).unwrap_or_default();
    let place = if name.is_empty() {
        format!("Tab {}", quote(obj.get("id")))
    } else {
        format!("Tab {}", quote_str(name))
    };
    let mut problems = Vec::new();
    let id = text(obj.get("id")).filter(|id| is_id(id));
    if id.is_none() {
        problems.push(format!(
            "{place}: the id must be 1 to 40 lower-case letters, digits, - or _"
        ));
    }
    problems.extend(check_name(obj.get("name"), &format!("{place}: the name")));
    let rows = match obj.get("rows") {
        None => Some(DEFAULT_ROWS),
        rows => int_in(rows, MIN_ROWS.into(), MAX_ROWS.into())
            .map(|r| u32::try_from(r).expect("in range")),
    };
    if rows.is_none() {
        problems.push(format!(
            "{place}: rows must be a whole number from {MIN_ROWS} to {MAX_ROWS}"
        ));
    }
    let list = obj.get("widgets").and_then(Value::as_array);
    if list.is_none() {
        problems.push(format!("{place}: it has no widget list"));
    }
    let (Some(id), Some(rows), Some(list), true) = (id, rows, list, problems.is_empty()) else {
        out.problems = problems;
        return out;
    };
    let extra = extra_keys(obj, TAB_FIELDS);
    if strict {
        problems.extend(
            extra
                .iter()
                .map(|k| format!("{place}: field {} is not allowed", quote_str(k))),
        );
    } else {
        out.skipped.extend(
            extra
                .iter()
                .map(|k| format!("{place}: ignored field {}", quote_str(k))),
        );
    }

    if list.len() > MAX_WIDGETS {
        if strict {
            problems.push(format!("{place}: at most {MAX_WIDGETS} widgets per tab"));
        } else {
            out.skipped.push(format!(
                "{place}: {} widgets over the cap of {MAX_WIDGETS} left out",
                list.len() - MAX_WIDGETS
            ));
        }
    }
    let mut widgets: Vec<Widget> = Vec::new();
    for (i, w) in list.iter().take(MAX_WIDGETS).enumerate() {
        let named = text(w.get("id"))
            .filter(|_| w.is_object())
            .map(|id| format!(" ({})", quote_str(id)))
            .unwrap_or_default();
        let at = format!("{place}, widget {}{named}", i + 1);
        let c = check_widget(w, rows, known);
        out.unknown_kinds.extend(c.unknown_kind);
        let mut reasons = c.problems;
        if let Some(widget) = &c.widget {
            if widgets.iter().any(|o| o.id == widget.id) {
                reasons.push("its id is used twice".to_owned());
            } else if widgets.iter().any(|o| overlaps(o, widget)) {
                reasons.push("it overlaps another widget".to_owned());
            }
        }
        if strict {
            problems.extend(reasons.iter().map(|r| format!("{at}: {r}")));
            problems.extend(
                c.ignored
                    .iter()
                    .map(|k| format!("{at}: field {} is not allowed", quote_str(k))),
            );
            if reasons.is_empty() {
                widgets.extend(c.widget);
            }
            continue;
        }
        match c.widget {
            Some(widget) if reasons.is_empty() => {
                widgets.push(widget);
                if !c.ignored.is_empty() {
                    let names: Vec<String> = c.ignored.iter().map(|k| quote_str(k)).collect();
                    out.skipped
                        .push(format!("{at}: ignored {}", names.join(", ")));
                }
            }
            _ => out
                .skipped
                .push(format!("{at}: skipped, {}", reasons.join("; "))),
        }
    }
    let mut placement = None;
    if obj.contains_key("placement") {
        let p = check_placement(obj.get("placement"));
        if strict {
            problems.extend(p.problems.iter().map(|r| format!("{place}: {r}")));
            problems.extend(
                p.ignored
                    .iter()
                    .map(|k| format!("{place}: placement field {} is not allowed", quote_str(k))),
            );
        } else if p.placement.is_none() {
            out.skipped.push(format!(
                "{place}: display placement left out, {}",
                p.problems.join("; ")
            ));
        } else if !p.ignored.is_empty() {
            let names: Vec<String> = p.ignored.iter().map(|k| quote_str(k)).collect();
            out.skipped
                .push(format!("{place}: placement ignored {}", names.join(", ")));
        }
        placement = p.placement;
    }
    let mut bezel = None;
    if obj.contains_key("bezel") {
        let b = check_bezel(obj.get("bezel"), known, strict);
        if strict {
            problems.extend(b.problems.iter().map(|r| format!("{place}, bezel: {r}")));
        } else if b.bezel.is_none() {
            out.skipped.push(format!(
                "{place}: bezel mapping left out, {}",
                b.problems.join("; ")
            ));
        } else {
            out.skipped
                .extend(b.skipped.iter().map(|r| format!("{place}, bezel {r}")));
        }
        bezel = b.bezel;
    }
    let mut theme = None;
    if let Some(raw) = obj.get("theme") {
        let t = check_theme_ref(raw, known);
        if strict {
            problems.extend(t.iter().map(|r| format!("{place}: {r}")));
        } else if !t.is_empty() {
            out.skipped
                .push(format!("{place}: theme left out, {}", t.join("; ")));
        }
        if t.is_empty() {
            theme = raw.as_str().map(str::to_owned);
        }
    }
    if !problems.is_empty() {
        out.problems = problems;
        return out;
    }
    out.tab = Some(Tab {
        id: id.to_owned(),
        name: js::trim(name).to_owned(),
        rows,
        widgets,
        placement,
        bezel,
        theme,
        placement_after_bezel: false,
    });
    out
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub ok: bool,
    pub tabs: usize,
    pub widgets: usize,
    /// Every tab, widget or field left out, with the reason.
    pub skipped: Vec<String>,
    pub unknown_kinds: Vec<String>,
    /// Action buttons in the confirm group; each press still needs the service-timed hold.
    pub confirm_buttons: usize,
    /// Mapped bezel buttons across all tabs, and those that send a confirm-group action (held
    /// 1.5 s too).
    pub bezel_buttons: usize,
    pub confirm_bezel_buttons: usize,
    /// The file's schema version is newer than this app's: only what this version knows was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_version: Option<Number>,
    /// Why nothing could be read, when `ok` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

impl ImportSummary {
    pub fn refused(problem: impl Into<String>) -> Self {
        Self {
            ok: false,
            tabs: 0,
            widgets: 0,
            skipped: vec![],
            unknown_kinds: vec![],
            confirm_buttons: 0,
            bezel_buttons: 0,
            confirm_bezel_buttons: 0,
            newer_version: None,
            problem: Some(problem.into()),
        }
    }
}

/// Read a dashboard file leniently: what is understood is kept, the rest is listed in the summary.
pub fn read_dashboard(input: Option<&Value>, known: &Known) -> (Option<Dashboard>, ImportSummary) {
    let Some(obj) = input.and_then(Value::as_object) else {
        return (
            None,
            ImportSummary::refused("This is not a dashboard file (no schema version)"),
        );
    };
    let version = obj
        .get("schemaVersion")
        .filter(|v| is_integer(v) && v.as_f64().is_some_and(|f| f >= 1.0));
    let Some(version) = version else {
        return (
            None,
            ImportSummary::refused("This is not a dashboard file (no schema version)"),
        );
    };
    let Some(list) = obj.get("tabs").and_then(Value::as_array) else {
        return (
            None,
            ImportSummary::refused("This dashboard file has no tabs"),
        );
    };
    let mut summary = ImportSummary {
        problem: None,
        ..ImportSummary::refused("")
    };
    if version
        .as_f64()
        .is_some_and(|v| v > f64::from(SCHEMA_VERSION))
    {
        summary.newer_version = version.as_number().cloned();
    }
    summary.skipped.extend(
        js_keys(obj)
            .into_iter()
            .filter(|k| !["schemaVersion", "tabs", "theme"].contains(k))
            .map(|k| format!("Ignored field {}", quote_str(k))),
    );
    let mut theme = None;
    if let Some(raw) = obj.get("theme") {
        let t = check_theme_ref(raw, known);
        if t.is_empty() {
            theme = raw.as_str().map(str::to_owned);
        } else {
            summary
                .skipped
                .push(format!("Dashboard theme left out, {}", t.join("; ")));
        }
    }
    if list.len() > MAX_TABS {
        summary.skipped.push(format!(
            "{} tabs over the cap of {MAX_TABS} left out",
            list.len() - MAX_TABS
        ));
    }

    let mut tabs: Vec<Tab> = Vec::new();
    for raw in list.iter().take(MAX_TABS) {
        let c = check_tab(raw, known, false);
        summary.skipped.extend(c.skipped);
        summary.unknown_kinds.extend(c.unknown_kinds);
        match c.tab {
            None => summary
                .skipped
                .extend(c.problems.iter().map(|p| format!("{p}; tab skipped"))),
            Some(tab) if tabs.iter().any(|t| t.id == tab.id) => {
                summary.skipped.push(format!(
                    "Tab {}: its id is used twice; tab skipped",
                    quote_str(&tab.name)
                ));
            }
            Some(tab) => tabs.push(tab),
        }
    }
    if tabs.is_empty() {
        summary.problem = Some("No tab in this file could be read".to_owned());
        return (None, summary);
    }
    let mut seen = HashSet::new();
    summary.unknown_kinds.retain(|k| seen.insert(k.clone()));
    summary.ok = true;
    summary.tabs = tabs.len();
    summary.widgets = tabs.iter().map(|t| t.widgets.len()).sum();
    summary.confirm_buttons = tabs
        .iter()
        .flat_map(|t| &t.widgets)
        .filter(|w| matches!(&w.bind, Bind::Action { id } if needs_confirm(id)))
        .count();
    let bezel_binds = || {
        tabs.iter()
            .flat_map(|t| t.bezel.iter().flat_map(|b| &b.buttons))
            .map(|b| &b.bind)
    };
    summary.bezel_buttons = bezel_binds().count();
    summary.confirm_bezel_buttons = bezel_binds()
        .filter(|b| matches!(b, BezelBind::Action { id } if needs_confirm(id)))
        .count();
    (
        Some(Dashboard {
            schema_version: SCHEMA_VERSION,
            theme,
            tabs,
        }),
        summary,
    )
}
