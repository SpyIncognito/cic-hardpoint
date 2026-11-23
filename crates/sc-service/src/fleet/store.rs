//! The fleets this player has joined (`fleets.json`) and what they agree to send
//! (`fleet-consent.json`), both in the settings folder.
//!
//! A joined fleet keeps its address, the server's identity key (the pin) and the handle; a hosted
//! one (a CIC relay) also the event chosen, for reconnects, and the portal's address when its
//! connection string named one. The join code and the connection token
//! are never written: each is spent on first use, and the server knows the member by key after
//! that. Both files are checked on reading; what is wrong is left out and logged.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use fleet_proto::card::is_handle;
use fleet_proto::invite::is_host;
use fleet_proto::{PublicKey, SendConsent};

use crate::json::to_pretty_text;

pub const FLEETS_FILE: &str = "fleets.json";
pub const CONSENT_FILE: &str = "fleet-consent.json";
pub const STORE_SCHEMA: u32 = 1;
pub const MAX_FLEETS: usize = 16;
/// A larger file is not read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinedFleet {
    pub host: String,
    pub port: u16,
    /// The server's identity key, pinned on every connection.
    pub key: PublicKey,
    pub handle: String,
    /// The event's name when last joined, for the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// A CIC relay, joined with a connection token: reconnects send `connect` with no token.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hosted: bool,
    /// Hosted: the event id last joined, sent again on reconnect.
    #[serde(default, rename = "eventId", skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Hosted: the CIC portal's address, for links to the event's page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal: Option<String>,
}

impl JoinedFleet {
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn same_server(&self, other: &Self) -> bool {
        self.key == other.key
    }

    fn problem(&self) -> Option<String> {
        if !is_host(&self.host) {
            return Some(format!("bad host {:?}", self.host));
        }
        if self.port == 0 {
            return Some("port 0".to_owned());
        }
        if !is_handle(&self.handle) {
            return Some(format!("{}: bad handle", self.address()));
        }
        if self
            .event
            .as_deref()
            .is_some_and(|e| !fleet_proto::structure::is_name(e))
        {
            return Some(format!("{}: bad event name", self.address()));
        }
        if self
            .event_id
            .as_deref()
            .is_some_and(|e| !fleet_proto::structure::is_id(e))
        {
            return Some(format!("{}: bad event id", self.address()));
        }
        if self
            .portal
            .as_deref()
            .is_some_and(|p| super::cic::portal_url(p).as_deref() != Some(p))
        {
            return Some(format!("{}: bad portal address", self.address()));
        }
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fleets {
    pub fleets: Vec<JoinedFleet>,
    /// The server key of the fleet to connect to at start; none after leaving.
    pub active: Option<PublicKey>,
}

impl Fleets {
    pub fn active_fleet(&self) -> Option<&JoinedFleet> {
        let key = self.active?;
        self.fleets.iter().find(|f| f.key == key)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FleetsOnDisk<'a> {
    schema_version: u32,
    fleets: &'a [JoinedFleet],
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<PublicKey>,
}

/// Reads a fleets.json: keeps what checks out and lists what it left out.
pub fn read_fleets(raw: &Value) -> (Fleets, Vec<String>) {
    let mut out = Fleets::default();
    let mut skipped = vec![];
    if raw.get("schemaVersion").and_then(Value::as_u64) != Some(u64::from(STORE_SCHEMA)) {
        skipped.push("the file is not schema version 1".to_owned());
        return (out, skipped);
    }
    for item in raw
        .get("fleets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match serde_json::from_value::<JoinedFleet>(item.clone()) {
            Err(e) => skipped.push(format!("a fleet that is not readable: {e}")),
            Ok(f) => match f.problem() {
                Some(p) => skipped.push(p),
                None if out.fleets.iter().any(|o| o.same_server(&f)) => {
                    skipped.push(format!("{}: repeated", f.address()));
                }
                None if out.fleets.len() >= MAX_FLEETS => {
                    skipped.push(format!("{}: over {MAX_FLEETS} fleets", f.address()));
                }
                None => out.fleets.push(f),
            },
        }
    }
    out.active = raw
        .get("active")
        .and_then(Value::as_str)
        .and_then(|k| k.parse().ok())
        .filter(|k| out.fleets.iter().any(|f| f.key == *k));
    (out, skipped)
}

fn read_capped(file: &Path) -> Option<Value> {
    let size = fs::metadata(file).ok()?.len();
    if size > MAX_FILE_BYTES {
        tracing::warn!(
            "[fleet] {} is over {} KiB; not read",
            file.display(),
            MAX_FILE_BYTES / 1024
        );
        return None;
    }
    let text = fs::read_to_string(file).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("[fleet] {} is not JSON ({e}); not read", file.display());
            None
        }
    }
}

