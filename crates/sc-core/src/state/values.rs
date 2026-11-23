use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::{Value, json};

use super::model::{Freshness, StateModel};
use crate::js;

/// Values a surface can show. Units are as the game sends them and are not verified against the
/// HUD yet, so none is shown; add one only after a recording and the HUD agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueKind {
    Bool,
    Number,
    Percent,
    Enum,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValueDef {
    pub id: String,
    pub label: String,
    /// Short label for a key (about 8 characters).
    pub short: String,
    pub kind: ValueKind,
    /// Where it is in the frame.
    pub path: String,
    /// For gauges `{current, max}`: the path holds the gauge and the value is current / max.
    pub gauge: bool,
    /// Proto3 default when the field is missing inside a present section.
    pub fallback: Option<Value>,
    /// Display text per enum name.
    pub enum_text: Option<&'static [(&'static str, &'static str)]>,
    /// Only meaningful in a vehicle.
    pub vehicle: bool,
    /// The unit to print after a number. The game does not state units: radar altitude is checked
    /// against the HUD (metres); the others assume the HUD's own metric units.
    pub unit: Option<&'static str>,
    /// A 0..1 fraction is shown as a percentage (with a bar); a larger value is shown as it is.
    pub fraction_as_percent: bool,
    /// Shown instead of the number when the game sends its "nothing below" sentinel (float max).
    pub sentinel_text: Option<&'static str>,
    /// For a bool: the words for true and false, when YES and NO would read oddly.
    pub words: Option<(&'static str, &'static str)>,
    /// A whole number, shown without decimals.
    pub integer: bool,
    /// For text: shown through a derivation of the raw field rather than as it is.
    pub derive: Option<Derive>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Derive {
    Manufacturer,
    Model,
    Name,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValueView {
    pub id: &'static str,
    pub label: &'static str,
    pub short: &'static str,
    pub freshness: Freshness,
    /// Text for the surface; [`DASH`] when absent or not applicable.
    pub text: String,
    /// 0..1 for percent values, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    /// The unit after the number, when there is one. After the fraction, where the TS object has
    /// it, so the JSON matches key for key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
    /// For bool values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
}

const DEPLOY: &[(&str, &str)] = &[
    ("VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED", "UP"),
    ("VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYING", "MOVING"),
    ("VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED", "DOWN"),
    ("VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTING", "MOVING"),
];

const CAUTION: &[(&str, &str)] = &[
    ("ANNUNCIATOR_STATE_NOMINAL", "OK"),
    ("ANNUNCIATOR_STATE_OFF", "OFF"),
    ("ANNUNCIATOR_STATE_MINOR_FLASHING", "CAUTION"),
    ("ANNUNCIATOR_STATE_MINOR_ON", "CAUTION"),
    ("ANNUNCIATOR_STATE_MAJOR_FLASHING", "WARNING"),
    ("ANNUNCIATOR_STATE_MAJOR_ON", "WARNING"),
];

const MASTER_MODES: &[(&str, &str)] = &[
    ("VEHICLE_MASTER_MODE_NAVIGATION", "NAV"),
    ("VEHICLE_MASTER_MODE_SCM", "SCM"),
    ("VEHICLE_MASTER_MODE_STEALTH", "STEALTH"),
];

const ON_OFF: (&str, &str) = ("ON", "OFF");

const LOCOMOTION: &[(&str, &str)] = &[
    ("ACTOR_LOCOMOTION_STATE_ON_FOOT", "ON FOOT"),
    ("ACTOR_LOCOMOTION_STATE_EVA", "EVA"),
    ("ACTOR_LOCOMOTION_STATE_SEATED", "SEATED"),
];

const CREW_ROLE: &[(&str, &str)] = &[
    ("ACTOR_CREW_ROLE_PILOT", "PILOT"),
    ("ACTOR_CREW_ROLE_GUNNER", "GUNNER"),
    ("ACTOR_CREW_ROLE_CREW", "CREW"),
];

/// A manufacturer code that prefixes a ship's class name (DRAK in DRAK_Corsair), the maker's
/// full name, and the short one a ship's name starts with ("Drake Corsair").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Maker {
    pub code: &'static str,
    pub name: &'static str,
    pub short: &'static str,
}

const fn maker(code: &'static str, name: &'static str, short: &'static str) -> Maker {
    Maker { code, name, short }
}

/// Best effort: the game does not publish this list, so a code missing here leaves the class
/// name whole.
pub const MANUFACTURERS: &[Maker] = &[
    maker("AEGS", "Aegis Dynamics", "Aegis"),
    maker("ANVL", "Anvil Aerospace", "Anvil"),
    maker("AOPO", "Aopoa", "Aopoa"),
    maker("ARGO", "Argo Astronautics", "Argo"),
    maker("BANU", "Banu", "Banu"),
    maker("CNOU", "Consolidated Outland", "C.O."),
    maker("CRUS", "Crusader Industries", "Crusader"),
    maker("DRAK", "Drake Interplanetary", "Drake"),
    maker("ESPR", "Esperia", "Esperia"),
    maker("GAMA", "Gatac Manufacture", "Gatac"),
    maker("GRIN", "Greycat Industrial", "Greycat"),
    maker("KRIG", "Kruger Intergalactic", "Kruger"),
    maker("MISC", "Musashi Industrial and Starflight Concern", "MISC"),
    maker("MRAI", "Mirai", "Mirai"),
    maker("ORIG", "Origin Jumpworks", "Origin"),
    maker("RSI", "Roberts Space Industries", "RSI"),
    maker("TMBL", "Tumbril Land Systems", "Tumbril"),
    maker("VNCL", "Vanduul", "Vanduul"),
    // Aopoa ships carry either code (XIAN on the older ones).
    maker("XIAN", "Aopoa", "Aopoa"),
    maker("XNAA", "Aopoa", "Aopoa"),
];

/// The class name's manufacturer and the rest, when its code is a known one.
fn split_class_name(class_name: &str) -> (Option<&'static Maker>, &str) {
    if let Some((code, model)) = class_name.split_once('_')
        && let Some(maker) = MANUFACTURERS.iter().find(|m| m.code == code)
    {
        return (Some(maker), model);
    }
    (None, class_name)
}

/// The manufacturer the game names, else the one the class name's code stands for.
pub fn ship_manufacturer(manufacturer: &str, class_name: &str) -> String {
    if manufacturer.is_empty() {
        split_class_name(class_name)
            .0
            .map_or("", |m| m.name)
            .to_owned()
    } else {
        manufacturer.to_owned()
    }
}

/// The class name without its manufacturer code, with spaces for underscores.
pub fn ship_model(class_name: &str) -> String {
    split_class_name(class_name).1.replace('_', " ")
}

/// An entity name is the class name with the entity id after it: DRAK_Corsair_877723283935.
fn without_entity_id(name: &str) -> &str {
    match name.rsplit_once('_') {
        Some((head, id)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => name,
    }
}

/// The ship's name for people: the game's own name when it is already plain words, else the
/// short maker and the model from the class name (or the entity name), e.g. "Drake Corsair".
/// The game sends no display name, so this is derived.
pub fn ship_name(name: &str, class_name: &str) -> String {
    let name = name.trim();
    if !name.is_empty() && !name.contains('_') {
        return name.to_owned();
    }
    let class_name = class_name.trim();
    let base = without_entity_id(if class_name.is_empty() {
        name
    } else {
        class_name
    });
    match split_class_name(base) {
        (Some(maker), model) if !model.is_empty() => {
            format!("{} {}", maker.short, model.replace('_', " "))
        }
        _ => base.replace('_', " ").trim().to_owned(),
    }
}

/// The short maker for a full manufacturer name, e.g. "Drake" for "Drake Interplanetary".
pub fn maker_short(manufacturer: &str) -> Option<&'static str> {
    MANUFACTURERS
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(manufacturer))
        .map(|m| m.short)
}

