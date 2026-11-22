//! The bus file: how clients find the running service (`{ port, token, pid, version }`), in the
//! same place and JSON as the TypeScript service, so the Stream Deck plugin connects unchanged.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusFile {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: u32,
}

/// Write the bus file so a reader sees either the old file or the whole new one, never a part.
pub fn write(file: &Path, bus: &BusFile) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let mut suffix = [0u8; 4];
    getrandom::fill(&mut suffix).map_err(io::Error::other)?;
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(format!(".{}.{}.tmp", std::process::id(), hex(&suffix)));
    let tmp = PathBuf::from(tmp);
    let text = serde_json::to_string(bus).expect("a bus file serializes");
    let result = write_private(&tmp, text.as_bytes()).and_then(|()| fs::rename(&tmp, file));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The bus file, `Ok(None)` when there is none, an error when it is there but not a bus file.
pub fn read(file: &Path) -> io::Result<Option<BusFile>> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let bus: BusFile = serde_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bus file {} is not valid ({e})", file.display()),
        )
    })?;
    let valid = bus.port > 0
        && bus.token.len() == 64
        && bus
            .token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bus file {} is not a valid bus file", file.display()),
        ));
    }
    Ok(Some(bus))
}

/// Remove the bus file only if it still holds this token, so a newer server's file survives.
pub fn remove(file: &Path, token: &str) -> bool {
    match read(file) {
        Ok(Some(bus)) if bus.token == token => fs::remove_file(file).is_ok(),
        _ => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// The token is the only thing keeping other local users off the bus; Windows has no modes.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(token: &str) -> BusFile {
        BusFile {
            port: 4000,
            token: token.to_owned(),
            pid: 7,
            version: 1,
        }
    }

    #[test]
    fn writes_reads_and_removes_only_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("bus.json");
        let mine = "a".repeat(64);
        write(&file, &bus(&mine)).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            format!(r#"{{"port":4000,"token":"{mine}","pid":7,"version":1}}"#)
        );
        assert_eq!(read(&file).unwrap(), Some(bus(&mine)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(!remove(&file, &"b".repeat(64)));
        assert!(file.exists());
        assert!(remove(&file, &mine));
        assert_eq!(read(&file).unwrap(), None);
        assert_eq!(
            fs::read_dir(file.parent().unwrap()).unwrap().count(),
            0,
            "no temp file left"
        );
    }

    #[test]
    fn refuses_a_file_that_is_not_a_bus_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bus.json");
        fs::write(&file, r#"{"port":1,"token":"x","pid":1,"version":1}"#).unwrap();
        assert!(read(&file).is_err());
        fs::write(&file, "not json").unwrap();
        assert!(read(&file).is_err());
    }
}
