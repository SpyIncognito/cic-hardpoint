//! The ship the player is in: which of its functions the stream reports (greyed controls) and
//! which dashboard goes with it (per-ship dashboards). One task looks at the state after every
//! change and twice a second, so the 10 s and 2 s rules hold while frames are sparse.

pub mod support;
pub mod switcher;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

use sc_core::ControlCore;
use sc_core::engine::ProgressStatus;
use sc_core::state::StateModel;
use sc_core::state::values::{ship_manufacturer, ship_model, ship_name};
use sc_game::LinkStatus;

use crate::dashboard::{DashboardStore, GENERIC_ID, ShipRef};
use support::{Build, Learned, Observation, SupportEntry, SupportFile, Tracker, build_of_actions};
use switcher::{Step, Switcher};

const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The ship as the values push gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipView {
    pub manufacturer: String,
    pub model: String,
    pub class_name: String,
    /// What people see, e.g. "Drake Corsair", or the name the user gave the ship.
    pub name: String,
    /// The name as derived from the stream, before the user's own.
    pub derived_name: String,
    /// `<manufacturer> / <model>`, e.g. "Drake Interplanetary / Corsair". Dashboards match on
    /// this and the class name, never on the shown name.
    pub key: String,
    /// The dashboard made for this ship, when there is one.
    pub dashboard: Option<String>,
}

impl ShipView {
    pub fn ship_ref(&self) -> ShipRef {
        ShipRef {
            manufacturer: self.manufacturer.clone(),
            model: self.model.clone(),
            class_name: Some(self.class_name.clone()),
        }
    }
}

