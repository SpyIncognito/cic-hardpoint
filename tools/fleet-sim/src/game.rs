//! A member's game: a shared recording played from the member's own offset, in its seat, with
//! scripted downed, dead and respawn moments put into the frames as the game's own events, so
//! the card state tracker sees them exactly as it would from the game.

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Instant;

use sc_game::{BoxFuture, Frame, FrameStream, GameAction, GameApi, TriggerResult, TriggerStatus};
use sc_service::fleet::card::{EVENT_DEAD, EVENT_DOWNED, EVENT_RESPAWN};

use crate::recording::Recording;
use crate::scenario::Seating;

/// Where a scripted member is in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Up,
    Downed,
    /// Back up after being downed: the game has no revive event, so only the health returns.
    Revived,
    Dead,
    /// Up again after a respawn.
    Respawned,
}

/// One cycle: up for a third, downed for a ninth, revived until two thirds, dead for a sixth,
/// then respawned. At the default three minutes: down at 1:00, revived at 1:20, dead at 2:00,
/// respawned at 2:30.
#[derive(Debug, Clone, Copy)]
pub struct Script {
    pub cycle_ms: f64,
    /// Where in the cycle this member starts, so members do not go down together.
    pub offset_ms: f64,
}

impl Script {
    pub fn phase(&self, elapsed_ms: f64) -> Phase {
        let at = (elapsed_ms + self.offset_ms).rem_euclid(self.cycle_ms) / self.cycle_ms;
        match at {
            a if a < 1.0 / 3.0 => Phase::Up,
            a if a < 4.0 / 9.0 => Phase::Downed,
            a if a < 2.0 / 3.0 => Phase::Revived,
            a if a < 5.0 / 6.0 => Phase::Dead,
            _ => Phase::Respawned,
        }
    }
}

/// The game event a member's frame carries on entering `phase`.
fn event_on_entering(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Downed => Some(EVENT_DOWNED),
        Phase::Dead => Some(EVENT_DEAD),
        Phase::Respawned => Some(EVENT_RESPAWN),
        Phase::Up | Phase::Revived => None,
    }
}

/// Puts the scripted state into a recorded frame: health at zero while downed or dead, and the
/// event on a change of phase (or on the first frame, for a member that starts down).
pub fn apply(frame: &mut Frame, phase: Phase, previous: Option<Phase>) {
    if matches!(phase, Phase::Downed | Phase::Dead)
        && let Some(health) = frame.pointer_mut("/actor/vitals/health/current")
    {
        *health = json!(0);
    }
    let entered = match previous {
        Some(p) => p != phase,
        None => matches!(phase, Phase::Downed | Phase::Dead),
    };
    let Some(event) = event_on_entering(phase).filter(|_| entered) else {
        return;
    };
    let Some(object) = frame.as_object_mut() else {
        return;
    };
    let events = object
        .entry("events")
        .or_insert_with(|| json!({ "events": [] }));
    if !events.get("events").is_some_and(Value::is_array) {
        *events = json!({ "events": [] });
    }
    if let Some(list) = events.get_mut("events").and_then(Value::as_array_mut) {
        list.push(json!({ "type": event }));
    }
}

struct Player {
    recording: Arc<Recording>,
    offset_ms: f64,
    frame_every: Duration,
    script: Option<Script>,
    seating: Option<Seating>,
    started: Instant,
}

impl Player {
    /// The member's frame now, and the phase it is in.
    fn frame(&self, previous: Option<Phase>) -> (Frame, Option<Phase>) {
        let elapsed = self.started.elapsed().as_secs_f64() * 1000.0;
        let mut frame = self.recording.frame_at(elapsed + self.offset_ms);
        if let Some(seating) = &self.seating {
            seating.apply(&mut frame);
        }
        let phase = self.script.map(|s| s.phase(elapsed));
        if let Some(phase) = phase {
            apply(&mut frame, phase, previous);
        }
        (frame, phase)
    }
}

/// Plays on the wall clock from when it was made, so a reopened stream (after a reconnect)
/// carries on where the member is rather than starting over.
pub struct SimGame {
    player: Arc<Player>,
    closed: watch::Sender<()>,
}

impl SimGame {
    pub fn new(
        recording: Arc<Recording>,
        offset_ms: f64,
        frame_every: Duration,
        script: Option<Script>,
        seating: Option<Seating>,
    ) -> Self {
        Self {
            player: Arc::new(Player {
                recording,
                offset_ms,
                frame_every,
                script,
                seating,
                started: Instant::now(),
            }),
            closed: watch::Sender::new(()),
        }
    }
}

impl GameApi for SimGame {
    fn open_stream(&self) -> FrameStream {
        let start = (
            Arc::clone(&self.player),
            None,
            true,
            self.closed.subscribe(),
        );
        Box::pin(stream::unfold(
            start,
            |(player, previous, first, mut closed)| async move {
                if !first {
                    tokio::select! {
                        () = tokio::time::sleep(player.frame_every) => {}
                        Ok(()) = closed.changed() => return None,
                    }
                }
                let (frame, phase) = player.frame(previous);
                Some((Ok(frame), (player, phase, false, closed)))
            },
        ))
    }

    fn list_actions(&self) -> BoxFuture<'_, Vec<GameAction>> {
        Box::pin(async { Vec::new() })
    }

    fn trigger_action(&self, _name: &str, _hold_ms: Option<u32>) -> BoxFuture<'_, TriggerResult> {
        Box::pin(async { TriggerResult::new(TriggerStatus::Ok, "") })
    }

    fn close(&self) {
        self.closed.send_replace(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_walks_through_each_phase() {
        let s = Script {
            cycle_ms: 9000.0,
            offset_ms: 0.0,
        };
        let phases: Vec<Phase> = [0.0, 3500.0, 5000.0, 6500.0, 8000.0, 9100.0]
            .iter()
            .map(|t| s.phase(*t))
            .collect();
        assert_eq!(
            phases,
            [
                Phase::Up,
                Phase::Downed,
                Phase::Revived,
                Phase::Dead,
                Phase::Respawned,
                Phase::Up
            ]
        );
    }

    fn frame() -> Frame {
        json!({ "actor": { "vitals": { "health": { "current": 80, "max": 100 } } } })
    }

    #[test]
    fn events_go_in_on_entering_a_phase_and_health_drops_while_down() {
        let mut f = frame();
        apply(&mut f, Phase::Downed, Some(Phase::Up));
        assert_eq!(f["actor"]["vitals"]["health"]["current"], 0);
        assert_eq!(f["events"]["events"][0]["type"], EVENT_DOWNED);

        let mut f = frame();
        apply(&mut f, Phase::Downed, Some(Phase::Downed));
        assert!(f.get("events").is_none());

        let mut f = frame();
        apply(&mut f, Phase::Respawned, Some(Phase::Dead));
        assert_eq!(f["actor"]["vitals"]["health"]["current"], 80);
        assert_eq!(f["events"]["events"][0]["type"], EVENT_RESPAWN);

        let mut f = frame();
        apply(&mut f, Phase::Dead, None);
        assert_eq!(f["events"]["events"][0]["type"], EVENT_DEAD);
        let mut f = frame();
        apply(&mut f, Phase::Respawned, None);
        assert!(
            f.get("events").is_none(),
            "no respawn without a death first"
        );
    }
}
