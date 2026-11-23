//! A hosted member: what Hardpoint does against a CIC relay (PROTOCOL.md 4.3, fleet-proto
//! 0.4.0). The app's `FleetClient` still joins by code only, so this side is written here,
//! from the same parts: the TLS connector, the identity handshake bound to the TLS session, the
//! card builder and the send consent.
//!
//! After `welcome`: `connect` with the token, `events` answered by `choose` when the relay
//! cannot pick, then `joined` and cards at the relay's rate. The token is spent on the first
//! answer; from then on (a drop, a restart) the member connects with no token and the event it
//! was in, by its bound key. A refusal is not retried.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, split,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use fleet_proto::card::card_fields;
use fleet_proto::consent::filter_card;
use fleet_proto::hello::capability;
use fleet_proto::identity::ClientHandshake;
use fleet_proto::message::{CARD_EVERY_MS, MAX_FRAME_BYTES, MIN_CARD_EVERY_MS};
use fleet_proto::{
    Card, Hello, Identity, IdentityError, Life, Message, Nonce, PublicKey, RefusalCode,
    Subscription, TlsBinding,
};
use sc_core::ControlCore;
use sc_service::fleet::card::{LifeTracker, Sender, build_card, health_fraction};
use sc_service::fleet::client::CONNECT_TIMEOUT;
use sc_service::fleet::{
    Backoff, Connected, Connector, ConsentStore, FleetHub, FleetViewStore, LinkState,
};

use crate::invite::HostedInvite;

const SOFTWARE: &str = concat!("fleet-sim ", env!("CARGO_PKG_VERSION"));
/// As Hardpoint: both `join` and `connect`, and nothing without `tls-binding`.
const CAPABILITIES: &[&str] = &[
    capability::CARDS,
    capability::SUBSCRIBE,
    capability::STRUCTURE,
    capability::JOIN,
    capability::CONNECT,
    capability::TLS_BINDING,
];
const MAX_CARD_EVERY_MS: u64 = 10_000;
/// The binding this member made, next to its key: never the token, only its digest.
pub const BINDING_FILE: &str = "fleet-hosted.json";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A relay's refusal, with what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: Option<RefusalCode>,
    pub reason: String,
}

impl Refusal {
    pub fn code_name(&self) -> Option<&'static str> {
        Some(match self.code? {
            RefusalCode::TokenSpent => "token_spent",
            RefusalCode::TokenExpired => "token_expired",
            RefusalCode::TokenUnknown => "token_unknown",
            RefusalCode::Unbound => "unbound",
            RefusalCode::Kicked => "kicked",
            RefusalCode::Revoked => "revoked",
            RefusalCode::NoSuchEvent => "no_such_event",
            RefusalCode::WrongEvent => "wrong_event",
            RefusalCode::Banned => "banned",
            RefusalCode::BadCode => "bad_code",
            RefusalCode::Full => "full",
            RefusalCode::SessionEnded => "session_ended",
            RefusalCode::Other => "other",
        })
    }

    /// What this means for a demo member, beyond the relay's own words.
    pub fn advice(&self) -> &'static str {
        match self.code {
            Some(RefusalCode::TokenSpent) => {
                "this connection code was used already. Make a new connection code in the CIC portal for this member's account and replace its line"
            }
            Some(RefusalCode::TokenExpired) => {
                "the connection code, or the binding it made, is past its 12 hours. Make a new connection code in the CIC portal and replace this member's line"
            }
            Some(RefusalCode::TokenUnknown) => {
                "no CIC account has this connection code: check the line was pasted whole, or make a new connection code in the CIC portal"
            }
            Some(RefusalCode::Unbound) => {
                "this member's key is not bound to a CIC account (its binding was dropped). Make a new connection code in the CIC portal and replace its line"
            }
            Some(RefusalCode::Kicked) => {
                "the account was kicked from the event: an admin or the admiral must add it again"
            }
            Some(RefusalCode::Revoked) => "the account is disabled: ask a CIC admin",
            Some(RefusalCode::NoSuchEvent) => {
                "the event chosen is not one of this account's open events: check --event"
            }
            Some(RefusalCode::WrongEvent) => {
                "this per-event connection code is for another event than the one asked for"
            }
            Some(
                RefusalCode::Banned
                | RefusalCode::BadCode
                | RefusalCode::Full
                | RefusalCode::SessionEnded,
            ) => "a peer-to-peer host's refusal, not a CIC relay's: check the invite",
            Some(RefusalCode::Other) | None => "the relay gave no code this tool knows",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = self.reason.trim().trim_end_matches('.');
        let reason = if reason.is_empty() { "refused" } else { reason };
        match self.code_name() {
            Some(code) => write!(f, "{reason} ({code}): {}", self.advice()),
            None => write!(f, "{reason}: {}", self.advice()),
        }
    }
}

