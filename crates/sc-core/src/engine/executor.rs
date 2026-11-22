use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sc_game::{GameApi, TriggerResult, TriggerStatus};
use tokio::time::{Instant, sleep, sleep_until};

use super::catalog::{
    MASTER_MODE, MasterMode, ToggleDef, ToggleView, needs_confirm, toggle_by_id, toggle_for_action,
    view_toggle,
};
use super::result::{
    ActionResult, FailStatus, Failure, OkStatus, ProgressStatus, SequenceProgress,
};
use super::sequences::{OnOff, Sequence, Step, TargetMode};
use crate::listeners::{ListenerGuard, Listeners, lock};
use crate::state::{Freshness, StateModel};

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Only act on state younger than this, in the state model's milliseconds.
    pub fresh_ms: i64,
    /// Least time between two actions sent to the game.
    pub spacing: Duration,
    /// Wait before the one retry after RATE_LIMITED.
    pub retry: Duration,
    /// How long a sequence step waits for the game to confirm.
    pub step_timeout: Duration,
    /// An extra pause after every action step. Zero by default: sequences say their own pauses.
    pub settle: Duration,
    pub poll: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            fresh_ms: 1000,
            spacing: Duration::from_millis(150),
            retry: Duration::from_millis(400),
            step_timeout: Duration::from_millis(5000),
            settle: Duration::ZERO,
            poll: Duration::from_millis(100),
        }
    }
}

type ProgressListener = dyn Fn(&SequenceProgress) + Send + Sync;

struct Run {
    aborted: AtomicBool,
    progress: Mutex<SequenceProgress>,
}

impl Run {
    fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    fn update(&self, change: impl FnOnce(&mut SequenceProgress)) -> SequenceProgress {
        let mut progress = lock(&self.progress);
        change(&mut progress);
        progress.clone()
    }
}

const STOPPED_RUNNING: &str = "Stopped the running sequence";

fn aborted_press() -> ActionResult {
    ActionResult::fail(FailStatus::AbortedSequence, STOPPED_RUNNING)
}

fn stopped() -> ActionResult {
    ActionResult::fail(FailStatus::AbortedSequence, "Stopped")
}

/// `TriggerStatus` as the game names it, e.g. "RATE_LIMITED".
fn status_name(status: TriggerStatus) -> String {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{status:?}"),
    }
}

/// The only writer to the game. Every surface goes through it, so actions are spaced, the confirm
/// group is enforced in one place, and one sequence runs at a time across all devices.
pub struct Engine {
    game: Arc<dyn GameApi>,
    state: Arc<Mutex<StateModel>>,
    opts: EngineOptions,
    /// Held while an action is sent, so sends go out one at a time in order; holds the last send.
    last_sent: tokio::sync::Mutex<Option<Instant>>,
    sequence: Mutex<Option<Arc<Run>>>,
    progress_listeners: Listeners<ProgressListener>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(game: Arc<dyn GameApi>, state: Arc<Mutex<StateModel>>, opts: EngineOptions) -> Self {
        Self {
            game,
            state,
            opts,
            last_sent: tokio::sync::Mutex::new(None),
            sequence: Mutex::new(None),
            progress_listeners: Listeners::new(),
        }
    }

    // ------------------------------------------------------------------ presses

    /// A key bound to a game action. Fires it as the game's own action, like the key would.
    pub async fn press(&self, action: &str, confirmed: bool) -> ActionResult {
        if self.abort() {
            return aborted_press();
        }
        if needs_confirm(action) && !confirmed {
            return ActionResult::fail(
                FailStatus::NeedsConfirm,
                format!("\"{action}\" needs a hold to confirm"),
            );
        }
        self.send(action).await
    }

    /// Bring a toggle to on or off, acting only if it is not there already.
    pub async fn set(&self, toggle_id: &str, to: OnOff) -> ActionResult {
        if self.abort() {
            return aborted_press();
        }
        let Some(t) = toggle_by_id(toggle_id) else {
            return ActionResult::fail(
                FailStatus::Unknown,
                format!("Unknown toggle \"{toggle_id}\""),
            );
        };
        self.set_toggle(t, to.is_on()).await
    }

