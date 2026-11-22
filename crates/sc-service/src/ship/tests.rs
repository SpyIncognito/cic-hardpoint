use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};

use sc_core::state::{StateModel, StateModelOptions};

use super::support::{
    ABSENT_REASON, Build, FIELDS, Learned, Observation, Rule, Seen, SupportFile, SupportState,
    Tracker, UNAVAILABLE_REASON, build_of_actions, read_support, seen,
};
use super::switcher::{STABLE_MS, Step, Switcher};
use super::{Ships, ship_of};
use crate::dashboard::{DashboardStore, GENERIC_ID, ShipRef};

fn clocked() -> (StateModel, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(0));
    let clock = Arc::clone(&now);
    let model = StateModel::new(StateModelOptions {
        stale_ms: 3000,
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
    });
    (model, now)
}

/// A Corsair as the stream might send it: gear and wings, coupled available, ESP not, no VTOL,
/// no g-meter, no door locks.
fn corsair() -> Value {
    json!({ "vehicle": { "state": {
        "name": "Corsair",
        "class_name": "DRAK_Corsair",
        "manufacturer": "Drake Interplanetary",
        "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" },
        "flight": {
            "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED", "transformation": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" },
            "settings": { "ifcs_coupled": { "enabled": true, "active": true }, "ifcs_esp": {} },
        },
        "annunciators": { "pwr": "ANNUNCIATOR_STATE_NOMINAL" },
        "power": {},
    } } })
}

fn arrow() -> Value {
    json!({ "vehicle": { "state": {
        "class_name": "ANVL_Arrow",
        "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED", "vtol": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED" } },
    } } })
}

fn build() -> Build {
    Build::Known(Some("actions-1".into()))
}