/// Attached to an event: the relay's `joined`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub event: String,
    pub name: String,
    pub member: Option<String>,
    pub handle: String,
    pub unit: Option<String>,
    pub card_every: Duration,
}

/// What to do with a message while attaching.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Send(Message),
    Wait,
    Joined(Joined),
    Refused(Refusal),
    Closed(String),
}

/// After `welcome`, until `joined`: `connect`, then `choose` when the relay sends `events`.
#[derive(Debug)]
pub struct Attach {
    choose: Option<String>,
    chosen: Option<String>,
    with_token: bool,
    answered: bool,
}

impl Attach {
    /// The `connect` to send first. `event` is the per-event token's event, or the event a
    /// reconnect was in; `choose` is the event to pick from an `events` list, else the first.
    pub fn new(
        token: Option<String>,
        event: Option<String>,
        choose: Option<String>,
    ) -> (Self, Message) {
        let attach = Self {
            choose,
            chosen: None,
            with_token: token.is_some(),
            answered: false,
        };
        (attach, Message::Connect { token, event })
    }

    /// The relay took the token: it answered a `connect` that carried one with anything but a
    /// refusal, so the token is spent.
    pub fn spent_token(&self) -> bool {
        self.with_token && self.answered
    }

    pub fn on(&mut self, message: Message) -> Step {
        match message {
            Message::Events { events } => {
                self.answered = true;
                if self.chosen.is_some() {
                    return Step::Wait;
                }
                let picked = self
                    .choose
                    .as_ref()
                    .and_then(|c| events.iter().find(|e| &e.id == c))
                    .or_else(|| events.first());
                match picked {
                    Some(e) => {
                        self.chosen = Some(e.id.clone());
                        Step::Send(Message::Choose {
                            event: e.id.clone(),
                        })
                    }
                    // The relay sends the list again when one opens.
                    None => Step::Wait,
                }
            }
            Message::Joined {
                event,
                name,
                member,
                handle,
                unit,
                card_every_ms,
                ..
            } => {
                self.answered = true;
                let every = card_every_ms
                    .unwrap_or(CARD_EVERY_MS)
                    .clamp(MIN_CARD_EVERY_MS, MAX_CARD_EVERY_MS);
                Step::Joined(Joined {
                    name: if name.is_empty() { event.clone() } else { name },
                    event,
                    member,
                    handle,
                    unit,
                    card_every: Duration::from_millis(every),
                })
            }
            Message::Refused { reason, code } => Step::Refused(Refusal { code, reason }),
            Message::Bye { reason } => Step::Closed(
                format!("the relay closed the connection {reason}")
                    .trim()
                    .to_owned(),
            ),
            Message::Roster { .. }
            | Message::Structure { .. }
            | Message::Card { .. }
            | Message::Subscribe { .. } => Step::Wait,
            _ => Step::Closed("the relay sent an unexpected message".to_owned()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `fleet-hosted.json` keeps: the server key, the digest of the token spent with it, and
/// the event joined last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Binding {
    pub key: Option<String>,
    pub spent: Option<String>,
    pub event: Option<String>,
}

impl Binding {
    pub fn read(file: &std::path::Path) -> Self {
        let Ok(text) = std::fs::read_to_string(file) else {
            return Self::default();
        };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else {
            return Self::default();
        };
        let field = |name| raw.get(name).and_then(Value::as_str).map(str::to_owned);
        Self {
            key: field("key"),
            spent: field("spent"),
            event: field("event").filter(|e| fleet_proto::structure::is_id(e)),
        }
    }

    pub fn write(&self, file: &std::path::Path) -> io::Result<()> {
        let mut raw = json!({});
        for (name, value) in [
            ("key", &self.key),
            ("spent", &self.spent),
            ("event", &self.event),
        ] {
            if let Some(v) = value {
                raw[name] = json!(v);
            }
        }
        std::fs::write(file, format!("{raw:#}\n"))
    }

    /// Whether this invite's token was spent here before, so the member connects by key.
    pub fn spent(&self, invite: &HostedInvite) -> bool {
        self.key.as_deref() == Some(invite.key().to_string().as_str())
            && self.spent.as_deref() == Some(hex(&invite.digest()).as_str())
    }
}

/// How one connection ended.
#[derive(Debug)]
enum End {
    Lost { reason: String, was_online: bool },
    Refused(Refusal),
    Stopped,
}

fn lost(reason: impl ToString, was_online: bool) -> End {
    End::Lost {
        reason: reason.to_string(),
        was_online,
    }
}

fn net(e: &io::Error, was_online: bool) -> End {
    use io::ErrorKind as K;
    let reason = match e.kind() {
        K::ConnectionRefused => "nothing is listening at that address".to_owned(),
        K::UnexpectedEof | K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => {
            "the connection dropped".to_owned()
        }
        K::TimedOut => "no answer".to_owned(),
        _ => e.to_string(),
    };
    lost(reason, was_online)
}

/// A handshake failure that a retry cannot fix, as a refusal without a code.
fn local_refusal(reason: impl ToString) -> End {
    End::Refused(Refusal {
        code: None,
        reason: reason.to_string(),
    })
}

fn identity_refusal(e: &IdentityError) -> End {
    match e {
        IdentityError::WrongServerKey => {
            local_refusal("the relay's identity key is not the one in the invite")
        }
        IdentityError::BadSignature => local_refusal("the relay could not prove its identity key"),
        IdentityError::Stale => local_refusal("the relay's clock and this one are too far apart"),
        other => local_refusal(other),
    }
}

/// One line, or `None` at a clean end. Cancel-safe: a part line stays in `buf`.
async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<Option<Message>> {
    let room = (MAX_FRAME_BYTES + 2).saturating_sub(buf.len()) as u64;
    (&mut *reader).take(room).read_until(b'\n', buf).await?;
    if buf.last() != Some(&b'\n') {
        if buf.is_empty() {
            return Ok(None);
        }
        if buf.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a line over 64 KiB",
            ));
        }
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let message = Message::decode(buf);
    buf.clear();
    message
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Message) -> io::Result<()> {
    writer.write_all(&message.encode()).await?;
    writer.flush().await
}

#[derive(Debug, Default)]
struct Shared {
    link: LinkState,
    problem: Option<String>,
    life: Option<Life>,
    handle: Option<String>,
    /// The token, until the relay takes it.
    token: Option<String>,
    /// A `connect` with the token went out and the connection dropped before any answer: the
    /// relay may have spent it, so a `token_spent` next is answered by connecting by key.
    token_maybe_spent: bool,
    event: Option<String>,
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub struct HostedClient {
    label: String,
    identity: Identity,
    core: Arc<ControlCore>,
    consent: ConsentStore,
    connector: Arc<dyn Connector>,
    invite: HostedInvite,
    binding_file: PathBuf,
    choose: Option<String>,
    /// Only for the subscription a Hardpoint with the default view sends.
    hub: FleetHub,
    shared: Mutex<Shared>,
    running: Mutex<Option<Running>>,
}

pub struct HostedOptions {
    /// For the log: `member-03`, never a handle or the token.
    pub label: String,
    pub identity: Identity,
    pub core: Arc<ControlCore>,
    pub consent: ConsentStore,
    pub connector: Arc<dyn Connector>,
    pub invite: HostedInvite,
    pub binding_file: PathBuf,
    pub choose: Option<String>,
}

impl HostedClient {
    pub fn new(o: HostedOptions) -> Arc<Self> {
        let binding = Binding::read(&o.binding_file);
        let (token, event) = if binding.spent(&o.invite) {
            (None, binding.event.clone())
        } else {
            (
                Some(o.invite.token().to_owned()),
                o.invite.event().map(str::to_owned),
            )
        };
        let me = o.identity.public();
        Arc::new(Self {
            label: o.label,
            identity: o.identity,
            core: o.core,
            consent: o.consent,
            connector: o.connector,
            invite: o.invite,
            binding_file: o.binding_file,
            choose: o.choose,
            hub: FleetHub::new(Arc::new(FleetViewStore::new(None)), me),
            shared: Mutex::new(Shared {
                token,
                event,
                ..Shared::default()
            }),
            running: Mutex::default(),
        })
    }

