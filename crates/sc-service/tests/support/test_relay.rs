//! A small fleet relay for tests and screenshots: TLS with a self-signed certificate, the
//! identity handshake, `join` with codes (peer to peer) or `connect` with connection tokens and
//! the event choice (hosted), an event structure, and card relaying between its connections. It
//! also sends cards for made-up members when asked. Not the CIC relay; just enough of the
//! protocol to drive the Hardpoint client against a real socket.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleet_proto::hello::capability;
use fleet_proto::identity::{ChallengeIssuer, TlsBinding};
use fleet_proto::message::MAX_FRAME_BYTES;
use fleet_proto::structure::EventStructure;
use fleet_proto::{
    Card, EventChoice, Hello, Identity, Message, PublicKey, RefusalCode, RosterEntry,
    StructureChange,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, split};
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

pub type StructureFor = dyn Fn(&PublicKey) -> Option<EventStructure> + Send + Sync;
pub type Ambient = dyn Fn() -> Vec<Card> + Send + Sync;

/// A connection token the hosted relay knows: who it binds, and the event of a per-event one.
#[derive(Clone)]
pub struct Token {
    pub token: String,
    pub user: String,
    pub event: Option<String>,
}

/// The hosted mode: accounts by user id, tokens, and each open event's full structure.
#[derive(Default)]
pub struct Hosted {
    pub tokens: Vec<Token>,
    /// Handles by user id.
    pub users: Vec<(String, String)>,
    pub events: Vec<EventStructure>,
    /// Keys already bound, as after a token was spent in an earlier run.
    pub bound: Vec<(PublicKey, String)>,
}

pub struct Options {
    /// Offer `join`; without it the relay acts as one that puts everyone in one event.
    pub join: bool,
    /// Offer `tls-binding` and bind the proofs to the TLS session.
    pub binding: bool,
    pub codes: Vec<String>,
    pub card_every_ms: u64,
    pub structure: Box<StructureFor>,
    /// Cards for made-up members, sent to every member twice a second.
    pub ambient: Box<Ambient>,
    /// Offer `connect` instead of `join`, as a CIC relay does.
    pub hosted: Option<Hosted>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            join: true,
            binding: true,
            codes: vec!["code-1".to_owned()],
            card_every_ms: 200,
            structure: Box::new(|_| None),
            ambient: Box::new(Vec::new),
            hosted: None,
        }
    }
}

#[derive(Default)]
pub struct Seen {
    /// The code of every join, in order (`None` for a rejoin by key).
    pub joins: Vec<Option<String>>,
    pub handles: Vec<String>,
    pub cards: Vec<Card>,
    pub byes: Vec<String>,
    pub handshakes: usize,
    /// Hosted: every `connect` (token, event), every `choose` and every change applied.
    pub connects: Vec<(Option<String>, Option<String>)>,
    pub chooses: Vec<String>,
    pub changes: Vec<StructureChange>,
}

struct State {
    identity: Identity,
    issuer: Mutex<ChallengeIssuer>,
    options: Options,
    known: Mutex<HashMap<PublicKey, String>>,
    seen: Mutex<Seen>,
    cards: broadcast::Sender<(u64, Card)>,
    kill: Notify,
    /// Hosted: keys bound to user ids, tokens spent, the live structures, and a tick on change.
    bound: Mutex<HashMap<PublicKey, String>>,
    spent: Mutex<HashSet<String>>,
    events: Mutex<Vec<EventStructure>>,
    changed: broadcast::Sender<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub struct TestRelay {
    pub addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn tls() -> TlsAcceptor {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = made.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(made.signing_key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

impl TestRelay {
    pub async fn start(options: Options) -> Self {
        Self::start_on("127.0.0.1:0", Identity::generate().unwrap(), options).await
    }

    pub async fn start_on(bind: &str, identity: Identity, options: Options) -> Self {
        let listener = TcpListener::bind(bind).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State {
            identity,
            issuer: Mutex::default(),
            known: Mutex::default(),
            seen: Mutex::default(),
            cards: broadcast::channel(256).0,
            kill: Notify::new(),
            bound: Mutex::new(
                options
                    .hosted
                    .iter()
                    .flat_map(|h| h.bound.iter().cloned())
                    .collect(),
            ),
            spent: Mutex::default(),
            events: Mutex::new(
                options
                    .hosted
                    .as_ref()
                    .map(|h| h.events.clone())
                    .unwrap_or_default(),
            ),
            changed: broadcast::channel(16).0,
            options,
        });
        let acceptor = tls();
        let serving = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let mut next = 1u64;
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let (state, acceptor, conn) = (Arc::clone(&serving), acceptor.clone(), next);
                next += 1;
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(tcp).await {
                        let binding = sc_service::fleet::tls_binding(stream.get_ref().1).ok();
                        let _ = state.handle(stream, binding, conn).await;
                    }
                });
            }
        });
        Self { addr, state, task }
    }

    pub fn key(&self) -> PublicKey {
        self.state.identity.public()
    }

    pub fn invite(&self, code: &str) -> String {
        format!(
            "cic://127.0.0.1:{}?key={}&code={code}",
            self.addr.port(),
            self.key()
        )
    }

    pub fn connect_invite(&self, token: &str, event: Option<&str>) -> String {
        let event = event.map(|e| format!("&event={e}")).unwrap_or_default();
        format!(
            "cic://127.0.0.1:{}?key={}&connect={token}{event}",
            self.addr.port(),
            self.key()
        )
    }

    /// Hosted: the structure of an event now.
    pub fn structure(&self, event: &str) -> Option<EventStructure> {
        lock(&self.state.events)
            .iter()
            .find(|s| s.event.id == event)
            .cloned()
    }

    /// Hosted: changes an event as the portal would, and tells every connection.
    pub fn portal_change(&self, event: &str, change: &StructureChange) {
        if let Some(s) = lock(&self.state.events)
            .iter_mut()
            .find(|s| s.event.id == event)
        {
            s.apply(change).unwrap();
        }
        let _ = self.state.changed.send(());
    }

    pub fn seen(&self) -> MutexGuard<'_, Seen> {
        lock(&self.state.seen)
    }

    /// Drops every open connection, as a relay restart would.
    pub fn drop_connections(&self) {
        self.state.kill.notify_waiters();
    }
}

