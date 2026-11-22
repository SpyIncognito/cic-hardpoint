//! One open portal or dashboard page: the service pushes the ship status (and, once the page asked
//! for the dashboard, its layout and values), and runs what the page asks through the same core
//! as the deck. The page never talks to the game itself. Messages from one page run in order,
//! except that `run` does not hold up the next one.
//!
//! Every page message goes through [`Session::run`]: the place where per-person permissions and
//! an audit log would go when the portal grows to more than one user.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use sc_core::engine::{
    ActionResult, CONFIRM_GROUP, FailStatus, OnOff, SequenceError, TargetMode, parse_sequence,
};
use sc_core::js;

use super::status::build_status;
use super::{catalog, known};
use crate::dashboard::{
    COLUMNS, ImportSummary, KindTable, LEVER_SIZE, LookOnlyFor, MAX_BEZEL_BUTTONS, MAX_BOARDS,
    MAX_BUTTON_INDEX, MAX_DISPLAY_LABEL, MAX_GAMEPAD_ID, MAX_LABEL, MAX_ROWS, MAX_TABS,
    MAX_WIDGETS, MIN_ROWS, build_values_with, check_ship, lamp_values, read_dashboard,
};
use crate::fleet::ViewChange;
use crate::json::{js_string, to_js_value, to_pretty_text, to_text};
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
async fn hello(stream: &mut SplitStream<WebSocket>, token: &Token) -> Option<String> {
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

/// Serve one page's WebSocket until it closes or the service stops.
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
    let session = Arc::new(Session {
        app: Arc::clone(&app),
        client,
        out,
        dash_page: AtomicBool::new(false),
        limits: Mutex::default(),
        pushes: Mutex::default(),
        fleet_viewer: app.fleet.as_ref().map_or(0, |hub| hub.viewer()),
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
    ];
    if let Some(hub) = &app.fleet {
        pushers.push(tokio::spawn(push_fleet(
            Arc::clone(&session),
            hub.subscribe(),
            hub.view().subscribe(),
        )));
    }
    session.send_catalog();
    session.push_status(true);

    let mut closing = app.closing.clone();
    loop {
        let frame = tokio::select! {
            frame = stream.next() => frame,
            _ = closing.changed() => break,
        };
        let text = match frame {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Binary(b))) => match String::from_utf8(b.to_vec()) {
                Ok(text) => text,
                Err(_) => continue,
            },
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        };
        session.handle(&text).await;
    }
    pushers.iter().for_each(JoinHandle::abort);
    writer.abort();
    if let Some(hub) = &app.fleet {
        hub.set_detail(session.fleet_viewer, None);
    }
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

/// `msg.<name>` as a string, or the error the TS would have thrown on.
fn string_field<'a>(msg: &'a Value, name: &str) -> Result<&'a str, String> {
    msg.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} must be a string"))
}

fn io_error(e: std::io::Error) -> String {
    e.to_string()
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
        let is_run = kind == "run";
        let work = self.run(kind.clone(), msg);
        let session = Arc::clone(self);
        let answer = async move {
            let outcome = work.await;
            session.respond(&kind, id, &detail, outcome);
        };
        // A long sequence must not block other messages from this page (an abort, say).
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
        let capped = ACTION_TYPES.contains(&kind.as_str()) && !self.admit();
        let session = Arc::clone(self);
        async move {
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
        if limits.recent.len() >= MAX_ACTIONS_PER_S {
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
            "dash.addTab" => to_js_value(&dashboards.add_tab(msg.get("name")).map_err(io_error)?),
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
            "dash.list" => to_js_value(&dashboards.list()),
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
                let change = dashboards
                    .create_for_ship(ship, from, msg.get("name"), &known)
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
            kind if kind.starts_with("fleet.") => self.fleet(kind, msg)?,
            // `catalog` is answered by the catalog push after the result.
            _ => Value::Null,
        })
    }

    /// The viewer's fleet panel: groups, card fields, the admiral view and the detail view.
    fn fleet(&self, kind: &str, msg: &Value) -> Result<Value, String> {
        let Some(hub) = &self.app.fleet else {
            return Ok(to_js_value(&ViewChange {
                problems: vec!["There is no fleet".to_owned()],
                group: None,
            }));
        };
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
        let quiet = ["catalog", "dash.get", "theme.list", "fleet.detail"].contains(&kind);
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
        if kind == "dash.get" {
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
                || kind.starts_with("fleet."))
            && !problems.is_empty()
        {
            let text: Vec<String> = problems.iter().map(|p| js_string(Some(p))).collect();
            self.notice(&text.join(" \u{b7} "));
        }
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

    fn push_dashboard(&self) {
        let store = &self.app.dashboards;
        let board = store.active_board();
        let list = store.list();
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
                "dashboards": MAX_BOARDS,
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
        let hub = self.app.fleet.as_ref()?;
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
