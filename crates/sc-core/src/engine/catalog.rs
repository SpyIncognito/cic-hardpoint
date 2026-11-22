//! What the core knows about the game's actions: which ones flip a state it can read (so a key
//! can show the real state and "set" can act only when needed), and which ones need a confirm.
//! Action names are the game's own ("v_toggle_landing_system"), the same names the keybinding
//! catalog uses after the action map prefix.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::js;
use crate::state::{Freshness, StateModel};

/// Reads a state from the value at a toggle's path (`None` when the path is missing).
pub type StateFn = fn(Option<&Value>) -> Option<bool>;

#[derive(Debug, Clone)]
pub struct ToggleDef {
    pub id: &'static str,
    pub label: &'static str,
    /// Short text for a key.
    pub short: &'static str,
    /// The game action that flips it.
    pub toggle: &'static str,
    /// Explicit set actions, where the game has them.
    pub on: Option<&'static str>,
    pub off: Option<&'static str>,
    /// Where the state is in the frame, and its proto3 default inside a present section.
    pub path: &'static str,
    pub fallback: Option<Value>,
    /// True on, false off, `None` when the value does not say.
    pub is_on: StateFn,
    /// The state is moving towards on (true) or off (false); `None` when settled.
    pub moving_to: Option<StateFn>,
}

impl ToggleDef {
    /// Every action that changes this state: the toggle, then the set actions the game has.
    pub fn actions(&self) -> impl Iterator<Item = &'static str> {
        [Some(self.toggle), self.on, self.off].into_iter().flatten()
    }
}

const DEPLOYED: &str = "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED";
const RETRACTED: &str = "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED";
const DEPLOYING: &str = "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYING";
const RETRACTING: &str = "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTING";

fn either(v: Option<&Value>, on: &str, off: &str) -> Option<bool> {
    match v?.as_str()? {
        s if s == on => Some(true),
        s if s == off => Some(false),
        _ => None,
    }
}

fn deployed(v: Option<&Value>) -> Option<bool> {
    either(v, DEPLOYED, RETRACTED)
}

fn deploying(v: Option<&Value>) -> Option<bool> {
    either(v, DEPLOYING, RETRACTING)
}

fn flag(v: Option<&Value>) -> Option<bool> {
    v?.as_bool()
}

/// An `Availability` message's `active`, false when the message is there without it.
fn active(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Object(fields) => Some(fields.get("active").is_some_and(js::truthy)),
        Value::Array(_) => Some(false),
        _ => None,
    }
}

fn decoupled(v: Option<&Value>) -> Option<bool> {
    active(v).map(|coupled| !coupled)
}

fn locked(v: Option<&Value>) -> Option<bool> {
    either(v, "DOOR_LOCK_STATE_LOCKED", "DOOR_LOCK_STATE_UNLOCKED")
}

struct Spec {
    id: &'static str,
    label: &'static str,
    short: &'static str,
    toggle: &'static str,
    on: Option<&'static str>,
    off: Option<&'static str>,
    path: &'static str,
}

impl Spec {
    fn build(
        self,
        fallback: Option<Value>,
        is_on: StateFn,
        moving_to: Option<StateFn>,
    ) -> ToggleDef {
        let Spec {
            id,
            label,
            short,
            toggle,
            on,
            off,
            path,
        } = self;
        ToggleDef {
            id,
            label,
            short,
            toggle,
            on,
            off,
            path,
            fallback,
            is_on,
            moving_to,
        }
    }
}

fn spec(
    id: &'static str,
    label: &'static str,
    short: &'static str,
    toggle: &'static str,
    on_off: Option<(&'static str, &'static str)>,
    path: &'static str,
) -> Spec {
    Spec {
        id,
        label,
        short,
        toggle,
        on: on_off.map(|(on, _)| on),
        off: on_off.map(|(_, off)| off),
        path,
    }
}

