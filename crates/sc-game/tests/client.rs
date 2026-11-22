mod fake_server;

use std::time::Duration;

use fake_server::{
    AfterFrames, FakeServer, FakeServerOptions, closed_address, respond_with, start_fake_server,
};
use futures_util::StreamExt;
use sc_game::{
    Frame, GameAction, GameApi, GameClient, GameClientOptions, StreamError, TriggerResult,
    TriggerStatus,
};
use serde_json::{Value as Json, json};
use std::sync::Arc;

async fn setup(options: FakeServerOptions) -> (FakeServer, GameClient) {
    let server = start_fake_server(options).await;
    let client = client_for(&server.address);
    (server, client)
}

fn client_for(address: &str) -> GameClient {
    GameClient::new(GameClientOptions {
        address: address.to_owned(),
        ..Default::default()
    })
    .unwrap()
}

async fn collect(api: &dyn GameApi) -> (Vec<Frame>, Option<StreamError>) {
    let mut stream = api.open_stream();
    let mut frames = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(frame) => frames.push(frame),
            Err(error) => return (frames, Some(error)),
        }
    }
    (frames, None)
}

async fn eventually(condition: impl Fn() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition not met within 1 s");
}

fn full_frame() -> Json {
    json!({
        "vehicle": {
            "state": {
                "name": "Test Ship",
                "type": "VEHICLE_TYPE_SPACESHIP",
                "lights_enabled": true,
                "flight": {
                    "state": { "is_landed": true, "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" },
                },
                "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM", "spooled": true },
                "shields": { "level": 0.5, "faces": [1, 0.25] },
            },
        },
        "actor": { "state": { "uec_balance": "9007199254740993", "locomotion": "ACTOR_LOCOMOTION_STATE_SEATED" } },
    })
}

fn frames_options(frames: Vec<Json>) -> FakeServerOptions {
    FakeServerOptions {
        frames,
        ..Default::default()
    }
}

#[tokio::test]
async fn stream_decodes_frames_as_json_enum_names_int64_as_string_absent_fields_absent() {
    let (_server, client) = setup(frames_options(vec![full_frame()])).await;
    let (frames, error) = collect(&client).await;
    assert_eq!(error, None);
    assert_eq!(frames, vec![full_frame()]);

    let state = frames[0]["vehicle"]["state"].as_object().unwrap();
    for absent in ["class_name", "health", "doors", "_flight"] {
        assert!(!state.contains_key(absent), "{absent} should be absent");
    }
    assert!(frames[0].get("weapons").is_none());
    assert!(frames[0]["actor"]["state"]["uec_balance"].is_string());
}

#[tokio::test]
async fn stream_passes_partial_frames_through_with_only_their_sections() {
    let partial = vec![
        json!({ "actor": { "vitals": { "heart_rate": 72 } } }),
        json!({ "weapons": { "preset_index": 2, "weapons": [{ "name": "Gun", "ready_to_fire": true }] } }),
        json!({ "events": { "events": [{ "type": "GAME_EVENT_TYPE_ACTOR_SPAWN" }] } }),
    ];
    let (_server, client) = setup(frames_options(partial.clone())).await;
    let (frames, _) = collect(&client).await;
    assert_eq!(frames, partial);
    let keys: Vec<&String> = frames[0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["actor"]);
}

#[tokio::test]
async fn stream_keeps_an_explicitly_set_proto3_optional_zero_enum() {
    let frame = json!({
        "vehicle": { "state": { "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_UNSPECIFIED" } } } },
    });
    let (_server, client) = setup(frames_options(vec![frame.clone()])).await;
    let (frames, _) = collect(&client).await;
    assert_eq!(frames, vec![frame]);
}

#[tokio::test]
async fn stream_keeps_an_explicitly_set_proto3_optional_empty_gauge() {
    let frame =
        json!({ "vehicle": { "state": { "flight": { "instruments": { "g_meter": {} } } } } });
    let (_server, client) = setup(frames_options(vec![frame.clone()])).await;
    let (frames, _) = collect(&client).await;
    assert_eq!(frames, vec![frame]);
}

#[tokio::test]
async fn stream_reports_a_failed_stream_with_an_error() {
    let (_server, client) = setup(FakeServerOptions {
        frames: vec![full_frame()],
        after_frames: AfterFrames::Error,
        ..Default::default()
    })
    .await;
    let (frames, error) = collect(&client).await;
    let error = error.expect("the stream fails");
    assert!(error.message.contains("scripted failure"), "{error}");
    assert_eq!(frames.len(), 1);
}

/// Cancelling is dropping the stream, so it cannot end twice or deliver more frames.
#[tokio::test]
async fn stream_cancels_on_drop() {
    let (server, client) = setup(FakeServerOptions {
        frames: vec![full_frame()],
        after_frames: AfterFrames::Hold,
        ..Default::default()
    })
    .await;
    let mut stream = client.open_stream();
    assert!(matches!(stream.next().await, Some(Ok(_))));
    drop(stream);
    eventually(|| server.cancelled_streams() == 1).await;
}

