//! DEMO ONLY: mock fleet peers replayed from telemetry recordings, so the fleet panel can be shown
//! before the fleet transport exists. Off unless the service is started with `--fleet-demo`.
//!
//! Each mock peer replays a recording into its own state model, offset in time and looped, and
//! builds its card with the real card builder every [`CARD_EVERY`], as a real sender would. The
//! handles and ship names are made up. Downed, dead and respawn moments are SCRIPTED here, since
//! the recordings have none: they go in as game event frames, through the same path as the
//! recording's own frames.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use sc_core::state::StateModel;
use sc_game::RecordedFrame;

use super::card::{
    EVENT_DEAD, EVENT_DOWNED, EVENT_RESPAWN, LifeTracker, SendConsent, Sender, Sharing, build_card,
    health_fraction,
};
use super::hub::FleetHub;

pub const DEFAULT_PEERS: usize = 5;
pub const MAX_PEERS: usize = 12;
/// How often each mock peer sends its card (the plan's "about twice a second").
pub const CARD_EVERY: Duration = Duration::from_millis(500);
/// Recordings are thinned to one frame in this many milliseconds (event frames are all kept).
const KEEP_EVERY_MS: f64 = 500.0;

/// A recording, thinned, with the sections no card reads left out.
#[derive(Debug, Clone)]
pub struct Recording {
    pub name: String,
    pub frames: Vec<RecordedFrame>,
    pub duration_ms: f64,
}

#[derive(Deserialize)]
struct TimeOnly {
    t: f64,
    #[allow(dead_code)]
    frame: IgnoredAny,
}

/// Drop what no card reads, to keep long recordings small in memory.
fn trim_frame(frame: &mut Value) {
    if let Some(state) = frame.pointer_mut("/vehicle/state/flight")
        && let Some(flight) = state.as_object_mut()
    {
        flight.remove("settings");
    }
    if let Some(weapons) = frame.get_mut("weapons").and_then(Value::as_object_mut) {
        weapons.remove("settings");
    }
    if let Some(actor) = frame.get_mut("actor").and_then(Value::as_object_mut) {
        actor.remove("environment");
    }
    if let Some(fields) = frame.as_object_mut() {
        fields.remove("force_feedback");
    }
}

/// Read a recording made by the recorder (`{ "t": ms, "frame": {...} }` per line).
pub fn read_recording(name: &str, reader: impl BufRead) -> Result<Recording, String> {
    let mut frames: Vec<RecordedFrame> = vec![];
    let mut last_kept = f64::NEG_INFINITY;
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("{name}: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let bad = || format!("{name} line {}: not a recording frame", index + 1);
        let t = serde_json::from_str::<TimeOnly>(&line)
            .map_err(|_| bad())?
            .t;
        let has_events = line.contains("\"events\"");
        if t - last_kept < KEEP_EVERY_MS && !has_events {
            continue;
        }
        let mut entry: RecordedFrame = serde_json::from_str(&line).map_err(|_| bad())?;
        if !entry.frame.is_object() || !entry.t.is_finite() {
            return Err(bad());
        }
        trim_frame(&mut entry.frame);
        last_kept = t;
        frames.push(entry);
    }
    if frames.is_empty() {
        return Err(format!("{name}: no frames"));
    }
    frames.sort_by(|a, b| a.t.total_cmp(&b.t));
    let first = frames[0].t;
    for f in &mut frames {
        f.t -= first;
    }
    let duration_ms = frames.last().map_or(0.0, |f| f.t) + KEEP_EVERY_MS;
    Ok(Recording {
        name: name.to_owned(),
        frames,
        duration_ms,
    })
}

