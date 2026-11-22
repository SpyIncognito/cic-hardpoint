//! Every value and toggle the core can show, keyed by id, so a dashboard widget binds to any of
//! them by id without the service knowing the layout. Absent data stays a dash.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::Value;

use sc_core::ControlCore;
use sc_core::engine::{SequenceProgress, TOGGLES};
use sc_core::js;
use sc_core::state::values::DASH;
use sc_core::state::{Freshness, VALUES, ValueView, value_def};
use sc_game::LinkStatus;

use super::model::OnOff;
use crate::json::js_string;
use crate::portal::status::{Reading, ToggleTile, Tone, shield_faces, tile, tone_of_text};

/// How a lamp draws: the game's annunciator states for cautions, on and off for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LampState {
    Nominal,
    Off,
    Minor,
    MinorFlash,
    Major,
    MajorFlash,
    On,
    Unknown,
}

impl LampState {
    fn of_on(on: Option<bool>) -> Self {
        match on {
            Some(true) => Self::On,
            Some(false) => Self::Off,
            None => Self::Unknown,
        }
    }

    fn of_annunciator(state: &str) -> Self {
        match state {
            "ANNUNCIATOR_STATE_NOMINAL" => Self::Nominal,
            "ANNUNCIATOR_STATE_OFF" => Self::Off,
            "ANNUNCIATOR_STATE_MINOR_ON" => Self::Minor,
            "ANNUNCIATOR_STATE_MINOR_FLASHING" => Self::MinorFlash,
            "ANNUNCIATOR_STATE_MAJOR_ON" => Self::Major,
            "ANNUNCIATOR_STATE_MAJOR_FLASHING" => Self::MajorFlash,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValueEntry {
    #[serde(flatten)]
    pub view: ValueView,
    pub tone: Tone,
    pub lamp: LampState,
}

/// What a click on a widget sends: set-to-state when the state is known, else the game's own
/// toggle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Click {
    Set { toggle: &'static str, to: OnOff },
    Press { action: &'static str },
}

impl Click {
    fn toggle(id: &'static str, action: &'static str, on: Option<bool>) -> Self {
        match on {
            None => Self::Press { action },
            Some(on) => Self::Set {
                toggle: id,
                to: if on { OnOff::Off } else { OnOff::On },
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToggleEntry {
    #[serde(flatten)]
    pub tile: ToggleTile,
    pub lamp: LampState,
    /// Deploying (true) or retracting (false) while the game reports it moving.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moving_to: Option<bool>,
    pub click: Click,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WeaponRow {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub powered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Countermeasure {
    #[serde(rename = "type")]
    pub kind: String,
    pub amount: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WeaponsEntry {
    pub preset: String,
    pub rows: Vec<WeaponRow>,
    pub countermeasures: Vec<Countermeasure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConfigGroup {
    Flight,
    #[serde(rename = "HUD")]
    Hud,
    Gunnery,
}

/// One row of the in-game Configuration pages; `click` only where the game has an action for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigRow {
    pub id: &'static str,
    pub group: ConfigGroup,
    pub label: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click: Option<Click>,
}

/// Entries keyed by id, in catalog order.
#[derive(Debug, Clone, PartialEq)]
pub struct ById<T>(pub Vec<(&'static str, T)>);

impl<T: Serialize> Serialize for ById<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (id, entry) in &self.0 {
            map.serialize_entry(id, entry)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValuesModel {
    pub link: LinkStatus,
    pub in_ship: bool,
    pub running: Option<SequenceProgress>,
    pub values: ById<ValueEntry>,
    pub toggles: ById<ToggleEntry>,
    /// Shield faces by index; which side each index is has not been checked in game.
    pub shield_faces: Vec<Reading>,
    /// `None` (null) while the game sends no weapons section.
    pub weapons: Option<WeaponsEntry>,
    pub config: Vec<ConfigRow>,
}

/// Names as the game sends them, cut to a length a row can show.
const MAX_NAME: usize = 40;

/// `String(v ?? '')`, cut to [`MAX_NAME`].
fn name_of(v: Option<&Value>) -> String {
    js::utf16_prefix(&name_of_uncut(v), MAX_NAME)
}

fn name_of_uncut(v: Option<&Value>) -> String {
    v.filter(|v| !v.is_null())
        .map(|v| js_string(Some(v)))
        .unwrap_or_default()
}

fn read_weapons(core: &ControlCore) -> Option<WeaponsEntry> {
    let state = core
        .state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let section = state.read("weapons", None);
    let w = section
        .value
        .filter(|v| section.freshness == Freshness::Fresh && js::truthy(v))?;
    let list = |name| {
        w.get(name)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    };
    let flag = |v: &Value, name| v.get(name).and_then(Value::as_bool);
    Some(WeaponsEntry {
        preset: name_of(w.get("preset_name")),
        rows: list("weapons")
            .iter()
            .map(|r| WeaponRow {
                name: name_of(r.get("name")),
                powered: flag(r, "is_powered"),
                ready: flag(r, "ready_to_fire"),
            })
            .collect(),
        countermeasures: list("countermeasures")
            .iter()
            .map(|c| {
                let full = name_of_uncut(c.get("type"));
                let short = full.strip_prefix("COUNTERMEASURE_TYPE_").unwrap_or(&full);
                let amount = c.get("amount").map_or(f64::NAN, js::to_number);
                Countermeasure {
                    kind: js::utf16_prefix(short, MAX_NAME),
                    amount: if amount.is_nan() || amount == 0.0 {
                        0.0
                    } else {
                        amount
                    },
                }
            })
            .collect(),
    })
}

/// The game's advanced HUD action; which of SCM, NAV and labels it changes is not checked in game.
pub const ADVANCED_HUD_TOGGLE: &str = "v_flight_advanced_hud_toggle";

const CONFIG_VALUES: &[(ConfigGroup, &str)] = &[
    (ConfigGroup::Flight, "flight.cruise"),
    (ConfigGroup::Flight, "flight.gravity_compensation"),
    (ConfigGroup::Flight, "flight.speed_limiter"),
    (ConfigGroup::Flight, "flight.boost_disables_gsafe"),
    (ConfigGroup::Hud, "hud.advanced_scm"),
    (ConfigGroup::Hud, "hud.advanced_nav"),
    (ConfigGroup::Hud, "hud.advanced_labels"),
    (ConfigGroup::Gunnery, "weapons.gimbals_locked"),
    (ConfigGroup::Gunnery, "weapons.precision_lines"),
    (ConfigGroup::Gunnery, "weapons.use_lag_pips"),
    (ConfigGroup::Gunnery, "weapons.use_fading_pips"),
    (ConfigGroup::Gunnery, "weapons.use_single_pips"),
    (ConfigGroup::Gunnery, "weapons.magnified_ui"),
    (ConfigGroup::Gunnery, "weapons.staggered_fire"),
];
const HUD_CLICKABLE: &[&str] = &["hud.advanced_scm", "hud.advanced_nav"];

fn build_config(values: &ById<ValueEntry>, toggles: &ById<ToggleEntry>) -> Vec<ConfigRow> {
    let find = |id| {
        toggles
            .0
            .iter()
            .find(|(t, _)| *t == id)
            .map(|(_, e)| e)
            .expect("the core has the toggle")
    };
    let from_toggle = |id, label, on: Option<bool>, click: &Click| ConfigRow {
        id,
        group: ConfigGroup::Flight,
        label,
        text: match on {
            None => DASH.to_owned(),
            Some(true) => "ON".to_owned(),
            Some(false) => "OFF".to_owned(),
        },
        on,
        click: Some(click.clone()),
    };
    let decoupled = find("decoupled");
    let gsafe = find("gsafe");
    let mut rows = vec![
        from_toggle(
            "coupled",
            "Coupled",
            decoupled.tile.on.map(|on| !on),
            &decoupled.click,
        ),
        from_toggle("gsafe", "G-safe", gsafe.tile.on, &gsafe.click),
    ];
    for &(group, id) in CONFIG_VALUES {
        let (id, v) = values
            .0
            .iter()
            .find(|(v, _)| *v == id)
            .expect("the core has the value");
        rows.push(ConfigRow {
            id,
            group,
            label: v.view.label,
            text: v.view.text.clone(),
            on: v.view.on,
            click: HUD_CLICKABLE.contains(id).then_some(Click::Press {
                action: ADVANCED_HUD_TOGGLE,
            }),
        });
    }
    rows
}

/// The caution's own state: its display text folds steady and flashing together.
fn caution_lamp(core: &ControlCore, view: &ValueView) -> LampState {
    let Some(def) = value_def(view.id) else {
        return LampState::Unknown;
    };
    if view.freshness != Freshness::Fresh || view.text == DASH {
        return LampState::Unknown;
    }
    let state = core
        .state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state
        .read(&def.path, None)
        .value
        .map_or(LampState::Unknown, |v| {
            LampState::of_annunciator(&js::to_display_string(v))
        })
}

pub fn build_values(core: &ControlCore) -> ValuesModel {
    build_values_with(core, |_| false)
}

/// As [`build_values`], with `greyed` telling which toggles the ship lacks: a click on one of
/// them sends the game's own toggle, never set-to-state, since the state the game would confirm
/// is not reported.
pub fn build_values_with(core: &ControlCore, greyed: impl Fn(&str) -> bool) -> ValuesModel {
    let values = VALUES
        .iter()
        .filter_map(|def| core.value(&def.id))
        .map(|view| {
            let lamp = if view.id.starts_with("caution.") {
                caution_lamp(core, &view)
            } else {
                LampState::of_on(view.on)
            };
            let tone = tone_of_text(&view.text, view.fraction, view.on);
            (view.id, ValueEntry { view, tone, lamp })
        })
        .collect();
    let toggles = TOGGLES
        .iter()
        .map(|t| {
            let view = core.view_toggle(t.id);
            let tile = tile(t.id, t.label, t.toggle, view.as_ref());
            let entry = ToggleEntry {
                lamp: LampState::of_on(tile.on),
                moving_to: view.and_then(|v| v.moving_to),
                click: if greyed(t.id) {
                    Click::Press { action: t.toggle }
                } else {
                    Click::toggle(t.id, t.toggle, tile.on)
                },
                tile,
            };
            (t.id, entry)
        })
        .collect();
    let (in_ship, shield_faces) = {
        let state = core
            .state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.in_vehicle(), shield_faces(&state))
    };
    let values = ById(values);
    let toggles = ById(toggles);
    let config = build_config(&values, &toggles);
    ValuesModel {
        link: core.link_status(),
        in_ship,
        running: core.running(),
        values,
        toggles,
        shield_faces,
        weapons: read_weapons(core),
        config,
    }
}