    pub fn public_key(&self) -> PublicKey {
        self.identity.public()
    }

    pub fn link(&self) -> LinkState {
        lock(&self.shared).link
    }

    /// Why the last connection ended, when it did not end well.
    pub fn problem(&self) -> Option<String> {
        lock(&self.shared).problem.clone()
    }

    pub fn life(&self) -> Option<Life> {
        lock(&self.shared).life
    }

    /// The handle the relay knows this member by, once joined.
    pub fn handle(&self) -> Option<String> {
        lock(&self.shared).handle.clone()
    }

    /// The event joined, or the one to ask for on the next connection.
    pub fn event(&self) -> Option<String> {
        lock(&self.shared).event.clone()
    }

    /// Whether the next `connect` carries the token.
    pub fn has_token(&self) -> bool {
        lock(&self.shared).token.is_some()
    }

    pub fn start(self: &Arc<Self>) {
        let mut running = lock(&self.running);
        if running.is_some() {
            return;
        }
        let (stop, stopped) = watch::channel(false);
        let me = Arc::clone(self);
        let task = tokio::spawn(async move { me.run(stopped).await });
        *running = Some(Running { stop, task });
    }

    /// Disconnects without leaving; [`Self::start`] connects again by key.
    pub async fn stop(&self) {
        let running = lock(&self.running).take();
        if let Some(r) = running {
            let _ = r.stop.send(true);
            let _ = r.task.await;
        }
        let mut s = lock(&self.shared);
        if s.link != LinkState::Refused {
            s.link = LinkState::None;
        }
    }

