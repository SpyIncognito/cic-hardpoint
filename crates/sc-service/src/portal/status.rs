//! The portal's ship status: one plain model built from the core's live state, so the page only
//! draws. Every value is the game's own, with a dash when it does not report it. The JSON is the
//! TypeScript status model's, key for key.

use serde::Serialize;
use serde_json::Value;

use sc_core::ControlCore;
use sc_core::engine::{SequenceProgress, TOGGLES, ToggleView};
use sc_core::js;
use sc_core::state::values::DASH;
use sc_core::state::{Freshness, StateModel};
use sc_game::LinkStatus;

// The card values carry tones and readings too, so their definitions are in fleet-proto.
pub use fleet_proto::card::{Reading, Tone};

/// `action`: the game's toggle for it, sent when its state is not known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToggleTile {
    pub id: &'static str,
    pub label: &'static str,
    pub action: &'static str,
    pub state: String,
    pub tone: Tone,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shields {
    pub level: Reading,
    pub faces: Vec<Reading>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusModel {
    pub link: LinkStatus,
    pub in_ship: bool,
    pub ship: String,
    pub flight: Vec<Reading>,
    pub power: Vec<Reading>,
    pub toggles: Vec<ToggleTile>,
    pub master_mode: String,
    pub shields: Shields,
    pub hull: Reading,
    pub fuel: Vec<Reading>,
    pub cautions: Vec<Reading>,
    pub weapons: Vec<Reading>,
    pub vitals: Vec<Reading>,
    pub running: Option<SequenceProgress>,
}

pub fn tone_of_text(text: &str, fraction: Option<f64>, on: Option<bool>) -> Tone {
    match text {
        DASH => Tone::Unknown,
        "WARNING" => Tone::Bad,
        "CAUTION" => Tone::Warn,
        "OK" => Tone::Ok,
        "OFF" => Tone::Off,
        _ => match (on, fraction) {
            (Some(true), _) => Tone::Ok,
            (Some(false), _) => Tone::Off,
            (None, Some(f)) if f >= 0.5 => Tone::Ok,
            (None, Some(f)) if f >= 0.25 => Tone::Warn,
            (None, Some(_)) => Tone::Bad,
            (None, None) => Tone::Info,
        },
    }
}

/// A toggle as a tile: the game's words for its states, MOVING while it moves.
pub fn tile(
    id: &'static str,
    label: &'static str,
    action: &'static str,
    view: Option<&ToggleView>,
) -> ToggleTile {
    let (on_word, off_word) = match id {
        "gear" => ("DOWN", "UP"),
        "door_locks" => ("LOCKED", "UNLOCKED"),
        "transform" => ("DEPLOYED", "RETRACTED"),
        _ => ("ON", "OFF"),
    };
    let word = |on: bool| if on { on_word } else { off_word };
    let make = |state: String, tone, on| ToggleTile {
        id,
        label,
        action,
        state,
        tone,
        on,
    };
    match view {
        Some(ToggleView {
            moving_to: Some(to),
            ..
        }) => make(format!("MOVING {}", word(*to)), Tone::Warn, Some(*to)),
        Some(ToggleView { on: Some(on), .. }) => make(
            word(*on).to_owned(),
            if *on { Tone::Ok } else { Tone::Off },
            Some(*on),
        ),
        Some(ToggleView {
            freshness: Freshness::Stale,
            ..
        }) => make("STALE".to_owned(), Tone::Unknown, None),
        _ => make(DASH.to_owned(), Tone::Unknown, None),
    }
}

fn read(core: &ControlCore, id: &str, label: Option<&str>) -> Reading {
    let v = core.value(id);
    let text = v
        .as_ref()
        .map_or_else(|| DASH.to_owned(), |v| v.text.clone());
    let fraction = v.as_ref().and_then(|v| v.fraction);
    Reading {
        label: label
            .map(str::to_owned)
            .or_else(|| v.as_ref().map(|v| v.label.to_owned()))
            .unwrap_or_else(|| id.to_owned()),
        tone: tone_of_text(&text, fraction, v.as_ref().and_then(|v| v.on)),
        text,
        fraction,
    }
}

fn reads(core: &ControlCore, ids: &[&str]) -> Vec<Reading> {
    ids.iter().map(|id| read(core, id, None)).collect()
}

fn info(label: &str, text: String, known: bool) -> Reading {
    Reading {
        label: label.to_owned(),
        text,
        tone: if known { Tone::Info } else { Tone::Unknown },
        fraction: None,
    }
}

/// Per-face shield levels, each that face's own 0..1 fraction (not its share of the total), shown
/// as a percent. With four faces the game sends front, back, left, right (Corsair, PTU, 22 Nov; left
/// by elimination); other counts are numbered. None out of a ship.
pub fn shield_faces(state: &StateModel) -> Vec<Reading> {
    if !state.in_vehicle() {
        return vec![];
    }
    let Some(Value::Array(faces)) = state.read("vehicle.state.shields.faces", None).value else {
        return vec![];
    };
    const FACE_LABELS: [&str; 4] = ["Front", "Back", "Left", "Right"];
    let named = faces.len() == 4;
    faces
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let n = js::to_number(f);
            let fraction = (0.0..=1.0).contains(&n).then_some(n);
            let text = fraction.map_or_else(
                || js::number_to_string(n),
                |f| format!("{}%", js::number_to_string((f * 100.0).round())),
            );
            Reading {
                label: if named {
                    FACE_LABELS[i].to_string()
                } else {
                    format!("Face {}", i + 1)
                },
                text,
                tone: tone_of_text("x", fraction, None),
                fraction,
            }
        })
        .collect()
}

