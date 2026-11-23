//! The companion's side: the relay listener (pairing while a code is shown, and the link for the
//! paired game PC), and the bridge from each local page to a stream on the link.
//!
//! A page is bridged for as long as the link it opened on lasts. When the link drops, the
//! bridge answers the page's open requests with "link lost" and closes the page's socket; the
//! page reconnects, shows that the game PC is not there, and is bridged afresh once the link is
//! back. Nothing is queued or replayed, and a hold never carries over.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::io::split;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use fleet_proto::PublicKey;

use super::game::{reader_task, writer_task};
use super::handshake::{self, EXPORTER_LABEL, LinkError, STEP_TIMEOUT};
use super::pairing::{self, Attempts, Paired, Window};
use super::peers::Role;
use super::wire::{MAX_STREAMS, Msg, Reader, Reason, StreamKind};
use super::{LINK_TIMEOUT, LinkState, Mode, PING_EVERY, PairingView, PeerView, Relay, Running};
use crate::portal::policy::TIMED;
use crate::portal::session::{self, HOLD, now_ms};
use crate::tls;
use crate::token::Token;

/// Added to the game PC's hold before a confirmed press goes on, so Wi-Fi jitter between the
/// start and the press does not cut a full hold short. The game PC's own timer is unchanged.
pub const HOLD_MARGIN: Duration = Duration::from_millis(150);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Cmd {
    Open {
        client: String,
        to_page: mpsc::UnboundedSender<String>,
        opened: oneshot::Sender<u32>,
    },
    Data {
        stream: u32,
        text: String,
    },
    Close {
        stream: u32,
    },
}

/// The link to the game PC while it is up.
#[derive(Clone)]
pub struct LinkHandle {
    id: u64,
    peer: String,
    cmds: mpsc::UnboundedSender<Cmd>,
    /// Dropping the sender ends the link.
    _end: Arc<watch::Sender<bool>>,
}

struct Pending {
    paired: Paired,
    from: IpAddr,
    answer: oneshot::Sender<bool>,
}

#[derive(Default)]
pub struct State {
    window: Mutex<Option<Window>>,
    attempts: Mutex<Attempts>,
    pending: Mutex<Option<Pending>>,
    link: Mutex<Option<LinkHandle>>,
    links: AtomicU64,
    /// The port the listener is bound to, when it is (port 0 picks one).
    port: AtomicU16,
}

impl State {
    pub(super) fn pairing_view(&self) -> Option<PairingView> {
        let window = lock(&self.window);
        let window = window.as_ref()?;
        let live = !window.burned() && !window.expired();
        let pending = lock(&self.pending).as_ref().map(|p| PeerView {
            name: super::peers::clean_name(&p.paired.name),
            fingerprint: p.paired.key.fingerprint(),
            last_address: Some(p.from.to_string()),
            paired_at: 0,
        });
        Some(PairingView {
            code: live.then(|| window.code.shown()),
            left_ms: u64::try_from(window.left().as_millis()).unwrap_or(0),
            failures: window.failures,
            burned: window.burned(),
            pending,
        })
    }

    /// Shows a new code, and refreshes the status when it runs out.
    pub(super) fn open_pairing(&self, relay: &Arc<Relay>) -> Result<(), String> {
        if relay.mode() != Mode::Companion {
            return Err("Turn on companion mode to show a pairing code".to_owned());
        }
        let window = Window::open().map_err(|e| e.to_string())?;
        let left = window.left();
        *lock(&self.window) = Some(window);
        self.cancel_pending();
        relay.refresh();
        let relay = Arc::clone(relay);
        tokio::spawn(async move {
            tokio::time::sleep(left).await;
            relay.refresh();
        });
        Ok(())
    }

    pub(super) fn close_pairing(&self) {
        *lock(&self.window) = None;
        self.cancel_pending();
    }

    fn cancel_pending(&self) {
        if let Some(p) = lock(&self.pending).take() {
            let _ = p.answer.send(false);
        }
    }

