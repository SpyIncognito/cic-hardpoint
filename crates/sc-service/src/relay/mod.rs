//! Relay mode: Hardpoint on the game PC relays its pages to Hardpoint on a companion (a
//! laptop or tablet beside it). The game PC dials the companion; each page the companion opens is
//! carried over the link as a stream and runs on the game PC as a normal portal session with the
//! companion policy (`portal::policy`), so holds, caps and the fleet view are the game PC's.
//! The wire is PROTOCOL.md section 5.
//!
//! - [`Mode::Relay`] (game PC): the service as always, plus a dialer to the paired companion.
//! - [`Mode::Companion`]: the page server bridges every page to the link; the fleet client is
//!   stopped and the core follows no game, since everything comes from the game PC.
//!
//! Security is the pairing (SPAKE2 on an 8-digit code, bound to TLS) and the pinned relay keys,
//! not the address: any reachable address works. The relay key is its own (`relay-identity.key`),
//! never the fleet key, so a peer never learns the fleet identity.

pub mod companion;
pub mod game;
pub mod handshake;
pub mod pairing;
pub mod peers;
pub mod wire;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use fleet_proto::Identity;

use crate::app_settings;
use crate::fleet::identity;
use crate::json::js_string;
use crate::portal::session::now_ms;
use crate::service::App;
use pairing::Code;
use peers::{Peer, PeerStore, Role, clean_name};

/// The companion's relay port, beside the pages' 47520.
pub const DEFAULT_PORT: u16 = 47521;
pub const KEY_FILE: &str = "relay-identity.key";
/// The key in `app-settings.json`: `{ "mode": "off" | "relay" | "companion", "port"? }`.
pub const SETTINGS_KEY: &str = "relay";
/// A link that hears nothing for this long is taken as lost.
pub const LINK_TIMEOUT: Duration = Duration::from_secs(5);
pub const PING_EVERY: Duration = Duration::from_secs(1);
const NOTICE_FOR: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Off,
    /// The game PC relays to its companion.
    Relay,
    /// This machine shows the game PC's pages.
    Companion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Off,
    Connecting,
    Up,
    /// Not trying: disconnected by the user, or refused in a way a retry cannot fix.
    Stopped,
    /// The companion waits for the game PC.
    Listening,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub name: String,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_address: Option<String>,
    pub paired_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    /// As shown, `4821 7730`; absent once burned or expired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub left_ms: u64,
    pub failures: u32,
    pub burned: bool,
    /// A game PC that proved the code and waits for Pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<PeerView>,
}

/// What the pages and the game PC's title bar show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mode: Mode,
    pub state: LinkState,
    /// One line, as the title bar says it.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_ms: Option<u64>,
    /// The paired machine: the companion on a game PC, the game PC on a companion.
    pub peer: Option<PeerView>,
    pub name: String,
    pub fingerprint: String,
    pub version: String,
    /// Where a companion listens.
    pub port: u16,
    pub addresses: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingView>,
    /// A one-off line, such as a fleet change made from the companion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Relay to a companion was chosen before one was paired: the next pairing turns it on.
    pub relay_after_pairing: bool,
}

impl Status {
    /// The game PC's title bar.
    pub fn title(&self) -> String {
        match &self.notice {
            Some(n) => format!("Hardpoint: {} ({n})", self.text),
            None => format!("Hardpoint: {}", self.text),
        }
    }
}

/// How the relay is set up; tests choose the address and version.
#[derive(Debug, Clone)]
pub struct RelayOptions {
    /// Where a companion listens: all interfaces by default.
    pub bind: IpAddr,
    pub port: u16,
    /// The app version both sides must share.
    pub version: String,
    /// This machine's name as the other side shows it.
    pub name: String,
}

impl Default for RelayOptions {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: DEFAULT_PORT,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            name: machine_name(),
        }
    }
}

/// The computer's name, or `Hardpoint`.
pub fn machine_name() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    match clean_name(&name).as_str() {
        "unnamed" => "Hardpoint".to_owned(),
        _ => clean_name(&name).to_lowercase(),
    }
}

/// The address this machine reaches other machines from (the route out), for the companion to
/// show. Nothing is sent: connecting a UDP socket only picks the route.
pub fn local_addresses() -> Vec<String> {
    let probe = || -> std::io::Result<IpAddr> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
        Ok(socket.local_addr()?.ip())
    };
    probe()
        .ok()
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
        .map(|ip| vec![ip.to_string()])
        .unwrap_or_default()
}

