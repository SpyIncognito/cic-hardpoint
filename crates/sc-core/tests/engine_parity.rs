//! Parity with the TypeScript core: sequence limits, defaults, validation reasons, store files
//! and engine results. The expected file is frozen; its generator (gen-engine-parity.mjs, run on
//! the TS core) lives in the Stream Deck plugin repo, which still has the TS core.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{TestGame, rig_with};
use sc_core::engine::{
    DEFAULT_SEQUENCES, MAX_NAME, MAX_PAUSE_S, MAX_STEPS, MAX_TIMEOUT_S, OnOff, Sequence,
    SequenceError, SequenceStore, Step, TargetMode, parse_sequence, validate,
};
use sc_game::{TriggerResult, TriggerStatus};
use serde_json::{Value, json};
use tokio::time::Instant;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/engine-parity.json")).expect("fixture is JSON")
}

/// Integers and floats compare as numbers, as they do in JS.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().expect("finite number")),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), normalize(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn to_json(v: impl serde::Serialize) -> Value {
    normalize(&serde_json::to_value(v).unwrap())
}

fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sc-core-parity-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.join("sequences.json")
}

#[test]
fn limits_and_default_sequences_match() {
    let f = fixture();
    assert_eq!(
        f["limits"],
        json!({ "MAX_NAME": MAX_NAME, "MAX_PAUSE_S": MAX_PAUSE_S as u64, "MAX_STEPS": MAX_STEPS, "MAX_TIMEOUT_S": MAX_TIMEOUT_S as u64 })
    );
    assert_eq!(
        to_json(&*DEFAULT_SEQUENCES),
        normalize(&f["defaultSequences"])
    );
    let parsed: Vec<Sequence> = serde_json::from_value(f["defaultSequences"].clone()).unwrap();
    assert_eq!(parsed, *DEFAULT_SEQUENCES);
}

/// Inputs the TS accepts that the typed model cannot hold (a missing or numeric id or action, a
/// pause given as a string): Rust refuses them as malformed.
const UNTYPED: &[&str] = &["id missing", "id number", "action missing", "pause string"];

#[test]
fn validation_reasons_match() {
    let f = fixture();
    let mut untyped = Vec::new();
    for case in f["validation"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let input = &case["input"];
        let got = parse_sequence(input);
        if case["throws"] == json!(true) {
            assert!(
                matches!(got, Err(SequenceError::Malformed(_))),
                "{name}: {got:?}"
            );
            continue;
        }
        let problems: Vec<String> = serde_json::from_value(case["problems"].clone()).unwrap();
        if problems.is_empty() {
            match got {
                Ok(seq) => assert_eq!(validate(&seq), Vec::<String>::new(), "{name}"),
                Err(SequenceError::Malformed(_)) => untyped.push(name),
                Err(e) => panic!("{name}: {e:?}"),
            }
            continue;
        }
        assert_eq!(got, Err(SequenceError::Invalid(problems.clone())), "{name}");
        // The typed path gives the same reasons wherever the input fits the types exactly.
        let typed = serde_json::from_value::<Sequence>(input.clone()).ok();
        if let Some(seq) = typed.filter(|seq| to_json(seq) == normalize(input)) {
            assert_eq!(validate(&seq), problems, "{name} (typed)");
        }
    }
    assert_eq!(untyped, UNTYPED);
}

#[test]
fn a_defaults_file_from_the_ts_store_loads_and_is_written_back_byte_for_byte() {
    let f = fixture();
    let text = f["storeFiles"]["defaults"].as_str().unwrap();
    let file = temp_file("defaults");
    fs::write(&file, text).unwrap();
    let store = SequenceStore::new(Some(file.clone()));
    assert_eq!(store.list(), *DEFAULT_SEQUENCES);
    assert!(store.reset("shutdown").unwrap());
    assert_eq!(fs::read_to_string(&file).unwrap(), text);
}

fn ts_style_mine() -> Sequence {
    Sequence {
        id: "mine".into(),
        name: "  My checks  ".into(),
        built_in: None,
        steps: vec![
            Step::Set {
                toggle: "lights".into(),
                to: OnOff::On,
                timeout_s: Some(2.5),
            },
            Step::Mode {
                to: TargetMode::Scm,
                timeout_s: None,
            },
            Step::Pause { seconds: 1.5 },
            Step::Action {
                action: "v_invoke_ping".into(),
            },
        ],
    }
}