    /// Set the master mode to NAV or SCM, acting only if it is not there already.
    pub async fn set_mode(&self, to: TargetMode) -> ActionResult {
        if self.abort() {
            return aborted_press();
        }
        self.set_master_mode(to).await
    }

    // ------------------------------------------------------------------ views

    /// The state a key bound to this game action can show, if the core can read it.
    pub fn view_for_action(&self, action: &str) -> Option<ToggleView> {
        self.view_toggle(toggle_for_action(action)?.id)
    }

    pub fn view_toggle(&self, id: &str) -> Option<ToggleView> {
        view_toggle(&lock(&self.state), id)
    }

    // ------------------------------------------------------------------ sequences

    pub fn running(&self) -> Option<SequenceProgress> {
        lock(&self.sequence)
            .as_ref()
            .map(|run| lock(&run.progress).clone())
    }

    /// Called on every step of a sequence and when it ends.
    pub fn on_progress(
        &self,
        listener: impl Fn(&SequenceProgress) + Send + Sync + 'static,
    ) -> ListenerGuard {
        self.progress_listeners.add(Arc::new(listener))
    }

    /// Stop the running sequence; true if one was running.
    pub fn abort(&self) -> bool {
        lock(&self.sequence)
            .as_ref()
            .is_some_and(|run| !run.aborted.swap(true, Ordering::SeqCst))
    }

    /// Run a sequence: each step waits for the game to confirm the previous one; any press, set
    /// or abort stops it. A second sequence while one runs stops the first and does not start.
    /// Dropping the future stops the sequence too.
    pub async fn run_sequence(&self, seq: &Sequence) -> SequenceProgress {
        let of = seq.steps.len();
        let progress = SequenceProgress {
            id: seq.id.clone(),
            name: seq.name.clone(),
            step: 0,
            of,
            status: ProgressStatus::Running,
            message: None,
            unconfirmed: None,
            skipped: None,
        };
        if self.abort() {
            return SequenceProgress {
                status: ProgressStatus::Aborted,
                message: Some(STOPPED_RUNNING.to_owned()),
                ..progress
            };
        }
        let run = Arc::new(Run {
            aborted: AtomicBool::new(false),
            progress: Mutex::new(progress),
        });
        *lock(&self.sequence) = Some(Arc::clone(&run));
        let _registered = Registered {
            engine: self,
            run: &run,
        };

        for (i, step) in seq.steps.iter().enumerate() {
            let p = run.update(|p| p.step = i + 1);
            self.emit(&p);
            if run.aborted() {
                return self.finish(&run, ProgressStatus::Aborted, "Stopped");
            }
            let r = self.step(step, &run).await;
            if run.aborted() {
                return self.finish(&run, ProgressStatus::Aborted, "Stopped");
            }
            match r {
                ActionResult::Failed(f) => {
                    return self.finish(&run, ProgressStatus::Failed, &f.message);
                }
                ActionResult::Ok(OkStatus::Sent) => {
                    run.update(|p| p.unconfirmed = Some(p.unconfirmed.unwrap_or(0) + 1));
                }
                ActionResult::Ok(OkStatus::Skipped) => {
                    run.update(|p| p.skipped = Some(p.skipped.unwrap_or(0) + 1));
                }
                ActionResult::Ok(_) => {}
            }
            if matches!(step, Step::Action { .. }) && !self.opts.settle.is_zero() && i + 1 < of {
                self.pause(self.opts.settle, &run).await;
            }
        }
        self.finish(&run, ProgressStatus::Done, "")
    }

    fn finish(&self, run: &Run, status: ProgressStatus, message: &str) -> SequenceProgress {
        let p = run.update(|p| {
            p.status = status;
            if !message.is_empty() {
                p.message = Some(message.to_owned());
            }
        });
        self.emit(&p);
        p
    }

