//! A fake game telemetry server over real gRPC, scripted per test.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures_util::stream;
use prost_reflect::DynamicMessage;
use sc_game::schema::{self, DynamicCodec};
use serde_json::{Value as Json, json};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Status, body::Body, server::NamedService, transport::Server};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type TriggerFn = Arc<dyn Fn(Json) -> BoxFuture<Json> + Send + Sync>;

/// What the stream does after the frames.
#[derive(Debug, Clone, Copy, Default)]
pub enum AfterFrames {
    /// Close cleanly.
    #[default]
    End,
    /// Keep the stream open.
    Hold,
    /// Fail it.
    Error,
}

/// Wire shapes are JSON as the game sends them (snake_case, enum names).
#[derive(Clone, Default)]
pub struct FakeServerOptions {
    pub frames: Vec<Json>,
    pub frame_interval: Option<Duration>,
    pub after_frames: AfterFrames,
    pub actions: Vec<Json>,
    pub trigger: Option<TriggerFn>,
}

pub fn respond_with(response: Json) -> TriggerFn {
    Arc::new(move |_| {
        let response = response.clone();
        Box::pin(async move { response })
    })
}

pub struct FakeServer {
    pub address: String,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl FakeServer {
    pub fn trigger_calls(&self) -> Vec<Json> {
        self.shared.trigger_calls.lock().unwrap().clone()
    }

    /// Number of streams the client cancelled.
    pub fn cancelled_streams(&self) -> usize {
        self.shared.cancelled.load(Ordering::SeqCst)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Shared {
    options: FakeServerOptions,
    trigger_calls: Mutex<Vec<Json>>,
    cancelled: AtomicUsize,
}

pub async fn start_fake_server(options: FakeServerOptions) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let shared = Arc::new(Shared {
        options,
        trigger_calls: Mutex::new(Vec::new()),
        cancelled: AtomicUsize::new(0),
    });
    let service = FakeService(shared.clone());
    let task = tokio::spawn(async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    FakeServer {
        address,
        shared,
        task,
    }
}

/// An address with nothing listening on it.
pub fn closed_address() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().to_string()
}

#[derive(Clone)]
struct FakeService(Arc<Shared>);

impl NamedService for FakeService {
    const NAME: &'static str = schema::SERVICE;
}

impl tower::Service<http::Request<Body>> for FakeService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let shared = self.0.clone();
        Box::pin(async move {
            let path = request.uri().path().to_owned();
            let response = match path.as_str() {
                schema::TELEMETRY_STREAM_PATH => {
                    let mut grpc = grpc_for("TelemetryStreamRequest");
                    let handler = tower::service_fn(move |_: tonic::Request<DynamicMessage>| {
                        let frames = telemetry(shared.clone());
                        async move { Ok::<_, Status>(tonic::Response::new(frames)) }
                    });
                    grpc.server_streaming(handler, request).await
                }
                schema::LIST_TRIGGERABLE_ACTIONS_PATH => {
                    let mut grpc = grpc_for("ListTriggerableActionsRequest");
                    let handler = tower::service_fn(move |_: tonic::Request<DynamicMessage>| {
                        let actions = json!({ "actions": shared.options.actions });
                        async move { Ok::<_, Status>(reply("ListTriggerableActionsResponse", &actions)) }
                    });
                    grpc.unary(handler, request).await
                }
                schema::TRIGGER_GAME_ACTION_PATH => {
                    let mut grpc = grpc_for("TriggerGameActionRequest");
                    let handler = tower::service_fn(move |call: tonic::Request<DynamicMessage>| {
                        let shared = shared.clone();
                        async move {
                            let call = schema::to_json(call.get_ref());
                            shared.trigger_calls.lock().unwrap().push(call.clone());
                            let response = match &shared.options.trigger {
                                Some(trigger) => trigger(call).await,
                                None => json!({ "status": "TRIGGER_GAME_ACTION_STATUS_OK" }),
                            };
                            Ok::<_, Status>(reply("TriggerGameActionResponse", &response))
                        }
                    });
                    grpc.unary(handler, request).await
                }
                _ => Status::unimplemented(path).into_http(),
            };
            Ok(response)
        })
    }
}

fn grpc_for(request: &str) -> tonic::server::Grpc<DynamicCodec> {
    tonic::server::Grpc::new(DynamicCodec::new(schema::message(request)))
}

fn reply(message: &str, json: &Json) -> tonic::Response<DynamicMessage> {
    tonic::Response::new(schema::from_json(schema::message(message), json).unwrap())
}

/// Counts the stream as cancelled if it is dropped before it finished.
struct CancelGuard {
    shared: Arc<Shared>,
    finished: bool,
}

impl CancelGuard {
    fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

type FrameStream = Pin<Box<dyn futures_util::Stream<Item = Result<DynamicMessage, Status>> + Send>>;

fn telemetry(shared: Arc<Shared>) -> FrameStream {
    let options = &shared.options;
    let interval = options.frame_interval.unwrap_or(Duration::from_millis(5));
    let after = options.after_frames;
    let frames: Arc<Vec<DynamicMessage>> = Arc::new(
        options
            .frames
            .iter()
            .map(|frame| {
                schema::from_json(schema::message("TelemetryStreamResponse"), frame).unwrap()
            })
            .collect(),
    );
    let guard = CancelGuard {
        shared: shared.clone(),
        finished: false,
    };
    Box::pin(stream::unfold((0, guard), move |(index, mut guard)| {
        let frames = frames.clone();
        async move {
            if guard.finished {
                return None;
            }
            if index > 0 {
                tokio::time::sleep(interval).await;
            }
            if let Some(frame) = frames.get(index) {
                return Some((Ok(frame.clone()), (index + 1, guard)));
            }
            match after {
                AfterFrames::End => {
                    guard.finish();
                    None
                }
                AfterFrames::Error => {
                    guard.finish();
                    Some((Err(Status::internal("scripted failure")), (index, guard)))
                }
                AfterFrames::Hold => std::future::pending().await,
            }
        }
    }))
}
