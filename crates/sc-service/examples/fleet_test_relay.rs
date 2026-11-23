//! A fleet relay for trying the fleet panel by hand and for the page tests: the test relay from
//! `tests/support`, with made-up members whose cards change over time. It is a test tool, not
//! part of the app; real fleets run on a CIC relay or a host player's app.
//!
//! `cargo run -p sc-service --example fleet_test_relay -- [--members N] [--event] [--port P]`
//! prints an invite (code `test`) to paste into the fleet panel. With `--event` it sends an event
//! structure (a wing of squadrons and a ground force of squads) and makes whoever joins the admiral.
//! With `--hosted` it acts as a CIC relay instead: it prints connection codes (a general one,
//! whose two open events make the player choose, and a per-event one) for the account `u-me`, the
//! admiral of an event with a fleet's multi-crew ship and its ship card, a wing, a ground force
//! and two Unassigned members. `--portal <https URL>` adds the portal's address to those codes,
//! as a CIC relay does, for the panel's "Open in CIC" links.

#[path = "../tests/support/test_relay.rs"]
mod test_relay;

use std::time::{SystemTime, UNIX_EPOCH};

use fleet_proto::card::{CARD_SCHEMA, FieldValue, Fields, Sharing, Tone};
use fleet_proto::structure::{EventInfo, EventState, EventStructure, Seat, Unit, UnitKind};
use fleet_proto::{Card, Identity, Life, PublicKey, Role, ship_card};
use test_relay::{Hosted, Options, TestRelay, Token};

const HANDLES: [&str; 11] = [
    "Vega", "Rook", "Kestrel", "Wren", "Ash", "Hawk", "Juno", "Orca", "Pike", "Sable", "Talon",
];
const SHIPS: [&str; 6] = [
    "Gladius", "Arrow", "Cutlass", "Hornet", "Vanguard", "Valkyrie",
];

fn member(n: usize) -> PublicKey {
    Identity::from_seed([u8::try_from(n + 1).unwrap_or(1); 32]).public()
}

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn pct(f: f64) -> FieldValue {
    let f = f.clamp(0.0, 1.0);
    let tone = match f {
        f if f < 0.25 => Tone::Bad,
        f if f < 0.5 => Tone::Warn,
        _ => Tone::Ok,
    };
    FieldValue {
        text: format!("{:.0}%", f * 100.0),
        tone,
        fraction: Some(f),
        items: None,
    }
}

/// Member `n`'s card at time `t`: slow waves on every bar, and set states for a few.
fn card(n: usize, t: f64) -> Card {
    #[allow(clippy::cast_precision_loss)]
    let phase = n as f64 * 1.3;
    let wave = |speed: f64, low: f64| low + (1.0 - low) * (0.5 + 0.5 * (t * speed + phase).sin());
    let life = match n {
        1 => Life::Downed,
        4 => Life::Dead,
        6 => Life::Respawned,
        _ => Life::Up,
    };
    let health = match life {
        Life::Downed => 0.0,
        Life::Dead => 0.0,
        _ => wave(0.05, 0.4),
    };
    let fields = vec![
        (
            "ship.name",
            FieldValue::text(SHIPS[n % SHIPS.len()], Tone::Info),
        ),
        ("ship.mode", FieldValue::text("SCM", Tone::Info)),
        ("ship.shields", pct(wave(0.11, 0.1))),
        ("ship.hull", pct(wave(0.03, 0.3))),
        ("ship.fuel", pct(wave(0.02, 0.2))),
        ("ship.quantum", pct(wave(0.01, 0.5))),
        (
            "ship.cautions",
            FieldValue::text(
                if n.is_multiple_of(3) { "HIT" } else { "NONE" },
                if n.is_multiple_of(3) {
                    Tone::Warn
                } else {
                    Tone::Ok
                },
            ),
        ),
        ("ship.weapons", FieldValue::text("4/4", Tone::Ok)),
        ("char.health", pct(health)),
        ("char.oxygen", pct(1.0)),
        (
            "char.activity",
            FieldValue::text("SEATED, PILOT", Tone::Info),
        ),
    ];
    Card {
        schema: CARD_SCHEMA,
        member: member(n).to_string(),
        handle: HANDLES[n % HANDLES.len()].to_owned(),
        sharing: Sharing {
            ship: true,
            character: true,
            detail: false,
        },
        state: Some(life),
        fields: Fields(fields),
        source: None,
    }
}