    /// The companion's user pressed Pair or Cancel.
    pub(super) fn answer(&self, pair: bool) -> Result<(), String> {
        let pending = lock(&self.pending)
            .take()
            .ok_or("No game PC is waiting to pair")?;
        let _ = pending.answer.send(pair);
        Ok(())
    }

    pub(super) fn drop_link(&self) {
        *lock(&self.link) = None;
    }

    pub fn link(&self) -> Option<LinkHandle> {
        lock(&self.link).clone()
    }

    pub(super) fn bound_port(&self) -> Option<u16> {
        Some(self.port.load(Ordering::SeqCst)).filter(|&p| p != 0)
    }
}

/// Binds the listener and serves it until stopped.
pub(super) async fn listen(relay: Arc<Relay>) -> Result<Running, String> {
    let addr = SocketAddr::new(relay.options.bind, relay.options.port);
    let listener = tls::Listener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on port {}: {e}", relay.options.port))?;
    let bound = listener.local_addr().unwrap_or(addr);
    relay.companion.port.store(bound.port(), Ordering::SeqCst);
    tracing::info!("[relay] companion listening on {bound}");
    relay.set_link(
        LinkState::Listening,
        "waiting for the game PC".to_owned(),
        None,
    );
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut stopping = stopped.clone();
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = stopping.changed() => break,
            };
            match accepted {
                Ok((tcp, from)) => {
                    let relay = Arc::clone(&relay);
                    let acceptor = listener.acceptor();
                    let stopped = stopped.clone();
                    tokio::spawn(async move {
                        if let Err(e) = connection(&relay, &acceptor, tcp, from, stopped).await {
                            tracing::info!("[relay] {from}: {e}");
                        }
                    });
                }
                Err(e) => tracing::warn!("[relay] accept: {e}"),
            }
        }
        relay.companion.drop_link();
        relay.companion.port.store(0, Ordering::SeqCst);
    });
    Ok(Running { stop, task })
}

/// One connection: TLS, then pairing or the link, as its first message says.
async fn connection(
    relay: &Arc<Relay>,
    acceptor: &tokio_rustls::TlsAcceptor,
    tcp: tokio::net::TcpStream,
    from: SocketAddr,
    stopped: watch::Receiver<bool>,
) -> Result<(), LinkError> {
    let broken = |e: std::io::Error| LinkError::Broken(e.to_string());
    let stream = tokio::time::timeout(STEP_TIMEOUT, tls::handshake(acceptor, tcp))
        .await
        .map_err(|_| LinkError::Broken("no TLS handshake".into()))?
        .map_err(broken)?;
    let binding = tls::export(stream.get_ref().1, EXPORTER_LABEL).map_err(broken)?;
    let (r, mut out) = split(stream);
    let mut reader = Reader::new(r);
    match handshake::recv(&mut reader).await? {
        Msg::PairHello {
            protocol,
            app,
            name,
            spake,
        } => {
            pair(
                relay,
                &mut reader,
                &mut out,
                (protocol, app, name, spake),
                &binding,
                from.ip(),
            )
            .await
        }
        Msg::RelayHello {
            protocol,
            app,
            key,
            nonce,
            proof: None,
        } => {
            let pinned: Option<PublicKey> = relay.peers.get(Role::Game).map(|p| p.key);
            handshake::accept(
                &mut reader,
                &mut out,
                (protocol, app, key, nonce),
                &relay.identity,
                pinned.as_ref(),
                &relay.options.version,
                &binding,
            )
            .await?;
            run_link(relay, reader, out, stopped).await;
            Ok(())
        }
        _ => {
            let _ = handshake::send(&mut out, &Msg::refused(Reason::Protocol)).await;
            Err(LinkError::Broken("not a relay hello".into()))
        }
    }
}

async fn refuse(
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
    reason: Reason,
) -> Result<(), LinkError> {
    let _ = handshake::send(out, &Msg::refused(reason)).await;
    Err(LinkError::Refused {
        reason,
        game: None,
        companion: None,
    })
}