/// The recordings in `sources`: each file, and every `.jsonl` in each folder (by name).
pub fn load(sources: &[PathBuf]) -> Result<Vec<Recording>, String> {
    let mut files = vec![];
    for source in sources {
        if source.is_dir() {
            let mut found: Vec<PathBuf> = fs::read_dir(source)
                .map_err(|e| format!("cannot read {}: {e}", source.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            found.sort();
            files.extend(found);
        } else {
            files.push(source.clone());
        }
    }
    if files.is_empty() {
        return Err("no .jsonl recordings found for --fleet-demo".to_owned());
    }
    files
        .iter()
        .map(|path| {
            let name = file_name(path);
            let file = File::open(path)
                .map_err(|e: io::Error| format!("cannot read {}: {e}", path.display()))?;
            read_recording(&name, BufReader::new(file))
        })
        .collect()
}

/// Load the recordings and set up `peers` mock peers on them, for `--fleet-demo`.
pub fn open(sources: &[PathBuf], peers: usize) -> Result<Demo, String> {
    let recordings = load(sources)?;
    for r in &recordings {
        tracing::info!(
            "[fleet demo] {}: {} frames kept, {:.0} s loop",
            r.name,
            r.frames.len(),
            r.duration_ms / 1000.0
        );
    }
    let demo = Demo::new(recordings, peers);
    let handles: Vec<&str> = demo
        .peers
        .iter()
        .map(|p| p.sender.handle.as_str())
        .collect();
    tracing::info!("[fleet demo] mock peers: {}", handles.join(", "));
    Ok(demo)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// SCRIPTED: one moment in a mock peer's loop. `health` holds the health gauge (0..1) from this
/// moment until the next one; `None` leaves the recording's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    pub at_ms: i64,
    pub event: Option<&'static str>,
    pub health: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub period_ms: i64,
    pub beats: Vec<Beat>,
}

const fn beat(at_s: i64, event: Option<&'static str>, health: Option<f64>) -> Beat {
    Beat {
        at_ms: at_s * 1000,
        event,
        health,
    }
}

impl Script {
    /// SCRIPTED: downed, then dead, then respawned, every 160 s.
    pub fn downed_dead_respawn() -> Self {
        Self {
            period_ms: 160_000,
            beats: vec![
                beat(25, Some(EVENT_DOWNED), Some(0.08)),
                beat(70, Some(EVENT_DEAD), Some(0.0)),
                beat(90, Some(EVENT_RESPAWN), None),
            ],
        }
    }

    /// SCRIPTED: downed, then revived by a medic (the game sends no revive event; health comes
    /// back), every 140 s.
    pub fn downed_revived() -> Self {
        Self {
            period_ms: 140_000,
            beats: vec![
                beat(40, Some(EVENT_DOWNED), Some(0.12)),
                beat(100, None, Some(0.6)),
                beat(115, None, None),
            ],
        }
    }

    fn health_at(&self, t_ms: i64) -> Option<f64> {
        let t = t_ms.rem_euclid(self.period_ms);
        self.beats
            .iter()
            .rev()
            .find(|b| b.at_ms <= t)
            .and_then(|b| b.health)
    }

    /// The events whose moment falls in (from, to], in loop time.
    pub(crate) fn events_between(&self, from_ms: i64, to_ms: i64) -> Vec<&'static str> {
        let mut out = vec![];
        if to_ms <= from_ms {
            return out;
        }
        let first_loop = from_ms.div_euclid(self.period_ms);
        let last_loop = to_ms.div_euclid(self.period_ms);
        for n in first_loop..=last_loop {
            for b in &self.beats {
                let at = n * self.period_ms + b.at_ms;
                if at > from_ms
                    && at <= to_ms
                    && let Some(e) = b.event
                {
                    out.push(e);
                }
            }
        }
        out
    }
}

/// SCRIPTED: a peer that drops off the fleet for a while (no cards sent), every `period_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropout {
    pub period_ms: i64,
    pub from_ms: i64,
    pub to_ms: i64,
}

impl Dropout {
    fn offline_at(self, t_ms: i64) -> bool {
        let t = t_ms.rem_euclid(self.period_ms);
        (self.from_ms..self.to_ms).contains(&t)
    }
}

/// One mock peer: who it says it is, what it replays, and its own game state.
pub struct Peer {
    pub sender: Sender,
    pub ship: &'static str,
    pub recording: usize,
    pub offset_ms: f64,
    pub script: Option<Script>,
    pub dropout: Option<Dropout>,
    state: StateModel,
    life: LifeTracker,
    /// Recording time last played, for this peer.
    played_ms: Option<f64>,
}

/// Made-up handles and ship names, obviously not real people.
const HANDLES: [&str; MAX_PEERS] = [
    "Demo Vega",
    "Demo Rook",
    "Demo Kestrel",
    "Demo Wren",
    "Demo Ash",
    "Demo Orion",
    "Demo Pike",
    "Demo Lumen",
    "Demo Sable",
    "Demo Quill",
    "Demo Tarn",
    "Demo Juno",
];
const SHIPS: [&str; 6] = [
    "Demo Lancer",
    "Demo Courier",
    "Demo Gunship",
    "Demo Scout",
    "Demo Hauler",
    "Demo Interceptor",
];

pub struct Demo {
    recordings: Vec<Recording>,
    peers: Vec<Peer>,
    last_ms: Option<i64>,
}