/// Weapons ready, decoys and noise, from a fresh weapons section only.
fn weapon_counts(state: &StateModel) -> Vec<Reading> {
    let section = state.read("weapons", None);
    let fresh = section
        .value
        .filter(|v| section.freshness == Freshness::Fresh && js::truthy(v));
    let field = |name| {
        fresh
            .and_then(|w| w.get(name))
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    };
    let weapons = field("weapons");
    let ready = weapons
        .iter()
        .filter(|w| w.get("ready_to_fire").is_some_and(js::truthy))
        .count();
    let countermeasure = |kind: &str| match fresh {
        None => DASH.to_owned(),
        Some(_) => field("countermeasures")
            .iter()
            .find(|c| c.get("type").and_then(Value::as_str) == Some(kind))
            .and_then(|c| c.get("amount"))
            .filter(|a| !a.is_null())
            .map_or_else(|| "0".to_owned(), js::to_display_string),
    };
    let known = fresh.is_some();
    vec![
        info(
            "Weapons ready",
            if known {
                format!("{ready}/{}", weapons.len())
            } else {
                DASH.to_owned()
            },
            known,
        ),
        info("Decoys", countermeasure("COUNTERMEASURE_TYPE_DECOY"), known),
        info("Noise", countermeasure("COUNTERMEASURE_TYPE_NOISE"), known),
    ]
}

pub fn build_status(core: &ControlCore) -> StatusModel {
    let (in_ship, faces, counts) = {
        let state = core
            .state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let in_ship = state.in_vehicle();
        (in_ship, shield_faces(&state), weapon_counts(&state))
    };
    let text_of = |id: &str| core.value(id).map(|v| v.text);
    let cautions = [
        "pwr", "wpn", "thr", "shld", "cool", "qdrv", "radr", "prox", "hit", "misl",
    ];
    StatusModel {
        link: core.link_status(),
        in_ship,
        ship: if in_ship {
            text_of("ship.name").unwrap_or_else(|| "Ship".to_owned())
        } else {
            "Not in a ship".to_owned()
        },
        flight: reads(
            core,
            &[
                "ship.speed",
                "ship.altitude",
                "ship.radar_altitude",
                "ship.vertical_speed",
                "ship.heading",
                "ship.boost_reserves",
            ],
        ),
        power: reads(
            core,
            &[
                "power.powerplants",
                "power.coolers",
                "power.life_support",
                "power.gravity_generators",
                "power.sensors",
                "power.shield_emitters",
            ],
        ),
        toggles: TOGGLES
            .iter()
            .map(|t| tile(t.id, t.label, t.toggle, core.view_toggle(t.id).as_ref()))
            .collect(),
        master_mode: text_of("ship.master_mode").unwrap_or_else(|| DASH.to_owned()),
        shields: Shields {
            level: read(core, "ship.shields", None),
            faces,
        },
        hull: read(core, "ship.hull", None),
        fuel: reads(core, &["ship.hydrogen_fuel", "ship.quantum_fuel"]),
        cautions: cautions
            .iter()
            .map(|k| read(core, &format!("caution.{k}"), Some(&k.to_uppercase())))
            .collect(),
        weapons: std::iter::once(read(core, "weapons.preset", None))
            .chain(counts)
            .collect(),
        vitals: reads(
            core,
            &[
                "actor.health",
                "actor.oxygen",
                "actor.hunger",
                "actor.thirst",
            ],
        ),
        running: core.running(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_follow_the_text_then_the_state_then_the_fraction() {
        assert_eq!(tone_of_text(DASH, Some(1.0), None), Tone::Unknown);
        assert_eq!(tone_of_text("WARNING", None, None), Tone::Bad);
        assert_eq!(tone_of_text("CAUTION", None, None), Tone::Warn);
        assert_eq!(tone_of_text("YES", Some(0.1), Some(true)), Tone::Ok);
        assert_eq!(tone_of_text("NO", None, Some(false)), Tone::Off);
        assert_eq!(tone_of_text("50%", Some(0.5), None), Tone::Ok);
        assert_eq!(tone_of_text("25%", Some(0.25), None), Tone::Warn);
        assert_eq!(tone_of_text("10%", Some(0.1), None), Tone::Bad);
        assert_eq!(tone_of_text("Ship", None, None), Tone::Info);
    }

    #[test]
    fn tiles_use_the_games_words() {
        let view = |on, moving_to, freshness| ToggleView {
            id: "gear",
            label: "Gear",
            short: "GEAR",
            freshness,
            on,
            moving_to,
        };
        let gear = |v: ToggleView| tile("gear", "Gear", "v_toggle_landing_system", Some(&v));
        assert_eq!(gear(view(Some(true), None, Freshness::Fresh)).state, "DOWN");
        assert_eq!(
            gear(view(Some(true), Some(false), Freshness::Fresh)).state,
            "MOVING UP"
        );
        assert_eq!(gear(view(None, None, Freshness::Stale)).state, "STALE");
        let absent = tile("lights", "Lights", "v_lights", None);
        assert_eq!(
            (absent.state.as_str(), absent.tone, absent.on),
            (DASH, Tone::Unknown, None)
        );
    }
}
