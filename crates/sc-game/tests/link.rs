use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::stream;
use sc_game::{
    BoxFuture, Frame, FrameStream, GameAction, GameApi, LinkOptions, LinkStatus, StreamError,
    TelemetryLink, TriggerResult, TriggerStatus,
};
use serde_json::json;
use tokio::{sync::mpsc, time::Instant};
use tokio_stream::wrappers::UnboundedReceiverStream;

/// A game whose streams the test drives by hand.
#[derive(Default)]
struct ManualGame {
    streams: Mutex<Vec<ManualStream>>,
}

struct ManualStream {
    sender: mpsc::UnboundedSender<Result<Frame, StreamError>>,
    opened_at: Instant,
}

impl ManualGame {
    fn count(&self) -> usize {
        self.streams.lock().unwrap().len()
    }

    /// The link dropped the stream.
    fn cancelled(&self, index: usize) -> bool {
        self.streams.lock().unwrap()[index].sender.is_closed()
    }

    fn open_count(&self) -> usize {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .filter(|s| !s.sender.is_closed())
            .count()
    }

    fn current_opened_at(&self) -> Instant {
        self.streams.lock().unwrap().last().unwrap().opened_at
    }

    fn push(&self, index: Option<usize>, item: Result<Frame, StreamError>) {
        let streams = self.streams.lock().unwrap();
        let stream = index
            .map_or_else(|| streams.last(), |i| streams.get(i))
            .unwrap();
        // A send to a dropped stream fails, as a late callback would be ignored.
        let _ = stream.sender.send(item);
    }

    async fn send(&self, frame: Frame) {
        self.push(None, Ok(frame));
        settle().await;
    }

    async fn send_default(&self) {
        self.send(json!({ "actor": {} })).await;
    }

    async fn fail(&self) {
        self.push(None, Err(StreamError::new("unavailable")));
        settle().await;
    }
}

impl GameApi for ManualGame {
    fn open_stream(&self) -> FrameStream {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.streams.lock().unwrap().push(ManualStream {
            sender,
            opened_at: Instant::now(),
        });
        Box::pin(UnboundedReceiverStream::new(receiver))
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async { Vec::new() })
    }

    fn trigger_action(&self, _: &str, _: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        Box::pin(async { TriggerResult::new(TriggerStatus::Ok, "") })
    }

    fn close(&self) {}
}

/// Lets every task woken so far run.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// With the clock paused, tokio fires each timer due before `ms` at its own time, in order.
async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    settle().await;
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Frame>>>);

