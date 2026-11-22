//! The app's own portal session with the in-process service: it reads the dashboard the way a page
//! does (`dash.get`, then the `dashboard` and `values` pushes) and sends bezel actions as page
//! messages, so they take the same path, rate cap and service-timed hold as a widget click.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::bridge;
use crate::model::DashState;

pub const CLIENT_NAME: &str = "hardpoint";
const RETRY: Duration = Duration::from_secs(1);
/// Results the app still waits for, to tell the tab a refusal came from.
const MAX_PENDING: usize = 64;

/// What the session tells the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The dashboard changed (or arrived); the new state is in the shared [`DashState`].
    Dashboard,
    /// An action from a tab's bezel buttons was refused or failed.
    Refused { tab: Option<String>, text: String },
}

/// A page message to send, and the tab it is for.
pub type Outgoing = (Value, Option<String>);

pub type Shared = Arc<Mutex<DashState>>;

pub fn lock(state: &Shared) -> std::sync::MutexGuard<'_, DashState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why a `result` counts as a refusal, as the page words it.
pub fn refusal(result: &Value) -> Option<String> {
    if let Some(e) = result.get("error").and_then(Value::as_str) {
        return Some(e.to_owned());
    }
    let r = result.get("result")?;
    let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    if r.get("status").and_then(Value::as_str) == Some("failed") {
        let step = r.get("step").map(Value::to_string).unwrap_or_default();
        return Some(format!(
            "{} failed at step {step}: {}",
            text("name"),
            text("message")
        ));
    }
    (r.get("ok") == Some(&Value::Bool(false))).then(|| text("message"))
}

/// Runs the session until `closing`, reconnecting when it drops.
pub async fn run(
    port: u16,
    token: String,
    state: Shared,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
    events: impl Fn(Event) + Send + 'static,
    mut closing: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = closing.wait_for(|c| *c) => return,
            r = session(port, &token, &state, &mut outgoing, &events) => {
                if let Err(e) = r {
                    tracing::warn!("app session with the service: {e}");
                }
            }
        }
        tokio::select! {
            _ = closing.wait_for(|c| *c) => return,
            _ = tokio::time::sleep(RETRY) => {}
        }
    }
}

async fn session(
    port: u16,
    token: &str,
    state: &Shared,
    outgoing: &mut mpsc::UnboundedReceiver<Outgoing>,
    events: &impl Fn(Event),
) -> Result<(), String> {
    let mut request = format!("ws://127.0.0.1:{port}/portal")
        .into_client_request()
        .map_err(|e| e.to_string())?;
    let origin = HeaderValue::from_str(&bridge::origin(port)).map_err(|e| e.to_string())?;
    request.headers_mut().insert("origin", origin);
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();
    let send = |v: Value| Message::Text(v.to_string().into());
    sink.send(send(
        json!({ "type": "hello", "token": token, "client": CLIENT_NAME }),
    ))
    .await
    .map_err(|e| e.to_string())?;
    sink.send(send(json!({ "id": 0, "type": "dash.get" })))
        .await
        .map_err(|e| e.to_string())?;
    // A bezel press made while the service was away is stale now.
    while outgoing.try_recv().is_ok() {}
    let mut next_id: u64 = 1;
    let mut pending: VecDeque<(u64, Option<String>)> = VecDeque::new();
    loop {
        tokio::select! {
            msg = stream.next() => {
                let text = match msg {
                    None => return Err("closed".into()),
                    Some(Err(e)) => return Err(e.to_string()),
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(_))) => return Err("closed by the service".into()),
                    Some(Ok(_)) => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v.get("type").and_then(Value::as_str) {
                    Some("dashboard") => {
                        if lock(state).apply_dashboard(&v) {
                            events(Event::Dashboard);
                        }
                    }
                    Some("values") => lock(state).apply_values(&v),
                    Some("result") => {
                        let id = v.get("id").and_then(Value::as_u64);
                        let tab = match pending.iter().position(|(p, _)| Some(*p) == id) {
                            Some(i) => pending.remove(i).and_then(|(_, t)| t),
                            None => None,
                        };
                        if let Some(text) = refusal(&v) {
                            tracing::warn!("bezel action refused: {text}");
                            events(Event::Refused { tab, text });
                        }
                    }
                    _ => {}
                }
            }
            out = outgoing.recv() => {
                let Some((mut message, tab)) = out else { return Ok(()) };
                let id = next_id;
                next_id += 1;
                if let Some(m) = message.as_object_mut() {
                    m.insert("id".into(), id.into());
                }
                if pending.len() >= MAX_PENDING {
                    pending.pop_front();
                }
                pending.push_back((id, tab));
                sink.send(send(message)).await.map_err(|e| e.to_string())?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_read_as_the_page_reads_them() {
        let ok = json!({ "type": "result", "id": 1, "result": { "ok": true } });
        assert_eq!(refusal(&ok), None);
        let no = json!({ "type": "result", "id": 1, "result": { "ok": false, "status": "unknown", "message": "Too many actions" } });
        assert_eq!(refusal(&no).as_deref(), Some("Too many actions"));
        let failed = json!({ "type": "result", "id": 1, "result": { "status": "failed", "name": "Power up", "step": 2, "message": "no power" } });
        assert_eq!(
            refusal(&failed).as_deref(),
            Some("Power up failed at step 2: no power")
        );
        let error = json!({ "type": "result", "id": 1, "error": "bad message" });
        assert_eq!(refusal(&error).as_deref(), Some("bad message"));
        let saved: Value = json!({ "type": "result", "id": 1, "result": [] });
        assert_eq!(refusal(&saved), None);
    }
}
