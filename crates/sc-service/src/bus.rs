//! The local bus: serves one ControlCore to surfaces in other processes (the Stream Deck plugin)
//! over a WebSocket on 127.0.0.1, protocol version 1 as in PROTOCOL.md section 1. Keeps the core
//! following the game while at least one client is connected. A hello may list `capabilities`;
//! the service acts on none of them now and ignores the ones it does not know, so a client that
//! still says `keys` connects as any other and is never asked to press a key.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use sc_core::engine::{OnOff, SequenceError, TargetMode, parse_sequence};
use sc_core::{ControlCore, ListenerGuard};
use sc_game::Subscription;

use crate::busfile::{self, BusFile};
use crate::json::{is_integer, to_js_value, to_text};
use crate::token::Token;

pub const BUS_VERSION: u32 = 1;
/// A message larger than this closes the connection.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;
/// What the socket reads at most. A message between the two caps is read whole, so the client
/// gets a clean 1009 close; a larger one ends the connection mid-message.
const READ_LIMIT: usize = 4 * MAX_MESSAGE_BYTES;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a refused client has to answer the close before the socket is dropped.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
/// Close codes, as the TS server sends them.
const POLICY: u16 = 1008;
const TOO_BIG: u16 = 1009;
/// Pushes a slow client may fall behind by before it misses some.
const PUSH_BUFFER: usize = 1024;

/// Following the game while anyone listens.
#[derive(Default)]
struct Follow {
    clients: usize,
    subscription: Option<Subscription>,
}

struct Shared {
    core: Arc<ControlCore>,
    token: Token,
    pushes: broadcast::Sender<Arc<str>>,
    follow: Mutex<Follow>,
    closing: watch::Receiver<bool>,
}

impl Shared {
    fn follow(&self) -> MutexGuard<'_, Follow> {
        self.follow.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn join(&self) {
        let mut follow = self.follow();
        follow.clients += 1;
        if follow.subscription.is_none() {
            follow.subscription = Some(self.core.follow());
        }
    }

    fn leave(&self) {
        let mut follow = self.follow();
        follow.clients -= 1;
        if follow.clients == 0 {
            follow.subscription = None;
        }
    }
}

fn broadcast(pushes: &broadcast::Sender<Arc<str>>, message: &Value) {
    if pushes.receiver_count() > 0 {
        let _ = pushes.send(to_text(message).into());
    }
}

pub struct BusServer {
    pub port: u16,
    token: Token,
    bus_file: PathBuf,
    shared: Arc<Shared>,
    close: watch::Sender<bool>,
    task: JoinHandle<()>,
    _listeners: (ListenerGuard, Subscription, ListenerGuard),
}

impl BusServer {
    /// Bind on 127.0.0.1 (any free port), then advertise it in `bus_file`.
    pub async fn start(core: Arc<ControlCore>, bus_file: PathBuf) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let token = Token::generate();
        let (close, closing) = watch::channel(false);
        let (pushes, _) = broadcast::channel(PUSH_BUFFER);

        let (frames, links, progress) = (pushes.clone(), pushes.clone(), pushes.clone());
        let listeners = (
            core.on_frame(move |frame| {
                broadcast(&frames, &json!({ "type": "state", "frame": frame }))
            }),
            core.on_link_status(move |status| {
                broadcast(&links, &json!({ "type": "link", "status": status }))
            }),
            core.on_progress(move |p| {
                broadcast(&progress, &json!({ "type": "progress", "progress": p }))
            }),
        );
        let shared = Arc::new(Shared {
            core,
            token: token.clone(),
            pushes,
            follow: Mutex::default(),
            closing: closing.clone(),
        });
        let app = Router::new()
            .fallback(upgrade)
            .with_state(Arc::clone(&shared));
        let mut stop = closing;
        let task = tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = stop.wait_for(|closed| *closed).await;
            });
            if let Err(e) = serve.await {
                tracing::error!("bus server stopped: {e}");
            }
        });
        let server = Self {
            port,
            token,
            bus_file,
            shared,
            close,
            task,
            _listeners: listeners,
        };
        // Only advertise once bound, so a client never reads a port nobody listens on.
        let file = BusFile {
            port,
            token: server.token.as_str().to_owned(),
            pid: std::process::id(),
            version: BUS_VERSION,
        };
        if let Err(e) = busfile::write(&server.bus_file, &file) {
            server.close().await;
            return Err(e);
        }
        Ok(server)
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    /// Stop serving, drop every client and remove the bus file if it is still ours.
    pub async fn close(self) {
        let _ = self.close.send(true);
        self.shared.follow().subscription = None;
        busfile::remove(&self.bus_file, self.token.as_str());
        let _ = self.task.await;
    }
}