/// Pairing on the companion: only while a code is shown, one try per address per 5 s.
async fn pair(
    relay: &Arc<Relay>,
    reader: &mut Reader<impl tokio::io::AsyncRead + Unpin>,
    out: &mut (impl tokio::io::AsyncWrite + Unpin),
    hello: (u32, String, String, String),
    binding: &[u8; 32],
    from: IpAddr,
) -> Result<(), LinkError> {
    let state = &relay.companion;
    let code = {
        let window = lock(&state.window);
        match window.as_ref() {
            None => None,
            Some(w) if w.expired() => None,
            Some(w) if w.burned() => Some(Err(Reason::PairingBurned)),
            Some(_) if lock(&state.pending).is_some() => Some(Err(Reason::Busy)),
            Some(_) if !lock(&state.attempts).admit(from) => Some(Err(Reason::Busy)),
            Some(w) => Some(Ok(w.code.clone())),
        }
    };
    let code = match code {
        None => return refuse(out, Reason::PairingClosed).await,
        Some(Err(reason)) => return refuse(out, reason).await,
        Some(Ok(code)) => code,
    };
    let accepted = pairing::accept(
        reader,
        &mut *out,
        hello,
        &relay.identity,
        &relay.options.name,
        &relay.options.version,
        binding,
        &code,
    )
    .await;
    let paired = match accepted {
        Ok(paired) => paired,
        Err(e) => {
            if matches!(
                e,
                LinkError::BadProof
                    | LinkError::Refused {
                        reason: Reason::PairingFailed,
                        ..
                    }
            ) && let Some(w) = lock(&state.window).as_mut()
                && w.failed()
            {
                tracing::warn!("[relay] pairing stopped after 3 wrong codes");
            }
            relay.refresh();
            return Err(e);
        }
    };
    let (answer, answered) = oneshot::channel();
    let left = lock(&state.window)
        .as_ref()
        .map_or(Duration::ZERO, Window::left);
    *lock(&state.pending) = Some(Pending {
        paired: paired.clone(),
        from,
        answer,
    });
    relay.refresh();
    let pressed = tokio::time::timeout(left, answered)
        .await
        .is_ok_and(|a| a.unwrap_or(false));
    lock(&state.pending).take();
    pairing::finish(out, pressed).await?;
    if pressed {
        let peer = relay.paired_now(&paired.name, paired.key, Role::Game, None);
        tracing::info!(
            "[relay] paired with the game PC {} ({})",
            peer.name,
            peer.key.fingerprint()
        );
        if let Err(e) = relay.peers.set(peer) {
            tracing::error!("[relay] the pairing cannot be saved: {e}");
        }
        state.close_pairing();
        state.drop_link();
    }
    relay.refresh();
    Ok(())
}