    fn set_link(&self, link: LinkState, problem: Option<String>) {
        let mut s = lock(&self.shared);
        s.link = link;
        s.problem = problem;
    }

    fn save_binding(&self) {
        let s = lock(&self.shared);
        let mut b = Binding::read(&self.binding_file);
        b.key = Some(self.invite.key().to_string());
        if s.token.is_none() {
            b.spent = Some(hex(&self.invite.digest()));
        }
        b.event.clone_from(&s.event);
        drop(s);
        if let Err(e) = b.write(&self.binding_file) {
            tracing::warn!("{}: could not keep its binding: {e}", self.label);
        }
    }

    async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut backoff = Backoff::default();
        loop {
            {
                let mut s = lock(&self.shared);
                s.link = LinkState::Connecting;
            }
            match self.connect_once(&mut stop).await {
                End::Stopped => return,
                End::Refused(refusal) => {
                    let mut s = lock(&self.shared);
                    if refusal.code == Some(RefusalCode::TokenSpent) && s.token_maybe_spent {
                        s.token = None;
                        s.token_maybe_spent = false;
                        drop(s);
                        tracing::info!(
                            "{}: the token went through before the drop; connecting by key",
                            self.label
                        );
                        self.save_binding();
                        continue;
                    }
                    drop(s);
                    tracing::warn!("{} refused: {refusal}", self.label);
                    self.set_link(LinkState::Refused, Some(refusal.to_string()));
                    return;
                }
                End::Lost { reason, was_online } => {
                    if was_online {
                        backoff.reset();
                    }
                    let wait = backoff.next_wait();
                    tracing::info!(
                        "{} offline ({reason}); trying again in {} ms",
                        self.label,
                        wait.as_millis()
                    );
                    self.set_link(LinkState::Offline, Some(reason));
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        _ = stop.wait_for(|s| *s) => return,
                    }
                }
            }
        }
    }

