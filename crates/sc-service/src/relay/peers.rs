//! `relay-peers.json` in the settings folder: the machine this one is paired with. The game PC
//! keeps its one companion; the companion keeps its one game PC. Read leniently: an entry that is
//! not understood is dropped with a warning, as `fleets.json` is.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use fleet_proto::PublicKey;
use sc_core::js;

use crate::json::to_pretty_text;

pub const FILE: &str = "relay-peers.json";
const SCHEMA_VERSION: u64 = 1;
/// The longest peer name kept, in UTF-16 code units.
pub const MAX_NAME: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The machine that plays and runs the sessions.
    Game,
    /// The machine that shows the pages.
    Companion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub name: String,
    pub key: PublicKey,
    /// Where the game PC dials the companion (`host:port`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_address: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub paired_at: i64,
    pub role: Role,
}

/// A peer's self-declared name made safe to keep and show: no control characters, trimmed,
/// capped. The other machine chooses it, so it is never trusted.
pub fn clean_name(raw: &str) -> String {
    let kept: String = raw.chars().filter(|c| !c.is_control()).collect();
    let capped = js::utf16_prefix(kept.trim(), MAX_NAME);
    if capped.is_empty() {
        "unnamed".to_owned()
    } else {
        capped
    }
}

#[derive(Debug, Default)]
pub struct PeerStore {
    file: Option<PathBuf>,
    peers: Mutex<Vec<Peer>>,
}

fn lock(m: &Mutex<Vec<Peer>>) -> MutexGuard<'_, Vec<Peer>> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PeerStore {
    /// The peers in `file`; in memory only when `None`.
    pub fn new(file: Option<PathBuf>) -> Self {
        let peers = file
            .as_ref()
            .and_then(|f| fs::read_to_string(f).ok())
            .map(|text| {
                let (peers, problems) = parse(&text);
                for p in problems {
                    tracing::warn!("[relay] {FILE}: {p}");
                }
                peers
            })
            .unwrap_or_default();
        Self {
            file,
            peers: Mutex::new(peers),
        }
    }

    pub fn get(&self, role: Role) -> Option<Peer> {
        lock(&self.peers).iter().find(|p| p.role == role).cloned()
    }

    /// Keeps `peer`, in place of any peer of its role.
    pub fn set(&self, peer: Peer) -> io::Result<()> {
        self.change(|peers| {
            peers.retain(|p| p.role != peer.role);
            peers.push(peer);
        })
    }

    /// True when there was one.
    pub fn remove(&self, role: Role) -> io::Result<bool> {
        let mut removed = false;
        self.change(|peers| {
            let before = peers.len();
            peers.retain(|p| p.role != role);
            removed = peers.len() != before;
        })?;
        Ok(removed)
    }

    pub fn update(&self, role: Role, edit: impl FnOnce(&mut Peer)) -> io::Result<bool> {
        let mut found = false;
        self.change(|peers| {
            if let Some(p) = peers.iter_mut().find(|p| p.role == role) {
                edit(p);
                found = true;
            }
        })?;
        Ok(found)
    }

    fn change(&self, edit: impl FnOnce(&mut Vec<Peer>)) -> io::Result<()> {
        let mut peers = lock(&self.peers);
        let mut next = peers.clone();
        edit(&mut next);
        if let Some(file) = &self.file {
            if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
                fs::create_dir_all(dir)?;
            }
            let text = to_pretty_text(&serde_json::json!({
                "schemaVersion": SCHEMA_VERSION,
                "peers": next,
            }));
            let tmp = file.with_extension("json.tmp");
            fs::write(&tmp, text)?;
            fs::rename(&tmp, file)?;
        }
        *peers = next;
        Ok(())
    }
}

/// The peers in a file's text, at most one per role, and what was dropped.
pub fn parse(text: &str) -> (Vec<Peer>, Vec<String>) {
    let mut problems = vec![];
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
        return (
            vec![],
            vec!["not a JSON object; no peer is kept".to_owned()],
        );
    };
    if obj.get("schemaVersion").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return (
            vec![],
            vec!["schemaVersion must be 1; no peer is kept".to_owned()],
        );
    }
    let mut peers: Vec<Peer> = vec![];
    for (i, raw) in obj
        .get("peers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        match serde_json::from_value::<Peer>(raw.clone()) {
            Ok(mut peer) if !peers.iter().any(|p| p.role == peer.role) => {
                peer.name = clean_name(&peer.name);
                peers.push(peer);
            }
            Ok(_) => problems.push(format!("peers[{i}]: a second peer of one role is dropped")),
            Err(e) => problems.push(format!("peers[{i}] is dropped: {e}")),
        }
    }
    (peers, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_proto::Identity;

    fn peer(role: Role, seed: u8) -> Peer {
        Peer {
            name: "laptop".into(),
            key: Identity::from_seed([seed; 32]).public(),
            last_address: Some("198.51.100.20:47521".into()),
            paired_at: 1,
            role,
        }
    }

    #[test]
    fn one_peer_per_role_is_kept_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        let store = PeerStore::new(Some(file.clone()));
        store.set(peer(Role::Companion, 1)).unwrap();
        store.set(peer(Role::Companion, 2)).unwrap();
        store.set(peer(Role::Game, 3)).unwrap();
        let again = PeerStore::new(Some(file));
        assert_eq!(again.get(Role::Companion), Some(peer(Role::Companion, 2)));
        assert_eq!(again.get(Role::Game), Some(peer(Role::Game, 3)));
        assert!(again.remove(Role::Companion).unwrap());
        assert!(!again.remove(Role::Companion).unwrap());
        assert!(
            again
                .update(Role::Game, |p| p.name = "gamepc".into())
                .unwrap()
        );
        assert_eq!(again.get(Role::Game).unwrap().name, "gamepc");
    }

    #[test]
    fn a_bad_file_or_entry_is_dropped_with_a_reason() {
        assert_eq!(parse("[]").0, vec![]);
        assert_eq!(parse(r#"{ "schemaVersion": 2, "peers": [] }"#).1.len(), 1);
        let good = serde_json::to_value(peer(Role::Game, 1)).unwrap();
        let text = serde_json::json!({
            "schemaVersion": 1,
            "peers": [good, good, { "name": "x", "key": "nope", "pairedAt": 1, "role": "game" }],
        })
        .to_string();
        let (peers, problems) = parse(&text);
        assert_eq!(peers, vec![peer(Role::Game, 1)]);
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn a_name_is_cleaned_and_capped() {
        assert_eq!(clean_name("  gamepc\u{7}\n "), "gamepc");
        assert_eq!(clean_name("\u{1b}"), "unnamed");
        assert_eq!(js::utf16_len(&clean_name(&"x".repeat(500))), MAX_NAME);
    }
}