    fn emit(&self, p: &SequenceProgress) {
        for listener in self.progress_listeners.snapshot() {
            listener(p);
        }
    }

    async fn step(&self, step: &Step, run: &Run) -> ActionResult {
        let timeout = match step {
            Step::Set { timeout_s, .. } | Step::Mode { timeout_s, .. } => timeout_s
                .filter(|s| *s != 0.0 && !s.is_nan())
                .map_or(self.opts.step_timeout, duration_from_secs),
            Step::Action { .. } | Step::Pause { .. } => self.opts.step_timeout,
        };
        match step {
            Step::Pause { seconds } => {
                self.pause(duration_from_secs(*seconds), run).await;
                if run.aborted() {
                    stopped()
                } else {
                    ActionResult::Ok(OkStatus::Done)
                }
            }
            Step::Action { action } => {
                if needs_confirm(action) {
                    return ActionResult::fail(
                        FailStatus::NeedsConfirm,
                        format!("\"{action}\" cannot run inside a sequence"),
                    );
                }
                self.send(action).await
            }
            Step::Set { toggle, to, .. } => self.set_step(toggle, *to, run, timeout).await,
            Step::Mode { to, .. } => {
                let r = self.set_master_mode(*to).await;
                if !matches!(r, ActionResult::Ok(OkStatus::Done)) {
                    return r;
                }
                self.wait_for(
                    || self.mode() == Some((*to).into()),
                    &format!("Master mode did not change to {to}"),
                    run,
                    timeout,
                )
                .await
            }
        }
    }

    async fn set_step(
        &self,
        toggle: &str,
        to: OnOff,
        run: &Run,
        timeout: Duration,
    ) -> ActionResult {
        let Some(t) = toggle_by_id(toggle) else {
            return ActionResult::fail(FailStatus::Unknown, format!("Unknown toggle \"{toggle}\""));
        };
        let target = to.is_on();
        // Right after power-on the game may not report this state yet: wait for it.
        let known = self
            .wait_for(
                || {
                    self.view_toggle(t.id)
                        .is_some_and(|v| v.on.is_some() || v.moving_to.is_some())
                },
                "",
                run,
                timeout,
            )
            .await;
        match known {
            ActionResult::Failed(Failure {
                status: FailStatus::AbortedSequence,
                ..
            }) => return known,
            ActionResult::Failed(_) => {
                // Never reported (not every ship reports every state): send the set action blind,
                // which is safe; a bare toggle could flip the wrong way, so that step is skipped.
                let action = if target { t.on } else { t.off };
                let Some(action) = action.filter(|a| !needs_confirm(a)) else {
                    return ActionResult::Ok(OkStatus::Skipped);
                };
                let sent = self.send(action).await;
                return if sent.is_ok() {
                    ActionResult::Ok(OkStatus::Sent)
                } else {
                    sent
                };
            }
            ActionResult::Ok(_) => {}
        }
        let r = self.set_toggle(t, target).await;
        if !matches!(r, ActionResult::Ok(OkStatus::Done)) {
            return r;
        }
        self.wait_for(
            || {
                self.view_toggle(t.id)
                    .is_some_and(|v| v.on == Some(target) || v.moving_to == Some(target))
            },
            &format!("{} did not turn {to}", t.label),
            run,
            timeout,
        )
        .await
    }

    async fn pause(&self, length: Duration, run: &Run) {
        let until = Instant::now() + length;
        loop {
            let now = Instant::now();
            if now >= until || run.aborted() {
                return;
            }
            sleep(self.opts.poll.min(until - now)).await;
        }
    }