impl ValueDef {
    fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        short: impl Into<String>,
        kind: ValueKind,
        path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            short: short.into(),
            kind,
            path: path.into(),
            gauge: false,
            fallback: None,
            enum_text: None,
            vehicle: false,
            unit: None,
            fraction_as_percent: false,
            sentinel_text: None,
            words: None,
            integer: false,
            derive: None,
        }
    }

    fn in_vehicle(self) -> Self {
        Self {
            vehicle: true,
            ..self
        }
    }
    fn fallback(self, fallback: Value) -> Self {
        Self {
            fallback: Some(fallback),
            ..self
        }
    }
    fn enum_text(self, table: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            enum_text: Some(table),
            ..self
        }
    }
    fn unit(self, unit: &'static str) -> Self {
        Self {
            unit: Some(unit),
            ..self
        }
    }
    fn fraction_as_percent(self) -> Self {
        Self {
            fraction_as_percent: true,
            ..self
        }
    }
    fn sentinel_text(self, text: &'static str) -> Self {
        Self {
            sentinel_text: Some(text),
            ..self
        }
    }
    fn words(self, words: (&'static str, &'static str)) -> Self {
        Self {
            words: Some(words),
            ..self
        }
    }
    fn integer(self) -> Self {
        Self {
            integer: true,
            ..self
        }
    }
    fn derive(self, derive: Derive) -> Self {
        Self {
            derive: Some(derive),
            ..self
        }
    }
    fn gauge(self) -> Self {
        Self {
            gauge: true,
            ..self
        }
    }
}

