//! The fleet card builder: what this player shares, built from their own game state. The card's
//! shape, its fields and the consent rules are in `fleet-proto`, shared with the relay.
//!
//! What goes on a card is capped twice: by what the sender consents to send ([`SendConsent`],
//! field by field) and by what viewers asked for (the subscription). The aUEC balance, keybinds
//! and sequences are never read here.

use std::collections::HashSet;

use serde_json::Value;

pub use fleet_proto::card::{
    CARD_SCHEMA, Card, Consent, FIELDS, FieldDef, FieldValue, Fields, Life, Sharing, card_fields,
    detail_fields, field_def,
};
pub use fleet_proto::consent::SendConsent;
use fleet_proto::consent::sendable;

use sc_core::js;
use sc_core::state::values::DASH;
use sc_core::state::{Freshness, StateModel, read_value, value_def};

use crate::portal::status::{Reading, Tone, shield_faces, tone_of_text};

/// How long a member shows RESPAWNED before going back to UP.
pub const RESPAWNED_MS: i64 = 30_000;
/// The game sends no revive event; a downed member whose health is back at or above this is taken
/// as revived. Not checked in game.
pub const REVIVED_AT: f64 = 0.5;

pub const EVENT_SPAWN: &str = "GAME_EVENT_TYPE_ACTOR_SPAWN";
pub const EVENT_DEAD: &str = "GAME_EVENT_TYPE_ACTOR_DEAD";
pub const EVENT_RESPAWN: &str = "GAME_EVENT_TYPE_ACTOR_RESPAWN";
pub const EVENT_DOWNED: &str = "GAME_EVENT_TYPE_ACTOR_DOWNED";

/// Follows the game's spawn, downed, dead and respawn events, frame by frame (the state model
/// keeps only the last events section, so events are read as frames arrive), and the health gauge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifeTracker {
    life: Life,
    since_ms: i64,
}

impl Default for LifeTracker {
    fn default() -> Self {
        Self {
            life: Life::Up,
            since_ms: 0,
        }
    }
}

impl LifeTracker {
    fn set(&mut self, life: Life, now_ms: i64) {
        if self.life != life {
            self.life = life;
            self.since_ms = now_ms;
        }
    }

