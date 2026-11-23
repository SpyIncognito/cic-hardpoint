//! Roles in an event (plan section 10.11). A role belongs to a member id (the CIC account's user
//! id on a relay, the key's text form peer to peer), never to the handle, and roles add up: a
//! squadron commander who flies one of the squadron's ships holds both grants.
//!
//! Two kinds of role: a lead role over a unit (or the whole event, for the admiral), and a seat
//! in the one ship or squad a member sits in.

use serde::{Deserialize, Serialize};

use crate::structure::UnitKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// One per event: everything.
    Admiral,
    FleetCommander,
    WingCommander,
    GroundCommander,
    SquadronCommander,
    SquadCommander,
    /// Leads a ship in a fleet; also sits in it, as Pilot, Copilot or Crew.
    Captain,
    /// Exactly one per ship, and the ship card's source.
    Pilot,
    Copilot,
    /// Every other seat on a ship in a fleet.
    Crew,
    /// A seat in a squad.
    Soldier,
}

impl Role {
    pub const ALL: [Self; 11] = [
        Self::Admiral,
        Self::FleetCommander,
        Self::WingCommander,
        Self::GroundCommander,
        Self::SquadronCommander,
        Self::SquadCommander,
        Self::Captain,
        Self::Pilot,
        Self::Copilot,
        Self::Crew,
        Self::Soldier,
    ];

    /// Higher in the hierarchy first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Admiral => 0,
            Self::FleetCommander | Self::WingCommander | Self::GroundCommander => 1,
            Self::SquadronCommander | Self::SquadCommander | Self::Captain => 2,
            Self::Pilot => 3,
            Self::Copilot => 4,
            Self::Crew | Self::Soldier => 5,
        }
    }

    /// The name both sides show.
    pub fn label(self) -> &'static str {
        match self {
            Self::Admiral => "Admiral",
            Self::FleetCommander => "Fleet Commander",
            Self::WingCommander => "Wing Commander",
            Self::GroundCommander => "Ground Commander",
            Self::SquadronCommander => "Squadron Commander",
            Self::SquadCommander => "Squad Commander",
            Self::Captain => "Captain",
            Self::Pilot => "Pilot",
            Self::Copilot => "Copilot",
            Self::Crew => "Crew",
            Self::Soldier => "Soldier",
        }
    }

    /// The icon id both sides draw: the wire name with `-` for `_`.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Admiral => "admiral",
            Self::FleetCommander => "fleet-commander",
            Self::WingCommander => "wing-commander",
            Self::GroundCommander => "ground-commander",
            Self::SquadronCommander => "squadron-commander",
            Self::SquadCommander => "squad-commander",
            Self::Captain => "captain",
            Self::Pilot => "pilot",
            Self::Copilot => "copilot",
            Self::Crew => "crew",
            Self::Soldier => "soldier",
        }
    }

    /// The unit kind this role leads, for a lead role other than the admiral.
    pub fn leads(self) -> Option<UnitKind> {
        match self {
            Self::FleetCommander => Some(UnitKind::Fleet),
            Self::WingCommander => Some(UnitKind::Wing),
            Self::GroundCommander => Some(UnitKind::Ground),
            Self::SquadronCommander => Some(UnitKind::Squadron),
            Self::SquadCommander => Some(UnitKind::Squad),
            Self::Captain => Some(UnitKind::Ship),
            _ => None,
        }
    }

    /// A seat a member sits in, as opposed to a lead role.
    pub fn is_seat(self) -> bool {
        matches!(
            self,
            Self::Pilot | Self::Copilot | Self::Crew | Self::Soldier
        )
    }

    /// A lead role: the admiral, a commander or a captain.
    pub fn has_admiral_view(self) -> bool {
        !self.is_seat()
    }

    /// The admiral and the commanders, who also see the Unassigned bucket. Captains do not.
    pub fn is_commander(self) -> bool {
        self.has_admiral_view() && self != Self::Captain
    }
}

/// What one role covers: the whole event, or one unit and everything under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum Scope {
    Event,
    Unit(String),
}

/// One role held by one member.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Grant {
    pub role: Role,
    pub scope: Scope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_have_wire_names_labels_and_icons() {
        for role in Role::ALL {
            let wire = serde_json::to_string(&role).unwrap();
            assert_eq!(wire.trim_matches('"').replace('_', "-"), role.icon());
            assert_eq!(serde_json::from_str::<Role>(&wire).unwrap(), role);
            assert!(!role.label().is_empty());
        }
        assert_eq!(
            serde_json::to_string(&Role::SquadronCommander).unwrap(),
            "\"squadron_commander\""
        );
        assert_eq!(Role::FleetCommander.label(), "Fleet Commander");
        assert!(Role::Captain.has_admiral_view() && !Role::Captain.is_commander());
        assert!(Role::Pilot.is_seat() && !Role::Pilot.has_admiral_view());
        let grant = Grant {
            role: Role::Pilot,
            scope: Scope::Unit("red-1".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&grant).unwrap(),
            r#"{"role":"pilot","scope":{"kind":"unit","id":"red-1"}}"#
        );
    }
}
