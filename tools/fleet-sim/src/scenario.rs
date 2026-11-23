//! Who sits where: a member as a ship's pilot, copilot or crew, written into its game frames as
//! the game would show it (ship class and name, crew role, seat name), so the cards carry
//! `ship.class`, `ship.crew_role` and `ship.seat` and a relay's ship-card sourcing has something
//! to choose between. And the pilot drop, which takes each pilot off line for a while every
//! cycle so the relay falls back to the copilot or a member standing in.
//!
//! The seats here only shape the cards. The relay's own structure (seats set in the CIC portal)
//! decides who the source is, so seat the members there to match.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde_json::{Value, json};

use sc_game::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatRole {
    Pilot,
    Copilot,
    Crew,
}

impl SeatRole {
    /// The game's crew role for this seat. The game has no copilot role, so a copilot reads
    /// as CREW; crew read as GUNNER, so the three are told apart on a card.
    pub fn crew_role(self) -> &'static str {
        match self {
            Self::Pilot => "ACTOR_CREW_ROLE_PILOT",
            Self::Copilot => "ACTOR_CREW_ROLE_CREW",
            Self::Crew => "ACTOR_CREW_ROLE_GUNNER",
        }
    }

    pub fn seat_name(self) -> &'static str {
        match self {
            Self::Pilot => "Pilot",
            Self::Copilot => "Copilot",
            Self::Crew => "Turret",
        }
    }
}

impl FromStr for SeatRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pilot" => Ok(Self::Pilot),
            "copilot" => Ok(Self::Copilot),
            "crew" => Ok(Self::Crew),
            other => Err(format!("{other:?} is not a seat (pilot, copilot or crew)")),
        }
    }
}

impl fmt::Display for SeatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pilot => "pilot",
            Self::Copilot => "copilot",
            Self::Crew => "crew",
        })
    }
}

/// One ship from `--ship CLASS:pilot,copilot,crew`: its class name and its seats in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ship {
    pub class: String,
    pub seats: Vec<SeatRole>,
}

const MAX_CLASS: usize = 64;

impl FromStr for Ship {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (class, seats) = s
            .split_once(':')
            .ok_or_else(|| format!("{s:?} is not CLASS:pilot,copilot,crew"))?;
        let class = class.trim();
        if !(1..=MAX_CLASS).contains(&class.len())
            || !class
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!(
                "{class:?} is not a ship class (1 to {MAX_CLASS} of A-Z a-z 0-9 _)"
            ));
        }
        let seats = seats
            .split(',')
            .map(str::parse)
            .collect::<Result<Vec<SeatRole>, _>>()?;
        for role in [SeatRole::Pilot, SeatRole::Copilot] {
            if seats.iter().filter(|r| **r == role).count() > 1 {
                return Err(format!("a ship has at most one {role}"));
            }
        }
        Ok(Self {
            class: class.to_owned(),
            seats,
        })
    }
}

/// One member's seat, as its frames show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seating {
    pub class: String,
    pub ship_name: String,
    pub role: SeatRole,
}

/// The ship name a class reads as: `RSI_Constellation_Andromeda` is `Constellation Andromeda`.
pub fn ship_name(class: &str) -> String {
    let model = match class.split_once('_') {
        Some((maker, model))
            if !model.is_empty() && maker.bytes().all(|b| b.is_ascii_uppercase()) =>
        {
            model
        }
        _ => class,
    };
    model.replace('_', " ")
}

/// The seats of every ship, in order, one per member from the first; members past the last
/// seat keep what their recording says.
pub fn seat_members(ships: &[Ship]) -> Vec<Seating> {
    ships
        .iter()
        .flat_map(|ship| {
            ship.seats.iter().map(|&role| Seating {
                class: ship.class.clone(),
                ship_name: ship_name(&ship.class),
                role,
            })
        })
        .collect()
}

impl Seating {
    /// The crew role and seat always (a seated member); the ship's class and name only while
    /// the recording is in a ship, so a member on foot stays out of it.
    pub fn apply(&self, frame: &mut Frame) {
        let Some(object) = frame.as_object_mut() else {
            return;
        };
        let actor = object.entry("actor").or_insert_with(|| json!({}));
        if let Some(actor) = actor.as_object_mut() {
            let state = actor.entry("state").or_insert_with(|| json!({}));
            if let Some(state) = state.as_object_mut() {
                state.insert("crew_role".to_owned(), json!(self.role.crew_role()));
                state.insert("seat_name".to_owned(), json!(self.role.seat_name()));
            }
        }
        if let Some(vehicle) = object
            .get_mut("vehicle")
            .and_then(|v| v.get_mut("state"))
            .and_then(Value::as_object_mut)
        {
            vehicle.insert("class_name".to_owned(), json!(self.class));
            vehicle.insert("name".to_owned(), json!(self.ship_name));
        }
    }
}

/// Each pilot drops off line for `down`, starting halfway through every `cycle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PilotDrop {
    pub cycle: Duration,
    pub down: Duration,
}