/// The link from the paired game PC, until it drops, another link replaces it, or the listener
/// stops.
async fn run_link(
    relay: &Arc<Relay>,
    reader: Reader<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    out: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    mut stopped: watch::Receiver<bool>,
) {
    let peer = relay
        .peers
        .get(Role::Game)
        .map_or_else(|| "the game PC".to_owned(), |p| p.name);
    let (mut incoming, read_task) = reader_task(reader);
    let (out, write_task) = writer_task(out);
    let (cmds, mut commands) = mpsc::unbounded_channel();
    let (end, mut ended) = watch::channel(false);
    let id = relay.companion.links.fetch_add(1, Ordering::SeqCst);
    // A new link from the game PC replaces one this side has not seen drop yet.
    *lock(&relay.companion.link) = Some(LinkHandle {
        id,
        peer: peer.clone(),
        cmds,
        _end: Arc::new(end),
    });
    tracing::info!("[relay] showing {peer}");
    relay.set_link(LinkState::Up, format!("showing {peer}"), None);
    let mut streams: HashMap<u32, mpsc::UnboundedSender<String>> = HashMap::new();
    let mut next_stream: u32 = 1;
    let mut tick = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            msg = incoming.recv() => match msg {
                None => break,
                Some(Msg::Data { stream, text }) => {
                    heard = Instant::now();
                    if let Some(to_page) = streams.get(&stream)
                        && to_page.send(text).is_err()
                    {
                        streams.remove(&stream);
                        let _ = out.send(Msg::Close { stream, reason: None });
                    }
                }
                Some(Msg::Close { stream, .. }) => {
                    heard = Instant::now();
                    streams.remove(&stream);
                }
                Some(Msg::Ping { t }) => {
                    heard = Instant::now();
                    let _ = out.send(Msg::Pong { t, at: now_ms() });
                }
                Some(Msg::Pong { .. }) => heard = Instant::now(),
                Some(other) => {
                    tracing::warn!("[relay] unexpected on the link: {other:?}");
                    break;
                }
            },
            Some(cmd) = commands.recv() => match cmd {
                Cmd::Open { client, to_page, opened } => {
                    if streams.len() < MAX_STREAMS {
                        let stream = next_stream;
                        next_stream = next_stream.wrapping_add(1).max(1);
                        streams.insert(stream, to_page);
                        let _ = out.send(Msg::Open { stream, kind: StreamKind::Page, client });
                        let _ = opened.send(stream);
                    }
                }
                Cmd::Data { stream, text } => {
                    if streams.contains_key(&stream) {
                        let _ = out.send(Msg::Data { stream, text });
                    }
                }
                Cmd::Close { stream } => {
                    if streams.remove(&stream).is_some() {
                        let _ = out.send(Msg::Close { stream, reason: None });
                    }
                }
            },
            _ = tick.tick() => if heard.elapsed() > LINK_TIMEOUT { break },
            changed = ended.changed() => if changed.is_err() || *ended.borrow() { break },
            _ = stopped.changed() => break,
        }
    }
    streams.clear();
    read_task.abort();
    drop(out);
    let _ = tokio::time::timeout(Duration::from_millis(200), write_task).await;
    let mut current = lock(&relay.companion.link);
    // Unless a newer link from the game PC took its place.
    if current.as_ref().is_none_or(|l| l.id == id) {
        *current = None;
        drop(current);
        tracing::warn!("[relay] lost the link to {peer}");
        if relay.mode() == Mode::Companion {
            relay.set_link(
                LinkState::Listening,
                "waiting for the game PC".to_owned(),
                None,
            );
        }
    }
}

/// The relay status as a page push, with `via` naming the game PC while linked.
fn relay_push(relay: &Relay, link: Option<&LinkHandle>) -> String {
    let mut push = json!({ "type": "relay" });
    if let (Some(obj), Ok(Value::Object(status))) =
        (push.as_object_mut(), serde_json::to_value(relay.status()))
    {
        obj.extend(status);
        if let Some(link) = link {
            obj.insert("via".into(), json!(link.peer));
        }
    }
    push.to_string()
}

fn error_result(id: &Value, text: &str) -> String {
    json!({ "type": "result", "id": id, "error": text }).to_string()
}

/// Serves one local page in companion mode: bridged to the game PC while the link is up, else
/// told the game PC is not there.
pub async fn bridge(relay: Arc<Relay>, socket: WebSocket, token: Token) {
    let (mut sink, mut stream) = socket.split();
    let Some(client) = session::hello(&mut stream, &token).await else {
        let _ = sink.send(Message::Close(None)).await;
        return;
    };
    let (to_page, mut outgoing) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = outgoing.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });
    let incoming = session::texts(stream);
    let link = relay.companion.link();
    let _ = to_page.send(relay_push(&relay, link.as_ref()));
    match link {
        Some(link) => linked(&relay, link, client, incoming, &to_page).await,
        None => unlinked(&relay, incoming, &to_page).await,
    }
    drop(to_page);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
}

/// A page message's id and type, when it has both.
fn id_and_kind(text: &str) -> Option<(Value, Value, String)> {
    let msg: Value = serde_json::from_str(text).ok()?;
    let id = msg.get("id").filter(|id| id.is_number())?.clone();
    let kind = msg.get("type")?.as_str()?.to_owned();
    Some((msg, id, kind))
}

/// `relay.*` from a page here is this machine's own business; it never goes over the link.
async fn local(relay: &Arc<Relay>, msg: &Value, id: &Value, kind: &str) -> String {
    match relay.page_message(kind, msg).await {
        Ok(result) => json!({ "type": "result", "id": id, "result": result }).to_string(),
        Err(e) => error_result(id, &e),
    }
}

