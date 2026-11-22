//! The viewer's side of a fleet: the cards received (with the receive time, which "updated N s
//! ago" and OFFLINE are measured from), what this viewer subscribes to, and the model the
//! dashboard's fleet panel draws. A real fleet transport hands received cards to
//! [`FleetHub::receive`] and asks [`FleetHub::wanted`] what to subscribe to, as the demo does.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use super::card::{CARD_SCHEMA, Card, Consent, FIELDS, card_fields, detail_fields};
use super::view::{
    FleetView, FleetViewStore, MAX_GROUP_MEMBERS, MAX_GROUP_NAME, MAX_GROUPS, is_member_id,
};
use fleet_proto::rollup::{Rollup, Status, rollup, sort_by_status};

/// A card not updated for this long shows OFFLINE.
pub const STALE_AFTER: Duration = Duration::from_secs(10);
/// Cards past this many members are dropped (and logged once each).
pub const MAX_MEMBERS: usize = 64;

struct Received {
    card: Card,
    at: Instant,
}

pub struct FleetHub {
    demo: bool,
    view: Arc<FleetViewStore>,
    cards: Mutex<Vec<Received>>,
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
    /// Milliseconds since the card was received.
    pub age_ms: u64,
    pub group: Option<String>,
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
    pub demo: bool,
    pub schema: u32,
    pub stale_ms: u64,
    pub fields: Vec<FieldInfo>,
    pub limits: FleetLimits,
    pub view: FleetView,
    pub members: Vec<MemberEntry>,
    /// The viewer's groups in order, then Ungrouped.
    pub groups: Vec<GroupEntry>,
    /// Every member, by status across groups, for the admiral view.
    pub admiral: Vec<String>,
    pub rollup: Rollup,
}

impl FleetHub {
    pub fn new(view: Arc<FleetViewStore>, demo: bool) -> Self {
        Self {
            demo,
            view,
            cards: Mutex::default(),
            details: Mutex::default(),
            next_viewer: AtomicU64::new(1),
            changes: watch::Sender::new(0),
        }
    }

    pub fn view(&self) -> &Arc<FleetViewStore> {
        &self.view
    }

    pub fn is_demo(&self) -> bool {
        self.demo
    }

    /// Changes on every card received and every detail view opened or closed.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// A card from the fleet transport, stamped with the receive time. A card with a newer
    /// schema, or a bad member id, is dropped.
    pub fn receive(&self, card: Card) -> bool {
        if card.schema > CARD_SCHEMA || !is_member_id(&card.member) {
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
        self.changes.send_modify(|n| *n += 1);
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
        self.changes.send_modify(|n| *n += 1);
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

    pub fn model(&self) -> FleetModel {
        let view = self.view.get();
        let now = Instant::now();
        let members: Vec<MemberEntry> = lock(&self.cards)
            .iter()
            .map(|r| {
                let age = now.saturating_duration_since(r.at);
                MemberEntry {
                    status: Status::of(r.card.state, age >= STALE_AFTER),
                    age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
                    group: view.group_of(&r.card.member).map(|g| g.id.clone()),
                    card: r.card.clone(),
                }
            })
            .collect();
        let status_of = |id: &String| {
            members
                .iter()
                .find(|m| &m.card.member == id)
                .map_or(Status::Offline, |m| m.status)
        };
        let entry = |id: Option<String>, name: String, collapsed: bool, mut ids: Vec<String>| {
            sort_by_status(&mut ids, status_of);
            GroupEntry {
                rollup: rollup(ids.iter().map(status_of)),
                id,
                name,
                collapsed,
                members: ids,
            }
        };
        let received = |id: &String| members.iter().any(|m| &m.card.member == id);
        let mut groups: Vec<GroupEntry> = view
            .groups
            .iter()
            .map(|g| {
                let ids = g.members.iter().filter(|m| received(m)).cloned().collect();
                entry(Some(g.id.clone()), g.name.clone(), g.collapsed, ids)
            })
            .collect();
        let ungrouped = members
            .iter()
            .filter(|m| m.group.is_none())
            .map(|m| m.card.member.clone())
            .collect();
        groups.push(entry(None, "Ungrouped".to_owned(), false, ungrouped));
        let mut admiral: Vec<String> = groups.iter().flat_map(|g| g.members.clone()).collect();
        sort_by_status(&mut admiral, status_of);
        FleetModel {
            demo: self.demo,
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
            rollup: rollup(members.iter().map(|m| m.status)),
            view,
            members,
            groups,
            admiral,
        }
    }
}
