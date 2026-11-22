//! Port of packages/sc-control-core/tests/engine.test.ts, on tokio's paused clock.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{rig, ship};
use sc_core::engine::{
    ActionResult, CONFIRM_GROUP, FailStatus, Failure, OkStatus, OnOff, ProgressStatus, Sequence,
    Step, TargetMode,
};
use sc_core::state::Freshness;
use sc_game::TriggerStatus;
use serde_json::json;
use tokio::time::Instant;

const DONE: ActionResult = ActionResult::Ok(OkStatus::Done);
const ALREADY: ActionResult = ActionResult::Ok(OkStatus::Already);

fn status(r: &ActionResult) -> Option<FailStatus> {
    match r {
        ActionResult::Ok(_) => None,
        ActionResult::Failed(f) => Some(f.status),
    }
}

fn seq(id: &str, steps: Vec<Step>) -> Sequence {
    Sequence {
        id: id.into(),
        name: id.to_uppercase(),
        built_in: None,
        steps,
    }
}

fn set(toggle: &str, to: OnOff) -> Step {
    Step::Set {
        toggle: toggle.into(),
        to,
        timeout_s: None,
    }
}

fn mode(to: TargetMode) -> Step {
    Step::Mode {
        to,
        timeout_s: None,
    }
}

fn action(a: &str) -> Step {
    Step::Action { action: a.into() }
}

// ---------------------------------------------------------------------- press

#[tokio::test(start_paused = true)]
async fn fires_a_game_action_and_spaces_two_presses() {
    let r = rig(&[]);
    assert_eq!(r.engine.press("v_invoke_ping", false).await, DONE);
    r.engine.press("v_target_hail", false).await;
    assert_eq!(r.game.actions(), ["v_invoke_ping", "v_target_hail"]);
    let sent = r.game.sent.lock().unwrap();
    assert!(sent[1].1 - sent[0].1 >= Duration::from_millis(150));
}