fn build_toggles() -> Vec<ToggleDef> {
    let no = || Some(Value::Bool(false));
    let empty = || Some(Value::Object(Map::new()));
    vec![
        spec(
            "gear",
            "Landing gear",
            "GEAR",
            "v_toggle_landing_system",
            Some(("v_deploy_landing_system", "v_retract_landing_system")),
            "vehicle.state.flight.state.gear",
        )
        .build(None, deployed, Some(deploying)),
        spec(
            "vtol",
            "VTOL",
            "VTOL",
            "v_vtol_toggle",
            Some(("v_vtol_on", "v_vtol_off")),
            "vehicle.state.flight.state.vtol",
        )
        .build(None, deployed, Some(deploying)),
        // "Cycle configuration": wings and similar transformations, separate from the gear.
        spec(
            "transform",
            "Configuration (wings)",
            "CONFIG",
            "v_transform_cycle",
            Some(("v_transform_deploy", "v_transform_retract")),
            "vehicle.state.flight.state.transformation",
        )
        .build(None, deployed, Some(deploying)),
        spec(
            "lights",
            "Ship lights",
            "LIGHTS",
            "v_lights",
            Some(("v_lights_on", "v_lights_off")),
            "vehicle.state.lights_enabled",
        )
        .build(no(), flag, None),
        spec(
            "power.shields",
            "Shields power",
            "SHLD",
            "v_power_toggle_shields",
            Some(("v_power_set_shields_on", "v_power_set_shields_off")),
            "vehicle.state.power.shield_generators",
        )
        .build(no(), flag, None),
        spec(
            "power.thrusters",
            "Thrusters power",
            "THR",
            "v_power_toggle_thrusters",
            Some(("v_power_set_thrusters_on", "v_power_set_thrusters_off")),
            "vehicle.state.power.engines",
        )
        .build(no(), flag, None),
        spec(
            "power.weapons",
            "Weapons power",
            "WPN",
            "v_power_toggle_weapons",
            Some(("v_power_set_weapons_on", "v_power_set_weapons_off")),
            "vehicle.state.power.weapons",
        )
        .build(no(), flag, None),
        spec(
            "decoupled",
            "Decoupled",
            "DECPL",
            "v_ifcs_vector_decoupling_toggle",
            Some((
                "v_ifcs_vector_decoupling_on",
                "v_ifcs_vector_decoupling_off",
            )),
            "vehicle.state.flight.settings.ifcs_coupled",
        )
        .build(empty(), decoupled, None),
        spec(
            "gsafe",
            "G-safe",
            "G-SAFE",
            "v_ifcs_toggle_gforce_safety",
            Some(("v_ifcs_gsafe_on", "v_ifcs_gsafe_off")),
            "vehicle.state.flight.settings.ifcs_g_safe",
        )
        .build(empty(), active, None),
        spec(
            "esp",
            "ESP",
            "ESP",
            "v_ifcs_toggle_esp",
            None,
            "vehicle.state.flight.settings.ifcs_esp",
        )
        .build(empty(), active, None),
        spec(
            "proximity",
            "Proximity assist",
            "PROX",
            "v_ifcs_proximity_assist_toggle",
            Some(("v_ifcs_proximity_assist_on", "v_ifcs_proximity_assist_off")),
            "vehicle.state.flight.settings.ifcs_proximity_assist",
        )
        .build(empty(), active, None),
        spec(
            "light_amp",
            "Light amplification",
            "NV",
            "v_light_amplification_toggle",
            Some(("v_light_amplification_on", "v_light_amplification_off")),
            "vehicle.state.flight.settings.flight_light_amplification",
        )
        .build(no(), flag, None),
        Spec {
            on: Some("v_lock_all_doors"),
            ..spec(
                "door_locks",
                "Door locks",
                "LOCKS",
                "v_toggle_all_doorlocks",
                None,
                "vehicle.state.doors.lock_state",
            )
        }
        .build(None, locked, None),
    ]
}

/// Every toggle the core can read, in display order.
pub static TOGGLES: LazyLock<Vec<ToggleDef>> = LazyLock::new(build_toggles);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum MasterMode {
    Nav,
    Scm,
    Stealth,
}

/// Master mode: a mode the core can read and set, not an on/off toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MasterModeDef {
    pub path: &'static str,
    pub cycle: &'static str,
    pub set_nav: &'static str,
    pub set_scm: &'static str,
}

