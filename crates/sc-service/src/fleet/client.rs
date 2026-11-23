//! The fleet client: joins a fleet from an invite and keeps the connection.
//!
//! One connection at a time (a member is in one event at a time). Over TLS, whose certificate is
//! not checked: the pin is the server's identity key from the invite, which the server proves in
//! the handshake from `fleet_proto::identity`. Both proofs are bound to the TLS session (the
//! `tls-binding` capability), so a party that terminates TLS in the middle cannot relay the
//! handshake; a server without it is refused. Then `connect` (a CIC relay, with the connection
//! token from the invite, or none once the key is bound) or `join` (peer to peer), then this
//! player's card at the server's rate, filtered by the send consent before it leaves, while the
//! cards, roster and event structure the role allows come in. A relay that cannot pick the event
//! sends the open ones and the player chooses. Structure changes are made in the CIC portal only;
//! this client receives them and never asks for one.
//!
//! A lost connection is tried again with backoff ([`Backoff`]) and the panel shows the fleet
//! offline; a refusal (wrong key, bad code, a spent token, a different protocol major) is not
//! retried until the player connects again. Neither the join code nor the connection token is
//! ever written to disk: each is held in memory until spent, and a later run reconnects by key.

use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    split,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use fleet_proto::card::{FIELDS, card_fields, is_handle};
use fleet_proto::consent::filter_card;
use fleet_proto::hello::capability;
use fleet_proto::identity::{ClientHandshake, TLS_BINDING_LABEL, TLS_BINDING_LEN, TlsBinding};
use fleet_proto::message::{
    BYE_LEAVE, CARD_EVERY_MS, FrameError, MAX_FRAME_BYTES, MIN_CARD_EVERY_MS,
};
use fleet_proto::{
    Card, EventChoice, Hello, Identity, IdentityError, Invite, Message, Nonce, PublicKey,
    RefusalCode, SendConsent, Subscription,
};
use sc_core::ControlCore;

use super::card::{LifeTracker, Sender, build_card, health_fraction};
use super::cic::portal_of;
use super::hub::{EventRef, FleetHub, FleetRef, LinkInfo, LinkState};
use super::store::{ConsentStore, FleetsStore, JoinedFleet};

pub const SOFTWARE: &str = concat!("hardpoint ", env!("CARGO_PKG_VERSION"));
const CAPABILITIES: &[&str] = &[
    capability::CARDS,
    capability::SUBSCRIBE,
    capability::STRUCTURE,
    capability::JOIN,
    capability::CONNECT,
    capability::TLS_BINDING,
];
/// The handle a hosted fleet shows until the relay says the account's.
const HOSTED_HANDLE: &str = "CIC member";
/// The TCP and TLS connection, and each handshake message, must come within this.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CARD_EVERY_MS: u64 = 10_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits before trying again: 1 s, doubling to 30 s, each with up to a fifth taken off at random
/// so many members do not all come back at once. Reset once a connection is up.
#[derive(Debug, Default, Clone)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub const FIRST: Duration = Duration::from_secs(1);
    pub const MAX: Duration = Duration::from_secs(30);

    /// The wait before attempt `n` (from 0), before the random part.
    pub fn base(n: u32) -> Duration {
        Self::FIRST.saturating_mul(1u32 << n.min(8)).min(Self::MAX)
    }

    pub fn next_wait(&mut self) -> Duration {
        let base = Self::base(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        let mut byte = [0u8];
        let _ = getrandom::fill(&mut byte);
        base - base * u32::from(byte[0]) / (255 * 5)
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// An open stream, and the binding exported from its TLS session (`None` when there is none,
/// which the client refuses).
pub struct Connected {
    pub stream: Box<dyn Io>,
    pub binding: Option<TlsBinding>,
}

type Connecting<'a> = Pin<Box<dyn Future<Output = io::Result<Connected>> + Send + 'a>>;

/// The exported keying material the identity proofs are bound to.
pub fn tls_binding(conn: &rustls::ConnectionCommon<impl Sized>) -> io::Result<TlsBinding> {
    conn.export_keying_material([0u8; TLS_BINDING_LEN], TLS_BINDING_LABEL, None)
        .map(TlsBinding)
        .map_err(io::Error::other)
}

/// Opens the stream a fleet connection runs over.
pub trait Connector: Send + Sync {
    fn connect<'a>(&'a self, host: &'a str, port: u16) -> Connecting<'a>;
}

/// TLS 1.3 to the fleet port. The certificate is not checked (the identity handshake pins the
/// server), but the server must still prove it holds the certificate's key.
pub struct TlsConnector {
    config: Arc<rustls::ClientConfig>,
}

impl TlsConnector {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            config: crate::tls::client_config()?,
        })
    }
}

