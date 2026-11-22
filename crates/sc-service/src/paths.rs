//! Where the service keeps its files. The settings folder is Hardpoint's own; the folder the
//! TypeScript service and earlier builds used (sc-key-emulator) is copied over once by
//! [`migrate_settings`] and then left alone as a backup. The bus file stays where the Stream Deck
//! plugin looks for it, in the old folder, so the plugin still finds the app.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const APP: &str = "Hardpoint";
/// On Linux and other XDG systems, folder names are lower case by convention.
const APP_XDG: &str = "hardpoint";
const LEGACY: &str = "sc-key-emulator";

/// The environment and platform the rules read, so tests can give their own.
#[derive(Debug, Clone)]
pub struct Env {
    pub vars: fn(&str) -> Option<OsString>,
    pub home: Option<PathBuf>,
    pub windows: bool,
    pub mac: bool,
}

impl Env {
    pub fn current() -> Self {
        Self {
            vars: |name| env::var_os(name),
            home: env::home_dir(),
            windows: cfg!(windows),
            mac: cfg!(target_os = "macos"),
        }
    }

    fn home(&self) -> PathBuf {
        self.home.clone().unwrap_or_default()
    }

    /// %APPDATA% on Windows, ~/Library/Application Support on macOS, $XDG_CONFIG_HOME (~/.config
    /// when unset) elsewhere. As in the TS, a set but empty variable is used as it is.
    fn config_base(&self) -> PathBuf {
        if self.windows {
            (self.vars)("APPDATA").map_or_else(
                || self.home().join("AppData").join("Roaming"),
                PathBuf::from,
            )
        } else if self.mac {
            self.home().join("Library").join("Application Support")
        } else {
            (self.vars)("XDG_CONFIG_HOME")
                .map_or_else(|| self.home().join(".config"), PathBuf::from)
        }
    }
}

/// The settings folder (sequences.json, dashboards.json and the rest): %APPDATA%\Hardpoint on
/// Windows, ~/Library/Application Support/Hardpoint on macOS, $XDG_CONFIG_HOME/hardpoint elsewhere.
pub fn config_dir(env: &Env) -> PathBuf {
    let name = if env.windows || env.mac { APP } else { APP_XDG };
    env.config_base().join(name)
}

/// The folder the TypeScript service and builds before the rename used, by the same rules.
pub fn legacy_config_dir(env: &Env) -> PathBuf {
    env.config_base().join(LEGACY)
}

/// Where the service advertises its bus: SC_CONTROL_BUS_FILE when set, otherwise
/// %APPDATA%\sc-key-emulator\bus.json on Windows and $XDG_CONFIG_HOME/sc-key-emulator/bus.json
/// (~/.config when unset or empty) elsewhere, macOS included. This is where the Stream Deck
/// plugin looks (its defaultBusFile), so it stays in the old folder.
pub fn default_bus_file(env: &Env) -> PathBuf {
    let non_empty = |name| (env.vars)(name).filter(|v| !v.is_empty());
    if let Some(file) = non_empty("SC_CONTROL_BUS_FILE") {
        return PathBuf::from(file);
    }
    let base = if env.windows {
        (env.vars)("APPDATA")
            .map_or_else(|| env.home().join("AppData").join("Roaming"), PathBuf::from)
    } else {
        non_empty("XDG_CONFIG_HOME").map_or_else(|| env.home().join(".config"), PathBuf::from)
    };
    base.join(LEGACY).join("bus.json")
}

/// What [`migrate_settings`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// The new folder was there already; nothing was read or written.
    NewExists,
    /// Neither folder was there.
    NothingToCopy,
    /// The old folder was copied to the new one; the paths are relative to the folder.
    Copied(Vec<PathBuf>),
}

/// [`migrate_settings`] from the old folder to the new one, by `env`'s rules. Run it before
/// anything creates the new folder (the log file does), and log the result with [`log_migration`].
pub fn migrate_default_settings(env: &Env) -> io::Result<Migration> {
    migrate_settings(&legacy_config_dir(env), &config_dir(env))
}

