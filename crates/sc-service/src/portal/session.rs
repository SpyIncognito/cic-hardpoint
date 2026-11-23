//! One open portal or dashboard page: the service pushes the ship status (and, once the page asked
//! for the dashboard, its layout and values), and runs what the page asks through the same core
//! as the deck. The page never talks to the game itself. Messages from one page run in order,
//! except that `run` does not hold up the next one.
//!
//! Every page message goes through [`Session::run`], which applies the session's [`Policy`]: a
//! page this machine served, or a companion's page carried over the relay link. A session runs
//! over any stream of text messages ([`serve_stream`]); [`serve`] is the local page's WebSocket.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, Stream, StreamExt, future};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use sc_core::engine::{
    ActionResult, CONFIRM_GROUP, FailStatus, OnOff, SequenceError, TargetMode, parse_sequence,
};
use sc_core::js;

use super::policy::Policy;
use super::status::build_status;
use super::{catalog, known};
use crate::dashboard::{
    BoardInfo, CELLS, COLUMNS, DEFAULT_CELL, ImportSummary, KindTable, LEVER_SIZE, LookOnlyFor,
    MAX_BEZEL_BUTTONS, MAX_BOARDS, MAX_BUTTON_INDEX, MAX_DISPLAY_LABEL, MAX_GAMEPAD_ID, MAX_LABEL,
    MAX_ROWS, MAX_TABS, MAX_WIDGETS, MIN_ROWS, build_values_with, check_ship, lamp_values,
    read_dashboard,
};
use crate::json::{js_string, to_js_value, to_pretty_text, to_text};
use crate::relay::Mode;
use crate::service::App;
use crate::ship::ShipView;
use crate::ship::support::{FIELDS, SupportEntry};
use crate::token::Token;

/// A confirm-group press counts only after the page held it this long, timed here, not by the page.
pub const HOLD: Duration = Duration::from_millis(1500);
const HOLD_MAX: Duration = Duration::from_secs(10);
/// Actions a page may send per second; a looping page must not flood the engine.
pub const MAX_ACTIONS_PER_S: usize = 5;
const ACTION_TYPES: &[&str] = &["press", "set", "mode", "run"];
const PUSH_EVERY: Duration = Duration::from_millis(200);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const FLEET_AT_LEAST_EVERY: Duration = Duration::from_secs(1);
/// The largest dashboard file a page may import, in UTF-16 code units as the TS counts it.
pub const MAX_IMPORT_UNITS: usize = 512 * 1024;
const MAX_HOLDS: usize = 20;

/// A push: `{ type, ...body }`.
#[derive(Serialize)]
struct Push<'a, T> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(flatten)]
    body: T,
}

