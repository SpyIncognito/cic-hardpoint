//! Peer-to-peer hosting (mode B, PROTOCOL 4.1 and 4.4): this player's app is the fleet server
//! for an impromptu group of friends. It listens with [`tls::Listener`], runs the server half of
//! the identity handshake bound to the TLS session, takes `join` with the session's code, and
//! relays each member's card to everyone else, after the sender's own consent filter.
//!
//! No roles, units or seats go on the wire. The host keeps a flat tree in memory only
//! ([`EventStructure::p2p_default`]), so the visibility rules work and the host's own card,
//! seated as a Pilot, reaches everyone. The host moderates: Kick (bans the key and replaces
//! the code), Remove (drops the member, who may join again with the current code) and New code.
//! The host's own Hardpoint joins over loopback with the host's key, like any member.
//!
//! The session lives while hosting is on: the code is in memory only, and a host restart is a
//! new session, so a rejoin by key is refused with `session_ended`. Bans are on disk
//! ([`super::bans`]). Logs carry counts only, never handles, keys, codes or card fields.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader, split};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use fleet_proto::card::{MAX_HANDLE, card_fields, is_handle};
use fleet_proto::hello::capability;
use fleet_proto::identity::ChallengeIssuer;
use fleet_proto::invite::new_join_code;
use fleet_proto::message::{BYE_LEAVE, CARD_EVERY_MS};
use fleet_proto::structure::{EventInfo, EventState, EventStructure};
use fleet_proto::{
    Card, Hello, Identity, Message, PublicKey, RefusalCode, RosterEntry, Subscription,
};

use super::bans::{Ban, BanStore};
use super::client::{FleetClient, read_message, tls_binding, write_message};
use super::hub::FleetHub;
use crate::tls;

/// The host's port, beside the pages' 47520 and the companion relay's 47521.
pub const DEFAULT_PORT: u16 = 47522;
/// The key in `app-settings.json`: `{ "host": { "port"?, "publicAddress"?, "handle"? } }`.
pub const SETTINGS_KEY: &str = "fleet";
/// A connection's outgoing queue; a member that falls this far behind misses cards.
const OUTBOX: usize = 256;

/// The listener's exposure limits (PROTOCOL 4.4). Tests may lower them.
#[derive(Debug, Clone)]
pub struct Limits {
    /// TLS and the identity handshake, from the accept.
    pub handshake: Duration,
    /// `join`, from the accept.
    pub join: Duration,
    /// Connections not yet joined, at once.
    pub max_pending: usize,
    /// Wrong codes per address in `failure_window` before the address is refused.
    pub max_failures: usize,
    pub failure_window: Duration,
    /// Members of the session, the host included.
    pub max_members: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            join: Duration::from_secs(30),
            max_pending: 32,
            max_failures: 10,
            failure_window: Duration::from_secs(600),
            max_members: super::hub::MAX_MEMBERS,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Failed codes per address in a sliding window, as the CIC relay counts failed connects.
#[derive(Debug, Default)]
struct Failures {
    seen: HashMap<IpAddr, VecDeque<Instant>>,
}

impl Failures {
    fn prune(&mut self, window: Duration) {
        let now = Instant::now();
        self.seen.retain(|_, times| {
            while times.front().is_some_and(|t| now - *t >= window) {
                times.pop_front();
            }
            !times.is_empty()
        });
    }

    fn blocked(&mut self, ip: IpAddr, max: usize, window: Duration) -> bool {
        self.prune(window);
        self.seen.get(&ip).is_some_and(|t| t.len() >= max)
    }

    fn fail(&mut self, ip: IpAddr, max: usize, window: Duration) {
        self.prune(window);
        // A flood of addresses cannot grow this without bound.
        if self.seen.len() >= 10_000 && !self.seen.contains_key(&ip) {
            return;
        }
        let times = self.seen.entry(ip).or_default();
        times.push_back(Instant::now());
        while times.len() > max {
            times.pop_front();
        }
    }
}

enum Out {
    Send(Message),
    /// Send this, then close.
    Close(Message),
}

struct Conn {
    member: String,
    tx: mpsc::Sender<Out>,
    /// What this viewer asked for; every card field until it says.
    subscription: Option<Subscription>,
    /// What this sender was last asked for.
    asked: Option<Subscription>,
}

/// One hosting session: lives from Start to Stop.
struct Session {
    event: EventInfo,
    code: String,
    /// Flat, in memory only: for the visibility rules, never sent.
    tree: EventStructure,
    handles: HashMap<String, String>,
    /// Members who may rejoin by key (not removed or kicked).
    keys: HashSet<String>,
    conns: HashMap<u64, Conn>,
    failures: Failures,
    cards: u64,
}

impl Session {
    fn unique_handle(&self, wanted: &str, member: &str) -> String {
        let taken = |h: &str| {
            self.handles
                .iter()
                .any(|(m, other)| m != member && other.eq_ignore_ascii_case(h))
        };
        if !taken(wanted) {
            return wanted.to_owned();
        }
        (2u32..)
            .map(|n| {
                let suffix = format!("-{n}");
                let base: String = wanted.chars().take(MAX_HANDLE - suffix.len()).collect();
                format!("{base}{suffix}")
            })
            .find(|h| !taken(h))
            .unwrap_or_default()
    }