async fn upgrade(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    match ws {
        Ok(ws) if peer.ip().is_loopback() => ws
            .max_message_size(READ_LIMIT)
            .max_frame_size(READ_LIMIT)
            .on_upgrade(move |socket| connection(socket, shared)),
        Ok(_) => StatusCode::FORBIDDEN.into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Why a hello is refused, if it is.
fn check_hello(message: Option<&Value>, token: &Token) -> Option<String> {
    let message = message.filter(|m| m.get("type").and_then(Value::as_str) == Some("hello"));
    let Some(message) = message else {
        return Some("expected hello".to_owned());
    };
    if !message
        .get("token")
        .and_then(Value::as_str)
        .is_some_and(|t| token.matches(t))
    {
        return Some("wrong token".to_owned());
    }
    if message.get("version").and_then(Value::as_f64) != Some(f64::from(BUS_VERSION)) {
        return Some(format!(
            "unsupported version, this bus is version {BUS_VERSION}"
        ));
    }
    None
}

/// A text or binary message as JSON; binary messages are never valid.
fn parse(message: &Message) -> Option<Value> {
    let Message::Text(text) = message else {
        return None;
    };
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(Value::is_object)
}

/// The read side's next message, or why the connection ends.
enum Incoming {
    Message(Message),
    TooBig,
    Gone,
}

fn incoming(frame: Option<Result<Message, axum::Error>>) -> Incoming {
    match frame {
        Some(Ok(Message::Text(t))) if t.len() > MAX_MESSAGE_BYTES => Incoming::TooBig,
        Some(Ok(Message::Binary(b))) if b.len() > MAX_MESSAGE_BYTES => Incoming::TooBig,
        Some(Ok(message)) => Incoming::Message(message),
        Some(Err(e)) if is_capacity(&e) => Incoming::TooBig,
        _ => Incoming::Gone,
    }
}

fn is_capacity(e: &axum::Error) -> bool {
    let source = std::error::Error::source(e);
    matches!(
        source.and_then(|s| s.downcast_ref::<tungstenite::Error>()),
        Some(tungstenite::Error::Capacity(_))
    )
}

/// Read until the client answers the close (or gives up), so it sees the close frame rather than
/// a reset while it is still sending.
async fn drain(stream: &mut futures_util::stream::SplitStream<WebSocket>) {
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(message)) = stream.next().await {
            if matches!(message, Message::Close(_)) {
                return;
            }
        }
    })
    .await;
}

async fn connection(socket: WebSocket, shared: Arc<Shared>) {
    let (mut sink, mut stream) = socket.split();
    let first = match tokio::time::timeout(HELLO_TIMEOUT, stream.next()).await {
        Err(_) => {
            let _ = sink.send(close_frame(POLICY, "no hello")).await;
            return;
        }
        Ok(frame) => incoming(frame),
    };
    let first = match first {
        Incoming::Message(Message::Close(_)) | Incoming::Gone => return,
        Incoming::TooBig => {
            let _ = sink.send(close_frame(TOO_BIG, "")).await;
            drain(&mut stream).await;
            return;
        }
        Incoming::Message(message) => message,
    };
    let hello = parse(&first);
    if let Some(reason) = check_hello(hello.as_ref(), &shared.token) {
        let refused = to_text(&json!({ "type": "refused", "reason": reason }));
        let _ = sink.send(Message::Text(refused.into())).await;
        let _ = sink.send(close_frame(POLICY, &reason)).await;
        drain(&mut stream).await;
        return;
    }

    // Subscribed before the welcome is built, so nothing between the two is lost; anything older
    // that arrives after the welcome is followed by what replaced it.
    let mut pushes = shared.pushes.subscribe();
    shared.join();
    let core = &shared.core;
    let welcome = {
        let state = core.state().lock().unwrap_or_else(PoisonError::into_inner);
        json!({ "type": "welcome", "sections": to_js_value(&state.snapshot()), "link": core.link_status(), "running": core.running() })
    };
    let mut alive = sink
        .send(Message::Text(to_text(&welcome).into()))
        .await
        .is_ok();

    let (results, mut answered) = mpsc::unbounded_channel::<String>();
    let mut closing = shared.closing.clone();
    while alive {
        tokio::select! {
            frame = stream.next() => match incoming(frame) {
                Incoming::Message(message @ (Message::Text(_) | Message::Binary(_))) => {
                    call(&shared, &message, &results);
                }
                Incoming::Message(Message::Close(_)) | Incoming::Gone => alive = false,
                Incoming::Message(_) => {}
                Incoming::TooBig => {
                    let _ = sink.send(close_frame(TOO_BIG, "")).await;
                    drain(&mut stream).await;
                    alive = false;
                }
            },
            push = pushes.recv() => match push {
                Ok(text) => alive = sink.send(Message::Text(text.as_ref().into())).await.is_ok(),
                Err(broadcast::error::RecvError::Lagged(missed)) => tracing::warn!("a bus client fell behind and missed {missed} pushes"),
                Err(broadcast::error::RecvError::Closed) => alive = false,
            },
            Some(text) = answered.recv() => alive = sink.send(Message::Text(text.into())).await.is_ok(),
            _ = closing.changed() => alive = false,
        }
    }
    shared.leave();
}

