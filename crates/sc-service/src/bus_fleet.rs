//! The bus's `fleet.status` capability (PROTOCOL.md section 1, Fleet status): a read-only reader,
//! such as the Hardpoint Fleet Status Stream Deck plugin, that gets the fleet's roll-ups, who is
//! down and each visible member's state, as this viewer's fleet panel sees them. It gets no game
//! frames, does not make the service follow the game and may call only `fleet.status`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use fleet_proto::rollup::{Rollup, Status, rollup};

use crate::bus::{Incoming, TOO_BIG, close_frame, drain, incoming, parse};
use crate::fleet::FleetHub;
use crate::fleet::hub::{FleetModel, LinkState, MemberEntry};
use crate::fleet::tree::{Person, UnitNode};
use crate::json::{is_integer, to_text};

pub const CAPABILITY: &str = "fleet.status";
/// Cards go OFFLINE with time alone, so the status is rebuilt this often as well as on changes.
const RECHECK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetStatus {
    pub link: LinkState,
    pub event: Option<EventName>,
    /// Event layout: this player holds a seat or a role. Unassigned, only their own card counts.
    pub placed: bool,
    /// The whole fleet first, then the units (or the viewer's own groups) in panel order.
    pub scopes: Vec<ScopeStatus>,
    pub members: Vec<MemberStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventName {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeStatus {
    /// `fleet`, `unit:<unit id>` or `group:<group id>`.
    pub id: String,
    pub name: String,
    pub kind: String,
    /// 0 for the whole fleet.
    pub depth: usize,
    /// False for a unit above the viewer's own: their role does not cover it, so nothing is
    /// counted.
    pub open: bool,
    pub rollup: Rollup,
    /// DOWNED and DEAD members counted here.
    pub down: Vec<Down>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Down {
    pub member: String,
    pub handle: String,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberStatus {
    /// Stable, unlike the handle.
    pub member: String,
    pub handle: String,
    pub status: Status,
    /// 0 to 1, when they share it and this viewer asks for it.
    pub health: Option<f64>,
    pub me: bool,
}

impl FleetStatus {
    fn empty(link: LinkState) -> Self {
        Self {
            link,
            event: None,
            placed: false,
            scopes: vec![],
            members: vec![],
        }
    }
}

pub fn status(hub: Option<&FleetHub>) -> FleetStatus {
    let Some(hub) = hub else {
        return FleetStatus::empty(LinkState::None);
    };
    let m = hub.model();
    if m.link.state != LinkState::Online {
        return FleetStatus::empty(m.link.state);
    }
    let structure = hub.structure();
    let people: Vec<&MemberEntry> = m.members.iter().filter(|e| !e.ship).collect();
    let down_of = |ids: &mut dyn Iterator<Item = &String>| -> Vec<Down> {
        let mut out: Vec<Down> = vec![];
        for id in ids {
            let Some(e) = people.iter().find(|e| &e.card.member == id) else {
                continue;
            };
            let down = matches!(e.status, Status::Downed | Status::Dead);
            if down && !out.iter().any(|d| &d.member == id) {
                out.push(Down {
                    member: id.clone(),
                    handle: e.card.handle.clone(),
                    status: e.status,
                });
            }
        }
        out
    };
    let mut members: Vec<MemberStatus> = people
        .iter()
        .filter(|e| m.placed || e.me)
        .map(|e| MemberStatus {
            member: e.card.member.clone(),
            handle: e.card.handle.clone(),
            status: e.status,
            health: e.card.fields.get("char.health").and_then(|v| v.fraction),
            me: e.me,
        })
        .collect();

    let (event, scopes) = match (&m.tree, &structure) {
        (Some(tree), Some(s)) if m.placed => {
            let seated: Vec<String> = s
                .units
                .iter()
                .flat_map(|u| u.seats.iter().map(|seat| seat.member.clone()))
                .collect();
            let mut scopes = vec![ScopeStatus {
                id: "fleet".to_owned(),
                name: tree.name.clone(),
                kind: "Event".to_owned(),
                depth: 0,
                open: true,
                rollup: tree.rollup.clone(),
                down: down_of(&mut seated.iter()),
            }];
            let mut walk = Walk {
                scopes: &mut scopes,
                members: &mut members,
                rollup_members: &|id: &str| s.rollup_members(id),
                down_of: &down_of,
            };
            for u in &tree.units {
                walk.unit(u, 1);
            }
            let event = EventName {
                id: tree.id.clone(),
                name: tree.name.clone(),
            };
            (Some(event), scopes)
        }
        (Some(tree), _) => {
            let own = rollup(people.iter().filter(|e| e.me).map(|e| e.status));
            let mine: Vec<String> = members.iter().map(|x| x.member.clone()).collect();
            let fleet = ScopeStatus {
                id: "fleet".to_owned(),
                name: tree.name.clone(),
                kind: "Event".to_owned(),
                depth: 0,
                open: true,
                rollup: own,
                down: down_of(&mut mine.iter()),
            };
            let event = EventName {
                id: tree.id.clone(),
                name: tree.name.clone(),
            };
            (Some(event), vec![fleet])
        }
        _ => {
            let session = m.link.event.as_ref().map(|e| EventName {
                id: e.id.clone(),
                name: e.name.clone(),
            });
            let scopes = personal_scopes(&m, session.as_ref(), &down_of);
            (session, scopes)
        }
    };
    FleetStatus {
        link: m.link.state,
        event,
        placed: m.placed,
        scopes,
        members,
    }
}

struct Walk<'a> {
    scopes: &'a mut Vec<ScopeStatus>,
    members: &'a mut Vec<MemberStatus>,
    rollup_members: &'a dyn Fn(&str) -> Vec<String>,
    down_of: &'a dyn Fn(&mut dyn Iterator<Item = &String>) -> Vec<Down>,
}

impl Walk<'_> {
    fn unit(&mut self, u: &UnitNode, depth: usize) {
        let (rollup, down) = if u.open {
            let counted = (self.rollup_members)(&u.id);
            (u.rollup.clone(), (self.down_of)(&mut counted.iter()))
        } else {
            (Rollup::default(), vec![])
        };
        self.scopes.push(ScopeStatus {
            id: format!("unit:{}", u.id),
            name: u.name.clone(),
            kind: u.kind_label.to_owned(),
            depth,
            open: u.open,
            rollup,
            down,
        });
        if u.open {
            let people = u.leads.iter().chain(u.seats.iter().map(|s| &s.person));
            for p in people {
                self.not_connected(p);
            }
        }
        for child in &u.units {
            self.unit(child, depth + 1);
        }
    }

    /// A member in a unit the viewer sees, with no card yet, is listed as OFFLINE.
    fn not_connected(&mut self, p: &Person) {
        if !self.members.iter().any(|x| x.member == p.member) {
            self.members.push(MemberStatus {
                member: p.member.clone(),
                handle: p.handle.clone(),
                status: Status::Offline,
                health: None,
                me: false,
            });
        }
    }
}

/// No event structure, as from a peer-to-peer host: the whole session, then the viewer's own
/// groups standing in for units (Ungrouped is left out).
fn personal_scopes(
    m: &FleetModel,
    session: Option<&EventName>,
    down_of: &dyn Fn(&mut dyn Iterator<Item = &String>) -> Vec<Down>,
) -> Vec<ScopeStatus> {
    let everyone: Vec<String> = m
        .members
        .iter()
        .filter(|e| !e.ship)
        .map(|e| e.card.member.clone())
        .collect();
    let fleet = ScopeStatus {
        id: "fleet".to_owned(),
        name: session
            .map(|e| e.name.clone())
            .or_else(|| m.link.server.clone())
            .unwrap_or_else(|| "Fleet".to_owned()),
        kind: "Fleet".to_owned(),
        depth: 0,
        open: true,
        rollup: m.rollup.clone(),
        down: down_of(&mut everyone.iter()),
    };
    let groups = m.groups.iter().filter_map(|g| {
        Some(ScopeStatus {
            id: format!("group:{}", g.id.as_ref()?),
            name: g.name.clone(),
            kind: "Group".to_owned(),
            depth: 1,
            open: true,
            rollup: g.rollup.clone(),
            down: down_of(&mut g.members.iter()),
        })
    });
    std::iter::once(fleet).chain(groups).collect()
}

pub(crate) fn wants(hello: Option<&Value>) -> bool {
    hello
        .and_then(|h| h.get("capabilities"))
        .and_then(Value::as_array)
        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(CAPABILITY)))
}

