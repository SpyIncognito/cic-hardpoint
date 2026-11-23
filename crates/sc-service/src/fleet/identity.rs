//! This player's fleet identity: an ed25519 key pair made on first use and kept in the settings
//! folder as `fleet-identity.key`, readable by the user only. Its public half is the member id on
//! cards; roles and bans bind to it, never to the handle. The secret never leaves this file.
//!
//! On Windows the settings folder is in the user's own profile, which only that user (and
//! administrators) can read; elsewhere the file is made with mode 0600 and put back to it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fleet_proto::Identity;

pub const KEY_FILE: &str = "fleet-identity.key";

fn invalid(file: &Path, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} is not a {what} key", file.display()),
    )
}

fn random() -> io::Result<Identity> {
    Identity::generate().map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(unix)]
fn user_only(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn user_only(_: &mut OpenOptions) {}

#[cfg(unix)]
fn keep_user_only(file: &File, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    if mode & 0o077 != 0 {
        tracing::warn!(
            "[identity] {} was readable by others; made it user-only",
            path.display()
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn keep_user_only(_: &File, _: &Path) -> io::Result<()> {
    Ok(())
}

/// The key in `file`, made there on first use; a key in memory only when there is no file.
pub fn load_or_create(file: Option<&Path>) -> io::Result<Identity> {
    load_or_create_as(file, "fleet")
}

/// [`load_or_create`] for another key kept the same way (`what` names it in logs and errors),
/// such as the relay's own identity.
pub fn load_or_create_as(file: Option<&Path>, what: &str) -> io::Result<Identity> {
    let Some(path) = file else {
        return random();
    };
    match File::open(path) {
        Ok(file) => {
            keep_user_only(&file, path)?;
            let mut text = String::new();
            file.take(256).read_to_string(&mut text)?;
            let seed = URL_SAFE_NO_PAD
                .decode(text.trim())
                .map_err(|_| invalid(path, what))?;
            let seed: [u8; 32] = seed.try_into().map_err(|_| invalid(path, what))?;
            Ok(Identity::from_seed(seed))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                fs::create_dir_all(dir)?;
            }
            let identity = random()?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            user_only(&mut options);
            let mut out = options.open(path)?;
            writeln!(out, "{}", URL_SAFE_NO_PAD.encode(identity.seed()))?;
            out.sync_all()?;
            tracing::info!(
                "[{what}] made this machine's {what} key ({})",
                identity.public().fingerprint()
            );
            Ok(identity)
        }
        Err(e) => Err(e),
    }
}
