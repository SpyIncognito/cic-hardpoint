use sc_game::scrub_frame;
use serde_json::json;

#[test]
fn zeroes_the_balance_and_makes_ship_seat_weapon_and_preset_names_generic() {
    let frame = json!({
        "actor": { "state": { "uec_balance": "1234567", "seat_name": "Pilot" } },
        "vehicle": {
            "state": { "name": "My Ship", "class_name": "ANVL_Arrow", "manufacturer": "Anvil", "health": 1 },
        },
        "weapons": { "preset_name": "Alpha strike", "weapons": [{ "name": "Gun A", "is_powered": true }, { "ready_to_fire": true }] },
    });
    assert_eq!(
        scrub_frame(&frame),
        json!({
            "actor": { "state": { "uec_balance": "0", "seat_name": "Seat" } },
            "vehicle": {
                "state": { "name": "Ship", "class_name": "SHIP_CLASS", "manufacturer": "Manufacturer", "health": 1 },
            },
            "weapons": { "preset_name": "Preset", "weapons": [{ "name": "Weapon 1", "is_powered": true }, { "ready_to_fire": true }] },
        })
    );
    assert_eq!(frame["actor"]["state"]["uec_balance"], "1234567");
}

#[test]
fn leaves_absent_fields_absent() {
    let frame = json!({ "vehicle": { "state": { "health": 1 } } });
    assert_eq!(scrub_frame(&frame), frame);
    assert_eq!(scrub_frame(&json!({})), json!({}));
}