fn write_atomic(file: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(".tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, file)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The joined fleets, kept in memory and written through.
pub struct FleetsStore {
    file: Option<PathBuf>,
    fleets: Mutex<Fleets>,
}

impl FleetsStore {
    pub fn new(file: Option<PathBuf>) -> Self {
        let fleets = file
            .as_deref()
            .and_then(read_capped)
            .map(|raw| {
                let (fleets, skipped) = read_fleets(&raw);
                if !skipped.is_empty() {
                    tracing::warn!("[fleet] fleets.json: left out {}", skipped.join("; "));
                }
                fleets
            })
            .unwrap_or_default();
        Self {
            file,
            fleets: Mutex::new(fleets),
        }
    }

    pub fn get(&self) -> Fleets {
        lock(&self.fleets).clone()
    }

    fn change(&self, f: impl FnOnce(&mut Fleets)) -> io::Result<()> {
        let mut fleets = lock(&self.fleets);
        f(&mut fleets);
        if let Some(file) = &self.file {
            let disk = FleetsOnDisk {
                schema_version: STORE_SCHEMA,
                fleets: &fleets.fleets,
                active: fleets.active,
            };
            write_atomic(file, &to_pretty_text(&disk))?;
        }
        Ok(())
    }

    /// Adds or updates a fleet (by server key), first in the list, and makes it the active one.
    pub fn joined(&self, fleet: JoinedFleet) -> io::Result<()> {
        self.change(|fs| {
            fs.fleets.retain(|f| !f.same_server(&fleet));
            fs.active = Some(fleet.key);
            fs.fleets.insert(0, fleet);
            fs.fleets.truncate(MAX_FLEETS);
        })
    }

    pub fn set_active(&self, key: Option<PublicKey>) -> io::Result<()> {
        self.change(|fs| {
            fs.active = key.filter(|k| fs.fleets.iter().any(|f| f.key == *k));
        })
    }

    pub fn remove(&self, key: &PublicKey) -> io::Result<()> {
        self.change(|fs| {
            fs.fleets.retain(|f| f.key != *key);
            if fs.active.as_ref() == Some(key) {
                fs.active = None;
            }
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsentOnDisk<'a> {
    schema_version: u32,
    #[serde(flatten)]
    consent: &'a SendConsent,
}

/// What this player agrees to send, field by field. Nothing until they turn fields on.
pub struct ConsentStore {
    file: Option<PathBuf>,
    consent: Mutex<SendConsent>,
}

impl ConsentStore {
    pub fn new(file: Option<PathBuf>) -> Self {
        let consent = file
            .as_deref()
            .and_then(read_capped)
            .filter(|raw| {
                raw.get("schemaVersion").and_then(Value::as_u64) == Some(u64::from(STORE_SCHEMA))
            })
            .and_then(|raw| serde_json::from_value::<SendConsent>(raw).ok())
            .unwrap_or_default();
        Self {
            file,
            consent: Mutex::new(consent),
        }
    }

    pub fn get(&self) -> SendConsent {
        lock(&self.consent).clone()
    }

    pub fn set(&self, consent: SendConsent) -> io::Result<()> {
        let mut held = lock(&self.consent);
        if let Some(file) = &self.file {
            let disk = ConsentOnDisk {
                schema_version: STORE_SCHEMA,
                consent: &consent,
            };
            write_atomic(file, &to_pretty_text(&disk))?;
        }
        *held = consent;
        Ok(())
    }
}
