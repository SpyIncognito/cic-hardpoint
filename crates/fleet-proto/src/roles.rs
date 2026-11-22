//! Roles in an event (plan section 1): admiral, wing commander, group leader, member. Roles bind
//! to a member's key, never to the handle, and add up: someone who commands a wing and leads a
//! group in it receives the union of both.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// One per event: every group.
    Admiral,
    /// One or more per wing: that wing's groups.
    WingCommander,
    /// One or more per group: that group.
    GroupLeader,
    /// Their own group.
    Member,
}

impl Role {
    /// Higher in the hierarchy first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Admiral => 0,
            Self::WingCommander => 1,
            Self::GroupLeader => 2,
            Self::Member => 3,
        }
    }

    /// Whether this role gets the admiral view of its scope.
    pub fn has_admiral_view(self) -> bool {
        self != Self::Member
    }
}

/// What one role covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum Scope {
    Event,
    Wing(String),
    Group(String),
}

/// One role held by one member.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Grant {
    pub role: Role,
    pub scope: Scope,
}
