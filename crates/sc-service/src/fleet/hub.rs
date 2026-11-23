//! The viewer's side of a fleet: the cards received (with the receive time, which "updated N s
//! ago" and OFFLINE are measured from), the event structure the server sent, what this viewer
//! subscribes to, and the model the dashboard's fleet panel draws. The fleet client
//! ([`super::client`]) hands it received cards, the structure and the connection's state.
//!
//! With an event structure the panel draws the event's unit tree ([`super::tree`]), as cards or
//! as the condensed admiral view, with a labelled roll-up per unit. Without one (a server that
//! sends none) it draws the viewer's own groups from fleet-view.json, then Ungrouped.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use super::card::{CARD_SCHEMA, Card, Consent, FIELDS, Life, card_fields, detail_fields};
use super::cic::event_url;
use super::host::HostView;
use super::tree::{EventTree, Inputs, Person};
use super::view::{
    FleetView, FleetViewStore, MAX_GROUP_MEMBERS, MAX_GROUP_NAME, MAX_GROUPS, is_member_id,
};
use fleet_proto::rollup::{Rollup, Status, rollup, sort_by_status};
use fleet_proto::ship::{ShipSource, SourceKind};
use fleet_proto::structure::{EventStructure, UnitKind};
use fleet_proto::{EventChoice, Grant, PublicKey, RefusalCode, Role, RosterEntry, Subscription};

/// A card not updated for this long shows OFFLINE.
pub const STALE_AFTER: Duration = Duration::from_secs(10);
/// Cards past this many members are dropped (and logged once each).
pub const MAX_MEMBERS: usize = 64;

struct Received {
    card: Card,
    at: Instant,
}

/// The fleet connection, as the panel shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkState {
    /// No fleet joined, or left.
    #[default]
    None,
    /// Also while the relay waits for the player to choose an event (`choices` set).
    Connecting,
    Online,
    /// Lost; trying again after `retryMs`.
    Offline,
    /// The server or the pin refused it; not retried until the player connects again.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetRef {
    pub address: String,
    pub key: PublicKey,
    pub fingerprint: String,
    pub handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// A CIC relay, joined with a connection code.
    pub hosted: bool,
    /// Hosted: the CIC portal's address, when the connection string named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRef {
    pub id: String,
    pub name: String,
    /// The group the event put this member in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// What the fleet client tells the panel.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    pub state: LinkState,
    /// The fleet connected to, or being connected to.
    pub fleet: Option<FleetRef>,
    /// The server's name, once it has said it.
    pub server: Option<String>,
    pub event: Option<EventRef>,
    /// Joined, but an admin has still to approve.
    pub pending: bool,
    /// The handle as the event knows it.
    pub handle: Option<String>,
    pub grants: Vec<Grant>,
    pub problem: Option<String>,
    /// What to do about a refusal, when its code says ("Make a new connection code ...").
    pub hint: Option<String>,
    /// Refused: the server's refusal code, when it gave one.
    pub code: Option<RefusalCode>,
    /// Connecting to a relay that cannot pick the event: the open events offered.
    pub choices: Option<Vec<EventChoice>>,
    pub retry_ms: Option<u64>,
    pub attempt: u32,
    /// The fleets joined before, for connecting again.
    pub fleets: Vec<FleetRef>,
    /// The fields this player agrees to send.
    pub consent: Vec<&'static str>,
}

