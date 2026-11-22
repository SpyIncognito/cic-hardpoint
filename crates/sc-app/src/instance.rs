//! One app at a time. The first launch holds a lock on a file in the settings folder and listens
//! on a 127.0.0.1 port it writes, with a per-launch secret, to a second file; a second launch finds
//! the lock taken, asks the first to show its dashboard, and exits.

use std::fs::{self, File, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LOCK: &str = "hardpoint.lock";
const ADDRESS: &str = "hardpoint.instance";
const ASK: &str = "show";
/// The first launch writes its address just after taking the lock.
const WAIT_FOR_FIRST: Duration = Duration::from_secs(3);

pub enum Launch {
    /// This is the only app; keep the guard for as long as it runs.
    First(Guard),
    /// Another app was running and was asked to show itself.
    Second,
}

/// Holds the lock; the listener hands each valid request to the callback.
pub struct Guard {
    _lock: File,
    listener: TcpListener,
    secret: String,
}

impl Guard {
    /// Calls `on_show` (on a thread of its own) each time another launch asks.
    pub fn serve(self, on_show: impl Fn() + Send + 'static) {
        std::thread::spawn(move || {
            // Bound by name so the closure owns the lock too, not only the fields it reads.
            let Guard {
                _lock: _held,
                listener,
                secret,
            } = self;
            let wanted = format!("{ASK} {secret}");
            for stream in listener.incoming().flatten() {
                if read_request(stream).is_some_and(|line| line == wanted) {
                    on_show();
                }
            }
        });
    }
}

fn read_request(stream: TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line).ok()?;
    Some(line.trim_end().to_owned())
}

fn secret() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn launch(dir: &Path) -> io::Result<Launch> {
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            ask_first(&dir.join(ADDRESS))?;
            return Ok(Launch::Second);
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let secret = secret()?;
    let port = listener.local_addr()?.port();
    fs::write(dir.join(ADDRESS), format!("{port} {secret}\n"))?;
    Ok(Launch::First(Guard {
        _lock: lock,
        listener,
        secret,
    }))
}

fn ask_first(address: &PathBuf) -> io::Result<()> {
    let started = Instant::now();
    loop {
        let tried = fs::read_to_string(address).and_then(|text| {
            let (port, secret) = text
                .trim()
                .split_once(' ')
                .ok_or_else(|| io::Error::other("no address yet"))?;
            let port: u16 = port.parse().map_err(io::Error::other)?;
            let mut s = TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, port).into(),
                Duration::from_secs(2),
            )?;
            s.write_all(format!("{ASK} {secret}\n").as_bytes())
        });
        match tried {
            Ok(()) => return Ok(()),
            Err(e) if started.elapsed() > WAIT_FOR_FIRST => {
                return Err(io::Error::other(format!(
                    "another Hardpoint is running but did not answer: {e}"
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_launch_asks_the_first_and_a_wrong_secret_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let Launch::First(guard) = launch(dir.path()).unwrap() else {
            panic!("the first launch is first");
        };
        let port = guard.listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        guard.serve(move || tx.send(()).unwrap());

        let mut wrong = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        wrong.write_all(b"show 00\n").unwrap();
        drop(wrong);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());

        assert!(matches!(launch(dir.path()).unwrap(), Launch::Second));
        rx.recv_timeout(Duration::from_secs(3))
            .expect("the first was asked to show");
    }
}
