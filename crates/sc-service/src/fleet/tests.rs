use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use sc_core::state::StateModel;

use super::card::{
    Consent, EVENT_DEAD, EVENT_DOWNED, EVENT_RESPAWN, EVENT_SPAWN, FIELDS, Life, LifeTracker,
    RESPAWNED_MS, SendConsent, Sender, Sharing, build_card, card_fields, detail_fields,
};
use super::demo::{Demo, Script, read_recording};
use super::hub::{FleetHub, STALE_AFTER};
use super::view::{FleetViewStore, MAX_GROUP_MEMBERS, MAX_GROUPS, read_view};
use crate::json::to_text;
use crate::portal::status::Tone;
use fleet_proto::rollup::Status;

fn frame() -> Value {
    json!({
        "vehicle": { "state": {
            "name": "Test ship",
            "health": 0.8,
            "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" },
            "flight": { "instruments": { "speed": 120, "hydrogen_fuel": 0.5, "quantum_fuel": 0.2, "altitude": 1500 } },
            "shields": { "level": 0.9, "faces": [1, 0.5, 0.2, 0.9] },
            "power": { "engines": true, "weapons": false },
            "annunciators": {
                "hit": "ANNUNCIATOR_STATE_MAJOR_FLASHING",
                "qdrv": "ANNUNCIATOR_STATE_MINOR_ON",
                "pwr": "ANNUNCIATOR_STATE_NOMINAL",
            },
        } },
        "weapons": { "weapons": [
            { "name": "Gun 1", "is_powered": true, "ready_to_fire": true },
            { "name": "Gun 2", "is_powered": true },
        ] },
        "actor": {
            "vitals": { "health": { "current": 70, "max": 100 }, "oxygen": { "current": 1, "max": 1 } },
            "state": { "locomotion": "ACTOR_LOCOMOTION_STATE_SEATED", "crew_role": "ACTOR_CREW_ROLE_PILOT", "uec_balance": "123456" },
        },
    })
}

fn state_of(frame: &Value) -> StateModel {
    let mut state = StateModel::default();
    state.ingest(frame);
    state
}

fn sender(sharing: Sharing) -> Sender {
    Sender {
        member: "m-1".to_owned(),
        handle: "Tester".to_owned(),
        consent: SendConsent::groups(sharing),
    }
}

fn all_fields() -> HashSet<&'static str> {
    FIELDS.iter().map(|f| f.id).collect()
}

#[test]
fn builds_a_card_from_the_game_state() {
    let state = state_of(&frame());
    let card = build_card(
        &state,
        &LifeTracker::default(),
        0,
        &sender(Sharing::ALL),
        &all_fields(),
    );
    assert_eq!(card.schema, 1);
    assert_eq!(card.state, Some(Life::Up));
    let f = |id| card.fields.get(id).unwrap_or_else(|| panic!("{id}"));
    assert_eq!(f("ship.name").text, "Test ship");
    assert_eq!(f("ship.mode").text, "SCM");
    assert_eq!(f("ship.shields").fraction, Some(0.9));
    assert_eq!(f("ship.shields").text, "90%");
    // Hull arrives as a plain 0..1 number.
    assert_eq!(f("ship.hull").text, "80%");
    assert_eq!(f("ship.hull").fraction, Some(0.8));
    assert_eq!(f("ship.fuel").text, "50%");
    assert_eq!(f("ship.quantum").tone, Tone::Bad);
    assert_eq!(f("ship.cautions").text, "QDRV HIT");
    assert_eq!(f("ship.cautions").tone, Tone::Bad);
    assert_eq!(f("ship.weapons").text, "1/2");
    assert_eq!(f("ship.weapons").tone, Tone::Warn);
    assert_eq!(f("ship.aboard").text, "IN SHIP");
    assert_eq!(f("char.health").text, "70%");
    assert_eq!(f("char.oxygen").fraction, Some(1.0));
    assert_eq!(f("char.activity").text, "SEATED, PILOT");
    assert_eq!(f("detail.shield_faces").items.as_ref().unwrap().len(), 4);
    let power = f("detail.power").items.clone().unwrap();
    assert!(
        power
            .iter()
            .any(|r| r.label == "Engines power" && r.text == "ON")
    );
    assert_eq!(f("detail.speed").text, "120 m/s");
    // Fields in display order, and never the balance.
    let ids: Vec<&str> = card.fields.0.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, FIELDS.iter().map(|f| f.id).collect::<Vec<_>>());
    let text = to_text(&card);
    assert!(!text.contains("123456") && !text.contains("uec"), "{text}");
}

