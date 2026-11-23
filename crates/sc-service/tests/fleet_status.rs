//! The bus's `fleet.status` capability against the fleet panel's own model: the same roll-ups as
//! the panel tree, only what the viewer's role covers, a peer-to-peer session with no structure,
//! and no way to act.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use fleet_proto::card::Tone;
use fleet_proto::structure::{EventInfo, EventState, Seat, Unit, UnitKind};
use fleet_proto::{
    CARD_SCHEMA, Card, EventStructure, FieldValue, Fields, Identity, Life, Role, RosterEntry,
    Sharing,
};
use sc_core::engine::EngineOptions;
use sc_game::{GameAction, GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::fleet::host;
use sc_service::fleet::hub::EventRef;
use sc_service::fleet::tree::UnitNode;
use sc_service::fleet::{FleetHub, LinkInfo, LinkState};
use sc_service::{Service, ServiceOptions, busfile};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Rig {
    service: Service,
    game: Arc<ReplayGame>,
    dir: tempfile::TempDir,
}

impl Rig {
    async fn start() -> Self {
        let frame =
            json!({ "vehicle": { "state": { "name": "Test ship", "lights_enabled": true } } });
        let frames = (0..50)
            .map(|i| RecordedFrame {
                t: f64::from(i) * 20.0,
                frame: frame.clone(),
            })
            .collect();
        let options = ReplayOptions {
            loop_playback: true,
            actions: vec![GameAction {
                name: "v_lights".into(),
                category: "test".into(),
                holdable: false,
            }],
            ..ReplayOptions::default()
        };
        let game = Arc::new(ReplayGame::new(frames, options).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let service = Service::start(ServiceOptions {
            port: 0,
            settings_dir: Some(dir.path().to_owned()),
            bus_file: dir.path().join("bus.json"),
            game: Some(Arc::clone(&game) as Arc<dyn GameApi>),
            engine: EngineOptions::default(),
            fleet_connector: None,
            // Peer-to-peer hosting listens on the relay's bind.
            relay: sc_service::relay::RelayOptions {
                bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
                ..sc_service::relay::RelayOptions::default()
            },
        })
        .await
        .unwrap();
        Self { service, game, dir }
    }

    fn hub(&self) -> &Arc<FleetHub> {
        self.service.fleet()
    }

    async fn reader(&self, capabilities: Value) -> Socket {
        let url = format!("ws://127.0.0.1:{}", self.service.bus_port());
        let mut ws = tokio_tungstenite::connect_async(url).await.unwrap().0;
        let token = busfile::read(&self.dir.path().join("bus.json"))
            .unwrap()
            .unwrap()
            .token;
        send(
            &mut ws,
            &json!({ "type": "hello", "token": token, "version": 1, "client": "test", "capabilities": capabilities }),
        )
        .await;
        let welcome = next(&mut ws).await.unwrap();
        assert_eq!(welcome["type"], "welcome");
        ws
    }
}

async fn send(ws: &mut Socket, msg: &Value) {
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();
}

async fn next(ws: &mut Socket) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .ok()??
        {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// The next `fleet.status` push that `found` accepts; panics on anything else in between.
async fn status_until(ws: &mut Socket, found: impl Fn(&Value) -> bool) -> Value {
    loop {
        let m = next(ws).await.expect("no fleet.status push came");
        assert_eq!(m["type"], "fleet.status", "only fleet.status pushes: {m}");
        if found(&m["status"]) {
            return m["status"].clone();
        }
    }
}

fn key(n: u8) -> String {
    Identity::from_seed([n; 32]).public().to_string()
}

fn card(n: u8, life: Life, health: Option<f64>) -> Card {
    let fields = health.map_or_else(Vec::new, |h| {
        vec![(
            "char.health",
            FieldValue {
                text: format!("{:.0}%", h * 100.0),
                tone: Tone::Ok,
                fraction: Some(h),
                items: None,
            },
        )]
    });
    Card {
        schema: CARD_SCHEMA,
        member: key(n),
        handle: format!("Pilot{n}"),
        sharing: Sharing::ALL,
        state: Some(life),
        fields: Fields(fields),
        source: None,
    }
}

/// Admiral 1. Wing "air" (commander 2) with squadrons f1 (pilots 2 and 3) and f2 (pilots 4 and
/// 5). Ground "army" with squad "ground" (soldier 6). Key 7 is Unassigned.
fn event() -> EventStructure {
    let unit = |id: &str, kind, parent: Option<&str>, lead: &[u8], seats: &[(u8, Role)]| Unit {
        id: id.to_owned(),
        kind,
        name: id.to_uppercase(),
        parent: parent.map(str::to_owned),
        lead: lead.iter().map(|n| key(*n)).collect(),
        seats: seats
            .iter()
            .map(|(n, role)| Seat {
                member: key(*n),
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
    EventStructure {
        event: EventInfo {
            id: "op".to_owned(),
            name: "Op".to_owned(),
            state: EventState::Open,
        },
        admiral: Some(key(1)),
        units: vec![
            unit("air", UnitKind::Wing, None, &[2], &[]),
            unit("f1", UnitKind::Squadron, Some("air"), &[2], &[]),
            ship("f1-2", "f1", 2),
            ship("f1-3", "f1", 3),
            unit("f2", UnitKind::Squadron, Some("air"), &[4], &[]),
            ship("f2-4", "f2", 4),
            ship("f2-5", "f2", 5),
            unit("army", UnitKind::Ground, None, &[6], &[]),
            unit(
                "ground",
                UnitKind::Squad,
                Some("army"),
                &[],
                &[(6, Role::Soldier)],
            ),
        ],
        unassigned: vec![key(7)],
    }
}

fn online() -> LinkInfo {
    LinkInfo {
        state: LinkState::Online,
        ..LinkInfo::default()
    }
}

/// Every unit of the panel tree, depth first, as (scope id, open, roll-up).
fn tree_rollups(units: &[UnitNode], out: &mut Vec<(String, bool, Value)>) {
    for u in units {
        out.push((
            format!("unit:{}", u.id),
            u.open,
            serde_json::to_value(&u.rollup).unwrap(),
        ));
        tree_rollups(&u.units, out);
    }
}

fn scope<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scope {id}"))
}

#[tokio::test]
async fn a_reader_sees_the_panels_roll_ups_and_only_what_its_role_covers() {
    let rig = Rig::start().await;
    let mut ws = rig.reader(json!(["fleet.status"])).await;
    let first = status_until(&mut ws, |_| true).await;
    assert_eq!(first["link"], "none");
    assert_eq!(first["scopes"], json!([]), "no fleet, nothing counted");

    // Key 3, a pilot in squadron f1, sees f1 and nothing of the ground force.
    let hub = rig.hub();
    hub.set_member(Some(key(3)));
    hub.set_structure(Some(event().view_for(&key(3))));
    for (n, life, health) in [
        (2, Life::Up, Some(0.7)),
        (3, Life::Downed, Some(0.1)),
        (1, Life::Dead, None),
    ] {
        assert!(hub.receive(card(n, life, health)));
    }
    hub.set_link(online());
    let status = status_until(&mut ws, |s| {
        s["link"] == "online" && s["members"].as_array().unwrap().len() >= 3
    })
    .await;

    let model = hub.model();
    let tree = model.tree.as_ref().unwrap();
    let fleet = scope(&status, "fleet");
    assert_eq!(fleet["depth"], 0);
    assert_eq!(fleet["rollup"], serde_json::to_value(&tree.rollup).unwrap());
    let mut expected = vec![];
    tree_rollups(&tree.units, &mut expected);
    let got: Vec<(String, bool, Value)> = status["scopes"].as_array().unwrap()[1..]
        .iter()
        .map(|s| {
            let open = s["open"].as_bool().unwrap();
            (
                s["id"].as_str().unwrap().to_owned(),
                open,
                s["rollup"].clone(),
            )
        })
        .collect();
    let opened: Vec<_> = expected.iter().filter(|(_, open, _)| *open).collect();
    let got_open: Vec<_> = got.iter().filter(|(_, open, _)| *open).collect();
    assert_eq!(
        got_open, opened,
        "open units carry the panel tree's roll-ups"
    );
    assert_eq!(
        got.iter()
            .map(|(id, open, _)| (id, open))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|(id, open, _)| (id, open))
            .collect::<Vec<_>>(),
    );

    let f1 = scope(&status, "unit:f1");
    assert_eq!(f1["rollup"]["text"], "1/2 Pilots up, 1 downed");
    assert_eq!(
        f1["down"],
        json!([{ "member": key(3), "handle": "Pilot3", "status": "DOWNED" }])
    );
    let air = scope(&status, "unit:air");
    assert_eq!(air["open"], false, "the wing above shows nothing");
    assert_eq!(air["rollup"]["total"], 0);
    assert_eq!(air["down"], json!([]));
    assert!(
        status["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != "unit:army" && s["id"] != "unit:f2"),
        "nothing outside the viewer's tree"
    );

    let member = |id: &str| {
        status["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["member"] == id)
            .cloned()
    };
    let two = member(&key(2)).unwrap();
    assert_eq!(two["status"], "UP");
    assert_eq!(two["health"], 0.7);
    assert_eq!(two["me"], false);
    assert_eq!(member(&key(3)).unwrap()["me"], true);
    assert!(member(&key(4)).is_none(), "no card from another squadron");

    // The refresh call answers with the same status.
    send(
        &mut ws,
        &json!({ "type": "call", "id": 1, "call": { "op": "fleet.status" } }),
    )
    .await;
    let reply = loop {
        let m = next(&mut ws).await.unwrap();
        if m["type"] == "result" {
            break m;
        }
    };
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["scopes"], status["scopes"]);

    // Unassigned: only their own card counts.
    hub.set_member(Some(key(7)));
    hub.set_structure(Some(event().view_for(&key(7))));
    assert!(hub.receive(card(7, Life::Dead, None)));
    let status = status_until(&mut ws, |s| s["placed"] == false).await;
    let scopes = status["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0]["rollup"]["total"], 1);
    assert_eq!(scopes[0]["rollup"]["dead"], 1);
    assert_eq!(
        status["members"],
        json!([{ "member": key(7), "handle": "Pilot7", "status": "DEAD", "health": null, "me": true }])
    );

    hub.set_link(LinkInfo {
        state: LinkState::Offline,
        ..LinkInfo::default()
    });
    let status = status_until(&mut ws, |s| s["link"] == "offline").await;
    assert_eq!(status["scopes"], json!([]));
    assert_eq!(status["members"], json!([]));
    assert_eq!(status["event"], Value::Null);
    rig.service.close().await;
}

#[tokio::test]
async fn a_reader_cannot_act_and_gets_no_game_frames() {
    let rig = Rig::start().await;
    let mut ws = rig.reader(json!(["fleet.status"])).await;
    status_until(&mut ws, |_| true).await;
    send(
        &mut ws,
        &json!({ "type": "call", "id": 7, "call": { "op": "press", "action": "v_lights" } }),
    )
    .await;
    let mut seen = vec![];
    let until = tokio::time::Instant::now() + Duration::from_millis(600);
    while let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout_at(until, ws.next()).await {
        seen.push(serde_json::from_str::<Value>(&t).unwrap());
    }
    let reply = seen.iter().find(|m| m["type"] == "result").unwrap();
    assert_eq!(reply["id"], 7);
    assert!(reply["error"].is_string() && reply.get("result").is_none());
    assert!(
        seen.iter()
            .all(|m| m["type"] == "result" || m["type"] == "fleet.status"),
        "no state, link or progress: {seen:?}"
    );
    assert!(
        rig.game.trigger_calls().is_empty(),
        "nothing reached the game"
    );

    // A client without the capability is served as before.
    let mut plain = rig.reader(json!([])).await;
    let m = next(&mut plain).await.unwrap();
    assert_ne!(m["type"], "fleet.status");
    rig.service.close().await;
}

/// A peer-to-peer session as the client leaves the hub: no structure, the session `joined` named,
/// the host's roster, and the viewer's own groups.
#[tokio::test]
async fn a_reader_of_a_peer_to_peer_session_sees_the_session_and_its_own_groups() {
    let rig = Rig::start().await;
    let mut ws = rig.reader(json!(["fleet.status"])).await;
    status_until(&mut ws, |_| true).await;

    let hub = rig.hub();
    hub.set_member(Some(key(1)));
    let roster = |ns: &[u8]| {
        ns.iter()
            .map(|n| RosterEntry {
                member: key(*n),
                handle: Some(format!("Pilot{n}")),
                role: None,
                unit: None,
                key: None,
            })
            .collect::<Vec<_>>()
    };
    hub.set_roster(roster(&[1, 2, 3, 4]));
    for (n, life) in [
        (1, Life::Up),
        (2, Life::Downed),
        (3, Life::Up),
        (4, Life::Dead),
    ] {
        assert!(hub.receive(card(n, life, None)));
    }
    let group = hub
        .view()
        .add_group(Some(&json!("Wingmen")))
        .unwrap()
        .group
        .unwrap();
    for n in [2, 3] {
        assert!(
            hub.view()
                .assign(&key(n), Some(&group.id), None)
                .unwrap()
                .problems
                .is_empty()
        );
    }
    hub.set_link(LinkInfo {
        state: LinkState::Online,
        server: Some("Vega".to_owned()),
        event: Some(EventRef {
            id: "p2p-7".to_owned(),
            name: "Vega's fleet".to_owned(),
            group: None,
        }),
        ..LinkInfo::default()
    });
    let status = status_until(&mut ws, |s| {
        s["link"] == "online" && s["scopes"].as_array().is_some_and(|a| a.len() == 2)
    })
    .await;

    assert_eq!(
        status["event"],
        json!({ "id": "p2p-7", "name": "Vega's fleet" })
    );
    assert_eq!(status["placed"], true);
    let fleet = scope(&status, "fleet");
    assert_eq!(
        fleet["name"], "Vega's fleet",
        "the whole session is the scope"
    );
    assert_eq!(fleet["kind"], "Fleet");
    assert_eq!(
        fleet["rollup"],
        serde_json::to_value(&hub.model().rollup).unwrap()
    );
    assert_eq!(fleet["rollup"]["total"], 4);
    assert_eq!(
        fleet["down"],
        json!([
            { "member": key(2), "handle": "Pilot2", "status": "DOWNED" },
            { "member": key(4), "handle": "Pilot4", "status": "DEAD" },
        ])
    );
    let wingmen = scope(&status, &format!("group:{}", group.id));
    assert_eq!(wingmen["name"], "Wingmen");
    assert_eq!(wingmen["kind"], "Group");
    assert_eq!(wingmen["depth"], 1);
    assert_eq!(wingmen["rollup"]["total"], 2);
    assert_eq!(
        wingmen["down"],
        json!([{ "member": key(2), "handle": "Pilot2", "status": "DOWNED" }])
    );
    assert_eq!(status["members"].as_array().unwrap().len(), 4);

    // The host kicks 2: the roster drops them and so does every scope.
    hub.set_roster(roster(&[1, 3, 4]));
    let status = status_until(&mut ws, |s| s["members"].as_array().unwrap().len() == 3).await;
    assert_eq!(scope(&status, "fleet")["rollup"]["total"], 3);
    let wingmen = scope(&status, &format!("group:{}", group.id));
    assert_eq!(wingmen["rollup"]["total"], 1);
    assert_eq!(wingmen["down"], json!([]));
    rig.service.close().await;
}

/// The host's own Hardpoint joins its session over loopback like any member, so its reader sees
/// the session, named after the host, as the whole fleet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hosts_own_reader_sees_its_session_as_the_whole_fleet() {
    let rig = Rig::start().await;
    let mut ws = rig.reader(json!(["fleet.status"])).await;
    status_until(&mut ws, |_| true).await;
    let (fleet_host, client) = (rig.service.fleet_host(), rig.service.fleet_client());
    host::start_hosting(fleet_host, client, 0, "Vega")
        .await
        .unwrap();
    let status = tokio::time::timeout(
        Duration::from_secs(10),
        status_until(&mut ws, |s| s["link"] == "online"),
    )
    .await
    .expect("the host's own Hardpoint joined");
    let session = status["event"].clone();
    assert_eq!(session["name"], "Vega's fleet");
    assert!(session["id"].as_str().unwrap().starts_with("p2p-"));
    assert_eq!(status["placed"], true);
    let fleet = scope(&status, "fleet");
    assert_eq!(fleet["name"], "Vega's fleet");
    assert_eq!(fleet["kind"], "Fleet");
    assert!(
        status["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] == "fleet" || s["id"].as_str().unwrap().starts_with("group:")),
        "no units: the host sends no structure"
    );
    host::stop_hosting(fleet_host, client).await;
    rig.service.close().await;
}
