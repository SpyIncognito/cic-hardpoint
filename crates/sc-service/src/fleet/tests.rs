use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use sc_core::state::StateModel;

use super::card::{
    Consent, EVENT_DEAD, EVENT_DOWNED, EVENT_RESPAWN, EVENT_SPAWN, FIELDS, Life, LifeTracker,
    RESPAWNED_MS, SendConsent, Sender, Sharing, build_card, card_fields, detail_fields,
};
use super::hub::{FleetHub, Layout, STALE_AFTER};
use super::view::{FleetViewStore, MAX_GROUP_MEMBERS, MAX_GROUPS, read_view};
use crate::json::to_text;
use crate::portal::status::Tone;
use fleet_proto::Role;
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
fn the_card_names_the_ship_as_the_dashboard_does() {
    let mut state = state_of(&json!({ "vehicle": { "state": {
        "name": "DRAK_Corsair_877723283935", "class_name": "DRAK_Corsair" } } }));
    let name = |state: &StateModel| {
        let card = build_card(
            state,
            &LifeTracker::default(),
            0,
            &sender(Sharing::ALL),
            &all_fields(),
        );
        card.fields.get("ship.name").unwrap().text.clone()
    };
    assert_eq!(name(&state), "Drake Corsair");
    state.set_ship_names(std::collections::HashMap::from([(
        "DRAK_Corsair".to_owned(),
        "Old Betsy".to_owned(),
    )]));
    assert_eq!(name(&state), "Old Betsy");
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
    let hub = FleetHub::new(Arc::clone(&view), key(0));
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
    assert_eq!(m.layout, Layout::Personal);
    assert!(m.admiral_allowed);
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
    assert_eq!(m.wings.len(), 1);
    assert_eq!(m.wings[0].id, None);
    assert_eq!(m.wings[0].rollup.total, 4);
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
    let hub = FleetHub::new(Arc::clone(&view), key(0));
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

fn key(n: u8) -> fleet_proto::PublicKey {
    fleet_proto::Identity::from_seed([n; 32]).public()
}

#[test]
fn the_fleet_key_is_made_once_user_only_and_a_damaged_one_is_refused() {
    use super::identity::{KEY_FILE, load_or_create};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings").join(KEY_FILE);
    let made = load_or_create(Some(&file)).unwrap();
    let again = load_or_create(Some(&file)).unwrap();
    assert_eq!(made.public(), again.public());
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains(&made.public().to_string())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |f: &std::path::Path| std::fs::metadata(f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create(Some(&file)).unwrap();
        assert_eq!(mode(&file), 0o600, "put back to user-only");
    }
    assert_ne!(load_or_create(None).unwrap().public(), made.public());
    std::fs::write(&file, "not a key\n").unwrap();
    assert!(load_or_create(Some(&file)).is_err());
}

#[test]
fn the_fleets_file_keeps_what_checks_out_and_never_a_code() {
    use super::store::{FleetsStore, JoinedFleet, read_fleets};
    let good = key(1).to_string();
    let raw = json!({
        "schemaVersion": 1,
        "active": good,
        "fleets": [
            { "host": "relay.example.org", "port": 7443, "key": good, "handle": "Vega", "code": "secret" },
            { "host": "bad host", "port": 1, "key": key(2).to_string(), "handle": "V" },
            { "host": "h", "port": 1, "key": "short", "handle": "V" },
            { "host": "h", "port": 0, "key": key(3).to_string(), "handle": "V" },
            { "host": "h", "port": 1, "key": key(4).to_string(), "handle": "<b>" },
            { "host": "other.example.org", "port": 1, "key": good, "handle": "Again" },
            { "host": "h", "port": 1, "key": key(9).to_string(), "handle": "V", "hosted": true, "portal": "http://cic.example.org" },
        ],
    });
    let (fleets, skipped) = read_fleets(&raw);
    assert_eq!(fleets.fleets.len(), 1, "{skipped:?}");
    assert_eq!(skipped.len(), 6, "{skipped:?}");
    assert!(skipped[5].contains("bad portal address"), "{skipped:?}");
    assert_eq!(
        fleets.active_fleet().map(|f| f.handle.as_str()),
        Some("Vega")
    );
    assert!(
        read_fleets(&json!({ "schemaVersion": 2, "fleets": [] }))
            .1
            .len()
            == 1
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fleets.json");
    let store = FleetsStore::new(Some(file.clone()));
    store
        .joined(JoinedFleet {
            host: "::1".to_owned(),
            port: 9,
            key: key(5),
            handle: "Vega".to_owned(),
            event: Some("Op One".to_owned()),
            hosted: true,
            event_id: Some("op-1".to_owned()),
            portal: Some("https://cic.example.org".to_owned()),
        })
        .unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("code"), "{text}");
    assert!(
        text.contains(r#""portal": "https://cic.example.org""#),
        "{text}"
    );
    let again = FleetsStore::new(Some(file));
    assert_eq!(again.get().fleets[0].address(), "[::1]:9");
    assert!(again.get().fleets[0].hosted);
    assert_eq!(again.get().fleets[0].event_id.as_deref(), Some("op-1"));
    assert_eq!(
        again.get().fleets[0].portal.as_deref(),
        Some("https://cic.example.org")
    );
    assert_eq!(again.get().active, Some(key(5)));
    again.remove(&key(5)).unwrap();
    assert!(again.get().fleets.is_empty() && again.get().active.is_none());
}

fn client_with(frame: &Value) -> Arc<super::client::FleetClient> {
    use super::client::{FleetClient, TlsConnector};
    use super::store::{ConsentStore, FleetsStore};
    let core = Arc::new(sc_core::ControlCore::new(
        sc_core::ControlCoreOptions::default(),
    ));
    core.ingest(frame);
    let identity = fleet_proto::Identity::from_seed([7; 32]);
    let hub = Arc::new(FleetHub::new(
        Arc::new(FleetViewStore::new(None)),
        identity.public(),
    ));
    FleetClient::new(
        hub,
        core,
        identity,
        FleetsStore::new(None),
        ConsentStore::new(None),
        Arc::new(TlsConnector::new().unwrap()),
    )
}

#[tokio::test]
async fn consent_filters_the_card_before_it_is_sent_and_starts_off() {
    let client = client_with(&frame());
    assert_eq!(
        client.consent(),
        SendConsent::none(),
        "nothing until opted in"
    );
    let card = client.preview();
    assert!(card.fields.0.is_empty());
    assert_eq!(card.state, None);
    assert_eq!(card.sharing, Sharing::default());

    client
        .set_consent(Some(&json!(["ship.hull", "char.health", "detail.speed"])))
        .unwrap();
    let card = client.preview();
    assert_eq!(
        card.fields.ids().collect::<Vec<_>>(),
        ["ship.hull", "char.health", "detail.speed"]
    );
    assert_eq!(card.state, Some(Life::Up));
    assert!(card.check().is_ok());
    let text = serde_json::to_string(&card).unwrap();
    assert!(!text.contains("123456"), "never the balance: {text}");

    assert!(
        client
            .set_consent(Some(&json!(["ship.hull", "nope"])))
            .is_err()
    );
    assert!(
        client.consent().allows("ship.hull"),
        "a refused change keeps the old one"
    );
    client.set_consent(Some(&json!([]))).unwrap();
    assert!(client.preview().fields.0.is_empty());
}

/// Admiral 1. Wing "air" (commander 2) with squadrons f2 (commander 4; pilots 4 and 5) and f1
/// (commander 2; pilots 2 and 3). Ground "army" (commander 6) with squad "ground" (soldier 6).
fn structured_event() -> fleet_proto::EventStructure {
    use fleet_proto::structure::{EventInfo, EventState, Seat, Unit, UnitKind};
    let unit = |id: &str, kind, parent: Option<&str>, lead: &[u8], seats: &[(u8, Role)]| Unit {
        id: id.to_owned(),
        kind,
        name: id.to_uppercase(),
        parent: parent.map(str::to_owned),
        lead: lead.iter().map(|n| key(*n).to_string()).collect(),
        seats: seats
            .iter()
            .map(|(n, role)| Seat {
                member: key(*n).to_string(),
                role: *role,
            })
            .collect(),
    };
    let ship = |id: &str, parent: &str, pilot: u8| {
        unit(
            id,
            UnitKind::Ship,
            Some(parent),
            &[],
            &[(pilot, Role::Pilot)],
        )
    };
    let mut air = unit("air", UnitKind::Wing, None, &[2], &[]);
    air.name = "Air wing".to_owned();
    let mut army = unit("army", UnitKind::Ground, None, &[6], &[]);
    army.name = "Army".to_owned();
    fleet_proto::EventStructure {
        event: EventInfo {
            id: "op".to_owned(),
            name: "Op".to_owned(),
            state: EventState::Open,
        },
        admiral: Some(key(1).to_string()),
        units: vec![
            air,
            unit("f2", UnitKind::Squadron, Some("air"), &[4], &[]),
            ship("f2-4", "f2", 4),
            ship("f2-5", "f2", 5),
            unit("f1", UnitKind::Squadron, Some("air"), &[2], &[]),
            ship("f1-2", "f1", 2),
            ship("f1-3", "f1", 3),
            army,
            unit(
                "ground",
                UnitKind::Squad,
                Some("army"),
                &[],
                &[(6, Role::Soldier)],
            ),
        ],
        unassigned: vec![],
    }
}

#[test]
fn the_panel_draws_the_unit_tree_with_labelled_roll_ups_and_role_icons() {
    use super::hub::LinkInfo;
    use fleet_proto::Grant;
    let view = Arc::new(FleetViewStore::new(None));
    let hub = FleetHub::new(view, key(3));
    hub.set_structure(Some(structured_event().view_for(&key(3).to_string())));
    let life = [
        (1, Life::Up),
        (2, Life::Up),
        (3, Life::Downed),
        (4, Life::Dead),
        (6, Life::Up),
        (9, Life::Up),
    ];
    for (n, l) in life {
        assert!(hub.receive(card(&key(n).to_string(), Some(l))));
    }
    let m = hub.model();
    assert_eq!(m.layout, Layout::Event);
    assert!(m.placed && !m.commander);
    assert!(!m.admiral_allowed, "a plain member has no admiral view");
    let tree = m.tree.as_ref().unwrap();
    assert_eq!(tree.name, "Op");
    assert_eq!(tree.admiral.as_ref().unwrap().member, key(1).to_string());
    assert_eq!(tree.admiral_role.label, "Admiral");
    let tops: Vec<_> = tree.units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(tops, ["Air wing"]);
    let air = &tree.units[0];
    assert_eq!(air.lead_role.label, "Wing Commander");
    assert_eq!(air.lead_role.icon, "wing-commander");
    assert_eq!(air.leads[0].member, key(2).to_string());
    let squadrons: Vec<_> = air.units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(squadrons, ["F1"], "not the wing's other squadron");
    let f1 = &air.units[0];
    assert!(f1.open, "a pilot sees their whole squadron");
    assert_eq!(f1.rollup.text, "1/2 Pilots up, 1 downed");
    assert_eq!(f1.lead_role.label, "Squadron Commander");
    let ships: Vec<_> = f1.units.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(ships, ["F1-2", "F1-3"]);
    let seat = &f1.units[1].seats[0];
    assert_eq!(seat.person.member, key(3).to_string());
    assert_eq!(seat.role.label, "Pilot");
    assert_eq!(f1.units[1].rollup.text, "0/1 Pilots up, 1 downed");
    assert!(f1.units[1].fighter);

    // A fighter's ship card is not drawn: the Pilot's own card stands for the ship.
    let mut relayed = fleet_proto::ShipSource::offline();
    relayed.kind = fleet_proto::SourceKind::Pilot;
    assert!(hub.receive(ship_card("f1-3", relayed, "ANVL_Arrow")));
    let m = hub.model();
    let f1_3 = &m.tree.as_ref().unwrap().units[0].units[0].units[1];
    assert!(f1_3.fighter && f1_3.ship_card.is_none());
    assert!(!air.open, "the wing above shows its name and lead only");
    assert_eq!(tree.units.len(), 1, "nothing of the ground force");
    assert!(m.unassigned.is_empty(), "a pilot does not see the bucket");

    let roles = |n: u8| {
        m.members
            .iter()
            .find(|x| x.card.member == key(n).to_string())
            .unwrap()
            .roles
            .clone()
    };
    assert_eq!(roles(1), [Role::Admiral]);
    assert_eq!(roles(2), [Role::WingCommander, Role::SquadronCommander]);
    assert!(roles(3).is_empty());
    assert!(
        m.members
            .iter()
            .any(|x| x.me && x.card.member == key(3).to_string())
    );

    // The admiral sees every unit; members not connected count as offline in the roll-ups.
    hub.set_member(Some(key(1).to_string()));
    hub.set_structure(Some(structured_event()));
    let m = hub.model();
    assert!(m.commander);
    let tree = m.tree.unwrap();
    assert_eq!(
        tree.units[0].rollup.text,
        "1/4 Pilots up, 1 downed, 1 dead, 1 offline"
    );
    assert_eq!(tree.units[1].rollup.text, "1/1 Soldiers up");
    assert_eq!(tree.units[1].units[0].rollup.text, "1/1 Soldiers up");
    let f2_5 = &tree.units[0].units[0].units[1].seats[0].person;
    assert!(!f2_5.connected && !f2_5.card, "key 5 has not connected yet");

    hub.set_link(LinkInfo {
        grants: structured_event()
            .grants_of(&key(2).to_string())
            .into_iter()
            .collect::<Vec<Grant>>(),
        ..LinkInfo::default()
    });
    assert!(hub.model().admiral_allowed, "a commander has it");
    hub.set_structure(None);
    assert_eq!(hub.model().layout, Layout::Personal);
}

#[test]
fn a_fighter_is_its_pilots_card_with_the_copilot_under_it() {
    use fleet_proto::card::{FieldValue, Tone};
    use fleet_proto::structure::Seat;
    let mut event = structured_event();
    let f1_3 = event.units.iter_mut().find(|u| u.id == "f1-3").unwrap();
    f1_3.seats.insert(
        0,
        Seat {
            member: key(9).to_string(),
            role: Role::Copilot,
        },
    );
    let hub = FleetHub::new(Arc::new(FleetViewStore::new(None)), key(1));
    hub.set_member(Some(key(1).to_string()));
    hub.set_structure(Some(event));
    let named = |n: u8, ship: &str| {
        let mut c = card(&key(n).to_string(), Some(Life::Up));
        c.fields
            .0
            .push(("ship.name", FieldValue::text(ship, Tone::Info)));
        c
    };
    hub.receive(named(3, "Arrow"));
    hub.receive(named(9, "Gladius"));
    let m = hub.model();
    let f1_3 = &m.tree.as_ref().unwrap().units[0].units[1].units[1];
    assert_eq!(f1_3.id, "f1-3");
    let rows: Vec<_> = f1_3
        .seats
        .iter()
        .map(|s| (s.role.label, s.not_aboard))
        .collect();
    assert_eq!(rows, [("Pilot", false), ("Copilot", true)]);
    assert_eq!(f1_3.rollup.text, "2/2 Pilots up");
}

/// The event from [`structured_event`], with keys 7 and 8 Unassigned and a fleet "navy"
/// (commander 6, who also soldiers) holding the ship "bounty" (captain 10; pilot 10, crew 11).
fn event_with_unassigned() -> fleet_proto::EventStructure {
    use fleet_proto::structure::{Seat, Unit, UnitKind};
    let mut s = structured_event();
    s.unassigned = vec![key(7).to_string(), key(8).to_string()];
    s.units.push(Unit {
        id: "navy".to_owned(),
        kind: UnitKind::Fleet,
        name: "Navy".to_owned(),
        parent: None,
        lead: vec![key(6).to_string()],
        seats: vec![],
    });
    s.units.push(Unit {
        id: "bounty".to_owned(),
        kind: UnitKind::Ship,
        name: "Bounty".to_owned(),
        parent: Some("navy".to_owned()),
        lead: vec![key(10).to_string()],
        seats: vec![
            Seat {
                member: key(10).to_string(),
                role: Role::Pilot,
            },
            Seat {
                member: key(11).to_string(),
                role: Role::Crew,
            },
        ],
    });
    assert!(s.problems().is_empty(), "{:?}", s.problems());
    s
}

#[test]
fn commanders_see_the_unassigned_by_name_and_an_unassigned_member_only_their_own_card() {
    use fleet_proto::RosterEntry;
    let full = event_with_unassigned();
    let view = Arc::new(FleetViewStore::new(None));
    let hub = FleetHub::new(view, key(20));
    // The relay sends each member their own view.
    hub.set_member(Some(key(6).to_string()));
    hub.set_structure(Some(full.view_for(&key(6).to_string())));
    hub.set_roster(vec![
        RosterEntry {
            member: key(7).to_string(),
            handle: Some("Wren".to_owned()),
            role: None,
            unit: None,
            key: Some(key(7)),
        },
        RosterEntry {
            member: key(8).to_string(),
            handle: Some("Ash".to_owned()),
            role: None,
            unit: None,
            key: None,
        },
    ]);
    let m = hub.model();
    assert!(m.commander, "a ground and fleet commander");
    let names: Vec<_> = m
        .unassigned
        .iter()
        .map(|p| (p.handle.as_str(), p.connected, p.card))
        .collect();
    assert_eq!(names, [("Wren", true, false), ("Ash", false, false)]);
    let text = to_text(&m.unassigned);
    assert!(
        !text.contains("fields") && !text.contains("state"),
        "{text}"
    );

    // Wren, connected while Unassigned, gets a view with no units and their own card only.
    let wren = FleetHub::new(Arc::new(FleetViewStore::new(None)), key(7));
    wren.set_member(Some(key(7).to_string()));
    wren.set_structure(Some(full.view_for(&key(7).to_string())));
    wren.receive(card(&key(7).to_string(), Some(Life::Up)));
    let m = wren.model();
    assert_eq!(m.layout, Layout::Event);
    assert!(!m.placed && !m.commander);
    assert!(m.tree.unwrap().units.is_empty());
    assert!(m.unassigned.is_empty());
    assert!(m.members[0].me);
}

fn ship_card(unit: &str, source: fleet_proto::ShipSource, class: &str) -> super::card::Card {
    use fleet_proto::card::{FieldValue, Fields, Tone};
    super::card::Card {
        schema: super::card::CARD_SCHEMA,
        member: fleet_proto::card::ship_card_id(unit),
        handle: "Bounty".to_owned(),
        sharing: Sharing {
            ship: true,
            ..Sharing::default()
        },
        state: None,
        fields: Fields(vec![
            ("ship.class", FieldValue::text(class, Tone::Info)),
            ("ship.hull", FieldValue::text("80%", Tone::Ok)),
        ]),
        source: Some(source),
    }
}

#[tokio::test(start_paused = true)]
async fn ship_cards_show_their_source_grey_out_when_stale_and_flag_crew_not_aboard() {
    use fleet_proto::card::{FieldValue, Tone};
    use fleet_proto::{ShipSource, SourceKind};
    let full = event_with_unassigned();
    let hub = FleetHub::new(Arc::new(FleetViewStore::new(None)), key(20));
    hub.set_member(Some(key(10).to_string()));
    hub.set_structure(Some(full.view_for(&key(10).to_string())));
    let source = |kind, handle: &str, role, age_ms| ShipSource {
        kind,
        member: Some(key(10).to_string()),
        handle: Some(handle.to_owned()),
        role,
        age_ms,
    };
    let label = |hub: &FleetHub| {
        let m = hub.model();
        let ship = m.members.iter().find(|x| x.ship).unwrap().clone();
        (ship.source_label.unwrap(), ship.status)
    };
    hub.receive(ship_card(
        "bounty",
        source(SourceKind::Pilot, "Rook", Some(Role::Pilot), None),
        "DRAK_Cutlass",
    ));
    assert_eq!(label(&hub), ("via Rook (Pilot)".to_owned(), Status::Up));
    hub.receive(ship_card(
        "bounty",
        source(SourceKind::StandingIn, "Kestrel", Some(Role::Crew), None),
        "DRAK_Cutlass",
    ));
    assert_eq!(label(&hub).0, "via Kestrel, standing in");
    hub.receive(ship_card(
        "bounty",
        source(SourceKind::Stale, "Kestrel", Some(Role::Crew), Some(4_000)),
        "DRAK_Cutlass",
    ));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        label(&hub),
        ("no report for 6 s".to_owned(), Status::Offline)
    );
    let mut offline = ShipSource::offline();
    hub.receive(ship_card("bounty", offline.clone(), "DRAK_Cutlass"));
    assert_eq!(label(&hub), ("OFFLINE".to_owned(), Status::Offline));
    offline.kind = SourceKind::NotShared;
    hub.receive(ship_card("bounty", offline, "DRAK_Cutlass"));
    assert_eq!(
        label(&hub),
        ("ship status not shared".to_owned(), Status::Unknown)
    );
    tokio::time::advance(STALE_AFTER).await;
    assert_eq!(
        label(&hub),
        ("OFFLINE".to_owned(), Status::Offline),
        "nothing came"
    );

    hub.receive(ship_card(
        "bounty",
        source(SourceKind::Pilot, "Rook", Some(Role::Pilot), None),
        "DRAK_Cutlass",
    ));
    let mut elsewhere = card(&key(11).to_string(), Some(Life::Up));
    elsewhere
        .fields
        .0
        .push(("ship.class", FieldValue::text("ANVL_Arrow", Tone::Info)));
    hub.receive(elsewhere);
    let m = hub.model();
    assert_eq!(m.rollup.total, 1, "a ship card is not a member");
    let bounty = &m.tree.unwrap().units[0].units[0];
    assert_eq!(bounty.ship_card.as_deref(), Some("ship:bounty"));
    assert!(!bounty.fighter, "a fleet's ship keeps its ship card");
    assert_eq!(bounty.rollup.text, "1/2 Crew up, 1 offline");
    let flags: Vec<_> = bounty
        .seats
        .iter()
        .map(|s| (s.role.label, s.not_aboard))
        .collect();
    assert_eq!(flags, [("Pilot", false), ("Crew", true)]);
}

#[tokio::test(start_paused = true)]
async fn a_ship_card_shows_its_sources_own_state_when_downed_or_dead() {
    use fleet_proto::{ShipSource, SourceKind};
    let full = event_with_unassigned();
    let hub = FleetHub::new(Arc::new(FleetViewStore::new(None)), key(20));
    hub.set_member(Some(key(10).to_string()));
    hub.set_structure(Some(full.view_for(&key(10).to_string())));
    let pilot = ShipSource {
        kind: SourceKind::Pilot,
        member: Some(key(10).to_string()),
        handle: Some("Rook".to_owned()),
        role: Some(Role::Pilot),
        age_ms: None,
    };
    let ship = |hub: &FleetHub| {
        let m = hub.model();
        let ship = m.members.iter().find(|x| x.ship).unwrap().clone();
        (ship.source_label.unwrap(), ship.status)
    };
    hub.receive(ship_card("bounty", pilot, "DRAK_Cutlass"));
    hub.receive(card(&key(10).to_string(), Some(Life::Dead)));
    assert_eq!(ship(&hub), ("via Rook (Pilot)".to_owned(), Status::Dead));
    hub.receive(card(&key(10).to_string(), Some(Life::Downed)));
    assert_eq!(ship(&hub).1, Status::Downed);
    hub.receive(card(&key(10).to_string(), Some(Life::Respawned)));
    assert_eq!(ship(&hub).1, Status::Up);

    // A stale own card says nothing about the ship now.
    hub.receive(card(&key(10).to_string(), Some(Life::Dead)));
    tokio::time::advance(STALE_AFTER).await;
    hub.receive(ship_card(
        "bounty",
        ShipSource {
            kind: SourceKind::Pilot,
            member: Some(key(10).to_string()),
            handle: Some("Rook".to_owned()),
            role: Some(Role::Pilot),
            age_ms: None,
        },
        "DRAK_Cutlass",
    ));
    assert_eq!(ship(&hub).1, Status::Up);
}

#[test]
fn consent_saved_before_the_new_ship_fields_leaves_them_off() {
    use super::store::ConsentStore;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fleet-consent.json");
    let old = [
        "ship.name",
        "ship.mode",
        "ship.shields",
        "ship.hull",
        "ship.fuel",
        "ship.quantum",
        "ship.cautions",
        "ship.weapons",
        "ship.aboard",
    ];
    std::fs::write(
        &file,
        json!({ "schemaVersion": 1, "fields": old }).to_string(),
    )
    .unwrap();
    let consent = ConsentStore::new(Some(file)).get();
    for id in old {
        assert!(consent.allows(id), "{id}");
    }
    for id in ["ship.class", "ship.crew_role", "ship.seat"] {
        assert!(!consent.allows(id), "{id} is off until chosen");
        let def = fleet_proto::card::field_def(id).unwrap();
        assert_eq!(
            def.consent,
            super::card::Consent::Ship,
            "{id} is in the Ship group"
        );
    }
    let wanted: Vec<_> = super::card::card_fields().map(|f| f.id).collect();
    for id in ["ship.class", "ship.crew_role", "ship.seat"] {
        assert!(wanted.contains(&id), "{id} can be picked for the cards");
    }
}

#[test]
fn backoff_doubles_to_a_cap_with_a_little_taken_off() {
    use super::client::Backoff;
    let secs: Vec<u64> = (0..8).map(|n| Backoff::base(n).as_secs()).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
    let mut b = Backoff::default();
    for n in 0..10 {
        let wait = b.next_wait();
        let base = Backoff::base(n);
        assert!(
            wait <= base && wait >= base * 4 / 5,
            "{wait:?} for {base:?}"
        );
    }
    b.reset();
    assert_eq!(b.attempt(), 0);
}

#[test]
fn the_model_links_to_the_event_page_only_for_a_hosted_fleet_with_a_portal() {
    use super::hub::{EventRef, FleetRef, LinkInfo, LinkState};
    let hub = FleetHub::new(Arc::new(FleetViewStore::new(None)), key(3));
    let link = |hosted: bool, portal: Option<&str>, event: &str| LinkInfo {
        state: LinkState::Online,
        fleet: Some(FleetRef {
            address: "cic.example.org:7443".to_owned(),
            key: key(5),
            fingerprint: key(5).fingerprint(),
            handle: "Vega".to_owned(),
            event: None,
            hosted,
            portal: portal.map(str::to_owned),
        }),
        event: Some(EventRef {
            id: event.to_owned(),
            name: "Op One".to_owned(),
            group: None,
        }),
        ..LinkInfo::default()
    };
    hub.set_link(link(true, Some("https://cic.example.org"), "op-1"));
    let m = hub.model();
    assert_eq!(
        m.event_url.as_deref(),
        Some("https://cic.example.org/events/op-1")
    );
    let text = to_text(&m);
    assert!(
        text.contains(r#""eventUrl":"https://cic.example.org/events/op-1""#),
        "{text}"
    );
    assert!(
        text.contains(r#""portal":"https://cic.example.org""#),
        "{text}"
    );
    hub.set_link(link(true, None, "op-1"));
    assert_eq!(hub.model().event_url, None, "an older connection string");
    hub.set_link(link(false, Some("https://cic.example.org"), "op-1"));
    assert_eq!(hub.model().event_url, None, "peer to peer");
    hub.set_link(link(true, Some("https://cic.example.org"), "Op One"));
    assert_eq!(hub.model().event_url, None, "not an event id");
}
