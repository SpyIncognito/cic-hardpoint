//! The dashboard file's types, the widget kind table and the starter tab. A new widget kind or
//! look is a row in [`WIDGET_KINDS`]; validation and the page's kind list read the table.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use sc_core::state::{VALUES, ValueKind};

pub const SCHEMA_VERSION: u32 = 1;
pub const COLUMNS: u32 = 24;
pub const DEFAULT_ROWS: u32 = 14;
pub const MIN_ROWS: u32 = 4;
pub const MAX_ROWS: u32 = 48;
pub const MAX_TABS: usize = 20;
pub const MAX_WIDGETS: usize = 200;
/// In UTF-16 code units, as the TS counts `length`.
pub const MAX_LABEL: usize = 40;
/// A display's name as the browser or Windows reports it, e.g. "DELL U2720Q".
pub const MAX_DISPLAY_LABEL: usize = 100;
pub const MAX_GAMEPAD_ID: usize = 200;
pub const MAX_BEZEL_BUTTONS: usize = 64;
/// Button indices a game controller may report; the MIP settings take the same range.
pub const MAX_BUTTON_INDEX: u32 = 127;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BindKind {
    Value,
    Toggle,
    Action,
    Sequence,
    Mode,
    Status,
    Shields,
    Power,
    Weapons,
    Fuel,
    Config,
}