/// One message; cancel-safe, since a part line stays in `line` for the next call.
async fn read<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<Option<Message>> {
    let n = (&mut *reader)
        .take(MAX_FRAME_BYTES as u64 + 2)
        .read_until(b'\n', line)
        .await?;
    if n == 0 && line.is_empty() {
        return Ok(None);
    }
    let message = Message::decode(line).map_err(io::Error::other);
    line.clear();
    message.map(Some)
}

async fn write<W: AsyncWrite + Unpin>(w: &mut W, m: &Message) -> io::Result<()> {
    w.write_all(&m.encode()).await?;
    w.flush().await
}

impl State {
    async fn handle<S>(
        self: Arc<Self>,
        stream: S,
        binding: Option<TlsBinding>,
        conn: u64,
    ) -> io::Result<()>
    where
        S: tokio::io::AsyncRead + AsyncWrite + Unpin,
    {
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        let Some(Message::ClientHello { hello, key, nonce }) = read(&mut reader, &mut line).await?
        else {
            return Ok(());
        };
        let mut caps = vec![capability::CARDS];
        if self.options.hosted.is_some() {
            caps.extend([capability::STRUCTURE, capability::CONNECT]);
        } else if self.options.join {
            caps.extend([capability::STRUCTURE, capability::JOIN]);
        }
        if self.options.binding {
            caps.push(capability::TLS_BINDING);
        }
        let ours = Hello::new("test-relay", &caps);
        let binding = binding.filter(|_| {
            ours.common(&hello)
                .iter()
                .any(|c| c == capability::TLS_BINDING)
        });
        if let Err(e) = ours.check(&hello) {
            return write(
                &mut writer,
                &Message::Refused {
                    reason: e.to_string(),
                    code: None,
                },
            )
            .await;
        }
        let (challenge, proof) = lock(&self.issuer)
            .issue(&self.identity, &key, &nonce, binding.as_ref(), now_ms())
            .unwrap();
        write(
            &mut writer,
            &Message::ServerHello {
                hello: ours.clone(),
                server: "Test relay".to_owned(),
                key: self.identity.public(),
                challenge,
                proof,
            },
        )
        .await?;
        let Some(Message::ClientProof { proof }) = read(&mut reader, &mut line).await? else {
            return Ok(());
        };
        let accepted = lock(&self.issuer).accept(
            &self.identity.public(),
            &key,
            &nonce,
            &challenge,
            &proof,
            binding.as_ref(),
            now_ms(),
        );
        if let Err(e) = accepted {
            return write(
                &mut writer,
                &Message::Refused {
                    reason: e.to_string(),
                    code: None,
                },
            )
            .await;
        }
        lock(&self.seen).handshakes += 1;
        write(
            &mut writer,
            &Message::Welcome {
                event: "test".to_owned(),
                capabilities: ours.common(&hello),
            },
        )
        .await?;
        if self.options.hosted.is_some() {
            return self
                .hosted(&mut reader, &mut writer, &mut line, key, conn)
                .await;
        }
        let structure = (self.options.structure)(&key);
        if self.options.join {
            let Some(Message::Join { code, handle }) = read(&mut reader, &mut line).await? else {
                return Ok(());
            };
            {
                let mut seen = lock(&self.seen);
                seen.joins.push(code.clone());
                seen.handles.push(handle.clone());
            }
            let known = lock(&self.known).contains_key(&key);
            let good = code
                .as_ref()
                .is_some_and(|c| self.options.codes.contains(c));
            if !known && !good {
                let reason = "That join code is not valid".to_owned();
                return write(&mut writer, &Message::Refused { reason, code: None }).await;
            }
            lock(&self.known).insert(key, handle.clone());
            let grants: Vec<_> = structure
                .as_ref()
                .map(|s| s.grants_of(&key.to_string()).into_iter().collect())
                .unwrap_or_default();
            let (event, name) = structure.as_ref().map_or_else(
                || ("test".to_owned(), "Test event".to_owned()),
                |s| (s.event.id.clone(), s.event.name.clone()),
            );
            write(
                &mut writer,
                &Message::Joined {
                    event,
                    name,
                    member: Some(key.to_string()),
                    handle,
                    unit: structure
                        .as_ref()
                        .and_then(|s| s.seat_of(&key.to_string()))
                        .map(|(u, _)| u.id.clone()),
                    grants: vec![],
                    card_every_ms: Some(self.options.card_every_ms),
                },
            )
            .await?;
            write(
                &mut writer,
                &Message::Roster {
                    you: grants,
                    members: vec![],
                },
            )
            .await?;
            if let Some(structure) = structure {
                write(&mut writer, &Message::Structure { structure }).await?;
            }
        }
        let mut relayed = self.cards.subscribe();
        let mut ambient = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                () = self.kill.notified() => return Ok(()),
                m = read(&mut reader, &mut line) => match m? {
                    None => return Ok(()),
                    Some(Message::Card { card }) => {
                        lock(&self.seen).cards.push(card.clone());
                        let _ = self.cards.send((conn, card));
                    }
                    Some(Message::Bye { reason }) => {
                        lock(&self.seen).byes.push(reason.clone());
                        if reason == fleet_proto::message::BYE_LEAVE {
                            lock(&self.known).remove(&key);
                        }
                        return Ok(());
                    }
                    Some(_) => {}
                },
                r = relayed.recv() => {
                    if let Ok((from, card)) = r && from != conn {
                        write(&mut writer, &Message::Card { card }).await?;
                    }
                }
                _ = ambient.tick() => {
                    for card in (self.options.ambient)() {
                        write(&mut writer, &Message::Card { card }).await?;
                    }
                }
            }
        }
    }
}