/// The ship from the state, while the player is in it and the report is fresh.
pub fn ship_of(state: &StateModel) -> Option<ShipView> {
    if !state.in_vehicle() {
        return None;
    }
    let text = |path| {
        state
            .read(path, None)
            .value
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let class_name = text("vehicle.state.class_name");
    if class_name.is_empty() {
        return None;
    }
    let manufacturer = ship_manufacturer(&text("vehicle.state.manufacturer"), &class_name);
    let model = ship_model(&class_name);
    let derived_name = ship_name(&text("vehicle.state.name"), &class_name);
    let name = state
        .ship_name_for(&class_name)
        .map_or_else(|| derived_name.clone(), str::to_owned);
    Some(ShipView {
        key: if manufacturer.is_empty() {
            model.clone()
        } else {
            format!("{manufacturer} / {model}")
        },
        manufacturer,
        model,
        class_name,
        name,
        derived_name,
        dashboard: None,
    })
}

/// What the values push carries: the ship and each greyed id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub ship: Option<ShipView>,
    pub support: Vec<(&'static str, SupportEntry)>,
}

impl Snapshot {
    pub fn greyed(&self, id: &str) -> bool {
        self.support.iter().any(|(i, _)| *i == id)
    }
}

#[derive(Debug)]
struct Inner {
    tracker: Tracker,
    switcher: Switcher,
    build: Build,
    snapshot: Snapshot,
    /// The ship the switcher recognised last, for a switch that waited.
    recognised: Option<ShipRef>,
}

/// Shared by the watch task and the page sessions.
#[derive(Debug)]
pub struct Ships {
    inner: Mutex<Inner>,
    file: SupportFile,
    changes: watch::Sender<u64>,
}

impl Ships {
    pub fn new(file: SupportFile) -> Self {
        let learned = file.load();
        Self {
            inner: Mutex::new(Inner {
                tracker: Tracker::new(learned),
                switcher: Switcher::default(),
                build: Build::Pending,
                snapshot: Snapshot::default(),
                recognised: None,
            }),
            file,
            changes: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.lock().snapshot.clone()
    }

    pub fn learned(&self) -> Vec<Learned> {
        self.lock().tracker.learned().to_vec()
    }

    pub fn build(&self) -> Build {
        self.lock().build.clone()
    }

    /// Changes whenever the snapshot does.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// A dashboard picked by hand: a switch waiting for a sequence is dropped.
    pub fn picked(&self) {
        self.lock().switcher.picked();
    }

    /// Forget the learned functions of one class name, or of every ship.
    pub fn reset(&self, class_name: Option<&str>) -> std::io::Result<usize> {
        let mut inner = self.lock();
        let removed = inner.tracker.reset(class_name);
        inner.tracker.take_dirty();
        self.file.save(inner.tracker.learned())?;
        inner.snapshot.support.clear();
        drop(inner);
        self.changes.send_modify(|n| *n += 1);
        Ok(removed)
    }

    /// Learn the build key from the game's action list, once per stream.
    pub fn set_build(&self, build: Build) {
        self.lock().build = build;
    }

    /// One look, at `now_ms`: learn, grey, and switch dashboards when the ship changed.
    pub fn look(
        &self,
        now_ms: i64,
        state: &StateModel,
        running: bool,
        dashboards: &DashboardStore,
    ) {
        let obs = Observation::of(state);
        let mut ship = ship_of(state);
        let mut inner = self.lock();
        let build = inner.build.clone();
        inner.tracker.observe(now_ms, &obs, &build);
        if inner.tracker.take_dirty()
            && let Err(e) = self.file.save(inner.tracker.learned())
        {
            tracing::warn!("[ship-support] cannot save: {e}");
        }
        let on = dashboards.switch_on_ship_change();
        let step = inner
            .switcher
            .step(now_ms, ship.as_ref().map(|s| s.key.as_str()), running, on);
        if let Some(s) = &ship
            && inner.switcher.recognised() == Some(s.key.as_str())
        {
            inner.recognised = Some(s.ship_ref());
        }
        match step {
            Step::Nothing => {}
            Step::Recognised(_) => dashboards.unpin(),
            Step::SwitchFor(_) => {
                let target = inner
                    .recognised
                    .as_ref()
                    .and_then(|s| dashboards.match_ship(s))
                    .unwrap_or_else(|| GENERIC_ID.to_owned());
                tracing::info!("[dashboards] ship changed: showing {target}");
                if let Err(e) = dashboards.select(&target, false) {
                    tracing::warn!("[dashboards] cannot save: {e}");
                }
            }
        }
        if let Some(s) = &mut ship {
            s.dashboard = dashboards.match_ship(&s.ship_ref());
        }
        let snapshot = Snapshot {
            ship,
            support: inner.tracker.support(&build),
        };
        let changed = snapshot != inner.snapshot;
        inner.snapshot = snapshot;
        drop(inner);
        if changed {
            self.changes.send_modify(|n| *n += 1);
        }
    }
}

/// The watch task: until `closing` turns true.
pub async fn watch(
    core: Arc<ControlCore>,
    ships: Arc<Ships>,
    dashboards: Arc<DashboardStore>,
    mut closing: watch::Receiver<bool>,
) {
    let changed = Arc::new(Notify::new());
    let on_change = Arc::clone(&changed);
    let _guard = core.on_change(move || on_change.notify_one());
    let mut boards = dashboards.subscribe();
    let start = Instant::now();
    loop {
        tokio::select! {
            () = changed.notified() => {}
            r = boards.changed() => if r.is_err() { return },
            () = tokio::time::sleep(LOOK_EVERY) => {}
            _ = closing.changed() => return,
        }
        let live = core.link_status() == LinkStatus::Live;
        match (ships.build(), live) {
            (Build::Pending, true) => {
                let actions = core.actions().await;
                let build = build_of_actions(actions.iter().map(|a| a.name.as_str()));
                tracing::info!(
                    "[ship-support] game build key: {}",
                    build
                        .as_deref()
                        .unwrap_or("none (the game listed no actions)")
                );
                ships.set_build(Build::Known(build));
            }
            // The stream stopped: the game may come back as another build.
            (Build::Known(_), false) => {
                ships.set_build(Build::Pending);
            }
            _ => {}
        }
        let running = core
            .running()
            .is_some_and(|p| p.status == ProgressStatus::Running);
        let now_ms = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
        let state = core.state().lock().unwrap_or_else(PoisonError::into_inner);
        ships.look(now_ms, &state, running, &dashboards);
    }
}
