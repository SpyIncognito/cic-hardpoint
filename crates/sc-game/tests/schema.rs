//! The schema, and the frame JSON pinned against what protobufjs gives with keepCase,
//! enums: String, longs: String, defaults: false, oneofs: false.

use prost::Message;
use prost_reflect::DynamicMessage;
use sc_game::schema;
use serde_json::{Value as Json, json};

#[test]
fn defines_the_three_rpcs() {
    let service = schema::service();
    let mut names: Vec<String> = service.methods().map(|m| m.name().to_owned()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "ListTriggerableActions",
            "TelemetryStream",
            "TriggerGameAction"
        ]
    );
    let stream = service
        .methods()
        .find(|m| m.name() == "TelemetryStream")
        .unwrap();
    assert!(stream.is_server_streaming());
    assert!(!stream.is_client_streaming());
    for method in service.methods() {
        let path = format!("/{}/{}", schema::SERVICE, method.name());
        assert!(
            [
                schema::TELEMETRY_STREAM_PATH,
                schema::TRIGGER_GAME_ACTION_PATH,
                schema::LIST_TRIGGERABLE_ACTIONS_PATH
            ]
            .contains(&path.as_str())
        );
    }
}

/// Encodes the JSON to the wire and decodes it back as the client does.
fn round_trip(message: &str, json: &Json) -> Json {
    let bytes = schema::from_json(schema::message(message), json)
        .unwrap()
        .encode_to_vec();
    decode(message, &bytes)
}

fn decode(message: &str, bytes: &[u8]) -> Json {
    schema::to_json(&DynamicMessage::decode(schema::message(message), bytes).unwrap())
}

#[test]
fn optional_fields_set_to_zero_are_present() {
    let frame = json!({
        "vehicle": { "state": {
            "flight": {
                "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_UNSPECIFIED" },
                "instruments": { "g_meter": {} },
            },
            "doors": {},
        } },
    });
    assert_eq!(round_trip("TelemetryStreamResponse", &frame), frame);
}

#[test]
fn implicit_presence_fields_at_their_default_are_absent() {
    let frame = json!({
        "vehicle": { "state": {
            "name": "",
            "type": "VEHICLE_TYPE_UNSPECIFIED",
            "lights_enabled": false,
            "health": 0,
            "shields": { "level": 0, "faces": [] },
        } },
        "actor": { "state": { "uec_balance": "0" } },
    });
    assert_eq!(
        round_trip("TelemetryStreamResponse", &frame),
        json!({ "vehicle": { "state": { "shields": {} } }, "actor": { "state": {} } })
    );
}

#[test]
fn sub_messages_are_present_when_sent_even_if_empty() {
    let frame = json!({ "actor": {}, "turret": {} });
    assert_eq!(round_trip("TelemetryStreamResponse", &frame), frame);
    assert_eq!(round_trip("TelemetryStreamResponse", &json!({})), json!({}));
}

#[test]
fn int64_is_a_string_and_uint32_a_number() {
    let frame = json!({
        "actor": { "state": { "uec_balance": "-9007199254740993" } },
        "weapons": { "preset_index": 4294967295u32, "countermeasures": [{ "amount": 7 }] },
    });
    assert_eq!(round_trip("TelemetryStreamResponse", &frame), frame);
}

#[test]
fn floats_widen_to_the_f64_a_js_number_holds() {
    let frame =
        json!({ "actor": { "vitals": { "gravity": 0.1, "heart_rate": 72, "pressure": -2.5 } } });
    assert_eq!(
        round_trip("TelemetryStreamResponse", &frame),
        json!({ "actor": { "vitals": { "gravity": 0.10000000149011612, "heart_rate": 72, "pressure": -2.5 } } })
    );
}

#[test]
fn integral_floats_are_integers_as_json_stringify_writes_them() {
    let decoded = round_trip(
        "VehicleShields",
        &json!({ "level": 1, "faces": [1, 0.5, -3] }),
    );
    // In schema field order, as the TS decoder writes the object.
    assert_eq!(decoded.to_string(), r#"{"level":1,"faces":[1,0.5,-3]}"#);
    assert!(decoded["level"].is_i64());
}

#[test]
fn non_finite_floats_are_null() {
    // VehicleShields.level (field 1, fixed32) = NaN, faces (field 2, packed) = [inf, -inf].
    let mut bytes = vec![0x0d];
    bytes.extend(f32::NAN.to_le_bytes());
    bytes.extend([0x12, 8]);
    bytes.extend(f32::INFINITY.to_le_bytes());
    bytes.extend(f32::NEG_INFINITY.to_le_bytes());
    assert_eq!(
        decode("VehicleShields", &bytes),
        json!({ "level": null, "faces": [null, null] })
    );
}

#[test]
fn enum_values_newer_than_the_schema_stay_numbers() {
    // VehicleMasterModeState.mode (field 1, varint) = 9.
    assert_eq!(
        decode("VehicleMasterModeState", &[0x08, 9]),
        json!({ "mode": 9 })
    );
}

#[test]
fn fields_newer_than_the_schema_are_ignored() {
    // Field 99 (varint) = 1, then spooled (field 2) = true.
    assert_eq!(
        decode("VehicleMasterModeState", &[0x98, 0x06, 1, 0x10, 1]),
        json!({ "spooled": true })
    );
}

/// Differs from protobufjs, which keeps a zero the sender wrote explicitly for a field without
/// presence. Standard proto3 encoders never write one, so the game should not either.
#[test]
fn an_explicit_zero_on_the_wire_for_a_field_without_presence_is_absent() {
    // VehicleShields.level (field 1, fixed32) = 0.
    assert_eq!(decode("VehicleShields", &[0x0d, 0, 0, 0, 0]), json!({}));
}