/// `host:port`, with the relay port when none is given. `None` when it is not an address.
pub fn parse_address(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() || typed.contains(char::is_whitespace) || typed.contains('/') {
        return None;
    }
    if typed.parse::<SocketAddr>().is_ok() {
        return Some(typed.to_owned());
    }
    if let Ok(ip) = typed.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, DEFAULT_PORT).to_string());
    }
    match typed.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            let port: u16 = port.parse().ok().filter(|&p| p != 0)?;
            (!host.is_empty()).then(|| format!("{host}:{port}"))
        }
        Some(_) => None,
        None => Some(format!("{typed}:{DEFAULT_PORT}")),
    }
}

/// `host` and `port` of a stored `host:port`.
pub fn split_address(address: &str) -> Option<(String, u16)> {
    if let Ok(addr) = address.parse::<SocketAddr>() {
        return Some((addr.ip().to_string(), addr.port()));
    }
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Running {
    async fn end(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

#[derive(Default)]
struct Tasks {
    dialer: Option<Running>,
    listener: Option<Running>,
}

/// The state behind [`Status`].
struct Link {
    state: LinkState,
    text: String,
    retry_ms: Option<u64>,
}

pub struct Relay {
    options: RelayOptions,
    settings_file: Option<PathBuf>,
    identity: Identity,
    peers: PeerStore,
    mode: Mutex<Mode>,
    link: Mutex<Link>,
    notice: Mutex<Option<String>>,
    relay_after_pairing: Mutex<bool>,
    status: watch::Sender<Status>,
    app: OnceLock<Weak<App>>,
    tasks: tokio::sync::Mutex<Tasks>,
    companion: companion::State,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn peer_view(peer: &Peer) -> PeerView {
    PeerView {
        name: peer.name.clone(),
        fingerprint: peer.key.fingerprint(),
        last_address: peer.last_address.clone(),
        paired_at: peer.paired_at,
    }
}

fn problems(list: &[&str]) -> Value {
    json!({ "problems": list })
}

/// The mode and port saved in `app-settings.json`.
pub fn saved(settings_dir: Option<&std::path::Path>) -> (Mode, Option<u16>) {
    let Some(text) =
        settings_dir.and_then(|d| std::fs::read_to_string(d.join(app_settings::FILE)).ok())
    else {
        return (Mode::Off, None);
    };
    let relay = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get(SETTINGS_KEY).cloned());
    let mode = relay
        .as_ref()
        .and_then(|r| r.get("mode"))
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    let port = relay
        .as_ref()
        .and_then(|r| r.get("port"))
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok());
    (mode, port)
}

impl Relay {
    pub fn new(
        options: RelayOptions,
        settings_dir: Option<&std::path::Path>,
    ) -> std::io::Result<Self> {
        let file = |name| settings_dir.map(|d| d.join(name));
        let identity =
            identity::load_or_create_as(file(KEY_FILE).as_deref(), "relay").or_else(|e| {
                tracing::error!(
                    "[relay] the relay key cannot be read ({e}); using one for this run only"
                );
                identity::load_or_create_as(None, "relay")
            })?;
        let status = Status {
            mode: Mode::Off,
            state: LinkState::Off,
            text: "not relaying".to_owned(),
            retry_ms: None,
            peer: None,
            name: options.name.clone(),
            fingerprint: identity.public().fingerprint(),
            version: options.version.clone(),
            port: options.port,
            addresses: vec![],
            pairing: None,
            notice: None,
            relay_after_pairing: false,
        };
        Ok(Self {
            options,
            settings_file: file(app_settings::FILE),
            identity,
            peers: PeerStore::new(file(peers::FILE)),
            mode: Mutex::new(Mode::Off),
            link: Mutex::new(Link {
                state: LinkState::Off,
                text: "not relaying".to_owned(),
                retry_ms: None,
            }),
            notice: Mutex::new(None),
            relay_after_pairing: Mutex::new(false),
            status: watch::Sender::new(status),
            app: OnceLock::new(),
            tasks: tokio::sync::Mutex::default(),
            companion: companion::State::default(),
        })
    }

    /// Called once the service's state exists; sessions for the companion run on it.
    pub fn attach(&self, app: &Arc<App>) {
        let _ = self.app.set(Arc::downgrade(app));
        self.refresh();
    }

    fn app(&self) -> Option<Arc<App>> {
        self.app.get().and_then(Weak::upgrade)
    }

    pub fn mode(&self) -> Mode {
        *lock(&self.mode)
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    pub fn options(&self) -> &RelayOptions {
        &self.options
    }

    pub fn public_key(&self) -> fleet_proto::PublicKey {
        self.identity.public()
    }

    /// The role of the machine this one pairs with, in the current mode.
    fn peer_role(&self) -> Role {
        if self.mode() == Mode::Companion {
            Role::Game
        } else {
            Role::Companion
        }
    }

    pub fn peer(&self, role: Role) -> Option<Peer> {
        self.peers.get(role)
    }

    fn set_link(&self, state: LinkState, text: impl Into<String>, retry_ms: Option<u64>) {
        *lock(&self.link) = Link {
            state,
            text: text.into(),
            retry_ms,
        };
        self.refresh();
    }

    /// Shows `text` beside the status for a while (the game PC's title bar shows it).
    pub fn note(self: &Arc<Self>, text: String) {
        *lock(&self.notice) = Some(text.clone());
        self.refresh();
        let relay = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(NOTICE_FOR).await;
            let mut notice = lock(&relay.notice);
            if notice.as_deref() == Some(text.as_str()) {
                *notice = None;
                drop(notice);
                relay.refresh();
            }
        });
    }

    /// Builds the status from the parts and sends it when it changed.
    fn refresh(&self) {
        let mode = self.mode();
        let role = self.peer_role();
        let link = lock(&self.link);
        let pairing = (mode == Mode::Companion)
            .then(|| self.companion.pairing_view())
            .flatten();
        let status = Status {
            mode,
            state: link.state,
            text: link.text.clone(),
            retry_ms: link.retry_ms,
            peer: self.peers.get(role).as_ref().map(peer_view),
            name: self.options.name.clone(),
            fingerprint: self.identity.public().fingerprint(),
            version: self.options.version.clone(),
            port: self.companion.bound_port().unwrap_or(self.options.port),
            addresses: if mode == Mode::Companion {
                local_addresses()
            } else {
                vec![]
            },
            pairing,
            notice: lock(&self.notice).clone(),
            relay_after_pairing: *lock(&self.relay_after_pairing),
        };
        drop(link);
        self.status.send_if_modified(|s| {
            let changed = *s != status;
            *s = status;
            changed
        });
    }

    /// Starts in the saved mode, at service start.
    pub async fn resume(self: &Arc<Self>, mode: Mode) {
        if let Err(e) = self.set_mode(mode, false).await {
            tracing::warn!("[relay] cannot start in {mode:?} mode: {e}");
        }
    }

    /// Switches mode: stops what the old mode ran, starts the new one's, and saves it.
    pub async fn set_mode(self: &Arc<Self>, mode: Mode, save: bool) -> Result<(), String> {
        *lock(&self.relay_after_pairing) = false;
        let mut tasks = self.tasks.lock().await;
        let was = self.mode();
        if let Some(r) = tasks.dialer.take() {
            r.end().await;
        }
        if mode != Mode::Companion
            && let Some(r) = tasks.listener.take()
        {
            r.end().await;
            self.companion.close_pairing();
        }
        let app = self.app();
        if was == Mode::Companion
            && mode != Mode::Companion
            && let Some(app) = &app
        {
            app.fleet_client.resume();
        }
        let started = match mode {
            Mode::Off => {
                *lock(&self.mode) = mode;
                self.set_link(LinkState::Off, "not relaying", None);
                Ok(())
            }
            Mode::Relay => {
                *lock(&self.mode) = mode;
                tasks.dialer = Some(self.start_dialer());
                Ok(())
            }
            Mode::Companion => {
                if was != Mode::Companion
                    && let Some(app) = &app
                {
                    app.fleet_client.close().await;
                }
                *lock(&self.mode) = mode;
                if tasks.listener.is_none() {
                    match companion::listen(Arc::clone(self)).await {
                        Ok(r) => {
                            tasks.listener = Some(r);
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Ok(())
                }
            }
        };
        if let Err(e) = started {
            *lock(&self.mode) = Mode::Off;
            self.set_link(LinkState::Off, "not relaying", None);
            if let Some(app) = &app {
                app.fleet_client.resume();
            }
            return Err(e);
        }
        if save {
            self.save(mode);
        }
        self.refresh();
        Ok(())
    }

    /// Stops the dialer and the listener, as the service closes.
    pub async fn close(&self) {
        let mut tasks = self.tasks.lock().await;
        for r in [tasks.dialer.take(), tasks.listener.take()]
            .into_iter()
            .flatten()
        {
            r.end().await;
        }
    }

    fn save(&self, mode: Mode) {
        let Some(file) = &self.settings_file else {
            return;
        };
        let mut value = json!({ "mode": mode });
        if self.options.port != DEFAULT_PORT {
            value["port"] = json!(self.options.port);
        }
        if let Err(e) = app_settings::write_field(file, SETTINGS_KEY, value) {
            tracing::warn!("[relay] cannot save the relay mode: {e}");
        }
    }

    fn start_dialer(self: &Arc<Self>) -> Running {
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(game::run(Arc::clone(self), stopped));
        Running { stop, task }
    }

    /// Game PC: dial the companion again (after Disconnect or a refusal).
    pub async fn connect(self: &Arc<Self>) -> Result<(), String> {
        if self.mode() != Mode::Relay {
            return Err("Turn relay mode on first".to_owned());
        }
        let mut tasks = self.tasks.lock().await;
        if let Some(r) = tasks.dialer.take() {
            r.end().await;
        }
        tasks.dialer = Some(self.start_dialer());
        Ok(())
    }

    /// Game PC: close the link and stop dialling; relay mode stays on.
    pub async fn disconnect(&self) {
        if let Some(r) = self.tasks.lock().await.dialer.take() {
            r.end().await;
        }
        if self.mode() == Mode::Relay {
            self.set_link(LinkState::Stopped, "not connected", None);
        }
    }

    /// Game PC: pair with the companion at `address` that shows `code`. Replaces the paired
    /// companion; the page asks before.
    pub async fn pair(self: &Arc<Self>, address: &str, code: &str) -> Value {
        if self.mode() == Mode::Companion {
            return problems(&[
                "This machine is a companion; leave companion mode to pair a companion",
            ]);
        }
        let Some(address) = parse_address(address) else {
            return problems(&[
                "The address must be the companion's host or IP, with :port if not 47521",
            ]);
        };
        let Some(code) = Code::parse(code) else {
            return problems(&["The code is the 8 digits the companion shows"]);
        };
        match game::pair(self, &address, &code).await {
            Ok(peer) => {
                let view = peer_view(&peer);
                if let Err(e) = self.peers.set(peer) {
                    return problems(&[&format!("Paired, but it cannot be saved: {e}")]);
                }
                let wanted = std::mem::take(&mut *lock(&self.relay_after_pairing));
                if self.mode() == Mode::Relay {
                    let _ = self.connect().await;
                } else if wanted && let Err(e) = self.set_mode(Mode::Relay, true).await {
                    tracing::warn!("[relay] paired, but relaying did not start: {e}");
                }
                self.refresh();
                json!({ "problems": [], "peer": view })
            }
            Err(e) => problems(&[&format!("Not paired: {e}")]),
        }
    }

    /// Forgets the paired machine and closes any link to it.
    pub async fn unpair(self: &Arc<Self>) -> Result<bool, String> {
        let role = self.peer_role();
        let removed = self.peers.remove(role).map_err(|e| e.to_string())?;
        match self.mode() {
            Mode::Relay => self.disconnect().await,
            Mode::Companion => self.companion.drop_link(),
            Mode::Off => {}
        }
        if self.mode() == Mode::Relay {
            self.set_link(LinkState::Stopped, "no companion is paired", None);
        }
        self.refresh();
        Ok(removed)
    }

    /// The page messages `relay.*`, from a page this machine served.
    pub async fn page_message(self: &Arc<Self>, kind: &str, msg: &Value) -> Result<Value, String> {
        let ok = || json!({ "problems": [] });
        let field = |name| js_string(msg.get(name));
        Ok(match kind {
            "relay.status" => serde_json::to_value(self.status()).map_err(|e| e.to_string())?,
            "relay.mode" => {
                let Ok(mode) =
                    serde_json::from_value::<Mode>(msg.get("mode").cloned().unwrap_or(Value::Null))
                else {
                    return Ok(problems(&["mode must be off, relay or companion"]));
                };
                // The choice is kept until a companion is paired, rather than refused and lost.
                if mode == Mode::Relay && self.peers.get(Role::Companion).is_none() {
                    if self.mode() == Mode::Companion {
                        self.set_mode(Mode::Off, true).await?;
                    }
                    *lock(&self.relay_after_pairing) = true;
                    self.refresh();
                    return Ok(ok());
                }
                match self.set_mode(mode, true).await {
                    Ok(()) => ok(),
                    Err(e) => problems(&[&e]),
                }
            }
            "relay.pair" => self.pair(&field("address"), &field("code")).await,
            "relay.connect" => match self.connect().await {
                Ok(()) => ok(),
                Err(e) => problems(&[&e]),
            },
            "relay.disconnect" => {
                self.disconnect().await;
                ok()
            }
            "relay.unpair" => match self.unpair().await {
                Ok(removed) => json!({ "problems": [], "removed": removed }),
                Err(e) => problems(&[&e]),
            },
            "relay.rename" => {
                let name = clean_name(&field("name"));
                match self.peers.update(self.peer_role(), |p| p.name = name) {
                    Ok(true) => {
                        self.refresh();
                        ok()
                    }
                    Ok(false) => problems(&["Nothing is paired"]),
                    Err(e) => problems(&[&e.to_string()]),
                }
            }
            "relay.address" => {
                let Some(address) = parse_address(&field("address")) else {
                    return Ok(problems(&[
                        "The address must be a host or IP, with :port if not 47521",
                    ]));
                };
                match self
                    .peers
                    .update(Role::Companion, |p| p.last_address = Some(address))
                {
                    Ok(true) => {
                        if self.mode() == Mode::Relay {
                            let _ = self.connect().await;
                        }
                        self.refresh();
                        ok()
                    }
                    Ok(false) => problems(&["No companion is paired"]),
                    Err(e) => problems(&[&e.to_string()]),
                }
            }
            "relay.code" => match self.companion.open_pairing(self) {
                Ok(()) => ok(),
                Err(e) => problems(&[&e]),
            },
            "relay.cancel" => {
                self.companion.close_pairing();
                self.refresh();
                ok()
            }
            "relay.accept" => match self
                .companion
                .answer(msg.get("pair") != Some(&Value::Bool(false)))
            {
                Ok(()) => ok(),
                Err(e) => problems(&[&e]),
            },
            _ => return Err(format!("unknown relay message {kind}")),
        })
    }

    pub(crate) fn paired_now(
        &self,
        name: &str,
        key: fleet_proto::PublicKey,
        role: Role,
        address: Option<String>,
    ) -> Peer {
        Peer {
            name: clean_name(name),
            key,
            last_address: address,
            paired_at: now_ms(),
            role,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_gets_the_relay_port_unless_it_names_one() {
        assert_eq!(
            parse_address("198.51.100.20").as_deref(),
            Some("198.51.100.20:47521")
        );
        assert_eq!(
            parse_address(" 203.0.113.2:5000 ").as_deref(),
            Some("203.0.113.2:5000")
        );
        assert_eq!(
            parse_address("laptop.lan").as_deref(),
            Some("laptop.lan:47521")
        );
        assert_eq!(
            parse_address("laptop:47600").as_deref(),
            Some("laptop:47600")
        );
        assert_eq!(parse_address("fe80::1").as_deref(), Some("[fe80::1]:47521"));
        assert_eq!(parse_address("[::1]:47600").as_deref(), Some("[::1]:47600"));
        for bad in ["", "a b", "http://x", "laptop:0", "laptop:x", ":5"] {
            assert_eq!(parse_address(bad), None, "{bad}");
        }
        assert_eq!(split_address("[::1]:47600"), Some(("::1".into(), 47600)));
        assert_eq!(
            split_address("laptop:47521"),
            Some(("laptop".into(), 47521))
        );
    }

    #[test]
    fn the_mode_is_read_from_the_app_settings() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(saved(Some(dir.path())), (Mode::Off, None));
        std::fs::write(
            dir.path().join(app_settings::FILE),
            r#"{ "splash": false, "relay": { "mode": "companion", "port": 47600 } }"#,
        )
        .unwrap();
        assert_eq!(saved(Some(dir.path())), (Mode::Companion, Some(47600)));
    }

    #[test]
    fn the_title_says_the_status_and_any_notice() {
        let relay = Relay::new(RelayOptions::default(), None).unwrap();
        let mut status = relay.status();
        status.text = "relaying to laptop".into();
        assert_eq!(status.title(), "Hardpoint: relaying to laptop");
        status.notice = Some("fleet sharing changed on laptop".into());
        assert_eq!(
            status.title(),
            "Hardpoint: relaying to laptop (fleet sharing changed on laptop)"
        );
    }
}
