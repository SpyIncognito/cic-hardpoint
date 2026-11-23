//! An event's structure (plan section 10.11): one tree of units, each with a kind, a parent and
//! the members who lead it or sit in it.
//!
//! ```text
//! event (admiral)
//! ├── fleet (fleet commander)
//! │   └── ship (captain; pilot, copilot, crew)
//! ├── wing (wing commander)
//! │   └── squadron (squadron commander)
//! │       └── ship (pilot, copilot): one per pilot
//! └── ground (ground commander)
//!     └── squad (squad commander; soldiers)
//! ```
//!
//! Every craft is a ship. A member sits in at most one ship or squad, so roll-ups never count
//! anyone twice; one who sits nowhere and leads nothing is Unassigned. Members are member ids:
//! the CIC account's user id on a relay, the key's text form peer to peer. The relay keeps the
//! structure; each member receives [`EventStructure::view_for`] themselves.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::card::is_member_id;
use crate::roles::{Grant, Role, Scope};

pub const MAX_NAME: usize = 40;
pub const MAX_ID: usize = 40;
pub const MAX_UNITS: usize = 200;
/// The unit ids of [`EventStructure::p2p_default`].
pub const P2P_WING: &str = "wing";
pub const P2P_SQUADRON: &str = "squadron";

/// A member id: 1 to 64 of `A-Z a-z 0-9 _ -` ([`is_member_id`]).
pub type MemberId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventState {
    /// Being set up; nobody can connect.
    Draft,
    /// Members connect and share.
    Open,
    /// No connections and no cards; structure kept so it can reopen.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInfo {
    pub id: String,
    pub name: String,
    pub state: EventState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Fleet,
    Wing,
    Ground,
    Squadron,
    Ship,
    Squad,
}

impl UnitKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fleet => "Fleet",
            Self::Wing => "Wing",
            Self::Ground => "Ground",
            Self::Squadron => "Squadron",
            Self::Ship => "Ship",
            Self::Squad => "Squad",
        }
    }

    /// The kinds this kind may sit under; `None` is the event itself.
    pub fn parents(self) -> &'static [Option<UnitKind>] {
        match self {
            Self::Fleet | Self::Wing | Self::Ground => &[None],
            Self::Squadron => &[Some(Self::Wing)],
            Self::Ship => &[Some(Self::Fleet), Some(Self::Squadron)],
            Self::Squad => &[Some(Self::Ground)],
        }
    }

    /// The role that leads a unit of this kind.
    pub fn lead_role(self) -> Role {
        match self {
            Self::Fleet => Role::FleetCommander,
            Self::Wing => Role::WingCommander,
            Self::Ground => Role::GroundCommander,
            Self::Squadron => Role::SquadronCommander,
            Self::Ship => Role::Captain,
            Self::Squad => Role::SquadCommander,
        }
    }

    /// The word in this kind's roll-up: "5/6 Crew up", "3/4 Pilots up", "7/8 Soldiers up".
    pub fn rollup_label(self) -> &'static str {
        match self {
            Self::Fleet | Self::Ship => "Crew",
            Self::Wing | Self::Squadron => "Pilots",
            Self::Ground | Self::Squad => "Soldiers",
        }
    }
}

/// One member in one seat of a ship or squad.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    pub member: MemberId,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub kind: UnitKind,
    pub name: String,
    /// `None` directly under the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The members holding this kind's lead role (at most one captain on a ship).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead: Vec<MemberId>,
    /// Ships and squads only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seats: Vec<Seat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStructure {
    pub event: EventInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admiral: Option<MemberId>,
    /// Parents before children is not required.
    #[serde(default)]
    pub units: Vec<Unit>,
    /// In the event with no seat and no role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unassigned: Vec<MemberId>,
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

/// Changes to the structure, as the admin portal, the admiral or a commander asks for them. The
/// server answers a member's change with a new `structure` and `roster`, or `changeRefused`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum StructureChange {
    /// Portal only: an admiral cannot hand off the role.
    SetAdmiral {
        #[serde(default)]
        member: Option<MemberId>,
    },
    AddUnit {
        id: String,
        kind: UnitKind,
        name: String,
        #[serde(default)]
        parent: Option<String>,
    },
    RenameUnit {
        id: String,
        name: String,
    },
    MoveUnit {
        id: String,
        #[serde(default)]
        parent: Option<String>,
    },
    /// With everything under it; their members become Unassigned unless they hold another role.
    RemoveUnit {
        id: String,
    },
    SetLead {
        unit: String,
        member: MemberId,
        on: bool,
    },
    /// Into a seat, out of any other. `unit: null` is Unassigned (leads are kept).
    Place {
        member: MemberId,
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        role: Option<Role>,
    },
}

impl EventStructure {
    pub fn new(event: EventInfo) -> Self {
        Self {
            event,
            admiral: None,
            units: Vec::new(),
            unassigned: Vec::new(),
        }
    }

    pub fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    fn unit_mut(&mut self, id: &str) -> Option<&mut Unit> {
        self.units.iter_mut().find(|u| u.id == id)
    }

    pub fn parent_of(&self, unit: &Unit) -> Option<&Unit> {
        unit.parent.as_deref().and_then(|p| self.unit(p))
    }

