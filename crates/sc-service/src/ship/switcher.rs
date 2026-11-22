//! When to show another dashboard because the ship changed (design notes, sections 2 and 4a).
//!
//! - Only a ship key that has held for [`STABLE_MS`] of fresh reports counts: entering and
//!   leaving seats flickers, and a stale report is no ship at all.
//! - Leaving a ship changes nothing; getting back into the same ship changes nothing. Only the
//!   next different ship recognised switches, and only with "Switch dashboard when I change
//!   ship" on.
//! - A switch never happens while a sequence runs; it waits for the sequence to end.
//! - A dashboard picked by hand stays until the next different ship.

/// A new ship key must hold this long before it counts.
pub const STABLE_MS: i64 = 2_000;

#[derive(Debug, Default)]
pub struct Switcher {
    candidate: Option<(String, i64)>,
    recognised: Option<String>,
    /// The ship recognised while a switch had to wait (a sequence was running).
    pending: Option<String>,
}

/// What one look decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// A different ship was recognised (the hand pick no longer holds), with switching off.
    Recognised(String),
    /// Show the dashboard for this ship key (switching on).
    SwitchFor(String),
}

impl Switcher {
    /// `key`: the ship while the player is in it and the report is fresh; `running`: a sequence
    /// is running; `on`: the setting.
    pub fn step(&mut self, now_ms: i64, key: Option<&str>, running: bool, on: bool) -> Step {
        let mut out = Step::Nothing;
        match key {
            None => self.candidate = None,
            Some(k) => match &self.candidate {
                Some((c, since)) if c == k => {
                    if now_ms - since >= STABLE_MS && self.recognised.as_deref() != Some(k) {
                        self.recognised = Some(k.to_owned());
                        if on {
                            self.pending = Some(k.to_owned());
                        } else {
                            self.pending = None;
                            out = Step::Recognised(k.to_owned());
                        }
                    }
                }
                _ => self.candidate = Some((k.to_owned(), now_ms)),
            },
        }
        if !on {
            self.pending = None;
        }
        if !running && let Some(k) = self.pending.take() {
            out = Step::SwitchFor(k);
        }
        out
    }

    /// A dashboard picked by hand: a waiting switch is dropped.
    pub fn picked(&mut self) {
        self.pending = None;
    }

    pub fn recognised(&self) -> Option<&str> {
        self.recognised.as_deref()
    }

    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }
}