impl BindKind {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "value" => Self::Value,
            "toggle" => Self::Toggle,
            "action" => Self::Action,
            "sequence" => Self::Sequence,
            "mode" => Self::Mode,
            "status" => Self::Status,
            "shields" => Self::Shields,
            "power" => Self::Power,
            "weapons" => Self::Weapons,
            "fuel" => Self::Fuel,
            "config" => Self::Config,
            _ => return None,
        })
    }

    /// The bind for a kind with no id: the widget shows one fixed part of the ship.
    pub fn fixed(self) -> Option<Bind> {
        Some(match self {
            Self::Mode => Bind::Mode,
            Self::Status => Bind::Status,
            Self::Shields => Bind::Shields,
            Self::Power => Bind::Power,
            Self::Weapons => Bind::Weapons,
            Self::Fuel => Bind::Fuel,
            Self::Config => Bind::Config,
            Self::Value | Self::Toggle | Self::Action | Self::Sequence => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Bind {
    /// `invert`: a lamp or toggle that shows the opposite state, e.g. CPLD lit while not decoupled.
    Value {
        id: String,
        #[serde(skip_serializing_if = "is_false")]
        invert: bool,
    },
    Toggle {
        id: String,
        #[serde(skip_serializing_if = "is_false")]
        invert: bool,
    },
    Action {
        id: String,
    },
    Sequence {
        id: String,
    },
    Mode,
    Status,
    Shields,
    Power,
    Weapons,
    Fuel,
    Config,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Widget {
    pub id: String,
    /// A kind from [`WIDGET_KINDS`].
    pub kind: &'static str,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub bind: Bind,
    /// One of the kind's looks.
    pub look: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Hidden in use while the ship lacks the function it shows; greyed otherwise.
    #[serde(rename = "hideUnsupported", skip_serializing_if = "is_false")]
    pub hide_unsupported: bool,
}

/// Where a detached tab goes: the display by its name, never its number, since Windows
/// renumbers displays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub display_label: String,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Mode {
    #[serde(rename = "NAV")]
    Nav,
    #[serde(rename = "SCM")]
    Scm,
}

/// What a bezel button sends: a toggle with `to` sets that state, without it acts as a widget
/// click.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BezelBind {
    Action {
        id: String,
    },
    Sequence {
        id: String,
    },
    Toggle {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        to: Option<OnOff>,
    },
    Mode {
        to: Mode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BezelButton {
    pub button: u32,
    pub bind: BezelBind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One game controller (the browser's gamepad id) and what each of its buttons does on this tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bezel {
    pub gamepad_id: String,
    pub buttons: Vec<BezelButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tab {
    pub id: String,
    pub name: String,
    pub rows: u32,
    pub widgets: Vec<Widget>,
    pub placement: Option<Placement>,
    pub bezel: Option<Bezel>,
    /// This tab's own theme, by name, over the dashboard's.
    pub theme: Option<String>,
    /// The TS store keeps a tab as a JS object: a placement set after the bezel is written after
    /// it, until the tab is next rebuilt. Kept so both services write the same bytes.
    pub placement_after_bezel: bool,
}

impl Serialize for Tab {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("rows", &self.rows)?;
        map.serialize_entry("widgets", &self.widgets)?;
        let placement = self.placement.as_ref();
        if let Some(p) = placement.filter(|_| !self.placement_after_bezel) {
            map.serialize_entry("placement", p)?;
        }
        if let Some(b) = &self.bezel {
            map.serialize_entry("bezel", b)?;
        }
        if let Some(p) = placement.filter(|_| self.placement_after_bezel) {
            map.serialize_entry("placement", p)?;
        }
        if let Some(t) = &self.theme {
            map.serialize_entry("theme", t)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub schema_version: u32,
    /// The dashboard's theme by name (built-in or the user's); RSI when left out. A theme is
    /// only ever named here, never defined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

/// What a widget kind may bind to, its looks (the first is the default), its starting and
/// smallest size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KindDef {
    #[serde(skip)]
    pub kind: &'static str,
    pub name: &'static str,
    pub looks: &'static [&'static str],
    pub binds: &'static [BindKind],
    pub size: Size,
    pub min: Size,
}

const ONE: Size = Size { w: 1, h: 1 };

const fn kind(
    kind: &'static str,
    name: &'static str,
    looks: &'static [&'static str],
    binds: &'static [BindKind],
    size: (u32, u32),
    min: Size,
) -> KindDef {
    KindDef {
        kind,
        name,
        looks,
        binds,
        size: Size {
            w: size.0,
            h: size.1,
        },
        min,
    }
}

#[rustfmt::skip]
pub const WIDGET_KINDS: &[KindDef] = &[
    kind("lamp", "Lamp", &["square", "round"], &[BindKind::Value, BindKind::Toggle], (2, 1), ONE),
    kind("readout", "Readout", &["number", "bar"], &[BindKind::Value], (3, 2), ONE),
    kind("toggle", "Toggle", &["button", "switch", "lever"], &[BindKind::Toggle], (3, 3), ONE),
    kind("action", "Action button", &["square", "round"], &[BindKind::Action], (3, 3), ONE),
    kind("sequence", "Sequence button", &["push", "square", "round"], &[BindKind::Sequence], (3, 3), ONE),
    kind("mode", "Master mode", &["selector"], &[BindKind::Mode], (4, 2), ONE),
    kind("status", "Status strip", &["strip"], &[BindKind::Status], (24, 1), ONE),
    kind("shields", "Shield faces", &["outline"], &[BindKind::Shields], (6, 6), Size { w: 6, h: 6 }),
    kind("power", "Power", &["columns"], &[BindKind::Power], (9, 5), Size { w: 6, h: 4 }),
    kind("weapons", "Weapons", &["list"], &[BindKind::Weapons], (7, 5), Size { w: 6, h: 4 }),
    kind("fuel", "Fuel", &["bars"], &[BindKind::Fuel], (4, 6), Size { w: 2, h: 3 }),
    kind("config", "Configuration", &["list"], &[BindKind::Config], (6, 9), Size { w: 4, h: 4 }),
];

/// Why a stored keybind kind, look or bezel bind is left out: the app is API only, and the
/// Stream Deck plugin keeps its own keybinds.
pub const KEYBIND_REMOVED: &str = "the app is API only (keybinds stay on the Stream Deck plugin)";
/// The keybind widget kind, and the keybind looks of kinds that stay, as (kind, look).
pub const REMOVED_KIND: &str = "keybind";
pub const REMOVED_LOOKS: &[(&str, &str)] = &[("shields", "controls"), ("power", "pips")];

/// Looks drawn for one toggle only: the gear lever is the landing gear's.
pub const LOOK_ONLY_FOR: &[(&str, &str)] = &[("lever", "gear")];
/// The lever's own starting size, taller than a button.
pub const LEVER_SIZE: Size = Size { w: 2, h: 4 };

pub fn look_only_for(look: &str) -> Option<&'static str> {
    LOOK_ONLY_FOR
        .iter()
        .find(|(l, _)| *l == look)
        .map(|(_, toggle)| *toggle)
}

/// [`LOOK_ONLY_FOR`] as the page gets it: an object keyed by look.
pub struct LookOnlyFor;

impl Serialize for LookOnlyFor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(LOOK_ONLY_FOR.len()))?;
        for (look, toggle) in LOOK_ONLY_FOR {
            map.serialize_entry(look, toggle)?;
        }
        map.end()
    }
}

pub fn kind_def(kind: &str) -> Option<&'static KindDef> {
    WIDGET_KINDS.iter().find(|k| k.kind == kind)
}