fn state_of(
    support: &[(&'static str, super::support::SupportEntry)],
    id: &str,
) -> Option<SupportState> {
    support.iter().find(|(i, _)| *i == id).map(|(_, e)| e.state)
}

#[test]
fn the_rule_table_greys_only_optional_fields_and_sub_messages() {
    // Checked against the proto: each field is reached through an `optional` field or a
    // sub-message, ends in a message or an enum (never a plain scalar), and an enum's zero is
    // UNSPECIFIED, so absence can never be a real state.
    let proto = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schema/sc_game_telemetry_v1.proto"
    ))
    .unwrap();
    let block = |kind: &str, name: &str| -> Option<String> {
        let start = proto.find(&format!("{kind} {name} {{"))?;
        let body = &proto[start..];
        let mut depth = 0;
        for (i, c) in body.char_indices() {
            match c {
                '{' => depth += 1,
                '}' if depth == 1 => return Some(body[..i].to_owned()),
                '}' => depth -= 1,
                _ => {}
            }
        }
        None
    };
    for f in FIELDS {
        let mut msg = "VehicleState".to_owned();
        let mut guarded = false;
        let segs: Vec<&str> = f
            .path
            .strip_prefix("vehicle.state.")
            .unwrap()
            .split('.')
            .collect();
        for (i, seg) in segs.iter().enumerate() {
            let body = block("message", &msg).unwrap_or_else(|| panic!("message {msg}"));
            let line = body
                .lines()
                .map(str::trim)
                .find(|l| l.split_whitespace().rev().nth(2) == Some(seg))
                .unwrap_or_else(|| panic!("{}: no field {seg} in {msg}", f.id));
            let words: Vec<&str> = line.split_whitespace().collect();
            let optional = words[0] == "optional";
            let ty = if optional { words[1] } else { words[0] };
            let is_msg = block("message", ty).is_some();
            guarded |= optional || is_msg;
            if i + 1 < segs.len() {
                assert!(is_msg, "{}: {seg} is not a message", f.id);
                msg = ty.to_owned();
                continue;
            }
            assert!(
                guarded,
                "{}: not behind an optional field or a sub-message",
                f.id
            );
            assert!(
                !["bool", "float", "double", "string", "int32", "uint32"].contains(&ty),
                "{}: a plain scalar is never greyed by absence",
                f.id
            );
            if !is_msg {
                let e = block("enum", ty).unwrap_or_else(|| panic!("{}: enum {ty}", f.id));
                let zero = e
                    .lines()
                    .map(str::trim)
                    .find(|l| l.ends_with("= 0;"))
                    .unwrap();
                assert!(
                    zero.contains("_UNSPECIFIED"),
                    "{}: zero of {ty} is {zero}",
                    f.id
                );
            }
            if f.rule == Rule::Availability {
                assert_eq!(ty, "Availability", "{}", f.id);
            }
        }
    }
    let gear = FIELDS.iter().find(|f| f.id == "gear").unwrap();
    assert_eq!(seen(gear, None), Seen::Missing);
    assert_eq!(
        seen(
            gear,
            Some(&json!("VEHICLE_COMPONENT_DEPLOYMENT_STATE_UNSPECIFIED"))
        ),
        Seen::Missing
    );
    assert_eq!(seen(gear, Some(&json!(0))), Seen::Missing);
    // Retracted is a real state, never absence.
    assert_eq!(
        seen(
            gear,
            Some(&json!("VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED"))
        ),
        Seen::Arrived
    );
    let g = FIELDS.iter().find(|f| f.id == "g_meter").unwrap();
    assert_eq!(
        seen(g, Some(&json!({}))),
        Seen::Arrived,
        "an empty message is present"
    );
    let esp = FIELDS.iter().find(|f| f.id == "ifcs_esp").unwrap();
    assert_eq!(esp.rule, Rule::Availability);
    assert_eq!(
        seen(esp, Some(&json!({}))),
        Seen::Unavailable,
        "enabled false is left off the wire"
    );
    assert_eq!(
        seen(esp, Some(&json!({ "enabled": false, "active": true }))),
        Seen::Unavailable
    );
    assert_eq!(seen(esp, Some(&json!({ "enabled": true }))), Seen::Arrived);
    assert_eq!(seen(esp, None), Seen::Missing);
    // Every value, toggle and row a field greys exists.
    for f in FIELDS {
        for id in f.greys {
            let known = sc_core::state::value_def(id).is_some()
                || sc_core::engine::toggle_by_id(id).is_some()
                || ["mode", "coupled"].contains(id);
            assert!(
                known,
                "{id} is a value, toggle, the mode widget or a config row"
            );
        }
    }
}

#[test]
fn not_available_greys_at_once_and_absence_only_after_ten_seconds_of_fresh_frames() {
    let (mut s, now) = clocked();
    let mut t = Tracker::default();
    s.ingest(&corsair());
    t.observe(0, &Observation::of(&s), &build());
    let sup = t.support(&build());
    assert_eq!(state_of(&sup, "esp"), Some(SupportState::Unavailable));
    assert_eq!(
        sup.iter().find(|(i, _)| *i == "esp").unwrap().1.reason,
        UNAVAILABLE_REASON
    );
    assert_eq!(state_of(&sup, "vtol"), None, "still learning");
    // Plain values are never in the list.
    assert!(
        sup.iter()
            .all(|(i, _)| !i.starts_with("power.") && *i != "lights")
    );

    for ms in (500..=9_500).step_by(500) {
        now.store(ms, Ordering::SeqCst);
        s.ingest(&corsair());
        t.observe(ms, &Observation::of(&s), &build());
    }
    assert_eq!(
        state_of(&t.support(&build()), "vtol"),
        None,
        "9.5 s is not enough"
    );
    now.store(10_000, Ordering::SeqCst);
    s.ingest(&corsair());
    t.observe(10_000, &Observation::of(&s), &build());
    let sup = t.support(&build());
    assert_eq!(state_of(&sup, "vtol"), Some(SupportState::Absent));
    assert_eq!(
        sup.iter().find(|(i, _)| *i == "vtol").unwrap().1.reason,
        ABSENT_REASON
    );
    assert_eq!(state_of(&sup, "ship.g_force"), Some(SupportState::Absent));
    assert_eq!(state_of(&sup, "door_locks"), Some(SupportState::Absent));
    assert_eq!(state_of(&sup, "caution.wpn"), Some(SupportState::Absent));
    for id in [
        "gear",
        "transform",
        "decoupled",
        "coupled",
        "mode",
        "caution.pwr",
    ] {
        assert_eq!(state_of(&sup, id), None, "{id} arrived");
    }
    assert!(t.take_dirty());
    let learned = &t.learned()[0];
    assert_eq!(learned.class_name, "DRAK_Corsair");
    assert!(learned.absent.contains("vtol") && learned.present.contains("gear"));
}