    /// The events in one frame, in order.
    pub fn observe_frame(&mut self, frame: &Value, now_ms: i64) {
        let Some(events) = frame
            .get("events")
            .and_then(|e| e.get("events"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for event in events {
            match event.get("type").and_then(Value::as_str) {
                Some(EVENT_SPAWN) => self.set(Life::Up, now_ms),
                Some(EVENT_RESPAWN) => self.set(Life::Respawned, now_ms),
                Some(EVENT_DEAD) => self.set(Life::Dead, now_ms),
                Some(EVENT_DOWNED) if self.life != Life::Dead => self.set(Life::Downed, now_ms),
                _ => {}
            }
        }
    }

    /// The health gauge as a 0..1 fraction, when fresh. Health at zero is downed; a downed member
    /// back at [`REVIVED_AT`] is up. Only a respawn ends DEAD.
    pub fn observe_health(&mut self, health: Option<f64>, now_ms: i64) {
        let Some(health) = health.filter(|h| h.is_finite()) else {
            return;
        };
        match self.life {
            Life::Up | Life::Respawned if health <= 0.0 => self.set(Life::Downed, now_ms),
            Life::Downed if health >= REVIVED_AT => self.set(Life::Up, now_ms),
            _ => {}
        }
    }

    pub fn life(&self, now_ms: i64) -> Life {
        match self.life {
            Life::Respawned if now_ms - self.since_ms >= RESPAWNED_MS => Life::Up,
            life => life,
        }
    }
}

fn dash() -> FieldValue {
    FieldValue::text(DASH, Tone::Unknown)
}

/// Who a card is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub member: String,
    pub handle: String,
    pub consent: SendConsent,
}

/// A percent value's fraction: the core's own, else the raw number when it is already 0..1
/// (hull health arrives as a 0..1 number).
fn fraction_of(state: &StateModel, id: &str) -> FieldValue {
    let Some(view) = read_value(state, id) else {
        return dash();
    };
    if view.freshness != Freshness::Fresh || view.text == DASH {
        return dash();
    }
    let fraction = view.fraction.or_else(|| {
        let def = value_def(id)?;
        let n = js::to_number(state.read(&def.path, def.fallback.as_ref()).value?);
        (0.0..=1.0).contains(&n).then_some(n)
    });
    match fraction {
        Some(f) => FieldValue {
            text: format!("{}%", js::number_to_string((f * 100.0).round())),
            tone: tone_of_text("x", Some(f), None),
            fraction: Some(f),
            items: None,
        },
        None => FieldValue::text(view.text, Tone::Info),
    }
}

fn text_of(state: &StateModel, id: &str) -> FieldValue {
    match read_value(state, id) {
        Some(view) if view.freshness == Freshness::Fresh && view.text != DASH => {
            let tone = tone_of_text(&view.text, view.fraction, view.on);
            FieldValue::text(view.text, tone)
        }
        _ => dash(),
    }
}

const CAUTIONS: [&str; 10] = [
    "pwr", "wpn", "thr", "shld", "cool", "qdrv", "radr", "prox", "hit", "misl",
];

/// The lit cautions by their short names, NONE when all are clear.
fn cautions(state: &StateModel) -> FieldValue {
    if !state.in_vehicle() {
        return dash();
    }
    let mut lit = Vec::new();
    let mut warning = false;
    for k in CAUTIONS {
        let text = read_value(state, &format!("caution.{k}")).map(|v| v.text);
        match text.as_deref() {
            Some("WARNING") => {
                warning = true;
                lit.push(k.to_uppercase());
            }
            Some("CAUTION") => lit.push(k.to_uppercase()),
            _ => {}
        }
    }
    if lit.is_empty() {
        FieldValue::text("NONE", Tone::Ok)
    } else {
        FieldValue::text(lit.join(" "), if warning { Tone::Bad } else { Tone::Warn })
    }
}

fn weapons_ready(state: &StateModel) -> FieldValue {
    let section = state.read("weapons", None);
    let Some(weapons) = section
        .value
        .filter(|v| section.freshness == Freshness::Fresh && js::truthy(v))
        .and_then(|w| w.get("weapons"))
        .and_then(Value::as_array)
    else {
        return dash();
    };
    let ready = weapons
        .iter()
        .filter(|w| w.get("ready_to_fire").is_some_and(js::truthy))
        .count();
    let tone = match (ready, weapons.len()) {
        (_, 0) | (0, _) => Tone::Off,
        (r, n) if r == n => Tone::Ok,
        _ => Tone::Warn,
    };
    FieldValue::text(format!("{ready}/{}", weapons.len()), tone)
}

fn aboard(state: &StateModel) -> FieldValue {
    if state.in_vehicle() {
        FieldValue::text("IN SHIP", Tone::Ok)
    } else {
        FieldValue::text("NOT IN SHIP", Tone::Off)
    }
}

fn activity(state: &StateModel) -> FieldValue {
    let read = |path| {
        let r = state.read(path, None);
        r.value
            .filter(|_| r.freshness == Freshness::Fresh)
            .and_then(Value::as_str)
    };
    let role = match read("actor.state.crew_role") {
        Some("ACTOR_CREW_ROLE_PILOT") => ", PILOT",
        Some("ACTOR_CREW_ROLE_GUNNER") => ", GUNNER",
        Some("ACTOR_CREW_ROLE_CREW") => ", CREW",
        _ => "",
    };
    match read("actor.state.locomotion") {
        Some("ACTOR_LOCOMOTION_STATE_SEATED") => {
            FieldValue::text(format!("SEATED{role}"), Tone::Info)
        }
        Some("ACTOR_LOCOMOTION_STATE_ON_FOOT") => FieldValue::text("ON FOOT", Tone::Info),
        Some("ACTOR_LOCOMOTION_STATE_EVA") => FieldValue::text("EVA", Tone::Info),
        _ => dash(),
    }
}

fn list(items: Vec<Reading>) -> FieldValue {
    if items.is_empty() {
        return dash();
    }
    FieldValue {
        text: String::new(),
        tone: Tone::Info,
        fraction: None,
        items: Some(items),
    }
}

const POWER: [&str; 9] = [
    "powerplants",
    "engines",
    "weapons",
    "shield_generators",
    "coolers",
    "life_support",
    "gravity_generators",
    "sensors",
    "shield_emitters",
];

fn power(state: &StateModel) -> FieldValue {
    let items = POWER
        .iter()
        .filter_map(|k| read_value(state, &format!("power.{k}")))
        .filter(|v| v.freshness == Freshness::Fresh && v.text != DASH)
        .map(|v| Reading {
            label: v.label.to_owned(),
            tone: tone_of_text(&v.text, v.fraction, v.on),
            text: v.text,
            fraction: None,
        })
        .collect();
    list(items)
}

fn value_of(state: &StateModel, id: &str) -> FieldValue {
    match id {
        "ship.name" => text_of(state, "ship.name"),
        "ship.class" => text_of(state, "ship.class_name"),
        "ship.mode" => text_of(state, "ship.master_mode"),
        "ship.shields" => fraction_of(state, "ship.shields"),
        "ship.hull" => fraction_of(state, "ship.hull"),
        "ship.fuel" => fraction_of(state, "ship.hydrogen_fuel"),
        "ship.quantum" => fraction_of(state, "ship.quantum_fuel"),
        "ship.cautions" => cautions(state),
        "ship.weapons" => weapons_ready(state),
        "ship.aboard" => aboard(state),
        "ship.crew_role" => text_of(state, "actor.crew_role"),
        "ship.seat" => text_of(state, "actor.seat"),
        "char.health" => fraction_of(state, "actor.health"),
        "char.oxygen" => fraction_of(state, "actor.oxygen"),
        "char.activity" => activity(state),
        "detail.shield_faces" => list(shield_faces(state)),
        "detail.power" => power(state),
        "detail.speed" => text_of(state, "ship.speed"),
        "detail.altitude" => text_of(state, "ship.altitude"),
        _ => dash(),
    }
}

/// The health gauge as a 0..1 fraction while the actor section is fresh, for [`LifeTracker`].
pub fn health_fraction(state: &StateModel) -> Option<f64> {
    read_value(state, "actor.health")
        .filter(|v| v.freshness == Freshness::Fresh)
        .and_then(|v| v.fraction)
}

/// Build the card a sender sends: only fields that are both consented to and `wanted`.
pub fn build_card(
    state: &StateModel,
    life: &LifeTracker,
    now_ms: i64,
    sender: &Sender,
    wanted: &HashSet<&'static str>,
) -> Card {
    let fields = sendable(&sender.consent, |id| wanted.contains(id))
        .map(|f| (f.id, value_of(state, f.id)))
        .collect();
    let sharing = sender.consent.sharing();
    Card {
        schema: CARD_SCHEMA,
        member: sender.member.clone(),
        handle: sender.handle.clone(),
        sharing,
        state: sharing.character.then(|| life.life(now_ms)),
        fields: Fields(fields),
        source: None,
    }
}