fn refusal(reason: &str, code: RefusalCode) -> Message {
    Message::Refused {
        reason: reason.to_owned(),
        code: Some(code),
    }
}

impl State {
    fn handle_of(&self, user: &str) -> String {
        self.options
            .hosted
            .iter()
            .flat_map(|h| h.users.iter())
            .find(|(u, _)| u == user)
            .map_or_else(|| user.to_owned(), |(_, h)| h.clone())
    }

    fn open_events(&self, user: &str) -> Vec<EventChoice> {
        lock(&self.events)
            .iter()
            .filter(|s| s.is_member(user))
            .map(|s| EventChoice {
                id: s.event.id.clone(),
                name: s.event.name.clone(),
                unit: s.seat_of(user).map(|(u, _)| u.id.clone()),
                role: s.top_role(user),
            })
            .collect()
    }

    fn roster(&self, s: &EventStructure, user: &str) -> Message {
        let bound = lock(&self.bound).clone();
        Message::Roster {
            you: s.grants_of(user).into_iter().collect(),
            members: s
                .roster_for(user)
                .into_iter()
                .map(|m| RosterEntry {
                    handle: Some(self.handle_of(&m)),
                    role: s.top_role(&m),
                    unit: s.seat_of(&m).map(|(u, _)| u.id.clone()),
                    key: bound.iter().find(|(_, u)| **u == m).map(|(k, _)| *k),
                    member: m,
                })
                .collect(),
        }
    }

