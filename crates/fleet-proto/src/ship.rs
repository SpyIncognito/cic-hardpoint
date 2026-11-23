//! The ship card (plan section 10.11): one card per ship, built by the relay (or a peer-to-peer
//! host) from the cards its seated members already send, with the id `ship:<unit id>`.
//!
//! Source, in order: the ship's Pilot; the Copilot; the freshest other seated member, "standing
//! in". A source is eligible when its card is fresh, carries ship fields, does not say
//! `NOT IN SHIP` and its own state is not DOWNED or DEAD. With none, a fresh Pilot's card that is
//! DOWNED or DEAD stays the source, so the viewer shows the Pilot's state rather than the last
//! values; else the last live values stay, greyed, as "no report for N s" until the stale window
//! ends, then the card is OFFLINE. Fresh seated cards that carry no ship fields make "ship status
//! not shared".
//!
//! Only the source's ship fields are copied, so the card holds nothing the source did not choose
//! to send; and its audience is [`crate::structure::EventStructure::ship_card_audience`], inside
//! the source's own. The relay logs none of the fields.

use serde::{Deserialize, Serialize};

use crate::card::{CARD_SCHEMA, Card, Consent, Fields, Life, Sharing, field_def, ship_card_id};
use crate::roles::Role;
use crate::structure::{EventStructure, MemberId, UnitKind};

/// A source's card older than this is not used live.
pub const SOURCE_FRESH_MS: u64 = 3_000;
/// After this with no live source, the ship card is OFFLINE (the viewer's stale window).
pub const SHIP_STALE_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Pilot,
    Copilot,
    StandingIn,
    /// The last live values, kept greyed.
    Stale,
    Offline,
    NotShared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipSource {
    pub kind: SourceKind,
    /// The member the fields come from (the last one, when stale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The source's seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Stale: milliseconds since the last live report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
}

impl ShipSource {
    pub fn offline() -> Self {
        Self {
            kind: SourceKind::Offline,
            member: None,
            handle: None,
            role: None,
            age_ms: None,
        }
    }

    /// The line the card shows under the ship's name.
    pub fn label(&self) -> String {
        let handle = self.handle.as_deref().unwrap_or("?");
        match self.kind {
            SourceKind::Pilot | SourceKind::Copilot => {
                format!("via {handle} ({})", self.role.map_or("Pilot", Role::label))
            }
            SourceKind::StandingIn => format!("via {handle}, standing in"),
            SourceKind::Stale => format!("no report for {} s", self.age_ms.unwrap_or(0) / 1000),
            SourceKind::Offline => "OFFLINE".to_owned(),
            SourceKind::NotShared => "ship status not shared".to_owned(),
        }
    }
}

fn shares_ship(card: &Card) -> bool {
    card.sharing.ship
        && card
            .fields
            .ids()
            .any(|id| field_def(id).is_some_and(|f| f.consent == Consent::Ship))
}

fn out_of_action(card: &Card) -> bool {
    matches!(card.state, Some(Life::Downed | Life::Dead))
}

fn aboard(card: &Card) -> bool {
    card.fields
        .get("ship.aboard")
        .is_none_or(|v| v.text != "NOT IN SHIP")
}