/// Start one call; its result is sent when it is done, so a long runSequence does not hold up
/// replies to other calls.
fn call(shared: &Arc<Shared>, message: &Message, results: &mpsc::UnboundedSender<String>) {
    let Some(message) = parse(message) else {
        return;
    };
    let Some(id) = message.get("id").filter(|id| is_integer(id)).cloned() else {
        return;
    };
    if message.get("type").and_then(Value::as_str) != Some("call") {
        return;
    }
    let shared = Arc::clone(shared);
    let results = results.clone();
    tokio::spawn(async move {
        let answer = match dispatch(&shared.core, message.get("call")).await {
            Ok(result) => json!({ "type": "result", "id": id, "result": result }),
            Err(error) => json!({ "type": "result", "id": id, "error": error }),
        };
        let _ = results.send(to_text(&answer));
    });
}

fn string<'a>(call: &'a Value, name: &str) -> Result<&'a str, String> {
    call.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string"))
}

fn integer(call: &Value, name: &str) -> Result<i64, String> {
    call.get(name)
        .filter(|v| is_integer(v))
        .and_then(Value::as_f64)
        .map(|f| f as i64)
        .ok_or_else(|| format!("{name} must be an integer"))
}

/// Only the shape; the store checks the content and answers with the problems.
fn sequence_shape(value: Option<&Value>) -> Result<&Value, String> {
    let Some(seq) = value.filter(|v| v.is_object()) else {
        return Err("sequence must be an object".to_owned());
    };
    let shaped = seq.get("id").is_some_and(Value::is_string)
        && seq.get("name").is_some_and(Value::is_string)
        && seq.get("steps").is_some_and(Value::is_array);
    if !shaped {
        return Err("sequence needs a string id, a string name and a steps array".to_owned());
    }
    if !seq["steps"]
        .as_array()
        .is_some_and(|steps| steps.iter().all(Value::is_object))
    {
        return Err("every step must be an object".to_owned());
    }
    Ok(seq)
}

async fn dispatch(core: &ControlCore, raw: Option<&Value>) -> Result<Value, String> {
    let Some(call) = raw.filter(|c| c.is_object()) else {
        return Err("call must be an object".to_owned());
    };
    let store = core.sequences();
    let io = |e: io::Error| e.to_string();
    let op = call.get("op");
    Ok(match op.and_then(Value::as_str) {
        Some("press") => {
            let confirmed = call.get("confirmed") == Some(&Value::Bool(true));
            to_js_value(&core.press(string(call, "action")?, confirmed).await)
        }
        Some("set") => {
            let to = match call.get("to").and_then(Value::as_str) {
                Some("on") => OnOff::On,
                Some("off") => OnOff::Off,
                _ => return Err("to must be on or off".to_owned()),
            };
            to_js_value(&core.set(string(call, "toggle")?, to).await)
        }
        Some("setMode") => {
            let to = match call.get("to").and_then(Value::as_str) {
                Some("NAV") => TargetMode::Nav,
                Some("SCM") => TargetMode::Scm,
                _ => return Err("to must be NAV or SCM".to_owned()),
            };
            to_js_value(&core.set_mode(to).await)
        }
        Some("runSequence") => to_js_value(&core.run_sequence(string(call, "id")?).await),
        Some("abort") => Value::Bool(core.abort()),
        Some("actions") => to_js_value(&core.actions().await),
        Some("sequences.list") => to_js_value(&store.list()),
        Some("sequences.save") => match parse_sequence(sequence_shape(call.get("sequence"))?) {
            Ok(seq) => to_js_value(&store.save(&seq).map_err(io)?),
            Err(SequenceError::Invalid(problems)) => to_js_value(&problems),
            Err(e @ SequenceError::Malformed(_)) => return Err(e.to_string()),
        },
        Some("sequences.remove") => Value::Bool(store.remove(string(call, "id")?).map_err(io)?),
        Some("sequences.duplicate") => {
            to_js_value(&store.duplicate(string(call, "id")?).map_err(io)?)
        }
        Some("sequences.moveStep") => {
            let id = string(call, "id")?;
            let (from, to) = (integer(call, "from")?, integer(call, "to")?);
            match (usize::try_from(from), usize::try_from(to)) {
                (Ok(from), Ok(to)) => Value::Bool(store.move_step(id, from, to).map_err(io)?),
                _ => Value::Bool(false),
            }
        }
        Some("sequences.reset") => Value::Bool(store.reset(string(call, "id")?).map_err(io)?),
        _ => {
            let shown = op.map_or_else(|| "undefined".to_owned(), Value::to_string);
            return Err(format!("unknown op {shown}"));
        }
    })
}