impl Demo {
    /// `peers` mock peers (1 to [`MAX_PEERS`]). The first ones replay each recording from its
    /// start; the others replay the same recordings from other points in them.
    pub fn new(recordings: Vec<Recording>, peers: usize) -> Self {
        let count = peers.clamp(1, MAX_PEERS);
        let n = recordings.len().max(1);
        let peers = (0..count)
            .map(|i| {
                let recording = i % n;
                // The first pass starts each recording at 0; later passes at other points.
                let pass = i / n;
                let fraction = [0.0, 0.5, 0.25, 0.75, 0.125, 0.625][pass % 6];
                let offset_ms = recordings
                    .get(recording)
                    .map_or(0.0, |r| r.duration_ms * fraction);
                let handle = HANDLES[i];
                let sharing = match i {
                    // Demo Wren shares ship status only; the card has no character status.
                    3 => Sharing {
                        ship: true,
                        character: false,
                        detail: true,
                    },
                    // Demo Ash shares no detail.
                    4 => Sharing {
                        detail: false,
                        ..Sharing::ALL
                    },
                    _ => Sharing::ALL,
                };
                Peer {
                    sender: Sender {
                        member: format!("demo-{}", handle[5..].to_lowercase()),
                        handle: handle.to_owned(),
                        consent: SendConsent::groups(sharing),
                    },
                    ship: SHIPS[i % SHIPS.len()],
                    recording,
                    offset_ms,
                    script: match i {
                        1 => Some(Script::downed_dead_respawn()),
                        2 => Some(Script::downed_revived()),
                        _ => None,
                    },
                    dropout: (i == 3).then_some(Dropout {
                        period_ms: 180_000,
                        from_ms: 60_000,
                        to_ms: 100_000,
                    }),
                    state: StateModel::default(),
                    life: LifeTracker::default(),
                    played_ms: None,
                }
            })
            .collect();
        Self {
            recordings,
            peers,
            last_ms: None,
        }
    }

    pub fn peers(&self) -> &[Peer] {
        &self.peers
    }

    /// Play every peer up to `elapsed_ms` since the demo started, then send each online peer's
    /// card to the hub.
    pub fn step(&mut self, elapsed_ms: i64, hub: &FleetHub) {
        let since = self.last_ms.unwrap_or(-1);
        self.last_ms = Some(elapsed_ms);
        for peer in &mut self.peers {
            let Some(rec) = self.recordings.get(peer.recording) else {
                continue;
            };
            peer.play(rec, since, elapsed_ms);
            if peer.dropout.is_some_and(|d| d.offline_at(elapsed_ms)) {
                continue;
            }
            peer.life
                .observe_health(health_fraction(&peer.state), elapsed_ms);
            let wanted = hub.wanted(&peer.sender.member);
            let card = build_card(&peer.state, &peer.life, elapsed_ms, &peer.sender, &wanted);
            hub.receive(card);
        }
    }
}

impl Peer {
    fn ingest(&mut self, mut frame: Value, health: Option<f64>, now_ms: i64) {
        // The recordings are scrubbed to a placeholder ship name; each mock peer has its own.
        if let Some(state) = frame
            .pointer_mut("/vehicle/state")
            .and_then(Value::as_object_mut)
        {
            state.insert("name".to_owned(), json!(self.ship));
        }
        // SCRIPTED: hold the health gauge where the script says.
        if let Some(h) = health
            && let Some(vitals) = frame
                .pointer_mut("/actor/vitals")
                .and_then(Value::as_object_mut)
        {
            vitals.insert(
                "health".to_owned(),
                json!({ "current": h * 100.0, "max": 100 }),
            );
        }
        self.life.observe_frame(&frame, now_ms);
        self.state.ingest(&frame);
    }

    fn play(&mut self, rec: &Recording, since_ms: i64, now_ms: i64) {
        #[allow(clippy::cast_precision_loss)]
        let target = (now_ms as f64 + self.offset_ms).rem_euclid(rec.duration_ms);
        let health = self.script.as_ref().and_then(|s| s.health_at(now_ms));
        // From a little before the start point on the first step, so the state is complete.
        let from = self.played_ms.unwrap_or(target - 3000.0);
        let index = |t: f64| rec.frames.partition_point(|f| f.t <= t);
        let mut frames: Vec<Value> = vec![];
        if target >= from {
            let range = index(from)..index(target);
            frames.extend(rec.frames[range].iter().map(|f| f.frame.clone()));
            if self.played_ms.is_none() && frames.is_empty() {
                let last = index(target).saturating_sub(1);
                frames.push(rec.frames[last].frame.clone());
            }
        } else {
            // Looped past the end.
            frames.extend(rec.frames[index(from)..].iter().map(|f| f.frame.clone()));
            frames.extend(rec.frames[..index(target)].iter().map(|f| f.frame.clone()));
        }
        for frame in frames {
            self.ingest(frame, health, now_ms);
        }
        if let Some(script) = &self.script {
            for event in script.events_between(since_ms, now_ms) {
                let frame = json!({ "events": { "events": [{ "type": event }] } });
                self.ingest(frame, None, now_ms);
            }
        }
        self.played_ms = Some(target);
    }
}

/// Run the demo until `closing` turns true.
pub fn spawn(
    hub: Arc<FleetHub>,
    mut demo: Demo,
    mut closing: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = Instant::now();
        let mut tick = tokio::time::interval(CARD_EVERY);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = closing.wait_for(|c| *c) => return,
            }
            let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
            demo.step(elapsed, &hub);
        }
    })
}