impl Recorder {
    fn listener(&self) -> impl Fn(&Frame) + Send + Sync + 'static {
        let frames = self.0.clone();
        move |frame| frames.lock().unwrap().push(frame.clone())
    }

    fn frames(&self) -> Vec<Frame> {
        self.0.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

fn setup() -> (Arc<ManualGame>, TelemetryLink) {
    setup_with(LinkOptions::default())
}

fn setup_with(options: LinkOptions) -> (Arc<ManualGame>, TelemetryLink) {
    let game = Arc::new(ManualGame::default());
    let link = TelemetryLink::new(game.clone(), options);
    (game, link)
}

#[tokio::test(start_paused = true)]
async fn opens_one_stream_for_the_first_subscriber_and_closes_it_with_the_last() {
    let (game, link) = setup();
    let a = Recorder::default();
    let b = Recorder::default();
    let subscription_a = link.subscribe(a.listener());
    let subscription_b = link.subscribe(b.listener());
    assert_eq!(game.count(), 1);

    game.send(json!({ "actor": { "state": {} } })).await;
    assert_eq!(a.frames(), vec![json!({ "actor": { "state": {} } })]);
    assert_eq!(b.count(), 1);

    // Unsubscribing twice cannot happen: unsubscribe consumes the subscription.
    subscription_a.unsubscribe();
    settle().await;
    assert!(!game.cancelled(0));
    game.send_default().await;
    assert_eq!(a.count(), 1);
    assert_eq!(b.count(), 2);

    subscription_b.unsubscribe();
    settle().await;
    assert!(game.cancelled(0));
    assert_eq!(link.status(), LinkStatus::Idle);

    let _again = link.subscribe(a.listener());
    assert_eq!(game.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn lets_the_same_function_subscribe_twice_and_unsubscribe_each_independently() {
    let (game, link) = setup();
    let recorder = Recorder::default();
    let listener = Arc::new(recorder.listener());
    let first = link.subscribe({
        let listener = listener.clone();
        move |frame| listener(frame)
    });
    let _second = link.subscribe(move |frame| listener(frame));
    first.unsubscribe();
    game.send_default().await;
    assert_eq!(recorder.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn reconnects_with_backoff_1_2_4_then_5_s() {
    let (game, link) = setup();
    let _subscription = link.subscribe(|_| {});
    settle().await;
    let mut delays = Vec::new();
    for _ in 0..6 {
        let failed_at = Instant::now();
        game.fail().await;
        let before = game.count();
        advance(10_000).await;
        assert_eq!(game.count(), before + 1);
        delays.push(game.current_opened_at() - failed_at);
    }
    let expected: Vec<Duration> = [1000, 2000, 4000, 5000, 5000, 5000]
        .map(Duration::from_millis)
        .to_vec();
    assert_eq!(delays, expected);
}

#[tokio::test(start_paused = true)]
async fn resets_the_backoff_once_a_frame_arrives() {
    let (game, link) = setup();
    let _subscription = link.subscribe(|_| {});
    settle().await;
    game.fail().await;
    advance(1000).await;
    game.fail().await;
    advance(2000).await;
    game.send_default().await;
    game.fail().await;
    advance(999).await;
    assert_eq!(game.count(), 3);
    advance(1).await;
    assert_eq!(game.count(), 4);
}

#[tokio::test(start_paused = true)]
async fn does_not_reconnect_after_the_last_subscriber_leaves() {
    let (game, link) = setup();
    let subscription = link.subscribe(|_| {});
    settle().await;
    game.fail().await;
    subscription.unsubscribe();
    advance(60_000).await;
    assert_eq!(game.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn reopens_a_stream_that_stays_silent_for_10_s() {
    let (game, link) = setup();
    let _subscription = link.subscribe(|_| {});
    advance(9999).await;
    assert_eq!(game.count(), 1);
    advance(1).await;
    assert!(game.cancelled(0));
    assert_eq!(game.count(), 2);
    assert_eq!(game.open_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn measures_silence_from_the_last_frame() {
    let (game, link) = setup();
    let _subscription = link.subscribe(|_| {});
    advance(8000).await;
    game.send_default().await;
    advance(9999).await;
    assert_eq!(game.count(), 1);
    advance(1).await;
    assert_eq!(game.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn ignores_the_cancelled_stream_once_it_has_been_replaced() {
    let (game, link) = setup();
    let recorder = Recorder::default();
    let _subscription = link.subscribe(recorder.listener());
    advance(10_000).await;
    game.push(Some(0), Ok(json!({ "stale": true })));
    game.push(Some(0), Err(StreamError::new("late")));
    settle().await;
    assert_eq!(recorder.count(), 0);
    advance(5000).await;
    assert_eq!(game.count(), 2);
}

#[tokio::test(start_paused = true)]
async fn moves_idle_waiting_live_waiting_idle_and_notifies_each_change_once() {
    let (game, link) = setup();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _status = link.on_status({
        let seen = seen.clone();
        move |status| seen.lock().unwrap().push(status)
    });
    assert_eq!(link.status(), LinkStatus::Idle);

    let subscription = link.subscribe(|_| {});
    assert_eq!(link.status(), LinkStatus::Waiting);

    game.send_default().await;
    advance(1000).await;
    game.send_default().await;
    assert_eq!(link.status(), LinkStatus::Live);

    advance(2999).await;
    assert_eq!(link.status(), LinkStatus::Live);
    advance(1).await;
    assert_eq!(link.status(), LinkStatus::Waiting);

    game.send_default().await;
    subscription.unsubscribe();
    use LinkStatus::*;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Waiting, Live, Waiting, Live, Idle]
    );
}

#[tokio::test(start_paused = true)]
async fn keeps_delivering_frames_when_one_subscriber_panics() {
    let errors = Arc::new(AtomicUsize::new(0));
    let (game, link) = setup_with(LinkOptions {
        on_listener_error: {
            let errors = errors.clone();
            Arc::new(move |_| {
                errors.fetch_add(1, Ordering::SeqCst);
            })
        },
        ..Default::default()
    });
    let good = Recorder::default();
    let _bad = link.subscribe(|_| panic!("boom"));
    let _good = link.subscribe(good.listener());
    game.send_default().await;
    assert_eq!(good.count(), 1);
    assert_eq!(errors.load(Ordering::SeqCst), 1);
}

/// The TypeScript test injects a clock and scheduler; here the paused tokio clock is both.
#[tokio::test(start_paused = true)]
async fn honours_custom_silence_and_stale_windows() {
    let (game, link) = setup_with(LinkOptions {
        silence: Duration::from_millis(100),
        stale: Duration::from_millis(50),
        ..Default::default()
    });
    let _subscription = link.subscribe(|_| {});
    game.send_default().await;
    assert_eq!(link.status(), LinkStatus::Live);
    advance(50).await;
    assert_eq!(link.status(), LinkStatus::Waiting);
    advance(50).await;
    assert_eq!(game.count(), 2);
}

/// Opens a stream that fails at once, without a frame.
#[derive(Default)]
struct RefusingGame {
    opened: AtomicUsize,
}

impl GameApi for RefusingGame {
    fn open_stream(&self) -> FrameStream {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Box::pin(stream::iter([Err(StreamError::new("refused"))]))
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async { Vec::new() })
    }

    fn trigger_action(&self, _: &str, _: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        Box::pin(async { TriggerResult::new(TriggerStatus::Ok, "") })
    }

    fn close(&self) {}
}

#[tokio::test(start_paused = true)]
async fn copes_with_a_game_that_ends_the_stream_at_once() {
    let game = Arc::new(RefusingGame::default());
    let link = TelemetryLink::new(game.clone(), LinkOptions::default());
    let _subscription = link.subscribe(|_| {});
    // Opens at 0, 1, 3, 7, 12, 17, 22 and 27 s; a stray silence timer would add more.
    advance(30_000).await;
    assert_eq!(game.opened.load(Ordering::SeqCst), 8);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_link_cancels_the_stream() {
    let (game, link) = setup();
    let _subscription = link.subscribe(|_| {});
    drop(link);
    settle().await;
    assert!(game.cancelled(0));
}