    async fn wait_for(
        &self,
        done: impl Fn() -> bool,
        message: &str,
        run: &Run,
        timeout: Duration,
    ) -> ActionResult {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if run.aborted() {
                return stopped();
            }
            if done() {
                return ActionResult::Ok(OkStatus::Done);
            }
            sleep(self.opts.poll).await;
        }
        ActionResult::fail(FailStatus::Timeout, message)
    }

    // ------------------------------------------------------------------ internals

    fn mode(&self) -> Option<MasterMode> {
        let state = lock(&self.state);
        let r = state.read(MASTER_MODE.path, None);
        let fresh =
            r.freshness == Freshness::Fresh && r.age_ms.is_some_and(|a| a <= self.opts.fresh_ms);
        fresh.then(|| MASTER_MODE.read(r.value)).flatten()
    }

    fn in_vehicle(&self) -> bool {
        lock(&self.state).in_vehicle()
    }

    async fn set_master_mode(&self, to: TargetMode) -> ActionResult {
        if !self.in_vehicle() {
            return ActionResult::fail(FailStatus::NotInVehicle, "Not in a vehicle");
        }
        match self.mode() {
            None => ActionResult::fail(FailStatus::Stale, "Master mode is not known right now"),
            Some(current) if current == to.into() => ActionResult::Ok(OkStatus::Already),
            Some(_) => {
                let action = MASTER_MODE
                    .set(to.into())
                    .expect("NAV and SCM have set actions");
                self.send(action).await
            }
        }
    }

    async fn set_toggle(&self, t: &ToggleDef, target: bool) -> ActionResult {
        let action = {
            let state = lock(&self.state);
            if !state.in_vehicle() {
                return ActionResult::fail(FailStatus::NotInVehicle, "Not in a vehicle");
            }
            let r = state.read(t.path, t.fallback.as_ref());
            if r.freshness != Freshness::Fresh || r.age_ms.is_none_or(|a| a > self.opts.fresh_ms) {
                return ActionResult::fail(
                    FailStatus::Stale,
                    format!("{}: no fresh state from the game", t.label),
                );
            }
            let on = (t.is_on)(r.value);
            let moving_to = t.moving_to.and_then(|f| f(r.value));
            if on == Some(target) || moving_to == Some(target) {
                return ActionResult::Ok(OkStatus::Already);
            }
            if on.is_none() && moving_to.is_none() {
                return ActionResult::fail(
                    FailStatus::Stale,
                    format!("{}: state not known", t.label),
                );
            }
            if target { t.on } else { t.off }.unwrap_or(t.toggle)
        };
        if needs_confirm(action) {
            return ActionResult::fail(
                FailStatus::NeedsConfirm,
                format!("\"{action}\" needs a hold to confirm"),
            );
        }
        self.send(action).await
    }

    /// Send one action, spaced from the last, with one retry when the game says it is too fast.
    async fn send(&self, action: &str) -> ActionResult {
        let mut last_sent = self.last_sent.lock().await;
        let mut retried = false;
        loop {
            if let Some(at) = *last_sent {
                sleep_until(at + self.opts.spacing).await;
            }
            *last_sent = Some(Instant::now());
            let TriggerResult { status, message } = self.game.trigger_action(action, None).await;
            match status {
                TriggerStatus::Ok => return ActionResult::Ok(OkStatus::Done),
                TriggerStatus::RateLimited if !retried => {
                    retried = true;
                    sleep(self.opts.retry).await;
                }
                _ => {
                    let message = if message.is_empty() {
                        format!("The game answered {}", status_name(status))
                    } else {
                        message
                    };
                    return ActionResult::Failed(Failure {
                        status: FailStatus::Game,
                        message,
                        game: Some(status),
                    });
                }
            }
        }
    }
}

/// Clears the running sequence when its run ends or its future is dropped, unless a newer one
/// took its place.
struct Registered<'a> {
    engine: &'a Engine,
    run: &'a Arc<Run>,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut current = lock(&self.engine.sequence);
        if current.as_ref().is_some_and(|r| Arc::ptr_eq(r, self.run)) {
            *current = None;
        }
    }
}

/// Negative, NaN or overlong lengths (never in a validated sequence) count as no wait.
fn duration_from_secs(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}