impl PilotDrop {
    pub fn dropped(&self, elapsed: Duration) -> bool {
        if self.cycle.is_zero() {
            return false;
        }
        let at = Duration::from_nanos(
            u64::try_from(elapsed.as_nanos() % self.cycle.as_nanos()).unwrap_or(0),
        );
        let from = self.cycle / 2;
        at >= from && at < from + self.down
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use fleet_proto::FIELDS;
    use sc_core::state::StateModel;
    use sc_service::fleet::card::{LifeTracker, Sender, build_card};

    #[test]
    fn ships_are_read_and_checked() {
        let ship: Ship = "RSI_Constellation_Andromeda:pilot,copilot,crew,crew"
            .parse()
            .unwrap();
        assert_eq!(ship.class, "RSI_Constellation_Andromeda");
        assert_eq!(
            ship.seats,
            [
                SeatRole::Pilot,
                SeatRole::Copilot,
                SeatRole::Crew,
                SeatRole::Crew
            ]
        );
        assert!("ANVL_Arrow".parse::<Ship>().is_err());
        assert!("ANVL_Arrow:pilot,pilot".parse::<Ship>().is_err());
        assert!("ANVL Arrow:pilot".parse::<Ship>().is_err());
        assert!("ANVL_Arrow:gunner".parse::<Ship>().is_err());
        assert_eq!(ship_name("ANVL_Arrow"), "Arrow");
        assert_eq!(ship_name("Cutter"), "Cutter");
    }

    #[test]
    fn members_are_seated_in_order() {
        let ships: Vec<Ship> = ["DRAK_Cutlass_Black:pilot,crew", "ANVL_Arrow:pilot"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let seats = seat_members(&ships);
        let roles: Vec<_> = seats
            .iter()
            .map(|s| (s.ship_name.as_str(), s.role))
            .collect();
        assert_eq!(
            roles,
            [
                ("Cutlass Black", SeatRole::Pilot),
                ("Cutlass Black", SeatRole::Crew),
                ("Arrow", SeatRole::Pilot)
            ]
        );
    }

    fn card_of(frame: &Frame) -> fleet_proto::Card {
        let mut state = StateModel::default();
        state.ingest(frame);
        let mut consent = fleet_proto::SendConsent::none();
        for f in FIELDS {
            consent.set(f.id, true);
        }
        let sender = Sender {
            member: "m".to_owned(),
            handle: "Demo Vega".to_owned(),
            consent,
        };
        let wanted: HashSet<&'static str> =
            ["ship.class", "ship.crew_role", "ship.seat", "ship.name"]
                .into_iter()
                .collect();
        build_card(&state, &LifeTracker::default(), 0, &sender, &wanted)
    }

    fn text(card: &fleet_proto::Card, id: &str) -> String {
        card.fields
            .get(id)
            .map(|v| v.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_seat_reaches_the_card_as_ship_class_crew_role_and_seat() {
        let recorded = || {
            json!({
                "vehicle": { "state": { "name": "Ship", "class_name": "SHIP_CLASS" } },
                "actor": { "state": { "crew_role": "ACTOR_CREW_ROLE_PILOT", "seat_name": "Seat" } },
            })
        };
        let card = card_of(&recorded());
        assert_eq!(
            text(&card, "ship.class"),
            "SHIP_CLASS",
            "the recording as it is"
        );

        let expect = [
            (SeatRole::Pilot, "PILOT", "Pilot"),
            (SeatRole::Copilot, "CREW", "Copilot"),
            (SeatRole::Crew, "GUNNER", "Turret"),
        ];
        for (role, crew_role, seat) in expect {
            let mut frame = recorded();
            Seating {
                class: "RSI_Constellation_Andromeda".to_owned(),
                ship_name: "Constellation Andromeda".to_owned(),
                role,
            }
            .apply(&mut frame);
            let card = card_of(&frame);
            assert_eq!(text(&card, "ship.class"), "RSI_Constellation_Andromeda");
            assert_eq!(text(&card, "ship.name"), "Constellation Andromeda");
            assert_eq!(text(&card, "ship.crew_role"), crew_role);
            assert_eq!(text(&card, "ship.seat"), seat);
        }

        let mut on_foot = json!({ "actor": { "vitals": {} } });
        Seating {
            class: "ANVL_Arrow".to_owned(),
            ship_name: "Arrow".to_owned(),
            role: SeatRole::Pilot,
        }
        .apply(&mut on_foot);
        assert!(on_foot.get("vehicle").is_none());
        assert_eq!(on_foot["actor"]["state"]["seat_name"], "Pilot");
    }

    #[test]
    fn a_pilot_drops_halfway_through_each_cycle() {
        let drop = PilotDrop {
            cycle: Duration::from_secs(10),
            down: Duration::from_secs(2),
        };
        let at = |s: f64| drop.dropped(Duration::from_secs_f64(s));
        assert!(!at(0.0) && !at(4.9) && at(5.0) && at(6.9) && !at(7.0) && at(15.5));
    }
}
