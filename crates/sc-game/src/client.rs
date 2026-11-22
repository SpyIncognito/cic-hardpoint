use std::{sync::Mutex, time::Duration};

use futures_util::stream;
use http::uri::PathAndQuery;
use prost_reflect::DynamicMessage;
use serde_json::{Value as Json, json};
use tonic::{
    Code, Status, Streaming,
    client::Grpc,
    transport::{Channel, Endpoint},
};

use crate::{
    lock,
    schema::{self, DynamicCodec},
    types::{
        BoxFuture, FrameStream, GameAction, GameApi, StreamError, TriggerResult, TriggerStatus,
    },
};

#[derive(Debug, Clone)]
pub struct GameClientOptions {
    /// host:port of the game's telemetry server.
    pub address: String,
    pub list_timeout: Duration,
    pub trigger_timeout: Duration,
}

impl Default for GameClientOptions {
    fn default() -> Self {
        Self {
            address: schema::DEFAULT_ADDRESS.to_owned(),
            list_timeout: Duration::from_millis(1500),
            trigger_timeout: Duration::from_millis(2000),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid game address {address:?}: {reason}")]
pub struct InvalidAddress {
    pub address: String,
    pub reason: String,
}

/// The live game over gRPC (plain HTTP/2). The connection is made on first use and remade by
/// tonic as needed, so a client can be created before the game runs.
pub struct GameClient {
    grpc: Mutex<Option<Grpc<Channel>>>,
    list_timeout: Duration,
    trigger_timeout: Duration,
}

const STATUS_PREFIX: &str = "TRIGGER_GAME_ACTION_STATUS_";

impl GameClient {
    /// Must be called inside a tokio runtime.
    pub fn new(options: GameClientOptions) -> Result<Self, InvalidAddress> {
        let endpoint =
            Endpoint::from_shared(format!("http://{}", options.address)).map_err(|error| {
                InvalidAddress {
                    address: options.address.clone(),
                    reason: error.to_string(),
                }
            })?;
        Ok(Self {
            grpc: Mutex::new(Some(Grpc::new(endpoint.connect_lazy()))),
            list_timeout: options.list_timeout,
            trigger_timeout: options.trigger_timeout,
        })
    }

    /// A client for the game on this machine at the default address.
    pub fn local() -> Self {
        Self::new(GameClientOptions::default()).expect("the default address is valid")
    }

    fn grpc(&self) -> Result<Grpc<Channel>, Status> {
        lock(&self.grpc)
            .clone()
            .ok_or_else(|| Status::unavailable("game client closed"))
    }

    async fn unary(
        &self,
        path: &'static str,
        request: DynamicMessage,
        response: &str,
        timeout: Duration,
    ) -> Result<Json, Status> {
        let mut grpc = self.grpc()?;
        let codec = DynamicCodec::new(schema::message(response));
        let call = async move {
            // A transport that is not ready means the game is not there to answer.
            grpc.ready()
                .await
                .map_err(|error| Status::unavailable(error.to_string()))?;
            let mut request = tonic::Request::new(request);
            request.set_timeout(timeout);
            grpc.unary(request, PathAndQuery::from_static(path), codec)
                .await
        };
        // grpc-timeout asks the server to give up; this makes sure the client does too.
        let response = tokio::time::timeout(timeout, call)
            .await
            .map_err(|_| Status::deadline_exceeded("the game did not answer in time"))??;
        Ok(schema::to_json(response.get_ref()))
    }
}

impl GameApi for GameClient {
    fn open_stream(&self) -> FrameStream {
        let start = StreamState::Connecting(self.grpc());
        Box::pin(stream::unfold(start, |state| async move {
            let mut streaming = match state {
                StreamState::Connecting(grpc) => match open_telemetry(grpc).await {
                    Ok(streaming) => streaming,
                    Err(status) => {
                        return Some((Err(StreamError::from(status)), StreamState::Done));
                    }
                },
                StreamState::Open(streaming) => streaming,
                StreamState::Done => return None,
            };
            match streaming.message().await {
                Ok(Some(message)) => {
                    Some((Ok(schema::to_json(&message)), StreamState::Open(streaming)))
                }
                Ok(None) => None,
                Err(status) => Some((Err(StreamError::from(status)), StreamState::Done)),
            }
        }))
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async move {
            let request = DynamicMessage::new(schema::message("ListTriggerableActionsRequest"));
            let Ok(response) = self
                .unary(
                    schema::LIST_TRIGGERABLE_ACTIONS_PATH,
                    request,
                    "ListTriggerableActionsResponse",
                    self.list_timeout,
                )
                .await
            else {
                return Vec::new();
            };
            let actions = response.get("actions").and_then(Json::as_array);
            actions
                .into_iter()
                .flatten()
                .map(|action| GameAction {
                    name: string_field(action, "action_name"),
                    category: string_field(action, "category"),
                    holdable: action.get("holdable") == Some(&Json::Bool(true)),
                })
                .collect()
        })
    }

    fn trigger_action(&self, name: &str, hold_ms: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        let mut request = json!({ "action_name": name });
        if let Some(hold_ms) = hold_ms {
            request["hold_ms"] = hold_ms.into();
        }
        Box::pin(async move {
            let request = schema::from_json(schema::message("TriggerGameActionRequest"), &request)
                .expect("the request matches the schema");
            let response = self
                .unary(
                    schema::TRIGGER_GAME_ACTION_PATH,
                    request,
                    "TriggerGameActionResponse",
                    self.trigger_timeout,
                )
                .await;
            match response {
                Ok(response) => TriggerResult::new(
                    to_trigger_status(response.get("status")),
                    string_field(&response, "message"),
                ),
                Err(status) => TriggerResult::new(
                    if is_unreachable(status.code()) {
                        TriggerStatus::Unreachable
                    } else {
                        TriggerStatus::Failed
                    },
                    error_message(&status),
                ),
            }
        })
    }

    /// Later calls fail as unreachable; streams already open run until cancelled, as in grpc-js.
    fn close(&self) {
        lock(&self.grpc).take();
    }
}

enum StreamState {
    Connecting(Result<Grpc<Channel>, Status>),
    Open(Streaming<DynamicMessage>),
    Done,
}

async fn open_telemetry(
    grpc: Result<Grpc<Channel>, Status>,
) -> Result<Streaming<DynamicMessage>, Status> {
    let mut grpc = grpc?;
    grpc.ready()
        .await
        .map_err(|error| Status::unavailable(error.to_string()))?;
    let request = tonic::Request::new(DynamicMessage::new(schema::message(
        "TelemetryStreamRequest",
    )));
    let codec = DynamicCodec::new(schema::message("TelemetryStreamResponse"));
    let response = grpc
        .server_streaming(
            request,
            PathAndQuery::from_static(schema::TELEMETRY_STREAM_PATH),
            codec,
        )
        .await?;
    Ok(response.into_inner())
}

/// The call never reached the game, as opposed to the game rejecting it.
fn is_unreachable(code: Code) -> bool {
    matches!(
        code,
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled
    )
}

fn error_message(status: &Status) -> String {
    if status.message().is_empty() {
        format!("{:?}", status.code())
    } else {
        status.message().to_owned()
    }
}

/// Absent means 0 (UNSPECIFIED); a number is a value newer than this schema.
fn to_trigger_status(raw: Option<&Json>) -> TriggerStatus {
    let Some(raw) = raw.and_then(Json::as_str) else {
        return TriggerStatus::Unspecified;
    };
    match raw.strip_prefix(STATUS_PREFIX).unwrap_or(raw) {
        "OK" => TriggerStatus::Ok,
        "UNKNOWN_ACTION" => TriggerStatus::UnknownAction,
        "MAP_INACTIVE" => TriggerStatus::MapInactive,
        "RATE_LIMITED" => TriggerStatus::RateLimited,
        "FAILED" => TriggerStatus::Failed,
        "HOLD_NOT_ALLOWED" => TriggerStatus::HoldNotAllowed,
        "FILTERED" => TriggerStatus::Filtered,
        _ => TriggerStatus::Unspecified,
    }
}

fn string_field(object: &Json, key: &str) -> String {
    object
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}
