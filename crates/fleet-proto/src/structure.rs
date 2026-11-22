//! An event's structure (plan sections 1 and 2): the event, its wings, its groups, and who holds
//! which role. The server keeps it; members receive the part that concerns them. Stored as a tree
//! (event, wing, group) so wings inside wings can come later without a protocol change; for now a
//! group belongs to at most one wing, so roll-ups never count anyone twice.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::identity::PublicKey;
use crate::roles::{Grant, Role, Scope};

pub const MAX_NAME: usize = 40;
pub const MAX_ID: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventState {
    /// Being set up; nobody can join.
    Draft,
    /// Join codes work; members attach and share.
    Open,
    /// No joins and no cards; structure kept so it can reopen.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInfo {
    pub id: String,
    pub name: String,
    pub state: EventState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wing {
    pub id: String,
    pub name: String,
    /// Group ids.
    pub groups: Vec<String>,
    pub commanders: Vec<PublicKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub leaders: Vec<PublicKey>,
    pub members: Vec<PublicKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStructure {
    pub event: EventInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admiral: Option<PublicKey>,
    #[serde(default)]
    pub wings: Vec<Wing>,
    #[serde(default)]
    pub groups: Vec<Group>,
}

pub fn is_id(s: &str) -> bool {
    (1..=MAX_ID).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn is_name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.chars().count())
        && !s.chars().any(|c| c.is_control() || c == '<' || c == '>')
}

impl EventStructure {
    /// What is wrong with it, empty when it is sound.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut check = |ok: bool, problem: String| {
            if !ok {
                problems.push(problem);
            }
        };
        check(is_id(&self.event.id), "bad event id".to_owned());
        check(is_name(&self.event.name), "bad event name".to_owned());

        let mut ids = HashSet::new();
        let mut members = HashSet::new();
        for g in &self.groups {
            check(is_id(&g.id), format!("bad group id {:?}", g.id));
            check(is_name(&g.name), format!("group {}: bad name", g.id));
            check(
                ids.insert(g.id.as_str()),
                format!("group {} repeated", g.id),
            );
            for m in &g.members {
                check(
                    members.insert(*m),
                    format!("member {} is in more than one group", m.fingerprint()),
                );
            }
        }
        let mut wing_ids = HashSet::new();
        let mut in_wing = HashSet::new();
        for w in &self.wings {
            check(is_id(&w.id), format!("bad wing id {:?}", w.id));
            check(is_name(&w.name), format!("wing {}: bad name", w.id));
            check(
                wing_ids.insert(w.id.as_str()),
                format!("wing {} repeated", w.id),
            );
            for g in &w.groups {
                check(
                    ids.contains(g.as_str()),
                    format!("wing {}: no group {g}", w.id),
                );
                check(
                    in_wing.insert(g.as_str()),
                    format!("group {g} is in more than one wing"),
                );
            }
        }
        problems
    }

    /// Every role `key` holds in this event.
    pub fn grants_of(&self, key: &PublicKey) -> BTreeSet<Grant> {
        let mut grants = BTreeSet::new();
        if self.admiral.as_ref() == Some(key) {
            grants.insert(Grant {
                role: Role::Admiral,
                scope: Scope::Event,
            });
        }
        for w in self.wings.iter().filter(|w| w.commanders.contains(key)) {
            grants.insert(Grant {
                role: Role::WingCommander,
                scope: Scope::Wing(w.id.clone()),
            });
        }
        for g in &self.groups {
            for (role, holds) in [
                (Role::GroupLeader, g.leaders.contains(key)),
                (Role::Member, g.members.contains(key)),
            ] {
                if holds {
                    grants.insert(Grant {
                        role,
                        scope: Scope::Group(g.id.clone()),
                    });
                }
            }
        }
        grants
    }

    /// The group ids whose cards `key` may receive: the union of their roles' scopes. Worked out
    /// again on every change to the structure, never cached per connection.
    pub fn visible_groups(&self, key: &PublicKey) -> BTreeSet<String> {
        let mut visible = BTreeSet::new();
        for grant in self.grants_of(key) {
            match grant.scope {
                Scope::Event => visible.extend(self.groups.iter().map(|g| g.id.clone())),
                Scope::Wing(id) => {
                    if let Some(w) = self.wings.iter().find(|w| w.id == id) {
                        visible.extend(w.groups.iter().cloned());
                    }
                }
                Scope::Group(id) => {
                    visible.insert(id);
                }
            }
        }
        visible
    }

    /// The group `key` is a member of.
    pub fn group_of(&self, key: &PublicKey) -> Option<&Group> {
        self.groups.iter().find(|g| g.members.contains(key))
    }
}