/// A wing of two squadrons (one ship per pilot) and a ground force of two squads; the member
/// who joins is the admiral and flies in Viper.
fn event(joiner: &PublicKey, members: usize) -> EventStructure {
    let ids = |ns: &[usize]| {
        ns.iter()
            .filter(|n| **n < members)
            .map(|n| member(*n).to_string())
            .collect::<Vec<_>>()
    };
    let unit = |id: &str, kind, name: &str, parent: Option<&str>, lead: &[usize]| Unit {
        id: id.to_owned(),
        kind,
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        lead: ids(lead),
        seats: vec![],
    };
    let mut units = vec![
        unit("air", UnitKind::Wing, "Air wing", None, &[0]),
        unit("viper", UnitKind::Squadron, "Viper", Some("air"), &[0]),
        unit("lancer", UnitKind::Squadron, "Lancer", Some("air"), &[3]),
        unit("ground", UnitKind::Ground, "Ground", None, &[5]),
    ];
    let mut ships = |squadron: &str, pilots: Vec<String>| {
        for (n, pilot) in pilots.into_iter().enumerate() {
            let id = format!("{squadron}-{}", n + 1);
            units.push(Unit {
                seats: vec![Seat {
                    member: pilot,
                    role: Role::Pilot,
                }],
                ..unit(&id, UnitKind::Ship, &id, Some(squadron), &[])
            });
        }
    };
    let mut viper = ids(&[0, 1, 2]);
    viper.push(joiner.to_string());
    ships("viper", viper);
    ships("lancer", ids(&[3, 4]));
    for (id, name, lead, soldiers) in [
        ("team-a", "Team A", 5, &[5, 6, 7][..]),
        ("medics", "Medics", 8, &[8, 9][..]),
    ] {
        units.push(Unit {
            seats: ids(soldiers)
                .into_iter()
                .map(|member| Seat {
                    member,
                    role: Role::Soldier,
                })
                .collect(),
            ..unit(id, UnitKind::Squad, name, Some("ground"), &[lead])
        });
    }
    EventStructure {
        event: EventInfo {
            id: "op-test".to_owned(),
            name: "Operation Test".to_owned(),
            state: EventState::Open,
        },
        admiral: Some(joiner.to_string()),
        units,
        unassigned: vec![],
    }
}

const GENERAL_TOKEN: &str = "TESTTESTTESTTESTTESTTESTTESTTESTTESTTESTTEQ";
const EVENT_TOKEN: &str = "EVENTEVENTEVENTEVENTEVENTEVENTEVENTEVENTEVA";

/// The made-up members of the hosted event, by user id: handle, unit, seat.
const CREW: [(&str, &str, &str, Role); 10] = [
    ("u-rook", "Rook", "bounty", Role::Pilot),
    ("u-kestrel", "Kestrel", "bounty", Role::Copilot),
    ("u-wren", "Wren", "bounty", Role::Crew),
    ("u-vega", "Vega", "viper-1", Role::Pilot),
    ("u-hawk", "Hawk", "viper-2", Role::Pilot),
    ("u-juno", "Juno", "viper-3", Role::Pilot),
    ("u-pike", "Pike", "team-a", Role::Soldier),
    ("u-sable", "Sable", "team-a", Role::Soldier),
    ("u-talon", "Talon", "team-a", Role::Soldier),
    ("u-lyra", "Lyra", "viper-2", Role::Copilot),
];

/// Admiral `u-me`; fleet Navy with the Bounty (captain Rook); wing Air with squadron Viper
/// (commander Vega; Hawk, dead, flies with copilot Lyra); ground force Ground with squad Team A (commander Pike). Talon has not
/// connected; Orca and Ash are Unassigned.
fn hosted_event(id: &str, name: &str) -> EventStructure {
    let unit = |id: &str, kind, name: &str, parent: Option<&str>, lead: &[&str]| Unit {
        id: id.to_owned(),
        kind,
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        lead: lead.iter().map(|m| (*m).to_owned()).collect(),
        seats: vec![],
    };
    let mut units = vec![
        unit("navy", UnitKind::Fleet, "Navy", None, &[]),
        unit(
            "bounty",
            UnitKind::Ship,
            "Bounty",
            Some("navy"),
            &["u-rook"],
        ),
        unit("air", UnitKind::Wing, "Air wing", None, &[]),
        unit(
            "viper",
            UnitKind::Squadron,
            "Viper",
            Some("air"),
            &["u-vega"],
        ),
        unit("viper-1", UnitKind::Ship, "Vega", Some("viper"), &[]),
        unit("viper-2", UnitKind::Ship, "Hawk", Some("viper"), &[]),
        unit("viper-3", UnitKind::Ship, "Juno", Some("viper"), &[]),
        unit("ground", UnitKind::Ground, "Ground", None, &[]),
        unit(
            "team-a",
            UnitKind::Squad,
            "Team A",
            Some("ground"),
            &["u-pike"],
        ),
    ];
    for (member, _, at, role) in CREW {
        let u = units.iter_mut().find(|u| u.id == at).unwrap();
        u.seats.push(Seat {
            member: member.to_owned(),
            role,
        });
    }
    EventStructure {
        event: EventInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            state: EventState::Open,
        },
        admiral: Some("u-me".to_owned()),
        units,
        unassigned: vec!["u-orca".to_owned(), "u-ash".to_owned()],
    }
}