pub struct FleetHub {
    me: PublicKey,
    /// The id the event knows this player by, from `joined`; the key's text form until then.
    member: Mutex<Option<String>>,
    roster: Mutex<Vec<RosterEntry>>,
    view: Arc<FleetViewStore>,
    cards: Mutex<Vec<Received>>,
    structure: Mutex<Option<EventStructure>>,
    link: Mutex<LinkInfo>,
    /// Peer-to-peer hosting, as the Hosting section shows it.
    host: Mutex<Option<HostView>>,
    /// Open detail views, by page session: their member gets the detail fields too.
    details: Mutex<HashMap<u64, String>>,
    next_viewer: AtomicU64,
    changes: watch::Sender<u64>,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub consent: Consent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberEntry {
    #[serde(flatten)]
    pub card: Card,
    pub status: Status,
    /// Roles above member in this event, highest first, for the icons on the card.
    pub roles: Vec<Role>,
    /// This player's own card, as the others receive it.
    pub me: bool,
    /// Milliseconds since the card was received.
    pub age_ms: u64,
    pub group: Option<String>,
    /// A relay-built ship card (`ship:<unit id>`).
    pub ship: bool,
    /// Ship cards: where the fields come from, as the card shows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupEntry {
    /// `None` for Ungrouped.
    pub id: Option<String>,
    pub name: String,
    pub collapsed: bool,
    /// Members received, downed first (status order), then in the viewer's order.
    pub members: Vec<String>,
    pub rollup: Rollup,
    /// Event groups: the leaders' member ids.
    pub leaders: Vec<String>,
}

/// A wing and its groups; `id` is `None` for the groups in no wing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WingEntry {
    pub id: Option<String>,
    pub name: String,
    pub commanders: Vec<String>,
    pub groups: Vec<GroupEntry>,
    pub rollup: Rollup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Layout {
    /// The event's unit tree, from the server.
    Event,
    /// The viewer's own groups.
    Personal,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    /// The id the event knows this player by: the CIC user id on a relay, else the key.
    pub member: String,
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetLimits {
    pub groups: usize,
    pub name: usize,
    pub members: usize,
}

/// The `fleet` push body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetModel {
    pub me: Me,
    pub link: LinkInfo,
    /// Online with at least one field agreed to: this player's card carries status.
    pub sharing: bool,
    pub layout: Layout,
    /// Whether this viewer's roles give the admiral view (always over their own groups).
    pub admiral_allowed: bool,
    pub schema: u32,
    pub stale_ms: u64,
    pub fields: Vec<FieldInfo>,
    pub limits: FleetLimits,
    pub view: FleetView,
    pub members: Vec<MemberEntry>,
    /// Personal layout: one section of the viewer's groups, then Ungrouped. Event layout: a
    /// two-level summary of the tree (top units of groups), for readers that predate `tree`.
    pub wings: Vec<WingEntry>,
    /// The same groups, flat, in the same order.
    pub groups: Vec<GroupEntry>,
    /// Event layout: the unit tree this viewer sees.
    pub tree: Option<EventTree>,
    /// Event layout, for the admiral and commanders: members with no seat and no role (names
    /// only, never card data).
    pub unassigned: Vec<Person>,
    /// Event layout: this player holds a seat or a role. Unassigned, they see their own card only.
    pub placed: bool,
    /// The admiral or a commander: seats, roles and units are changed in the CIC portal, so the
    /// panel links there.
    pub commander: bool,
    /// Hosted, with the portal's address known: the event's page in the CIC portal.
    pub event_url: Option<String>,
    /// Every member card received, ship cards left out.
    pub rollup: Rollup,
    /// Peer-to-peer hosting: the session, its invite and members, and the bans.
    pub host: Option<HostView>,
}

impl FleetHub {
    pub fn new(view: Arc<FleetViewStore>, me: PublicKey) -> Self {
        Self {
            me,
            member: Mutex::default(),
            roster: Mutex::default(),
            view,
            cards: Mutex::default(),
            structure: Mutex::default(),
            link: Mutex::default(),
            host: Mutex::default(),
            details: Mutex::default(),
            next_viewer: AtomicU64::new(1),
            changes: watch::Sender::new(0),
        }
    }

    pub fn view(&self) -> &Arc<FleetViewStore> {
        &self.view
    }

    pub fn me(&self) -> PublicKey {
        self.me
    }

    /// The id the event knows this player by, once joined.
    pub fn set_member(&self, member: Option<String>) {
        *lock(&self.member) = member;
        self.changed();
    }

    pub fn member(&self) -> String {
        lock(&self.member)
            .clone()
            .unwrap_or_else(|| self.me.to_string())
    }

    /// The server's roster. With no structure (a peer-to-peer host), a member the roster no
    /// longer lists (removed or kicked) has their card dropped at once.
    pub fn set_roster(&self, roster: Vec<RosterEntry>) {
        if !roster.is_empty() && lock(&self.structure).is_none() {
            let listed: HashSet<&str> = roster.iter().map(|e| e.member.as_str()).collect();
            lock(&self.cards).retain(|r| {
                r.card.ship_unit().is_some() || listed.contains(r.card.member.as_str())
            });
        }
        *lock(&self.roster) = roster;
        self.changed();
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n += 1);
    }

    pub fn set_link(&self, link: LinkInfo) {
        let mut held = lock(&self.link);
        if *held != link {
            *held = link;
            drop(held);
            self.changed();
        }
    }

