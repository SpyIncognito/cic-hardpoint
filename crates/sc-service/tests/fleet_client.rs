//! The fleet client end to end over TLS: against the test relay in `support/`, and (ignored
//! unless asked for) against a CIC relay binary named by `CIC_RELAY_BIN`.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use fleet_proto::Role;
use fleet_proto::structure::{EventInfo, EventState, EventStructure, Seat, Unit, UnitKind};
use fleet_proto::{Identity, PublicKey};
use sc_core::engine::EngineOptions;
use sc_game::{GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::fleet::{LinkInfo, LinkState};
use sc_service::{Service, ServiceOptions};
use support::test_relay::{Hosted, Options, TestRelay, Token};

struct Player {
    service: Service,
    dir: tempfile::TempDir,
}

fn frame() -> Value {
    json!({
        "vehicle": { "state": {
            "name": "Test ship",
            "health": 0.8,
            "shields": { "level": 0.9 },
        } },
        "actor": {
            "vitals": { "health": { "current": 70, "max": 100 }, "oxygen": { "current": 1, "max": 1 } },
            "state": { "uec_balance": "987654" },
        },
    })
}

impl Player {
    async fn start() -> Self {
        Self::start_in(tempfile::tempdir().unwrap()).await
    }

    async fn start_in(dir: tempfile::TempDir) -> Self {
        let frames = (0..50)
            .map(|i| RecordedFrame {
                t: f64::from(i) * 20.0,
                frame: frame(),
            })
            .collect();
        let options = ReplayOptions {
            loop_playback: true,
            ..ReplayOptions::default()
        };
        let game = Arc::new(ReplayGame::new(frames, options).unwrap());
        let service = Service::start(ServiceOptions {
            port: 0,
            settings_dir: Some(dir.path().to_owned()),
            bus_file: dir.path().join("bus.json"),
            game: Some(game as Arc<dyn GameApi>),
            engine: EngineOptions::default(),
            fleet_connector: None,
            relay: sc_service::relay::RelayOptions::default(),
        })
        .await
        .unwrap();
        Self { service, dir }
    }

    fn link(&self) -> LinkInfo {
        self.service.fleet().link()
    }

    async fn until(&self, what: &str, ok: impl Fn(&LinkInfo) -> bool) -> LinkInfo {
        for _ in 0..200 {
            let link = self.link();
            if ok(&link) {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}: {:?}", self.link());
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
}

fn key(n: u8) -> PublicKey {
    Identity::from_seed([n; 32]).public()
}

/// Peer to peer, members are keys: the player is the admiral and flies one of the two ships in
/// squadron "Flight 1" of wing "Air wing"; key 2 commands both and flies the other.
fn event_with(me: PublicKey) -> EventStructure {
    let unit = |id: &str, kind, name: &str, parent: Option<&str>| Unit {
        id: id.to_owned(),
        kind,
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        lead: vec![],
        seats: vec![],
    };
    let pilot = |id: &str, who: PublicKey| Unit {
        seats: vec![Seat {
            member: who.to_string(),
            role: Role::Pilot,
        }],
        ..unit(id, UnitKind::Ship, id, Some("f1"))
    };
    EventStructure {
        event: EventInfo {
            id: "op-1".to_owned(),
            name: "Op One".to_owned(),
            state: EventState::Open,
        },
        admiral: Some(me.to_string()),
        units: vec![
            Unit {
                lead: vec![key(2).to_string()],
                ..unit("air", UnitKind::Wing, "Air wing", None)
            },
            Unit {
                lead: vec![key(2).to_string()],
                ..unit("f1", UnitKind::Squadron, "Flight 1", Some("air"))
            },
            pilot("f1-a", me),
            pilot("f1-b", key(2)),
        ],
        unassigned: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn joins_from_an_invite_sends_only_consented_fields_and_keeps_no_code() {
    let relay = TestRelay::start(Options {
        structure: Box::new(|k| Some(event_with(*k))),
        ..Options::default()
    })
    .await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    client
        .set_consent(Some(&json!(["ship.hull", "char.health"])))
        .unwrap();
    client.join(&relay.invite("code-1"), "  Vega ").unwrap();
    let link = player
        .until("online with roles", |l| {
            l.state == LinkState::Online && !l.grants.is_empty()
        })
        .await;
    assert_eq!(link.handle.as_deref(), Some("Vega"));
    assert_eq!(link.event.as_ref().map(|e| e.name.as_str()), Some("Op One"));
    assert!(
        link.grants
            .iter()
            .any(|g| g.role == fleet_proto::Role::Admiral)
    );
    assert_eq!(relay.seen().joins, [Some("code-1".to_owned())]);

    // Cards at the relay carry the consented fields only, never the balance.
    for _ in 0..100 {
        if relay.seen().cards.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let card = relay.seen().cards.last().cloned().expect("a card");
    assert_eq!(card.member, client.public_key().to_string());
    assert_eq!(
        card.fields.ids().collect::<Vec<_>>(),
        ["ship.hull", "char.health"]
    );
    assert!(card.sharing.character && card.sharing.ship);
    assert!(!serde_json::to_string(&card).unwrap().contains("987654"));

    // The model: the event's structure, this player's own card in it, sharing on.
    let model = player.service.fleet().model();
    assert!(model.sharing);
    let tree = model.tree.as_ref().expect("the unit tree");
    assert_eq!(tree.units[0].name, "Air wing");
    assert_eq!(tree.units[0].units[0].name, "Flight 1");
    assert_eq!(
        tree.units[0].units[0].rollup.text,
        "1/2 Pilots up, 1 offline"
    );
    assert!(model.admiral_allowed);
    assert!(model.members.iter().any(|m| m.me));

    // Kept: address, key and handle; never the code.
    let saved = player.file("fleets.json");
    assert!(saved.contains(&relay.key().to_string()), "{saved}");
    assert!(saved.contains("\"Vega\"") && saved.contains("Op One"));
    assert!(!saved.contains("code-1"), "{saved}");
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_with_another_key_is_refused_and_not_kept() {
    let relay = TestRelay::start(Options::default()).await;
    let player = Player::start().await;
    let other = Identity::generate().unwrap().public();
    let invite = relay
        .invite("code-1")
        .replace(&relay.key().to_string(), &other.to_string());
    player.service.fleet_client().join(&invite, "Vega").unwrap();
    let link = player
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("The server's identity key is not the one in the invite")
    );
    assert_eq!(
        relay.seen().joins.len(),
        0,
        "nothing sent past the pin check"
    );
    assert!(!player.file("fleets.json").contains(&other.to_string()));
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_does_not_bind_to_tls_is_refused() {
    let relay = TestRelay::start(Options {
        binding: false,
        ..Options::default()
    })
    .await;
    let player = Player::start().await;
    player
        .service
        .fleet_client()
        .join(&relay.invite("code-1"), "Vega")
        .unwrap();
    let link = player
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("The server does not bind its identity to the TLS session: it needs an update")
    );
    assert_eq!(relay.seen().handshakes, 0, "no client proof was sent");
    player.service.close().await;
}

/// Terminates TLS from the player with its own certificate, opens its own TLS session to the
/// relay, and passes the bytes through unchanged: a relay of the identity handshake.
async fn man_in_the_middle(relay: std::net::SocketAddr) -> std::net::SocketAddr {
    use sc_service::fleet::Connector;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = support::test_relay::tls();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut player) = acceptor.accept(tcp).await else {
                    return;
                };
                let upstream = sc_service::fleet::TlsConnector::new()
                    .unwrap()
                    .connect("127.0.0.1", relay.port())
                    .await;
                if let Ok(mut up) = upstream {
                    let _ = tokio::io::copy_bidirectional(&mut player, &mut up.stream).await;
                }
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handshake_relayed_across_two_tls_sessions_is_refused() {
    let relay = TestRelay::start(Options::default()).await;
    let mitm = man_in_the_middle(relay.addr).await;
    let player = Player::start().await;
    let invite = relay.invite("code-1").replace(
        &format!(":{}?", relay.addr.port()),
        &format!(":{}?", mitm.port()),
    );
    player.service.fleet_client().join(&invite, "Vega").unwrap();
    let link = player
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("The server could not prove its identity key")
    );
    assert_eq!(relay.seen().handshakes, 0);
    assert!(relay.seen().joins.is_empty());
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_join_code_is_refused_and_bad_invites_are_answered_at_once() {
    let relay = TestRelay::start(Options::default()).await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    assert_eq!(
        client.join("cic://nowhere?code=x", "Vega"),
        Err("the invite has no valid port (1 to 65535)".to_owned())
    );
    assert!(client.join(&relay.invite("code-1"), "<Vega>").is_err());
    client.join(&relay.invite("wrong"), "Vega").unwrap();
    let link = player
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(link.problem.as_deref(), Some("That join code is not valid"));
    assert_eq!(player.file("fleets.json"), "");
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_connection_shows_offline_then_reconnects_and_a_restart_rejoins_by_key() {
    let relay = TestRelay::start(Options::default()).await;
    let player = Player::start().await;
    player
        .service
        .fleet_client()
        .join(&relay.invite("code-1"), "Vega")
        .unwrap();
    player
        .until("online", |l| l.state == LinkState::Online)
        .await;

    relay.drop_connections();
    let offline = player
        .until("offline", |l| l.state == LinkState::Offline)
        .await;
    let retry = offline.retry_ms.expect("a retry time");
    assert!(
        (800..=1000).contains(&retry),
        "first retry after about 1 s: {retry}"
    );
    assert_eq!(
        offline.fleet.as_ref().map(|f| f.handle.as_str()),
        Some("Vega")
    );
    player
        .until("online again", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(relay.seen().handshakes, 2);
    // The code is spent on the first join: a reconnect is by key.
    assert_eq!(relay.seen().joins[1], None);

    // A new run: the fleet is joined again, by key, with no code.
    let dir = {
        let Player { service, dir } = player;
        service.close().await;
        dir
    };
    assert!(
        !std::fs::read_to_string(dir.path().join("fleets.json"))
            .unwrap()
            .contains("code-1")
    );
    let player = Player::start_in(dir).await;
    player
        .until("online after a restart", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(relay.seen().joins.last(), Some(&None));
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_tells_the_server_and_forgets_the_fleet() {
    let relay = TestRelay::start(Options::default()).await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    client.join(&relay.invite("code-1"), "Vega").unwrap();
    player
        .until("online", |l| l.state == LinkState::Online)
        .await;
    client.leave(&relay.key().to_string()).await.unwrap();
    assert_eq!(player.link().state, LinkState::None);
    assert!(player.link().fleets.is_empty());
    for _ in 0..100 {
        if !relay.seen().byes.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(relay.seen().byes, ["leave"]);
    assert!(
        !player
            .file("fleets.json")
            .contains(&relay.key().to_string())
    );
    assert!(player.service.fleet().model().members.is_empty());
    player.service.close().await;
}

/// Against a running CIC relay: `CIC_RELAY_INVITE=<invite> cargo test -- --ignored`, the invite
/// for an open event whose code puts members in a group (members in no group see nobody).
/// Two players join, see each other's cards with only the consented fields, and the second
/// comes back after its connection is replaced.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a running CIC relay and an invite in CIC_RELAY_INVITE"]
async fn against_a_cic_relay() {
    let invite = std::env::var("CIC_RELAY_INVITE").expect("CIC_RELAY_INVITE");
    let (a, b) = (Player::start().await, Player::start().await);
    for (p, handle, fields) in [
        (&a, "Alpha", json!(["ship.hull", "char.health"])),
        (&b, "Alpha", json!(["ship.shields"])),
    ] {
        let client = p.service.fleet_client();
        client.set_consent(Some(&fields)).unwrap();
        client.join(&invite, handle).unwrap();
    }
    let la = a.until("a online", |l| l.state == LinkState::Online).await;
    let lb = b.until("b online", |l| l.state == LinkState::Online).await;
    eprintln!("a: event {:?}, handle {:?}", la.event, la.handle);
    eprintln!("b: event {:?}, handle {:?}", lb.event, lb.handle);
    assert_ne!(la.handle, lb.handle, "the event makes handles unique");
    let b_key = b.service.fleet_client().public_key().to_string();
    let a_key = a.service.fleet_client().public_key().to_string();
    let mut got = None;
    for _ in 0..100 {
        got = a
            .service
            .fleet()
            .model()
            .members
            .into_iter()
            .find(|m| m.card.member == b_key);
        if got.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let b_card = got.expect("a receives b's card through the relay").card;
    assert_eq!(b_card.fields.ids().collect::<Vec<_>>(), ["ship.shields"]);
    let model = a.service.fleet().model();
    assert_eq!(
        model.layout,
        sc_service::fleet::hub::Layout::Event,
        "a structure came"
    );
    eprintln!(
        "a sees units {:?}",
        model
            .tree
            .iter()
            .flat_map(|t| &t.units)
            .map(|u| &u.name)
            .collect::<Vec<_>>()
    );
    for _ in 0..100 {
        if b.service
            .fleet()
            .model()
            .members
            .iter()
            .any(|m| m.card.member == a_key)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let a_card = b
        .service
        .fleet()
        .model()
        .members
        .into_iter()
        .find(|m| m.card.member == a_key)
        .expect("b receives a's card")
        .card;
    assert_eq!(
        a_card.fields.ids().collect::<Vec<_>>(),
        ["ship.hull", "char.health"]
    );

    // Leaving tells the relay; b sees a go offline in time, and a's fleet is forgotten.
    let key = la.fleet.as_ref().unwrap().key.to_string();
    a.service.fleet_client().leave(&key).await.unwrap();
    assert!(a.link().fleets.is_empty());
    a.service.close().await;
    b.service.close().await;
}

const TOKEN_A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TOKEN_B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA";
const TOKEN_C: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCA";

/// A hosted event: Vega (`u-vega`) is the admiral; fleet "Navy" holds the ship "Bounty" with
/// Rook as its Pilot; Wren is Unassigned.
fn hosted_event(id: &str, name: &str) -> EventStructure {
    EventStructure {
        event: EventInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            state: EventState::Open,
        },
        admiral: Some("u-vega".to_owned()),
        units: vec![
            Unit {
                id: "navy".to_owned(),
                kind: UnitKind::Fleet,
                name: "Navy".to_owned(),
                parent: None,
                lead: vec![],
                seats: vec![],
            },
            Unit {
                id: "bounty".to_owned(),
                kind: UnitKind::Ship,
                name: "Bounty".to_owned(),
                parent: Some("navy".to_owned()),
                lead: vec![],
                seats: vec![Seat {
                    member: "u-rook".to_owned(),
                    role: Role::Pilot,
                }],
            },
        ],
        unassigned: vec!["u-wren".to_owned()],
    }
}

fn hosted(events: Vec<EventStructure>) -> Options {
    let token = |token: &str, user: &str, event: Option<&str>| Token {
        token: token.to_owned(),
        user: user.to_owned(),
        event: event.map(str::to_owned),
    };
    Options {
        hosted: Some(Hosted {
            tokens: vec![
                token(TOKEN_A, "u-vega", None),
                token(TOKEN_B, "u-vega", Some("op-2")),
                token(TOKEN_C, "u-vega", None),
            ],
            users: vec![
                ("u-vega".to_owned(), "Vega".to_owned()),
                ("u-rook".to_owned(), "Rook".to_owned()),
                ("u-wren".to_owned(), "Wren".to_owned()),
            ],
            events,
            bound: vec![],
        }),
        ambient: Box::new(|| {
            use fleet_proto::card::{CARD_SCHEMA, FieldValue, Fields, Sharing, Tone};
            use fleet_proto::{Card, ShipSource, SourceKind};
            let ship = || {
                vec![
                    ("ship.name", FieldValue::text("Bounty", Tone::Info)),
                    ("ship.hull", FieldValue::text("80%", Tone::Ok)),
                ]
            };
            vec![
                Card {
                    schema: CARD_SCHEMA,
                    member: "u-rook".to_owned(),
                    handle: "Rook".to_owned(),
                    sharing: Sharing {
                        ship: true,
                        ..Sharing::default()
                    },
                    state: None,
                    fields: Fields(ship()),
                    source: None,
                },
                Card {
                    schema: CARD_SCHEMA,
                    member: "ship:bounty".to_owned(),
                    handle: "Bounty".to_owned(),
                    sharing: Sharing {
                        ship: true,
                        ..Sharing::default()
                    },
                    state: None,
                    fields: Fields(ship()),
                    source: Some(ShipSource {
                        kind: SourceKind::Pilot,
                        member: Some("u-rook".to_owned()),
                        handle: Some("Rook".to_owned()),
                        role: Some(Role::Pilot),
                        age_ms: None,
                    }),
                },
            ]
        }),
        ..Options::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connects_with_a_token_chooses_the_event_and_reconnects_by_key_without_it() {
    let relay = TestRelay::start(hosted(vec![
        hosted_event("op-1", "Op One"),
        hosted_event("op-2", "Op Two"),
    ]))
    .await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    client.set_consent(Some(&json!(["ship.hull"]))).unwrap();
    assert!(client.choose("op-2").is_err(), "nothing offered yet");
    client
        .join(&relay.connect_invite(TOKEN_A, None), "")
        .unwrap();
    let choosing = player
        .until("the event choice", |l| l.choices.is_some())
        .await;
    let offered: Vec<_> = choosing
        .choices
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.role))
        .collect();
    assert_eq!(
        offered,
        [
            ("op-1".to_owned(), Some(Role::Admiral)),
            ("op-2".to_owned(), Some(Role::Admiral))
        ]
    );
    assert!(client.choose("op-9").is_err());
    client.choose("op-2").unwrap();
    let link = player
        .until("online", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(link.event.as_ref().map(|e| e.id.as_str()), Some("op-2"));
    assert_eq!(link.handle.as_deref(), Some("Vega"), "the account's handle");
    assert!(link.choices.is_none());
    assert_eq!(relay.seen().connects, [(Some(TOKEN_A.to_owned()), None)]);
    assert_eq!(relay.seen().chooses, ["op-2"]);

    // "Me" is the user id: the own card in the model carries it, the wire card the key.
    let model = player.service.fleet().model();
    assert_eq!(model.me.member, "u-vega");
    for _ in 0..100 {
        if !relay.seen().cards.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        relay.seen().cards[0].member,
        client.public_key().to_string()
    );
    let model = player.service.fleet().model();
    assert!(
        model
            .members
            .iter()
            .any(|m| m.me && m.card.member == "u-vega")
    );

    // Kept: host, key, the event; never the token.
    let saved = player.file("fleets.json");
    assert!(saved.contains(&relay.key().to_string()), "{saved}");
    assert!(
        saved.contains("\"eventId\": \"op-2\"") && saved.contains("\"hosted\": true"),
        "{saved}"
    );
    assert!(!saved.contains(TOKEN_A), "{saved}");

    // A dropped connection comes back by key, to the same event, with no token and no choice.
    relay.drop_connections();
    player
        .until("offline", |l| l.state == LinkState::Offline)
        .await;
    player
        .until("online again", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(relay.seen().connects[1], (None, Some("op-2".to_owned())));
    assert_eq!(relay.seen().chooses.len(), 1);

    // So does a new run.
    let dir = {
        let Player { service, dir } = player;
        service.close().await;
        dir
    };
    let player = Player::start_in(dir).await;
    player
        .until("online after a restart", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(
        relay.seen().connects.last(),
        Some(&(None, Some("op-2".to_owned())))
    );
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_general_code_offers_the_events_again_after_joining_one_before() {
    let relay = TestRelay::start(hosted(vec![
        hosted_event("op-1", "Op One"),
        hosted_event("op-2", "Op Two"),
    ]))
    .await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    client
        .join(&relay.connect_invite(TOKEN_A, None), "")
        .unwrap();
    player
        .until("the event choice", |l| l.choices.is_some())
        .await;
    client.choose("op-1").unwrap();
    player
        .until("online in op-1", |l| l.state == LinkState::Online)
        .await;
    assert!(player.file("fleets.json").contains("\"eventId\": \"op-1\""));

    client
        .join(&relay.connect_invite(TOKEN_C, None), "")
        .unwrap();
    let choosing = player
        .until("the event choice again", |l| l.choices.is_some())
        .await;
    assert_eq!(choosing.choices.unwrap().len(), 2);
    assert_eq!(
        relay.seen().connects.last(),
        Some(&(Some(TOKEN_C.to_owned()), None)),
        "the event joined before is not sent with a new code"
    );
    client.choose("op-2").unwrap();
    let link = player
        .until("online in op-2", |l| {
            l.state == LinkState::Online && l.event.as_ref().is_some_and(|e| e.id == "op-2")
        })
        .await;
    assert_eq!(link.fleets.len(), 1);
    assert!(player.file("fleets.json").contains("\"eventId\": \"op-2\""));

    // A reconnect by key goes back to the event chosen last.
    relay.drop_connections();
    player
        .until("offline", |l| l.state == LinkState::Offline)
        .await;
    player
        .until("online again", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(
        relay.seen().connects.last(),
        Some(&(None, Some("op-2".to_owned())))
    );
    assert_eq!(relay.seen().chooses, ["op-1", "op-2"]);
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_event_on_the_same_relay_drops_the_cards_of_the_one_before() {
    let alone = EventStructure {
        units: vec![],
        unassigned: vec![],
        ..hosted_event("op-2", "Op Two")
    };
    let relay = TestRelay::start(hosted(vec![hosted_event("op-1", "Op One"), alone])).await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    let fleet = player.service.fleet();
    client
        .join(&relay.connect_invite(TOKEN_A, None), "")
        .unwrap();
    player
        .until("the event choice", |l| l.choices.is_some())
        .await;
    client.choose("op-1").unwrap();
    player
        .until("online in op-1", |l| l.state == LinkState::Online)
        .await;
    for _ in 0..100 {
        let model = fleet.model();
        if model.members.iter().any(|m| m.ship)
            && model.members.iter().any(|m| m.card.member == "u-rook")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let model = fleet.model();
    assert!(model.members.iter().any(|m| m.card.member == "u-rook"));
    assert!(model.members.iter().any(|m| m.ship));
    assert_eq!(model.rollup.total, 2);

    client
        .join(&relay.connect_invite(TOKEN_C, None), "")
        .unwrap();
    player
        .until("the event choice again", |l| l.choices.is_some())
        .await;
    assert!(
        fleet.model().members.iter().all(|m| m.me),
        "no op-1 cards while choosing"
    );
    client.choose("op-2").unwrap();
    player
        .until("online in op-2", |l| {
            l.state == LinkState::Online && l.event.as_ref().is_some_and(|e| e.id == "op-2")
        })
        .await;
    // Past a few of the relay's card rounds, which would bring op-1's members back.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let model = fleet.model();
    let members: Vec<_> = model
        .members
        .iter()
        .map(|m| m.card.member.as_str())
        .collect();
    assert_eq!(members, ["u-vega"], "only this player in op-2");
    assert_eq!(model.rollup.total, 1);
    assert_eq!(model.rollup.offline, 0);
    assert!(model.unassigned.is_empty());

    // A dropped connection to the same event keeps the panel.
    relay.drop_connections();
    player
        .until("offline", |l| l.state == LinkState::Offline)
        .await;
    assert_eq!(
        fleet.model().link.event.map(|e| e.id).as_deref(),
        Some("op-2")
    );
    player
        .until("online again", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(fleet.model().rollup.total, 1);
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_per_event_token_joins_its_event_without_asking() {
    let relay = TestRelay::start(hosted(vec![
        hosted_event("op-1", "Op One"),
        hosted_event("op-2", "Op Two"),
    ]))
    .await;
    let player = Player::start().await;
    player
        .service
        .fleet_client()
        .join(&relay.connect_invite(TOKEN_B, Some("op-2")), "")
        .unwrap();
    let link = player
        .until("online", |l| l.state == LinkState::Online)
        .await;
    assert_eq!(link.event.as_ref().map(|e| e.name.as_str()), Some("Op Two"));
    assert_eq!(
        relay.seen().connects,
        [(Some(TOKEN_B.to_owned()), Some("op-2".to_owned()))]
    );
    assert!(relay.seen().chooses.is_empty());
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn token_refusals_say_to_make_a_new_code_and_a_fleet_never_joined_is_not_kept() {
    const HINT: &str = "Make a new connection code in the CIC portal";
    let relay = TestRelay::start(hosted(vec![hosted_event("op-1", "Op One")])).await;

    let unknown = Player::start().await;
    let bad = "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDA";
    unknown
        .service
        .fleet_client()
        .join(&relay.connect_invite(bad, None), "")
        .unwrap();
    let link = unknown
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("No account has that connection code")
    );
    assert_eq!(link.hint.as_deref(), Some(HINT));
    let saved = unknown.file("fleets.json");
    assert!(
        !saved.contains(&relay.key().to_string()) && !saved.contains(bad),
        "{saved}"
    );
    assert!(link.fleets.is_empty(), "never joined, so not listed");

    // One player spends the code; a second with the same code is told it is spent.
    let first = Player::start().await;
    first
        .service
        .fleet_client()
        .join(&relay.connect_invite(TOKEN_C, None), "")
        .unwrap();
    first
        .until("online", |l| l.state == LinkState::Online)
        .await;
    let second = Player::start().await;
    second
        .service
        .fleet_client()
        .join(&relay.connect_invite(TOKEN_C, None), "")
        .unwrap();
    let link = second
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("That connection code was used already")
    );
    assert_eq!(link.hint.as_deref(), Some(HINT));

    assert!(link.fleets.is_empty());
    let key = relay.key().to_string();
    assert!(second.service.fleet_client().connect(&key).is_err());
    assert!(!second.file("fleets.json").contains(&key));

    // A refusal for a fleet joined before keeps it listed.
    let attempts = relay.seen().connects.len();
    first
        .service
        .fleet_client()
        .join(&relay.connect_invite(TOKEN_C, None), "")
        .unwrap();
    let link = first
        .until("refused", |l| {
            l.state == LinkState::Refused && relay.seen().connects.len() > attempts
        })
        .await;
    assert_eq!(link.fleets.len(), 1);
    assert!(first.file("fleets.json").contains(&key));
    for p in [unknown, first, second] {
        p.service.close().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_to_peer_server_never_gets_a_connect_and_a_relay_never_a_join() {
    let p2p = TestRelay::start(Options::default()).await;
    let player = Player::start().await;
    let invite = p2p
        .invite("code-1")
        .replace("code=code-1", &format!("connect={TOKEN_A}"));
    player.service.fleet_client().join(&invite, "").unwrap();
    let link = player
        .until("refused", |l| l.state == LinkState::Refused)
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("This server does not take CIC connection codes: it needs an update")
    );
    assert!(p2p.seen().joins.is_empty());

    let relay = TestRelay::start(hosted(vec![hosted_event("op-1", "Op One")])).await;
    let invite = relay
        .connect_invite(TOKEN_A, None)
        .replace(&format!("connect={TOKEN_A}"), "code=code-1");
    player.service.fleet_client().join(&invite, "Vega").unwrap();
    let link = player
        .until("refused by the relay", |l| {
            l.state == LinkState::Refused && l.fleet.as_ref().is_some_and(|f| f.key == relay.key())
        })
        .await;
    assert_eq!(
        link.problem.as_deref(),
        Some("This is a CIC relay: paste a connection code from the CIC portal")
    );
    assert!(relay.seen().connects.is_empty());
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_admiral_sees_ship_cards_the_unassigned_and_a_link_to_the_portal() {
    let relay = TestRelay::start(hosted(vec![hosted_event("op-1", "Op One")])).await;
    let player = Player::start().await;
    let client = player.service.fleet_client();
    let invite = format!(
        "{}&portal=https%3A%2F%2Fcic.example.org%2F",
        relay.connect_invite(TOKEN_A, None)
    );
    client.join(&invite, "").unwrap();
    player
        .until("online", |l| l.state == LinkState::Online)
        .await;
    let fleet = player.service.fleet();
    for _ in 0..100 {
        if fleet.model().members.iter().any(|m| m.ship) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let model = fleet.model();
    let ship = model
        .members
        .iter()
        .find(|m| m.ship)
        .expect("the ship card");
    assert_eq!(ship.card.member, "ship:bounty");
    assert_eq!(ship.source_label.as_deref(), Some("via Rook (Pilot)"));
    assert!(model.commander && model.placed);
    let wren: Vec<_> = model
        .unassigned
        .iter()
        .map(|p| (p.handle.as_str(), p.connected))
        .collect();
    assert_eq!(wren, [("Wren", false)], "not connected yet");
    let bounty = &model.tree.as_ref().unwrap().units[0].units[0];
    assert_eq!(bounty.ship_card.as_deref(), Some("ship:bounty"));
    assert_eq!(bounty.seats[0].person.handle, "Rook");

    assert_eq!(
        model.event_url.as_deref(),
        Some("https://cic.example.org/events/op-1")
    );
    let saved = std::fs::read_to_string(player.dir.path().join("fleets.json")).unwrap();
    assert!(
        saved.contains(r#""portal": "https://cic.example.org""#),
        "{saved}"
    );
    assert!(!saved.contains(TOKEN_A), "never the token: {saved}");

    // Seats are changed in the portal; the panel follows the structure the relay sends.
    relay.portal_change(
        "op-1",
        &fleet_proto::StructureChange::Place {
            member: "u-wren".to_owned(),
            unit: Some("bounty".to_owned()),
            role: Some(Role::Crew),
        },
    );
    for _ in 0..100 {
        if fleet.model().unassigned.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let model = fleet.model();
    assert!(model.unassigned.is_empty());
    let seats: Vec<_> = model.tree.unwrap().units[0].units[0]
        .seats
        .iter()
        .map(|s| (s.person.handle.clone(), s.role.label))
        .collect();
    assert_eq!(
        seats,
        [("Rook".to_owned(), "Pilot"), ("Wren".to_owned(), "Crew")]
    );
    assert!(relay.seen().changes.is_empty(), "Hardpoint sends no change");
    player.service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ship_card_from_a_peer_to_peer_host_is_taken_over_its_connection() {
    let mut options = hosted(vec![]);
    options.hosted = None;
    let relay = TestRelay::start(options).await;
    let player = Player::start().await;
    player
        .service
        .fleet_client()
        .join(&relay.invite("code-1"), "Vega")
        .unwrap();
    player
        .until("online", |l| l.state == LinkState::Online)
        .await;
    let fleet = player.service.fleet();
    for _ in 0..100 {
        if fleet
            .model()
            .members
            .iter()
            .any(|m| m.card.member == "u-rook")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for _ in 0..100 {
        if fleet.model().members.iter().any(|m| m.ship) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let model = fleet.model();
    assert!(model.members.iter().any(|m| m.card.member == "u-rook"));
    // fleet-proto 0.4.2: a peer-to-peer host may build ship cards too.
    assert!(model.members.iter().any(|m| m.ship));
    player.service.close().await;
}
