//! Port of packages/sc-control-core/tests/core.test.ts, on tokio's paused clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sc_core::engine::{ActionResult, OkStatus, OnOff, ProgressStatus};
use sc_core::state::Freshness;
use sc_core::{ControlCore, ControlCoreOptions};
use sc_game::{LinkStatus, RecordedFrame, ReplayGame, ReplayOptions};
use serde_json::json;

fn frame(lights: bool) -> serde_json::Value {
    json!({ "vehicle": { "state": { "name": "Ship", "lights_enabled": lights, "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" } } } })
}

#[tokio::test(start_paused = true)]
async fn follows_the_game_shows_state_and_acts_through_the_engine() {
    let frames = (0..40)
        .map(|i| RecordedFrame {
            t: f64::from(i) * 20.0,
            frame: frame(false),
        })
        .collect();
    let game = Arc::new(
        ReplayGame::new(
            frames,
            ReplayOptions {
                loop_playback: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap(),
    );
    let core = ControlCore::new(ControlCoreOptions {
        game: Some(game.clone()),
        ..ControlCoreOptions::default()
    });
    let changes = Arc::new(AtomicUsize::new(0));
    let counter = changes.clone();
    let _changes = core.on_change(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let seen_frames = Arc::new(AtomicUsize::new(0));
    let frame_counter = seen_frames.clone();
    let _frames = core.on_frame(move |_| {
        frame_counter.fetch_add(1, Ordering::SeqCst);
    });
    let follow = core.follow();
    tokio::time::sleep(Duration::from_millis(120)).await;

    assert_eq!(core.link_status(), LinkStatus::Live);
    let v = core.view_for_action("lights_controller.v_lights").unwrap();
    assert_eq!(
        (v.id, v.on, v.freshness),
        ("lights", Some(false), Freshness::Fresh)
    );
    assert_eq!(core.value("ship.master_mode").unwrap().text, "SCM");
    assert_eq!(
        core.set("lights", OnOff::On).await,
        ActionResult::Ok(OkStatus::Done)
    );
    assert_eq!(
        core.press("spaceship_radar.v_invoke_ping", false).await,
        ActionResult::Ok(OkStatus::Done)
    );
    let names: Vec<_> = game.trigger_calls().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["v_lights_on", "v_invoke_ping"]);
    assert!(!core.press("v_self_destruct", false).await.is_ok());
    let missing = core.run_sequence("nope").await;
    assert_eq!(missing.status, ProgressStatus::Failed);
    assert_eq!(missing.message.as_deref(), Some("No sequence \"nope\""));
    assert!(changes.load(Ordering::SeqCst) > 2);
    assert!(seen_frames.load(Ordering::SeqCst) > 2);

    drop(follow);
    assert_eq!(core.link_status(), LinkStatus::Idle);
    core.close();
}

#[tokio::test(start_paused = true)]
async fn change_listeners_can_read_the_state_back() {
    let game = Arc::new(ReplayGame::new(Vec::new(), ReplayOptions::default()).unwrap());
    let core = Arc::new(ControlCore::new(ControlCoreOptions {
        game: Some(game),
        ..ControlCoreOptions::default()
    }));
    let reader = Arc::downgrade(&core);
    let texts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = texts.clone();
    let _guard = core.on_change(move || {
        if let Some(core) = reader.upgrade() {
            log.lock()
                .unwrap()
                .push(core.value("ship.master_mode").unwrap().text);
        }
    });
    core.ingest(&frame(true));
    assert_eq!(*texts.lock().unwrap(), ["SCM"]);
    assert!(core.sequences().get("flight_ready").is_some());
}