    pub fn set_host(&self, host: Option<HostView>) {
        let mut held = lock(&self.host);
        if *held != host {
            *held = host;
            drop(held);
            self.changed();
        }
    }

    pub fn link(&self) -> LinkInfo {
        lock(&self.link).clone()
    }

    /// The event structure from the server; `None` puts the viewer's own groups back.
    pub fn set_structure(&self, structure: Option<EventStructure>) {
        *lock(&self.structure) = structure;
        self.changed();
    }

    pub fn structure(&self) -> Option<EventStructure> {
        lock(&self.structure).clone()
    }

    /// Forget every card and the structure, as after leaving a fleet.
    pub fn clear(&self) {
        *lock(&self.member) = None;
        self.clear_event();
    }

    /// Forget the cards, roster and structure of the event left behind; the member id stays.
    pub fn clear_event(&self) {
        lock(&self.cards).clear();
        lock(&self.roster).clear();
        *lock(&self.structure) = None;
        self.changed();
    }

    /// Changes on every card received and every detail view opened or closed.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// A card from the fleet transport, stamped with the receive time. A card with a newer
    /// schema, or a bad member id, is dropped. A ship card is taken here; the client takes one
    /// only from a relay.
    pub fn receive(&self, card: Card) -> bool {
        if card.schema > CARD_SCHEMA || !(is_member_id(&card.member) || card.ship_unit().is_some())
        {
            return false;
        }
        let mut cards = lock(&self.cards);
        let at = Instant::now();
        let full = cards.len() >= MAX_MEMBERS;
        match cards.iter_mut().find(|r| r.card.member == card.member) {
            Some(r) => *r = Received { card, at },
            None if full => {
                tracing::warn!("[fleet] more than {MAX_MEMBERS} members; dropped a card");
                return false;
            }
            None => cards.push(Received { card, at }),
        }
        drop(cards);
        self.changed();
        true
    }

    /// A key for one page's detail view.
    pub fn viewer(&self) -> u64 {
        self.next_viewer.fetch_add(1, Ordering::Relaxed)
    }

    /// Open (`Some`) or close a page's detail view of a member.
    pub fn set_detail(&self, viewer: u64, member: Option<&str>) {
        let mut details = lock(&self.details);
        match member.filter(|m| is_member_id(m)) {
            Some(m) => details.insert(viewer, m.to_owned()),
            None => details.remove(&viewer),
        };
        drop(details);
        self.changed();
    }