#[tokio::test(start_paused = true)]
async fn refuses_the_confirm_group_without_confirmed_and_allows_it_with() {
    let r = rig(&[]);
    for a in CONFIRM_GROUP {
        assert_eq!(
            status(&r.engine.press(a, false).await),
            Some(FailStatus::NeedsConfirm)
        );
    }
    assert!(r.game.actions().is_empty());
    assert!(r.engine.press("v_eject", true).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn retries_once_on_rate_limited_then_reports_the_game_answer() {
    let a = rig(&[TriggerStatus::RateLimited, TriggerStatus::Ok]);
    let t0 = Instant::now();
    assert_eq!(a.engine.press("v_lights", false).await, DONE);
    assert_eq!(a.game.actions().len(), 2);
    assert!(t0.elapsed() >= Duration::from_millis(400));

    let b = rig(&[TriggerStatus::RateLimited, TriggerStatus::RateLimited]);
    assert_eq!(
        b.engine.press("v_lights", false).await,
        ActionResult::Failed(Failure {
            status: FailStatus::Game,
            message: "The game answered RATE_LIMITED".into(),
            game: Some(TriggerStatus::RateLimited),
        })
    );
    assert_eq!(b.game.actions().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn passes_other_game_refusals_through_without_retry() {
    let r = rig(&[TriggerStatus::MapInactive]);
    let res = r.engine.press("v_lights", false).await;
    assert!(matches!(
        res,
        ActionResult::Failed(Failure {
            status: FailStatus::Game,
            game: Some(TriggerStatus::MapInactive),
            ..
        })
    ));
    assert_eq!(r.game.actions().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn concurrent_presses_go_out_one_at_a_time_in_order() {
    let r = rig(&[]);
    let (a, b, c) = tokio::join!(
        r.engine.press("v_a", false),
        r.engine.press("v_b", false),
        r.engine.press("v_c", false)
    );
    assert_eq!([a, b, c], [DONE, DONE, DONE]);
    assert_eq!(r.game.actions(), ["v_a", "v_b", "v_c"]);
    let sent = r.game.sent.lock().unwrap();
    assert!(sent[2].1 - sent[0].1 >= Duration::from_millis(300));
}

// ---------------------------------------------------------------------- set to state

#[tokio::test(start_paused = true)]
async fn acts_only_when_the_state_differs_using_the_explicit_set_action() {
    let r = rig(&[]);
    r.ingest(&ship(json!({ "lights_enabled": true })));
    assert_eq!(r.engine.set("lights", OnOff::On).await, ALREADY);
    assert!(r.game.actions().is_empty());
    assert_eq!(r.engine.set("lights", OnOff::Off).await, DONE);
    assert_eq!(r.game.actions(), ["v_lights_off"]);
}

#[tokio::test(start_paused = true)]
async fn reads_a_missing_bool_inside_a_present_section_as_false() {
    let r = rig(&[]);
    r.ingest(&ship(json!({})));
    assert_eq!(r.engine.set("lights", OnOff::Off).await, ALREADY);
    assert!(r.game.actions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn treats_a_gear_already_moving_to_the_target_as_done() {
    let r = rig(&[]);
    r.ingest(&ship(json!({ "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYING" } } })));
    assert_eq!(r.engine.set("gear", OnOff::On).await, ALREADY);
    assert_eq!(r.engine.set("gear", OnOff::Off).await, DONE);
    assert_eq!(r.game.actions(), ["v_retract_landing_system"]);
}

#[tokio::test(start_paused = true)]
async fn uses_the_toggle_action_when_the_game_has_no_set_action() {
    let r = rig(&[]);
    r.ingest(&ship(
        json!({ "flight": { "settings": { "ifcs_esp": { "enabled": true } } } }),
    ));
    assert_eq!(r.engine.set("esp", OnOff::On).await, DONE);
    assert_eq!(r.game.actions(), ["v_ifcs_toggle_esp"]);
}

#[tokio::test(start_paused = true)]
async fn does_nothing_on_stale_state_unknown_state_or_outside_a_vehicle() {
    let r = rig(&[]);
    assert_eq!(
        status(&r.engine.set("lights", OnOff::On).await),
        Some(FailStatus::NotInVehicle)
    );
    r.ingest(&ship(json!({ "lights_enabled": false })));
    tokio::time::advance(Duration::from_millis(1500)).await;
    assert_eq!(
        status(&r.engine.set("lights", OnOff::On).await),
        Some(FailStatus::Stale)
    );
    r.ingest(&ship(json!({})));
    assert_eq!(
        status(&r.engine.set("gear", OnOff::On).await),
        Some(FailStatus::Stale)
    );
    assert_eq!(
        status(&r.engine.set("nope", OnOff::On).await),
        Some(FailStatus::Unknown)
    );
    assert!(r.game.actions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn master_mode_acts_only_when_it_differs() {
    let r = rig(&[]);
    r.ingest(&ship(
        json!({ "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" } }),
    ));
    assert_eq!(r.engine.set_mode(TargetMode::Scm).await, ALREADY);
    assert_eq!(r.engine.set_mode(TargetMode::Nav).await, DONE);
    assert_eq!(r.game.actions(), ["v_master_mode_set_nav"]);
}

// ---------------------------------------------------------------------- views

#[tokio::test(start_paused = true)]
async fn shows_on_off_moving_and_unknown() {
    let r = rig(&[]);
    let v = r.engine.view_toggle("gear").unwrap();
    assert_eq!((v.freshness, v.on), (Freshness::Absent, None));
    r.ingest(&ship(
        json!({ "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" } } }),
    ));
    let v = r.engine.view_for_action("v_toggle_landing_system").unwrap();
    assert_eq!((v.id, v.on), ("gear", Some(true)));
    r.ingest(&ship(json!({ "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTING" } } })));
    let v = r.engine.view_toggle("gear").unwrap();
    assert_eq!((v.on, v.moving_to), (None, Some(false)));
    assert!(r.engine.view_for_action("v_invoke_ping").is_none());
}

// ---------------------------------------------------------------------- sequences

#[tokio::test(start_paused = true)]
async fn runs_each_step_after_the_game_confirms_the_last_one() {
    let r = rig(&[]);
    let s = Arc::new(Mutex::new(json!({
        "master_mode": { "mode": "VEHICLE_MASTER_MODE_NAVIGATION" },
        "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED" } },
    })));
    r.ingest(&ship(s.lock().unwrap().clone()));
    // The fake game applies each action to the next frame.
    let (state, shared) = (r.state.clone(), s.clone());
    r.game.on_send(move |a| {
        let mut s = shared.lock().unwrap();
        match a {
            "v_master_mode_set_scm" => {
                s["master_mode"] = json!({ "mode": "VEHICLE_MASTER_MODE_SCM" })
            }
            "v_deploy_landing_system" => {
                s["flight"] =
                    json!({ "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYING" } })
            }
            "v_lights_on" => s["lights_enabled"] = json!(true),
            _ => {}
        }
        state.lock().unwrap().ingest(&ship(s.clone()));
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let _guard = r.engine.on_progress(move |p| {
        log.lock()
            .unwrap()
            .push(format!("{}/{} {:?}", p.step, p.of, p.status))
    });
    let landing = seq(
        "landing_prep",
        vec![
            mode(TargetMode::Scm),
            set("gear", OnOff::On),
            set("lights", OnOff::On),
        ],
    );
    let p = r.engine.run_sequence(&landing).await;
    assert_eq!((p.status, p.step, p.of), (ProgressStatus::Done, 3, 3));
    assert_eq!(
        r.game.actions(),
        [
            "v_master_mode_set_scm",
            "v_deploy_landing_system",
            "v_lights_on"
        ]
    );
    assert_eq!(seen.lock().unwrap().last().unwrap(), "3/3 Done");
    assert!(r.engine.running().is_none());
}

#[tokio::test(start_paused = true)]
async fn fails_at_the_step_the_game_does_not_confirm_and_says_which() {
    let r = rig(&[]);
    r.ingest(&ship(
        json!({ "master_mode": { "mode": "VEHICLE_MASTER_MODE_NAVIGATION" } }),
    ));
    let p = r
        .engine
        .run_sequence(&seq("x", vec![mode(TargetMode::Scm)]))
        .await;
    assert_eq!((p.status, p.step), (ProgressStatus::Failed, 1));
    assert_eq!(
        p.message.as_deref(),
        Some("Master mode did not change to SCM")
    );
}

#[tokio::test(start_paused = true)]
async fn a_press_anywhere_aborts_the_running_sequence_and_does_nothing_else() {
    let r = rig(&[]);
    r.ingest(&ship(
        json!({ "master_mode": { "mode": "VEHICLE_MASTER_MODE_NAVIGATION" } }),
    ));
    let engine = r.engine.clone();
    let run = tokio::spawn(async move {
        engine
            .run_sequence(&seq(
                "x",
                vec![mode(TargetMode::Scm), set("lights", OnOff::On)],
            ))
            .await
    });
    while r.engine.running().is_none() {
        tokio::task::yield_now().await;
    }
    let pressed = r.engine.press("v_invoke_ping", false).await;
    assert_eq!(status(&pressed), Some(FailStatus::AbortedSequence));
    let p = run.await.unwrap();
    assert_eq!(
        (p.status, p.message.as_deref()),
        (ProgressStatus::Aborted, Some("Stopped"))
    );
    assert!(!r.game.actions().contains(&"v_invoke_ping".to_owned()));
    assert!(r.engine.running().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_second_sequence_stops_the_first_and_does_not_start() {
    let r = rig(&[]);
    r.ingest(&ship(json!({})));
    let engine = r.engine.clone();
    let first = tokio::spawn(async move {
        engine
            .run_sequence(&seq("a", vec![Step::Pause { seconds: 10.0 }]))
            .await
    });
    while r.engine.running().is_none() {
        tokio::task::yield_now().await;
    }
    let second = r
        .engine
        .run_sequence(&seq("b", vec![Step::Pause { seconds: 1.0 }]))
        .await;
    assert_eq!(
        serde_json::to_value(&second).unwrap(),
        json!({ "id": "b", "name": "B", "step": 0, "of": 1, "status": "aborted", "message": "Stopped the running sequence" })
    );
    assert_eq!(first.await.unwrap().status, ProgressStatus::Aborted);
    assert!(!r.engine.abort());
}

#[tokio::test(start_paused = true)]
async fn dropping_a_run_clears_it() {
    let r = rig(&[]);
    let engine = r.engine.clone();
    let run = tokio::spawn(async move {
        engine
            .run_sequence(&seq("a", vec![Step::Pause { seconds: 10.0 }]))
            .await
    });
    while r.engine.running().is_none() {
        tokio::task::yield_now().await;
    }
    run.abort();
    let _ = run.await;
    assert!(r.engine.running().is_none());
}

#[tokio::test(start_paused = true)]
async fn refuses_a_confirm_group_action_as_a_step() {
    let r = rig(&[]);
    let p = r
        .engine
        .run_sequence(&seq("x", vec![action("v_power_set_off")]))
        .await;
    assert_eq!(p.status, ProgressStatus::Failed);
    assert_eq!(
        p.message.as_deref(),
        Some("\"v_power_set_off\" cannot run inside a sequence")
    );
    assert!(r.game.actions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn flight_ready_from_cold_waits_for_the_state_to_appear_after_power_on() {
    let r = rig(&[]);
    // The stream keeps sending; the door-lock state only appears once the ship has powered on.
    let s = Arc::new(Mutex::new(ship(json!({}))));
    let feeder = r.feed(s.clone());
    r.ingest(&s.lock().unwrap().clone());
    let shared = s.clone();
    r.game.on_send(move |a| {
        let lock_state = match a {
            "v_flightready" => "DOOR_LOCK_STATE_UNLOCKED",
            "v_lock_all_doors" => "DOOR_LOCK_STATE_LOCKED",
            _ => return,
        };
        *shared.lock().unwrap() = ship(json!({ "doors": { "lock_state": lock_state } }));
    });
    let p = r
        .engine
        .run_sequence(&seq(
            "fr",
            vec![action("v_flightready"), set("door_locks", OnOff::On)],
        ))
        .await;
    feeder.abort();
    assert_eq!(
        (p.status, p.step, p.unconfirmed),
        (ProgressStatus::Done, 2, None)
    );
    assert_eq!(r.game.actions(), ["v_flightready", "v_lock_all_doors"]);
}

#[tokio::test(start_paused = true)]
async fn a_state_the_game_never_reports_sends_the_set_action_unconfirmed() {
    let r = rig(&[]);
    r.ingest(&ship(json!({})));
    let p = r
        .engine
        .run_sequence(&seq("x", vec![set("door_locks", OnOff::On)]))
        .await;
    assert_eq!((p.status, p.unconfirmed), (ProgressStatus::Done, Some(1)));
    assert_eq!(r.game.actions(), ["v_lock_all_doors"]);
}

#[tokio::test(start_paused = true)]
async fn a_never_reported_toggle_with_no_set_action_is_skipped() {
    let r = rig(&[]);
    r.ingest(&json!({ "vehicle": { "state": { "name": "Ship", "flight": { "settings": {} } } } }));
    let p = r
        .engine
        .run_sequence(&seq("x", vec![set("door_locks", OnOff::Off)]))
        .await;
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({ "id": "x", "name": "X", "step": 1, "of": 1, "status": "done", "skipped": 1 })
    );
    assert!(r.game.actions().is_empty());
}

// ---------------------------------------------------------------------- pauses, timeouts, settle

#[tokio::test(start_paused = true)]
async fn a_pause_waits_then_the_next_step_runs() {
    let r = rig(&[]);
    let s = Arc::new(Mutex::new(ship(json!({ "lights_enabled": false }))));
    let feeder = r.feed(s.clone());
    r.ingest(&s.lock().unwrap().clone());
    let t0 = Instant::now();
    let p = r
        .engine
        .run_sequence(&seq(
            "p",
            vec![Step::Pause { seconds: 3.0 }, set("lights", OnOff::Off)],
        ))
        .await;
    feeder.abort();
    assert_eq!(p.status, ProgressStatus::Done);
    assert!(t0.elapsed() >= Duration::from_millis(3000));
    assert!(r.game.actions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_step_timeout_bounds_the_wait() {
    let r = rig(&[]);
    let s = Arc::new(Mutex::new(ship(
        json!({ "master_mode": { "mode": "VEHICLE_MASTER_MODE_NAVIGATION" } }),
    )));
    let feeder = r.feed(s.clone());
    r.ingest(&s.lock().unwrap().clone());
    let t0 = Instant::now();
    let p = r
        .engine
        .run_sequence(&seq(
            "m",
            vec![Step::Mode {
                to: TargetMode::Scm,
                timeout_s: Some(1.0),
            }],
        ))
        .await;
    feeder.abort();
    assert_eq!(p.status, ProgressStatus::Failed);
    assert!(t0.elapsed() >= Duration::from_millis(1000));
    assert!(t0.elapsed() < Duration::from_millis(1500));
}

#[tokio::test(start_paused = true)]
async fn settle_pauses_after_action_steps_but_not_the_last() {
    let game = Arc::new(common::TestGame::new(&[]));
    let state = common::paused_state();
    let engine = sc_core::engine::Engine::new(
        game.clone(),
        state,
        sc_core::engine::EngineOptions {
            settle: Duration::from_millis(500),
            spacing: Duration::ZERO,
            ..Default::default()
        },
    );
    let t0 = Instant::now();
    let p = engine
        .run_sequence(&seq("s", vec![action("v_a"), action("v_b")]))
        .await;
    assert_eq!(p.status, ProgressStatus::Done);
    let sent = game.sent.lock().unwrap();
    assert_eq!(sent[1].1 - sent[0].1, Duration::from_millis(500));
    assert_eq!(t0.elapsed(), Duration::from_millis(500));
}