/// Each greyed id and why, as an object keyed by id.
struct SupportMap(Vec<(&'static str, SupportEntry)>);

impl Serialize for SupportMap {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (id, entry) in &self.0 {
            map.serialize_entry(id, entry)?;
        }
        map.end()
    }
}

/// The values model, with the ship and what it lacks.
#[derive(Serialize)]
struct ValuesPush<T> {
    #[serde(flatten)]
    model: T,
    /// The ship while the player is in it and the report is fresh, else null.
    ship: Option<ShipView>,
    /// The ids whose function this ship lacks: `{ state: 'absent' | 'unavailable', reason }`.
    support: SupportMap,
    /// Sent only while the app records telemetry.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    recording: bool,
}

/// Sends a model at most every [`PUSH_EVERY`], and only when it changed.
#[derive(Default)]
struct Throttle {
    last_sent: String,
    last_at: Option<Instant>,
}

#[derive(Default)]
struct Pushes {
    status: Throttle,
    values: Throttle,
    fleet: Throttle,
}

#[derive(Default)]
struct Limits {
    holds: HashMap<String, Instant>,
    recent: VecDeque<Instant>,
    /// The import this page was shown the summary for; only that exact file can then be applied.
    shown_import: Option<String>,
}

pub struct Session {
    app: Arc<App>,
    client: String,
    policy: Policy,
    out: mpsc::UnboundedSender<String>,
    /// A page that shows a dashboard asks for it once (`dash.get`); only then do values and
    /// layouts follow.
    dash_page: AtomicBool,
    limits: Mutex<Limits>,
    pushes: Mutex<Pushes>,
    /// This page's key for its fleet detail view.
    fleet_viewer: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The first message must be the hello with the token, within [`HELLO_TIMEOUT`]; the client name
/// it gives, or `None` to close.
pub(crate) async fn hello(stream: &mut SplitStream<WebSocket>, token: &Token) -> Option<String> {
    let first = async {
        loop {
            match stream.next().await?.ok()? {
                Message::Text(t) => return Some(t.to_string()),
                Message::Binary(b) => return String::from_utf8(b.to_vec()).ok(),
                Message::Close(_) => return None,
                Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    };
    let text = tokio::time::timeout(HELLO_TIMEOUT, first).await.ok()??;
    let msg: Value = serde_json::from_str(&text).ok()?;
    let token_ok = msg
        .get("token")
        .and_then(Value::as_str)
        .is_some_and(|t| token.matches(t));
    if msg.get("type").and_then(Value::as_str) != Some("hello") || !token_ok {
        return None;
    }
    Some(
        msg.get("client")
            .and_then(Value::as_str)
            .map_or_else(|| "portal".to_owned(), |c| js::utf16_prefix(c, 64)),
    )
}

/// The page's text messages until it closes.
pub(crate) fn texts(stream: SplitStream<WebSocket>) -> impl Stream<Item = String> + Unpin + Send {
    stream
        .take_while(|frame| {
            future::ready(matches!(frame, Ok(m) if !matches!(m, Message::Close(_))))
        })
        .filter_map(|frame| {
            future::ready(match frame {
                Ok(Message::Text(t)) => Some(t.to_string()),
                Ok(Message::Binary(b)) => String::from_utf8(b.to_vec()).ok(),
                _ => None,
            })
        })
}

/// Serve one local page's WebSocket until it closes or the service stops.
pub async fn serve(socket: WebSocket, app: Arc<App>) {
    let (mut sink, mut stream) = socket.split();
    let Some(client) = hello(&mut stream, &app.token).await else {
        let _ = sink.send(Message::Close(None)).await;
        return;
    };
    let (out, mut outgoing) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(text) = outgoing.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
    });
    serve_stream(app, client, Policy::Local, texts(stream), out).await;
    writer.abort();
}

/// Serve one page over any stream of text messages, sending to `out`, until the stream ends or
/// the service stops. The page is already known (by token here, or by the relay link).
pub async fn serve_stream(
    app: Arc<App>,
    client: String,
    policy: Policy,
    mut incoming: impl Stream<Item = String> + Unpin + Send,
    out: mpsc::UnboundedSender<String>,
) {
    let session = Arc::new(Session {
        app: Arc::clone(&app),
        client,
        policy,
        out,
        dash_page: AtomicBool::new(false),
        limits: Mutex::default(),
        pushes: Mutex::default(),
        fleet_viewer: app.fleet.viewer(),
    });

    let _follow = app.core.follow();
    let changed = Arc::new(Notify::new());
    let on_change = Arc::clone(&changed);
    let _on_change = app.core.on_change(move || on_change.notify_one());
    let mut pushers = vec![
        tokio::spawn(push_models(Arc::clone(&session), changed)),
        tokio::spawn(push_flag(Arc::clone(&session), app.recording.subscribe())),
        tokio::spawn(push_on(Arc::clone(&session), app.ships.subscribe())),
        tokio::spawn(push_dashboards(
            Arc::clone(&session),
            app.dashboards.subscribe(),
        )),
        tokio::spawn(push_themes(Arc::clone(&session), app.themes.subscribe())),
        tokio::spawn(push_time(Arc::clone(&session), app.time.subscribe())),
        tokio::spawn(push_ship_names(
            Arc::clone(&session),
            app.ship_names.subscribe(),
        )),
    ];
    pushers.push(tokio::spawn(push_fleet_layouts(
        Arc::clone(&session),
        app.fleet_layouts.subscribe(),
    )));
    pushers.push(tokio::spawn(push_fleet(
        Arc::clone(&session),
        app.fleet.subscribe(),
        app.fleet.view().subscribe(),
    )));
    let local = !session.policy.is_companion();
    if local {
        pushers.push(tokio::spawn(push_relay(Arc::clone(&session))));
    }
    session.send_catalog();
    session.push_status(true);

    let mut closing = app.closing.clone();
    let mut relay = app.relay.subscribe();
    loop {
        let text = tokio::select! {
            text = incoming.next() => text,
            _ = closing.changed() => break,
            // A page here ends when this machine becomes a companion; it comes back bridged.
            changed = relay.changed(), if local => {
                if changed.is_err() || relay.borrow().mode == Mode::Companion {
                    break;
                }
                continue;
            }
        };
        let Some(text) = text else { break };
        session.handle(&text).await;
    }
    pushers.iter().for_each(JoinHandle::abort);
    app.fleet.set_detail(session.fleet_viewer, None);
}

/// The status and values, throttled, after every core change.
async fn push_models(session: Arc<Session>, changed: Arc<Notify>) {
    loop {
        changed.notified().await;
        session.push_models().await;
    }
}

/// The values again when recording starts or stops, for the `recording` flag.
async fn push_flag(session: Arc<Session>, mut flag: watch::Receiver<bool>) {
    while flag.changed().await.is_ok() {
        session.push_values(true);
    }
}

/// The values again when the ship or its greyed functions change.
async fn push_on(session: Arc<Session>, mut changes: watch::Receiver<u64>) {
    while changes.changed().await.is_ok() {
        session.push_values(true);
    }
}

/// The layout, to every open dashboard page, whenever the dashboard changes.
async fn push_dashboards(session: Arc<Session>, mut dashboards: watch::Receiver<u64>) {
    while dashboards.changed().await.is_ok() {
        if session.is_dash_page() {
            session.push_dashboard();
        }
    }
}

/// The themes, to every open dashboard page, whenever a user theme changes.
async fn push_themes(session: Arc<Session>, mut themes: watch::Receiver<u64>) {
    while themes.changed().await.is_ok() {
        if session.is_dash_page() {
            session.push_themes();
        }
    }
}

/// The time settings, to every open dashboard page, whenever they change.
async fn push_time(session: Arc<Session>, mut time: watch::Receiver<u64>) {
    while time.changed().await.is_ok() {
        if session.is_dash_page() {
            session.send(&session.app.time.push());
        }
    }
}

/// The ship names, and the picker that shows them, to every open dashboard page when one changes.
async fn push_ship_names(session: Arc<Session>, mut names: watch::Receiver<u64>) {
    while names.changed().await.is_ok() {
        if session.is_dash_page() {
            session.send(&session.app.ship_names.push());
            session.push_dashboard();
        }
    }
}

/// The fleet panel, to every open dashboard page, when a card arrives or the viewer's groups
/// change, and at least every second so ages and OFFLINE stay current.
async fn push_fleet(
    session: Arc<Session>,
    mut cards: watch::Receiver<u64>,
    mut view: watch::Receiver<u64>,
) {
    loop {
        tokio::select! {
            changed = cards.changed() => if changed.is_err() { return },
            changed = view.changed() => if changed.is_err() { return },
            () = tokio::time::sleep(FLEET_AT_LEAST_EVERY) => {}
        }
        while let Some(wait) = session.push_fleet(false) {
            tokio::time::sleep(wait).await;
        }
    }
}

/// A note to every open dashboard page that a Fleet tab layout was saved, so a page showing the
/// Fleet tab (here or on a companion) asks for it again.
async fn push_fleet_layouts(session: Arc<Session>, mut layouts: watch::Receiver<u64>) {
    while layouts.changed().await.is_ok() {
        if session.is_dash_page() {
            session.send(&json!({ "type": "fleetLayout" }));
        }
    }
}

/// The relay status, to every local dashboard page, whenever it changes.
async fn push_relay(session: Arc<Session>) {
    let mut status = session.app.relay.subscribe();
    while status.changed().await.is_ok() {
        if session.is_dash_page() {
            session.push_relay();
        }
    }
}

/// `msg.<name>` as a string, or the error the TS would have thrown on.
fn string_field<'a>(msg: &'a Value, name: &str) -> Result<&'a str, String> {
    msg.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string"))
}