    /// The fields this viewer subscribes to from `member`: the chosen card fields, and the detail
    /// fields while a detail view of that member is open.
    pub fn wanted(&self, member: &str) -> HashSet<&'static str> {
        let view = self.view.get();
        let mut wanted: HashSet<&'static str> = card_fields()
            .filter(|f| view.fields.iter().any(|v| v == f.id))
            .map(|f| f.id)
            .collect();
        if lock(&self.details).values().any(|m| m == member) {
            wanted.extend(detail_fields().map(|f| f.id));
        }
        wanted
    }

    /// What this viewer asks senders for: the chosen card fields, and the detail fields while a
    /// detail view is open (a subscription names fields, not members).
    pub fn subscription(&self) -> Subscription {
        let view = self.view.get();
        let mut ids: Vec<&str> = card_fields()
            .filter(|f| view.fields.iter().any(|v| v == f.id))
            .map(|f| f.id)
            .collect();
        if !lock(&self.details).is_empty() {
            ids.extend(detail_fields().map(|f| f.id));
        }
        Subscription::of(ids)
    }

    pub fn model(&self) -> FleetModel {
        let view = self.view.get();
        let structure = lock(&self.structure).clone();
        let roster = lock(&self.roster).clone();
        let link = self.link();
        let me = self.member();
        let now = Instant::now();
        let cards = lock(&self.cards);
        let fresh_state = |member: &str| {
            cards
                .iter()
                .find(|r| r.card.member == member)
                .filter(|r| now.saturating_duration_since(r.at) < STALE_AFTER)
                .and_then(|r| r.card.state)
        };
        let members: Vec<MemberEntry> = cards
            .iter()
            .map(|r| {
                let age = now.saturating_duration_since(r.at);
                let stale = age >= STALE_AFTER;
                let age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
                let ship = r.card.ship_unit().is_some();
                MemberEntry {
                    status: match &r.card.source {
                        Some(source) if ship => ship_status(
                            source,
                            stale,
                            source.member.as_deref().and_then(fresh_state),
                        ),
                        _ => Status::of(r.card.state, stale),
                    },
                    age_ms,
                    group: match &structure {
                        Some(s) => panel_group(s, &r.card.member),
                        None => view.group_of(&r.card.member).map(|g| g.id.clone()),
                    },
                    roles: match &structure {
                        Some(s) => roles_of(s, &r.card.member),
                        None => vec![],
                    },
                    me: r.card.member == me,
                    ship,
                    source_label: r
                        .card
                        .source
                        .as_ref()
                        .filter(|_| ship)
                        .map(|source| source_label(source, stale, age_ms)),
                    card: r.card.clone(),
                }
            })
            .collect();
        drop(cards);
        let (wings, tree, unassigned, placed, commander) = match &structure {
            Some(s) => {
                let inputs = Inputs {
                    structure: s,
                    me: &me,
                    members: &members,
                    roster: &roster,
                };
                let grants = s.grants_of(&me);
                (
                    event_layout(s, &members),
                    Some(inputs.tree()),
                    inputs.unassigned(),
                    !grants.is_empty(),
                    grants.iter().any(|g| g.role.is_commander()),
                )
            }
            None => (
                vec![personal_layout(&view, &members)],
                None,
                vec![],
                true,
                false,
            ),
        };
        let event_url = link
            .fleet
            .as_ref()
            .filter(|f| f.hosted)
            .and_then(|f| f.portal.as_deref())
            .zip(link.event.as_ref())
            .and_then(|(portal, event)| event_url(portal, &event.id));
        let admiral_allowed =
            structure.is_none() || link.grants.iter().any(|g| g.role.has_admiral_view());
        FleetModel {
            me: Me {
                fingerprint: self.me.fingerprint(),
                key: self.me.to_string(),
                member: me,
            },
            sharing: link.state == LinkState::Online && !link.consent.is_empty(),
            link,
            layout: if structure.is_some() {
                Layout::Event
            } else {
                Layout::Personal
            },
            admiral_allowed,
            schema: CARD_SCHEMA,
            stale_ms: u64::try_from(STALE_AFTER.as_millis()).unwrap_or(u64::MAX),
            fields: FIELDS
                .iter()
                .map(|f| FieldInfo {
                    id: f.id,
                    label: f.label,
                    consent: f.consent,
                })
                .collect(),
            limits: FleetLimits {
                groups: MAX_GROUPS,
                name: MAX_GROUP_NAME,
                members: MAX_GROUP_MEMBERS,
            },
            rollup: rollup(members.iter().filter(|m| !m.ship).map(|m| m.status)),
            groups: wings.iter().flat_map(|w| w.groups.clone()).collect(),
            wings,
            tree,
            unassigned,
            placed,
            commander,
            event_url,
            view,
            members,
            host: lock(&self.host).clone(),
        }
    }
}

/// A ship card is live while it has a source, greyed once stale or offline. It carries no state of
/// its own, so a source that is DOWNED or DEAD on their own card (a relay falls back to the Pilot
/// when no one else can report) shows that state instead of UP.
fn ship_status(source: &ShipSource, stale: bool, source_state: Option<Life>) -> Status {
    match source.kind {
        _ if stale => Status::Offline,
        SourceKind::Pilot | SourceKind::Copilot | SourceKind::StandingIn => match source_state {
            Some(Life::Downed) => Status::Downed,
            Some(Life::Dead) => Status::Dead,
            _ => Status::Up,
        },
        SourceKind::Stale | SourceKind::Offline => Status::Offline,
        SourceKind::NotShared => Status::Unknown,
    }
}

/// The source line, counting the time since the card came on top of the relay's own age.
fn source_label(source: &ShipSource, stale: bool, age_ms: u64) -> String {
    if stale {
        return ShipSource::offline().label();
    }
    match source.kind {
        SourceKind::Stale => ShipSource {
            age_ms: Some(source.age_ms.unwrap_or(0).saturating_add(age_ms)),
            ..source.clone()
        }
        .label(),
        _ => source.label(),
    }
}

/// The lead roles `member` holds, highest first, each once.
fn roles_of(structure: &EventStructure, member: &str) -> Vec<Role> {
    let mut roles: Vec<Role> = structure
        .grants_of(member)
        .into_iter()
        .map(|g| g.role)
        .filter(|r| r.has_admiral_view())
        .collect();
    roles.sort_by_key(|r| r.rank());
    roles.dedup();
    roles
}