    /// `connect`, the event choice, then cards (rewritten to the user id and filtered by the
    /// structure), structure changes, and a new structure and roster whenever one changes.
    async fn hosted<R, W>(
        self: &Arc<Self>,
        reader: &mut R,
        writer: &mut W,
        line: &mut Vec<u8>,
        key: PublicKey,
        conn: u64,
    ) -> io::Result<()>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let Some(Message::Connect { token, event }) = read(reader, line).await? else {
            return Ok(());
        };
        lock(&self.seen)
            .connects
            .push((token.clone(), event.clone()));
        let hosted = self.options.hosted.as_ref().unwrap();
        let mut fixed = None;
        let user = match &token {
            Some(t) => {
                let Some(found) = hosted.tokens.iter().find(|x| &x.token == t) else {
                    let m = refusal(
                        "No account has that connection code",
                        RefusalCode::TokenUnknown,
                    );
                    return write(writer, &m).await;
                };
                let fresh = lock(&self.spent).insert(t.clone());
                if !fresh {
                    let m = refusal(
                        "That connection code was used already",
                        RefusalCode::TokenSpent,
                    );
                    return write(writer, &m).await;
                }
                lock(&self.bound).insert(key, found.user.clone());
                fixed.clone_from(&found.event);
                found.user.clone()
            }
            None => {
                let bound = lock(&self.bound).get(&key).cloned();
                let Some(u) = bound else {
                    let m = refusal(
                        "This Hardpoint is not connected to an account",
                        RefusalCode::Unbound,
                    );
                    return write(writer, &m).await;
                };
                u
            }
        };
        let open = self.open_events(&user);
        let mut chosen = match (&fixed, &event) {
            (Some(f), Some(e)) if f != e => {
                return write(
                    writer,
                    &refusal("That code is for another event", RefusalCode::WrongEvent),
                )
                .await;
            }
            (Some(f), _) => Some(f.clone()),
            (None, Some(e)) if open.iter().any(|o| &o.id == e) => Some(e.clone()),
            _ if open.len() == 1 => Some(open[0].id.clone()),
            _ => None,
        };
        if chosen.is_none() {
            write(
                writer,
                &Message::Events {
                    events: open.clone(),
                },
            )
            .await?;
        }
        while chosen.is_none() {
            match read(reader, line).await? {
                Some(Message::Choose { event }) => {
                    lock(&self.seen).chooses.push(event.clone());
                    if !open.iter().any(|o| o.id == event) {
                        return write(
                            writer,
                            &refusal("That event is not open to you", RefusalCode::NoSuchEvent),
                        )
                        .await;
                    }
                    chosen = Some(event);
                }
                None => return Ok(()),
                Some(_) => {}
            }
        }
        let event = chosen.unwrap();
        let view = |state: &State| {
            lock(&state.events)
                .iter()
                .find(|s| s.event.id == event)
                .cloned()
        };
        let Some(s) = view(self) else {
            return Ok(());
        };
        write(
            writer,
            &Message::Joined {
                event: s.event.id.clone(),
                name: s.event.name.clone(),
                member: Some(user.clone()),
                handle: self.handle_of(&user),
                unit: s.seat_of(&user).map(|(u, _)| u.id.clone()),
                grants: vec![],
                card_every_ms: Some(self.options.card_every_ms),
            },
        )
        .await?;
        let push = |s: &EventStructure| {
            [
                self.roster(s, &user),
                Message::Structure {
                    structure: s.view_for(&user),
                },
            ]
        };
        for m in push(&s) {
            write(writer, &m).await?;
        }
        let sees = |s: &EventStructure, card: &Card| match card.ship_unit() {
            Some(unit) => s.visible_units(&user).contains(unit),
            None => s.can_see_card(&user, &card.member),
        };
        let mut relayed = self.cards.subscribe();
        let mut changed = self.changed.subscribe();
        let mut ambient = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                () = self.kill.notified() => return Ok(()),
                m = read(reader, line) => match m? {
                    None => return Ok(()),
                    Some(Message::Card { mut card }) => {
                        lock(&self.seen).cards.push(card.clone());
                        if card.member == key.to_string() {
                            card.member.clone_from(&user);
                            card.handle = self.handle_of(&user);
                            let _ = self.cards.send((conn, card));
                        }
                    }
                    Some(Message::Change { change }) => {
                        let applied = {
                            let mut events = lock(&self.events);
                            let s = events.iter_mut().find(|s| s.event.id == event).unwrap();
                            if s.may_change(&user, &change) {
                                s.apply(&change)
                            } else {
                                Err("Your role does not allow that change".to_owned())
                            }
                        };
                        match applied {
                            Ok(()) => {
                                lock(&self.seen).changes.push(change);
                                let _ = self.changed.send(());
                            }
                            Err(reason) => write(writer, &Message::ChangeRefused { reason }).await?,
                        }
                    }
                    Some(Message::Bye { reason }) => {
                        lock(&self.seen).byes.push(reason);
                        return Ok(());
                    }
                    Some(_) => {}
                },
                _ = changed.recv() => {
                    if let Some(s) = view(self) {
                        for m in push(&s) {
                            write(writer, &m).await?;
                        }
                    }
                }
                r = relayed.recv() => {
                    if let Ok((from, card)) = r && from != conn
                        && view(self).is_some_and(|s| sees(&s, &card))
                    {
                        write(writer, &Message::Card { card }).await?;
                    }
                }
                _ = ambient.tick() => {
                    let Some(s) = view(self) else { continue };
                    for card in (self.options.ambient)() {
                        if sees(&s, &card) {
                            write(writer, &Message::Card { card }).await?;
                        }
                    }
                }
            }
        }
    }
}
