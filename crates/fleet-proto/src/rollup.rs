//! A member's status as the viewer sees it (the card's state, or OFFLINE once the card is stale),
//! the order members are listed in, and a unit's roll-up such as "4/6 up, 1 downed, 1 dead".

use serde::{Deserialize, Serialize};

use crate::card::Life;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Up,
    Downed,
    Dead,
    Respawned,
    /// The card is stale: nothing received within the stale window.
    Offline,
    /// Online, but the member does not share character status.
    Unknown,
}

impl Status {
    pub fn of(life: Option<Life>, stale: bool) -> Self {
        if stale {
            return Self::Offline;
        }
        match life {
            Some(Life::Up) => Self::Up,
            Some(Life::Downed) => Self::Downed,
            Some(Life::Dead) => Self::Dead,
            Some(Life::Respawned) => Self::Respawned,
            None => Self::Unknown,
        }
    }

    /// Who needs attention first: downed (evac), then dead (reinforce), then the rest.
    pub fn rank(self) -> u8 {
        match self {
            Self::Downed => 0,
            Self::Dead => 1,
            Self::Respawned => 2,
            Self::Up => 3,
            Self::Unknown => 4,
            Self::Offline => 5,
        }
    }
}

/// Sort by status rank, keeping the given order within a rank.
pub fn sort_by_status<T>(items: &mut [T], status: impl Fn(&T) -> Status) {
    items.sort_by_key(|item| status(item).rank());
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rollup {
    pub total: usize,
    /// UP and RESPAWNED.
    pub up: usize,
    pub downed: usize,
    pub dead: usize,
    pub offline: usize,
    pub unknown: usize,
    pub text: String,
}

pub fn rollup(statuses: impl IntoIterator<Item = Status>) -> Rollup {
    rollup_as("", statuses)
}

/// A roll-up whose text names what it counts, such as "3/4 Pilots up, 1 downed" with the
/// unit kind's [`crate::structure::UnitKind::rollup_label`].
pub fn rollup_as(label: &str, statuses: impl IntoIterator<Item = Status>) -> Rollup {
    let mut r = Rollup::default();
    for s in statuses {
        r.total += 1;
        match s {
            Status::Up | Status::Respawned => r.up += 1,
            Status::Downed => r.downed += 1,
            Status::Dead => r.dead += 1,
            Status::Offline => r.offline += 1,
            Status::Unknown => r.unknown += 1,
        }
    }
    r.text = if r.total == 0 {
        "empty".to_owned()
    } else {
        let counted = if label.is_empty() {
            String::new()
        } else {
            format!(" {label}")
        };
        let mut parts = vec![format!("{}/{}{counted} up", r.up, r.total)];
        for (n, what) in [
            (r.downed, "downed"),
            (r.dead, "dead"),
            (r.offline, "offline"),
            (r.unknown, "no status"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        parts.join(", ")
    };
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_up_and_sorts_by_status() {
        use Status::*;
        let r = rollup([Up, Up, Respawned, Up, Downed, Dead]);
        assert_eq!(r.text, "4/6 up, 1 downed, 1 dead");
        assert_eq!((r.total, r.up, r.downed, r.dead), (6, 4, 1, 1));
        assert_eq!(
            rollup([Up, Offline, Unknown]).text,
            "1/3 up, 1 offline, 1 no status"
        );
        assert_eq!(rollup([]).text, "empty");
        assert_eq!(
            rollup_as("Pilots", [Up, Up, Up, Downed]).text,
            "3/4 Pilots up, 1 downed"
        );
        assert_eq!(rollup_as("Crew", [Up; 6]).text, "6/6 Crew up");

        assert_eq!(Status::of(Some(Life::Downed), true), Offline);
        assert_eq!(Status::of(None, false), Unknown);
        assert_eq!(Status::of(Some(Life::Respawned), false), Respawned);

        let mut items = [
            ("a", Up),
            ("b", Offline),
            ("c", Dead),
            ("d", Downed),
            ("e", Up),
        ];
        sort_by_status(&mut items, |i| i.1);
        let order: Vec<&str> = items.iter().map(|i| i.0).collect();
        assert_eq!(order, ["d", "c", "a", "e", "b"]);
    }
}