impl MasterModeDef {
    pub fn read(&self, v: Option<&Value>) -> Option<MasterMode> {
        match v?.as_str()? {
            "VEHICLE_MASTER_MODE_NAVIGATION" => Some(MasterMode::Nav),
            "VEHICLE_MASTER_MODE_SCM" => Some(MasterMode::Scm),
            "VEHICLE_MASTER_MODE_STEALTH" => Some(MasterMode::Stealth),
            _ => None,
        }
    }

    /// The action that sets `mode`; the game has none for stealth.
    pub fn set(&self, mode: MasterMode) -> Option<&'static str> {
        match mode {
            MasterMode::Nav => Some(self.set_nav),
            MasterMode::Scm => Some(self.set_scm),
            MasterMode::Stealth => None,
        }
    }
}

pub const MASTER_MODE: MasterModeDef = MasterModeDef {
    path: "vehicle.state.master_mode.mode",
    cycle: "v_master_mode_cycle",
    set_nav: "v_master_mode_set_nav",
    set_scm: "v_master_mode_set_scm",
};

/// Actions that never run from a plain press: they destroy, eject or leave the ship open. A surface
/// may run one only with `confirmed: true`, after its own press-and-hold with a countdown, and never
/// from a dial, a multi-action or a sequence step.
pub const CONFIRM_GROUP: &[&str] = &[
    "v_self_destruct",
    "v_eject",
    "v_emergency_exit",
    "v_power_set_off",
    "v_power_toggle",
    "v_open_all_doors",
    "v_toggle_all_doors",
    "v_unlock_all_doors",
    "v_unlock_all_ports",
    // These toggles unlock when the doors or ports are locked, so they need the same hold.
    "v_toggle_all_doorlocks",
    "v_toggle_all_portlocks",
];

pub fn needs_confirm(action: &str) -> bool {
    CONFIRM_GROUP.contains(&action)
}

static TOGGLE_BY_ID: LazyLock<HashMap<&'static str, &'static ToggleDef>> =
    LazyLock::new(|| TOGGLES.iter().map(|t| (t.id, t)).collect());

static TOGGLE_BY_ACTION: LazyLock<HashMap<&'static str, &'static ToggleDef>> =
    LazyLock::new(|| {
        TOGGLES
            .iter()
            .flat_map(|t| t.actions().map(move |a| (a, t)))
            .collect()
    });

pub fn toggle_by_id(id: &str) -> Option<&'static ToggleDef> {
    TOGGLE_BY_ID.get(id).copied()
}

/// The toggle whose state an action changes, if the core can read that state.
pub fn toggle_for_action(action: &str) -> Option<&'static ToggleDef> {
    TOGGLE_BY_ACTION.get(action).copied()
}

/// "spaceship_movement.v_toggle_landing_system" (keybinding catalog id) to the game's action name.
pub fn game_action_name(action_id: &str) -> &str {
    action_id
        .rsplit_once('.')
        .map_or(action_id, |(_, name)| name)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename = "toggle", rename_all = "camelCase")]
pub struct ToggleView {
    pub id: &'static str,
    pub label: &'static str,
    pub short: &'static str,
    pub freshness: Freshness,
    /// `None` when the state is not known (absent, stale, not in a vehicle).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// Moving towards on (true) or off (false), e.g. gear deploying.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moving_to: Option<bool>,
}

/// The state a key bound to this toggle can show.
pub fn view_toggle(state: &StateModel, id: &str) -> Option<ToggleView> {
    let t = toggle_by_id(id)?;
    let view = |freshness| ToggleView {
        id: t.id,
        label: t.label,
        short: t.short,
        freshness,
        on: None,
        moving_to: None,
    };
    if !state.in_vehicle() {
        return Some(view(Freshness::Absent));
    }
    let r = state.read(t.path, t.fallback.as_ref());
    if r.freshness != Freshness::Fresh {
        return Some(view(r.freshness));
    }
    Some(ToggleView {
        on: (t.is_on)(r.value),
        moving_to: t.moving_to.and_then(|f| f(r.value)),
        ..view(r.freshness)
    })
}