fn push(status: &FleetStatus) -> String {
    to_text(&json!({ "type": "fleet.status", "status": status }))
}

/// A reader's connection after its welcome: the status now and on every change, and answers to
/// `fleet.status` calls. Every other call is refused.
pub(crate) async fn serve(
    mut sink: SplitSink<WebSocket, Message>,
    mut stream: SplitStream<WebSocket>,
    hub: Option<Arc<FleetHub>>,
    mut closing: watch::Receiver<bool>,
) {
    let mut changes = hub.as_ref().map(|h| h.subscribe());
    let mut tick = tokio::time::interval(RECHECK);
    let mut last: Option<FleetStatus> = None;
    loop {
        let answer = tokio::select! {
            frame = stream.next() => match incoming(frame) {
                Incoming::Message(message @ (Message::Text(_) | Message::Binary(_))) => {
                    match answer(&message, hub.as_deref()) {
                        Some(text) => text,
                        None => continue,
                    }
                }
                Incoming::Message(Message::Close(_)) | Incoming::Gone => return,
                Incoming::Message(_) => continue,
                Incoming::TooBig => {
                    let _ = sink.send(close_frame(TOO_BIG, "")).await;
                    drain(&mut stream).await;
                    return;
                }
            },
            Some(Ok(())) = async { Some(changes.as_mut()?.changed().await) } => String::new(),
            _ = tick.tick() => String::new(),
            _ = closing.changed() => return,
        };
        let now = status(hub.as_deref());
        if last.as_ref() != Some(&now) {
            if sink.send(Message::Text(push(&now).into())).await.is_err() {
                return;
            }
            last = Some(now);
        }
        if !answer.is_empty() && sink.send(Message::Text(answer.into())).await.is_err() {
            return;
        }
    }
}

fn answer(message: &Message, hub: Option<&FleetHub>) -> Option<String> {
    let message = parse(message)?;
    let id = message.get("id").filter(|id| is_integer(id))?;
    if message.get("type").and_then(Value::as_str) != Some("call") {
        return None;
    }
    let op = message.get("call").and_then(|c| c.get("op"));
    let reply = if op.and_then(Value::as_str) == Some(CAPABILITY) {
        json!({ "type": "result", "id": id, "result": status(hub) })
    } else {
        json!({ "type": "result", "id": id, "error": "this client said fleet.status: only the fleet.status call is answered" })
    };
    Some(to_text(&reply))
}