impl Connector for TlsConnector {
    fn connect<'a>(&'a self, host: &'a str, port: u16) -> Connecting<'a> {
        Box::pin(async move {
            let name = rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let tcp = TcpStream::connect((host, port)).await?;
            tcp.set_nodelay(true)?;
            let tls = tokio_rustls::TlsConnector::from(Arc::clone(&self.config))
                .connect(name, tcp)
                .await?;
            let binding = tls_binding(tls.get_ref().1)?;
            Ok(Connected {
                stream: Box::new(tls),
                binding: Some(binding),
            })
        })
    }
}

/// Where to connect, and the join code or connection token while it is still unspent.
#[derive(Debug, Clone)]
struct Target {
    fleet: JoinedFleet,
    code: Option<String>,
    token: Option<String>,
    /// The event a per-event invite names.
    event: Option<String>,
    /// Joined before: kept in fleets.json even when refused.
    known: bool,
}

impl Target {
    fn rejoin(fleet: JoinedFleet) -> Self {
        Self {
            fleet,
            code: None,
            token: None,
            event: None,
            known: true,
        }
    }
}

/// What the panel asks of the running connection.
#[derive(Debug)]
enum Command {
    Choose(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Disconnect (the app is closing, or another fleet is joined).
    Quit,
    /// Leave the event: tell the server, then disconnect.
    Leave,
}

/// How one connection ended.
#[derive(Debug)]
enum End {
    Lost {
        reason: String,
        was_online: bool,
    },
    Refused {
        reason: String,
        hint: Option<&'static str>,
        code: Option<RefusalCode>,
    },
    Stopped(Stop),
}

fn refused(e: impl ToString) -> End {
    End::Refused {
        reason: e.to_string(),
        hint: None,
        code: None,
    }
}

/// The server's refusal, with what to do when its code says more.
fn server_refusal(reason: String, code: Option<RefusalCode>) -> End {
    End::Refused {
        reason,
        hint: code.and_then(RefusalCode::hint),
        code,
    }
}

fn lost(e: impl ToString, was_online: bool) -> End {
    End::Lost {
        reason: e.to_string(),
        was_online,
    }
}

/// A network error as a player reads it.
fn net(e: &io::Error, was_online: bool) -> End {
    use io::ErrorKind as K;
    let reason = match e.kind() {
        K::ConnectionRefused => "nothing is listening at that address".to_owned(),
        K::UnexpectedEof | K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => {
            "the connection dropped".to_owned()
        }
        K::TimedOut => "no answer".to_owned(),
        K::NotFound => "the host name is not known".to_owned(),
        _ => e.to_string(),
    };
    lost(reason, was_online)
}

fn identity_refusal(e: &IdentityError) -> End {
    match e {
        IdentityError::WrongServerKey => {
            refused("The server's identity key is not the one in the invite")
        }
        IdentityError::BadSignature => refused("The server could not prove its identity key"),
        IdentityError::Stale => refused("The server's clock and this PC's are too far apart"),
        other => refused(other),
    }
}

struct Running {
    stop: watch::Sender<Option<Stop>>,
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
    key: PublicKey,
}

pub struct FleetClient {
    hub: Arc<FleetHub>,
    core: Arc<ControlCore>,
    identity: Identity,
    fleets: FleetsStore,
    consent: ConsentStore,
    connector: Arc<dyn Connector>,
    link: Mutex<LinkInfo>,
    running: Mutex<Option<Running>>,
}

impl FleetClient {
    pub fn new(
        hub: Arc<FleetHub>,
        core: Arc<ControlCore>,
        identity: Identity,
        fleets: FleetsStore,
        consent: ConsentStore,
        connector: Arc<dyn Connector>,
    ) -> Arc<Self> {
        let client = Arc::new(Self {
            hub,
            core,
            identity,
            fleets,
            consent,
            connector,
            link: Mutex::default(),
            running: Mutex::default(),
        });
        client.publish(|_| {});
        client
    }