pub fn log_migration(env: &Env, result: &io::Result<Migration>) {
    let (from, to) = (legacy_config_dir(env), config_dir(env));
    match result {
        Ok(Migration::Copied(files)) => {
            tracing::info!(
                "settings moved to {}: copied {} file(s) from {}, which is kept as a backup",
                to.display(),
                files.len(),
                from.display()
            );
            for file in files {
                tracing::info!("  copied {}", file.display());
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(
            "settings not copied from {} to {}: {e}; starting with defaults",
            from.display(),
            to.display()
        ),
    }
}

/// Left behind: the bus file (the plugin's, still written in the old folder), the old app's
/// lock, instance and log files, and the WebView2 cache.
fn skipped(name: &str) -> bool {
    matches!(
        name,
        "bus.json"
            | "sc-control-app.lock"
            | "sc-control-app.instance"
            | "sc-control-app.log"
            | "WebView2"
    ) || name.ends_with(".tmp")
}

/// Once: when `new` is not there and `old` is, copy `old` into `new`. The copy is made in a
/// sibling folder and renamed into place, so a failed copy never leaves a partial `new` that
/// would stop a later try. `old` is never changed, and an existing `new` is never touched.
pub fn migrate_settings(old: &Path, new: &Path) -> io::Result<Migration> {
    if new.exists() {
        return Ok(Migration::NewExists);
    }
    if !old.is_dir() {
        return Ok(Migration::NothingToCopy);
    }
    let parent = new.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = new.file_name().unwrap_or_default().to_string_lossy();
    let staging = parent.join(format!(".{name}.migrating-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let mut copied = Vec::new();
    let result = copy_tree(old, &staging, Path::new(""), &mut copied)
        .and_then(|()| fs::rename(&staging, new));
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    copied.sort();
    Ok(Migration::Copied(copied))
}

fn copy_tree(from: &Path, to: &Path, rel: &Path, copied: &mut Vec<PathBuf>) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if skipped(&name.to_string_lossy()) {
            continue;
        }
        let kind = entry.file_type()?;
        let rel = rel.join(&name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &to.join(&name), &rel, copied)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), to.join(&name))?;
            copied.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: fn(&str) -> Option<OsString>, windows: bool, mac: bool) -> Env {
        Env {
            vars,
            home: Some(PathBuf::from("/home/u")),
            windows,
            mac,
        }
    }

    #[test]
    fn the_settings_folder_is_hardpoint_and_the_bus_file_stays_where_the_plugin_looks() {
        let none = env(|_| None, false, false);
        assert_eq!(config_dir(&none), Path::new("/home/u/.config/hardpoint"));
        assert_eq!(
            legacy_config_dir(&none),
            Path::new("/home/u/.config/sc-key-emulator")
        );
        assert_eq!(
            default_bus_file(&none),
            Path::new("/home/u/.config/sc-key-emulator/bus.json")
        );

        let xdg = env(
            |k| (k == "XDG_CONFIG_HOME").then(|| "/x".into()),
            false,
            false,
        );
        assert_eq!(config_dir(&xdg), Path::new("/x/hardpoint"));
        assert_eq!(legacy_config_dir(&xdg), Path::new("/x/sc-key-emulator"));

        // An empty XDG_CONFIG_HOME: the settings folder keeps it (`??`), the bus file does not (`||`).
        let empty = env(
            |k| (k == "XDG_CONFIG_HOME").then(OsString::new),
            false,
            false,
        );
        assert_eq!(config_dir(&empty), Path::new("hardpoint"));
        assert_eq!(
            default_bus_file(&empty),
            Path::new("/home/u/.config/sc-key-emulator/bus.json")
        );

        let mac = env(|_| None, false, true);
        assert_eq!(
            config_dir(&mac),
            Path::new("/home/u/Library/Application Support/Hardpoint")
        );
        assert_eq!(
            default_bus_file(&mac),
            Path::new("/home/u/.config/sc-key-emulator/bus.json")
        );

        let bus = env(
            |k| (k == "SC_CONTROL_BUS_FILE").then(|| "/tmp/b.json".into()),
            false,
            false,
        );
        assert_eq!(default_bus_file(&bus), Path::new("/tmp/b.json"));

        let win = env(|k| (k == "APPDATA").then(|| "/appdata".into()), true, false);
        assert_eq!(config_dir(&win), Path::new("/appdata/Hardpoint"));
        assert_eq!(
            legacy_config_dir(&win),
            Path::new("/appdata/sc-key-emulator")
        );
        assert_eq!(
            default_bus_file(&win),
            Path::new("/appdata/sc-key-emulator/bus.json")
        );
    }

    const FILES: [&str; 7] = [
        "app-settings.json",
        "dashboards.json",
        "dashboards.json.bak",
        "fleet-view.json",
        "sequences.json",
        "ship-support.json",
        "themes.json",
    ];

    /// An old folder with every settings file, two recordings, and what is left behind.
    fn old_folder(root: &Path) -> PathBuf {
        let old = root.join("sc-key-emulator");
        fs::create_dir_all(old.join("recordings")).unwrap();
        for f in FILES {
            fs::write(old.join(f), format!("old {f}")).unwrap();
        }
        fs::write(old.join("recordings/telemetry-1.jsonl"), "{}\n").unwrap();
        fs::write(old.join("recordings/telemetry-1.actions.json"), "[]\n").unwrap();
        fs::write(old.join("bus.json"), "{}").unwrap();
        fs::write(old.join("sc-control-app.log"), "log").unwrap();
        old
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(dir).unwrap();
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn old_only_is_copied_with_its_recordings_and_left_as_it_was() {
        let root = tempfile::tempdir().unwrap();
        let old = old_folder(root.path());
        let before = listing(&old);
        let new = root.path().join("hardpoint");
        let Migration::Copied(copied) = migrate_settings(&old, &new).unwrap() else {
            panic!("expected a copy");
        };
        let mut want: Vec<String> = FILES.iter().map(|f| (*f).to_owned()).collect();
        want.push("recordings/telemetry-1.actions.json".to_owned());
        want.push("recordings/telemetry-1.jsonl".to_owned());
        want.sort();
        let copied: Vec<String> = copied
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(copied, want);
        assert_eq!(listing(&new), want);
        assert_eq!(
            fs::read_to_string(new.join("dashboards.json")).unwrap(),
            "old dashboards.json"
        );
        assert_eq!(
            fs::read_to_string(new.join("recordings/telemetry-1.jsonl")).unwrap(),
            "{}\n"
        );
        assert_eq!(listing(&old), before, "the old folder is the backup");
        assert!(!new.join("bus.json").exists());
        let leftovers: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("migrating"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn new_only_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let new = root.path().join("hardpoint");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("dashboards.json"), "new").unwrap();
        let old = root.path().join("sc-key-emulator");
        assert_eq!(migrate_settings(&old, &new).unwrap(), Migration::NewExists);
        assert_eq!(listing(&new), ["dashboards.json"]);
        assert!(!old.exists());
    }

    #[test]
    fn both_never_overwrites_the_new_folder() {
        let root = tempfile::tempdir().unwrap();
        let old = old_folder(root.path());
        let new = root.path().join("hardpoint");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("dashboards.json"), "new").unwrap();
        assert_eq!(migrate_settings(&old, &new).unwrap(), Migration::NewExists);
        assert_eq!(listing(&new), ["dashboards.json"]);
        assert_eq!(
            fs::read_to_string(new.join("dashboards.json")).unwrap(),
            "new"
        );
    }

    #[test]
    fn neither_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("sc-key-emulator");
        let new = root.path().join("hardpoint");
        assert_eq!(
            migrate_settings(&old, &new).unwrap(),
            Migration::NothingToCopy
        );
        assert!(!new.exists() && !old.exists());
    }

    #[test]
    fn a_second_start_copies_nothing() {
        let root = tempfile::tempdir().unwrap();
        let old = old_folder(root.path());
        let new = root.path().join("hardpoint");
        assert!(matches!(
            migrate_settings(&old, &new).unwrap(),
            Migration::Copied(_)
        ));
        fs::write(old.join("dashboards.json"), "changed later").unwrap();
        assert_eq!(migrate_settings(&old, &new).unwrap(), Migration::NewExists);
        assert_eq!(
            fs::read_to_string(new.join("dashboards.json")).unwrap(),
            "old dashboards.json"
        );
    }
}
