//! Who a page session acts for, and what that lets it do. A page this machine served may do
//! everything; a companion's page, run here over the relay link, may not set this machine's
//! displays, controllers or relay pairing, and its actions are also capped across the whole link
//! and dropped when they arrive late.
//!
//! The companion's `sentAt` is the companion's own clock and only guards against lag: the hold
//! timer and the caps here are what protect the game.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::Instant;

use sc_core::engine::{ActionResult, FailStatus};

use super::session::MAX_ACTIONS_PER_S;
use crate::json::to_js_value;

/// An action older than this when it reaches the game PC is not run.
pub const STALE_AFTER_MS: i64 = 1000;
/// The messages a companion's page stamps with `sentAt` and the game PC checks for lag.
pub const TIMED: &[&str] = &["press", "hold.start", "set", "mode", "run"];

/// The relay link's share of the rules: one action cap for all its pages, and the companion's
/// clock as measured by ping and pong.
#[derive(Debug, Default)]
pub struct LinkGuard {
    recent: Mutex<VecDeque<Instant>>,
    /// The companion's clock minus this one's, in ms, once a pong measured it.
    offset_ms: AtomicI64,
    measured: AtomicBool,
}

impl LinkGuard {
    /// Counts an action against the link's per-second cap; false when over it.
    pub fn admit(&self) -> bool {
        let mut recent = self.recent.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        while recent
            .front()
            .is_some_and(|&t| now - t > Duration::from_secs(1))
        {
            recent.pop_front();
        }
        if recent.len() >= MAX_ACTIONS_PER_S {
            return false;
        }
        recent.push_back(now);
        true
    }

    /// A ping sent at `sent` (this clock) came back at `back`, stamped `at` by the companion.
    pub fn measured(&self, sent: i64, at: i64, back: i64) {
        let midway = sent + (back - sent).max(0) / 2;
        self.offset_ms.store(at - midway, Ordering::SeqCst);
        self.measured.store(true, Ordering::SeqCst);
    }

    /// How old a message stamped `sent_at` (the companion's clock) is at `now` (this clock).
    /// Before the first measure the clocks are taken as equal.
    pub fn age_ms(&self, sent_at: i64, now: i64) -> i64 {
        let offset = if self.measured.load(Ordering::SeqCst) {
            self.offset_ms.load(Ordering::SeqCst)
        } else {
            0
        };
        now - (sent_at - offset)
    }
}

#[derive(Debug, Clone)]
pub enum Policy {
    /// A page this machine served.
    Local,
    /// A companion's page over the relay link.
    Companion(Arc<LinkGuard>),
}

impl Policy {
    pub fn is_companion(&self) -> bool {
        matches!(self, Self::Companion(_))
    }

    /// The answer for a message this policy does not allow, or `None` to run it.
    pub fn refusal(&self, kind: &str, msg: &Value, now_ms: i64) -> Option<Value> {
        let Self::Companion(link) = self else {
            return None;
        };
        if matches!(kind, "dash.setPlacement" | "dash.setBezel") {
            return Some(json!({ "problems": [
                "Displays and bezel buttons belong to the game PC; set them there"
            ] }));
        }
        if kind.starts_with("fleet.host.") {
            return Some(json!({ "problems": [
                "Hosting is set on the game PC itself"
            ] }));
        }
        if kind.starts_with("relay.") {
            return Some(json!({ "problems": [
                "Relay pairing is set on each machine itself"
            ] }));
        }
        if TIMED.contains(&kind) {
            let age = msg
                .get("sentAt")
                .and_then(Value::as_i64)
                .map(|sent| link.age_ms(sent, now_ms));
            if age.is_none_or(|age| age > STALE_AFTER_MS) {
                return Some(to_js_value(&ActionResult::fail(
                    FailStatus::Stale,
                    "It reached the game PC too late over the link, so it was not sent",
                )));
            }
        }
        None
    }

    /// Counts an action against the link's cap; always true for a local page.
    pub fn admit(&self) -> bool {
        match self {
            Self::Local => true,
            Self::Companion(link) => link.admit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn companion() -> (Policy, Arc<LinkGuard>) {
        let link = Arc::new(LinkGuard::default());
        (Policy::Companion(Arc::clone(&link)), link)
    }

    #[test]
    fn a_local_page_is_refused_nothing() {
        let msg = json!({ "type": "dash.setBezel" });
        assert_eq!(Policy::Local.refusal("dash.setBezel", &msg, 0), None);
        assert_eq!(Policy::Local.refusal("press", &json!({}), 0), None);
    }

    #[test]
    fn a_companion_may_not_set_displays_bezels_or_relay_pairing() {
        let (policy, _) = companion();
        for kind in [
            "dash.setPlacement",
            "dash.setBezel",
            "relay.pair",
            "relay.unpair",
            "fleet.host.start",
            "fleet.host.kick",
        ] {
            let refused = policy.refusal(kind, &json!({}), 0).unwrap();
            assert_eq!(refused["problems"].as_array().unwrap().len(), 1, "{kind}");
        }
        for kind in [
            "dash.saveTab",
            "dash.import",
            "fleet.connect",
            "fleet.consent",
            "fleet.layout.get",
            "fleet.layout.save",
        ] {
            assert_eq!(policy.refusal(kind, &json!({}), 0), None, "{kind}");
        }
    }

    #[test]
    fn a_late_or_unstamped_action_is_stale() {
        let (policy, link) = companion();
        let stale = |msg: Value, now| policy.refusal("press", &msg, now);
        assert_eq!(stale(json!({ "sentAt": 10_000 }), 10_900), None);
        assert_eq!(
            stale(json!({ "sentAt": 10_000 }), 11_001).unwrap()["status"],
            "stale"
        );
        assert_eq!(stale(json!({}), 0).unwrap()["status"], "stale");
        // The companion's clock runs 5 s ahead: ping at 1000, its stamp 6050, back at 1100.
        link.measured(1000, 6050, 1100);
        assert_eq!(stale(json!({ "sentAt": 15_000 }), 10_900), None);
        assert_eq!(
            stale(json!({ "sentAt": 15_000 }), 11_100).unwrap()["status"],
            "stale"
        );
        // hold.end only ever ends a hold, so it is never stale.
        assert_eq!(policy.refusal("hold.end", &json!({}), 0), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_link_caps_actions_across_its_pages() {
        let link = LinkGuard::default();
        assert!((0..MAX_ACTIONS_PER_S).all(|_| link.admit()));
        assert!(!link.admit());
        tokio::time::advance(Duration::from_millis(1001)).await;
        assert!(link.admit());
    }
}
