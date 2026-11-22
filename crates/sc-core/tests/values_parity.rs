//! Parity with the TypeScript core: tables and display output for hand-written frames.
//! The expected file is frozen; its generator (gen-values-parity.mjs, run on the TS core) lives in
//! the Stream Deck plugin repo, which still has the TS core.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use sc_core::engine::{TOGGLES, ToggleDef, view_toggle};
use sc_core::state::{StateModel, StateModelOptions, VALUES, ValueDef, read_value};
use serde_json::{Map, Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/values-parity.json")).expect("fixture is JSON")
}

/// Integers and floats compare as numbers, as they do in JS (`1` and `1.0` are the same value).
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

/// A [`ValueDef`] in the TS object's shape: optional fields only when set.
fn value_def_json(d: &ValueDef) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(d.id));
    o.insert("label".into(), json!(d.label));
    o.insert("short".into(), json!(d.short));
    o.insert("kind".into(), json!(d.kind));
    o.insert("path".into(), json!(d.path));
    let mut opt = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            o.insert(key.into(), value);
        }
    };
    opt("gauge", d.gauge.then_some(json!(true)));
    opt("fallback", d.fallback.clone());
    opt(
        "enumText",
        d.enum_text
            .map(|t| t.iter().map(|(k, v)| ((*k).to_owned(), json!(v))).collect()),
    );
    opt("vehicle", d.vehicle.then_some(json!(true)));
    opt("unit", d.unit.map(|u| json!(u)));
    opt(
        "fractionAsPercent",
        d.fraction_as_percent.then_some(json!(true)),
    );
    opt("sentinelText", d.sentinel_text.map(|t| json!(t)));
    opt("words", d.words.map(|(yes, no)| json!([yes, no])));
    opt("integer", d.integer.then_some(json!(true)));
    opt("derive", d.derive.map(|x| json!(x)));
    Value::Object(o)
}

fn toggle_def_json(t: &ToggleDef) -> Value {
    let mut o = json!({ "id": t.id, "label": t.label, "short": t.short, "toggle": t.toggle, "path": t.path, "hasMovingTo": t.moving_to.is_some() });
    let fields = o.as_object_mut().unwrap();
    for (key, value) in [
        ("on", t.on.map(|a| json!(a))),
        ("off", t.off.map(|a| json!(a))),
        ("fallback", t.fallback.clone()),
    ] {
        if let Some(value) = value {
            fields.insert(key.into(), value);
        }
    }
    o
}

#[test]
fn values_table_matches_ts() {
    let expected = fixture()["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(normalize)
        .collect::<Vec<_>>();
    let actual = VALUES
        .iter()
        .map(|d| normalize(&value_def_json(d)))
        .collect::<Vec<_>>();
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(&expected) {
        assert_eq!(a, e);
    }
}

#[test]
fn toggles_table_matches_ts() {
    let expected = fixture()["toggles"]
        .as_array()
        .unwrap()
        .iter()
        .map(normalize)
        .collect::<Vec<_>>();
    let actual = TOGGLES
        .iter()
        .map(|t| normalize(&toggle_def_json(t)))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn views_match_ts_for_every_id_in_every_scenario() {
    let fixture = fixture();
    let stale_ms = fixture["staleMs"].as_i64().unwrap();
    let scenarios = fixture["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 10);

    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let now = Arc::new(AtomicI64::new(0));
        let clock = Arc::clone(&now);
        let mut state = StateModel::new(StateModelOptions {
            stale_ms,
            now: Arc::new(move || clock.load(Ordering::SeqCst)),
        });
        for step in scenario["steps"].as_array().unwrap() {
            now.store(step["at"].as_i64().unwrap(), Ordering::SeqCst);
            state.ingest(&step["frame"]);
        }
        now.store(scenario["readAt"].as_i64().unwrap(), Ordering::SeqCst);

        let values = scenario["values"].as_object().unwrap();
        assert_eq!(values.len(), VALUES.len(), "{name}");
        for def in VALUES.iter() {
            let actual = normalize(&serde_json::to_value(read_value(&state, &def.id)).unwrap());
            assert_eq!(actual, normalize(&values[&def.id]), "{name}: {}", def.id);
        }

        let toggles = scenario["toggles"].as_object().unwrap();
        assert_eq!(toggles.len(), TOGGLES.len(), "{name}");
        for t in TOGGLES.iter() {
            let actual = serde_json::to_value(view_toggle(&state, t.id)).unwrap();
            assert_eq!(actual, toggles[t.id], "{name}: toggle {}", t.id);
        }
    }
}