#[test]
fn a_mixed_file_from_the_ts_store_round_trips_and_the_same_ops_write_the_same_file() {
    let f = fixture();
    let files = &f["storeFiles"];
    let ts_file: Value = serde_json::from_str(files["mixed"].as_str().unwrap()).unwrap();

    let file = temp_file("mixed-load");
    fs::write(&file, files["mixed"].as_str().unwrap()).unwrap();
    let store = SequenceStore::new(Some(file.clone()));
    assert_eq!(to_json(store.list()), normalize(&files["mixedList"]));
    assert!(store.move_step("mine", 0, 0).unwrap());
    let rewritten: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(normalize(&rewritten), normalize(&ts_file));

    let file = temp_file("mixed-ops");
    let store = SequenceStore::new(Some(file.clone()));
    let edited = Sequence {
        name: "Flight ready (mine)".into(),
        built_in: None,
        ..DEFAULT_SEQUENCES[0].clone()
    };
    let bad = Sequence {
        id: "Bad".into(),
        name: String::new(),
        built_in: None,
        steps: vec![],
    };
    let saves = [
        store.save(&ts_style_mine()).unwrap(),
        store.save(&edited).unwrap(),
        store.save(&bad).unwrap(),
    ];
    assert_eq!(to_json(saves), normalize(&files["saves"]));
    store.duplicate("landing_prep").unwrap();
    store.duplicate("landing_prep").unwrap();
    store.move_step("landing_prep-copy", 0, 3).unwrap();
    store.remove("combat_ready").unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(normalize(&written), normalize(&ts_file));
}

fn trigger_result(answer: &Value) -> TriggerResult {
    let status: TriggerStatus = serde_json::from_value(answer["status"].clone()).unwrap();
    TriggerResult::new(status, answer["message"].as_str().unwrap_or(""))
}

async fn run_scenario(s: &Value) {
    let name = s["name"].as_str().unwrap();
    let answers = s["answers"]
        .as_array()
        .map_or(Vec::new(), |a| a.iter().map(trigger_result).collect());
    let t0 = Instant::now();
    let r = rig_with(TestGame::with_results(answers));
    if let Some(on_send) = s["onSend"].as_object().cloned() {
        let state = r.state.clone();
        r.game.on_send(move |action| {
            if let Some(frame) = on_send.get(action) {
                state.lock().unwrap().ingest(frame);
            }
        });
    }
    if !s["frame"].is_null() {
        r.ingest(&s["frame"]);
    }
    let progress = Arc::new(Mutex::new(Vec::new()));
    let log = progress.clone();
    let _guard = r
        .engine
        .on_progress(move |p| log.lock().unwrap().push(to_json(p)));

    let mut results = Vec::new();
    for op in s["ops"].as_array().unwrap() {
        let text = |key: &str| op[key].as_str().unwrap().to_owned();
        let result = match op["op"].as_str().unwrap() {
            "advance" => {
                tokio::time::advance(Duration::from_millis(op["ms"].as_u64().unwrap())).await;
                continue;
            }
            "press" => to_json(
                r.engine
                    .press(&text("action"), op["confirmed"] == json!(true))
                    .await,
            ),
            "set" => {
                let to: OnOff = serde_json::from_value(op["to"].clone()).unwrap();
                to_json(r.engine.set(&text("toggle"), to).await)
            }
            "setMode" => {
                let to: TargetMode = serde_json::from_value(op["to"].clone()).unwrap();
                to_json(r.engine.set_mode(to).await)
            }
            "run" => {
                let seq: Sequence = serde_json::from_value(op["sequence"].clone()).unwrap();
                to_json(r.engine.run_sequence(&seq).await)
            }
            other => panic!("unknown op {other}"),
        };
        results.push(result);
    }

    let ms = |at: Instant| (at - t0).as_millis() as u64;
    let sent: Vec<Value> = r
        .game
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(action, at)| json!({ "action": action, "at": ms(*at) }))
        .collect();
    assert_eq!(
        Value::Array(results),
        normalize(&s["results"]),
        "{name}: results"
    );
    assert_eq!(
        Value::Array(progress.lock().unwrap().clone()),
        normalize(&s["progress"]),
        "{name}: progress"
    );
    assert_eq!(
        normalize(&Value::Array(sent)),
        normalize(&s["sent"]),
        "{name}: sent"
    );
    assert_eq!(
        json!(ms(Instant::now()) as f64),
        normalize(&s["elapsed"]),
        "{name}: elapsed"
    );
}

#[tokio::test(start_paused = true)]
async fn engine_runs_match() {
    let f = fixture();
    let runs = f["engineRuns"].as_array().unwrap();
    assert!(runs.len() >= 10);
    for s in runs {
        run_scenario(s).await;
    }
}