    fn connected(&self, member: &str) -> bool {
        self.conns.values().any(|c| c.member == member)
    }

    /// Takes `member` out of the session: tree, handle, rejoin by key. Their connection, if
    /// any, is closed with `bye`.
    fn forget(&mut self, member: &str, bye: Message) {
        if let Some(id) = self
            .tree
            .seat_of(member)
            .map(|(u, _)| u.id.clone())
            .filter(|_| self.tree.admiral.as_deref() != Some(member))
        {
            self.tree.units.retain(|u| u.id != id);
        }
        self.tree.remove_member(member);
        self.handles.remove(member);
        self.keys.remove(member);
        let ids: Vec<u64> = self
            .conns
            .iter()
            .filter(|(_, c)| c.member == member)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(c) = self.conns.remove(&id) {
                let _ = c.tx.try_send(Out::Close(bye.clone()));
            }
        }
    }

    /// The roster every member gets: everyone in the session, no roles or units.
    fn roster(&self) -> Message {
        Message::Roster {
            you: vec![],
            members: self
                .tree
                .members()
                .into_iter()
                .map(|m| RosterEntry {
                    handle: self.handles.get(&m).cloned(),
                    role: None,
                    unit: None,
                    key: self.connected(&m).then(|| m.parse().ok()).flatten(),
                    member: m,
                })
                .collect(),
        }
    }

    /// After a change: the roster to everyone, and each sender asked for what its audience
    /// wants (the CIC relay's rule: a viewer with no subscription counts as every card field).
    fn changed(&mut self) {
        let roster = self.roster();
        for c in self.conns.values() {
            let _ = c.tx.try_send(Out::Send(roster.clone()));
        }
        self.ask();
    }

    fn ask(&mut self) {
        let every = Subscription::of(card_fields().map(|f| f.id));
        let wanted: Vec<(u64, Subscription)> = self
            .conns
            .iter()
            .map(|(id, sender)| {
                let audience = self.tree.card_audience(&sender.member);
                let sub = self
                    .conns
                    .values()
                    .filter(|v| v.member != sender.member && audience.contains(&v.member))
                    .fold(Subscription::default(), |all, v| {
                        all.union(v.subscription.as_ref().unwrap_or(&every))
                    });
                (*id, sub)
            })
            .collect();
        for (id, sub) in wanted {
            if let Some(c) = self.conns.get_mut(&id)
                && c.asked.as_ref() != Some(&sub)
            {
                let sent = c.tx.try_send(Out::Send(Message::Subscribe {
                    subscription: sub.clone(),
                }));
                if sent.is_ok() {
                    c.asked = Some(sub);
                }
            }
        }
    }

    /// Relays `card` from `member` to everyone who may see it, each with the fields they
    /// subscribed to. Returns how many received it.
    fn relay(&mut self, member: &str, mut card: Card) -> usize {
        if let Some(h) = self.handles.get(member) {
            card.handle.clone_from(h);
        }
        let audience: BTreeSet<String> = self.tree.card_audience(member);
        let mut sent = 0;
        for c in self.conns.values() {
            if c.member == member || !audience.contains(&c.member) {
                continue;
            }
            let mut out = card.clone();
            if let Some(sub) = &c.subscription {
                out.fields.retain(|id| sub.wants(id));
            }
            if c.tx
                .try_send(Out::Send(Message::Card { card: out }))
                .is_ok()
            {
                sent += 1;
            }
        }
        self.cards += 1;
        sent
    }
}

/// One member in the host's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostMember {
    pub member: String,
    pub handle: String,
    pub fingerprint: String,
    pub connected: bool,
    /// The host's own entry, which cannot be kicked or removed.
    pub host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BanView {
    pub key: String,
    pub fingerprint: String,
    pub handle: String,
    pub at: i64,
}

/// What the fleet panel's Hosting section shows (`FleetModel.host`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    pub hosting: bool,
    /// The port hosting uses, or will use.
    pub port: u16,
    pub handle: String,
    pub fingerprint: String,
    /// This machine's LAN address, the route out.
    pub lan_address: Option<String>,
    /// The address or DNS name the host typed for players outside the LAN.
    pub public_address: Option<String>,
    /// While hosting: the code and the invites with it.
    pub code: Option<String>,
    pub invite: Option<String>,
    pub public_invite: Option<String>,
    pub members: Vec<HostMember>,
    pub bans: Vec<BanView>,
    pub max_members: usize,
    pub problem: Option<String>,
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    port: u16,
}

