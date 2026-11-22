use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sc_game::{
    Frame, GameAction, GameApi, GameClient, LinkOptions, LinkStatus, Subscription, TelemetryLink,
};
use tokio::time::Instant;

use crate::engine::{
    ActionResult, Engine, EngineOptions, OnOff, ProgressStatus, SequenceProgress, SequenceStore,
    TargetMode, ToggleView, game_action_name,
};
use crate::listeners::{ListenerGuard, Listeners, lock};
use crate::state::{Clock, StateModel, StateModelOptions, ValueView, read_value};

pub struct ControlCoreOptions {
    /// The game connection; the real local API by default.
    pub game: Option<Arc<dyn GameApi>>,
    /// Where the user's sequences are kept; in memory only when `None`.
    pub sequences_file: Option<PathBuf>,
    pub engine: EngineOptions,
    /// Section age after which state is stale.
    pub stale_ms: i64,
}

impl Default for ControlCoreOptions {
    fn default() -> Self {
        Self {
            game: None,
            sequences_file: None,
            engine: EngineOptions::default(),
            stale_ms: 3000,
        }
    }
}

impl fmt::Debug for ControlCoreOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlCoreOptions")
            .field("sequences_file", &self.sequences_file)
            .field("engine", &self.engine)
            .field("stale_ms", &self.stale_ms)
            .finish_non_exhaustive()
    }
}

type ChangeListener = dyn Fn() + Send + Sync;
type FrameListener = dyn Fn(&Frame) + Send + Sync;

/// What the link's frame callback and the change listeners share.
struct Shared {
    state: Arc<Mutex<StateModel>>,
    change_listeners: Listeners<ChangeListener>,
    frame_listeners: Listeners<FrameListener>,
}

impl Shared {
    fn notify(&self) {
        for listener in self.change_listeners.snapshot() {
            listener();
        }
    }

    fn ingest(&self, frame: &Frame) {
        lock(&self.state).ingest(frame);
        // After the lock is released, so a listener can read the state back.
        self.notify();
    }
}

/// The contract every surface (Stream Deck, MIP screens, UFC) uses: read state, call functions
/// by name, follow sequences. Surfaces never hold the game connection; only this core writes to
/// the game.
pub struct ControlCore {
    shared: Arc<Shared>,
    engine: Engine,
    game: Arc<dyn GameApi>,
    link: TelemetryLink,
    store: SequenceStore,
    _link_status: Subscription,
    _progress: ListenerGuard,
}

impl fmt::Debug for ControlCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlCore")
            .field("engine", &self.engine)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// Wall-clock milliseconds read through tokio's clock: monotonic, and paused with it in tests.
fn runtime_clock() -> Clock {
    let start = Instant::now();
    let base = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Arc::new(move || base + i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX))
}

impl ControlCore {
    /// # Panics
    /// Outside a tokio runtime.
    pub fn new(opts: ControlCoreOptions) -> Self {
        let game = opts
            .game
            .unwrap_or_else(|| Arc::new(GameClient::local()) as Arc<dyn GameApi>);
        let state = Arc::new(Mutex::new(StateModel::new(StateModelOptions {
            stale_ms: opts.stale_ms,
            now: runtime_clock(),
        })));
        let shared = Arc::new(Shared {
            state: Arc::clone(&state),
            change_listeners: Listeners::new(),
            frame_listeners: Listeners::new(),
        });
        let engine = Engine::new(Arc::clone(&game), state, opts.engine);
        let link = TelemetryLink::new(
            Arc::clone(&game),
            LinkOptions {
                stale: Duration::from_millis(opts.stale_ms.try_into().unwrap_or(0)),
                ..LinkOptions::default()
            },
        );
        let notify = |shared: &Arc<Shared>| {
            let shared = Arc::clone(shared);
            move || shared.notify()
        };
        let on_status = notify(&shared);
        let on_progress = notify(&shared);
        Self {
            _link_status: link.on_status(move |_| on_status()),
            _progress: engine.on_progress(move |_| on_progress()),
            shared,
            engine,
            game,
            link,
            store: SequenceStore::new(opts.sequences_file),
        }
    }

