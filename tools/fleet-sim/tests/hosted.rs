//! fleet-sim members against a hosted relay: connection tokens, `events` and `choose`, the new
//! ship fields on the cards, reconnecting by key after a drop and a restart, the pilot drop,
//! and the refusals.

#[path = "../../../crates/sc-service/tests/support/test_relay.rs"]
mod test_relay;

#[path = "support/hosted_relay.rs"]
mod hosted_relay;

use std::sync::Arc;
use std::time::Duration;

use fleet_proto::{Card, PublicKey};
use fleet_sim::invite::HostedInvite;
use fleet_sim::recording::Recording;
use fleet_sim::scenario::Ship;
use fleet_sim::{Options, Sim};
use hosted_relay::{Account, HostedRelay};
use sc_service::fleet::LinkState;
use serde_json::json;

fn recording() -> Arc<Recording> {
    let lines: Vec<String> = (0..40)
        .map(|i| {
            json!({ "t": i * 250, "frame": {
                "vehicle": { "state": { "name": "Ship", "class_name": "SHIP_CLASS", "health": 0.9 } },
                "actor": {
                    "vitals": { "health": { "current": 90, "max": 100 } },
                    "state": { "crew_role": "ACTOR_CREW_ROLE_PILOT", "seat_name": "Seat" },
                },
            } })
            .to_string()
        })
        .collect();
    Arc::new(Recording::read(lines.join("\n").as_bytes(), 250.0).unwrap())
}

fn account(id: &str, events: &[&str]) -> Account {
    Account {
        id: id.to_owned(),
        handle: format!("Handle {id}"),
        events: events
            .iter()
            .map(|e| ((*e).to_owned(), format!("Op {e}")))
            .collect(),
        kicked: false,
    }
}