#[test]
fn a_card_carries_only_what_the_sender_allows_and_the_viewer_wants() {
    let state = state_of(&frame());
    let life = LifeTracker::default();
    let ship_only = Sharing {
        ship: true,
        ..Sharing::default()
    };
    let card = build_card(&state, &life, 0, &sender(ship_only), &all_fields());
    assert_eq!(card.state, None);
    assert!(card.fields.0.iter().all(|(id, _)| id.starts_with("ship.")));
    assert!(!to_text(&card).contains("\"state\""));

    let nothing = build_card(&state, &life, 0, &sender(Sharing::default()), &all_fields());
    assert!(nothing.fields.0.is_empty() && nothing.state.is_none());

    // The viewer's card fields: no detail unless a detail view asked for it.
    let wanted: HashSet<_> = ["ship.hull", "char.health"].into();
    let card = build_card(&state, &life, 0, &sender(Sharing::ALL), &wanted);
    let ids: Vec<&str> = card.fields.0.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, ["ship.hull", "char.health"]);
}

#[test]
fn a_card_carries_only_the_single_fields_consented_to() {
    let state = state_of(&frame());
    let mut consent = SendConsent::none();
    consent.set("ship.hull", true);
    consent.set("char.health", true);
    let s = Sender {
        consent,
        ..sender(Sharing::default())
    };
    let card = build_card(&state, &LifeTracker::default(), 0, &s, &all_fields());
    let ids: Vec<&str> = card.fields.ids().collect();
    assert_eq!(ids, ["ship.hull", "char.health"]);
    assert!(card.sharing.ship && card.sharing.character && !card.sharing.detail);
    assert_eq!(card.state, Some(Life::Up));
    assert!(card.check().is_ok());
}

#[test]
fn out_of_a_ship_the_ship_fields_are_dashes() {
    let mut f = frame();
    f.as_object_mut().unwrap().remove("vehicle");
    f["actor"]["state"]["locomotion"] = json!("ACTOR_LOCOMOTION_STATE_ON_FOOT");
    let card = build_card(
        &state_of(&f),
        &LifeTracker::default(),
        0,
        &sender(Sharing::ALL),
        &all_fields(),
    );
    let f = |id| card.fields.get(id).unwrap().clone();
    assert_eq!(f("ship.shields").text, "\u{2014}");
    assert_eq!(f("ship.shields").tone, Tone::Unknown);
    assert_eq!(f("ship.cautions").text, "\u{2014}");
    assert_eq!(f("ship.aboard").text, "NOT IN SHIP");
    assert_eq!(f("char.activity").text, "ON FOOT");
    assert_eq!(f("char.health").text, "70%");
}

fn events(types: &[&str]) -> Value {
    let list: Vec<Value> = types.iter().map(|t| json!({ "type": t })).collect();
    json!({ "events": { "events": list } })
}