    /// The units above `id`, nearest first.
    pub fn ancestors(&self, id: &str) -> Vec<&Unit> {
        let mut out: Vec<&Unit> = Vec::new();
        let mut at = self.unit(id).and_then(|u| self.parent_of(u));
        while let Some(u) = at {
            if out.iter().any(|seen| seen.id == u.id) {
                break;
            }
            out.push(u);
            at = self.parent_of(u);
        }
        out
    }

    /// `id` and every unit under it, in structure order.
    pub fn subtree(&self, id: &str) -> Vec<&Unit> {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        if self.unit(id).is_some() {
            ids.insert(id);
        }
        loop {
            let before = ids.len();
            for u in &self.units {
                if u.parent.as_deref().is_some_and(|p| ids.contains(p)) {
                    ids.insert(u.id.as_str());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        self.units
            .iter()
            .filter(|u| ids.contains(u.id.as_str()))
            .collect()
    }

    /// The seat `member` sits in, with its unit.
    pub fn seat_of(&self, member: &str) -> Option<(&Unit, Role)> {
        self.units.iter().find_map(|u| {
            u.seats
                .iter()
                .find(|s| s.member == member)
                .map(|s| (u, s.role))
        })
    }

    /// Every member of the event, each once.
    pub fn members(&self) -> BTreeSet<MemberId> {
        let mut all: BTreeSet<MemberId> = self.admiral.iter().cloned().collect();
        for u in &self.units {
            all.extend(u.lead.iter().cloned());
            all.extend(u.seats.iter().map(|s| s.member.clone()));
        }
        all.extend(self.unassigned.iter().cloned());
        all
    }

    pub fn is_member(&self, member: &str) -> bool {
        self.admiral.as_deref() == Some(member)
            || self.unassigned.iter().any(|m| m == member)
            || self.units.iter().any(|u| {
                u.lead.iter().any(|m| m == member) || u.seats.iter().any(|s| s.member == member)
            })
    }

    /// Every role `member` holds in this event: lead roles and the seat.
    pub fn grants_of(&self, member: &str) -> BTreeSet<Grant> {
        let mut grants = BTreeSet::new();
        if self.admiral.as_deref() == Some(member) {
            grants.insert(Grant {
                role: Role::Admiral,
                scope: Scope::Event,
            });
        }
        for u in &self.units {
            if u.lead.iter().any(|m| m == member) {
                grants.insert(Grant {
                    role: u.kind.lead_role(),
                    scope: Scope::Unit(u.id.clone()),
                });
            }
            for s in u.seats.iter().filter(|s| s.member == member) {
                grants.insert(Grant {
                    role: s.role,
                    scope: Scope::Unit(u.id.clone()),
                });
            }
        }
        grants
    }

    /// The highest role `member` holds.
    pub fn top_role(&self, member: &str) -> Option<Role> {
        self.grants_of(member)
            .into_iter()
            .map(|g| g.role)
            .min_by_key(|r| r.rank())
    }

    /// The unit ids whose members' cards `member` may receive: their own seat's unit, the whole
    /// squadron for a seat on a squadron's ship, and everything under any unit they lead (all of
    /// it for the admiral). Worked out again on every change, never cached per connection.
    pub fn visible_units(&self, member: &str) -> BTreeSet<String> {
        let mut visible = BTreeSet::new();
        let mut add = |units: Vec<&Unit>| visible.extend(units.into_iter().map(|u| u.id.clone()));
        for grant in self.grants_of(member) {
            match grant.scope {
                Scope::Event => add(self.units.iter().collect()),
                Scope::Unit(id) if !grant.role.is_seat() => add(self.subtree(&id)),
                Scope::Unit(id) => {
                    let Some(unit) = self.unit(&id) else { continue };
                    match self.parent_of(unit) {
                        Some(p) if p.kind == UnitKind::Squadron => add(self.subtree(&p.id)),
                        _ => add(vec![unit]),
                    }
                }
            }
        }
        visible
    }

    /// The units a member's card belongs to: their seat's and every unit they lead. An
    /// Unassigned member's card belongs nowhere, so only they see it.
    fn home_units(&self, member: &str) -> Vec<&str> {
        self.units
            .iter()
            .filter(|u| {
                u.lead.iter().any(|m| m == member) || u.seats.iter().any(|s| s.member == member)
            })
            .map(|u| u.id.as_str())
            .collect()
    }

    /// Whether `viewer` may receive `source`'s card.
    pub fn can_see_card(&self, viewer: &str, source: &str) -> bool {
        if !self.is_member(viewer) || !self.is_member(source) {
            return false;
        }
        if viewer == source {
            return true;
        }
        let visible = self.visible_units(viewer);
        self.home_units(source)
            .into_iter()
            .any(|u| visible.contains(u))
    }

    /// Everyone who may receive `source`'s card, `source` included.
    pub fn card_audience(&self, source: &str) -> BTreeSet<MemberId> {
        self.members()
            .into_iter()
            .filter(|v| self.can_see_card(v, source))
            .collect()
    }

    /// Who receives the ship card of `unit` built from `source`: those who see the ship, and
    /// never anyone outside `source`'s own audience, so a ship card cannot widen consent.
    pub fn ship_card_audience(&self, unit: &str, source: &str) -> BTreeSet<MemberId> {
        let theirs = self.card_audience(source);
        self.members()
            .into_iter()
            .filter(|v| theirs.contains(v) && self.visible_units(v).contains(unit))
            .collect()
    }

    /// The admiral and the commanders see the Unassigned bucket (as roster entries, never
    /// cards).
    pub fn sees_unassigned(&self, member: &str) -> bool {
        self.grants_of(member).iter().any(|g| g.role.is_commander())
    }

    /// The members `viewer`'s roster lists: themselves, the admiral, everyone leading or seated
    /// in a unit they see, the leads of the units above theirs, and the Unassigned when
    /// [`Self::sees_unassigned`].
    pub fn roster_for(&self, viewer: &str) -> BTreeSet<MemberId> {
        let mut out = BTreeSet::new();
        if !self.is_member(viewer) {
            return out;
        }
        out.insert(viewer.to_owned());
        out.extend(self.admiral.iter().cloned());
        let visible = self.visible_units(viewer);
        for u in self.units.iter().filter(|u| visible.contains(&u.id)) {
            out.extend(u.lead.iter().cloned());
            out.extend(u.seats.iter().map(|s| s.member.clone()));
        }
        for home in self.home_units(viewer) {
            for a in self.ancestors(home) {
                out.extend(a.lead.iter().cloned());
            }
        }
        if self.sees_unassigned(viewer) {
            out.extend(self.unassigned.iter().cloned());
        }
        out
    }

    /// The structure as `viewer` receives it: the units they see with their seats, the units
    /// above those with their names and leads only, and the Unassigned when they may see them.
    pub fn view_for(&self, viewer: &str) -> EventStructure {
        let visible = self.visible_units(viewer);
        let mut above: BTreeSet<String> = BTreeSet::new();
        for id in visible
            .iter()
            .map(String::as_str)
            .chain(self.home_units(viewer))
        {
            above.extend(self.ancestors(id).into_iter().map(|u| u.id.clone()));
        }
        let units = self
            .units
            .iter()
            .filter_map(|u| {
                if visible.contains(&u.id) {
                    Some(u.clone())
                } else if above.contains(&u.id) {
                    Some(Unit {
                        seats: Vec::new(),
                        ..u.clone()
                    })
                } else {
                    None
                }
            })
            .collect();
        EventStructure {
            event: self.event.clone(),
            admiral: self.admiral.clone(),
            units,
            unassigned: if self.sees_unassigned(viewer) {
                self.unassigned.clone()
            } else {
                Vec::new()
            },
        }
    }

    /// The members a unit's roll-up counts, each once: pilots only in a wing or squadron (a
    /// copilot shows on their ship's card), soldiers in ground and squads, every seat in a fleet
    /// and on a ship.
    pub fn rollup_members(&self, unit: &str) -> Vec<MemberId> {
        let Some(u) = self.unit(unit) else {
            return Vec::new();
        };
        let counts = |role: Role| match u.kind {
            UnitKind::Wing | UnitKind::Squadron => role == Role::Pilot,
            UnitKind::Ground | UnitKind::Squad => role == Role::Soldier,
            UnitKind::Fleet | UnitKind::Ship => true,
        };
        self.subtree(unit)
            .into_iter()
            .flat_map(|u| u.seats.iter())
            .filter(|s| counts(s.role))
            .map(|s| s.member.clone())
            .collect()
    }

    /// The seat roles a unit takes: Pilot, Copilot and Crew on a fleet's ship, Pilot and Copilot
    /// on a squadron's, Soldier in a squad, none elsewhere.
    pub fn seat_roles(&self, unit: &Unit) -> &'static [Role] {
        match (unit.kind, self.parent_of(unit).map(|p| p.kind)) {
            (UnitKind::Ship, Some(UnitKind::Fleet)) => &[Role::Pilot, Role::Copilot, Role::Crew],
            (UnitKind::Ship, _) => &[Role::Pilot, Role::Copilot],
            (UnitKind::Squad, _) => &[Role::Soldier],
            _ => &[],
        }
    }

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
        if let Some(a) = &self.admiral {
            check(is_member_id(a), "bad admiral id".to_owned());
        }
        check(
            self.units.len() <= MAX_UNITS,
            format!("more than {MAX_UNITS} units"),
        );

        let kinds: HashMap<&str, UnitKind> =
            self.units.iter().map(|u| (u.id.as_str(), u.kind)).collect();
        let mut ids = HashSet::new();
        let mut seated: HashSet<&str> = HashSet::new();
        for u in &self.units {
            check(is_id(&u.id), format!("bad unit id {:?}", u.id));
            check(is_name(&u.name), format!("unit {}: bad name", u.id));
            check(ids.insert(u.id.as_str()), format!("unit {} repeated", u.id));
            let parent = match &u.parent {
                None => Some(None),
                Some(p) => kinds.get(p.as_str()).map(|k| Some(*k)),
            };
            match parent {
                None => check(
                    false,
                    format!(
                        "unit {}: no unit {}",
                        u.id,
                        u.parent.as_deref().unwrap_or("")
                    ),
                ),
                Some(p) => check(
                    u.kind.parents().contains(&p),
                    format!(
                        "unit {}: a {} cannot sit in {}",
                        u.id,
                        u.kind.label(),
                        p.map_or("the event", UnitKind::label)
                    ),
                ),
            }
            for m in &u.lead {
                check(is_member_id(m), format!("unit {}: bad lead id", u.id));
            }
            check(
                u.kind != UnitKind::Ship || u.lead.len() <= 1,
                format!("ship {}: more than one captain", u.id),
            );
            let in_fleet = self.parent_of(u).is_some_and(|p| p.kind == UnitKind::Fleet);
            check(
                u.kind != UnitKind::Ship || u.lead.is_empty() || in_fleet,
                format!("ship {}: a captain only on a fleet's ship", u.id),
            );
            let allowed = self.seat_roles(u);
            for role in [Role::Pilot, Role::Copilot] {
                check(
                    u.seats.iter().filter(|s| s.role == role).count() <= 1,
                    format!("ship {}: more than one {}", u.id, role.label()),
                );
            }
            for s in &u.seats {
                check(
                    is_member_id(&s.member),
                    format!("unit {}: bad member id", u.id),
                );
                check(
                    allowed.contains(&s.role),
                    format!("unit {}: no {} seat", u.id, s.role.label()),
                );
                check(
                    seated.insert(s.member.as_str()),
                    format!("member {} has more than one seat", s.member),
                );
            }
        }
        let mut loose = HashSet::new();
        for m in &self.unassigned {
            check(is_member_id(m), "bad unassigned id".to_owned());
            check(loose.insert(m.as_str()), format!("member {m} repeated"));
            check(
                self.grants_of(m).is_empty(),
                format!("member {m} is Unassigned but holds a role"),
            );
        }
        problems
    }

    /// Whether `actor` may make `change` over the wire. The admiral makes any change but
    /// [`StructureChange::SetAdmiral`] (the portal's). A commander places members and sets leads
    /// within the units under theirs, taking an Unassigned member or one already there.
    pub fn may_change(&self, actor: &str, change: &StructureChange) -> bool {
        if matches!(change, StructureChange::SetAdmiral { .. }) {
            return false;
        }
        if self.admiral.as_deref() == Some(actor) {
            return true;
        }
        let mut below: BTreeSet<&str> = BTreeSet::new();
        for g in self.grants_of(actor) {
            if let (true, Scope::Unit(id)) = (g.role.is_commander(), &g.scope) {
                below.extend(self.subtree(id).into_iter().map(|u| u.id.as_str()));
            }
        }
        match change {
            StructureChange::SetLead { unit, .. } => {
                below.contains(unit.as_str())
                    && !self
                        .unit(unit)
                        .is_some_and(|u| u.lead.iter().any(|m| m == actor))
            }
            StructureChange::Place { member, unit, .. } => {
                let from_ok = match self.seat_of(member) {
                    Some((u, _)) => below.contains(u.id.as_str()),
                    None => self.unassigned.iter().any(|m| m == member),
                };
                let to_ok = unit.as_deref().is_none_or(|u| below.contains(u));
                from_ok && to_ok && !below.is_empty()
            }
            _ => false,
        }
    }

    /// Applies `change`, keeping the structure sound, or says why not and changes nothing.
    pub fn apply(&mut self, change: &StructureChange) -> Result<(), String> {
        let mut next = self.clone();
        next.apply_unchecked(change)?;
        next.tidy();
        match next.problems().into_iter().next() {
            Some(p) => Err(p),
            None => {
                *self = next;
                Ok(())
            }
        }
    }

    fn apply_unchecked(&mut self, change: &StructureChange) -> Result<(), String> {
        let missing = |id: &str| format!("no unit {id}");
        match change {
            StructureChange::SetAdmiral { member } => {
                if let Some(old) = self.admiral.take() {
                    self.unassigned.push(old);
                }
                self.admiral.clone_from(member);
            }
            StructureChange::AddUnit {
                id,
                kind,
                name,
                parent,
            } => {
                if self.unit(id).is_some() {
                    return Err(format!("unit {id} exists"));
                }
                self.units.push(Unit {
                    id: id.clone(),
                    kind: *kind,
                    name: name.clone(),
                    parent: parent.clone(),
                    lead: Vec::new(),
                    seats: Vec::new(),
                });
            }
            StructureChange::RenameUnit { id, name } => {
                self.unit_mut(id)
                    .ok_or_else(|| missing(id))?
                    .name
                    .clone_from(name);
            }
            StructureChange::MoveUnit { id, parent } => {
                self.unit_mut(id)
                    .ok_or_else(|| missing(id))?
                    .parent
                    .clone_from(parent);
            }
            StructureChange::RemoveUnit { id } => {
                let gone: BTreeSet<String> =
                    self.subtree(id).into_iter().map(|u| u.id.clone()).collect();
                if gone.is_empty() {
                    return Err(missing(id));
                }
                for u in self.units.iter().filter(|u| gone.contains(&u.id)) {
                    self.unassigned.extend(u.lead.iter().cloned());
                    self.unassigned
                        .extend(u.seats.iter().map(|s| s.member.clone()));
                }
                self.units.retain(|u| !gone.contains(&u.id));
            }
            StructureChange::SetLead { unit, member, on } => {
                let u = self.unit_mut(unit).ok_or_else(|| missing(unit))?;
                u.lead.retain(|m| m != member);
                if *on {
                    u.lead.push(member.clone());
                } else {
                    self.unassigned.push(member.clone());
                }
            }
            StructureChange::Place { member, unit, role } => {
                for u in &mut self.units {
                    u.seats.retain(|s| &s.member != member);
                }
                self.unassigned.retain(|m| m != member);
                match unit {
                    None => self.unassigned.push(member.clone()),
                    Some(id) => {
                        let u = self.unit(id).ok_or_else(|| missing(id))?;
                        let role = match role {
                            Some(r) => *r,
                            None => *self
                                .seat_roles(u)
                                .iter()
                                .find(|r| {
                                    !matches!(r, Role::Pilot | Role::Copilot)
                                        || !u.seats.iter().any(|s| s.role == **r)
                                })
                                .ok_or_else(|| format!("unit {id} has no seats"))?,
                        };
                        self.unit_mut(id)
                            .ok_or_else(|| missing(id))?
                            .seats
                            .push(Seat {
                                member: member.clone(),
                                role,
                            });
                    }
                }
            }
        }
        Ok(())
    }

    /// Unassigned holds exactly the members with no role, each once.
    fn tidy(&mut self) {
        let mut seen = HashSet::new();
        let list = std::mem::take(&mut self.unassigned);
        let keep: Vec<MemberId> = list
            .into_iter()
            .filter(|m| seen.insert(m.clone()))
            .filter(|m| self.grants_of(m).is_empty())
            .collect();
        self.unassigned = keep;
    }

    /// The flat tree a peer-to-peer host keeps in memory for the visibility rules and never sends
    /// (PROTOCOL 4.1): wing
    /// `wing`, squadron `squadron` under it, `host` as the admiral and also Pilot of their own
    /// ship. The seat matters: the admiral grant is the event's, not a unit's, so a host with no
    /// seat would have no home unit and nobody would receive their card.
    pub fn p2p_default(event: EventInfo, host: &str, host_ship: &str) -> Self {
        let mut s = Self::new(event);
        s.admiral = Some(host.to_owned());
        s.units = vec![
            Unit {
                id: P2P_WING.to_owned(),
                kind: UnitKind::Wing,
                name: "Wing".to_owned(),
                parent: None,
                lead: Vec::new(),
                seats: Vec::new(),
            },
            Unit {
                id: P2P_SQUADRON.to_owned(),
                kind: UnitKind::Squadron,
                name: "Squadron".to_owned(),
                parent: Some(P2P_WING.to_owned()),
                lead: Vec::new(),
                seats: Vec::new(),
            },
        ];
        s.p2p_seat(host, host_ship);
        s
    }

    /// Puts `member` in a new ship of the default squadron as its Pilot, unless they already
    /// sit somewhere; the ship's id is the first free `ship-<n>`. Returns the unit id of their
    /// seat.
    pub fn p2p_seat(&mut self, member: &str, ship_name: &str) -> Option<String> {
        if let Some((u, _)) = self.seat_of(member) {
            return Some(u.id.clone());
        }
        self.unit(P2P_SQUADRON)?;
        let id = (1..)
            .map(|n| format!("ship-{n}"))
            .find(|id| self.unit(id).is_none())?;
        let name: String = ship_name.chars().take(MAX_NAME).collect();
        self.unassigned.retain(|m| m != member);
        self.units.push(Unit {
            id: id.clone(),
            kind: UnitKind::Ship,
            name: if is_name(&name) {
                name
            } else {
                "Ship".to_owned()
            },
            parent: Some(P2P_SQUADRON.to_owned()),
            lead: Vec::new(),
            seats: vec![Seat {
                member: member.to_owned(),
                role: Role::Pilot,
            }],
        });
        Some(id)
    }

    /// Takes `member` out of the event entirely (removed or kicked).
    pub fn remove_member(&mut self, member: &str) {
        if self.admiral.as_deref() == Some(member) {
            self.admiral = None;
        }
        for u in &mut self.units {
            u.lead.retain(|m| m != member);
            u.seats.retain(|s| s.member != member);
        }
        self.unassigned.retain(|m| m != member);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn unit(
        id: &str,
        kind: UnitKind,
        parent: Option<&str>,
        lead: &[&str],
        seats: &[(&str, Role)],
    ) -> Unit {
        Unit {
            id: id.to_owned(),
            kind,
            name: id.to_uppercase(),
            parent: parent.map(str::to_owned),
            lead: lead.iter().map(|m| (*m).to_owned()).collect(),
            seats: seats
                .iter()
                .map(|(m, role)| Seat {
                    member: (*m).to_owned(),
                    role: *role,
                })
                .collect(),
        }
    }

    /// Admiral `adm`. Fleet `navy` (commander `fc`) with ship `carrack` (captain `cap`, who is
    /// also crew; pilot `hp`, copilot `cp`, crew `c1`) and ship `cutlass` (pilot `p2`). Wing
    /// `air` (commander `wc`) with squadron `red` (commander `rl`, who flies `red-1`), ships
    /// `red-1` (pilot `rl`) and `red-2` (pilot `r2`, copilot `r2c`). Ground `army` (commander
    /// `gc`) with squads `alpha` (commander `sl`, soldiers `sl`, `s1`) and `bravo` (soldier `s2`).
    /// Unassigned `new`.
    pub(crate) fn event() -> EventStructure {
        use Role::*;
        use UnitKind::*;
        EventStructure {
            event: EventInfo {
                id: "op-1".to_owned(),
                name: "Op One".to_owned(),
                state: EventState::Open,
            },
            admiral: Some("adm".to_owned()),
            units: vec![
                unit("navy", Fleet, None, &["fc"], &[]),
                unit(
                    "carrack",
                    Ship,
                    Some("navy"),
                    &["cap"],
                    &[("hp", Pilot), ("cp", Copilot), ("c1", Crew), ("cap", Crew)],
                ),
                unit("cutlass", Ship, Some("navy"), &[], &[("p2", Pilot)]),
                unit("air", Wing, None, &["wc"], &[]),
                unit("red", Squadron, Some("air"), &["rl"], &[]),
                unit("red-1", Ship, Some("red"), &[], &[("rl", Pilot)]),
                unit(
                    "red-2",
                    Ship,
                    Some("red"),
                    &[],
                    &[("r2", Pilot), ("r2c", Copilot)],
                ),
                unit("army", Ground, None, &["gc"], &[]),
                unit(
                    "alpha",
                    Squad,
                    Some("army"),
                    &["sl"],
                    &[("sl", Soldier), ("s1", Soldier)],
                ),
                unit("bravo", Squad, Some("army"), &[], &[("s2", Soldier)]),
            ],
            unassigned: vec!["new".to_owned()],
        }
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_example_is_sound_and_round_trips() {
        let e = event();
        assert!(e.problems().is_empty(), "{:?}", e.problems());
        let text = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<EventStructure>(&text).unwrap(), e);
        assert!(text.contains(r#"{"id":"red-1","kind":"ship","name":"RED-1","parent":"red","seats":[{"member":"rl","role":"pilot"}]}"#), "{text}");
        assert!(text.ends_with(r#""unassigned":["new"]}"#), "{text}");
    }

    #[test]
    fn each_role_sees_its_own_unit_and_everything_it_leads() {
        let e = event();
        let all: BTreeSet<String> = e.units.iter().map(|u| u.id.clone()).collect();
        assert_eq!(e.visible_units("adm"), all);
        assert_eq!(e.visible_units("fc"), set(&["navy", "carrack", "cutlass"]));
        // A captain sees their own ship, never the fleet's other ships.
        assert_eq!(e.visible_units("cap"), set(&["carrack"]));
        assert_eq!(e.visible_units("c1"), set(&["carrack"]));
        assert_eq!(e.visible_units("hp"), set(&["carrack"]));
        assert_eq!(
            e.visible_units("wc"),
            set(&["air", "red", "red-1", "red-2"])
        );
        assert_eq!(e.visible_units("rl"), set(&["red", "red-1", "red-2"]));
        // Pilots and copilots see their whole squadron.
        assert_eq!(e.visible_units("r2"), set(&["red", "red-1", "red-2"]));
        assert_eq!(e.visible_units("r2c"), set(&["red", "red-1", "red-2"]));
        assert_eq!(e.visible_units("gc"), set(&["army", "alpha", "bravo"]));
        assert_eq!(e.visible_units("s1"), set(&["alpha"]));
        assert!(e.visible_units("new").is_empty());
        assert!(e.visible_units("stranger").is_empty());

        assert_eq!(e.top_role("cap"), Some(Role::Captain));
        assert_eq!(e.top_role("rl"), Some(Role::SquadronCommander));
        assert_eq!(e.grants_of("rl").len(), 2);
        assert_eq!(e.top_role("new"), None);
        assert_eq!(
            e.seat_of("r2c").map(|(u, r)| (u.id.as_str(), r)),
            Some(("red-2", Role::Copilot))
        );
    }

    #[test]
    fn cards_go_to_those_who_see_the_sender_and_unassigned_cards_to_nobody() {
        let e = event();
        assert_eq!(
            e.card_audience("c1"),
            set(&["adm", "fc", "cap", "hp", "cp", "c1"])
        );
        assert_eq!(e.card_audience("p2"), set(&["adm", "fc", "p2"]));
        assert_eq!(
            e.card_audience("r2"),
            set(&["adm", "wc", "rl", "r2", "r2c"])
        );
        assert_eq!(e.card_audience("s2"), set(&["adm", "gc", "s2"]));
        // A commander's card belongs to the unit they lead.
        assert_eq!(e.card_audience("fc"), set(&["adm", "fc"]));
        assert_eq!(e.card_audience("new"), set(&["new"]));
        assert!(e.can_see_card("new", "new"));
        assert!(!e.can_see_card("adm", "new"));
        assert!(!e.can_see_card("stranger", "c1"));
        assert!(!e.can_see_card("c1", "p2"));
        assert!(!e.can_see_card("s1", "s2"));
    }

    #[test]
    fn a_ship_card_audience_is_inside_its_sources() {
        let e = event();
        for (ship, source) in [
            ("carrack", "hp"),
            ("carrack", "c1"),
            ("red-2", "r2"),
            ("red-2", "r2c"),
            ("cutlass", "p2"),
        ] {
            let ship_audience = e.ship_card_audience(ship, source);
            assert!(
                ship_audience.is_subset(&e.card_audience(source)),
                "{ship} via {source}"
            );
            assert!(!ship_audience.is_empty());
        }
        assert_eq!(
            e.ship_card_audience("carrack", "hp"),
            set(&["adm", "fc", "cap", "hp", "cp", "c1"])
        );
        // A source who is not aboard widens nothing: only those who see both.
        assert_eq!(e.ship_card_audience("carrack", "p2"), set(&["adm", "fc"]));
        assert!(e.ship_card_audience("carrack", "new").is_empty());
    }

    #[test]
    fn the_unassigned_bucket_is_for_the_admiral_and_commanders() {
        let e = event();
        for m in ["adm", "fc", "wc", "gc", "rl", "sl"] {
            assert!(e.sees_unassigned(m), "{m}");
            assert!(e.roster_for(m).contains("new"), "{m}");
            assert_eq!(e.view_for(m).unassigned, ["new"], "{m}");
        }
        for m in ["cap", "hp", "c1", "r2", "s1", "new"] {
            assert!(!e.sees_unassigned(m), "{m}");
            assert!(e.view_for(m).unassigned.is_empty(), "{m}");
        }
        assert_eq!(e.roster_for("new"), set(&["adm", "new"]));
        assert!(e.view_for("new").units.is_empty());
    }

    #[test]
    fn a_roster_lists_the_units_seen_and_the_chain_of_command() {
        let e = event();
        assert_eq!(
            e.roster_for("c1"),
            set(&["adm", "fc", "cap", "hp", "cp", "c1"])
        );
        assert_eq!(e.roster_for("s2"), set(&["adm", "gc", "s2"]));
        assert_eq!(e.roster_for("r2"), set(&["adm", "wc", "rl", "r2", "r2c"]));
        assert!(e.roster_for("stranger").is_empty());

        let v = e.view_for("c1");
        let ids: Vec<&str> = v.units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["navy", "carrack"]);
        assert!(v.units[0].seats.is_empty() && v.units[0].lead == ["fc"]);
        assert_eq!(v.units[1].seats.len(), 4);
        assert!(v.problems().is_empty(), "{:?}", v.problems());
        assert_eq!(e.view_for("adm"), e);
    }

    #[test]
    fn rollups_count_each_member_once_with_their_kinds_label() {
        let e = event();
        assert_eq!(e.rollup_members("carrack"), ["hp", "cp", "c1", "cap"]);
        assert_eq!(e.rollup_members("navy").len(), 5);
        assert_eq!(e.rollup_members("red"), ["rl", "r2"]);
        assert_eq!(e.rollup_members("air"), ["rl", "r2"]);
        assert_eq!(e.rollup_members("red-2"), ["r2", "r2c"]);
        assert_eq!(e.rollup_members("army"), ["sl", "s1", "s2"]);
        assert!(e.rollup_members("nope").is_empty());
        assert_eq!(UnitKind::Ship.rollup_label(), "Crew");
        assert_eq!(UnitKind::Squadron.rollup_label(), "Pilots");
        assert_eq!(UnitKind::Squad.rollup_label(), "Soldiers");
    }

    #[test]
    fn problems_name_what_is_wrong() {
        use Role::*;
        use UnitKind::*;
        let mut e = event();
        e.units.push(unit("lost", Ship, None, &[], &[]));
        e.units.push(unit("odd", Squad, Some("air"), &[], &[]));
        e.units.push(unit("ghost", Squad, Some("nope"), &[], &[]));
        e.units[5].lead.push("x".to_owned());
        e.units[6].seats.push(Seat {
            member: "s1".to_owned(),
            role: Crew,
        });
        e.units[2].seats.push(Seat {
            member: "z".to_owned(),
            role: Pilot,
        });
        e.unassigned.push("hp".to_owned());
        let p = e.problems();
        for want in [
            "unit lost: a Ship cannot sit in the event",
            "unit odd: a Squad cannot sit in Wing",
            "unit ghost: no unit nope",
            "ship red-1: a captain only on a fleet's ship",
            "unit red-2: no Crew seat",
            "member s1 has more than one seat",
            "ship cutlass: more than one Pilot",
            "member hp is Unassigned but holds a role",
        ] {
            assert!(p.iter().any(|p| p == want), "{want} not in {p:?}");
        }
    }

    #[test]
    fn changes_apply_and_a_placed_member_sees_their_unit_at_once() {
        let mut e = event();
        assert!(e.visible_units("new").is_empty());
        e.apply(&StructureChange::Place {
            member: "new".to_owned(),
            unit: Some("bravo".to_owned()),
            role: None,
        })
        .unwrap();
        assert!(e.unassigned.is_empty());
        assert_eq!(
            e.seat_of("new").map(|(u, r)| (u.id.as_str(), r)),
            Some(("bravo", Role::Soldier))
        );
        assert_eq!(e.visible_units("new"), set(&["bravo"]));
        assert!(e.can_see_card("new", "s2"));

        e.apply(&StructureChange::Place {
            member: "new".to_owned(),
            unit: None,
            role: None,
        })
        .unwrap();
        assert_eq!(e.unassigned, ["new"]);

        // A second pilot is refused and nothing changes.
        let before = e.clone();
        let err = e
            .apply(&StructureChange::Place {
                member: "new".to_owned(),
                unit: Some("cutlass".to_owned()),
                role: Some(Role::Pilot),
            })
            .unwrap_err();
        assert_eq!(err, "ship cutlass: more than one Pilot");
        assert_eq!(e, before);
        // Without a role, a ship with a pilot gives the next seat.
        e.apply(&StructureChange::Place {
            member: "new".to_owned(),
            unit: Some("cutlass".to_owned()),
            role: None,
        })
        .unwrap();
        assert_eq!(e.seat_of("new").map(|(_, r)| r), Some(Role::Copilot));

        e.apply(&StructureChange::AddUnit {
            id: "blue".to_owned(),
            kind: UnitKind::Squadron,
            name: "Blue".to_owned(),
            parent: Some("air".to_owned()),
        })
        .unwrap();
        assert!(
            e.apply(&StructureChange::AddUnit {
                id: "bad".to_owned(),
                kind: UnitKind::Squad,
                name: "Bad".to_owned(),
                parent: Some("air".to_owned()),
            })
            .is_err()
        );
        e.apply(&StructureChange::RemoveUnit {
            id: "air".to_owned(),
        })
        .unwrap();
        assert!(e.unit("red-2").is_none() && e.unit("blue").is_none());
        for m in ["wc", "rl", "r2", "r2c"] {
            assert!(e.unassigned.iter().any(|u| u == m), "{m}");
        }
        e.apply(&StructureChange::SetLead {
            unit: "navy".to_owned(),
            member: "wc".to_owned(),
            on: true,
        })
        .unwrap();
        assert!(!e.unassigned.iter().any(|u| u == "wc"));
        assert!(e.problems().is_empty(), "{:?}", e.problems());

        let text = serde_json::to_string(&StructureChange::Place {
            member: "a".to_owned(),
            unit: Some("b".to_owned()),
            role: Some(Role::Crew),
        })
        .unwrap();
        assert_eq!(
            text,
            r#"{"op":"place","member":"a","unit":"b","role":"crew"}"#
        );
    }

    #[test]
    fn the_p2p_default_seats_the_host_and_everyone_sees_everyone() {
        let info = EventInfo {
            id: "p2p-1".to_owned(),
            name: "Vega's fleet".to_owned(),
            state: EventState::Open,
        };
        let mut e = EventStructure::p2p_default(info, "host", "Vega");
        assert_eq!(e.admiral.as_deref(), Some("host"));
        assert_eq!(
            e.seat_of("host").map(|(u, r)| (u.id.as_str(), r)),
            Some(("ship-1", Role::Pilot))
        );
        assert!(e.problems().is_empty(), "{:?}", e.problems());
        for (m, name) in [("a", "Rook"), ("b", "Wren"), ("c", "Kite")] {
            assert!(e.p2p_seat(m, name).is_some());
        }
        assert_eq!(e.p2p_seat("a", "Rook").as_deref(), Some("ship-2"));
        assert!(e.problems().is_empty(), "{:?}", e.problems());
        let everyone = set(&["host", "a", "b", "c"]);
        for m in &everyone {
            assert_eq!(&e.card_audience(m), &everyone, "{m}");
            assert_eq!(&e.roster_for(m), &everyone, "{m}");
        }
        // Without the seat nobody but the host would get the host's card.
        let mut admiral_only = e.clone();
        admiral_only.units.retain(|u| u.id != "ship-1");
        assert_eq!(admiral_only.card_audience("host"), set(&["host"]));

        // A freed ship id is used again, and a name that is not one becomes "Ship".
        e.apply(&StructureChange::RemoveUnit {
            id: "ship-3".to_owned(),
        })
        .unwrap();
        e.remove_member("b");
        assert_eq!(e.p2p_seat("d", "<bad>").as_deref(), Some("ship-3"));
        assert_eq!(e.unit("ship-3").map(|u| u.name.as_str()), Some("Ship"));
        assert!(e.problems().is_empty(), "{:?}", e.problems());
    }

    #[test]
    fn only_the_admiral_and_commanders_over_a_unit_change_it() {
        let e = event();
        let place = |member: &str, unit: Option<&str>| StructureChange::Place {
            member: member.to_owned(),
            unit: unit.map(str::to_owned),
            role: None,
        };
        assert!(e.may_change("adm", &place("c1", Some("bravo"))));
        assert!(!e.may_change("adm", &StructureChange::SetAdmiral { member: None }));
        assert!(e.may_change("gc", &place("new", Some("bravo"))));
        assert!(e.may_change("gc", &place("s1", Some("bravo"))));
        assert!(e.may_change("sl", &place("new", Some("alpha"))));
        // Not from outside their units, not into another's, and not by a captain or a seat.
        assert!(!e.may_change("gc", &place("c1", Some("bravo"))));
        assert!(!e.may_change("sl", &place("new", Some("bravo"))));
        assert!(!e.may_change("cap", &place("new", Some("carrack"))));
        assert!(!e.may_change("s1", &place("new", Some("alpha"))));
        assert!(!e.may_change("new", &place("new", Some("alpha"))));
        let lead = |unit: &str| StructureChange::SetLead {
            unit: unit.to_owned(),
            member: "new".to_owned(),
            on: true,
        };
        assert!(e.may_change("gc", &lead("bravo")));
        assert!(!e.may_change("gc", &lead("army")));
        assert!(!e.may_change(
            "fc",
            &StructureChange::RemoveUnit {
                id: "carrack".to_owned()
            }
        ));
    }
}
