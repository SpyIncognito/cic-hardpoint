//! fleet-sim members against the test relay from sc-service's tests, seen by a Hardpoint
//! service as the event's admiral; then a restart that rejoins by key. The run against a real
//! CIC relay is the ignored test at the end.

#[path = "../../../crates/sc-service/tests/support/test_relay.rs"]
mod test_relay;

use std::sync::Arc;
use std::time::Duration;

use fleet_proto::structure::{EventInfo, EventState, EventStructure, Seat, Unit, UnitKind};
use fleet_proto::{Identity, Life, PublicKey, Role};
use fleet_sim::recording::Recording;
use fleet_sim::{Options, Sim};
use sc_core::engine::EngineOptions;
use sc_game::{GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::fleet::LinkState;
use sc_service::fleet::identity::{KEY_FILE, load_or_create};
use sc_service::{Service, ServiceOptions};
use serde_json::json;
use test_relay::{Options as RelayOptions, TestRelay};

fn recording() -> Arc<Recording> {
    let lines: Vec<String> = (0..40)
        .map(|i| {
            json!({ "t": i * 250, "frame": {
                "vehicle": { "state": { "name": "Ship", "health": 0.5 + f64::from(i) / 100.0 } },
                "actor": { "vitals": { "health": { "current": 90, "max": 100 } } },
            } })
            .to_string()
        })
        .collect();
    Arc::new(Recording::read(lines.join("\n").as_bytes(), 250.0).unwrap())
}

async fn viewer(dir: &std::path::Path) -> Service {
    let frames = vec![RecordedFrame {
        t: 0.0,
        frame: json!({ "actor": { "vitals": { "health": { "current": 100, "max": 100 } } } }),
    }];
    let game = ReplayGame::new(
        frames,
        ReplayOptions {
            loop_playback: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    Service::start(ServiceOptions {
        port: 0,
        settings_dir: Some(dir.to_owned()),
        bus_file: dir.join("bus.json"),
        game: Some(Arc::new(game) as Arc<dyn GameApi>),
        engine: EngineOptions::default(),
        fleet_connector: None,
        relay: sc_service::relay::RelayOptions::default(),
    })
    .await
    .unwrap()
}

/// Squadron "Flight 1" in wing "Air wing", one ship per member as its pilot.
fn event(admiral: PublicKey, members: Vec<PublicKey>) -> EventStructure {
    let unit = |id: String, kind, name: &str, parent: Option<&str>, seats| Unit {
        id,
        kind,
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        lead: vec![],
        seats,
    };
    let mut units = vec![
        unit("air".to_owned(), UnitKind::Wing, "Air wing", None, vec![]),
        unit(
            "f1".to_owned(),
            UnitKind::Squadron,
            "Flight 1",
            Some("air"),
            vec![],
        ),
    ];
    for (n, member) in members.iter().enumerate() {
        let seat = Seat {
            member: member.to_string(),
            role: Role::Pilot,
        };
        units.push(unit(
            format!("f1-{n}"),
            UnitKind::Ship,
            "Ship",
            Some("f1"),
            vec![seat],
        ));
    }
    EventStructure {
        event: EventInfo {
            id: "demo".to_owned(),
            name: "Demo op".to_owned(),
            state: EventState::Open,
        },
        admiral: Some(admiral.to_string()),
        units,
        unassigned: vec![],
    }
}

async fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    for _ in 0..300 {
        if let Some(v) = check() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn members_join_are_seen_by_the_admiral_and_rejoin_by_key_after_a_restart() {
    const COUNT: usize = 4;
    let data = tempfile::tempdir().unwrap();
    // The members' keys, made where the sim keeps them, so the event can place them.
    let keys: Vec<PublicKey> = (1..=COUNT)
        .map(|n| {
            let dir = data.path().join(format!("member-{n:02}"));
            load_or_create(Some(&dir.join(KEY_FILE))).unwrap().public()
        })
        .collect();
    let viewer_dir = tempfile::tempdir().unwrap();
    let viewer_key = load_or_create(Some(&viewer_dir.path().join(KEY_FILE)))
        .unwrap()
        .public();
    let structure = event(viewer_key, keys.clone());
    let relay = TestRelay::start(RelayOptions {
        structure: Box::new(move |_| Some(structure.clone())),
        ..RelayOptions::default()
    })
    .await;

    let start = || {
        let mut options = Options::new(
            relay.invite("code-1"),
            vec![recording()],
            data.path().to_owned(),
        );
        options.count = COUNT;
        options.cycle = Duration::from_secs(3);
        options.frame_every = Duration::from_millis(100);
        Sim::start(options).unwrap()
    };
    let sim = start();
    assert_eq!(sim.members().iter().filter(|m| m.scripted).count(), 2);
    until("every member online", || {
        (sim.counts().online == COUNT).then_some(())
    })
    .await;
    assert_eq!(
        relay.seen().joins,
        vec![Some("code-1".to_owned()); COUNT],
        "first joins carry the code"
    );

    let service = viewer(viewer_dir.path()).await;
    let client = service.fleet_client();
    client.join(&relay.invite("code-1"), "Viewer").unwrap();
    let model = until("the admiral sees every member in Flight 1", || {
        let model = service.fleet().model();
        let group = model.groups.iter().find(|g| g.name == "Flight 1")?;
        (group.members.len() == COUNT).then_some(model)
    })
    .await;
    assert!(model.admiral_allowed);
    assert_eq!(service.fleet().link().state, LinkState::Online);
    let mut handles: Vec<String> = model
        .members
        .iter()
        .filter(|m| !m.me)
        .map(|m| m.card.handle.clone())
        .collect();
    handles.sort();
    assert_eq!(
        handles,
        ["Demo Kestrel", "Demo Rook", "Demo Vega", "Demo Wren"]
    );
    let card = &model.members.iter().find(|m| !m.me).unwrap().card;
    // Demo members share detail too; it is sent only while a viewer asks for it.
    assert!(card.sharing.ship && card.sharing.character && card.sharing.detail);
    assert!(card.fields.ids().any(|id| id == "ship.hull"));
    assert!(!card.fields.ids().any(|id| id.starts_with("detail.")));

    // The script reaches the viewer as card states, from the game events.
    for life in [Life::Downed, Life::Dead, Life::Respawned] {
        until(&format!("a member {life:?}"), || {
            service
                .fleet()
                .model()
                .members
                .iter()
                .any(|m| !m.me && m.card.state == Some(life))
                .then_some(())
        })
        .await;
    }

    // A restart: the same keys, no code, the same members.
    sim.close().await;
    assert!(
        relay.seen().byes.iter().all(|b| b.is_empty()),
        "nobody left"
    );
    let joins_before = relay.seen().joins.len();
    let sim = start();
    until("every member online again", || {
        (sim.counts().online == COUNT).then_some(())
    })
    .await;
    let joins = relay.seen().joins.clone();
    let rejoins = joins[joins_before..].iter().filter(|c| c.is_none()).count();
    assert_eq!(rejoins, COUNT, "{joins:?}");
    let mut again: Vec<PublicKey> = sim
        .members()
        .iter()
        .map(fleet_sim::Member::public_key)
        .collect();
    again.sort();
    let mut keys = keys;
    keys.sort();
    assert_eq!(again, keys);
    sim.close().await;
    service.close().await;
}

#[test]
fn a_member_key_is_kept_in_its_folder() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let data = tempfile::tempdir().unwrap();
    let options = Options::new(
        format!(
            "cic://127.0.0.1:1?key={}&code=c",
            Identity::generate().unwrap().public()
        ),
        vec![recording()],
        data.path().to_owned(),
    );
    let sim = Sim::start(options).unwrap();
    assert_eq!(sim.members().len(), 8);
    assert!(data.path().join("member-08").join(KEY_FILE).exists());
    let consent =
        std::fs::read_to_string(data.path().join("member-01/fleet-consent.json")).unwrap();
    assert!(consent.contains("ship.hull") && consent.contains("detail."));
    rt.block_on(sim.close());
}