fn ship(id: &str, label: &str, short: &str, kind: ValueKind, path: &str) -> ValueDef {
    ValueDef::new(id, label, short, kind, path).in_vehicle()
}

fn build_values() -> Vec<ValueDef> {
    use ValueKind::*;
    let instruments = "vehicle.state.flight.instruments";
    let mut values = vec![
        // Ship
        ship(
            "ship.speed",
            "Speed",
            "SPD",
            Number,
            &format!("{instruments}.speed"),
        )
        .fallback(json!(0))
        .unit("m/s"),
        ship(
            "ship.altitude",
            "Altitude",
            "ALT",
            Number,
            &format!("{instruments}.altitude"),
        )
        .fallback(json!(0))
        .unit("m")
        .sentinel_text("SPACE"),
        ship(
            "ship.radar_altitude",
            "Radar altitude",
            "R-ALT",
            Number,
            &format!("{instruments}.radar_altitude"),
        )
        .fallback(json!(0))
        .unit("m")
        .sentinel_text("SPACE"),
        ship(
            "ship.vertical_speed",
            "Vertical speed",
            "VSI",
            Number,
            &format!("{instruments}.vertical_speed_indicator"),
        )
        .fallback(json!(0))
        .unit("m/s"),
        ship(
            "ship.heading",
            "Heading",
            "HDG",
            Number,
            &format!("{instruments}.heading"),
        )
        .fallback(json!(0))
        .unit("°"),
        ship(
            "ship.hydrogen_fuel",
            "Hydrogen fuel",
            "H2",
            Number,
            &format!("{instruments}.hydrogen_fuel"),
        )
        .fallback(json!(0))
        .fraction_as_percent(),
        ship(
            "ship.quantum_fuel",
            "Quantum fuel",
            "QT",
            Number,
            &format!("{instruments}.quantum_fuel"),
        )
        .fallback(json!(0))
        .fraction_as_percent(),
        ship(
            "ship.boost_output",
            "Boost output",
            "BST OUT",
            Number,
            &format!("{instruments}.boost.output"),
        )
        .fallback(json!(0))
        .fraction_as_percent(),
        ship(
            "ship.boost_reserves",
            "Boost reserves",
            "BOOST",
            Number,
            &format!("{instruments}.boost.reserves"),
        )
        .fallback(json!(0))
        .fraction_as_percent(),
        ship(
            "ship.shields",
            "Shields",
            "SHLD",
            Number,
            "vehicle.state.shields.level",
        )
        .fallback(json!(0))
        .fraction_as_percent(),
        ship("ship.hull", "Hull", "HULL", Number, "vehicle.state.health")
            .fallback(json!(0))
            .fraction_as_percent(),
        ship(
            "ship.gear",
            "Landing gear",
            "GEAR",
            Enum,
            "vehicle.state.flight.state.gear",
        )
        .enum_text(DEPLOY),
        ship(
            "ship.landed",
            "Landed",
            "LANDED",
            Bool,
            "vehicle.state.flight.state.is_landed",
        )
        .fallback(json!(false)),
        ship(
            "ship.master_mode",
            "Master mode",
            "MODE",
            Enum,
            "vehicle.state.master_mode.mode",
        )
        .enum_text(MASTER_MODES),
        ship("ship.name", "Ship", "SHIP", Text, "vehicle.state.name")
            .fallback(json!(""))
            .derive(Derive::Name),
    ];

    for k in [
        "pwr", "wpn", "thr", "shld", "cool", "qdrv", "radr", "prox", "hit", "misl",
    ] {
        let upper = k.to_uppercase();
        values.push(
            ship(
                &format!("caution.{k}"),
                &format!("Caution {upper}"),
                &upper,
                Enum,
                &format!("vehicle.state.annunciators.{k}"),
            )
            .enum_text(CAUTION),
        );
    }

    for (k, label, short) in [
        ("powerplants", "Power plant", "PWR PLT"),
        ("engines", "Engines power", "ENG"),
        ("weapons", "Weapons power", "WPN PWR"),
        ("shield_generators", "Shield generators", "SHLD GEN"),
        ("coolers", "Coolers", "COOL"),
        ("life_support", "Life support", "LIFE"),
        ("gravity_generators", "Gravity", "GRAV"),
        ("sensors", "Sensors", "SENS"),
        ("shield_emitters", "Shield emitters", "EMIT"),
    ] {
        values.push(
            ship(
                &format!("power.{k}"),
                label,
                short,
                Bool,
                &format!("vehicle.state.power.{k}"),
            )
            .fallback(json!(false))
            .words(ON_OFF),
        );
    }

    values.push(
        ship(
            "ship.boost_critical",
            "Boost critical",
            "BST CRIT",
            Bool,
            &format!("{instruments}.boost.critical"),
        )
        .fallback(json!(false)),
    );
    values.extend([
        ship(
            "ship.manufacturer",
            "Manufacturer",
            "MAKER",
            Text,
            "vehicle.state.manufacturer",
        )
        .fallback(json!(""))
        .derive(Derive::Manufacturer),
        ship(
            "ship.model",
            "Model",
            "MODEL",
            Text,
            "vehicle.state.class_name",
        )
        .fallback(json!(""))
        .derive(Derive::Model),
        ship(
            "ship.class_name",
            "Ship class",
            "CLASS",
            Text,
            "vehicle.state.class_name",
        )
        .fallback(json!("")),
        ship(
            "ship.g_force",
            "G-force",
            "G",
            Number,
            &format!("{instruments}.g_meter.current"),
        )
        .fallback(json!(0))
        .unit("g"),
        ship(
            "ship.g_force_max",
            "G-force max",
            "G MAX",
            Number,
            &format!("{instruments}.g_meter.max"),
        )
        .fallback(json!(0))
        .unit("g"),
        // Unit not known: 0.2 to 0.98 in the recordings, so likely relative to a reference density.
        ship(
            "ship.atmospheric_density",
            "Atmospheric density",
            "ATMO",
            Number,
            &format!("{instruments}.atmospheric_density"),
        )
        .fallback(json!(0)),
        ship(
            "ship.boost_engaged",
            "Boost engaged",
            "BST ON",
            Bool,
            &format!("{instruments}.boost.engaged"),
        )
        .fallback(json!(false)),
        ship(
            "ship.master_mode_spooled",
            "Mode spooled",
            "SPOOLED",
            Bool,
            "vehicle.state.master_mode.spooled",
        )
        .fallback(json!(false)),
    ]);
    values.push(
        ValueDef::new(
            "weapons.preset",
            "Weapon preset",
            "PRESET",
            Text,
            "weapons.preset_name",
        )
        .fallback(json!("")),
    );

    // Configuration pages (read-only: the game has no action for most of these)
    for (id, label, short, field) in [
        (
            "flight.cruise",
            "Cruise mode",
            "CRUISE",
            "ifcs_cruise_mode.active",
        ),
        (
            "flight.gravity_compensation",
            "Gravity compensation",
            "GRAV CMP",
            "ifcs_gravity_compensation.active",
        ),
        (
            "flight.speed_limiter",
            "Speed limiter",
            "SPD LIM",
            "ifcs_speed_limiter.active",
        ),
        (
            "flight.boost_disables_gsafe",
            "Boost disables G-safe",
            "BST GSAFE",
            "ifcs_g_safe_disabled_by_boost",
        ),
        (
            "hud.advanced_scm",
            "Advanced HUD: SCM",
            "AHUD SCM",
            "advanced_hud_enabled_scm",
        ),
        (
            "hud.advanced_nav",
            "Advanced HUD: NAV",
            "AHUD NAV",
            "advanced_hud_enabled_nav",
        ),
        (
            "hud.advanced_labels",
            "Advanced HUD: labels",
            "AHUD LBL",
            "advanced_hud_labels",
        ),
    ] {
        values.push(
            ship(
                id,
                label,
                short,
                Bool,
                &format!("vehicle.state.flight.settings.{field}"),
            )
            .fallback(json!(false))
            .words(ON_OFF),
        );
    }

    for (k, label, short) in [
        ("gimbals_locked", "Gimbals locked", "GMBL LCK"),
        ("precision_lines", "Precision lines", "PREC LN"),
        ("use_lag_pips", "Lag pips", "LAG PIP"),
        ("use_fading_pips", "Fade pips", "FADE PIP"),
        ("use_single_pips", "Single pips", "ONE PIP"),
        ("magnified_ui", "Magnify symbology", "MAGNIFY"),
        ("staggered_fire", "Staggered fire", "STAGGER"),
    ] {
        values.push(
            ValueDef::new(
                format!("weapons.{k}"),
                label,
                short,
                Bool,
                format!("weapons.settings.{k}"),
            )
            .fallback(json!(false))
            .words(ON_OFF),
        );
    }

    // Character
    for (k, label, short) in [
        ("health", "Health", "HP"),
        ("oxygen", "Oxygen", "O2"),
        ("hunger", "Hunger", "FOOD"),
        ("thirst", "Thirst", "WATER"),
    ] {
        values.push(
            ValueDef::new(
                format!("actor.{k}"),
                label,
                short,
                Percent,
                format!("actor.vitals.{k}"),
            )
            .gauge(),
        );
    }
    let actor = |id: &str, label: &str, short: &str, kind, path: &str| {
        ValueDef::new(
            format!("actor.{id}"),
            label,
            short,
            kind,
            format!("actor.{path}"),
        )
    };
    values.extend([
        // Assumed Celsius: the recordings hold 36.5 with a max of 100.
        actor(
            "body_temperature",
            "Body temperature",
            "BODY T",
            Number,
            "vitals.body_temperature.current",
        )
        .fallback(json!(0))
        .unit("°C"),
        actor(
            "heart_rate",
            "Heart rate",
            "HR",
            Number,
            "vitals.heart_rate",
        )
        .fallback(json!(0))
        .unit("bpm"),
        // Assumed g: about 1.0 on a planet and in a ship with gravity in the recordings.
        actor(
            "gravity",
            "Local gravity",
            "LOCAL G",
            Number,
            "vitals.gravity",
        )
        .fallback(json!(0))
        .unit("g"),
        // Assumed atmospheres: 1 in a pressurised ship and down to 0.87 outside in the recordings.
        actor("pressure", "Pressure", "PRESS", Number, "vitals.pressure")
            .fallback(json!(0))
            .unit("atm"),
        actor(
            "intoxication",
            "Intoxication",
            "INTOX",
            Number,
            "vitals.intoxication_level",
        )
        .fallback(json!(0))
        .integer(),
        // Assumed Celsius: 21 in a ship and 2.7 outside in the recordings.
        actor(
            "external_temperature",
            "External temperature",
            "EXT T",
            Number,
            "environment.external_temperature",
        )
        .fallback(json!(0))
        .unit("°C"),
        // No unit: the recordings never carry a nonzero rate.
        actor(
            "radiation",
            "Ambient radiation",
            "RAD",
            Number,
            "environment.ambient_radiation_rate",
        )
        .fallback(json!(0)),
        actor(
            "breathable",
            "Breathable atmosphere",
            "AIR",
            Bool,
            "environment.breathable_atmosphere",
        )
        .fallback(json!(false))
        .words(("BREATHABLE", "NOT BREATHABLE")),
        actor("locomotion", "Locomotion", "MOVE", Enum, "state.locomotion").enum_text(LOCOMOTION),
        actor("crew_role", "Crew role", "ROLE", Enum, "state.crew_role").enum_text(CREW_ROLE),
        actor("seat", "Seat", "SEAT", Text, "state.seat_name").fallback(json!("")),
    ]);

    values
}