    pub fn public_key(&self) -> PublicKey {
        self.identity.public()
    }

    pub fn consent(&self) -> SendConsent {
        self.consent.get()
    }

    /// Updates the connection's state and tells the panel, with the stored fleets and consent.
    fn publish(&self, change: impl FnOnce(&mut LinkInfo)) {
        let mut link = lock(&self.link);
        change(&mut link);
        link.fleets = self.fleets.get().fleets.iter().map(fleet_ref).collect();
        let consent = self.consent.get();
        link.consent = FIELDS
            .iter()
            .filter(|f| consent.allows(f.id))
            .map(|f| f.id)
            .collect();
        self.hub.set_link(link.clone());
    }

    /// Connects to the fleet joined last, if the player has not left it.
    pub fn resume(self: &Arc<Self>) {
        if let Some(fleet) = self.fleets.get().active_fleet().cloned() {
            self.start(Target::rejoin(fleet));
        }
    }

    /// Joins the fleet an invite names: a CIC connection code (the handle comes from the
    /// account) or a peer-to-peer join code, as `handle`. Another fleet's connection is closed
    /// first (without leaving it).
    pub fn join(self: &Arc<Self>, text: &str, handle: &str) -> Result<(), String> {
        let invite: Invite = text.parse().map_err(|e| format!("{e}"))?;
        let before = self
            .fleets
            .get()
            .fleets
            .into_iter()
            .find(|f| f.key == invite.key);
        let hosted = invite.connect.is_some();
        let handle = match check_handle(handle) {
            Ok(h) => h,
            Err(_) if hosted => before
                .as_ref()
                .map_or_else(|| HOSTED_HANDLE.to_owned(), |f| f.handle.clone()),
            Err(e) => return Err(e),
        };
        let known = before.is_some();
        let before = before.filter(|f| f.hosted == hosted);
        // A string without the portal (an older relay, or one near the length cap) keeps the one
        // known for this server.
        let portal = hosted
            .then(|| portal_of(text).or_else(|| before.as_ref().and_then(|f| f.portal.clone())))
            .flatten();
        let event = before.and_then(|f| f.event);
        self.start(Target {
            fleet: JoinedFleet {
                host: invite.host,
                port: invite.port,
                key: invite.key,
                handle,
                event,
                hosted,
                // A new connection code offers the events again; `joined` sets the one chosen.
                event_id: None,
                portal,
            },
            code: invite.code,
            token: invite.connect,
            event: invite.event,
            known,
        });
        Ok(())
    }

    /// The event picked from the relay's list of open events.
    pub fn choose(&self, event: &str) -> Result<(), String> {
        let offered = lock(&self.link)
            .choices
            .as_ref()
            .is_some_and(|c| c.iter().any(|e| e.id == event));
        if !offered {
            return Err("That event is not one of those offered".to_owned());
        }
        self.command(Command::Choose(event.to_owned()))
    }

    fn command(&self, command: Command) -> Result<(), String> {
        lock(&self.running)
            .as_ref()
            .and_then(|r| r.commands.send(command).ok())
            .ok_or_else(|| "Not connected to the fleet".to_owned())
    }

    /// Connects again to a fleet joined before, by its server key.
    pub fn connect(self: &Arc<Self>, key: &str) -> Result<(), String> {
        let fleet = key
            .parse::<PublicKey>()
            .ok()
            .and_then(|k| self.fleets.get().fleets.into_iter().find(|f| f.key == k))
            .ok_or("No such fleet")?;
        self.start(Target::rejoin(fleet));
        Ok(())
    }

    /// Leaves a fleet: tells the server when connected to it, and forgets it here.
    pub async fn leave(self: &Arc<Self>, key: &str) -> Result<(), String> {
        let key = key.parse::<PublicKey>().map_err(|_| "No such fleet")?;
        let current = lock(&self.running).as_ref().is_some_and(|r| r.key == key);
        if current {
            self.stop(Stop::Leave).await;
            self.hub.clear();
            self.publish(|l| *l = LinkInfo::default());
        }
        self.fleets.remove(&key).map_err(|e| e.to_string())?;
        self.publish(|_| {});
        Ok(())
    }