#[test]
fn the_state_follows_game_events_and_health() {
    let mut life = LifeTracker::default();
    assert_eq!(life.life(0), Life::Up);
    life.observe_frame(&events(&[EVENT_DOWNED]), 1_000);
    assert_eq!(life.life(1_000), Life::Downed);
    // Health back up is taken as revived.
    life.observe_health(Some(0.3), 2_000);
    assert_eq!(life.life(2_000), Life::Downed);
    life.observe_health(Some(0.5), 3_000);
    assert_eq!(life.life(3_000), Life::Up);
    // Health at zero is downed, then dead stays dead whatever the health and a later downed.
    life.observe_health(Some(0.0), 4_000);
    assert_eq!(life.life(4_000), Life::Downed);
    life.observe_frame(&events(&[EVENT_DEAD, EVENT_DOWNED]), 5_000);
    assert_eq!(life.life(5_000), Life::Dead);
    life.observe_health(Some(1.0), 6_000);
    assert_eq!(life.life(6_000), Life::Dead);
    // Respawned for a while, then up.
    life.observe_frame(&events(&[EVENT_RESPAWN]), 7_000);
    assert_eq!(life.life(7_000), Life::Respawned);
    assert_eq!(life.life(7_000 + RESPAWNED_MS - 1), Life::Respawned);
    assert_eq!(life.life(7_000 + RESPAWNED_MS), Life::Up);
    // Spawn is up; other events and frames without events change nothing.
    life.observe_frame(&events(&[EVENT_DEAD, EVENT_SPAWN]), 50_000);
    assert_eq!(life.life(50_000), Life::Up);
    life.observe_frame(&events(&["GAME_EVENT_TYPE_ACTOR_ENTER_SEAT"]), 51_000);
    life.observe_frame(&frame(), 51_000);
    life.observe_health(None, 51_000);
    assert_eq!(life.life(51_000), Life::Up);
}

