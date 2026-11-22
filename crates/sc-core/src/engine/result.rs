//! What the engine answers, in the JSON shape the bus and portal carry (PROTOCOL.md "Shared types").

use serde::{Deserialize, Serialize};

use sc_game::TriggerStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OkStatus {
    Done,
    Already,
    /// In a sequence: sent, but the game never reported the state, so it is not confirmed.
    Sent,
    /// In a sequence: the state was never reported and only a toggle exists, so nothing was sent.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailStatus {
    /// The state is older than the fresh window or unknown, so the core will not act on it.
    Stale,
    NotInVehicle,
    /// In the confirm group: the surface must hold to confirm and pass `confirmed`.
    NeedsConfirm,
    /// The press stopped a running sequence and did nothing else.
    AbortedSequence,
    Unknown,
    /// The game refused or did not answer; see `game`.
    Game,
    /// The game did not reach the asked state in time.
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub status: FailStatus,
    pub message: String,
    pub game: Option<TriggerStatus>,
}

/// `{ ok: true, status }` or `{ ok: false, status, message, game? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "ResultWire", try_from = "ResultWire")]
pub enum ActionResult {
    Ok(OkStatus),
    Failed(Failure),
}

impl ActionResult {
    pub fn fail(status: FailStatus, message: impl Into<String>) -> Self {
        Self::Failed(Failure {
            status,
            message: message.into(),
            game: None,
        })
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum WireStatus {
    Ok(OkStatus),
    Fail(FailStatus),
}

#[derive(Serialize, Deserialize)]
struct ResultWire {
    ok: bool,
    status: WireStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game: Option<TriggerStatus>,
}

impl From<ActionResult> for ResultWire {
    fn from(r: ActionResult) -> Self {
        match r {
            ActionResult::Ok(status) => Self {
                ok: true,
                status: WireStatus::Ok(status),
                message: None,
                game: None,
            },
            ActionResult::Failed(f) => Self {
                ok: false,
                status: WireStatus::Fail(f.status),
                message: Some(f.message),
                game: f.game,
            },
        }
    }
}

impl TryFrom<ResultWire> for ActionResult {
    type Error = &'static str;

    fn try_from(w: ResultWire) -> Result<Self, Self::Error> {
        match (w.ok, w.status, w.message) {
            (true, WireStatus::Ok(status), _) => Ok(Self::Ok(status)),
            (false, WireStatus::Fail(status), Some(message)) => Ok(Self::Failed(Failure {
                status,
                message,
                game: w.game,
            })),
            _ => Err("a result is { ok: true, status } or { ok: false, status, message, game? }"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressStatus {
    Running,
    Done,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceProgress {
    pub id: String,
    pub name: String,
    /// 1-based step being run, or the step that failed.
    pub step: usize,
    pub of: usize,
    pub status: ProgressStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Steps sent without the game confirming them (it did not report that state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unconfirmed: Option<u32>,
    /// Steps skipped because their state was never reported and only a toggle exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn results_serialize_as_the_ts_shapes() {
        let cases = [
            (
                ActionResult::Ok(OkStatus::Already),
                json!({ "ok": true, "status": "already" }),
            ),
            (
                ActionResult::fail(FailStatus::NotInVehicle, "Not in a vehicle"),
                json!({ "ok": false, "status": "not-in-vehicle", "message": "Not in a vehicle" }),
            ),
            (
                ActionResult::Failed(Failure {
                    status: FailStatus::Game,
                    message: "m".into(),
                    game: Some(TriggerStatus::RateLimited),
                }),
                json!({ "ok": false, "status": "game", "message": "m", "game": "RATE_LIMITED" }),
            ),
        ];
        for (result, expected) in cases {
            assert_eq!(serde_json::to_value(&result).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<ActionResult>(expected).unwrap(),
                result
            );
        }
        assert!(
            serde_json::from_value::<ActionResult>(json!({ "ok": true, "status": "stale" }))
                .is_err()
        );
        assert_eq!(
            serde_json::to_string(&ActionResult::fail(FailStatus::AbortedSequence, "x")).unwrap(),
            r#"{"ok":false,"status":"aborted-sequence","message":"x"}"#
        );
    }
}