/// The crew's cards as the relay rewrites them (user ids), then a ship card for every ship.
fn hosted_cards(t: f64) -> Vec<Card> {
    let mut cards: Vec<Card> = CREW
        .iter()
        .enumerate()
        .filter(|(_, (member, ..))| *member != "u-talon")
        .map(|(n, (member, handle, at, role))| {
            let mut c = card(n, t);
            c.member = (*member).to_owned();
            c.handle = (*handle).to_owned();
            let ship = match *at {
                "viper-1" => "Vega",
                "viper-2" => "Hawk",
                "viper-3" => "Juno",
                _ => "Bounty",
            };
            // Wren reports another ship, so the panel flags "not aboard".
            let class = if *member == "u-wren" {
                "ANVL_Arrow"
            } else {
                "DRAK_Cutlass"
            };
            c.fields.0.retain(|(id, _)| *id != "ship.name");
            c.fields
                .0
                .insert(0, ("ship.name", FieldValue::text(ship, Tone::Info)));
            c.fields
                .0
                .insert(1, ("ship.class", FieldValue::text(class, Tone::Info)));
            c.fields.0.push((
                "ship.seat",
                FieldValue::text(role.label().to_uppercase(), Tone::Info),
            ));
            if *role == Role::Soldier {
                c.fields.0.retain(|(id, _)| !id.starts_with("ship."));
                c.sharing.ship = false;
                c.fields
                    .0
                    .push(("char.activity", FieldValue::text("ON FOOT", Tone::Info)));
                c.fields
                    .0
                    .retain(|(id, v)| *id != "char.activity" || v.text == "ON FOOT");
            }
            c
        })
        .collect();
    let structure = hosted_event("op-1", "Op One");
    let reports: Vec<(&Card, u64)> = cards.iter().map(|c| (c, 100)).collect();
    let ships: Vec<Card> = structure
        .units
        .iter()
        .filter_map(|u| ship_card(&structure, &u.id, &reports, None))
        .collect();
    cards.extend(ships);
    cards
}

fn hosted_options() -> Options {
    let fake = |n: u8, user: &str| (Identity::from_seed([n; 32]).public(), user.to_owned());
    let mut users: Vec<(String, String)> = CREW
        .iter()
        .map(|(m, h, ..)| ((*m).to_owned(), (*h).to_owned()))
        .collect();
    users.extend(
        [("u-me", "Tester"), ("u-orca", "Orca"), ("u-ash", "Ash")]
            .map(|(m, h)| (m.to_owned(), h.to_owned())),
    );
    Options {
        card_every_ms: 500,
        hosted: Some(Hosted {
            tokens: vec![
                Token {
                    token: GENERAL_TOKEN.to_owned(),
                    user: "u-me".to_owned(),
                    event: None,
                },
                Token {
                    token: EVENT_TOKEN.to_owned(),
                    user: "u-me".to_owned(),
                    event: Some("op-1".to_owned()),
                },
            ],
            users,
            events: vec![
                hosted_event("op-1", "Operation Test"),
                hosted_event("op-2", "Evening patrol"),
            ],
            // Everyone made up but Talon and Ash counts as connected.
            bound: CREW
                .iter()
                .enumerate()
                .filter(|(_, (m, ..))| *m != "u-talon")
                .map(|(n, (m, ..))| fake(u8::try_from(n + 50).unwrap_or(50), m))
                .chain([fake(90, "u-orca")])
                .collect(),
        }),
        ambient: Box::new(|| hosted_cards(now_s())),
        ..Options::default()
    }
}

#[tokio::main]
async fn main() {
    let mut members = 4usize;
    let mut structured = false;
    let mut hosted = false;
    let mut port = 0u16;
    let mut portal = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--members" => {
                members = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(4)
                    .min(11)
            }
            "--event" => structured = true,
            "--hosted" => hosted = true,
            "--port" => port = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--portal" => {
                let url = args.next().unwrap_or_default();
                portal = format!("&portal={}", percent_encode(&url));
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if hosted {
        let relay = TestRelay::start_on(
            &format!("127.0.0.1:{port}"),
            Identity::generate().expect("randomness"),
            hosted_options(),
        )
        .await;
        println!(
            "invite: {}{portal}",
            relay.connect_invite(GENERAL_TOKEN, None)
        );
        println!(
            "event invite: {}{portal}",
            relay.connect_invite(EVENT_TOKEN, Some("op-1"))
        );
        let _ = tokio::signal::ctrl_c().await;
        return;
    }
    let options = Options {
        codes: vec!["test".to_owned()],
        card_every_ms: 500,
        structure: if structured {
            Box::new(move |k| Some(event(k, members)))
        } else {
            Box::new(|_| None)
        },
        ambient: Box::new(move || (0..members).map(|n| card(n, now_s())).collect()),
        ..Options::default()
    };
    let relay = TestRelay::start_on(
        &format!("127.0.0.1:{port}"),
        Identity::generate().expect("randomness"),
        options,
    )
    .await;
    println!("invite: {}", relay.invite("test"));
    let _ = tokio::signal::ctrl_c().await;
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
