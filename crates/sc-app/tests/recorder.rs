//! The recorder against a replayed game: what it writes loads with sc-game's parse_recording.
//! The check against the TypeScript replay's parseRecording moved to the Stream Deck plugin repo.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use sc_service::json::to_js_value;

use sc_app::recorder::{self, DEFAULT_CAP, Recorder, StopReason, Summary};
use sc_game::{
    GameAction, GameApi, LinkOptions, RecordedFrame, ReplayGame, ReplayOptions, TelemetryLink,
    parse_recording, scrub_frame,
};

fn frames() -> Vec<RecordedFrame> {
    (0..5)
        .map(|i| RecordedFrame {
            t: f64::from(i) * 20.0,
            frame: json!({
                "actor": { "state": { "uec_balance": "123456", "seat_name": "Pilot seat", "health": 0.1 + f64::from(i) } },
                "vehicle": { "state": { "name": "Corsair", "class_name": "DRAK_Corsair", "manufacturer": "Drake", "speed": 1.0e-7 } },
                "weapons": { "preset_name": "Mine", "weapons": [{ "name": "Gun", "ready_to_fire": true }, "odd"] },
                "shields": { "faces": [{ "level": 1.5 }, { "level": 2.0 }] },
                "seq": i,
            }),
        })
        .collect()
}

fn actions() -> Vec<GameAction> {
    vec![GameAction {
        name: "v_toggle_landing_system".into(),
        category: "spaceship_movement".into(),
        holdable: false,
    }]
}

fn replay() -> (Arc<ReplayGame>, TelemetryLink) {
    let options = ReplayOptions {
        actions: actions(),
        ..ReplayOptions::default()
    };
    let game = Arc::new(ReplayGame::new(frames(), options).unwrap());
    let link = TelemetryLink::new(
        Arc::clone(&game) as Arc<dyn GameApi>,
        LinkOptions::default(),
    );
    (game, link)
}

#[tokio::test]
async fn a_recording_loads_in_the_replay() {
    let dir = tempfile::tempdir().unwrap();
    let folder = recorder::folder(dir.path());
    let (game, link) = replay();
    let actions = game.list_actions().await;
    let rec = Recorder::start(&link, &folder, &actions, DEFAULT_CAP, |s| {
        panic!("stopped by itself: {s:?}")
    })
    .unwrap();
    let path = rec.path().to_owned();
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("telemetry-") && name.ends_with("Z.jsonl"),
        "{name}"
    );
    // The five frames take 80 ms; the link's first reconnect waits a second.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let summary = rec.stop(StopReason::Asked);
    assert_eq!(summary.frames, 5);
    assert_eq!(summary.why, StopReason::Asked);

    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(summary.bytes, text.len() as u64);
    let parsed = parse_recording(&text).unwrap();
    let originals = frames();
    assert_eq!(parsed.len(), 5);
    assert_eq!(parsed[0].t, 0.0);
    for (got, sent) in parsed.iter().zip(&originals) {
        assert_eq!(got.frame, to_js_value(&scrub_frame(&sent.frame)));
    }
    assert_eq!(parsed[0].frame["actor"]["state"]["uec_balance"], "0");
    assert_eq!(parsed[0].frame["weapons"]["weapons"][0]["name"], "Weapon 1");
    assert!(parsed.windows(2).all(|w| w[0].t <= w[1].t));
    let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let keys: Vec<&String> = line.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["t", "frame"]);
    assert!(
        line["t"].is_u64(),
        "t is whole milliseconds, as Date.now() gives"
    );

    let actions_file = folder.join(name.replace(".jsonl", ".actions.json"));
    assert_eq!(
        std::fs::read_to_string(actions_file).unwrap(),
        format!("{}\n", serde_json::to_string_pretty(&actions).unwrap())
    );
}

#[tokio::test]
async fn stops_by_itself_at_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (_game, link) = replay();
    let first_line =
        1 + serde_json::to_string(&json!({ "t": 0, "frame": scrub_frame(&frames()[0].frame) }))
            .unwrap()
            .len() as u64;
    let cap = first_line * 2 + 10;
    let ended: Arc<Mutex<Option<Summary>>> = Arc::default();
    let heard = Arc::clone(&ended);
    let rec = Recorder::start(&link, dir.path(), &[], cap, move |s| {
        *heard.lock().unwrap() = Some(s.clone());
    })
    .unwrap();
    let path = rec.path().to_owned();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ended = ended.lock().unwrap().clone().expect("the cap stops it");
    assert_eq!(ended.why, StopReason::Cap(cap));
    assert_eq!(ended.frames, 2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.len() as u64, ended.bytes);
    assert!(ended.bytes <= cap);
    assert_eq!(parse_recording(&text).unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(path.with_extension("actions.json")).unwrap(),
        "[]\n"
    );
    // Stopping after the cap gives the same summary, with the reason it stopped.
    assert_eq!(rec.stop(StopReason::Asked), ended);
}