#[test]
fn stale_frames_do_not_count_toward_the_ten_seconds() {
    let (mut s, now) = clocked();
    let mut t = Tracker::default();
    s.ingest(&corsair());
    t.observe(0, &Observation::of(&s), &build());
    // 20 s with no new frame: the report goes stale after 3 s; the clock stops.
    for ms in (500..=20_000).step_by(500) {
        now.store(ms, Ordering::SeqCst);
        t.observe(ms, &Observation::of(&s), &build());
    }
    assert!(
        t.support(&build()).is_empty(),
        "nothing greyed on stale data"
    );
    assert_eq!(t.ship(), None);
}

#[test]
fn a_learned_absence_greys_from_the_first_second_and_an_arrival_upgrades_at_once() {
    let learned = vec![Learned {
        class_name: "ANVL_Arrow".into(),
        build: Some("actions-1".into()),
        present: ["gear".to_owned()].into(),
        absent: ["vtol".to_owned(), "g_meter".to_owned()].into(),
    }];
    let (mut s, _) = clocked();
    let mut t = Tracker::new(learned);
    s.ingest(&json!({ "vehicle": { "state": { "class_name": "ANVL_Arrow" } } }));
    t.observe(0, &Observation::of(&s), &build());
    let sup = t.support(&build());
    assert_eq!(state_of(&sup, "vtol"), Some(SupportState::Absent));
    assert_eq!(state_of(&sup, "ship.g_force"), Some(SupportState::Absent));
    // VTOL arrives: supported at once, and learned so.
    s.ingest(&arrow());
    t.observe(100, &Observation::of(&s), &build());
    assert_eq!(state_of(&t.support(&build()), "vtol"), None);
    assert!(t.learned()[0].present.contains("vtol"));
    assert!(!t.learned()[0].absent.contains("vtol"));
    assert!(t.take_dirty());
}

#[test]
fn a_learned_absence_expires_when_the_build_changes() {
    let learned = vec![Learned {
        class_name: "ANVL_Arrow".into(),
        build: Some("actions-old".into()),
        present: ["gear".to_owned()].into(),
        absent: ["vtol".to_owned()].into(),
    }];
    let (mut s, _) = clocked();
    let mut t = Tracker::new(learned);
    s.ingest(&json!({ "vehicle": { "state": { "class_name": "ANVL_Arrow" } } }));
    t.observe(0, &Observation::of(&s), &build());
    assert_eq!(
        state_of(&t.support(&build()), "vtol"),
        None,
        "learned under another build"
    );
    let l = &t.learned()[0];
    assert_eq!(l.build.as_deref(), Some("actions-1"));
    assert!(l.absent.is_empty(), "absences expired");
    assert!(l.present.contains("gear"), "what arrived stays known");
    // While the build is not known yet, nothing learned is used or written.
    let mut t2 = Tracker::new(t.learned().to_vec());
    t2.observe(0, &Observation::of(&s), &Build::Pending);
    assert!(!t2.take_dirty());
}

#[test]
fn reset_forgets_one_ship_or_all() {
    let l = |c: &str| Learned {
        class_name: c.into(),
        build: None,
        present: Default::default(),
        absent: ["vtol".to_owned()].into(),
    };
    let mut t = Tracker::new(vec![l("A"), l("B")]);
    assert_eq!(t.reset(Some("A")), 1);
    assert_eq!(t.learned().len(), 1);
    assert_eq!(t.reset(None), 1);
    assert!(t.learned().is_empty());
}