#[tokio::test]
async fn stream_ends_with_an_error_when_the_server_is_unreachable() {
    let client = client_for(&closed_address());
    let (_, error) = collect(&client).await;
    assert!(error.is_some());
}

#[tokio::test]
async fn lists_actions() {
    let (_server, client) = setup(FakeServerOptions {
        actions: vec![
            json!({ "action_name": "v_toggle_landing_system", "category": "spaceship_movement", "holdable": false }),
            json!({ "action_name": "v_weapon_cycle", "category": "spaceship_weapons", "holdable": true }),
            json!({ "action_name": "bare" }),
        ],
        ..Default::default()
    })
    .await;
    let action = |name: &str, category: &str, holdable| GameAction {
        name: name.to_owned(),
        category: category.to_owned(),
        holdable,
    };
    assert_eq!(
        client.list_actions().await,
        vec![
            action("v_toggle_landing_system", "spaceship_movement", false),
            action("v_weapon_cycle", "spaceship_weapons", true),
            action("bare", "", false),
        ]
    );
}

#[tokio::test]
async fn empty_list_and_unreachable_when_nothing_listens() {
    let client = client_for(&closed_address());
    assert_eq!(client.list_actions().await, vec![]);
    let result = client.trigger_action("v_toggle_landing_system", None).await;
    assert_eq!(result.status, TriggerStatus::Unreachable);
    assert_ne!(result.message, "");
}

#[tokio::test]
async fn maps_each_wire_status() {
    let statuses = [
        ("TRIGGER_GAME_ACTION_STATUS_OK", TriggerStatus::Ok),
        (
            "TRIGGER_GAME_ACTION_STATUS_UNKNOWN_ACTION",
            TriggerStatus::UnknownAction,
        ),
        (
            "TRIGGER_GAME_ACTION_STATUS_MAP_INACTIVE",
            TriggerStatus::MapInactive,
        ),
        (
            "TRIGGER_GAME_ACTION_STATUS_RATE_LIMITED",
            TriggerStatus::RateLimited,
        ),
        ("TRIGGER_GAME_ACTION_STATUS_FAILED", TriggerStatus::Failed),
        (
            "TRIGGER_GAME_ACTION_STATUS_HOLD_NOT_ALLOWED",
            TriggerStatus::HoldNotAllowed,
        ),
        (
            "TRIGGER_GAME_ACTION_STATUS_FILTERED",
            TriggerStatus::Filtered,
        ),
        (
            "TRIGGER_GAME_ACTION_STATUS_UNSPECIFIED",
            TriggerStatus::Unspecified,
        ),
    ];
    for (wire, expected) in statuses {
        let message = format!("said {wire}");
        let (_server, client) = setup(FakeServerOptions {
            trigger: Some(respond_with(json!({ "status": wire, "message": message }))),
            ..Default::default()
        })
        .await;
        assert_eq!(
            client.trigger_action("x", None).await,
            TriggerResult::new(expected, message)
        );
    }
}

#[tokio::test]
async fn sends_the_action_name_and_hold_ms_only_when_given() {
    let (server, client) = setup(FakeServerOptions::default()).await;
    client.trigger_action("v_toggle_landing_system", None).await;
    client.trigger_action("v_weapon_cycle", Some(250)).await;
    assert_eq!(
        server.trigger_calls(),
        vec![
            json!({ "action_name": "v_toggle_landing_system" }),
            json!({ "action_name": "v_weapon_cycle", "hold_ms": 250 }),
        ]
    );
}

#[tokio::test]
async fn sends_hold_ms_zero_when_given() {
    let (server, client) = setup(FakeServerOptions::default()).await;
    client.trigger_action("v_weapon_cycle", Some(0)).await;
    assert_eq!(
        server.trigger_calls(),
        vec![json!({ "action_name": "v_weapon_cycle", "hold_ms": 0 })]
    );
}

#[tokio::test]
async fn unreachable_when_the_game_does_not_answer_in_time() {
    let server = start_fake_server(FakeServerOptions {
        trigger: Some(Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                json!({ "status": "TRIGGER_GAME_ACTION_STATUS_OK" })
            })
        })),
        ..Default::default()
    })
    .await;
    let client = GameClient::new(GameClientOptions {
        address: server.address.clone(),
        trigger_timeout: Duration::from_millis(20),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        client.trigger_action("slow", None).await.status,
        TriggerStatus::Unreachable
    );
}

#[tokio::test]
async fn closed_client_answers_unreachable() {
    let (_server, client) = setup(FakeServerOptions::default()).await;
    client.close();
    assert_eq!(
        client.trigger_action("x", None).await.status,
        TriggerStatus::Unreachable
    );
    assert_eq!(client.list_actions().await, vec![]);
}