fn status_in(members: &[MemberEntry]) -> impl Fn(&String) -> Status + '_ {
    |id| {
        members
            .iter()
            .find(|m| &m.card.member == id)
            .map_or(Status::Offline, |m| m.status)
    }
}

/// A group of the members received, downed first, with its roll-up.
fn group_entry(
    members: &[MemberEntry],
    id: Option<String>,
    name: String,
    collapsed: bool,
    wanted: impl IntoIterator<Item = String>,
    leaders: Vec<String>,
) -> GroupEntry {
    let status_of = status_in(members);
    let mut ids: Vec<String> = wanted
        .into_iter()
        .filter(|id| members.iter().any(|m| &m.card.member == id))
        .collect();
    sort_by_status(&mut ids, &status_of);
    GroupEntry {
        rollup: rollup(ids.iter().map(&status_of)),
        id,
        name,
        collapsed,
        members: ids,
        leaders,
    }
}

fn wing_entry(
    members: &[MemberEntry],
    id: Option<String>,
    name: String,
    commanders: Vec<String>,
    groups: Vec<GroupEntry>,
) -> WingEntry {
    let status_of = status_in(members);
    WingEntry {
        rollup: rollup(groups.iter().flat_map(|g| g.members.iter()).map(status_of)),
        id,
        name,
        commanders,
        groups,
    }
}

/// The two-level summary kept for readers of `wings` and `groups` (the panel draws `tree`): a
/// top unit (fleet, wing, ground) is a wing, and a fleet's ship, a squadron or a squad is a group.
fn panel_group(s: &EventStructure, member: &str) -> Option<String> {
    let (seat, _) = s.seat_of(member)?;
    match s.parent_of(seat) {
        Some(p) if p.kind == UnitKind::Squadron => Some(p.id.clone()),
        _ => Some(seat.id.clone()),
    }
}

/// The event's top units in order, each with its groups, then the members in no group (when
/// there are any).
fn event_layout(s: &EventStructure, members: &[MemberEntry]) -> Vec<WingEntry> {
    let children = |parent: &str| -> Vec<&fleet_proto::Unit> {
        s.units
            .iter()
            .filter(|u| u.parent.as_deref() == Some(parent))
            .collect()
    };
    let mut wings: Vec<WingEntry> = s
        .units
        .iter()
        .filter(|u| u.parent.is_none())
        .map(|top| {
            let groups = children(&top.id)
                .into_iter()
                .map(|g| {
                    let seated = s
                        .subtree(&g.id)
                        .into_iter()
                        .flat_map(|u| u.seats.iter().map(|seat| seat.member.clone()));
                    group_entry(
                        members,
                        Some(g.id.clone()),
                        g.name.clone(),
                        false,
                        seated.collect::<Vec<_>>(),
                        g.lead.clone(),
                    )
                })
                .collect();
            wing_entry(
                members,
                Some(top.id.clone()),
                top.name.clone(),
                top.lead.clone(),
                groups,
            )
        })
        .collect();
    let ungrouped: Vec<String> = members
        .iter()
        .filter(|m| m.group.is_none())
        .map(|m| m.card.member.clone())
        .collect();
    if !ungrouped.is_empty() {
        let loose = group_entry(
            members,
            None,
            "Unassigned".to_owned(),
            false,
            ungrouped,
            vec![],
        );
        wings.push(wing_entry(
            members,
            None,
            String::new(),
            vec![],
            vec![loose],
        ));
    }
    wings
}

/// The viewer's groups in order, then Ungrouped, as one section with no wing.
fn personal_layout(view: &FleetView, members: &[MemberEntry]) -> WingEntry {
    let mut groups: Vec<GroupEntry> = view
        .groups
        .iter()
        .map(|g| {
            group_entry(
                members,
                Some(g.id.clone()),
                g.name.clone(),
                g.collapsed,
                g.members.clone(),
                vec![],
            )
        })
        .collect();
    let ungrouped = members
        .iter()
        .filter(|m| m.group.is_none())
        .map(|m| m.card.member.clone());
    groups.push(group_entry(
        members,
        None,
        "Ungrouped".to_owned(),
        false,
        ungrouped,
        vec![],
    ));
    wing_entry(members, None, String::new(), vec![], groups)
}