    /// Forgets a fleet here without telling it, such as this Hardpoint's own hosting session
    /// left in fleets.json by a crash.
    pub fn forget(&self, key: &PublicKey) {
        if self.fleets.get().fleets.iter().any(|f| f.key == *key) {
            if let Err(e) = self.fleets.remove(key) {
                tracing::warn!("[fleet] could not update fleets.json: {e}");
            }
            self.publish(|_| {});
        }
    }

    /// Closes the connection, as when the app quits; the fleet is joined again at the next start.
    pub async fn close(&self) {
        self.stop(Stop::Quit).await;
    }

    async fn stop(&self, how: Stop) {
        let running = lock(&self.running).take();
        if let Some(r) = running {
            let _ = r.stop.send(Some(how));
            let _ = r.task.await;
        }
    }

    fn start(self: &Arc<Self>, target: Target) {
        let (stop, stopped) = watch::channel(None);
        let (commands, received) = mpsc::unbounded_channel();
        let key = target.fleet.key;
        let new_code = target.token.is_some();
        let previous = lock(&self.running).take();
        let me = Arc::clone(self);
        let task = tokio::spawn(async move {
            if let Some(r) = previous {
                let _ = r.stop.send(Some(Stop::Quit));
                let _ = r.task.await;
                if r.key != key {
                    me.hub.clear();
                }
            }
            // A new connection code may lead to another event; a reconnect keeps the panel.
            if new_code {
                me.hub.clear_event();
                me.publish(|l| l.event = None);
            }
            me.run(target, stopped, received).await;
        });
        *lock(&self.running) = Some(Running {
            stop,
            commands,
            task,
            key,
        });
    }

    /// Sets what this player agrees to send, from a page's list of field ids.
    pub fn set_consent(&self, raw: Option<&Value>) -> Result<(), Vec<String>> {
        let Some(items) = raw.and_then(Value::as_array) else {
            return Err(vec!["fields must be a list of field ids".to_owned()]);
        };
        let mut consent = SendConsent::none();
        let problems: Vec<String> = items
            .iter()
            .filter(|item| !item.as_str().is_some_and(|id| consent.set(id, true)))
            .map(|item| format!("unknown field {item}"))
            .collect();
        if !problems.is_empty() {
            return Err(problems);
        }
        self.consent.set(consent).map_err(|e| vec![e.to_string()])?;
        self.publish(|_| {});
        Ok(())
    }

    /// This player's card now, exactly as it would leave this PC for a viewer who asks for every
    /// field (a detail view open): the "see it first" preview.
    pub fn preview(&self) -> Card {
        let handle = lock(&self.link)
            .handle
            .clone()
            .or_else(|| self.fleets.get().fleets.first().map(|f| f.handle.clone()))
            .unwrap_or_else(|| "You".to_owned());
        let life = LifeTracker::default();
        self.card(
            &handle,
            &life,
            &Subscription::of(FIELDS.iter().map(|f| f.id)),
        )
    }

    /// Builds this player's card from the game state, then drops everything outside the consent
    /// and the subscription: the last guard before anything is sent.
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