/// Changes to the structure, as the admin portal, the admiral or a leader asks for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum StructureChange {
    SetAdmiral {
        key: Option<PublicKey>,
    },
    AddWing {
        id: String,
        name: String,
    },
    RemoveWing {
        id: String,
    },
    AddGroup {
        id: String,
        name: String,
        wing: Option<String>,
    },
    RemoveGroup {
        id: String,
    },
    SetGroupWing {
        group: String,
        wing: Option<String>,
    },
    SetCommander {
        wing: String,
        key: PublicKey,
        on: bool,
    },
    SetLeader {
        group: String,
        key: PublicKey,
        on: bool,
    },
    /// Into a group, out of any other; `None` takes the member out of every group.
    Assign {
        key: PublicKey,
        group: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn key(n: u8) -> PublicKey {
        Identity::from_seed([n; 32]).public()
    }

    fn group(id: &str, leaders: &[u8], members: &[u8]) -> Group {
        Group {
            id: id.to_owned(),
            name: id.to_uppercase(),
            leaders: leaders.iter().map(|n| key(*n)).collect(),
            members: members.iter().map(|n| key(*n)).collect(),
        }
    }

    fn event() -> EventStructure {
        EventStructure {
            event: EventInfo {
                id: "op-1".to_owned(),
                name: "Op One".to_owned(),
                state: EventState::Open,
            },
            admiral: Some(key(1)),
            wings: vec![Wing {
                id: "air".to_owned(),
                name: "Air wing".to_owned(),
                groups: vec!["f1".to_owned(), "f2".to_owned()],
                commanders: vec![key(2)],
            }],
            groups: vec![
                group("f1", &[2], &[2, 3]),
                group("f2", &[4], &[4, 5]),
                group("ground", &[6], &[6, 7]),
            ],
        }
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn each_role_sees_its_scope_and_roles_add_up() {
        let e = event();
        assert!(e.problems().is_empty(), "{:?}", e.problems());
        assert_eq!(e.visible_groups(&key(1)), set(&["f1", "f2", "ground"]));
        // Commander of the air wing and member of f1.
        assert_eq!(e.visible_groups(&key(2)), set(&["f1", "f2"]));
        assert_eq!(e.grants_of(&key(2)).len(), 3);
        assert_eq!(e.visible_groups(&key(4)), set(&["f2"]));
        assert_eq!(e.visible_groups(&key(7)), set(&["ground"]));
        assert!(e.visible_groups(&key(99)).is_empty());
        assert_eq!(e.group_of(&key(5)).map(|g| g.id.as_str()), Some("f2"));
    }

    #[test]
    fn problems_name_what_is_wrong() {
        let mut e = event();
        e.wings.push(Wing {
            id: "ground-w".to_owned(),
            name: "Ground".to_owned(),
            groups: vec!["f1".to_owned(), "nope".to_owned()],
            commanders: vec![],
        });
        e.groups[2].members.push(key(3));
        let p = e.problems();
        assert!(p.iter().any(|p| p.contains("more than one group")), "{p:?}");
        assert!(
            p.iter().any(|p| p == "group f1 is in more than one wing"),
            "{p:?}"
        );
        assert!(
            p.iter().any(|p| p == "wing ground-w: no group nope"),
            "{p:?}"
        );
    }

    #[test]
    fn structure_round_trips_through_json() {
        let e = event();
        let text = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<EventStructure>(&text).unwrap(), e);
        let change = StructureChange::Assign {
            key: key(3),
            group: Some("f2".to_owned()),
        };
        let text = serde_json::to_string(&change).unwrap();
        assert!(text.starts_with(r#"{"op":"assign""#), "{text}");
        assert_eq!(
            serde_json::from_str::<StructureChange>(&text).unwrap(),
            change
        );
    }
}
