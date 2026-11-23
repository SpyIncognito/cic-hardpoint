//! The event's unit tree as the panel draws it: event, then fleets, wings and ground forces, then
//! ships, squadrons and squads, then a squadron's ships, each with its lead, its seats and a
//! roll-up that names what it counts ("5/6 Crew up"). Built from the structure as the server sent
//! it to this viewer, the cards received and the roster. Also the Unassigned bucket (names only,
//! never card data).

use serde::Serialize;

use fleet_proto::card::FieldValue;
use fleet_proto::rollup::{Rollup, Status, rollup, rollup_as};
use fleet_proto::structure::{EventStructure, Unit, UnitKind};
use fleet_proto::{Role, RosterEntry};
use sc_core::state::values::DASH;

use super::hub::MemberEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RoleTag {
    pub role: Role,
    pub label: &'static str,
    pub icon: &'static str,
}

impl From<Role> for RoleTag {
    fn from(role: Role) -> Self {
        Self {
            role,
            label: role.label(),
            icon: role.icon(),
        }
    }
}

/// A member as the tree names them: whether they are connected and whether a card came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub member: String,
    pub handle: String,
    /// Connected now; a member with no key in the roster is "not connected yet".
    pub connected: bool,
    /// A card of theirs has been received.
    pub card: bool,
    /// Seated anywhere in the event (a lead with no seat has their card under the unit).
    pub seated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatNode {
    #[serde(flatten)]
    pub person: Person,
    pub role: RoleTag,
    /// Their card names another ship class or name than the ship card: a hint, never a block.
    pub not_aboard: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitNode {
    pub id: String,
    pub kind: UnitKind,
    pub kind_label: &'static str,
    pub name: String,
    pub lead_role: RoleTag,
    pub leads: Vec<Person>,
    pub seats: Vec<SeatNode>,
    /// The ship card's member id, once one came. Never for a squadron's ship: there the Pilot's
    /// own card is the ship card.
    pub ship_card: Option<String>,
    /// A squadron's ship: drawn as its Pilot's card, then the Copilot's.
    pub fighter: bool,
    /// Whether this viewer sees inside: a unit above their own shows its name and lead only.
    pub open: bool,
    pub rollup: Rollup,
    pub units: Vec<UnitNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTree {
    pub id: String,
    pub name: String,
    pub admiral: Option<Person>,
    pub admiral_role: RoleTag,
    pub rollup: Rollup,
    pub units: Vec<UnitNode>,
}

pub struct Inputs<'a> {
    pub structure: &'a EventStructure,
    pub me: &'a str,
    pub members: &'a [MemberEntry],
    pub roster: &'a [RosterEntry],
}

impl Inputs<'_> {
    fn card(&self, member: &str) -> Option<&MemberEntry> {
        self.members.iter().find(|m| m.card.member == member)
    }

    fn status(&self, member: &str) -> Status {
        self.card(member).map_or(Status::Offline, |m| m.status)
    }

    pub fn person(&self, member: &str) -> Person {
        let entry = self.roster.iter().find(|r| r.member == member);
        let card = self.card(member);
        Person {
            member: member.to_owned(),
            handle: entry
                .and_then(|r| r.handle.clone())
                .or_else(|| card.map(|c| c.card.handle.clone()))
                .unwrap_or_else(|| member.to_owned()),
            connected: member == self.me
                || entry.map_or_else(
                    || card.is_some_and(|c| c.status != Status::Offline),
                    RosterEntry::connected,
                ),
            card: card.is_some(),
            seated: self.structure.seat_of(member).is_some(),
        }
    }

    pub fn tree(&self) -> EventTree {
        let s = self.structure;
        let visible = s.visible_units(self.me);
        let all_seats = s
            .units
            .iter()
            .flat_map(|u| u.seats.iter())
            .map(|seat| self.status(&seat.member));
        EventTree {
            id: s.event.id.clone(),
            name: s.event.name.clone(),
            admiral: s.admiral.as_deref().map(|a| self.person(a)),
            admiral_role: Role::Admiral.into(),
            rollup: rollup(all_seats),
            units: self.children(None, &visible),
        }
    }

    fn children(
        &self,
        parent: Option<&str>,
        visible: &std::collections::BTreeSet<String>,
    ) -> Vec<UnitNode> {
        self.structure
            .units
            .iter()
            .filter(|u| u.parent.as_deref() == parent)
            .map(|u| self.node(u, visible))
            .collect()
    }

    fn node(&self, u: &Unit, visible: &std::collections::BTreeSet<String>) -> UnitNode {
        let s = self.structure;
        let open = visible.contains(&u.id);
        let fighter = u.kind == UnitKind::Ship
            && s.parent_of(u).is_some_and(|p| p.kind == UnitKind::Squadron);
        let ship_card = (u.kind == UnitKind::Ship)
            .then(|| fleet_proto::card::ship_card_id(&u.id))
            .filter(|id| self.card(id).is_some());
        let ship_card = ship_card.filter(|_| !fighter);
        // A fighter's crew is checked against the card drawn for the ship: the Pilot's own.
        let ship = if fighter {
            u.seats
                .iter()
                .find(|seat| seat.role == Role::Pilot)
                .and_then(|seat| self.card(&seat.member))
        } else {
            ship_card.as_deref().and_then(|id| self.card(id))
        };
        let rollup_label = if fighter {
            UnitKind::Squadron.rollup_label()
        } else {
            u.kind.rollup_label()
        };
        let mut seats: Vec<SeatNode> = u
            .seats
            .iter()
            .filter(|_| open)
            .map(|seat| SeatNode {
                person: self.person(&seat.member),
                role: seat.role.into(),
                not_aboard: ship.is_some_and(|ship| {
                    self.card(&seat.member)
                        .is_some_and(|m| not_aboard(&m.card.fields, &ship.card.fields))
                }),
            })
            .collect();
        seats.sort_by_key(|seat| {
            (
                seat.role.role.rank(),
                self.status(&seat.person.member).rank(),
            )
        });
        let counted = s.rollup_members(&u.id);
        UnitNode {
            id: u.id.clone(),
            kind: u.kind,
            kind_label: u.kind.label(),
            name: u.name.clone(),
            lead_role: u.kind.lead_role().into(),
            leads: u.lead.iter().map(|m| self.person(m)).collect(),
            seats,
            ship_card,
            fighter,
            open,
            rollup: rollup_as(rollup_label, counted.iter().map(|m| self.status(m))),
            units: self.children(Some(&u.id), visible),
        }
    }

    pub fn unassigned(&self) -> Vec<Person> {
        self.structure
            .unassigned
            .iter()
            .map(|m| self.person(m))
            .collect()
    }
}

fn reported<'a>(fields: &'a fleet_proto::Fields, id: &str) -> Option<&'a FieldValue> {
    fields
        .get(id)
        .filter(|v| v.text != DASH && !v.text.is_empty())
}

/// The heuristic: the stream has no vehicle instance id, so a member whose ship class or name
/// differs from the ship card's is flagged, never left out.
pub fn not_aboard(member: &fleet_proto::Fields, ship: &fleet_proto::Fields) -> bool {
    ["ship.class", "ship.name"].iter().any(|id| {
        matches!(
            (reported(member, id), reported(ship, id)),
            (Some(a), Some(b)) if a.text != b.text
        )
    })
}