    async fn run(
        self: Arc<Self>,
        mut target: Target,
        mut stop: watch::Receiver<Option<Stop>>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        let mut backoff = Backoff::default();
        let fleet = fleet_ref(&target.fleet);
        loop {
            let attempt = backoff.attempt();
            self.publish(|l| {
                *l = LinkInfo {
                    state: LinkState::Connecting,
                    fleet: Some(fleet.clone()),
                    attempt,
                    ..l.clone()
                };
                l.retry_ms = None;
                l.choices = None;
            });
            let end = self
                .connect_once(&mut target, &mut stop, &mut commands)
                .await;
            match end {
                End::Stopped(_) => return,
                End::Refused { reason, hint, code } => {
                    tracing::warn!("[fleet] {} refused: {reason}", target.fleet.address());
                    // Never joined: a new connection code carries the host and key again.
                    if target.fleet.hosted
                        && !target.known
                        && let Err(e) = self.fleets.remove(&target.fleet.key)
                    {
                        tracing::warn!("[fleet] could not update fleets.json: {e}");
                    }
                    self.publish(|l| {
                        l.state = LinkState::Refused;
                        l.problem = Some(reason);
                        l.hint = hint.map(str::to_owned);
                        l.code = code;
                        l.choices = None;
                    });
                    return;
                }
                End::Lost { reason, was_online } => {
                    if was_online {
                        backoff.reset();
                    }
                    let wait = backoff.next_wait();
                    tracing::info!(
                        "[fleet] {} offline ({reason}); trying again in {} ms",
                        target.fleet.address(),
                        wait.as_millis()
                    );
                    self.publish(|l| {
                        l.state = LinkState::Offline;
                        l.problem = Some(reason);
                        l.retry_ms = Some(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX));
                        l.attempt = backoff.attempt();
                    });
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        _ = stop.changed() => return,
                    }
                }
            }
        }
    }

    async fn connect_once(
        &self,
        target: &mut Target,
        stop: &mut watch::Receiver<Option<Stop>>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> End {
        let (host, port) = (target.fleet.host.clone(), target.fleet.port);
        let Connected { stream, binding } = tokio::select! {
            r = timeout(CONNECT_TIMEOUT, self.connector.connect(&host, port)) => match r {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return net(&e, false),
                Err(_) => return lost("no answer", false),
            },
            _ = stop.wait_for(Option::is_some) => return End::Stopped(Stop::Quit),
        };
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);
        let end = Session {
            client: self,
            target,
            binding,
            commands,
            buf: Vec::new(),
        }
        .run(&mut reader, &mut writer, stop)
        .await;
        if let End::Stopped(how) = end {
            let reason = if how == Stop::Leave { BYE_LEAVE } else { "" };
            let _ = write_message(
                &mut writer,
                &Message::Bye {
                    reason: reason.to_owned(),
                },
            )
            .await;
        }
        let _ = timeout(Duration::from_secs(1), writer.shutdown()).await;
        end
    }
}

fn check_handle(raw: &str) -> Result<String, String> {
    let handle = raw.trim();
    if is_handle(handle) {
        Ok(handle.to_owned())
    } else {
        Err("The handle must be 1 to 40 characters of plain text (no < or >)".to_owned())
    }
}

fn fleet_ref(f: &JoinedFleet) -> FleetRef {
    FleetRef {
        address: f.address(),
        key: f.key,
        fingerprint: f.key.fingerprint(),
        handle: f.handle.clone(),
        event: f.event.clone(),
        hosted: f.hosted,
        portal: f.portal.clone(),
    }
}

/// How the player asked to stop, once they have.
async fn stopped(stop: &mut watch::Receiver<Option<Stop>>) -> Stop {
    let how = stop.wait_for(Option::is_some).await.ok().and_then(|h| *h);
    how.unwrap_or(Stop::Quit)
}

/// One line from the peer, or `None` at a clean end. Cancel-safe: a part line stays in `buf`.
pub(crate) async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<Option<Result<Message, FrameError>>> {
    let room = (MAX_FRAME_BYTES + 2).saturating_sub(buf.len()) as u64;
    (&mut *reader).take(room).read_until(b'\n', buf).await?;
    if buf.last() != Some(&b'\n') {
        if buf.len() > MAX_FRAME_BYTES {
            return Ok(Some(Err(FrameError::TooLong)));
        }
        if buf.is_empty() {
            return Ok(None);
        }
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let message = Message::decode(buf);
    buf.clear();
    Ok(Some(message))
}

pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> io::Result<()> {
    writer.write_all(&message.encode()).await?;
    writer.flush().await
}

struct Session<'a> {
    client: &'a FleetClient,
    target: &'a mut Target,
    binding: Option<TlsBinding>,
    commands: &'a mut mpsc::UnboundedReceiver<Command>,
    buf: Vec<u8>,
}

/// The event this connection is attached to, from `joined`.
struct Attached {
    event: String,
    name: String,
    member: Option<String>,
    handle: String,
    unit: Option<String>,
    grants: Vec<fleet_proto::Grant>,
    card_every_ms: Option<u64>,
}

impl Attached {
    fn from(message: Message) -> Option<Self> {
        let Message::Joined {
            event,
            name,
            member,
            handle,
            unit,
            grants,
            card_every_ms,
        } = message
        else {
            return None;
        };
        Some(Self {
            name: if name.is_empty() { event.clone() } else { name },
            event,
            member,
            handle,
            unit,
            grants,
            card_every_ms,
        })
    }
}

