use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use sc_game::{
    GameAction, GameApi, LinkOptions, LinkStatus, RecordedFrame, ReplayGame, ReplayOptions,
    TelemetryLink, TriggerCall, TriggerResponder, TriggerResult, TriggerStatus, parse_recording,
};
use serde_json::{Value as Json, json};
use tokio::{task::JoinHandle, time::Instant};

fn recording() -> Vec<RecordedFrame> {
    [(1000.0, 1), (1100.0, 2), (1400.0, 3)]
        .map(|(t, n)| RecordedFrame {
            t,
            frame: json!({ "n": n }),
        })
        .to_vec()
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    settle().await;
}

struct Playback {
    /// (ms since start, frame.n)
    frames: Arc<Mutex<Vec<(u64, Json)>>>,
    ends: Arc<AtomicUsize>,
    errors: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Playback {
    fn times(&self) -> Vec<u64> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .map(|(at, _)| *at)
            .collect()
    }
}

fn play(game: &ReplayGame) -> Playback {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let ends = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let mut stream = game.open_stream();
    let task = tokio::spawn({
        let (frames, ends, errors) = (frames.clone(), ends.clone(), errors.clone());
        async move {
            let start = Instant::now();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(frame) => {
                        let at = start.elapsed().as_millis() as u64;
                        frames.lock().unwrap().push((at, frame["n"].clone()));
                    }
                    Err(_) => {
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            ends.fetch_add(1, Ordering::SeqCst);
        }
    });
    Playback {
        frames,
        ends,
        errors,
        task,
    }
}

fn replay(frames: Vec<RecordedFrame>, options: ReplayOptions) -> ReplayGame {
    ReplayGame::new(frames, options).unwrap()
}

fn looped() -> ReplayOptions {
    ReplayOptions {
        loop_playback: true,
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn plays_frames_on_their_relative_timing_then_ends() {
    let playback = play(&replay(recording(), ReplayOptions::default()));
    advance(10_000).await;
    assert_eq!(
        *playback.frames.lock().unwrap(),
        vec![(0, json!(1)), (100, json!(2)), (400, json!(3))]
    );
    assert_eq!(playback.ends.load(Ordering::SeqCst), 1);
    assert_eq!(playback.errors.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn applies_the_speed_factor() {
    let playback = play(&replay(
        recording(),
        ReplayOptions {
            speed: 2.0,
            ..Default::default()
        },
    ));
    advance(10_000).await;
    assert_eq!(playback.times(), vec![0, 50, 200]);
}

#[tokio::test]
async fn rejects_a_speed_that_is_not_positive() {
    for speed in [0.0, -1.0, f64::NAN] {
        assert!(
            ReplayGame::new(
                vec![],
                ReplayOptions {
                    speed,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn loops_with_the_mean_frame_gap_between_cycles() {
    let playback = play(&replay(recording(), looped()));
    advance(1000).await;
    assert_eq!(playback.times(), vec![0, 100, 400, 600, 700, 1000]);
    assert_eq!(playback.ends.load(Ordering::SeqCst), 0);
}

/// Cancelling is dropping the stream, so it ends once and without an error by construction.
#[tokio::test(start_paused = true)]
async fn stops_on_cancel() {
    let playback = play(&replay(recording(), looped()));
    advance(150).await;
    playback.task.abort();
    advance(10_000).await;
    assert_eq!(playback.frames.lock().unwrap().len(), 2);
    assert_eq!(playback.errors.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn close_ends_open_streams_without_an_error() {
    let game = replay(recording(), looped());
    let playback = play(&game);
    advance(150).await;
    game.close();
    advance(10_000).await;
    assert_eq!(playback.frames.lock().unwrap().len(), 2);
    assert_eq!(playback.ends.load(Ordering::SeqCst), 1);
    assert_eq!(playback.errors.load(Ordering::SeqCst), 0);

    let after = play(&game);
    advance(0).await;
    assert_eq!(
        after.frames.lock().unwrap().len(),
        1,
        "streams opened after close still play"
    );
}

#[tokio::test(start_paused = true)]
async fn hands_out_copies_so_subscribers_cannot_change_the_recording() {
    let game = replay(
        vec![RecordedFrame {
            t: 0.0,
            frame: json!({ "a": { "b": 1 } }),
        }],
        looped(),
    );
    let mut stream = game.open_stream();
    let mut first = stream.next().await.unwrap().unwrap();
    first["a"]["b"] = json!(99);
    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second, json!({ "a": { "b": 1 } }));
    game.close();
}

#[tokio::test(start_paused = true)]
async fn ends_an_empty_recording_asynchronously() {
    let playback = play(&replay(vec![], ReplayOptions::default()));
    assert_eq!(playback.ends.load(Ordering::SeqCst), 0);
    advance(0).await;
    assert_eq!(playback.ends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn records_trigger_calls_and_answers_with_the_configured_result() {
    let game = replay(
        vec![],
        ReplayOptions {
            trigger_result: TriggerResponder::Fixed(TriggerResult::new(
                TriggerStatus::MapInactive,
                "no",
            )),
            ..Default::default()
        },
    );
    assert_eq!(
        game.trigger_action("v_toggle_landing_system", None).await,
        TriggerResult::new(TriggerStatus::MapInactive, "no")
    );
    game.trigger_action("v_weapon_cycle", Some(300)).await;
    assert_eq!(
        game.trigger_calls(),
        vec![
            TriggerCall {
                name: "v_toggle_landing_system".into(),
                hold_ms: None
            },
            TriggerCall {
                name: "v_weapon_cycle".into(),
                hold_ms: Some(300)
            },
        ]
    );
}

#[tokio::test]
async fn answers_ok_by_default_or_from_a_function() {
    let game = replay(vec![], ReplayOptions::default());
    assert_eq!(
        game.trigger_action("x", None).await,
        TriggerResult::new(TriggerStatus::Ok, "")
    );
    let game = replay(
        vec![],
        ReplayOptions {
            trigger_result: TriggerResponder::with(|call| {
                let status = if call.name == "known" {
                    TriggerStatus::Ok
                } else {
                    TriggerStatus::UnknownAction
                };
                TriggerResult::new(status, "")
            }),
            ..Default::default()
        },
    );
    assert_eq!(
        game.trigger_action("other", None).await.status,
        TriggerStatus::UnknownAction
    );
}

#[tokio::test]
async fn lists_the_configured_actions() {
    let actions = vec![GameAction {
        name: "v_toggle_landing_system".into(),
        category: "spaceship_movement".into(),
        holdable: false,
    }];
    let game = replay(
        vec![],
        ReplayOptions {
            actions: actions.clone(),
            ..Default::default()
        },
    );
    assert_eq!(game.list_actions().await, actions);
    assert_eq!(
        replay(vec![], ReplayOptions::default())
            .list_actions()
            .await,
        vec![]
    );
}

#[tokio::test(start_paused = true)]
async fn drives_a_telemetry_link_to_live() {
    let link = TelemetryLink::new(
        Arc::new(replay(recording(), looped())),
        LinkOptions::default(),
    );
    let count = Arc::new(AtomicUsize::new(0));
    let _subscription = link.subscribe({
        let count = count.clone();
        move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    advance(500).await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert_eq!(link.status(), LinkStatus::Live);
    link.close();
}

#[test]
fn parses_jsonl_skipping_blank_lines_and_accepting_crlf() {
    let text = "{\"t\":0,\"frame\":{\"a\":1}}\r\n\n{\"t\":59,\"frame\":{}}\n";
    assert_eq!(
        parse_recording(text).unwrap(),
        vec![
            RecordedFrame {
                t: 0.0,
                frame: json!({ "a": 1 })
            },
            RecordedFrame {
                t: 59.0,
                frame: json!({})
            },
        ]
    );
}

#[test]
fn names_the_bad_line() {
    let error = |text| parse_recording(text).unwrap_err().to_string();
    assert!(error("{\"t\":0,\"frame\":{}}\nnope").contains("line 2: not JSON"));
    assert!(error("{\"t\":\"0\",\"frame\":{}}").contains("line 1: expected"));
    assert!(error("{\"t\":0,\"frame\":[]}").contains("line 1: expected"));
}
