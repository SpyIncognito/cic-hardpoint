use serde_json::Value as Json;

use crate::types::Frame;

/// A copy of the frame without what identifies the player: the aUEC balance becomes 0, and
/// ship, seat, weapon and preset names become generic. Fields absent in the frame stay absent.
pub fn scrub_frame(frame: &Frame) -> Frame {
    let mut scrubbed = frame.clone();

    if let Some(actor_state) = section(&mut scrubbed, &["actor", "state"]) {
        replace(actor_state, "uec_balance", "0");
        replace(actor_state, "seat_name", "Seat");
    }

    if let Some(vehicle_state) = section(&mut scrubbed, &["vehicle", "state"]) {
        replace(vehicle_state, "name", "Ship");
        replace(vehicle_state, "class_name", "SHIP_CLASS");
        replace(vehicle_state, "manufacturer", "Manufacturer");
    }

    if let Some(weapons_frame) = section(&mut scrubbed, &["weapons"]) {
        replace(weapons_frame, "preset_name", "Preset");
        if let Some(Json::Array(weapons)) = weapons_frame.get_mut("weapons") {
            for (index, weapon) in weapons.iter_mut().enumerate() {
                if weapon.is_object() {
                    replace(weapon, "name", &format!("Weapon {}", index + 1));
                }
            }
        }
    }

    scrubbed
}

/// The object at `path`, if every step is an object.
fn section<'a>(value: &'a mut Json, path: &[&str]) -> Option<&'a mut Json> {
    let mut current = value.is_object().then_some(value)?;
    for key in path {
        current = current.get_mut(*key).filter(|child| child.is_object())?;
    }
    Some(current)
}

fn replace(object: &mut Json, key: &str, with: &str) {
    if let Some(value) = object.get_mut(key) {
        *value = with.into();
    }
}