#[test]
fn the_build_key_is_a_fingerprint_of_the_action_list() {
    assert_eq!(build_of_actions([]), None);
    let a = build_of_actions(["v_b", "v_a", "v_a"]).unwrap();
    assert_eq!(
        a,
        build_of_actions(["v_a", "v_b"]).unwrap(),
        "order and repeats do not matter"
    );
    assert!(a.starts_with("actions-") && a.len() == 24);
    assert_ne!(a, build_of_actions(["v_a", "v_b", "v_c"]).unwrap());
}

#[test]
fn the_support_file_is_validated_capped_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let file = SupportFile::new(Some(dir.path().join("ship-support.json")));
    let learned = vec![Learned {
        class_name: "DRAK_Corsair".into(),
        build: Some("actions-1".into()),
        present: ["gear".to_owned()].into(),
        absent: ["vtol".to_owned()].into(),
    }];
    file.save(&learned).unwrap();
    assert_eq!(file.load(), learned);
    let (read, skipped) = read_support(Some(&json!({ "schemaVersion": 1, "ships": [
        { "className": "<b>x</b>", "absent": ["vtol"] },
        { "className": "OK", "build": 7 },
        { "className": "OK2", "present": ["gear"], "absent": ["gear", "nope", "power.engines"] },
    ] })));
    assert_eq!(read.len(), 1);
    assert!(
        read[0].absent.is_empty(),
        "present wins; unknown fields left out"
    );
    assert_eq!(skipped.len(), 4, "{skipped:?}");
    let many: Vec<Value> = (0..600)
        .map(|i| json!({ "className": format!("S{i}") }))
        .collect();
    let (read, skipped) = read_support(Some(&json!({ "schemaVersion": 1, "ships": many })));
    assert_eq!(read.len(), 500);
    assert_eq!(read[0].class_name, "S100", "the most recent are kept");
    assert_eq!(skipped.len(), 1);
    assert!(
        read_support(Some(&json!({ "schemaVersion": 2, "ships": [] })))
            .0
            .is_empty()
    );
}

#[test]
fn the_ship_key_is_manufacturer_and_model() {
    let (mut s, _) = clocked();
    s.ingest(&corsair());
    let ship = ship_of(&s).unwrap();
    assert_eq!(ship.key, "Drake Interplanetary / Corsair");
    assert_eq!(ship.class_name, "DRAK_Corsair");
    s.ingest(&arrow());
    // No manufacturer in the stream: the class name's code stands for it.
    assert_eq!(ship_of(&s).unwrap().key, "Anvil Aerospace / Arrow");
}

#[test]
fn switches_only_on_a_stable_new_ship_and_never_mid_sequence() {
    let mut sw = Switcher::default();
    let (ka, kb) = ("Drake Interplanetary / Corsair", "Anvil Aerospace / Arrow");
    let a = Some(ka);
    let b = Some(kb);
    assert_eq!(sw.step(0, a, false, true), Step::Nothing);
    assert_eq!(sw.step(STABLE_MS - 1, a, false, true), Step::Nothing);
    assert_eq!(
        sw.step(STABLE_MS, a, false, true),
        Step::SwitchFor(ka.into())
    );
    // Leaving the ship (or stale data) changes nothing, and getting back in neither.
    assert_eq!(sw.step(3_000, None, false, true), Step::Nothing);
    assert_eq!(sw.step(4_000, a, false, true), Step::Nothing);
    assert_eq!(sw.step(9_000, a, false, true), Step::Nothing);
    // A flicker of another ship shorter than 2 s is not a ship change.
    assert_eq!(sw.step(10_000, b, false, true), Step::Nothing);
    assert_eq!(sw.step(11_000, a, false, true), Step::Nothing);
    assert_eq!(sw.step(13_500, a, false, true), Step::Nothing);
    // A new ship while a sequence runs: the switch waits for it to end.
    assert_eq!(sw.step(20_000, b, true, true), Step::Nothing);
    assert_eq!(sw.step(22_000, b, true, true), Step::Nothing);
    assert!(sw.waiting());
    assert_eq!(sw.step(25_000, b, true, true), Step::Nothing);
    assert_eq!(sw.step(26_000, b, false, true), Step::SwitchFor(kb.into()));
    assert!(!sw.waiting());
}