/// What the join screen shows before anything is sent: where the invite points, the server's
/// key fingerprint, whether it is a CIC relay, and a host's name as the invite labels it.
fn inspect_invite(text: &str) -> Value {
    match text.parse::<fleet_proto::Invite>() {
        Err(e) => json!({ "problems": [e.to_string()] }),
        Ok(i) => json!({ "problems": [], "invite": {
            "address": if i.host.contains(':') { format!("[{}]:{}", i.host, i.port) } else { format!("{}:{}", i.host, i.port) },
            "fingerprint": i.key.fingerprint(),
            "key": i.key.to_string(),
            "hosted": i.connect.is_some(),
            "hostName": crate::fleet::host::host_name_of(text),
        } }),
    }
}

fn io_error(e: std::io::Error) -> String {
    e.to_string()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Session {
    fn send(&self, msg: &impl Serialize) {
        let _ = self.out.send(to_text(msg));
    }

    fn is_dash_page(&self) -> bool {
        self.dash_page.load(Ordering::SeqCst)
    }

    /// Parse one page message and answer it.
    async fn handle(self: &Arc<Self>, text: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let Some(id) = msg.get("id").filter(|id| id.is_number()).cloned() else {
            return;
        };
        let Some(kind) = msg.get("type").and_then(Value::as_str).map(str::to_owned) else {
            return;
        };
        let detail = log_detail(&msg);
        // A long sequence, or pairing that waits for the companion's user, must not block other
        // messages from this page (an abort, say).
        let is_run = kind == "run" || kind == "relay.pair";
        let work = self.run(kind.clone(), msg);
        let session = Arc::clone(self);
        let answer = async move {
            let outcome = work.await;
            session.respond(&kind, id, &detail, outcome);
        };
        if is_run {
            tokio::spawn(answer);
        } else {
            answer.await;
        }
    }

    /// Every action from a page goes through here. The rate cap is taken when the message
    /// arrives, before the returned future runs.
    fn run(
        self: &Arc<Self>,
        kind: String,
        msg: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send + 'static {
        let refused = self.policy.refusal(&kind, &msg, now_ms());
        let capped = refused.is_none() && ACTION_TYPES.contains(&kind.as_str()) && !self.admit();
        let session = Arc::clone(self);
        async move {
            if let Some(refused) = refused {
                return Ok(refused);
            }
            if capped {
                let refused = ActionResult::fail(
                    FailStatus::Unknown,
                    "Too many actions from this page; try again in a moment",
                );
                return Ok(to_js_value(&refused));
            }
            session.dispatch(&kind, &msg).await
        }
    }

    /// Counts an action against the per-second cap; false when over it.
    fn admit(&self) -> bool {
        let mut limits = lock(&self.limits);
        let now = Instant::now();
        while limits
            .recent
            .front()
            .is_some_and(|&t| now - t > Duration::from_secs(1))
        {
            limits.recent.pop_front();
        }
        if limits.recent.len() >= MAX_ACTIONS_PER_S || !self.policy.admit() {
            return false;
        }
        limits.recent.push_back(now);
        true
    }

    async fn dispatch(&self, kind: &str, msg: &Value) -> Result<Value, String> {
        let core = &self.app.core;
        let store = core.sequences();
        let dashboards = &self.app.dashboards;
        let themes = &self.app.themes;
        let seq_id = || {
            msg.get("sequence")
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let tab_id = || js_string(msg.get("tab"));
        let board_id = || js_string(msg.get("dashboard"));
        Ok(match kind {
            "hold.start" => {
                let mut limits = lock(&self.limits);
                if limits.holds.len() >= MAX_HOLDS {
                    limits.holds.clear();
                }
                limits
                    .holds
                    .insert(js_string(msg.get("action")), Instant::now());
                Value::Null
            }
            "hold.end" => {
                if let Some(action) = msg.get("action").and_then(Value::as_str) {
                    lock(&self.limits).holds.remove(action);
                }
                Value::Null
            }
            "press" => {
                let action = string_field(msg, "action")?;
                let confirmed = msg.get("confirmed").is_some_and(js::truthy);
                to_js_value(&self.press(action, confirmed).await)
            }
            "set" => {
                let toggle = string_field(msg, "toggle")?;
                let to = match msg.get("to").and_then(Value::as_str) {
                    Some("on") => OnOff::On,
                    Some("off") => OnOff::Off,
                    _ => return Err("to must be on or off".to_owned()),
                };
                to_js_value(&core.set(toggle, to).await)
            }
            "mode" => {
                let to = match msg.get("to").and_then(Value::as_str) {
                    Some("NAV") => TargetMode::Nav,
                    Some("SCM") => TargetMode::Scm,
                    _ => return Err("to must be NAV or SCM".to_owned()),
                };
                to_js_value(&core.set_mode(to).await)
            }
            "run" => to_js_value(&core.run_sequence(string_field(msg, "sequence")?).await),
            "abort" => Value::Bool(core.abort()),
            "seq.save" => match parse_sequence(msg.get("sequence").unwrap_or(&Value::Null)) {
                Ok(seq) => to_js_value(&store.save(&seq).map_err(io_error)?),
                Err(SequenceError::Invalid(problems)) => to_js_value(&problems),
                Err(e @ SequenceError::Malformed(_)) => return Err(e.to_string()),
            },
            "seq.remove" => Value::Bool(store.remove(seq_id()).map_err(io_error)?),
            "seq.duplicate" => to_js_value(&store.duplicate(seq_id()).map_err(io_error)?),
            "seq.reset" => Value::Bool(store.reset(seq_id()).map_err(io_error)?),
            "dash.get" => {
                self.dash_page.store(true, Ordering::SeqCst);
                self.push_values(true);
                to_js_value(&dashboards.get())
            }
            "dash.saveTab" => {
                let known = known(core, dashboards, themes).await;
                to_js_value(
                    &dashboards
                        .save_tab(msg.get("tab").unwrap_or(&Value::Null), &known)
                        .map_err(io_error)?,
                )
            }
            "dash.addTab" => {
                let known = known(core, dashboards, themes).await;
                to_js_value(
                    &dashboards
                        .add_tab_in(msg.get("name"), msg.get("theme"), &known)
                        .map_err(io_error)?,
                )
            }
            "dash.removeTab" => to_js_value(&dashboards.remove_tab(&tab_id()).map_err(io_error)?),
            "dash.duplicateTab" => {
                to_js_value(&dashboards.duplicate_tab(&tab_id()).map_err(io_error)?)
            }
            "dash.moveTab" => to_js_value(
                &dashboards
                    .move_tab(&tab_id(), msg.get("to"))
                    .map_err(io_error)?,
            ),
            "dash.setPlacement" => to_js_value(
                &dashboards
                    .set_placement(&tab_id(), msg.get("placement"))
                    .map_err(io_error)?,
            ),
            "dash.setBezel" => {
                let known = known(core, dashboards, themes).await;
                to_js_value(
                    &dashboards
                        .set_bezel(&tab_id(), msg.get("bezel"), &known)
                        .map_err(io_error)?,
                )
            }
            "dash.resetStarter" => to_js_value(&dashboards.reset_starter().map_err(io_error)?),
            "dash.export" => {
                json!({ "name": "sc-dashboard.json", "text": to_pretty_text(&dashboards.get()) })
            }
            "dash.import" => {
                self.import(
                    msg.get("text"),
                    msg.get("confirm") == Some(&Value::Bool(true)),
                )
                .await?
            }
            "dash.setTheme" => {
                let known = known(core, dashboards, themes).await;
                to_js_value(
                    &dashboards
                        .set_theme(msg.get("theme"), &known)
                        .map_err(io_error)?,
                )
            }
            "dash.setCell" => to_js_value(&dashboards.set_cell(msg.get("cell")).map_err(io_error)?),
            "theme.list" => themes.model(),
            "theme.save" => to_js_value(
                &themes
                    .save(msg.get("theme").unwrap_or(&Value::Null))
                    .map_err(io_error)?,
            ),
            "theme.duplicate" => to_js_value(
                &themes
                    .duplicate(&js_string(msg.get("theme")), msg.get("name"))
                    .map_err(io_error)?,
            ),
            "theme.remove" => {
                let id = js_string(msg.get("theme"));
                let change = themes.remove(&id).map_err(io_error)?;
                if change.problems.is_empty() {
                    dashboards.drop_theme(&id).map_err(io_error)?;
                }
                to_js_value(&change)
            }
            "dash.list" => {
                let mut list = dashboards.list();
                self.named(&mut list.dashboards);
                to_js_value(&list)
            }
            "dash.select" => {
                let change = dashboards.select(&board_id(), true).map_err(io_error)?;
                if change.problems.is_empty() {
                    self.app.ships.picked();
                }
                to_js_value(&change)
            }
            "dash.createForShip" => {
                let ship = match msg.get("ship") {
                    None | Some(Value::Null) => {
                        self.app.ships.snapshot().ship.map(|s| s.ship_ref())
                    }
                    raw => match check_ship(raw) {
                        Ok(s) => Some(s),
                        Err(problems) => return Ok(json!({ "problems": problems })),
                    },
                };
                let Some(ship) = ship else {
                    return Ok(
                        json!({ "problems": ["Not in a ship: get in one, or name the ship"] }),
                    );
                };
                let known = known(core, dashboards, themes).await;
                let from = msg.get("from").and_then(Value::as_str);
                let users_name = ship
                    .class_name
                    .as_deref()
                    .and_then(|c| self.app.ship_names.name_of(c))
                    .map(Value::String);
                let name = msg.get("name").or(users_name.as_ref());
                let change = dashboards
                    .create_for_ship(ship, from, name, &known)
                    .map_err(io_error)?;
                self.app.ships.picked();
                to_js_value(&change)
            }
            "dash.duplicate" => to_js_value(
                &dashboards
                    .duplicate(&board_id(), msg.get("name"))
                    .map_err(io_error)?,
            ),
            "dash.rename" => to_js_value(
                &dashboards
                    .rename(&board_id(), msg.get("name"))
                    .map_err(io_error)?,
            ),
            "dash.remove" => to_js_value(&dashboards.remove(&board_id()).map_err(io_error)?),
            "dash.setSwitch" => {
                to_js_value(&dashboards.set_switch(msg.get("on")).map_err(io_error)?)
            }
            "support.reset" => {
                let class_name = msg.get("className").and_then(Value::as_str);
                let removed = self.app.ships.reset(class_name).map_err(io_error)?;
                json!({ "problems": [], "removed": removed })
            }
            "time.set" => {
                let problems = self.app.time.set(msg.get("time").unwrap_or(&Value::Null));
                json!({ "problems": problems.map_err(io_error)?, "time": self.app.time.get() })
            }
            "shipNames.set" => {
                let problems = self.app.ship_names.set(msg).map_err(io_error)?;
                json!({ "problems": problems, "names": self.app.ship_names.get() })
            }
            kind if kind.starts_with("fleet.") => self.fleet(kind, msg).await?,
            kind if kind.starts_with("relay.") => self.app.relay.page_message(kind, msg).await?,
            // `catalog` is answered by the catalog push after the result.
            _ => Value::Null,
        })
    }

    /// The fleet panel: joining and leaving, the event choice, the send consent and its preview,
    /// and the viewer's groups, card fields, admiral view and detail view. Structure changes are
    /// the CIC portal's, so `fleet.change` and `fleet.moves` get the unknown-message error.
    async fn fleet(&self, kind: &str, msg: &Value) -> Result<Value, String> {
        let hub = &self.app.fleet;
        let client = &self.app.fleet_client;
        let done = |r: Result<(), String>| json!({ "problems": r.err().map_or_else(Vec::new, |p| vec![p]) });
        match kind {
            "fleet.join" => {
                let invite = js_string(msg.get("invite"));
                let handle = js_string(msg.get("handle"));
                return Ok(done(client.join(&invite, &handle)));
            }
            "fleet.connect" => return Ok(done(client.connect(&js_string(msg.get("key"))))),
            "fleet.leave" => return Ok(done(client.leave(&js_string(msg.get("key"))).await)),
            "fleet.consent" => {
                let problems = client
                    .set_consent(msg.get("fields"))
                    .err()
                    .unwrap_or_default();
                return Ok(json!({ "problems": problems }));
            }
            "fleet.preview" => return Ok(json!({ "card": client.preview() })),
            "fleet.choose" => return Ok(done(client.choose(&js_string(msg.get("event"))))),
            "fleet.inspect" => return Ok(inspect_invite(&js_string(msg.get("invite")))),
            kind if kind.starts_with("fleet.host.") => return Ok(self.fleet_host(kind, msg).await),
            "fleet.layout.get" => {
                let key = js_string(msg.get("key"));
                return Ok(json!({ "key": key, "layout": self.app.fleet_layouts.get(&key) }));
            }
            "fleet.layout.save" => {
                let key = js_string(msg.get("key"));
                let change = self
                    .app
                    .fleet_layouts
                    .save(&key, msg.get("layout"), now_ms())
                    .map_err(io_error)?;
                return Ok(to_js_value(&change));
            }
            _ => {}
        }
        let view = hub.view();
        let group = || js_string(msg.get("group"));
        let change = match kind {
            "fleet.group.add" => view.add_group(msg.get("name")),
            "fleet.group.rename" => view.rename_group(&group(), msg.get("name")),
            "fleet.group.move" => view.move_group(&group(), msg.get("to")),
            "fleet.group.collapse" => view.collapse_group(&group(), msg.get("collapsed")),
            "fleet.group.remove" => view.remove_group(&group()),
            "fleet.assign" => view.assign(
                &js_string(msg.get("member")),
                msg.get("group").and_then(Value::as_str),
                msg.get("index"),
            ),
            "fleet.fields" => view.set_fields(msg.get("fields")),
            "fleet.admiral" => view.set_admiral(msg.get("on")),
            "fleet.detail" => {
                hub.set_detail(self.fleet_viewer, msg.get("member").and_then(Value::as_str));
                return Ok(Value::Null);
            }
            _ => return Err(format!("unknown fleet message {kind}")),
        };
        Ok(to_js_value(&change.map_err(io_error)?))
    }

    /// Peer-to-peer hosting: start and stop, the public address, a new code, Kick (bans and
    /// replaces the code), Remove, and lifting a ban.
    async fn fleet_host(&self, kind: &str, msg: &Value) -> Value {
        let host = &self.app.fleet_host;
        let client = &self.app.fleet_client;
        let done = |r: Result<(), String>| json!({ "problems": r.err().map_or_else(Vec::new, |p| vec![p]) });
        let field = |name: &str| js_string(msg.get(name));
        match kind {
            "fleet.host.start" => {
                let port = match msg.get("port") {
                    None | Some(Value::Null) => Ok(host.setup().0),
                    Some(v) => v
                        .as_u64()
                        .and_then(|p| u16::try_from(p).ok())
                        .filter(|p| *p >= 1024)
                        .ok_or_else(|| "The port must be 1024 to 65535".to_owned()),
                };
                let started = match port {
                    Ok(port) => {
                        crate::fleet::host::start_hosting(host, client, port, &field("handle"))
                            .await
                            .map(|_| ())
                    }
                    Err(e) => Err(e),
                };
                done(started)
            }
            "fleet.host.stop" => {
                crate::fleet::host::stop_hosting(host, client).await;
                done(Ok(()))
            }
            "fleet.host.address" => done(host.set_public_address(&field("address"))),
            "fleet.host.rotate" => done(host.rotate().map(|_| ())),
            "fleet.host.kick" => done(host.kick(&field("member"))),
            "fleet.host.remove" => done(host.remove(&field("member"))),
            "fleet.host.lift" => done(host.lift(&field("key"))),
            _ => done(Err(format!("unknown fleet message {kind}"))),
        }
    }

    async fn press(&self, action: &str, confirmed: bool) -> ActionResult {
        let core = &self.app.core;
        if !confirmed {
            return core.press(action, false).await;
        }
        let started = lock(&self.limits).holds.remove(action);
        match started.map(|s| s.elapsed()) {
            Some(held) if (HOLD..=HOLD_MAX).contains(&held) => core.press(action, true).await,
            _ => ActionResult::fail(
                FailStatus::NeedsConfirm,
                format!("Hold {action} for {} s to send it", HOLD.as_secs_f64()),
            ),
        }
    }

    /// Two steps: the first call only reads the file and answers the summary; a second call with
    /// `confirm` and the same text applies it.
    async fn import(&self, text: Option<&Value>, confirm: bool) -> Result<Value, String> {
        let answer = |applied: bool, summary: &ImportSummary| json!({ "applied": applied, "summary": to_js_value(summary) });
        let Some(text) = text
            .and_then(Value::as_str)
            .filter(|t| js::utf16_len(t) <= MAX_IMPORT_UNITS)
        else {
            let problem = format!(
                "The file must be text of at most {} KiB",
                MAX_IMPORT_UNITS / 1024
            );
            return Ok(answer(false, &ImportSummary::refused(problem)));
        };
        let parsed: Option<Value> = serde_json::from_str(text).ok();
        let known = known(&self.app.core, &self.app.dashboards, &self.app.themes).await;
        let (dashboard, summary) = read_dashboard(parsed.as_ref(), &known);
        let apply = {
            let mut limits = lock(&self.limits);
            let shown = limits.shown_import.take();
            match dashboard {
                Some(dashboard) if confirm && shown.as_deref() == Some(text) => Some(dashboard),
                Some(_) => {
                    limits.shown_import = Some(text.to_owned());
                    None
                }
                None => None,
            }
        };
        let Some(dashboard) = apply else {
            return Ok(answer(false, &summary));
        };
        self.app.dashboards.replace(dashboard).map_err(io_error)?;
        Ok(answer(true, &summary))
    }

    fn respond(
        self: &Arc<Self>,
        kind: &str,
        id: Value,
        detail: &str,
        outcome: Result<Value, String>,
    ) {
        let result = match outcome {
            Ok(result) => result,
            Err(error) => {
                self.send(&json!({ "type": "result", "id": id, "error": error }));
                return;
            }
        };
        let refused = result.get("ok") == Some(&Value::Bool(false));
        let quiet = [
            "catalog",
            "dash.get",
            "theme.list",
            "fleet.detail",
            "fleet.inspect",
            "relay.status",
        ]
        .contains(&kind);
        if !quiet && !kind.starts_with("hold.") {
            tracing::info!(
                "[portal {}] {kind} {detail}: {}",
                self.client,
                if refused { "refused" } else { "ok" }
            );
        }
        self.send(&json!({ "type": "result", "id": id, "actor": self.client, "result": result }));
        if kind.starts_with("seq.") || kind == "catalog" {
            self.send_catalog();
        }
        if self.policy.is_companion()
            && [
                "fleet.join",
                "fleet.connect",
                "fleet.leave",
                "fleet.consent",
            ]
            .contains(&kind)
            && result
                .get("problems")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        {
            let what = if kind == "fleet.consent" {
                "fleet sharing"
            } else {
                "fleet"
            };
            let from = self.client.split('/').next().unwrap_or("the companion");
            self.app.relay.note(format!("{what} changed on {from}"));
        }
        if kind == "dash.get" {
            if !self.policy.is_companion() {
                self.push_relay();
            }
            self.send(&self.app.time.push());
            self.send(&self.app.ship_names.push());
            self.push_themes();
            self.push_dashboard();
            self.push_fleet(true);
        }
        if kind != "run" && refused {
            let message = js_string(result.get("message"));
            let game = result
                .get("game")
                .filter(|g| js::truthy(g))
                .map(|g| format!(" ({})", js_string(Some(g))))
                .unwrap_or_default();
            self.notice(&format!("{message}{game}"));
        }
        let problems = if result.is_array() {
            Some(&result)
        } else {
            result.get("problems")
        };
        if let Some(Value::Array(problems)) = problems
            && (kind.starts_with("dash.")
                || kind.starts_with("support.")
                || kind.starts_with("theme.")
                || kind.starts_with("time.")
                || kind.starts_with("shipNames.")
                || kind.starts_with("fleet.")
                || kind.starts_with("relay."))
            && !problems.is_empty()
        {
            let text: Vec<String> = problems.iter().map(|p| js_string(Some(p))).collect();
            self.notice(&text.join(" \u{b7} "));
        }
    }

    fn push_relay(&self) {
        let mut push = json!({ "type": "relay" });
        if let (Some(push), Ok(Value::Object(status))) = (
            push.as_object_mut(),
            serde_json::to_value(self.app.relay.status()),
        ) {
            push.extend(status);
        }
        self.send(&push);
    }

    fn notice(&self, text: &str) {
        self.send(&json!({ "type": "notice", "tone": "bad", "text": text }));
    }

    /// On connect and after any `seq.*` or `catalog` message; the game's list may take a moment.
    fn send_catalog(self: &Arc<Self>) {
        let session = Arc::clone(self);
        tokio::spawn(async move {
            let body = catalog(&session.app.core).await;
            session.send(&Push {
                kind: "catalog",
                body,
            });
        });
    }

    /// The built-in and user themes, before the first layout so the page draws in its theme.
    fn push_themes(&self) {
        let mut push = json!({ "type": "themes" });
        if let (Some(push), Value::Object(model)) = (push.as_object_mut(), self.app.themes.model())
        {
            push.extend(model);
        }
        self.send(&push);
    }

    /// The picker's ship names with the user's own in place of the derived ones.
    fn named(&self, boards: &mut [BoardInfo]) {
        for b in boards {
            let users = b.ship.as_ref().and_then(|s| s.class_name.as_deref());
            if let Some(name) = users.and_then(|c| self.app.ship_names.name_of(c)) {
                b.ship_name = Some(name);
            }
        }
    }

    fn push_dashboard(&self) {
        let store = &self.app.dashboards;
        let board = store.active_board();
        let mut list = store.list();
        self.named(&mut list.dashboards);
        let mut push = json!({
            "type": "dashboard",
            "dashboard": to_js_value(&board.dashboard()),
            "board": { "id": board.id, "name": board.name, "ship": to_js_value(&board.ship), "generic": board.is_generic() },
            "dashboards": to_js_value(&list.dashboards),
            "switchOnShipChange": list.switch_on_ship_change,
            "pinned": list.pinned,
            "kinds": to_js_value(&KindTable),
            "lookOnlyFor": to_js_value(&LookOnlyFor),
            "leverSize": to_js_value(&LEVER_SIZE),
            "lampValues": lamp_values(),
            "confirmGroup": CONFIRM_GROUP,
            "limits": {
                "columns": COLUMNS, "minRows": MIN_ROWS, "maxRows": MAX_ROWS,
                "tabs": MAX_TABS, "widgets": MAX_WIDGETS, "label": MAX_LABEL,
                "displayLabel": MAX_DISPLAY_LABEL, "gamepadId": MAX_GAMEPAD_ID,
                "bezelButtons": MAX_BEZEL_BUTTONS, "buttonIndex": MAX_BUTTON_INDEX,
                "dashboards": MAX_BOARDS, "cells": CELLS, "defaultCell": DEFAULT_CELL,
            },
            // The value, toggle and widget ids that grey when the ship lacks them.
            "greyable": FIELDS.iter().flat_map(|f| f.greys.iter()).collect::<Vec<_>>(),
            "holdMs": HOLD.as_millis(),
        });
        // A detached window keeps a generic tab when the ship's dashboard has none of its name.
        if !board.is_generic() {
            let generic = store.generic();
            push["generic"] = json!({ "id": generic.id, "theme": generic.theme, "tabs": to_js_value(&generic.tabs) });
        }
        if let Some(problem) = store.file_problem() {
            push["fileProblem"] = json!(problem);
        }
        self.send(&push);
    }

    /// Send `build()` unless it went out less than [`PUSH_EVERY`] ago (then the time left) or is
    /// unchanged. `now` skips the wait.
    fn throttled(
        &self,
        which: fn(&mut Pushes) -> &mut Throttle,
        now: bool,
        build: impl FnOnce() -> String,
    ) -> Option<Duration> {
        let mut pushes = lock(&self.pushes);
        let throttle = which(&mut pushes);
        let at = Instant::now();
        if !now
            && let Some(due) = throttle
                .last_at
                .map(|last| last + PUSH_EVERY)
                .filter(|&due| due > at)
        {
            return Some(due - at);
        }
        throttle.last_at = Some(at);
        let text = build();
        if text != throttle.last_sent {
            let _ = self.out.send(text.clone());
            throttle.last_sent = text;
        }
        None
    }

    fn push_status(&self, now: bool) -> Option<Duration> {
        self.throttled(|p| &mut p.status, now, || {
            to_text(&json!({ "type": "status", "status": to_js_value(&build_status(&self.app.core)) }))
        })
    }

    fn push_values(&self, now: bool) -> Option<Duration> {
        if !self.is_dash_page() {
            return None;
        }
        self.throttled(
            |p| &mut p.values,
            now,
            || {
                let snapshot = self.app.ships.snapshot();
                to_text(&Push {
                    kind: "values",
                    body: ValuesPush {
                        model: build_values_with(&self.app.core, |id| snapshot.greyed(id)),
                        ship: snapshot.ship.clone(),
                        support: SupportMap(snapshot.support.clone()),
                        recording: *self.app.recording.borrow(),
                    },
                })
            },
        )
    }

    /// The fleet panel's model, to a dashboard page, when the service has a fleet.
    fn push_fleet(&self, now: bool) -> Option<Duration> {
        let hub = &self.app.fleet;
        if !self.is_dash_page() {
            return None;
        }
        self.throttled(
            |p| &mut p.fleet,
            now,
            || {
                to_text(&Push {
                    kind: "fleet",
                    body: hub.model(),
                })
            },
        )
    }

    /// Both models after a change, each as soon as its throttle lets it go.
    async fn push_models(&self) {
        let (mut status, mut values) = (true, true);
        while status || values {
            let mut wait: Option<Duration> = None;
            if status {
                wait = self.push_status(false);
                status = wait.is_some();
            }
            if values {
                let held = self.push_values(false);
                values = held.is_some();
                wait = wait.into_iter().chain(held).min();
            }
            if let Some(wait) = wait {
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// What the log line names: the action, sequence, toggle or target.
fn log_detail(msg: &Value) -> String {
    let field = |name| msg.get(name);
    if let Some(action) = field("action") {
        return js_string(Some(action));
    }
    if let Some(seq) = field("sequence") {
        return seq
            .as_str()
            .map_or_else(|| js_string(seq.get("id")), str::to_owned);
    }
    if let Some(f) = field("fn") {
        return js_string(Some(f));
    }
    if let Some(toggle) = field("toggle") {
        return format!("{} {}", js_string(Some(toggle)), js_string(field("to")));
    }
    field("to")
        .map(|to| js_string(Some(to)))
        .unwrap_or_default()
}