#[test]
fn the_view_store_keeps_groups_within_its_limits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fleet-view.json");
    let store = FleetViewStore::new(Some(file.clone()));
    let add = |name: &str| store.add_group(Some(&json!(name))).unwrap();
    let wing = add("Wing 1").group.unwrap();
    let escort = add(" Escort ").group.unwrap();
    assert_eq!(escort.name, "Escort");
    assert_eq!(
        (wing.id.as_str(), escort.id.as_str()),
        ("group-1", "group-2")
    );
    let long = "x".repeat(41);
    for bad in ["", "   ", "<b>bold</b>", long.as_str()] {
        assert!(!add(bad).problems.is_empty(), "{bad:?}");
    }
    assert!(
        !store
            .add_group(Some(&json!(3)))
            .unwrap()
            .problems
            .is_empty(),
        "a name must be text"
    );

    // A member is in one group at most; Ungrouped is no group.
    assert!(
        store
            .assign("m-1", Some(&wing.id), None)
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        store
            .assign("m-2", Some(&wing.id), None)
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        store
            .assign("m-3", Some(&wing.id), Some(&json!(0)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        store
            .assign("m-1", Some(&escort.id), None)
            .unwrap()
            .problems
            .is_empty()
    );
    let v = store.get();
    assert_eq!(v.groups[0].members, ["m-3", "m-2"]);
    assert_eq!(v.groups[1].members, ["m-1"]);
    assert!(store.assign("m-2", None, None).unwrap().problems.is_empty());
    assert_eq!(store.get().groups[0].members, ["m-3"]);
    assert!(
        !store
            .assign("bad id!", Some(&wing.id), None)
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        !store
            .assign("m-4", Some("nope"), None)
            .unwrap()
            .problems
            .is_empty()
    );

    // Rename, move, collapse, remove.
    assert!(
        store
            .rename_group(&wing.id, Some(&json!("Alpha")))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        !store
            .rename_group(&wing.id, Some(&json!("<x>")))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        !store
            .move_group(&wing.id, Some(&json!(2)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        store
            .move_group(&wing.id, Some(&json!(1)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(store.get().groups[1].name, "Alpha");
    assert!(
        store
            .collapse_group(&wing.id, Some(&json!(true)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        !store
            .collapse_group(&wing.id, Some(&json!("yes")))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(store.get().groups[1].collapsed);
    assert!(store.remove_group(&escort.id).unwrap().problems.is_empty());
    let v = store.get();
    assert_eq!(v.groups.len(), 1);
    assert!(v.group_of("m-1").is_none(), "back to Ungrouped");
    assert!(!store.remove_group("group-9").unwrap().problems.is_empty());

    // Fields: only card fields, kept in display order.
    let set = |fields: Value| store.set_fields(Some(&fields)).unwrap();
    assert!(set(json!(["char.health", "ship.hull"])).problems.is_empty());
    assert_eq!(store.get().fields, ["ship.hull", "char.health"]);
    assert!(!set(json!(["detail.power"])).problems.is_empty());
    assert!(!set(json!(["ship.balance"])).problems.is_empty());
    assert!(!set(json!("ship.hull")).problems.is_empty());
    assert!(
        store
            .set_admiral(Some(&json!(true)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(
        !store
            .set_admiral(Some(&json!(1)))
            .unwrap()
            .problems
            .is_empty()
    );

    // Saved as plain text and read back as it was.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"Alpha\""));
    assert_eq!(FleetViewStore::new(Some(file)).get(), store.get());

    // Caps.
    let store = FleetViewStore::new(None);
    for i in 0..MAX_GROUPS {
        assert!(
            store
                .add_group(Some(&json!(format!("G{i}"))))
                .unwrap()
                .problems
                .is_empty()
        );
    }
    assert!(
        !store
            .add_group(Some(&json!("one more")))
            .unwrap()
            .problems
            .is_empty()
    );
    for i in 0..MAX_GROUP_MEMBERS {
        store
            .assign(&format!("m{i}"), Some("group-1"), None)
            .unwrap();
    }
    assert!(
        !store
            .assign("extra", Some("group-1"), None)
            .unwrap()
            .problems
            .is_empty()
    );
    // Moving within a full group is not adding.
    assert!(
        store
            .assign("m0", Some("group-1"), Some(&json!(5)))
            .unwrap()
            .problems
            .is_empty()
    );
}

#[test]
fn reads_a_view_file_and_leaves_out_what_is_wrong() {
    let raw = json!({
        "schemaVersion": 1,
        "extra": true,
        "fields": ["ship.hull", "detail.power", "ship.unknown", "char.health"],
        "admiral": true,
        "groups": [
            { "id": "group-1", "name": "Wing", "collapsed": true, "members": ["a", "b", "a", "bad id"] },
            { "id": "group-1", "name": "Again" },
            { "id": "BAD", "name": "Upper" },
            { "id": "group-2", "name": "<script>" },
            { "id": "group-3", "name": "Escort", "members": ["b", "c"] },
        ],
    });
    let (view, skipped) = read_view(&raw);
    assert_eq!(view.fields, ["ship.hull", "char.health"]);
    assert!(view.admiral);
    let names: Vec<&str> = view.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Wing", "Escort"]);
    assert_eq!(view.groups[0].members, ["a", "b"]);
    assert!(view.groups[0].collapsed);
    assert_eq!(view.groups[1].members, ["c"], "b is in Wing already");
    assert_eq!(skipped.len(), 9, "{skipped:#?}");

    let many: Vec<Value> = (0..30)
        .map(|i| json!({ "id": format!("g{i}"), "name": format!("G{i}") }))
        .collect();
    let (view, _) = read_view(&json!({ "groups": many }));
    assert_eq!(view.groups.len(), MAX_GROUPS);
    let (view, skipped) = read_view(&json!([1, 2]));
    assert!(view.groups.is_empty() && skipped.len() == 1);
    assert_eq!(view.fields.len(), card_fields().count(), "the defaults");
}

#[test]
fn a_view_file_over_the_cap_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fleet-view.json");
    let name = "x".repeat(300 * 1024);
    std::fs::write(
        &file,
        json!({ "groups": [{ "id": "g", "name": name }] }).to_string(),
    )
    .unwrap();
    assert!(FleetViewStore::new(Some(file)).get().groups.is_empty());
}

fn card(member: &str, life: Option<Life>) -> super::card::Card {
    let sharing = Sharing {
        character: life.is_some(),
        ..Sharing::ALL
    };
    let mut c = build_card(
        &StateModel::default(),
        &LifeTracker::default(),
        0,
        &Sender {
            member: member.to_owned(),
            handle: member.to_uppercase(),
            consent: SendConsent::groups(sharing),
        },
        &HashSet::new(),
    );
    c.state = life;
    c
}

#[tokio::test(start_paused = true)]
async fn the_hub_groups_members_rolls_them_up_and_marks_stale_cards_offline() {
    let view = Arc::new(FleetViewStore::new(None));
    let hub = FleetHub::new(Arc::clone(&view), true);
    let wing = view.add_group(Some(&json!("Wing"))).unwrap().group.unwrap();
    for m in ["a", "b", "c"] {
        view.assign(m, Some(&wing.id), None).unwrap();
    }
    view.assign("ghost", Some(&wing.id), None).unwrap();
    assert!(hub.receive(card("a", Some(Life::Up))));
    assert!(hub.receive(card("b", Some(Life::Dead))));
    assert!(hub.receive(card("c", Some(Life::Downed))));
    assert!(hub.receive(card("d", None)));
    let mut newer = card("e", None);
    newer.schema = 2;
    assert!(!hub.receive(newer), "a newer card schema is dropped");
    assert!(!hub.receive(card("bad id", None)));

    let m = hub.model();
    assert!(m.demo);
    assert_eq!(m.groups.len(), 2);
    assert_eq!(
        m.groups[0].members,
        ["c", "b", "a"],
        "downed first; ghost not received"
    );
    assert_eq!(m.groups[0].rollup.text, "1/3 up, 1 downed, 1 dead");
    assert_eq!(m.groups[1].id, None);
    assert_eq!(m.groups[1].name, "Ungrouped");
    assert_eq!(m.groups[1].members, ["d"]);
    assert_eq!(m.admiral, ["c", "b", "a", "d"]);
    assert_eq!(m.rollup.total, 4);

    // A card not updated within the stale window is OFFLINE; a fresh one is not.
    tokio::time::advance(STALE_AFTER - Duration::from_millis(1)).await;
    hub.receive(card("a", Some(Life::Up)));
    tokio::time::advance(Duration::from_millis(1)).await;
    let m = hub.model();
    let status = |id: &str| {
        m.members
            .iter()
            .find(|x| x.card.member == id)
            .unwrap()
            .status
    };
    assert_eq!(status("a"), Status::Up);
    assert_eq!(status("b"), Status::Offline);
    assert_eq!(m.groups[0].members, ["a", "b", "c"]);
    let text = to_text(&m);
    assert!(text.contains("\"ageMs\":1,"), "{text}");
}

#[test]
fn the_hub_asks_for_detail_fields_only_while_a_detail_view_is_open() {
    let view = Arc::new(FleetViewStore::new(None));
    let hub = FleetHub::new(Arc::clone(&view), false);
    view.set_fields(Some(&json!(["ship.hull"]))).unwrap();
    assert_eq!(hub.wanted("a"), HashSet::from(["ship.hull"]));
    let page = hub.viewer();
    hub.set_detail(page, Some("a"));
    let wanted = hub.wanted("a");
    assert!(detail_fields().all(|f| wanted.contains(f.id)));
    assert!(!wanted.contains("char.health"));
    assert_eq!(hub.wanted("b"), HashSet::from(["ship.hull"]));
    hub.set_detail(page, None);
    assert_eq!(hub.wanted("a"), HashSet::from(["ship.hull"]));
    assert!(
        FIELDS
            .iter()
            .filter(|f| f.consent == Consent::Detail)
            .count()
            > 0
    );
}

fn recording_text() -> String {
    // 20 s at 10 frames a second, in a ship, with one real event.
    (0..200)
        .map(|i| {
            let mut f = frame();
            if i == 57 {
                f["events"] = json!({ "events": [{ "type": "GAME_EVENT_TYPE_ACTOR_LEAVE_SEAT" }] });
            }
            json!({ "t": 1000 + i * 100, "frame": f }).to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_recording_is_thinned_to_two_frames_a_second_keeping_events() {
    let rec = read_recording("test", Cursor::new(recording_text())).unwrap();
    assert_eq!(rec.frames[0].t, 0.0);
    assert_eq!(rec.frames.len(), 41, "40 kept plus the event frame");
    assert!(rec.frames.iter().any(|f| f.frame.get("events").is_some()));
    assert!(rec.duration_ms > 19_000.0);
    assert!(
        rec.frames[0].frame["vehicle"]["state"]["flight"]
            .get("settings")
            .is_none()
    );
    assert!(read_recording("bad", Cursor::new("{\"t\": 1}")).is_err());
    assert!(read_recording("empty", Cursor::new("\n")).is_err());
}

#[test]
fn scripted_events_fire_once_per_loop() {
    let s = Script::downed_dead_respawn();
    assert!(s.beats.iter().all(|b| b.at_ms < s.period_ms));
    let one = s.events_between(0, s.period_ms);
    assert_eq!(one, [EVENT_DOWNED, EVENT_DEAD, EVENT_RESPAWN]);
    let two = s.events_between(-1, 2 * s.period_ms);
    assert_eq!(two.len(), 6);
    assert!(s.events_between(26_000, 27_000).is_empty());
}

#[test]
fn the_demo_peers_send_cards_through_the_hub() {
    let rec = read_recording("test", Cursor::new(recording_text())).unwrap();
    let view = Arc::new(FleetViewStore::new(None));
    let hub = FleetHub::new(view, true);
    let mut demo = Demo::new(vec![rec.clone(), rec], 5);
    let handles: Vec<&str> = demo
        .peers()
        .iter()
        .map(|p| p.sender.handle.as_str())
        .collect();
    assert_eq!(
        handles,
        [
            "Demo Vega",
            "Demo Rook",
            "Demo Kestrel",
            "Demo Wren",
            "Demo Ash"
        ]
    );
    assert_eq!(demo.peers()[0].offset_ms, 0.0);
    assert_eq!(demo.peers()[1].offset_ms, 0.0);
    assert!(
        demo.peers()[2].offset_ms > 0.0,
        "derived peers start elsewhere"
    );

    let status = |hub: &FleetHub, id: &str| {
        hub.model()
            .members
            .iter()
            .find(|m| m.card.member == id)
            .map(|m| m.status)
    };
    demo.step(0, &hub);
    assert_eq!(hub.model().members.len(), 5);
    assert_eq!(status(&hub, "demo-rook"), Some(Status::Up));
    assert_eq!(
        status(&hub, "demo-wren"),
        Some(Status::Unknown),
        "ship status only"
    );
    let vega = hub.model().members[0].card.clone();
    assert_eq!(vega.handle, "Demo Vega");
    assert_eq!(vega.fields.get("ship.name").unwrap().text, "Demo Lancer");
    assert!(
        vega.fields.get("detail.power").is_none(),
        "no detail view open"
    );

    // SCRIPTED: Rook goes down at 25 s, dies at 70 s, respawns at 90 s; Kestrel is downed at 40 s.
    for t in (500..=26_000).step_by(500) {
        demo.step(t, &hub);
    }
    assert_eq!(status(&hub, "demo-rook"), Some(Status::Downed));
    let rook = hub
        .model()
        .members
        .into_iter()
        .find(|m| m.card.member == "demo-rook")
        .unwrap();
    assert_eq!(rook.card.fields.get("char.health").unwrap().text, "8%");
    for t in (26_500..=71_000).step_by(500) {
        demo.step(t, &hub);
    }
    assert_eq!(status(&hub, "demo-rook"), Some(Status::Dead));
    assert_eq!(status(&hub, "demo-kestrel"), Some(Status::Downed));
    for t in (71_500..=91_000).step_by(500) {
        demo.step(t, &hub);
    }
    assert_eq!(status(&hub, "demo-rook"), Some(Status::Respawned));
}