/// The state a key bound to this game action can show, if the core can read it.
pub fn view_for_action(state: &StateModel, action: &str) -> Option<ToggleView> {
    view_toggle(state, toggle_for_action(action)?.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    fn ship(state: Value) -> Value {
        let mut s = json!({ "name": "Test ship" });
        s.as_object_mut()
            .unwrap()
            .extend(state.as_object().unwrap().clone());
        json!({ "vehicle": { "state": s } })
    }

    #[test]
    fn shows_on_off_moving_and_unknown() {
        let mut state = StateModel::default();
        let gear = view_toggle(&state, "gear").unwrap();
        assert_eq!((gear.freshness, gear.on), (Freshness::Absent, None));

        state.ingest(&ship(
            json!({ "flight": { "state": { "gear": DEPLOYED } }, "flight_extra": 1 }),
        ));
        let v = view_for_action(&state, "v_toggle_landing_system").unwrap();
        assert_eq!((v.id, v.on), ("gear", Some(true)));

        state.ingest(&ship(
            json!({ "flight": { "state": { "gear": RETRACTING } } }),
        ));
        let gear = view_toggle(&state, "gear").unwrap();
        assert_eq!((gear.on, gear.moving_to), (None, Some(false)));
        assert!(view_for_action(&state, "v_invoke_ping").is_none());
    }

    #[test]
    fn maps_catalog_ids_to_game_action_names_and_toggles() {
        assert_eq!(
            game_action_name("spaceship_movement.v_toggle_landing_system"),
            "v_toggle_landing_system"
        );
        assert_eq!(game_action_name("v_lights"), "v_lights");
        assert_eq!(
            toggle_for_action("v_power_set_shields_on").unwrap().id,
            "power.shields"
        );
    }

    #[test]
    fn fallbacks_read_as_off_inside_a_present_section() {
        let mut state = StateModel::default();
        state.ingest(&ship(json!({})));
        for (id, on) in [
            ("lights", false),
            ("gsafe", false),
            ("decoupled", true),
            ("esp", false),
            ("light_amp", false),
        ] {
            assert_eq!(view_toggle(&state, id).unwrap().on, Some(on), "{id}");
        }
        assert_eq!(view_toggle(&state, "gear").unwrap().on, None);
        assert_eq!(view_toggle(&state, "door_locks").unwrap().on, None);

        state.ingest(&ship(json!({
            "flight": { "settings": { "ifcs_coupled": { "enabled": true, "active": true }, "ifcs_g_safe": { "active": true } } },
            "doors": { "lock_state": "DOOR_LOCK_STATE_LOCKED" },
        })));
        assert_eq!(view_toggle(&state, "decoupled").unwrap().on, Some(false));
        assert_eq!(view_toggle(&state, "gsafe").unwrap().on, Some(true));
        assert_eq!(view_toggle(&state, "door_locks").unwrap().on, Some(true));
    }

    #[test]
    fn toggle_view_serializes_as_the_ts_shape() {
        let mut state = StateModel::default();
        state.ingest(&ship(
            json!({ "flight": { "state": { "gear": DEPLOYING } } }),
        ));
        assert_eq!(
            serde_json::to_value(view_toggle(&state, "gear").unwrap()).unwrap(),
            json!({ "kind": "toggle", "id": "gear", "label": "Landing gear", "short": "GEAR", "freshness": "fresh", "movingTo": true }),
        );
    }

    #[test]
    fn catalog_is_consistent() {
        let ids: HashSet<_> = TOGGLES.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), TOGGLES.len());
        for t in TOGGLES.iter() {
            for a in t.actions() {
                assert_eq!(toggle_for_action(a).unwrap().id, t.id);
                // The door-lock toggle unlocks when locked, so it alone needs the hold.
                assert_eq!(
                    needs_confirm(a),
                    a == "v_toggle_all_doorlocks",
                    "{a}: only the door-lock toggle may be a toggle action in the confirm group"
                );
            }
        }
        assert!(needs_confirm("v_eject"));
        assert!(!needs_confirm("v_lights"));
        assert_eq!(
            MASTER_MODE.read(Some(&json!("VEHICLE_MASTER_MODE_SCM"))),
            Some(MasterMode::Scm)
        );
        assert_eq!(MASTER_MODE.read(None), None);
        assert_eq!(
            MASTER_MODE.set(MasterMode::Nav),
            Some("v_master_mode_set_nav")
        );
        assert_eq!(MASTER_MODE.set(MasterMode::Stealth), None);
    }
}