/// The kind table as the page gets it: an object keyed by kind, in table order.
pub struct KindTable;

impl Serialize for KindTable {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(WIDGET_KINDS.len()))?;
        for def in WIDGET_KINDS {
            map.serialize_entry(def.kind, def)?;
        }
        map.end()
    }
}

/// Value ids a lamp can show: the cautions and the on/off values.
pub fn lamp_values() -> Vec<&'static str> {
    VALUES
        .iter()
        .map(|v| v.id.as_str())
        .filter(|id| is_lamp_value(id))
        .collect()
}

pub fn is_lamp_value(id: &str) -> bool {
    sc_core::state::value_def(id)
        .is_some_and(|v| v.kind == ValueKind::Bool || v.id.starts_with("caution."))
}

fn lamp(id: &str, x: u32, y: u32, bind: Bind, label: &str) -> Widget {
    Widget {
        id: id.into(),
        kind: "lamp",
        x,
        y,
        w: 2,
        h: 1,
        bind,
        look: "square",
        label: Some(label.into()),
        hide_unsupported: false,
    }
}

fn readout(id: &str, x: u32, y: u32, w: u32, value: &str, look: &'static str) -> Widget {
    Widget {
        id: id.into(),
        kind: "readout",
        x,
        y,
        w,
        h: 4,
        bind: value_bind(value),
        look,
        label: None,
        hide_unsupported: false,
    }
}

fn toggle(id: &str, x: u32, bind: Bind, label: Option<&str>) -> Widget {
    Widget {
        id: id.into(),
        kind: "toggle",
        x,
        y: 9,
        w: 6,
        h: 5,
        bind,
        look: "button",
        label: label.map(Into::into),
        hide_unsupported: false,
    }
}

fn value_bind(id: &str) -> Bind {
    Bind::Value {
        id: id.into(),
        invert: false,
    }
}

fn toggle_bind(id: &str, invert: bool) -> Bind {
    Bind::Toggle {
        id: id.into(),
        invert,
    }
}

/// The starter tab: the main status, the MFD status rail down the left, and four ship systems.
/// The same tab as the TS STARTER_TAB, field for field.
#[rustfmt::skip]
pub fn starter_tab() -> Tab {
    Tab {
        id: "main".into(),
        name: "Main".into(),
        rows: DEFAULT_ROWS,
        widgets: vec![
            Widget { id: "status".into(), kind: "status", x: 0, y: 0, w: 24, h: 1, bind: Bind::Status, look: "strip", label: None, hide_unsupported: false },
            lamp("rail-pwr", 0, 1, value_bind("caution.pwr"), "PWR"),
            lamp("rail-wpn", 0, 2, value_bind("caution.wpn"), "WPN"),
            lamp("rail-thr", 0, 3, value_bind("caution.thr"), "THR"),
            lamp("rail-shld", 0, 4, value_bind("caution.shld"), "SHLD"),
            lamp("rail-cool", 0, 5, value_bind("caution.cool"), "COOL"),
            lamp("rail-cpld", 0, 6, toggle_bind("decoupled", true), "CPLD"),
            lamp("rail-esp", 0, 7, toggle_bind("esp", false), "ESP"),
            // LOCK is door locks until a game test says whether the MFD means gimbals or doors.
            lamp("rail-lock", 0, 8, toggle_bind("door_locks", false), "LOCK"),
            readout("speed", 2, 1, 6, "ship.speed", "number"),
            readout("altitude", 8, 1, 6, "ship.altitude", "number"),
            readout("radar-altitude", 14, 1, 6, "ship.radar_altitude", "number"),
            Widget { id: "mode".into(), kind: "mode", x: 20, y: 1, w: 4, h: 4, bind: Bind::Mode, look: "selector", label: None, hide_unsupported: false },
            readout("shields", 2, 5, 6, "ship.shields", "bar"),
            readout("hull", 8, 5, 6, "ship.hull", "bar"),
            readout("h2", 14, 5, 5, "ship.hydrogen_fuel", "bar"),
            readout("qt", 19, 5, 5, "ship.quantum_fuel", "bar"),
            toggle("vtol", 0, toggle_bind("vtol", false), None),
            toggle("gear", 6, toggle_bind("gear", false), None),
            toggle("coupled", 12, toggle_bind("decoupled", true), Some("Coupled")),
            toggle("lights", 18, toggle_bind("lights", false), Some("Lights")),
        ],
        ..Tab::default()
    }
}