async fn unlinked(
    relay: &Arc<Relay>,
    mut incoming: impl Stream<Item = String> + Unpin,
    to_page: &mpsc::UnboundedSender<String>,
) {
    let mut status = relay.subscribe();
    loop {
        tokio::select! {
            text = incoming.next() => {
                let Some(text) = text else { return };
                let Some((msg, id, kind)) = id_and_kind(&text) else { continue };
                let answer = if kind.starts_with("relay.") {
                    local(relay, &msg, &id, &kind).await
                } else {
                    error_result(&id, "Not connected to the game PC")
                };
                let _ = to_page.send(answer);
            }
            changed = status.changed() => {
                if changed.is_err() || relay.mode() != Mode::Companion {
                    return;
                }
                // Reconnecting gives the page a fresh session on the game PC.
                if relay.companion.link().is_some() {
                    return;
                }
                let _ = to_page.send(relay_push(relay, None));
            }
        }
    }
}

async fn linked(
    relay: &Arc<Relay>,
    link: LinkHandle,
    client: String,
    mut incoming: impl Stream<Item = String> + Unpin,
    to_page: &mpsc::UnboundedSender<String>,
) {
    let (from_link, mut from_game) = mpsc::unbounded_channel::<String>();
    let (opened, stream) = oneshot::channel();
    let open = Cmd::Open {
        client,
        to_page: from_link,
        opened,
    };
    if link.cmds.send(open).is_err() {
        return;
    }
    let Ok(stream) = stream.await else {
        let _ = to_page.send(
            json!({ "type": "notice", "tone": "bad", "text": "Too many pages are open over the link" })
                .to_string(),
        );
        return;
    };
    let mut status = relay.subscribe();
    let mut waiting: HashSet<String> = HashSet::new();
    let mut holds: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::select! {
            text = incoming.next() => {
                let Some(text) = text else {
                    let _ = link.cmds.send(Cmd::Close { stream });
                    return;
                };
                let Some((mut msg, id, kind)) = id_and_kind(&text) else { continue };
                if kind.starts_with("relay.") {
                    let _ = to_page.send(local(relay, &msg, &id, &kind).await);
                    continue;
                }
                let action = msg.get("action").and_then(Value::as_str).map(str::to_owned);
                match (kind.as_str(), action) {
                    ("hold.start", Some(action)) => {
                        holds.insert(action, Instant::now());
                    }
                    ("press", Some(action)) if msg.get("confirmed") == Some(&Value::Bool(true)) => {
                        if let Some(started) = holds.remove(&action) {
                            tokio::time::sleep_until(started + HOLD + HOLD_MARGIN).await;
                        }
                    }
                    _ => {}
                }
                if TIMED.contains(&kind.as_str()) {
                    msg["sentAt"] = json!(now_ms());
                }
                waiting.insert(id.to_string());
                if link.cmds.send(Cmd::Data { stream, text: msg.to_string() }).is_err() {
                    break;
                }
            }
            text = from_game.recv() => {
                let Some(text) = text else { break };
                if text.starts_with("{\"type\":\"result\"")
                    && let Ok(result) = serde_json::from_str::<Value>(&text)
                    && let Some(id) = result.get("id")
                {
                    waiting.remove(&id.to_string());
                }
                let _ = to_page.send(text);
            }
            changed = status.changed() => {
                if changed.is_err() || relay.mode() != Mode::Companion {
                    let _ = link.cmds.send(Cmd::Close { stream });
                    return;
                }
                let current = relay.companion.link();
                if current.as_ref().is_none_or(|l| l.id != link.id) {
                    break;
                }
                let _ = to_page.send(relay_push(relay, current.as_ref()));
            }
        }
    }
    let lost = format!("Lost the link to {}", link.peer);
    for id in waiting {
        let id: Value = serde_json::from_str(&id).unwrap_or(Value::Null);
        let _ = to_page.send(error_result(&id, &lost));
    }
    let _ = to_page.send(relay_push(relay, None));
}
