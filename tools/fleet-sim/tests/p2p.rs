//! The v0.1.0 peer-to-peer gate end to end on loopback: a Hardpoint service hosting (its own
//! Hardpoint joined over loopback), fleet-sim members joining with the invite, cards flowing
//! with each sender's consent applied, then Remove, Kick (a ban and a new code), an old code
//! refused, a rejoin by key after the new code, and a ban lifted.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use fleet_proto::RefusalCode;
use fleet_sim::recording::Recording;
use fleet_sim::{Options, Sim};
use sc_core::engine::EngineOptions;
use sc_game::{GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::fleet::LinkState;
use sc_service::fleet::host;
use sc_service::{Service, ServiceOptions};
use serde_json::{Value, json};

const COUNT: usize = 4;

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

async fn hardpoint(dir: &std::path::Path) -> Service {
    let frames = vec![RecordedFrame {
        t: 0.0,
        frame: json!({
            "vehicle": { "state": { "name": "Host ship", "health": 0.9 } },
            "actor": { "vitals": { "health": { "current": 100, "max": 100 } } },
        }),
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
        relay: sc_service::relay::RelayOptions {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ..sc_service::relay::RelayOptions::default()
        },
    })
    .await
    .unwrap()
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

fn sim(invite: String, data: &std::path::Path, count: usize, consent: &str) -> Sim {
    let mut options = Options::new(invite, vec![recording()], data.to_owned());
    options.count = count;
    options.frame_every = Duration::from_millis(100);
    options.cycle = Duration::from_secs(600);
    options.scripted = Some(0);
    options.consent = fleet_sim::steps::parse_consent(consent).unwrap();
    Sim::start(options).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hardpoint_host_and_four_members_with_consent_kick_remove_and_a_new_code() {
    let host_dir = tempfile::tempdir().unwrap();
    let service = hardpoint(host_dir.path()).await;
    let (fleet_host, client) = (service.fleet_host(), service.fleet_client());
    client
        .set_consent(Some(&json!(["ship.hull", "ship.name"])))
        .unwrap();
    host::start_hosting(fleet_host, client, 0, "Host")
        .await
        .unwrap();
    let host_key = fleet_host.key().to_string();
    until("the host's own Hardpoint joined", || {
        (service.fleet().link().state == LinkState::Online).then_some(())
    })
    .await;

    // Members send hull and health, never the ship's name.
    let data = tempfile::tempdir().unwrap();
    let invite = fleet_host.loopback_invite().unwrap();
    let mut members = sim(invite.clone(), data.path(), COUNT, "ship.hull,char.health");
    until("every member online", || {
        (members.counts().online == COUNT).then_some(())
    })
    .await;
    assert_eq!(fleet_host.member_count(), COUNT + 1);

    let keys: Vec<String> = members
        .members()
        .iter()
        .map(|m| m.public_key().to_string())
        .collect();
    // Everyone sees everyone: each member the host and the three others.
    until("every member sees the other four", || {
        members
            .members()
            .iter()
            .enumerate()
            .all(|(n, m)| {
                let seen: BTreeSet<String> = m.received().into_iter().map(|(k, _)| k).collect();
                let want: BTreeSet<String> = keys
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != n)
                    .map(|(_, k)| k.clone())
                    .chain([host_key.clone()])
                    .collect();
                seen == want
            })
            .then_some(())
    })
    .await;
    // The host's panel has the four, and none carries a field its sender did not consent to.
    let model = until("the host sees four members with hull", || {
        let model = service.fleet().model();
        let others: Vec<_> = model.members.iter().filter(|m| !m.me).collect();
        (others.len() == COUNT
            && others
                .iter()
                .all(|m| m.card.fields.get("ship.hull").is_some()))
        .then_some(model)
    })
    .await;
    for m in model.members.iter().filter(|m| !m.me) {
        assert!(
            m.card.fields.get("ship.name").is_none(),
            "unconsented ship.name"
        );
        assert!(
            m.card
                .fields
                .ids()
                .all(|f| ["ship.hull", "char.health"].contains(&f))
        );
    }
    // No roles or tree on the wire: the panel draws the viewer's own groups.
    assert!(model.tree.is_none());
    assert!(model.link.grants.is_empty());
    // And the host's card at the members carries only what the host consented to.
    let line: Value = serde_json::from_str(&members.received_line(0)).unwrap();
    for entry in line["members"].as_array().unwrap() {
        let fields: Vec<&str> = entry["received"][&host_key]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        assert!(
            fields
                .iter()
                .all(|f| ["ship.hull", "ship.name"].contains(f)),
            "{fields:?}"
        );
        assert!(!fields.contains(&"char.health"));
        assert_eq!(entry["link"], "online");
    }

    // Remove member 3: dropped, may join again with the current code.
    let code = fleet_host.code().unwrap();
    fleet_host.remove(&keys[2]).unwrap();
    until("member 3 removed", || {
        (members.members()[2].refusal() == Some(RefusalCode::Kicked)).then_some(())
    })
    .await;
    assert_eq!(fleet_host.code().unwrap(), code);
    members
        .join_member(2, &fleet_host.loopback_invite().unwrap())
        .unwrap();
    until("member 3 back", || {
        (members.members()[2].link() == LinkState::Online).then_some(())
    })
    .await;

    // Kick member 2: banned and a new code; the others stay.
    let old_invite = fleet_host.loopback_invite().unwrap();
    fleet_host.kick(&keys[1]).unwrap();
    until("member 2 kicked", || {
        (members.members()[1].refusal() == Some(RefusalCode::Banned)).then_some(())
    })
    .await;
    assert_ne!(fleet_host.code().unwrap(), code, "kick replaces the code");
    until(
        "the kicked card gone from the host's panel and the members'",
        || {
            let host_has = service
                .fleet()
                .model()
                .members
                .iter()
                .any(|m| m.card.member == keys[1]);
            let member_has = members.members()[0]
                .received()
                .iter()
                .any(|(k, _)| *k == keys[1]);
            (!host_has && !member_has).then_some(())
        },
    )
    .await;
    assert_eq!(fleet_host.bans().len(), 1);
    tokio::time::sleep(Duration::from_millis(600)).await;
    for n in [0, 2, 3] {
        assert_eq!(
            members.members()[n].link(),
            LinkState::Online,
            "member {}",
            n + 1
        );
    }
    // The kicked key with the current invite is still refused.
    members
        .join_member(1, &fleet_host.loopback_invite().unwrap())
        .unwrap();
    until("member 2 banned again", || {
        (members.members()[1].link() == LinkState::Refused
            && members.members()[1].refusal() == Some(RefusalCode::Banned))
        .then_some(())
    })
    .await;

    // The old code is dead for a new member...
    let late = tempfile::tempdir().unwrap();
    let newcomer = sim(old_invite, late.path(), 1, "ship.hull");
    let why = until("the old code refused", || newcomer.all_refused()).await;
    assert_eq!(why, Some(RefusalCode::BadCode));
    newcomer.close().await;
    // ...while member 4, dropped after it, rejoins by key.
    members.drop_member(3).await.unwrap();
    members.rejoin_member(3).unwrap();
    until("member 4 back by key, connected at the host", || {
        let connected = fleet_host
            .view()
            .members
            .iter()
            .any(|m| m.member == keys[3] && m.connected);
        (connected && members.counts().online == 3).then_some(())
    })
    .await;

    // Lift the ban: the member joins with the current code.
    fleet_host.lift(&keys[1]).unwrap();
    members
        .join_member(1, &fleet_host.loopback_invite().unwrap())
        .unwrap();
    until("member 2 back after the lift", || {
        (members.members()[1].link() == LinkState::Online).then_some(())
    })
    .await;
    until("every member online at the end", || {
        (members.counts().online == COUNT).then_some(())
    })
    .await;

    members.close().await;
    host::stop_hosting(fleet_host, client).await;
    assert!(!fleet_host.is_hosting());
    service.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eight_members_hold_a_session_against_a_hardpoint_host() {
    let host_dir = tempfile::tempdir().unwrap();
    let service = hardpoint(host_dir.path()).await;
    let (fleet_host, client) = (service.fleet_host(), service.fleet_client());
    host::start_hosting(fleet_host, client, 0, "Host")
        .await
        .unwrap();
    let data = tempfile::tempdir().unwrap();
    let members = sim(fleet_host.loopback_invite().unwrap(), data.path(), 8, "all");
    until("eight online", || {
        (members.counts().online == 8).then_some(())
    })
    .await;
    until("the host sees eight cards", || {
        (service
            .fleet()
            .model()
            .members
            .iter()
            .filter(|m| !m.me)
            .count()
            == 8)
            .then_some(())
    })
    .await;
    // Held: still eight, every member seeing the other seven and the host.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(members.counts().online, 8);
    assert!(members.members().iter().all(|m| m.received().len() == 8));
    let ages: Vec<u64> = service
        .fleet()
        .model()
        .members
        .iter()
        .filter(|m| !m.me)
        .map(|m| m.age_ms)
        .collect();
    assert!(ages.iter().all(|a| *a < 3_000), "fresh cards: {ages:?}");
    members.close().await;
    host::stop_hosting(fleet_host, client).await;
    service.close().await;
}