    async fn connect_once(&self, stop: &mut watch::Receiver<bool>) -> End {
        let (host, port) = (self.invite.host().to_owned(), self.invite.port());
        let Connected { stream, binding } = tokio::select! {
            r = timeout(CONNECT_TIMEOUT, self.connector.connect(&host, port)) => match r {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return net(&e, false),
                Err(_) => return lost("no answer", false),
            },
            _ = stop.wait_for(|s| *s) => return End::Stopped,
        };
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        let end = tokio::select! {
            end = self.session(&mut reader, &mut writer, &mut buf, binding) => end,
            _ = stop.wait_for(|s| *s) => End::Stopped,
        };
        if matches!(end, End::Stopped) {
            let bye = Message::Bye {
                reason: String::new(),
            };
            let _ = write_message(&mut writer, &bye).await;
        }
        let _ = timeout(Duration::from_secs(1), writer.shutdown()).await;
        end
    }

    /// The next message while attaching, within [`CONNECT_TIMEOUT`].
    async fn next<R: AsyncBufRead + Unpin>(
        reader: &mut R,
        buf: &mut Vec<u8>,
        limit: Option<Duration>,
    ) -> Result<Message, End> {
        let read = read_message(reader, buf);
        let got = match limit {
            Some(limit) => timeout(limit, read)
                .await
                .map_err(|_| lost("the relay did not answer", false))?,
            None => read.await,
        };
        match got {
            Err(e) => Err(net(&e, false)),
            Ok(None) => Err(lost("the relay closed the connection", false)),
            Ok(Some(m)) => Ok(m),
        }
    }

