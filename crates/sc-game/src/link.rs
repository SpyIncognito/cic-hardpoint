use std::{
    any::Any,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use futures_util::StreamExt;
use tokio::{runtime::Handle, task::JoinHandle, time::Instant};

use crate::{
    lock,
    types::{Frame, FrameStream, GameApi, LinkStatus},
};

/// What a panicking listener panicked with.
pub type ListenerError = Box<dyn Any + Send>;

type FrameListener = Arc<dyn Fn(&Frame) + Send + Sync>;
type StatusListener = Arc<dyn Fn(LinkStatus) + Send + Sync>;

#[derive(Clone)]
pub struct LinkOptions {
    /// Reopen if the stream is open but silent this long (e.g. it opened before spawn).
    pub silence: Duration,
    /// A frame within this window counts as live.
    pub stale: Duration,
    /// Delays before successive reconnects; the last one repeats.
    pub backoff: Vec<Duration>,
    /// Called when a frame or status listener panics, so one bad listener cannot stop the rest.
    pub on_listener_error: Arc<dyn Fn(ListenerError) + Send + Sync>,
}

impl Default for LinkOptions {
    fn default() -> Self {
        Self {
            silence: Duration::from_secs(10),
            stale: Duration::from_secs(3),
            backoff: [1000, 2000, 4000, 5000].map(Duration::from_millis).to_vec(),
            on_listener_error: Arc::new(|error| {
                tracing::error!("telemetry listener failed: {}", panic_message(&error));
            }),
        }
    }
}

impl fmt::Debug for LinkOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkOptions")
            .field("silence", &self.silence)
            .field("stale", &self.stale)
            .field("backoff", &self.backoff)
            .finish_non_exhaustive()
    }
}

fn panic_message(error: &ListenerError) -> &str {
    error
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| error.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic")
}

/// One shared telemetry stream for any number of subscribers: opened with the first, closed
/// with the last, and kept alive in between (reconnect with backoff, reopen when silent).
///
/// Listeners run on the tokio runtime the link was created in. Dropping the link closes it.
pub struct TelemetryLink {
    shared: Arc<Shared>,
}

impl TelemetryLink {
    /// # Panics
    /// Outside a tokio runtime.
    pub fn new(game: Arc<dyn GameApi>, options: LinkOptions) -> Self {
        let shared = Shared {
            game,
            options,
            runtime: Handle::current(),
            state: Mutex::new(State::default()),
        };
        Self {
            shared: Arc::new(shared),
        }
    }

    pub fn subscribe(&self, on_frame: impl Fn(&Frame) + Send + Sync + 'static) -> Subscription {
        let id = {
            let mut state = lock(&self.shared.state);
            let id = state.next_id();
            state.frame_listeners.push((id, Arc::new(on_frame)));
            if state.frame_listeners.len() == 1 {
                self.shared.open(&mut state);
            }
            id
        };
        self.shared.update_status();
        Subscription {
            link: Arc::downgrade(&self.shared),
            id,
            kind: ListenerKind::Frame,
        }
    }

    pub fn status(&self) -> LinkStatus {
        lock(&self.shared.state).status(self.shared.options.stale)
    }

    pub fn on_status(&self, listener: impl Fn(LinkStatus) + Send + Sync + 'static) -> Subscription {
        let mut state = lock(&self.shared.state);
        let id = state.next_id();
        state.status_listeners.push((id, Arc::new(listener)));
        Subscription {
            link: Arc::downgrade(&self.shared),
            id,
            kind: ListenerKind::Status,
        }
    }

    /// Drops every frame subscriber and closes the stream. The game stays open; its owner
    /// closes it.
    pub fn close(&self) {
        self.shared.close();
    }
}