pub fn starter() -> Dashboard {
    Dashboard {
        schema_version: SCHEMA_VERSION,
        theme: None,
        tabs: vec![starter_tab()],
    }
}

/// The dashboards file's own version: a list of dashboards. Version 1 files (one dashboard) are
/// read and migrated; the page and the export still see one dashboard in the version 1 shape.
pub const FILE_SCHEMA_VERSION: u32 = 2;
/// The generic dashboard's id: the one for any ship without its own.
pub const GENERIC_ID: &str = "generic";
pub const MAX_BOARDS: usize = 40;
/// A ship's manufacturer and model as the stream gives them, in UTF-16 code units.
pub const MAX_SHIP_TEXT: usize = 60;
/// The stream's raw class name, e.g. "DRAK_Corsair".
pub const MAX_CLASS_NAME: usize = 100;

/// The ship a dashboard is for: manufacturer and model (from `ship.manufacturer` and
/// `ship.model`), and the stream's raw class name when it was made in the ship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipRef {
    pub manufacturer: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
}

/// One dashboard: its tabs and theme, and the ship it is for (none for the generic one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Board {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ship: Option<ShipRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub tabs: Vec<Tab>,
}

impl Board {
    pub fn is_generic(&self) -> bool {
        self.id == GENERIC_ID
    }

    /// The version 1 shape the page draws and the export writes.
    pub fn dashboard(&self) -> Dashboard {
        Dashboard {
            schema_version: SCHEMA_VERSION,
            theme: self.theme.clone(),
            tabs: self.tabs.clone(),
        }
    }
}

/// dashboards.json, version 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashFile {
    pub schema_version: u32,
    /// The dashboard on screen.
    pub active: String,
    /// "Switch dashboard when I change ship"; off by default.
    pub switch_on_ship_change: bool,
    /// The generic dashboard first.
    pub dashboards: Vec<Board>,
}

pub fn generic_board(dash: Dashboard) -> Board {
    Board {
        id: GENERIC_ID.into(),
        name: "Generic".into(),
        ship: None,
        theme: dash.theme,
        tabs: dash.tabs,
    }
}

pub fn starter_file() -> DashFile {
    DashFile {
        schema_version: FILE_SCHEMA_VERSION,
        active: GENERIC_ID.into(),
        switch_on_ship_change: false,
        dashboards: vec![generic_board(starter())],
    }
}

/// Built-in themes that stand for a manufacturer: a new ship dashboard starts in its maker's.
pub const MANUFACTURER_THEMES: &[(&str, &str, &str)] = &[
    ("DRAK", "Drake Interplanetary", "drake"),
    ("RSI", "Roberts Space Industries", "rsi"),
];

/// The manufacturer's theme, by its name or the class name's code.
pub fn manufacturer_theme(ship: &ShipRef) -> Option<&'static str> {
    let code = ship
        .class_name
        .as_deref()
        .and_then(|c| c.split_once('_'))
        .map(|(code, _)| code);
    MANUFACTURER_THEMES
        .iter()
        .find(|(c, name, _)| {
            ship.manufacturer.eq_ignore_ascii_case(name) || code.is_some_and(|k| k == *c)
        })
        .map(|(_, _, theme)| *theme)
}