    /// The state model, shared with the engine. Ingest through [`ControlCore::ingest`] so change
    /// listeners hear of it.
    pub fn state(&self) -> &Arc<Mutex<StateModel>> {
        &self.shared.state
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Start following the game while there is something to show; dropping the subscription stops.
    pub fn follow(&self) -> Subscription {
        let shared = Arc::clone(&self.shared);
        self.link.subscribe(move |frame| {
            shared.ingest(frame);
            for listener in shared.frame_listeners.snapshot() {
                listener(frame);
            }
        })
    }

    /// The game's telemetry stream, for another subscriber beside [`ControlCore::follow`] (the
    /// app's recorder); a subscription keeps it open.
    pub fn telemetry(&self) -> &TelemetryLink {
        &self.link
    }

    /// Merge a frame into the state and tell the change listeners.
    pub fn ingest(&self, frame: &Frame) {
        self.shared.ingest(frame);
    }

    /// Every game frame as it arrives, while something follows the game (the bus forwards these).
    pub fn on_frame(&self, listener: impl Fn(&Frame) + Send + Sync + 'static) -> ListenerGuard {
        self.shared.frame_listeners.add(Arc::new(listener))
    }

    pub fn on_link_status(
        &self,
        listener: impl Fn(LinkStatus) + Send + Sync + 'static,
    ) -> Subscription {
        self.link.on_status(listener)
    }

    pub fn on_progress(
        &self,
        listener: impl Fn(&SequenceProgress) + Send + Sync + 'static,
    ) -> ListenerGuard {
        self.engine.on_progress(listener)
    }

    pub fn link_status(&self) -> LinkStatus {
        self.link.status()
    }

    /// Called on every state change, link status change and sequence step.
    pub fn on_change(&self, listener: impl Fn() + Send + Sync + 'static) -> ListenerGuard {
        self.shared.change_listeners.add(Arc::new(listener))
    }

    /// Fire a game action; accepts a keybinding catalog id ("map.action") or a bare action name.
    pub async fn press(&self, action: &str, confirmed: bool) -> ActionResult {
        self.engine.press(game_action_name(action), confirmed).await
    }

    pub async fn set(&self, toggle_id: &str, to: OnOff) -> ActionResult {
        self.engine.set(toggle_id, to).await
    }

    pub async fn set_mode(&self, to: TargetMode) -> ActionResult {
        self.engine.set_mode(to).await
    }

    /// What a key bound to this action can show, if the core can read its state.
    pub fn view_for_action(&self, action: &str) -> Option<ToggleView> {
        self.engine.view_for_action(game_action_name(action))
    }

    pub fn view_toggle(&self, id: &str) -> Option<ToggleView> {
        self.engine.view_toggle(id)
    }

    pub fn value(&self, id: &str) -> Option<ValueView> {
        read_value(&lock(&self.shared.state), id)
    }

    /// The game's own action list (empty when the game is not reachable).
    pub async fn actions(&self) -> Vec<GameAction> {
        self.game.list_actions().await
    }

    pub fn sequences(&self) -> &SequenceStore {
        &self.store
    }

    pub async fn run_sequence(&self, id: &str) -> SequenceProgress {
        let Some(seq) = self.store.get(id) else {
            return SequenceProgress {
                id: id.to_owned(),
                name: id.to_owned(),
                step: 0,
                of: 0,
                status: ProgressStatus::Failed,
                message: Some(format!("No sequence \"{id}\"")),
                unconfirmed: None,
                skipped: None,
            };
        };
        self.engine.run_sequence(&seq).await
    }

    pub fn running(&self) -> Option<SequenceProgress> {
        self.engine.running()
    }

    pub fn abort(&self) -> bool {
        self.engine.abort()
    }

    pub fn close(&self) {
        self.link.close();
        self.game.close();
    }
}