pub struct FleetHost {
    identity: Identity,
    bans: BanStore,
    hub: Arc<FleetHub>,
    limits: Limits,
    issuer: Mutex<ChallengeIssuer>,
    session: Mutex<Option<Session>>,
    running: Mutex<Option<Running>>,
    /// Port, handle and public address, as last set.
    setup: Mutex<(u16, String, Option<String>)>,
    problem: Mutex<Option<String>>,
    pending: AtomicUsize,
    next_conn: AtomicU64,
    bind: IpAddr,
    /// `app-settings.json`, where the port, handle and public address are kept.
    settings: Option<PathBuf>,
}

/// Counts a connection not yet joined; dropped when it joins or ends.
struct Pending<'a>(&'a AtomicUsize);

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The characters kept as they are in the invite's `name`.
const NAME_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.');

/// `cic://host:port?key=K&code=C&name=<host handle>`; `name` is a label for the join screen only.
pub fn invite(host: &str, port: u16, key: &PublicKey, code: &str, name: &str) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!(
        "cic://{host}:{port}?key={key}&code={code}&name={}",
        utf8_percent_encode(name, NAME_SET)
    )
}

/// The `name` an invite gives its host, decoded; `None` when absent or not a handle.
pub fn host_name_of(invite: &str) -> Option<String> {
    let (_, query) = invite.trim().split_once('?')?;
    let raw = query.split('&').find_map(|p| p.strip_prefix("name="))?;
    let name = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?
        .into_owned();
    is_handle(&name).then_some(name)
}

/// The next message before `by`; anything else ends the connection.
async fn read_by<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    by: Instant,
) -> io::Result<Message> {
    match timeout_at(by, read_message(reader, buf)).await {
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "too slow")),
        Ok(Ok(Some(Ok(m)))) => Ok(m),
        Ok(Ok(_)) => Err(io::Error::other("not a fleet message")),
        Ok(Err(e)) => Err(e),
    }
}