impl Drop for TelemetryLink {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// A frame or status listener; dropping it unsubscribes.
#[must_use = "dropping a Subscription unsubscribes it"]
pub struct Subscription {
    link: Weak<Shared>,
    id: u64,
    kind: ListenerKind,
}

impl Subscription {
    pub fn unsubscribe(self) {}
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(link) = self.link.upgrade() {
            link.remove_listener(self.kind, self.id);
        }
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum ListenerKind {
    Frame,
    Status,
}

#[derive(Debug, Clone, Copy)]
enum TimerKind {
    Reconnect,
    Silence,
    Stale,
}

/// A sequence number tells a timer that fires after being cleared or replaced that it is stale,
/// since aborting its task cannot stop one already waiting for the lock.
struct Timer {
    seq: u64,
    task: JoinHandle<()>,
}

struct Shared {
    game: Arc<dyn GameApi>,
    options: LinkOptions,
    runtime: Handle,
    state: Mutex<State>,
}

struct State {
    frame_listeners: Vec<(u64, FrameListener)>,
    status_listeners: Vec<(u64, StatusListener)>,
    next_id: u64,
    /// The task reading the current stream; aborting it drops (cancels) the stream.
    stream: Option<JoinHandle<()>>,
    /// Bumped whenever the current stream is dropped, so its late frames are ignored.
    generation: u64,
    attempt: usize,
    opened_at: Option<Instant>,
    last_frame_at: Option<Instant>,
    reconnect_timer: Option<Timer>,
    silence_timer: Option<Timer>,
    stale_timer: Option<Timer>,
    last_status: LinkStatus,
}

impl Default for State {
    fn default() -> Self {
        Self {
            frame_listeners: Vec::new(),
            status_listeners: Vec::new(),
            next_id: 0,
            stream: None,
            generation: 0,
            attempt: 0,
            opened_at: None,
            last_frame_at: None,
            reconnect_timer: None,
            silence_timer: None,
            stale_timer: None,
            last_status: LinkStatus::Idle,
        }
    }
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn status(&self, stale: Duration) -> LinkStatus {
        if self.frame_listeners.is_empty() {
            return LinkStatus::Idle;
        }
        match self.last_frame_at {
            Some(at) if at.elapsed() < stale => LinkStatus::Live,
            _ => LinkStatus::Waiting,
        }
    }

    fn timer(&mut self, kind: TimerKind) -> &mut Option<Timer> {
        match kind {
            TimerKind::Reconnect => &mut self.reconnect_timer,
            TimerKind::Silence => &mut self.silence_timer,
            TimerKind::Stale => &mut self.stale_timer,
        }
    }

    fn clear_timer(&mut self, kind: TimerKind) {
        if let Some(timer) = self.timer(kind).take() {
            timer.task.abort();
        }
    }

    /// Stops listening to the current stream (if any) without touching subscribers.
    fn drop_stream(&mut self) {
        self.generation += 1;
        if let Some(stream) = self.stream.take() {
            stream.abort();
        }
        self.clear_timer(TimerKind::Silence);
    }

    fn shutdown(&mut self) {
        self.drop_stream();
        self.clear_timer(TimerKind::Reconnect);
        self.clear_timer(TimerKind::Stale);
        self.attempt = 0;
        self.last_frame_at = None;
    }
}

impl Shared {
    fn open(self: &Arc<Self>, state: &mut State) {
        state.generation += 1;
        state.opened_at = Some(Instant::now());
        let stream = self.game.open_stream();
        let pump = pump(Arc::downgrade(self), state.generation, stream);
        state.stream = Some(self.runtime.spawn(pump));
        self.set_timer(state, TimerKind::Silence, self.options.silence);
    }

    fn handle_frame(self: &Arc<Self>, generation: u64, frame: &Frame) {
        let listeners: Vec<FrameListener> = {
            let mut state = lock(&self.state);
            if state.generation != generation {
                return;
            }
            state.last_frame_at = Some(Instant::now());
            state.attempt = 0;
            state
                .frame_listeners
                .iter()
                .map(|(_, listener)| listener.clone())
                .collect()
        };
        for listener in listeners {
            self.guard(|| listener(frame));
        }
        {
            let mut state = lock(&self.state);
            // A listener may have closed the link meanwhile.
            if state.generation == generation && state.stale_timer.is_none() {
                self.set_timer(&mut state, TimerKind::Stale, self.options.stale);
            }
        }
        self.update_status();
    }