fn options(invites: &[String], data: &std::path::Path) -> Options {
    let invites: Vec<HostedInvite> = invites.iter().map(|i| i.parse().unwrap()).collect();
    let mut o = Options::hosted(invites, vec![recording()], data.to_owned());
    o.frame_every = Duration::from_millis(100);
    o.cycle = Duration::from_secs(3);
    o
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

fn text(card: &Card, id: &str) -> String {
    card.fields
        .get(id)
        .map(|v| v.text.clone())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn members_connect_choose_send_ship_fields_and_reconnect_by_key() {
    let relay = HostedRelay::start(vec![
        account("u-a", &["ops-a", "ops-b"]),
        account("u-b", &["ops-b"]),
        account("u-c", &["ops-a", "ops-b"]),
    ])
    .await;
    let invites = [
        relay.invite(0, None),
        relay.invite(1, None),
        relay.invite(2, Some("ops-b")),
    ];
    let data = tempfile::tempdir().unwrap();
    let start = || {
        let mut o = options(&invites, data.path());
        o.choose = Some("ops-b".to_owned());
        o.ships = vec![
            "RSI_Constellation_Andromeda:pilot,copilot,crew"
                .parse::<Ship>()
                .unwrap(),
        ];
        Sim::start(o).unwrap()
    };
    let sim = start();
    until("every member online", || {
        (sim.counts().online == 3).then_some(())
    })
    .await;
    {
        let seen = relay.seen();
        let mut connects = seen.connects.clone();
        connects.sort();
        assert_eq!(
            connects,
            [(true, None), (true, None), (true, Some("ops-b".to_owned()))],
            "the per-event token sends its event"
        );
        assert_eq!(
            seen.chooses,
            ["ops-b"],
            "only the member with two events chose"
        );
        let mut joined = seen.joined.clone();
        joined.sort();
        assert_eq!(
            joined,
            [
                ("u-a".to_owned(), "ops-b".to_owned()),
                ("u-b".to_owned(), "ops-b".to_owned()),
                ("u-c".to_owned(), "ops-b".to_owned())
            ]
        );
    }
    for m in sim.members() {
        let c = m.hosted().unwrap();
        assert!(!c.has_token(), "the token is spent");
        assert_eq!(c.event().as_deref(), Some("ops-b"));
    }
    assert_eq!(sim.members()[0].handle(), "Handle u-a");

    let keys: Vec<PublicKey> = sim.members().iter().map(|m| m.public_key()).collect();
    let expect = [
        (keys[0], "PILOT", "Pilot"),
        (keys[1], "CREW", "Copilot"),
        (keys[2], "GUNNER", "Turret"),
    ];
    let cards: Vec<Card> = until("a card with ship fields from each member", || {
        let seen = relay.seen();
        expect
            .iter()
            .map(|(key, _, _)| {
                seen.cards
                    .iter()
                    .rev()
                    .find(|(k, c)| k == key && !text(c, "ship.class").is_empty())
                    .map(|(_, c)| c.clone())
            })
            .collect()
    })
    .await;
    for (card, (key, role, seat)) in cards.iter().zip(expect) {
        assert_eq!(
            card.member,
            key.to_string(),
            "the key; the relay rewrites it"
        );
        assert_eq!(text(card, "ship.class"), "RSI_Constellation_Andromeda");
        assert_eq!(text(card, "ship.name"), "Constellation Andromeda");
        assert_eq!(text(card, "ship.crew_role"), role);
        assert_eq!(text(card, "ship.seat"), seat);
        assert!(card.source.is_none());
    }

    // A drop: back by key, with the event each was in.
    let before = relay.seen().connects.len();
    relay.drop_connections();
    until("every member back after the drop", || {
        (relay.seen().connects.len() >= before + 3 && sim.counts().online == 3).then_some(())
    })
    .await;
    let again: Vec<_> = relay.seen().connects[before..].to_vec();
    assert!(
        again
            .iter()
            .all(|c| *c == (false, Some("ops-b".to_owned()))),
        "{again:?}"
    );

    // A restart with the same, spent invites: by key again.
    sim.close().await;
    let before = relay.seen().connects.len();
    let sim = start();
    until("every member back after a restart", || {
        (sim.counts().online == 3).then_some(())
    })
    .await;
    assert!(relay.seen().connects[before..].iter().all(|(t, _)| !t));
    for n in 1..=3 {
        let file = data.path().join(format!("member-{n:02}/fleet-hosted.json"));
        let kept = std::fs::read_to_string(file).unwrap();
        assert!(invites.iter().all(|i| {
            let token = i
                .split("connect=")
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap();
            !kept.contains(token)
        }));
    }
    sim.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_are_clear_and_not_retried() {
    let relay =
        HostedRelay::start(vec![account("u-a", &["ops-a"]), account("u-b", &["ops-a"])]).await;
    let shared = relay.invite(0, None);
    let invites = [
        relay.unknown_invite(),
        shared.clone(),
        shared,
        relay.invite(1, None),
    ];
    let data = tempfile::tempdir().unwrap();
    let sim = Sim::start(options(&invites, data.path())).unwrap();
    until("two refused, two online", || {
        let c = sim.counts();
        (c.refused == 2 && c.online == 2).then_some(())
    })
    .await;
    let problem = |n: usize| sim.members()[n].problem().unwrap_or_default();
    let unknown = problem(0);
    assert!(unknown.contains("(token_unknown)"), "{unknown}");
    assert!(unknown.contains("make a new connection code in the CIC portal"));
    // Two members with one token: the relay takes it once.
    let spent = (1..=2)
        .find(|&n| sim.members()[n].link() == LinkState::Refused)
        .unwrap();
    let text = problem(spent);
    assert!(text.contains("(token_spent)"), "{text}");
    assert!(text.contains("replace its line"), "{text}");
    for problem in [&unknown, &text] {
        assert!(
            invites
                .iter()
                .all(|i| !problem.contains(i.split("connect=").nth(1).unwrap()))
        );
    }

    // Unbound on the reconnect after a drop, for both members that were online.
    relay.unbind_all();
    relay.drop_connections();
    until("the bound members refused as unbound", || {
        (sim.counts().refused == 4).then_some(())
    })
    .await;
    let unbound = problem(3);
    assert!(unbound.contains("(unbound)"), "{unbound}");
    let connects = relay.seen().connects.len();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        relay.seen().connects.len(),
        connects,
        "no retry after a refusal"
    );
    sim.close().await;

    let kicked_data = tempfile::tempdir().unwrap();
    let invite = [relay.invite(1, None)];
    let sim = Sim::start(options(&invite, kicked_data.path())).unwrap();
    until("online", || (sim.counts().online == 1).then_some(())).await;
    relay.kick(1);
    until("kicked", || (sim.counts().refused == 1).then_some(())).await;
    let kicked = sim.members()[0].problem().unwrap();
    assert!(
        kicked.contains("(kicked)") && kicked.contains("add it again"),
        "{kicked}"
    );
    sim.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pilot_drops_and_comes_back_by_key_while_the_crew_stays() {
    let relay =
        HostedRelay::start(vec![account("u-a", &["ops-a"]), account("u-b", &["ops-a"])]).await;
    let invites = [relay.invite(0, None), relay.invite(1, None)];
    let data = tempfile::tempdir().unwrap();
    let mut o = options(&invites, data.path());
    o.cycle = Duration::from_secs(2);
    o.pilot_drop = Some(Duration::from_millis(600));
    o.scripted = Some(0);
    o.ships = vec!["ANVL_Carrack:pilot,copilot".parse().unwrap()];
    let mut sim = Sim::start(o).unwrap();
    let mut dropped = false;
    for _ in 0..100 {
        sim.tend().await;
        dropped |= sim.counts().dropped == 1;
        if dropped && sim.counts().online == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dropped, "the pilot dropped");
    assert_eq!(sim.counts().online, 2, "and came back");
    let pilot = sim.members()[0].public_key();
    assert!(sim.members()[0].is_pilot() && !sim.members()[1].is_pilot());
    {
        let seen = relay.seen();
        assert_eq!(seen.connects.len(), 3, "{:?}", seen.connects);
        assert_eq!(seen.connects[2], (false, Some("ops-a".to_owned())));
        assert!(seen.byes >= 1);
    }
    assert_eq!(sim.members()[0].link(), LinkState::Online);
    assert_eq!(sim.members()[0].public_key(), pilot);
    sim.close().await;
}
