//! The keys a peer-to-peer host has kicked (`fleet-bans.json` in the settings folder): a ban
//! binds to the fleet key, never the handle, and outlives the session and a restart until the
//! host lifts it. The file is the user's only, checked on reading, and capped at
//! [`MAX_BANS`]; the handle is only a label for the list, escaped by the page that draws it.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use fleet_proto::PublicKey;
use fleet_proto::card::is_handle;

use crate::json::to_pretty_text;

pub const BANS_FILE: &str = "fleet-bans.json";
pub const BANS_SCHEMA: u32 = 1;
pub const MAX_BANS: usize = 500;
/// A larger file is not read.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The label for a ban whose stored handle does not check out.
const UNKNOWN_HANDLE: &str = "(unknown)";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ban {
    pub key: PublicKey,
    pub handle: String,
    /// Milliseconds since the Unix epoch.
    pub at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnDisk<'a> {
    schema_version: u32,
    bans: &'a [Ban],
}

/// Reads a fleet-bans.json: keeps what checks out, at most [`MAX_BANS`], and lists what it left
/// out.
pub fn read_bans(raw: &Value) -> (Vec<Ban>, Vec<String>) {
    let mut out: Vec<Ban> = Vec::new();
    let mut skipped = vec![];
    if raw.get("schemaVersion").and_then(Value::as_u64) != Some(u64::from(BANS_SCHEMA)) {
        skipped.push("the file is not schema version 1".to_owned());
        return (out, skipped);
    }
    for item in raw
        .get("bans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(key) = item
            .get("key")
            .and_then(Value::as_str)
            .and_then(|k| k.parse::<PublicKey>().ok())
        else {
            skipped.push("a ban with no valid key".to_owned());
            continue;
        };
        if out.iter().any(|b| b.key == key) {
            skipped.push("a repeated ban".to_owned());
            continue;
        }
        if out.len() >= MAX_BANS {
            skipped.push(format!("a ban over {MAX_BANS}"));
            continue;
        }
        let handle = item
            .get("handle")
            .and_then(Value::as_str)
            .filter(|h| is_handle(h))
            .unwrap_or(UNKNOWN_HANDLE)
            .to_owned();
        let at = item.get("at").and_then(Value::as_i64).unwrap_or(0);
        out.push(Ban { key, handle, at });
    }
    (out, skipped)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(unix)]
fn user_only(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn user_only(_: &mut OpenOptions) {}

/// The bans, kept in memory and written through; in memory only with no file.
pub struct BanStore {
    file: Option<PathBuf>,
    bans: Mutex<Vec<Ban>>,
}

impl BanStore {
    pub fn new(file: Option<PathBuf>) -> Self {
        let bans = file.as_deref().map(Self::load).unwrap_or_default();
        Self {
            file,
            bans: Mutex::new(bans),
        }
    }

    fn load(file: &std::path::Path) -> Vec<Ban> {
        let Ok(meta) = fs::metadata(file) else {
            return vec![];
        };
        if meta.len() > MAX_FILE_BYTES {
            tracing::warn!(
                "[host] {} is over {} KiB; not read",
                file.display(),
                MAX_FILE_BYTES / 1024
            );
            return vec![];
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                tracing::warn!(
                    "[host] {} was readable by others; made it user-only",
                    file.display()
                );
                let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o600));
            }
        }
        let raw = fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let Some(raw) = raw else {
            tracing::warn!("[host] {} is not JSON; not read", file.display());
            return vec![];
        };
        let (bans, skipped) = read_bans(&raw);
        if !skipped.is_empty() {
            tracing::warn!(
                "[host] {}: {} entries left out ({})",
                file.display(),
                skipped.len(),
                skipped.join("; ")
            );
        }
        bans
    }

    pub fn get(&self) -> Vec<Ban> {
        lock(&self.bans).clone()
    }

    pub fn is_banned(&self, key: &PublicKey) -> bool {
        lock(&self.bans).iter().any(|b| b.key == *key)
    }

    /// Bans `key`; already banned is fine. Refused when the list is full.
    pub fn add(&self, key: PublicKey, handle: &str, at: i64) -> Result<(), String> {
        let mut bans = lock(&self.bans);
        if bans.iter().any(|b| b.key == key) {
            return Ok(());
        }
        if bans.len() >= MAX_BANS {
            return Err(format!(
                "The ban list is full ({MAX_BANS}): lift a ban first"
            ));
        }
        let handle = if is_handle(handle) {
            handle
        } else {
            UNKNOWN_HANDLE
        };
        bans.push(Ban {
            key,
            handle: handle.to_owned(),
            at,
        });
        self.save(&bans)
            .map_err(|e| format!("The ban list cannot be saved: {e}"))
    }

    /// Lifts the ban on `key`; false when there was none.
    pub fn lift(&self, key: &PublicKey) -> Result<bool, String> {
        let mut bans = lock(&self.bans);
        let before = bans.len();
        bans.retain(|b| b.key != *key);
        if bans.len() == before {
            return Ok(false);
        }
        self.save(&bans)
            .map(|()| true)
            .map_err(|e| format!("The ban list cannot be saved: {e}"))
    }

    fn save(&self, bans: &[Ban]) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let text = to_pretty_text(&serde_json::to_value(OnDisk {
            schema_version: BANS_SCHEMA,
            bans,
        })?);
        let tmp = file.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        user_only(&mut options);
        let mut out = options.open(&tmp)?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        drop(out);
        fs::rename(&tmp, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_proto::Identity;
    use serde_json::json;

    fn key(n: u8) -> PublicKey {
        Identity::from_seed([n; 32]).public()
    }

    #[test]
    fn bans_persist_are_user_only_and_lift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(BANS_FILE);
        let store = BanStore::new(Some(file.clone()));
        store.add(key(1), "Vega", 5).unwrap();
        store.add(key(1), "Vega", 6).unwrap();
        store.add(key(2), "<b>x</b>", 7).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let again = BanStore::new(Some(file.clone()));
        assert!(again.is_banned(&key(1)) && again.is_banned(&key(2)));
        assert_eq!(again.get().len(), 2);
        assert_eq!(again.get()[1].handle, "(unknown)");
        assert!(again.lift(&key(1)).unwrap());
        assert!(!again.lift(&key(1)).unwrap());
        assert!(!BanStore::new(Some(file)).is_banned(&key(1)));
    }

    #[test]
    fn the_list_is_capped_and_checked_on_reading() {
        let store = BanStore::new(None);
        for n in 0..MAX_BANS {
            let k = Identity::from_seed({
                let mut s = [0u8; 32];
                s[..8].copy_from_slice(&(n as u64).to_le_bytes());
                s
            })
            .public();
            store.add(k, "M", 0).unwrap();
        }
        assert!(store.add(key(200), "M", 0).unwrap_err().contains("full"));

        let raw = json!({ "schemaVersion": 1, "bans": [
            { "key": key(1).to_string(), "handle": "Vega", "at": 1 },
            { "key": "nope", "handle": "x" },
            { "key": key(1).to_string(), "handle": "Again" },
            { "key": key(3).to_string(), "handle": "a\u{7}b" },
        ] });
        let (bans, skipped) = read_bans(&raw);
        assert_eq!(bans.len(), 2);
        assert_eq!(bans[1].handle, "(unknown)");
        assert_eq!(skipped.len(), 2);
        assert!(read_bans(&json!({ "schemaVersion": 2 })).0.is_empty());
    }
}
