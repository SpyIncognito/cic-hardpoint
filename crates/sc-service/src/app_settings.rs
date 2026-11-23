//! `app-settings.json` in the settings folder: the desktop app's own file, which the service also
//! keeps some dashboard settings in. Each writer changes only its own field.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::json::to_pretty_text;

pub const FILE: &str = "app-settings.json";

// Two stores write fields of the one file; without this a save could drop the other's field.
static WRITING: Mutex<()> = Mutex::new(());

/// Puts `value` at `key` in the file, keeping its other fields; a file that is not a JSON object
/// is started again.
pub fn write_field(file: &Path, key: &str, value: Value) -> io::Result<()> {
    let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut obj = match fs::read_to_string(file) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(obj)) => obj,
            _ => Map::new(),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e),
    };
    obj.insert(key.to_owned(), value);
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, to_pretty_text(&Value::Object(obj)))?;
    fs::rename(&tmp, file)
}
