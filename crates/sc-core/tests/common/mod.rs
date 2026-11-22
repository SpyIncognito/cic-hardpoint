#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use sc_core::engine::{Engine, EngineOptions};
use sc_core::state::{StateModel, StateModelOptions};
use sc_game::{BoxFuture, FrameStream, GameAction, GameApi, TriggerResult, TriggerStatus};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

pub type OnSend = Box<dyn Fn(&str) + Send + Sync>;

/// Records every trigger with its time and answers from a script (OK once the script runs out).
pub struct TestGame {
    pub sent: Mutex<Vec<(String, Instant)>>,
    answers: Mutex<VecDeque<TriggerResult>>,
    on_send: Mutex<Vec<OnSend>>,
}

impl TestGame {
    pub fn new(answers: &[TriggerStatus]) -> Self {
        Self::with_results(answers.iter().map(|s| TriggerResult::new(*s, "")).collect())
    }

    pub fn with_results(answers: Vec<TriggerResult>) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            answers: Mutex::new(answers.into()),
            on_send: Mutex::new(Vec::new()),
        }
    }

    pub fn on_send(&self, f: impl Fn(&str) + Send + Sync + 'static) {
        self.on_send.lock().unwrap().push(Box::new(f));
    }

    pub fn actions(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|(a, _)| a.clone())
            .collect()
    }
}

impl GameApi for TestGame {
    fn open_stream(&self) -> FrameStream {
        Box::pin(futures_util::stream::empty())
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async { Vec::new() })
    }

    fn trigger_action(&self, name: &str, _hold_ms: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        self.sent
            .lock()
            .unwrap()
            .push((name.to_owned(), Instant::now()));
        for f in self.on_send.lock().unwrap().iter() {
            f(name);
        }
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| TriggerResult::new(TriggerStatus::Ok, ""));
        Box::pin(async move { answer })
    }

    fn close(&self) {}
}

pub struct Rig {
    pub engine: Arc<Engine>,
    pub state: Arc<Mutex<StateModel>>,
    pub game: Arc<TestGame>,
}

/// The state model's clock, on tokio's paused clock.
pub fn paused_state() -> Arc<Mutex<StateModel>> {
    let start = Instant::now();
    Arc::new(Mutex::new(StateModel::new(StateModelOptions {
        stale_ms: 3000,
        now: Arc::new(move || 1_000_000 + start.elapsed().as_millis() as i64),
    })))
}

pub fn rig(answers: &[TriggerStatus]) -> Rig {
    rig_with(TestGame::new(answers))
}

/// The engine as the TS engine tests set it up.
pub fn rig_with(game: TestGame) -> Rig {
    let game = Arc::new(game);
    let state = paused_state();
    let engine = Arc::new(Engine::new(
        game.clone(),
        state.clone(),
        EngineOptions {
            spacing: Duration::from_millis(150),
            retry: Duration::from_millis(400),
            step_timeout: Duration::from_millis(2000),
            poll: Duration::from_millis(100),
            ..EngineOptions::default()
        },
    ));
    Rig {
        engine,
        state,
        game,
    }
}

impl Rig {
    pub fn ingest(&self, frame: &Value) {
        self.state.lock().unwrap().ingest(frame);
    }

    /// Keep frames coming while the engine waits, as the game would.
    pub fn feed(&self, frame: Arc<Mutex<Value>>) -> tokio::task::JoinHandle<()> {
        let state = self.state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let f = frame.lock().unwrap().clone();
                state.lock().unwrap().ingest(&f);
            }
        })
    }
}

pub fn ship(state: Value) -> Value {
    let mut s = json!({ "name": "Test ship" });
    s.as_object_mut()
        .unwrap()
        .extend(state.as_object().unwrap().clone());
    json!({ "vehicle": { "state": s } })
}