    async fn session<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        buf: &mut Vec<u8>,
        binding: Option<TlsBinding>,
    ) -> End
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        if let Err(end) = self.handshake(reader, writer, buf, binding).await {
            return end;
        }
        let joined = match self.attach(reader, writer, buf).await {
            Ok(j) => j,
            Err(end) => return end,
        };
        self.online(reader, writer, buf, joined).await
    }

    async fn handshake<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        buf: &mut Vec<u8>,
        binding: Option<TlsBinding>,
    ) -> Result<(), End>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let ours = Hello::new(SOFTWARE, CAPABILITIES);
        let nonce = Nonce::random().map_err(|e| lost(e, false))?;
        let hello = Message::ClientHello {
            hello: ours.clone(),
            key: self.identity.public(),
            nonce,
        };
        write_message(writer, &hello)
            .await
            .map_err(|e| net(&e, false))?;
        let (hello, key, challenge, proof) =
            match Self::next(reader, buf, Some(CONNECT_TIMEOUT)).await? {
                Message::ServerHello {
                    hello,
                    key,
                    challenge,
                    proof,
                    ..
                } => (hello, key, challenge, proof),
                Message::Refused { reason, code } => {
                    return Err(End::Refused(Refusal { code, reason }));
                }
                _ => return Err(lost("the relay did not say hello", false)),
            };
        ours.check(&hello).map_err(local_refusal)?;
        if !hello.has(capability::TLS_BINDING) {
            return Err(local_refusal(
                "the relay does not bind its identity to the TLS session",
            ));
        }
        if !hello.has(capability::CONNECT) {
            return Err(local_refusal(
                "the server does not take connection codes (no connect): is this a CIC relay?",
            ));
        }
        let binding =
            binding.ok_or_else(|| local_refusal("the connection has no TLS session to bind to"))?;
        let answer = ClientHandshake {
            identity: &self.identity,
            nonce,
            pinned: Some(self.invite.key()),
            binding: Some(binding),
        }
        .answer(&key, &challenge, &proof, now_ms())
        .map_err(|e| identity_refusal(&e))?;
        write_message(writer, &Message::ClientProof { proof: answer })
            .await
            .map_err(|e| net(&e, false))?;
        match Self::next(reader, buf, Some(CONNECT_TIMEOUT)).await? {
            Message::Welcome { .. } => Ok(()),
            Message::Refused { reason, code } => Err(End::Refused(Refusal { code, reason })),
            _ => Err(lost("the relay did not welcome this member", false)),
        }
    }

    async fn attach<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        buf: &mut Vec<u8>,
    ) -> Result<Joined, End>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let (token, event) = {
            let s = lock(&self.shared);
            (s.token.clone(), s.event.clone())
        };
        let (mut attach, connect) = Attach::new(token, event, self.choose.clone());
        write_message(writer, &connect)
            .await
            .map_err(|e| net(&e, false))?;
        let mut waited_for_events = false;
        loop {
            // A relay with no open event for this account sends `events` and waits: no limit.
            let limit = (!waited_for_events).then_some(CONNECT_TIMEOUT);
            let message = match Self::next(reader, buf, limit).await {
                Ok(m) => m,
                Err(end) => {
                    if attach.with_token && !attach.answered {
                        lock(&self.shared).token_maybe_spent = true;
                    }
                    return Err(end);
                }
            };
            let step = attach.on(message);
            if attach.spent_token() && self.has_token() {
                let mut s = lock(&self.shared);
                s.token = None;
                s.token_maybe_spent = false;
                drop(s);
                self.save_binding();
            }
            match step {
                Step::Send(m) => {
                    if let Message::Choose { event } = &m {
                        tracing::info!("{}: choosing event {event}", self.label);
                    }
                    write_message(writer, &m)
                        .await
                        .map_err(|e| net(&e, false))?;
                    waited_for_events = true;
                }
                Step::Wait => {
                    if attach.answered {
                        waited_for_events = true;
                    }
                }
                Step::Joined(j) => return Ok(j),
                Step::Refused(r) => return Err(End::Refused(r)),
                Step::Closed(reason) => return Err(lost(reason, false)),
            }
        }
    }

    fn card(&self, handle: &str, life: &LifeTracker, wanted: &Subscription) -> Card {
        let consent = self.consent.get();
        let sender = Sender {
            member: self.identity.public().to_string(),
            handle: handle.to_owned(),
            consent: consent.clone(),
        };
        let fields: HashSet<&'static str> = wanted.fields.iter().copied().collect();
        let state = self
            .core
            .state()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut card = build_card(&state, life, now_ms(), &sender, &fields);
        drop(state);
        filter_card(&mut card, &consent, wanted);
        card
    }

    async fn online<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        buf: &mut Vec<u8>,
        joined: Joined,
    ) -> End
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        {
            let mut s = lock(&self.shared);
            s.link = LinkState::Online;
            s.problem = None;
            s.handle = Some(joined.handle.clone());
            s.event = Some(joined.event.clone());
        }
        self.save_binding();
        tracing::info!(
            "{} joined event {} ({})",
            self.label,
            joined.event,
            joined.unit.as_deref().unwrap_or("unassigned")
        );
        let mut handle = joined.handle;
        let _following = self.core.follow();
        let life = Arc::new(Mutex::new(LifeTracker::default()));
        let _frames = {
            let life = Arc::clone(&life);
            self.core
                .on_frame(move |frame| lock(&life).observe_frame(frame, now_ms()))
        };
        let subscription = self.hub.subscription();
        if let Err(e) = write_message(
            writer,
            &Message::Subscribe {
                subscription: subscription.clone(),
            },
        )
        .await
        {
            return net(&e, true);
        }
        let mut wanted = Subscription::of(card_fields().map(|f| f.id));
        let mut tick = tokio::time::interval(joined.card_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                read = read_message(reader, buf) => {
                    let message = match read {
                        Err(e) => return net(&e, true),
                        Ok(None) => return lost("the relay closed the connection", true),
                        Ok(Some(m)) => m,
                    };
                    match message {
                        Message::Subscribe { subscription } => wanted = subscription,
                        Message::Joined { event, handle: known_as, .. } => {
                            handle.clone_from(&known_as);
                            let mut s = lock(&self.shared);
                            s.handle = Some(known_as);
                            s.event = Some(event);
                        }
                        Message::Card { .. }
                        | Message::Roster { .. }
                        | Message::Structure { .. }
                        | Message::Events { .. }
                        | Message::ChangeRefused { .. } => {}
                        Message::Refused { reason, code } => {
                            return End::Refused(Refusal { code, reason });
                        }
                        Message::Bye { reason } => {
                            return lost(
                                format!("the relay closed the connection {reason}").trim(),
                                true,
                            );
                        }
                        _ => return lost("the relay sent an unexpected message", true),
                    }
                }
                _ = tick.tick() => {
                    let card = {
                        let mut tracker = lock(&life);
                        let health = health_fraction(
                            &self.core.state().lock().unwrap_or_else(PoisonError::into_inner),
                        );
                        tracker.observe_health(health, now_ms());
                        self.card(&handle, &tracker, &wanted)
                    };
                    lock(&self.shared).life = card.state;
                    if let Err(e) = write_message(writer, &Message::Card { card }).await {
                        return net(&e, true);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_proto::EventChoice;
    use fleet_proto::invite::new_connect_token;

    fn events(ids: &[&str]) -> Message {
        Message::Events {
            events: ids
                .iter()
                .map(|id| EventChoice {
                    id: (*id).to_owned(),
                    name: format!("Op {id}"),
                    unit: None,
                    role: None,
                })
                .collect(),
        }
    }

    fn joined(event: &str, every: Option<u64>) -> Message {
        Message::Joined {
            event: event.to_owned(),
            name: String::new(),
            member: Some("u-1".to_owned()),
            handle: "Vega".to_owned(),
            unit: Some("ship-1".to_owned()),
            grants: vec![],
            card_every_ms: every,
        }
    }

    #[test]
    fn connect_carries_the_token_and_the_event() {
        let (attach, m) = Attach::new(Some("t".to_owned()), Some("op-1".to_owned()), None);
        assert_eq!(
            m,
            Message::Connect {
                token: Some("t".to_owned()),
                event: Some("op-1".to_owned())
            }
        );
        assert!(!attach.spent_token());
        let (_, by_key) = Attach::new(None, None, None);
        assert_eq!(
            by_key,
            Message::Connect {
                token: None,
                event: None
            }
        );
    }

    #[test]
    fn events_are_answered_with_the_configured_event_else_the_first_and_only_once() {
        let (mut a, _) = Attach::new(Some("t".to_owned()), None, Some("b".to_owned()));
        assert_eq!(a.on(events(&[])), Step::Wait, "none open yet");
        assert!(a.spent_token(), "any answer but a refusal spends the token");
        assert_eq!(
            a.on(events(&["a", "b"])),
            Step::Send(Message::Choose {
                event: "b".to_owned()
            })
        );
        assert_eq!(a.on(events(&["a", "b", "c"])), Step::Wait);
        let Step::Joined(j) = a.on(joined("b", Some(50))) else {
            panic!("not joined");
        };
        assert_eq!(j.event, "b");
        assert_eq!(j.name, "b");
        assert_eq!(j.member.as_deref(), Some("u-1"));
        assert_eq!(j.card_every, Duration::from_millis(MIN_CARD_EVERY_MS));

        let (mut first, _) = Attach::new(None, None, Some("zzz".to_owned()));
        assert_eq!(
            first.on(events(&["a", "b"])),
            Step::Send(Message::Choose {
                event: "a".to_owned()
            })
        );
        assert!(!first.spent_token(), "no token was sent");
    }

    #[test]
    fn joined_at_once_uses_the_default_rate_and_a_refusal_keeps_the_token() {
        let (mut a, _) = Attach::new(Some("t".to_owned()), None, None);
        let Step::Joined(j) = a.on(joined("op", None)) else {
            panic!("not joined");
        };
        assert_eq!(j.card_every, Duration::from_millis(CARD_EVERY_MS));

        let (mut r, _) = Attach::new(Some("t".to_owned()), None, None);
        let refused = Message::Refused {
            reason: "Spent".to_owned(),
            code: Some(RefusalCode::TokenSpent),
        };
        assert!(matches!(r.on(refused), Step::Refused(_)));
        assert!(!r.spent_token());
        assert!(matches!(
            r.on(Message::Bye {
                reason: String::new()
            }),
            Step::Closed(_)
        ));
        assert!(matches!(
            r.on(Message::Welcome {
                event: String::new(),
                capabilities: vec![]
            }),
            Step::Closed(_)
        ));
    }

    #[test]
    fn every_refusal_code_says_what_to_do() {
        let codes = [
            RefusalCode::TokenSpent,
            RefusalCode::TokenExpired,
            RefusalCode::TokenUnknown,
            RefusalCode::Unbound,
            RefusalCode::Kicked,
            RefusalCode::Revoked,
            RefusalCode::NoSuchEvent,
            RefusalCode::WrongEvent,
        ];
        let mut seen = HashSet::new();
        for code in codes {
            let r = Refusal {
                code: Some(code),
                reason: "The relay said no.".to_owned(),
            };
            let text = r.to_string();
            let name = r.code_name().unwrap();
            assert!(
                text.starts_with(&format!("The relay said no ({name}): ")),
                "{text}"
            );
            assert!(seen.insert(r.advice()), "{name} has its own advice");
        }
        let spent = Refusal {
            code: Some(RefusalCode::TokenSpent),
            reason: String::new(),
        };
        assert!(
            spent
                .to_string()
                .contains("Make a new connection code in the CIC portal")
        );
        let revoked = Refusal {
            code: Some(RefusalCode::Revoked),
            reason: String::new(),
        };
        assert!(revoked.to_string().contains("ask a CIC admin"));
        let other = Refusal {
            code: None,
            reason: "Full".to_owned(),
        };
        assert_eq!(
            other.to_string(),
            "Full: the relay gave no code this tool knows"
        );
    }

    #[test]
    fn the_binding_keeps_a_digest_never_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(BINDING_FILE);
        let token = new_connect_token().unwrap();
        let key = Identity::from_seed([5; 32]).public();
        let invite: HostedInvite = format!("cic://cic.example.org:7443?key={key}&connect={token}")
            .parse()
            .unwrap();
        assert!(!Binding::read(&file).spent(&invite));
        let b = Binding {
            key: Some(key.to_string()),
            spent: Some(hex(&invite.digest())),
            event: Some("op-1".to_owned()),
        };
        b.write(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&token));
        let back = Binding::read(&file);
        assert_eq!(back, b);
        assert!(back.spent(&invite));
        let other: HostedInvite = format!(
            "cic://cic.example.org:7443?key={key}&connect={}",
            new_connect_token().unwrap()
        )
        .parse()
        .unwrap();
        assert!(!back.spent(&other), "a new token is used");
    }
}