    fn handle_end(self: &Arc<Self>, generation: u64) {
        let mut state = lock(&self.state);
        if state.generation != generation {
            return;
        }
        state.drop_stream();
        if state.frame_listeners.is_empty() {
            return;
        }
        let backoff = &self.options.backoff;
        let delay = backoff
            .get(state.attempt.min(backoff.len().saturating_sub(1)))
            .copied()
            .unwrap_or_default();
        state.attempt += 1;
        self.set_timer(&mut state, TimerKind::Reconnect, delay);
    }

    fn set_timer(self: &Arc<Self>, state: &mut State, kind: TimerKind, delay: Duration) {
        state.clear_timer(kind);
        let seq = state.next_id();
        let link = Arc::downgrade(self);
        let task = self.runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(link) = link.upgrade() {
                link.on_timer(kind, seq);
            }
        });
        *state.timer(kind) = Some(Timer { seq, task });
    }

    // The silence and stale checks re-arm lazily instead of resetting a timer on every frame
    // (about 17 a second).
    fn on_timer(self: &Arc<Self>, kind: TimerKind, seq: u64) {
        let notify = {
            let mut state = lock(&self.state);
            let slot = state.timer(kind);
            if slot.as_ref().map(|timer| timer.seq) != Some(seq) {
                return;
            }
            *slot = None;
            match kind {
                TimerKind::Reconnect => {
                    self.open(&mut state);
                    false
                }
                TimerKind::Silence => {
                    let opened_at = state.opened_at.unwrap_or_else(Instant::now);
                    let heard_at = state
                        .last_frame_at
                        .map_or(opened_at, |at| at.max(opened_at));
                    match remaining(heard_at + self.options.silence) {
                        Some(delay) => self.set_timer(&mut state, TimerKind::Silence, delay),
                        None => {
                            state.drop_stream();
                            self.open(&mut state);
                        }
                    }
                    false
                }
                TimerKind::Stale => {
                    let Some(last_frame_at) = state.last_frame_at else {
                        return;
                    };
                    match remaining(last_frame_at + self.options.stale) {
                        Some(delay) => {
                            self.set_timer(&mut state, TimerKind::Stale, delay);
                            false
                        }
                        None => true,
                    }
                }
            }
        };
        if notify {
            self.update_status();
        }
    }

    fn remove_listener(&self, kind: ListenerKind, id: u64) {
        let mut state = lock(&self.state);
        match kind {
            ListenerKind::Frame => {
                let Some(index) = state.frame_listeners.iter().position(|(i, _)| *i == id) else {
                    return;
                };
                state.frame_listeners.remove(index);
                if state.frame_listeners.is_empty() {
                    state.shutdown();
                }
                drop(state);
                self.update_status();
            }
            ListenerKind::Status => state.status_listeners.retain(|(i, _)| *i != id),
        }
    }

    fn close(&self) {
        {
            let mut state = lock(&self.state);
            state.frame_listeners.clear();
            state.shutdown();
        }
        self.update_status();
    }

    fn update_status(&self) {
        let (status, listeners): (LinkStatus, Vec<StatusListener>) = {
            let mut state = lock(&self.state);
            let status = state.status(self.options.stale);
            if status == state.last_status {
                return;
            }
            state.last_status = status;
            let listeners = state
                .status_listeners
                .iter()
                .map(|(_, l)| l.clone())
                .collect();
            (status, listeners)
        };
        for listener in listeners {
            self.guard(|| listener(status));
        }
    }

    fn guard(&self, call: impl FnOnce()) {
        if let Err(error) = catch_unwind(AssertUnwindSafe(call)) {
            (self.options.on_listener_error)(error);
        }
    }
}

/// Time left until `deadline`, or `None` if it has passed.
fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

async fn pump(link: Weak<Shared>, generation: u64, mut stream: FrameStream) {
    while let Some(item) = stream.next().await {
        let Some(shared) = link.upgrade() else {
            return;
        };
        match item {
            Ok(frame) => shared.handle_frame(generation, &frame),
            Err(error) => {
                tracing::debug!("telemetry stream failed: {error}");
                break;
            }
        }
    }
    drop(stream);
    if let Some(shared) = link.upgrade() {
        shared.handle_end(generation);
    }
}