/// The ship card for `unit` now. `reports` are the latest cards of the event's members (with
/// `member` as the structure knows them) and their ages in milliseconds; `last` is the last live
/// ship card this returned for the unit (Pilot, Copilot or standing in) and its age. `None` when `unit` is not a ship.
pub fn ship_card(
    structure: &EventStructure,
    unit: &str,
    reports: &[(&Card, u64)],
    last: Option<(&Card, u64)>,
) -> Option<Card> {
    let ship = structure.unit(unit).filter(|u| u.kind == UnitKind::Ship)?;
    let seated: Vec<(&Card, u64, Role)> = ship
        .seats
        .iter()
        .filter_map(|s| {
            reports
                .iter()
                .find(|(c, _)| c.member == s.member)
                .map(|(c, age)| (*c, *age, s.role))
        })
        .collect();
    let eligible = |(c, age, _): &&(&Card, u64, Role)| {
        *age <= SOURCE_FRESH_MS && shares_ship(c) && aboard(c) && !out_of_action(c)
    };
    let pick = seated
        .iter()
        .filter(eligible)
        .find(|s| s.2 == Role::Pilot)
        .map(|s| (s, SourceKind::Pilot))
        .or_else(|| {
            seated
                .iter()
                .filter(eligible)
                .find(|s| s.2 == Role::Copilot)
                .map(|s| (s, SourceKind::Copilot))
        })
        .or_else(|| {
            seated
                .iter()
                .filter(eligible)
                .min_by_key(|s| s.1)
                .map(|s| (s, SourceKind::StandingIn))
        })
        // The ship card carries no state, so the viewer reads the Pilot's from their own card.
        .or_else(|| {
            seated
                .iter()
                .find(|(c, age, role)| {
                    *role == Role::Pilot && *age <= SOURCE_FRESH_MS && out_of_action(c)
                })
                .map(|s| (s, SourceKind::Pilot))
        });

    let mut card = Card {
        schema: CARD_SCHEMA,
        member: ship_card_id(unit),
        handle: ship.name.clone(),
        sharing: Sharing::default(),
        state: None,
        fields: Fields::default(),
        source: None,
    };
    if let Some(((source, _, role), kind)) = pick {
        if shares_ship(source) && aboard(source) {
            card.sharing.ship = true;
            card.fields = source.fields.clone();
            card.fields
                .retain(|id| field_def(id).is_some_and(|f| f.consent == Consent::Ship));
        }
        card.source = Some(ShipSource {
            kind,
            member: Some(source.member.clone()),
            handle: Some(source.handle.clone()),
            role: Some(*role),
            age_ms: None,
        });
        return Some(card);
    }
    let was_live = |c: &Card| {
        c.source.as_ref().is_some_and(|s| {
            matches!(
                s.kind,
                SourceKind::Pilot | SourceKind::Copilot | SourceKind::StandingIn
            )
        })
    };
    if let Some((previous, age)) = last.filter(|(c, age)| *age < SHIP_STALE_MS && was_live(c)) {
        let mut stale = previous.clone();
        if let Some(s) = stale.source.as_mut() {
            s.kind = SourceKind::Stale;
            s.age_ms = Some(age);
        }
        return Some(stale);
    }
    let fresh_but_private = seated
        .iter()
        .any(|(c, age, _)| *age <= SOURCE_FRESH_MS && !shares_ship(c));
    card.source = Some(if fresh_but_private {
        ShipSource {
            kind: SourceKind::NotShared,
            ..ShipSource::offline()
        }
    } else {
        ShipSource::offline()
    });
    Some(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::{FieldValue, Tone};
    use crate::structure::tests::event;

    fn report(member: &str, handle: &str, ship: bool, aboard: bool) -> Card {
        let mut fields = vec![("char.health", FieldValue::text("100%", Tone::Ok))];
        if ship {
            fields.insert(
                0,
                (
                    "ship.hull",
                    FieldValue::text(format!("hull of {member}"), Tone::Ok),
                ),
            );
            fields.insert(
                1,
                (
                    "ship.aboard",
                    FieldValue::text(if aboard { "IN SHIP" } else { "NOT IN SHIP" }, Tone::Info),
                ),
            );
        }
        Card {
            schema: CARD_SCHEMA,
            member: member.to_owned(),
            handle: handle.to_owned(),
            sharing: Sharing {
                ship,
                character: true,
                detail: false,
            },
            state: Some(crate::card::Life::Up),
            fields: Fields(fields),
            source: None,
        }
    }

    fn source(card: &Card) -> &ShipSource {
        card.source.as_ref().unwrap()
    }

    #[test]
    fn the_pilot_is_the_source_then_the_copilot_then_someone_standing_in() {
        let e = event();
        let hp = report("hp", "Rook", true, true);
        let cp = report("cp", "Wren", true, true);
        let c1 = report("c1", "Kite", true, true);
        let cap = report("cap", "Hawk", true, true);

        let card = ship_card(&e, "carrack", &[(&c1, 100), (&hp, 400), (&cp, 0)], None).unwrap();
        assert_eq!(card.member, "ship:carrack");
        assert_eq!(card.handle, "CARRACK");
        assert_eq!(source(&card).kind, SourceKind::Pilot);
        assert_eq!(source(&card).label(), "via Rook (Pilot)");
        assert_eq!(card.fields.get("ship.hull").unwrap().text, "hull of hp");
        // Ship fields only: the pilot's health and state stay on the pilot's own card.
        assert_eq!(
            card.fields.ids().collect::<Vec<_>>(),
            ["ship.hull", "ship.aboard"]
        );
        assert_eq!(card.state, None);
        assert!(card.check().is_ok(), "{:?}", card.check());

        let stale_pilot = (&hp, SOURCE_FRESH_MS + 1);
        let card = ship_card(&e, "carrack", &[stale_pilot, (&cp, 0), (&c1, 0)], None).unwrap();
        assert_eq!(source(&card).label(), "via Wren (Copilot)");

        let out = report("hp", "Rook", true, false);
        let card = ship_card(&e, "carrack", &[(&out, 0), (&c1, 900), (&cap, 200)], None).unwrap();
        assert_eq!(source(&card).kind, SourceKind::StandingIn);
        assert_eq!(source(&card).label(), "via Hawk, standing in");
        assert_eq!(source(&card).role, Some(Role::Crew));

        // A member not seated in this ship is never a source.
        let p2 = report("p2", "Lone", true, true);
        let card = ship_card(&e, "carrack", &[(&p2, 0)], None).unwrap();
        assert_eq!(source(&card).kind, SourceKind::Offline);
    }

    fn in_state(mut card: Card, state: Life) -> Card {
        card.state = Some(state);
        card
    }

    #[test]
    fn a_downed_or_dead_member_is_not_a_source() {
        let e = event();
        let dead_pilot = in_state(report("hp", "Rook", true, true), Life::Dead);
        let cp = report("cp", "Wren", true, true);
        let c1 = report("c1", "Kite", true, true);
        let downed_cp = in_state(cp.clone(), Life::Downed);

        let card = ship_card(&e, "carrack", &[(&dead_pilot, 0), (&cp, 0)], None).unwrap();
        assert_eq!(source(&card).label(), "via Wren (Copilot)");

        let reports = [(&dead_pilot, 0), (&downed_cp, 0), (&c1, 500)];
        let card = ship_card(&e, "carrack", &reports, None).unwrap();
        assert_eq!(source(&card).label(), "via Kite, standing in");

        let respawned = in_state(report("hp", "Rook", true, true), Life::Respawned);
        let card = ship_card(&e, "carrack", &[(&respawned, 0), (&cp, 0)], None).unwrap();
        assert_eq!(source(&card).label(), "via Rook (Pilot)");
    }

    #[test]
    fn with_no_eligible_source_a_fresh_pilot_shows_their_own_state() {
        let e = event();
        let live = ship_card(&e, "red-2", &[(&report("r2", "Vega", true, true), 0)], None).unwrap();
        let dead = in_state(report("r2", "Vega", true, true), Life::Dead);
        let downed_cp = in_state(report("r2c", "Lyra", true, true), Life::Downed);

        // Before the stale values: the Pilot's fresh card says what happened to the ship.
        let reports = [(&dead, 200), (&downed_cp, 0)];
        let card = ship_card(&e, "red-2", &reports, Some((&live, 1_000))).unwrap();
        assert_eq!(source(&card).kind, SourceKind::Pilot);
        assert_eq!(source(&card).member.as_deref(), Some("r2"));
        assert_eq!(source(&card).label(), "via Vega (Pilot)");
        assert_eq!(card.state, None);
        assert_eq!(card.fields.get("ship.hull").unwrap().text, "hull of r2");
        assert!(card.check().is_ok(), "{:?}", card.check());

        let downed = in_state(report("r2", "Vega", false, false), Life::Downed);
        let card = ship_card(&e, "red-2", &[(&downed, 0)], None).unwrap();
        assert_eq!(source(&card).label(), "via Vega (Pilot)");
        assert!(card.fields.0.is_empty() && !card.sharing.ship);
        assert!(card.check().is_ok());

        // Only the Pilot stands for the ship this way, and only while fresh.
        let dead_cp = in_state(report("r2c", "Lyra", true, true), Life::Dead);
        let card = ship_card(&e, "red-2", &[(&dead_cp, 0)], None).unwrap();
        assert_eq!(source(&card).kind, SourceKind::Offline);
        let old = (&dead, SOURCE_FRESH_MS + 1);
        let card = ship_card(&e, "red-2", &[old], Some((&live, 4_000))).unwrap();
        assert_eq!(source(&card).kind, SourceKind::Stale);
    }

    #[test]
    fn with_no_live_source_the_card_goes_stale_then_offline() {
        let e = event();
        let r2 = report("r2", "Vega", true, true);
        let live = ship_card(&e, "red-2", &[(&r2, 0)], None).unwrap();
        assert_eq!(source(&live).kind, SourceKind::Pilot);

        let stale = ship_card(&e, "red-2", &[(&r2, 5_000)], Some((&live, 4_200))).unwrap();
        assert_eq!(source(&stale).kind, SourceKind::Stale);
        assert_eq!(source(&stale).label(), "no report for 4 s");
        assert_eq!(stale.fields, live.fields);
        assert_eq!(source(&stale).handle.as_deref(), Some("Vega"));
        assert!(stale.check().is_ok());

        let still = ship_card(&e, "red-2", &[], Some((&live, 9_000))).unwrap();
        assert_eq!(source(&still).age_ms, Some(9_000));
        // Only a live card is kept as the last values.
        assert_eq!(
            source(&ship_card(&e, "red-2", &[], Some((&still, 1))).unwrap()).kind,
            SourceKind::Offline
        );

        let gone = ship_card(&e, "red-2", &[], Some((&live, SHIP_STALE_MS))).unwrap();
        assert_eq!(source(&gone).label(), "OFFLINE");
        assert!(gone.fields.0.is_empty() && !gone.sharing.ship);
        assert!(gone.check().is_ok());
    }

    #[test]
    fn a_crew_that_shares_no_ship_fields_says_so() {
        let e = event();
        let private = report("r2", "Vega", false, true);
        let card = ship_card(&e, "red-2", &[(&private, 0)], None).unwrap();
        assert_eq!(source(&card).label(), "ship status not shared");
        assert!(card.fields.0.is_empty());
        assert!(ship_card(&e, "red", &[], None).is_none());
        assert!(ship_card(&e, "nope", &[], None).is_none());
    }

    #[test]
    fn the_source_round_trips() {
        let s = ShipSource {
            kind: SourceKind::StandingIn,
            member: Some("u-1".to_owned()),
            handle: Some("Kite".to_owned()),
            role: Some(Role::Crew),
            age_ms: None,
        };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(
            text,
            r#"{"kind":"standing_in","member":"u-1","handle":"Kite","role":"crew"}"#
        );
        assert_eq!(serde_json::from_str::<ShipSource>(&text).unwrap(), s);
    }
}