/// Every value a surface can show, in display order.
pub static VALUES: LazyLock<Vec<ValueDef>> = LazyLock::new(build_values);

static BY_ID: LazyLock<HashMap<&'static str, &'static ValueDef>> =
    LazyLock::new(|| VALUES.iter().map(|d| (d.id.as_str(), d)).collect());

pub fn value_def(id: &str) -> Option<&'static ValueDef> {
    BY_ID.get(id).copied()
}

/// Shown when a value is absent or not applicable.
pub const DASH: &str = "\u{2014}";

const SENTINEL: f64 = 1e30;

fn format_number(n: f64, integer: bool) -> String {
    let abs = n.abs();
    if abs >= 10000.0 {
        format!("{}k", js::to_fixed(n / 1000.0, 0))
    } else if integer || abs >= 100.0 {
        js::to_fixed(n, 0)
    } else if abs >= 10.0 {
        js::to_fixed(n, 1)
    } else {
        js::to_fixed(n, 2)
    }
}

/// `Math.round(x * 100)` as text, for a fraction in 0..1 (where JS and Rust rounding agree).
fn percent_text(fraction: f64) -> String {
    format!("{}%", js::number_to_string((fraction * 100.0).round()))
}

/// Read a value for display. A ship value while not in a ship is absent, never a stale number.
pub fn read_value(state: &StateModel, id: &str) -> Option<ValueView> {
    let def = value_def(id)?;
    let view = |freshness, text: String| ValueView {
        id: &def.id,
        label: &def.label,
        short: &def.short,
        unit: None,
        freshness,
        text,
        fraction: None,
        on: None,
    };
    if def.vehicle && !state.in_vehicle() {
        return Some(view(Freshness::Absent, DASH.to_owned()));
    }

    let r = state.read(&def.path, def.fallback.as_ref());
    let value = match r.value {
        Some(value) if r.freshness == Freshness::Fresh => value,
        _ => return Some(view(r.freshness, DASH.to_owned())),
    };
    let fresh = |text: String| view(Freshness::Fresh, text);

    Some(match def.kind {
        ValueKind::Percent => {
            let field = |name| {
                value
                    .get(name)
                    .filter(|v| !v.is_null())
                    .map_or(0.0, js::to_number)
            };
            let max = field("max");
            if max.is_nan() || max <= 0.0 {
                return Some(fresh(DASH.to_owned()));
            }
            let ratio = field("current") / max;
            // NaN passes through, as JS Math.min/Math.max do.
            let fraction = if ratio.is_nan() {
                ratio
            } else {
                ratio.clamp(0.0, 1.0)
            };
            ValueView {
                fraction: Some(fraction),
                ..fresh(percent_text(fraction))
            }
        }
        ValueKind::Number => {
            let n = js::to_number(value);
            // The game sends float max (3.4e38) when there is no ground below, e.g. in space.
            if !n.is_finite() || n.abs() >= SENTINEL {
                return Some(fresh(def.sentinel_text.unwrap_or(DASH).to_owned()));
            }
            if def.fraction_as_percent && (0.0..=1.0).contains(&n) {
                return Some(ValueView {
                    fraction: Some(n),
                    unit: Some("%"),
                    ..fresh(percent_text(n))
                });
            }
            match def.unit {
                Some("m") if n.abs() >= 10_000.0 => ValueView {
                    unit: Some("km"),
                    ..fresh(format!("{} km", format_number(n / 1000.0, false)))
                },
                Some(unit) => {
                    let space = if unit == "°" { "" } else { " " };
                    ValueView {
                        unit: Some(unit),
                        ..fresh(format!("{}{space}{unit}", format_number(n, def.integer)))
                    }
                }
                None => fresh(format_number(n, def.integer)),
            }
        }
        ValueKind::Bool => {
            let on = js::truthy(value);
            let (yes, no) = def.words.unwrap_or(("YES", "NO"));
            ValueView {
                on: Some(on),
                ..fresh(if on { yes } else { no }.to_owned())
            }
        }
        ValueKind::Enum => {
            let text = value
                .as_str()
                .and_then(|name| def.enum_text?.iter().find(|(k, _)| *k == name))
                .map_or(DASH, |(_, text)| text);
            fresh(text.to_owned())
        }
        ValueKind::Text => {
            let raw = js::to_display_string(value);
            let class_name = || {
                let empty = json!("");
                let r = state.read("vehicle.state.class_name", Some(&empty));
                r.value.map_or_else(String::new, js::to_display_string)
            };
            let text = match def.derive {
                Some(Derive::Manufacturer) => ship_manufacturer(&raw, &class_name()),
                Some(Derive::Model) => ship_model(&raw),
                Some(Derive::Name) => {
                    let class_name = class_name();
                    state
                        .ship_name_for(&class_name)
                        .map_or_else(|| ship_name(&raw, &class_name), str::to_owned)
                }
                None => raw,
            };
            fresh(if text.is_empty() {
                DASH.to_owned()
            } else {
                text
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn formats_gauges_enums_and_numbers_and_never_shows_a_ship_value_outside_a_ship() {
        let mut s = StateModel::default();
        s.ingest(&json!({ "actor": { "vitals": { "oxygen": { "current": 45, "max": 100 } } } }));
        let oxygen = read_value(&s, "actor.oxygen").unwrap();
        assert_eq!((oxygen.text.as_str(), oxygen.fraction), ("45%", Some(0.45)));
        let speed = read_value(&s, "ship.speed").unwrap();
        assert_eq!(
            (speed.freshness, speed.text.as_str()),
            (Freshness::Absent, DASH)
        );

        s.ingest(&json!({ "vehicle": { "state": {
            "name": "C2",
            "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" },
            "flight": { "instruments": { "speed": 212.4 } },
        } } }));
        assert_eq!(read_value(&s, "ship.master_mode").unwrap().text, "SCM");
        let speed = read_value(&s, "ship.speed").unwrap();
        assert_eq!((speed.text.as_str(), speed.unit), ("212 m/s", Some("m/s")));
        assert_eq!(read_value(&s, "ship.altitude").unwrap().text, "0.00 m");
        assert_eq!(read_value(&s, "caution.qdrv").unwrap().text, DASH);
        assert!(read_value(&s, "nope").is_none());
    }

    #[test]
    fn shows_space_for_the_no_ground_sentinel_km_above_10_km_and_on_off_for_power() {
        let mut s = StateModel::default();
        let flt_max = 3.4028234663852886e38;
        s.ingest(&json!({ "vehicle": { "state": {
            "name": "C2",
            "power": { "engines": true },
            "flight": { "instruments": { "altitude": flt_max, "radar_altitude": 25_400, "vertical_speed_indicator": flt_max } },
        } } }));
        assert_eq!(read_value(&s, "ship.altitude").unwrap().text, "SPACE");
        assert_eq!(
            read_value(&s, "ship.radar_altitude").unwrap().text,
            "25.4 km"
        );
        assert_eq!(read_value(&s, "ship.vertical_speed").unwrap().text, DASH);
        assert_eq!(read_value(&s, "power.engines").unwrap().text, "ON");
    }

    #[test]
    fn names_the_ship_from_its_manufacturer_field_else_from_the_class_name_code() {
        let mut s = StateModel::default();
        let text = |s: &StateModel, id| read_value(s, id).unwrap().text;
        s.ingest(
            &json!({ "vehicle": { "state": { "name": "Corsair", "class_name": "DRAK_Corsair" } } }),
        );
        assert_eq!(text(&s, "ship.manufacturer"), "Drake Interplanetary");
        assert_eq!(text(&s, "ship.model"), "Corsair");
        assert_eq!(text(&s, "ship.class_name"), "DRAK_Corsair");
        s.ingest(&json!({ "vehicle": { "state": { "name": "C", "class_name": "RSI_Constellation_Andromeda", "manufacturer": "RSI Corp" } } }));
        assert_eq!(text(&s, "ship.manufacturer"), "RSI Corp");
        assert_eq!(text(&s, "ship.model"), "Constellation Andromeda");
        s.ingest(&json!({ "vehicle": { "state": { "name": "X", "class_name": "SHIP_CLASS" } } }));
        assert_eq!(text(&s, "ship.manufacturer"), DASH);
        assert_eq!(text(&s, "ship.model"), "SHIP CLASS");
    }

    #[test]
    fn derives_a_friendly_ship_name() {
        assert_eq!(
            ship_name("DRAK_Corsair_877723283935", "DRAK_Corsair"),
            "Drake Corsair"
        );
        assert_eq!(ship_name("DRAK_Corsair_877723283935", ""), "Drake Corsair");
        assert_eq!(ship_name("", "ANVL_Carrack"), "Anvil Carrack");
        assert_eq!(ship_name("", "AEGS_Gladius"), "Aegis Gladius");
        assert_eq!(ship_name("", "DRAK_Cutlass_Black"), "Drake Cutlass Black");
        assert_eq!(ship_name("", "VNCL_Scythe"), "Vanduul Scythe");
        assert_eq!(ship_name("", "XNAA_SanTokYai"), "Aopoa SanTokYai");
        assert_eq!(ship_name("", "XIAN_Nox"), "Aopoa Nox");
        assert_eq!(ship_name("", "CNOU_Mustang_Alpha"), "C.O. Mustang Alpha");
        assert_eq!(ship_name("", "ORIG_600i"), "Origin 600i");
        assert_eq!(ship_name("", "ZZZZ_Mystery_Ship"), "ZZZZ Mystery Ship");
        assert_eq!(ship_name("ZZZZ_Mystery_42", ""), "ZZZZ Mystery");
        assert_eq!(ship_name("Corsair", "DRAK_Corsair"), "Corsair");
        assert_eq!(ship_name("  My Ship ", "DRAK_Corsair"), "My Ship");
        assert_eq!(ship_name("", ""), "");
        assert_eq!(ship_name("_123", ""), "");
        assert_eq!(maker_short("drake interplanetary"), Some("Drake"));
        assert_eq!(maker_short("Nobody"), None);
    }

    #[test]
    fn every_manufacturer_code_is_listed_once_with_a_short_name() {
        let codes: HashSet<_> = MANUFACTURERS.iter().map(|m| m.code).collect();
        assert_eq!(codes.len(), MANUFACTURERS.len());
        assert!(
            MANUFACTURERS
                .iter()
                .all(|m| !m.short.is_empty() && !m.code.contains('_'))
        );
    }

    #[test]
    fn the_ship_value_is_the_friendly_name_or_the_users_own() {
        let mut s = StateModel::default();
        s.ingest(&json!({ "vehicle": { "state": { "name": "DRAK_Corsair_877723283935", "class_name": "DRAK_Corsair" } } }));
        assert_eq!(read_value(&s, "ship.name").unwrap().text, "Drake Corsair");
        s.set_ship_names(HashMap::from([(
            "DRAK_Corsair".to_owned(),
            "Old Betsy".to_owned(),
        )]));
        assert_eq!(read_value(&s, "ship.name").unwrap().text, "Old Betsy");
        s.ingest(&json!({ "vehicle": { "state": { "name": "ANVL_Arrow_1", "class_name": "ANVL_Arrow" } } }));
        assert_eq!(read_value(&s, "ship.name").unwrap().text, "Anvil Arrow");
        s.ingest(&json!({ "vehicle": { "state": { "health": 1 } } }));
        assert_eq!(read_value(&s, "ship.name").unwrap().text, DASH);
    }

    #[test]
    fn shows_g_force_and_the_character_vitals_environment_and_seat() {
        let mut s = StateModel::default();
        s.ingest(&json!({
            "vehicle": { "state": { "name": "C2", "master_mode": { "spooled": true }, "flight": { "instruments": { "g_meter": { "current": 4.567, "max": 29.94 } } } } },
            "actor": {
                "vitals": { "body_temperature": { "current": 36.5, "max": 100 }, "heart_rate": 72.4, "intoxication_level": 2 },
                "environment": { "breathable_atmosphere": false },
                "state": { "locomotion": "ACTOR_LOCOMOTION_STATE_EVA", "crew_role": "ACTOR_CREW_ROLE_GUNNER", "seat_name": "Turret" },
            },
        }));
        let text = |id| read_value(&s, id).unwrap().text;
        assert_eq!(text("ship.g_force"), "4.57 g");
        assert_eq!(text("ship.g_force_max"), "29.9 g");
        assert_eq!(text("ship.master_mode_spooled"), "YES");
        assert_eq!(text("actor.body_temperature"), "36.5 °C");
        assert_eq!(text("actor.heart_rate"), "72.4 bpm");
        assert_eq!(text("actor.intoxication"), "2");
        assert_eq!(text("actor.breathable"), "NOT BREATHABLE");
        assert_eq!(text("actor.locomotion"), "EVA");
        assert_eq!(text("actor.crew_role"), "GUNNER");
        assert_eq!(text("actor.seat"), "Turret");
    }

    #[test]
    fn has_unique_ids() {
        let ids: HashSet<_> = VALUES.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids.len(), VALUES.len());
    }

    #[test]
    fn number_formatting_rounds_halfway_values_like_to_fixed() {
        assert_eq!(format_number(0.125, false), "0.13");
        assert_eq!(format_number(2.675, false), "2.67");
        assert_eq!(format_number(1.005, false), "1.00");
        assert_eq!(format_number(10.25, false), "10.3");
        assert_eq!(format_number(100.5, false), "101");
        assert_eq!(format_number(12_500.0, false), "13k");
        assert_eq!(format_number(-0.0, false), "0.00");
    }
}