/// The port, handle and public address in `app-settings.json`.
fn saved(file: &std::path::Path) -> (Option<u16>, Option<String>, Option<String>) {
    let host = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get(SETTINGS_KEY)?.get("host").cloned());
    let Some(host) = host else {
        return (None, None, None);
    };
    let text = |k: &str| {
        host.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    (
        host.get("port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0),
        text("handle").filter(|h| is_handle(h)),
        text("publicAddress").filter(|h| fleet_proto::invite::is_host(h)),
    )
}

/// Starts hosting and joins it from this Hardpoint over loopback, so the host's card goes
/// through the same consent filter and preview as any member's. Another fleet's connection
/// closes first.
pub async fn start_hosting(
    host: &Arc<FleetHost>,
    client: &Arc<FleetClient>,
    port: u16,
    handle: &str,
) -> Result<u16, String> {
    let bound = host.start(port, handle).await?;
    let invite = host.loopback_invite().ok_or("Not hosting")?;
    if let Err(e) = client.join(&invite, handle) {
        host.stop().await;
        return Err(e);
    }
    Ok(bound)
}

/// Leaves this Hardpoint's own session, then stops hosting.
pub async fn stop_hosting(host: &Arc<FleetHost>, client: &Arc<FleetClient>) {
    if host.is_hosting() {
        let _ = client.leave(&host.key().to_string()).await;
    }
    host.stop().await;
}

/// The handle as joined: trimmed, or `Member` when it is not one.
fn clean_handle(raw: &str) -> String {
    let h = raw.trim();
    if is_handle(h) {
        h.to_owned()
    } else {
        "Member".to_owned()
    }
}

fn refused(reason: &str, code: RefusalCode) -> Message {
    Message::Refused {
        reason: reason.to_owned(),
        code: Some(code),
    }
}

fn same_code(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// The event name for a host: `<handle>'s fleet`, shortened to fit.
fn event_name(handle: &str) -> String {
    let name = format!("{handle}'s fleet");
    if fleet_proto::structure::is_name(&name) {
        name
    } else {
        "Fleet".to_owned()
    }
}

impl FleetHost {
    /// `bind` is every interface in the app; tests pass loopback. The port, handle and public
    /// address saved in `settings` are read here.
    pub fn new(
        identity: &Identity,
        bans: BanStore,
        hub: Arc<FleetHub>,
        bind: IpAddr,
        limits: Limits,
        settings: Option<PathBuf>,
    ) -> Arc<Self> {
        let saved = settings.as_deref().map(saved).unwrap_or_default();
        let host = Arc::new(Self {
            identity: Identity::from_seed(identity.seed()),
            bans,
            hub,
            limits,
            issuer: Mutex::default(),
            session: Mutex::default(),
            running: Mutex::default(),
            setup: Mutex::new((
                saved.0.unwrap_or(DEFAULT_PORT),
                saved.1.unwrap_or_default(),
                saved.2,
            )),
            problem: Mutex::default(),
            pending: AtomicUsize::new(0),
            next_conn: AtomicU64::new(1),
            bind,
            settings,
        });
        host.publish();
        host
    }

    pub fn key(&self) -> PublicKey {
        self.identity.public()
    }

    /// The saved port, handle and public address, before hosting starts.
    pub fn set_up(&self, port: Option<u16>, handle: Option<String>, public: Option<String>) {
        let mut setup = lock(&self.setup);
        if let Some(p) = port.filter(|p| *p != 0) {
            setup.0 = p;
        }
        if let Some(h) = handle.filter(|h| is_handle(h)) {
            setup.1 = h;
        }
        if public.is_some() {
            setup.2 = public.filter(|p| !p.is_empty());
        }
        drop(setup);
        self.save();
        self.publish();
    }

    /// The public address or DNS name players outside the LAN use, `host` or `host:port`
    /// (the port is the hosting port either way); empty clears it.
    pub fn set_public_address(&self, typed: &str) -> Result<(), String> {
        let typed = typed.trim();
        let host = match typed.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => h,
            _ => typed,
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if !typed.is_empty() && !fleet_proto::invite::is_host(host) {
            return Err("The public address must be a host name or an IP address".to_owned());
        }
        lock(&self.setup).2 = (!host.is_empty()).then(|| host.to_owned());
        self.save();
        self.publish();
        Ok(())
    }

    fn save(&self) {
        let Some(file) = &self.settings else { return };
        let (port, handle, public) = self.setup();
        let mut host = serde_json::Map::new();
        host.insert("port".to_owned(), port.into());
        if !handle.is_empty() {
            host.insert("handle".to_owned(), handle.into());
        }
        if let Some(p) = public {
            host.insert("publicAddress".to_owned(), p.into());
        }
        let value = serde_json::json!({ "host": host });
        if let Err(e) = crate::app_settings::write_field(file, SETTINGS_KEY, value) {
            tracing::warn!("[host] could not save the hosting settings: {e}");
        }
    }

    pub fn setup(&self) -> (u16, String, Option<String>) {
        lock(&self.setup).clone()
    }

    /// The port listened on while hosting.
    pub fn port(&self) -> Option<u16> {
        lock(&self.running).as_ref().map(|r| r.port)
    }

    pub fn is_hosting(&self) -> bool {
        lock(&self.running).is_some()
    }

    /// The current code, while hosting.
    pub fn code(&self) -> Option<String> {
        lock(&self.session).as_ref().map(|s| s.code.clone())
    }

    /// The invite to this host over loopback, for the host's own Hardpoint.
    pub fn loopback_invite(&self) -> Option<String> {
        let (port, code) = (self.port()?, self.code()?);
        let host = if self.bind.is_ipv6() {
            "::1"
        } else {
            "127.0.0.1"
        };
        Some(invite(host, port, &self.key(), &code, &self.setup().1))
    }

    /// Starts hosting on `port` (0 picks one) as `handle`: a new session with a new code.
    pub async fn start(self: &Arc<Self>, port: u16, handle: &str) -> Result<u16, String> {
        let handle = handle.trim();
        if !is_handle(handle) {
            return Err(
                "The handle must be 1 to 40 characters of plain text (no < or >)".to_owned(),
            );
        }
        if self.is_hosting() {
            return Err("Already hosting: stop first".to_owned());
        }
        let listener = tls::Listener::bind(SocketAddr::new(self.bind, port))
            .await
            .map_err(|e| format!("Cannot listen on port {port}: {e}"))?;
        let bound = listener.local_addr().map_or(port, |a| a.port());
        let code = new_join_code().map_err(|e| e.to_string())?;
        let mut id = [0u8; 4];
        getrandom::fill(&mut id).map_err(|e| e.to_string())?;
        let event = EventInfo {
            id: format!("p2p-{}", u32::from_le_bytes(id)),
            name: event_name(handle),
            state: EventState::Open,
        };
        let me = self.key().to_string();
        let tree = EventStructure::p2p_default(event.clone(), &me, handle);
        *lock(&self.session) = Some(Session {
            event,
            code,
            tree,
            handles: HashMap::from([(me.clone(), handle.to_owned())]),
            keys: HashSet::from([me]),
            conns: HashMap::new(),
            failures: Failures::default(),
            cards: 0,
        });
        {
            let mut setup = lock(&self.setup);
            setup.0 = if port == 0 { setup.0 } else { bound };
            setup.1 = handle.to_owned();
        }
        self.save();
        *lock(&self.problem) = None;
        let (stop, stopped) = watch::channel(false);
        let host = Arc::clone(self);
        let task = tokio::spawn(async move {
            let mut stopping = stopped.clone();
            loop {
                let accepted = tokio::select! {
                    accepted = listener.accept() => accepted,
                    _ = stopping.changed() => break,
                };
                match accepted {
                    Ok((tcp, from)) => {
                        if host.pending.load(Ordering::SeqCst) >= host.limits.max_pending {
                            tracing::info!("[host] a connection refused: too many not joined");
                            continue;
                        }
                        host.pending.fetch_add(1, Ordering::SeqCst);
                        let host = Arc::clone(&host);
                        let acceptor = listener.acceptor();
                        let stopped = stopped.clone();
                        tokio::spawn(async move {
                            let pending = Pending(&host.pending);
                            if let Err(e) = host
                                .connection(&acceptor, tcp, from, pending, stopped)
                                .await
                            {
                                tracing::debug!("[host] a connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => tracing::warn!("[host] accept: {e}"),
                }
            }
        });
        *lock(&self.running) = Some(Running {
            stop,
            task,
            port: bound,
        });
        tracing::info!("[host] hosting on port {bound}");
        self.publish();
        Ok(bound)
    }

    /// Stops hosting: every member is told and dropped, and the session ends.
    pub async fn stop(&self) {
        let running = lock(&self.running).take();
        if let Some(s) = lock(&self.session).take() {
            let bye = Message::Bye {
                reason: "the host stopped hosting".to_owned(),
            };
            for c in s.conns.values() {
                let _ = c.tx.try_send(Out::Close(bye.clone()));
            }
            tracing::info!(
                "[host] stopped: {} members, {} cards relayed",
                s.tree.members().len(),
                s.cards
            );
        }
        if let Some(r) = running {
            let _ = r.stop.send(true);
            let _ = r.task.await;
        }
        self.publish();
    }

    /// A new code: the old one is refused at once; members already in stay.
    pub fn rotate(&self) -> Result<String, String> {
        let code = new_join_code().map_err(|e| e.to_string())?;
        lock(&self.session)
            .as_mut()
            .ok_or("Not hosting")?
            .code
            .clone_from(&code);
        tracing::info!("[host] a new code");
        self.publish();
        Ok(code)
    }

    fn other_member(&self, s: &Session, member: &str) -> Result<(), String> {
        if member == self.key().to_string() {
            return Err("That is you, the host".to_owned());
        }
        if !s.tree.is_member(member) {
            return Err("No such member".to_owned());
        }
        Ok(())
    }

    /// Kick, for a bad player: drops them, bans the key and replaces the code, so the invite
    /// they had is dead. The members still connected stay.
    pub fn kick(&self, member: &str) -> Result<(), String> {
        {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or("Not hosting")?;
            self.other_member(s, member)?;
            let key: PublicKey = member.parse().map_err(|_| "No such member")?;
            let handle = s.handles.get(member).cloned().unwrap_or_default();
            self.bans.add(key, &handle, now_ms())?;
            s.code = new_join_code().map_err(|e| e.to_string())?;
            s.forget(
                member,
                refused("The host kicked you from the fleet", RefusalCode::Banned),
            );
            s.changed();
            tracing::info!(
                "[host] a member kicked and banned; a new code ({} members)",
                s.tree.members().len()
            );
        }
        self.publish();
        Ok(())
    }

    /// Remove, for a bad connection: drops them; they may join again with the current code.
    pub fn remove(&self, member: &str) -> Result<(), String> {
        {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or("Not hosting")?;
            self.other_member(s, member)?;
            s.forget(
                member,
                refused(
                    "The host removed you from the fleet: join again with the current invite",
                    RefusalCode::Kicked,
                ),
            );
            s.changed();
            tracing::info!(
                "[host] a member removed ({} members)",
                s.tree.members().len()
            );
        }
        self.publish();
        Ok(())
    }

    pub fn bans(&self) -> Vec<Ban> {
        self.bans.get()
    }

    pub fn lift(&self, key: &str) -> Result<(), String> {
        let key: PublicKey = key.parse().map_err(|_| "No such ban")?;
        if !self.bans.lift(&key)? {
            return Err("No such ban".to_owned());
        }
        self.publish();
        Ok(())
    }

    pub fn view(&self) -> HostView {
        let (port, handle, public) = self.setup();
        let lan = crate::relay::local_addresses().into_iter().next();
        let me = self.key();
        let session = lock(&self.session);
        let mut view = HostView {
            hosting: session.is_some(),
            port: self.port().unwrap_or(port),
            handle: handle.clone(),
            fingerprint: me.fingerprint(),
            lan_address: lan.clone(),
            public_address: public.clone(),
            max_members: self.limits.max_members,
            problem: lock(&self.problem).clone(),
            bans: self
                .bans
                .get()
                .into_iter()
                .map(|b| BanView {
                    fingerprint: b.key.fingerprint(),
                    key: b.key.to_string(),
                    handle: b.handle,
                    at: b.at,
                })
                .collect(),
            ..HostView::default()
        };
        if let Some(s) = session.as_ref() {
            let port = view.port;
            let host = lan.as_deref().unwrap_or("127.0.0.1");
            view.invite = Some(invite(host, port, &me, &s.code, &handle));
            view.public_invite = public
                .as_deref()
                .map(|p| invite(p, port, &me, &s.code, &handle));
            view.code = Some(s.code.clone());
            let mine = me.to_string();
            view.members = s
                .tree
                .members()
                .into_iter()
                .map(|m| HostMember {
                    handle: s.handles.get(&m).cloned().unwrap_or_default(),
                    fingerprint: m
                        .parse::<PublicKey>()
                        .map(|k| k.fingerprint())
                        .unwrap_or_default(),
                    connected: s.connected(&m),
                    host: m == mine,
                    member: m,
                })
                .collect();
            view.members.sort_by_key(|m| !m.host);
        }
        view
    }

    fn publish(&self) {
        self.hub.set_host(Some(self.view()));
    }

    /// The members of the session now, the host included.
    pub fn member_count(&self) -> usize {
        lock(&self.session)
            .as_ref()
            .map_or(0, |s| s.tree.members().len())
    }

    /// One connection: TLS, the handshake within [`Limits::handshake`], `join` within
    /// [`Limits::join`], then cards both ways until it ends.
    async fn connection(
        self: &Arc<Self>,
        acceptor: &tokio_rustls::TlsAcceptor,
        tcp: TcpStream,
        from: SocketAddr,
        pending: Pending<'_>,
        stopped: watch::Receiver<bool>,
    ) -> io::Result<()> {
        let started = Instant::now();
        let shake_by = started + self.limits.handshake;
        let join_by = started + self.limits.join;
        let stream = timeout_at(shake_by, tls::handshake(acceptor, tcp))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no TLS handshake"))??;
        let binding = tls_binding(stream.get_ref().1)?;
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        let Message::ClientHello { hello, key, nonce } =
            read_by(&mut reader, &mut buf, shake_by).await?
        else {
            return Ok(());
        };
        let ours = Hello::new(
            concat!("hardpoint ", env!("CARGO_PKG_VERSION")),
            &[
                capability::CARDS,
                capability::SUBSCRIBE,
                capability::STRUCTURE,
                capability::JOIN,
                capability::TLS_BINDING,
            ],
        );
        if let Err(e) = ours.check(&hello) {
            return write_message(
                &mut writer,
                &Message::Refused {
                    reason: e.to_string(),
                    code: None,
                },
            )
            .await;
        }
        if !hello.has(capability::TLS_BINDING) {
            let reason = "This host needs the TLS binding: update Hardpoint".to_owned();
            return write_message(&mut writer, &Message::Refused { reason, code: None }).await;
        }
        let (challenge, proof) = lock(&self.issuer)
            .issue(&self.identity, &key, &nonce, Some(&binding), now_ms())
            .map_err(|e| io::Error::other(e.to_string()))?;
        let server = self.setup().1;
        write_message(
            &mut writer,
            &Message::ServerHello {
                hello: ours.clone(),
                server,
                key: self.key(),
                challenge,
                proof,
            },
        )
        .await?;
        let Message::ClientProof { proof } = read_by(&mut reader, &mut buf, shake_by).await? else {
            return Ok(());
        };
        let accepted = lock(&self.issuer).accept(
            &self.key(),
            &key,
            &nonce,
            &challenge,
            &proof,
            Some(&binding),
            now_ms(),
        );
        if let Err(e) = accepted {
            return write_message(
                &mut writer,
                &Message::Refused {
                    reason: e.to_string(),
                    code: None,
                },
            )
            .await;
        }
        write_message(
            &mut writer,
            &Message::Welcome {
                event: String::new(),
                capabilities: ours.common(&hello),
            },
        )
        .await?;
        let Message::Join { code, handle } = read_by(&mut reader, &mut buf, join_by).await? else {
            return Ok(());
        };
        let member = key.to_string();
        let (tx, mut rx) = mpsc::channel(OUTBOX);
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let admitted = self.admit(&key, code.as_deref(), &handle, from.ip(), conn, tx);
        let joined = match admitted {
            Ok(joined) => joined,
            Err(refusal) => return write_message(&mut writer, &refusal).await,
        };
        drop(pending);
        self.publish();
        let end = self
            .online(
                &mut reader,
                &mut writer,
                &mut buf,
                &mut rx,
                joined,
                &member,
                conn,
                stopped,
            )
            .await;
        let left = {
            let mut guard = lock(&self.session);
            match guard.as_mut() {
                Some(s) if s.conns.contains_key(&conn) => {
                    s.conns.remove(&conn);
                    if matches!(end, Ok(true)) && member != self.key().to_string() {
                        s.forget(
                            &member,
                            Message::Bye {
                                reason: String::new(),
                            },
                        );
                    }
                    s.changed();
                    true
                }
                _ => false,
            }
        };
        if left {
            self.publish();
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), writer.shutdown()).await;
        end.map(|_| ())
    }

    /// The host's decision on a `join` (PROTOCOL 4.4): the `joined` to send, or the refusal.
    #[allow(clippy::result_large_err)]
    fn admit(
        &self,
        key: &PublicKey,
        code: Option<&str>,
        handle: &str,
        ip: IpAddr,
        conn: u64,
        tx: mpsc::Sender<Out>,
    ) -> Result<Message, Message> {
        let mut guard = lock(&self.session);
        let Some(s) = guard.as_mut() else {
            return Err(refused(
                "The host stopped hosting",
                RefusalCode::SessionEnded,
            ));
        };
        let member = key.to_string();
        let own = *key == self.key() && ip.is_loopback();
        if !own && self.bans.is_banned(key) {
            return Err(refused(
                "The host banned this fleet key",
                RefusalCode::Banned,
            ));
        }
        let known = s.keys.contains(&member);
        if !own && !known {
            let (max, window) = (self.limits.max_failures, self.limits.failure_window);
            if s.failures.blocked(ip, max, window) {
                return Err(refused(
                    "Too many wrong codes from this address: try again later",
                    RefusalCode::BadCode,
                ));
            }
            match code {
                None => {
                    return Err(refused(
                        "This host's session does not know this Hardpoint",
                        RefusalCode::SessionEnded,
                    ));
                }
                Some(c) if !same_code(c, &s.code) => {
                    s.failures.fail(ip, max, window);
                    return Err(refused("That join code is not valid", RefusalCode::BadCode));
                }
                Some(_) => {}
            }
            if !s.tree.is_member(&member) && s.tree.members().len() >= self.limits.max_members {
                return Err(refused(
                    &format!(
                        "The fleet has its most members ({})",
                        self.limits.max_members
                    ),
                    RefusalCode::Full,
                ));
            }
        }
        let handle = if own {
            s.handles
                .get(&member)
                .cloned()
                .unwrap_or_else(|| clean_handle(handle))
        } else {
            s.unique_handle(&clean_handle(handle), &member)
        };
        s.handles.insert(member.clone(), handle.clone());
        s.keys.insert(member.clone());
        s.tree.p2p_seat(&member, &handle);
        // The same key connecting again replaces its older connection.
        let older: Vec<u64> = s
            .conns
            .iter()
            .filter(|(_, c)| c.member == member)
            .map(|(id, _)| *id)
            .collect();
        for id in older {
            if let Some(c) = s.conns.remove(&id) {
                let _ = c.tx.try_send(Out::Close(Message::Bye {
                    reason: "connected again".to_owned(),
                }));
            }
        }
        let joined = Message::Joined {
            event: s.event.id.clone(),
            name: s.event.name.clone(),
            member: Some(member.clone()),
            handle,
            unit: None,
            grants: vec![],
            card_every_ms: Some(CARD_EVERY_MS),
        };
        s.conns.insert(
            conn,
            Conn {
                member,
                tx,
                subscription: None,
                asked: None,
            },
        );
        tracing::info!(
            "[host] a member joined ({} members, {} connected)",
            s.tree.members().len(),
            s.conns.len()
        );
        Ok(joined)
    }

    /// Joined: `joined`, the roster, then cards in and out. `Ok(true)` when the member left
    /// the event (`bye` with `leave`).
    #[allow(clippy::too_many_arguments)]
    async fn online<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        buf: &mut Vec<u8>,
        rx: &mut mpsc::Receiver<Out>,
        joined: Message,
        member: &str,
        conn: u64,
        mut stopped: watch::Receiver<bool>,
    ) -> io::Result<bool>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        write_message(writer, &joined).await?;
        if let Some(s) = lock(&self.session).as_mut() {
            s.changed();
        }
        loop {
            tokio::select! {
                _ = stopped.changed() => {
                    while let Ok(out) = rx.try_recv() {
                        if let Out::Close(m) = out {
                            let _ = write_message(writer, &m).await;
                            break;
                        }
                    }
                    return Ok(false);
                }
                out = rx.recv() => match out {
                    Some(Out::Send(m)) => write_message(writer, &m).await?,
                    Some(Out::Close(m)) => {
                        write_message(writer, &m).await?;
                        return Ok(false);
                    }
                    None => return Ok(false),
                },
                read = read_message(reader, buf) => {
                    let message = match read? {
                        None => return Ok(false),
                        Some(Err(e)) => return Err(io::Error::other(e)),
                        Some(Ok(m)) => m,
                    };
                    match message {
                        Message::Card { card } => {
                            let ok = card.member == member
                                && card.ship_unit().is_none()
                                && card.check().is_ok();
                            if ok && let Some(s) = lock(&self.session).as_mut() && s.conns.contains_key(&conn) {
                                s.relay(member, card);
                            }
                        }
                        Message::Subscribe { subscription } => {
                            if let Some(s) = lock(&self.session).as_mut()
                                && let Some(c) = s.conns.get_mut(&conn)
                            {
                                c.subscription = Some(subscription);
                                s.ask();
                            }
                        }
                        Message::Bye { reason } => return Ok(reason == BYE_LEAVE),
                        Message::Change { .. } => {
                            let reason = "A peer-to-peer host takes no structure changes".to_owned();
                            write_message(writer, &Message::ChangeRefused { reason }).await?;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// For tests: the flat tree in memory.
    pub fn tree(&self) -> Option<EventStructure> {
        lock(&self.session).as_ref().map(|s| s.tree.clone())
    }

    /// For tests and the panel: the handles by member id.
    pub fn handles(&self) -> HashMap<String, String> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.handles.clone())
            .unwrap_or_default()
    }
}

/// Every interface, as the companion relay binds.
pub const BIND_ALL: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invite_carries_the_host_name_as_a_label() {
        let key = Identity::from_seed([4; 32]).public();
        let text = invite(
            "203.0.113.5",
            47522,
            &key,
            "ABCD-EFGH-JKLM-NPQR",
            "Vega & Co",
        );
        let parsed: fleet_proto::Invite = text.parse().unwrap();
        assert_eq!(parsed.port, 47522);
        assert_eq!(parsed.code.as_deref(), Some("ABCD-EFGH-JKLM-NPQR"));
        assert_eq!(host_name_of(&text).as_deref(), Some("Vega & Co"));
        let v6 = invite("fe80::1", 1, &key, "c", "V");
        assert!(v6.starts_with("cic://[fe80::1]:1?"), "{v6}");
        assert!(v6.parse::<fleet_proto::Invite>().is_ok());
        assert_eq!(host_name_of("cic://h:1?key=k&code=c&name=%3Cb%3E"), None);
    }

    #[test]
    fn handles_clash_with_a_suffix_ignoring_case() {
        let event = EventInfo {
            id: "p2p-1".to_owned(),
            name: "F".to_owned(),
            state: EventState::Open,
        };
        let mut s = Session {
            tree: EventStructure::p2p_default(event.clone(), "h", "Vega"),
            event,
            code: "c".to_owned(),
            handles: HashMap::from([("h".to_owned(), "Vega".to_owned())]),
            keys: HashSet::new(),
            conns: HashMap::new(),
            failures: Failures::default(),
            cards: 0,
        };
        assert_eq!(s.unique_handle("vega", "a"), "vega-2");
        s.handles.insert("a".to_owned(), "vega-2".to_owned());
        assert_eq!(s.unique_handle("VEGA", "b"), "VEGA-3");
        assert_eq!(s.unique_handle("Vega", "h"), "Vega");
        let long = "x".repeat(40);
        s.handles.insert("c".to_owned(), long.clone());
        let suffixed = s.unique_handle(&long, "d");
        assert_eq!(suffixed.len(), 40);
        assert!(suffixed.ends_with("-2"));
    }

    #[tokio::test(start_paused = true)]
    async fn failures_block_an_address_within_the_window_only() {
        let mut f = Failures::default();
        let (a, b): (IpAddr, IpAddr) = ([10, 0, 0, 1].into(), [10, 0, 0, 2].into());
        let window = Duration::from_secs(600);
        for _ in 0..10 {
            assert!(!f.blocked(a, 10, window));
            f.fail(a, 10, window);
        }
        assert!(f.blocked(a, 10, window));
        assert!(!f.blocked(b, 10, window));
        tokio::time::advance(Duration::from_secs(601)).await;
        assert!(!f.blocked(a, 10, window));
    }
}