/// A message read during the handshake: a `refused` ends it.
fn handshake_read(read: io::Result<Option<Result<Message, FrameError>>>) -> Result<Message, End> {
    match read {
        Err(e) => Err(net(&e, false)),
        Ok(None) => Err(lost("the server closed the connection", false)),
        Ok(Some(Err(e))) => Err(lost(e, false)),
        Ok(Some(Ok(Message::Refused { reason, code }))) => Err(server_refusal(reason, code)),
        Ok(Some(Ok(m))) => Ok(m),
    }
}

impl Session<'_> {
    /// The next message during the handshake, within [`CONNECT_TIMEOUT`]; a `refused` from the
    /// server ends it.
    async fn handshake_message<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<Message, End> {
        match timeout(CONNECT_TIMEOUT, read_message(reader, &mut self.buf)).await {
            Err(_) => Err(lost("the server did not answer the handshake", false)),
            Ok(read) => handshake_read(read),
        }
    }

    /// Hosted: `connect`, then the event choice when the relay offers several (or none open
    /// yet), until `joined`. The token is spent once the relay answers with either.
    async fn connect<R, W>(&mut self, reader: &mut R, writer: &mut W) -> Result<Attached, End>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let connect = Message::Connect {
            token: self.target.token.clone(),
            event: match self.target.token {
                Some(_) => self.target.event.clone(),
                None => self.target.fleet.event_id.clone(),
            },
        };
        write_message(writer, &connect)
            .await
            .map_err(|e| net(&e, false))?;
        let mut choices: Option<Vec<EventChoice>> = None;
        loop {
            // The player may take their time choosing, so no handshake timeout then.
            let message = if choices.is_some() {
                tokio::select! {
                    read = read_message(reader, &mut self.buf) => handshake_read(read)?,
                    command = self.commands.recv() => {
                        if let Some(Command::Choose(event)) = command {
                            write_message(writer, &Message::Choose { event })
                                .await
                                .map_err(|e| net(&e, false))?;
                        }
                        continue;
                    }
                }
            } else {
                self.handshake_message(reader).await?
            };
            self.target.token = None;
            match message {
                Message::Events { events } => {
                    self.client.publish(|l| l.choices = Some(events.clone()));
                    choices = Some(events);
                }
                joined @ Message::Joined { .. } => {
                    return Attached::from(joined)
                        .ok_or_else(|| lost("the server did not answer the connect", false));
                }
                _ => return Err(lost("the server did not answer the connect", false)),
            }
        }
    }

    /// Peer to peer: `join` with the code, or by key alone.
    async fn join<R, W>(&mut self, reader: &mut R, writer: &mut W) -> Result<Attached, End>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let join = Message::Join {
            code: self.target.code.clone(),
            handle: self.target.fleet.handle.clone(),
        };
        write_message(writer, &join)
            .await
            .map_err(|e| net(&e, false))?;
        let answer = self.handshake_message(reader).await.map_err(|end| {
            // A rejoin by key alone: a server that needs the code again closes on it.
            match (&self.target.code, end) {
                (None, End::Lost { .. }) => {
                    refused("The server wants the join code again: paste the invite to rejoin")
                }
                (_, end) => end,
            }
        })?;
        Attached::from(answer).ok_or_else(|| lost("the server did not answer the join", false))
    }

    async fn run<R, W>(
        mut self,
        reader: &mut R,
        writer: &mut W,
        stop: &mut watch::Receiver<Option<Stop>>,
    ) -> End
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let handshake = tokio::select! {
            r = self.handshake(reader, writer) => r,
            _ = stop.wait_for(Option::is_some) => return End::Stopped(Stop::Quit),
        };
        let card_every = match handshake {
            Ok(every) => every,
            Err(end) => return end,
        };
        self.online(reader, writer, stop, card_every).await
    }

    /// Hello and the challenge both ways, then `join` when the server takes it. Returns the
    /// card rate.
    async fn handshake<R, W>(&mut self, reader: &mut R, writer: &mut W) -> Result<Duration, End>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let client = self.client;
        let ours = Hello::new(SOFTWARE, CAPABILITIES);
        let nonce = Nonce::random().map_err(|e| lost(e, false))?;
        let hello = Message::ClientHello {
            hello: ours.clone(),
            key: client.identity.public(),
            nonce,
        };
        write_message(writer, &hello)
            .await
            .map_err(|e| net(&e, false))?;
        let Message::ServerHello {
            hello,
            server,
            key,
            challenge,
            proof,
        } = self.handshake_message(reader).await?
        else {
            return Err(lost("the server did not say hello", false));
        };
        ours.check(&hello).map_err(refused)?;
        if !hello.has(capability::TLS_BINDING) {
            return Err(refused(
                "The server does not bind its identity to the TLS session: it needs an update",
            ));
        }
        let Some(binding) = self.binding else {
            return Err(refused("The connection has no TLS session to bind to"));
        };
        let answer = ClientHandshake {
            identity: &client.identity,
            nonce,
            pinned: Some(self.target.fleet.key),
            binding: Some(binding),
        }
        .answer(&key, &challenge, &proof, now_ms())
        .map_err(|e| identity_refusal(&e))?;
        write_message(writer, &Message::ClientProof { proof: answer })
            .await
            .map_err(|e| net(&e, false))?;
        let Message::Welcome {
            event,
            capabilities,
        } = self.handshake_message(reader).await?
        else {
            return Err(lost("the server did not welcome this member", false));
        };
        let offers = |c: &str| capabilities.iter().any(|x| x == c);
        let attached = if self.target.fleet.hosted {
            if !offers(capability::CONNECT) {
                return Err(refused(
                    "This server does not take CIC connection codes: it needs an update",
                ));
            }
            // Kept before the relay answers, so a token spent on a connection that drops can
            // be followed by a reconnect by key.
            if let Err(e) = client.fleets.joined(self.target.fleet.clone()) {
                tracing::warn!("[fleet] could not keep the fleet in fleets.json: {e}");
            }
            Some(self.connect(reader, writer).await?)
        } else if offers(capability::JOIN)
            // A relay on fleet-proto 0.1 takes joins but can only say so by offering structure.
            || (offers(capability::STRUCTURE) && !offers(capability::CONNECT))
        {
            Some(self.join(reader, writer).await?)
        } else if offers(capability::CONNECT) {
            return Err(refused(
                "This is a CIC relay: paste a connection code from the CIC portal",
            ));
        } else {
            None
        };
        let (joined, member, handle, grants, every) = match attached {
            Some(a) => (
                EventRef {
                    id: a.event,
                    name: a.name,
                    group: a.unit,
                },
                a.member,
                a.handle,
                a.grants,
                a.card_every_ms.unwrap_or(CARD_EVERY_MS),
            ),
            None => (
                EventRef {
                    id: event.clone(),
                    name: event,
                    group: None,
                },
                None,
                self.target.fleet.handle.clone(),
                vec![],
                CARD_EVERY_MS,
            ),
        };
        let every = Duration::from_millis(every.clamp(MIN_CARD_EVERY_MS, MAX_CARD_EVERY_MS));

        // Spent: reconnects are by key, so a host that restarted answers `session_ended`, not a
        // wrong code. Kept without the code or token, which stay in memory only until spent.
        self.target.code = None;
        self.target.fleet.event =
            Some(joined.name.clone()).filter(|e| fleet_proto::structure::is_name(e));
        if self.target.fleet.hosted {
            self.target.fleet.event_id =
                Some(joined.id.clone()).filter(|e| fleet_proto::structure::is_id(e));
            if is_handle(&handle) {
                self.target.fleet.handle.clone_from(&handle);
            }
        }
        if let Err(e) = client.fleets.joined(self.target.fleet.clone()) {
            tracing::warn!("[fleet] could not keep the fleet in fleets.json: {e}");
        }
        self.target.known = true;
        client.hub.set_member(Some(
            member
                .filter(|m| fleet_proto::card::is_member_id(m))
                .unwrap_or_else(|| client.identity.public().to_string()),
        ));
        tracing::info!(
            "[fleet] joined {} at {} as {handle}",
            server,
            self.target.fleet.address()
        );
        let same_event = lock(&client.link)
            .event
            .as_ref()
            .is_some_and(|e| e.id == joined.id);
        if !same_event {
            client.hub.clear_event();
        }
        let fleet = fleet_ref(&self.target.fleet);
        client.publish(|l| {
            l.state = LinkState::Online;
            l.fleet = Some(fleet);
            l.server = Some(server);
            l.event = Some(joined);
            l.pending = false;
            l.handle = Some(handle);
            l.grants = grants;
            l.problem = None;
            l.hint = None;
            l.code = None;
            l.choices = None;
            l.retry_ms = None;
            l.attempt = 0;
        });
        Ok(every)
    }

    async fn online<R, W>(
        mut self,
        reader: &mut R,
        writer: &mut W,
        stop: &mut watch::Receiver<Option<Stop>>,
        card_every: Duration,
    ) -> End
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let client = self.client;
        let key = client.identity.public().to_string();
        let member = client.hub.member();
        // While connected, follow the game and track this player's state from its events.
        let _following = client.core.follow();
        let life = Arc::new(Mutex::new(LifeTracker::default()));
        let _frames = {
            let life = Arc::clone(&life);
            client
                .core
                .on_frame(move |frame| lock(&life).observe_frame(frame, now_ms()))
        };
        // What the server asks of this sender; every card field until it says.
        let mut wanted = Subscription::of(card_fields().map(|f| f.id));
        let mut subscribed: Option<Subscription> = None;
        let mut tick = tokio::time::interval(card_every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                how = stopped(stop) => return End::Stopped(how),
                Some(Command::Choose(_)) = self.commands.recv() => {
                    tracing::debug!("[fleet] an event choice after joining; ignored");
                }
                read = read_message(reader, &mut self.buf) => {
                    let message = match read {
                        Err(e) => return net(&e, true),
                        Ok(None) => return lost("the server closed the connection", true),
                        Ok(Some(Err(e))) => return lost(e, true),
                        Ok(Some(Ok(m))) => m,
                    };
                    match message {
                        Message::Card { card } => {
                            let own = card.member == key || card.member == member;
                            // A ship card comes only over this, the server connection: a relay
                            // or a peer-to-peer host builds it (0.4.2).
                            if !own && card.check().is_ok() {
                                client.hub.receive(card);
                            }
                        }
                        Message::Structure { structure } => {
                            let problems = structure.problems();
                            if problems.is_empty() {
                                client.hub.set_structure(Some(structure));
                            } else {
                                tracing::warn!("[fleet] structure refused: {}", problems.join("; "));
                            }
                        }
                        Message::Subscribe { subscription } => wanted = subscription,
                        Message::Joined { event, name, handle, unit, .. } => {
                            if lock(&client.link).event.as_ref().is_none_or(|e| e.id != event) {
                                client.hub.clear_event();
                            }
                            client.publish(|l| {
                                l.event = Some(EventRef {
                                    name: if name.is_empty() { event.clone() } else { name },
                                    id: event,
                                    group: unit,
                                });
                                l.handle = Some(handle);
                            });
                        }
                        Message::Roster { you, members } => {
                            client.hub.set_roster(members);
                            client.publish(|l| l.grants = you);
                        }
                        Message::ChangeRefused { reason } => {
                            tracing::debug!("[fleet] a change refused, though none was asked: {reason}");
                        }
                        Message::Events { .. } => {
                            tracing::debug!("[fleet] an event list after joining; ignored");
                        }
                        Message::Change { .. } => {
                            tracing::debug!("[fleet] a structure change; waiting for the structure");
                        }
                        Message::Refused { reason, code } => return server_refusal(reason, code),
                        Message::Bye { reason } => {
                            return lost(format!("the server closed the connection {reason}").trim().to_owned(), true);
                        }
                        other => {
                            tracing::debug!("[fleet] unexpected message {other:?}");
                            return lost("the server sent an unexpected message", true);
                        }
                    }
                }
                _ = tick.tick() => {
                    let sub = client.hub.subscription();
                    if subscribed.as_ref() != Some(&sub) {
                        let message = Message::Subscribe { subscription: sub.clone() };
                        if let Err(e) = write_message(writer, &message).await {
                            return net(&e, true);
                        }
                        subscribed = Some(sub);
                    }
                    let handle = lock(&client.link).handle.clone().unwrap_or_else(|| self.target.fleet.handle.clone());
                    let card = {
                        let mut tracker = lock(&life);
                        let health = health_fraction(
                            &client.core.state().lock().unwrap_or_else(PoisonError::into_inner),
                        );
                        tracker.observe_health(health, now_ms());
                        client.card(&handle, &tracker, &wanted)
                    };
                    // The others know this card by the member id, so the panel does too.
                    client.hub.receive(Card { member: member.clone(), ..card.clone() });
                    if let Err(e) = write_message(writer, &Message::Card { card }).await {
                        return net(&e, true);
                    }
                }
            }
        }
    }
}