#[test]
fn with_switching_off_a_new_ship_is_only_recognised() {
    let mut sw = Switcher::default();
    let a = Some("A / One");
    sw.step(0, a, false, false);
    assert_eq!(
        sw.step(STABLE_MS, a, false, false),
        Step::Recognised("A / One".into())
    );
    // Turning it on later does not switch for the ship already recognised.
    assert_eq!(sw.step(10_000, a, false, true), Step::Nothing);
    // A hand pick drops a switch that was waiting.
    let b = Some("B / Two");
    sw.step(11_000, b, true, true);
    sw.step(13_000, b, true, true);
    assert!(sw.waiting());
    sw.picked();
    assert_eq!(sw.step(14_000, b, false, true), Step::Nothing);
}

fn ship_ref(m: &str, model: &str, class: &str) -> ShipRef {
    ShipRef {
        manufacturer: m.into(),
        model: model.into(),
        class_name: Some(class.into()),
    }
}

#[test]
fn ships_switch_the_dashboard_and_leave_it_on_getting_out() {
    let (mut s, now) = clocked();
    let store = DashboardStore::default();
    let ships = Ships::new(SupportFile::new(None));
    ships.set_build(build());
    let known = crate::dashboard::Known::default();
    let corsair_board = store
        .create_for_ship(
            ship_ref("Drake Interplanetary", "Corsair", "DRAK_Corsair"),
            None,
            None,
            &known,
        )
        .unwrap()
        .dashboard
        .unwrap();
    assert_eq!(
        corsair_board.theme.as_deref(),
        Some("drake"),
        "Drake's theme"
    );
    store.select(GENERIC_ID, true).unwrap();
    store.set_switch(Some(&json!(true))).unwrap();
    let mut tick = |ms: i64, frame: Option<Value>| {
        now.store(ms, Ordering::SeqCst);
        if let Some(f) = frame {
            s.ingest(&f);
        }
        ships.look(ms, &s, false, &store);
    };
    tick(0, Some(corsair()));
    assert_eq!(store.list().active, GENERIC_ID, "not yet stable");
    tick(2_000, Some(corsair()));
    assert_eq!(store.list().active, corsair_board.id);
    let snap = ships.snapshot();
    assert_eq!(
        snap.ship.unwrap().dashboard.as_deref(),
        Some(corsair_board.id.as_str())
    );
    // Out of the ship: the report goes stale; the dashboard stays.
    tick(8_000, None);
    assert!(ships.snapshot().ship.is_none());
    assert_eq!(store.list().active, corsair_board.id);
    // An Arrow has no dashboard of its own: the generic one.
    tick(9_000, Some(arrow()));
    tick(11_000, Some(arrow()));
    assert_eq!(store.list().active, GENERIC_ID);
}

#[test]
fn with_the_checkbox_off_the_dashboard_never_changes() {
    let (mut s, now) = clocked();
    let store = DashboardStore::default();
    let ships = Ships::new(SupportFile::new(None));
    ships.set_build(build());
    let known = crate::dashboard::Known::default();
    store
        .create_for_ship(
            ship_ref("Drake Interplanetary", "Corsair", "DRAK_Corsair"),
            None,
            None,
            &known,
        )
        .unwrap();
    store.select(GENERIC_ID, true).unwrap();
    assert!(store.list().pinned);
    for ms in [0, 1_000, 2_000, 3_000] {
        now.store(ms, Ordering::SeqCst);
        s.ingest(&corsair());
        ships.look(ms, &s, false, &store);
    }
    assert_eq!(store.list().active, GENERIC_ID);
    assert!(!store.list().pinned, "a new ship ends the hand pick");
    // The picker still offers the ship's dashboard.
    assert!(ships.snapshot().ship.unwrap().dashboard.is_some());
}
