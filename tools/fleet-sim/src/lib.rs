//! Demo fleet members for testing a CIC relay. Not part of Hardpoint: it is never built into
//! `hardpoint.exe` or shipped.
//!
//! Each member is a real Hardpoint fleet client ([`FleetClient`] from `sc-service`, the same
//! handshake, join, consent filter and card builder as the app) with its own ed25519 key and
//! files in its own folder under the data folder, so a restart rejoins by key. Its game is a
//! scrubbed recording replayed from its own offset ([`game::SimGame`]); a few members also go
//! down, die and respawn on a script, through the game events the card state tracker reads.
//!
//! Two ways in: peer to peer with one `code=` invite for all ([`FleetClient`]), or hosted
//! against a CIC relay with one `connect=` invite per member ([`hosted::HostedClient`]). A
//! member can sit in a ship as its pilot, copilot or crew ([`scenario`]), which its cards show.

pub mod game;
pub mod hosted;
pub mod invite;
pub mod recording;
pub mod scenario;
pub mod steps;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleet_proto::{FIELDS, Invite, PublicKey, RefusalCode, SendConsent, Status};
use sc_core::{ControlCore, ControlCoreOptions};
use sc_game::GameApi;
use sc_service::fleet::store::{CONSENT_FILE, FLEETS_FILE};
use sc_service::fleet::{
    Connector, ConsentStore, FleetClient, FleetHub, FleetViewStore, FleetsStore, LinkState,
    TlsConnector, identity,
};

use game::{Script, SimGame};
use hosted::{BINDING_FILE, HostedClient, HostedOptions};
use invite::HostedInvite;
use recording::Recording;
use scenario::{PilotDrop, SeatRole, Seating, Ship, seat_members};

pub const MAX_MEMBERS: usize = 64;
/// Made-up call signs; past the end they repeat with a number.
pub const NAMES: [&str; 16] = [
    "Vega", "Rook", "Kestrel", "Wren", "Ash", "Hawk", "Juno", "Orca", "Pike", "Sable", "Talon",
    "Lynx", "Mako", "Nova", "Onyx", "Quill",
];

/// The handle of member `n` (from 0), such as `Demo Vega`.
pub fn handle(prefix: &str, n: usize) -> String {
    let name = NAMES[n % NAMES.len()];
    let round = n / NAMES.len();
    let base = if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix} {name}")
    };
    if round == 0 {
        base
    } else {
        format!("{base} {}", round + 1)
    }
}

/// Every field, detail included: demo members exist to be seen.
pub fn default_consent() -> SendConsent {
    let mut consent = SendConsent::none();
    for f in FIELDS.iter() {
        consent.set(f.id, true);
    }
    consent
}

/// SplitMix64: enough to spread offsets and pick scripted members from `--seed`.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..1`.
    pub fn fraction(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How the members get in.
#[derive(Debug, Clone)]
pub enum Join {
    /// Peer to peer: one `code=` invite for every member.
    Code(String),
    /// Hosted: one `connect=` invite per member, each for its own CIC account.
    Hosted(Vec<HostedInvite>),
}

pub struct Options {
    pub join: Join,
    /// Peer to peer only; hosted, it is the number of invites.
    pub count: usize,
    pub seed: u64,
    pub handle_prefix: String,
    pub data_dir: PathBuf,
    pub recordings: Vec<Arc<Recording>>,
    /// The consent a member starts with; a member's own `fleet-consent.json` wins once it exists.
    pub consent: SendConsent,
    /// How often a member's game sends a frame.
    pub frame_every: Duration,
    /// The scripted members' downed, dead and respawn cycle.
    pub cycle: Duration,
    /// How fleet connections are opened; TLS by default.
    pub connector: Option<Arc<dyn Connector>>,
    /// Seats for the members in order, from the first.
    pub ships: Vec<Ship>,
    /// How many members follow the downed, dead and respawn script; a third when `None`.
    pub scripted: Option<usize>,
    /// Takes each seated pilot off line for this long every cycle.
    pub pilot_drop: Option<Duration>,
    /// Hosted: the event to choose when the relay offers a list; the first when `None`.
    pub choose: Option<String>,
}

impl Options {
    /// Peer to peer, with a `code=` invite.
    pub fn new(invite: String, recordings: Vec<Arc<Recording>>, data_dir: PathBuf) -> Self {
        Self::with(Join::Code(invite), 8, recordings, data_dir)
    }

    /// Hosted, one member per invite.
    pub fn hosted(
        invites: Vec<HostedInvite>,
        recordings: Vec<Arc<Recording>>,
        data_dir: PathBuf,
    ) -> Self {
        let count = invites.len();
        Self::with(Join::Hosted(invites), count, recordings, data_dir)
    }

    fn with(join: Join, count: usize, recordings: Vec<Arc<Recording>>, data_dir: PathBuf) -> Self {
        Self {
            join,
            count,
            seed: 1,
            handle_prefix: "Demo".to_owned(),
            data_dir,
            recordings,
            consent: default_consent(),
            frame_every: Duration::from_millis(250),
            cycle: Duration::from_secs(180),
            connector: None,
            ships: Vec::new(),
            scripted: None,
            pilot_drop: None,
            choose: None,
        }
    }
}

enum Link {
    Peer {
        client: Arc<FleetClient>,
        hub: Arc<FleetHub>,
        /// Joined again with the code once, if a rejoin by key was refused.
        rejoined: bool,
    },
    Hosted(Arc<HostedClient>),
}

pub struct Member {
    /// Peer to peer, the handle it joins as; hosted, the relay's once joined.
    handle: String,
    pub scripted: bool,
    pub seating: Option<Seating>,
    key: PublicKey,
    link: Link,
    core: Arc<ControlCore>,
    /// Off line for the pilot drop.
    dropped: bool,
}

impl Member {
    pub fn public_key(&self) -> PublicKey {
        self.key
    }

    pub fn handle(&self) -> String {
        match &self.link {
            Link::Peer { .. } => self.handle.clone(),
            Link::Hosted(c) => c.handle().unwrap_or_default(),
        }
    }

    /// The hosted side, for a member connecting to a CIC relay.
    pub fn hosted(&self) -> Option<&Arc<HostedClient>> {
        match &self.link {
            Link::Hosted(c) => Some(c),
            Link::Peer { .. } => None,
        }
    }

    pub fn link(&self) -> LinkState {
        match &self.link {
            Link::Peer { hub, .. } => hub.link().state,
            Link::Hosted(c) => c.link(),
        }
    }

    /// Why the member is refused or offline, when it is.
    pub fn problem(&self) -> Option<String> {
        match &self.link {
            Link::Peer { hub, .. } => hub.link().problem,
            Link::Hosted(c) => c.problem(),
        }
    }

    /// This member's own state as its card says it.
    pub fn status(&self) -> Option<Status> {
        match &self.link {
            Link::Peer { hub, .. } => hub
                .model()
                .members
                .into_iter()
                .find(|m| m.me)
                .map(|m| m.status),
            Link::Hosted(c) => c.life().map(|l| Status::of(Some(l), false)),
        }
    }

    /// The refusal code, while refused.
    pub fn refusal(&self) -> Option<RefusalCode> {
        match &self.link {
            Link::Peer { hub, .. } => hub.link().code,
            Link::Hosted(_) => None,
        }
    }

    /// Peer to peer: the cards this member receives now, by member id, with their field ids
    /// (never the values). Its own card left out.
    pub fn received(&self) -> Vec<(String, Vec<&'static str>)> {
        match &self.link {
            Link::Peer { hub, .. } => hub
                .model()
                .members
                .into_iter()
                .filter(|m| !m.me)
                .map(|m| (m.card.member.clone(), m.card.fields.ids().collect()))
                .collect(),
            Link::Hosted(_) => Vec::new(),
        }
    }

    pub fn is_pilot(&self) -> bool {
        self.seating
            .as_ref()
            .is_some_and(|s| s.role == SeatRole::Pilot)
    }

    async fn disconnect(&mut self) {
        match &self.link {
            Link::Peer { client, .. } => client.close().await,
            Link::Hosted(c) => c.stop().await,
        }
        self.dropped = true;
    }

    fn reconnect(&mut self) {
        match &self.link {
            Link::Peer { client, .. } => client.resume(),
            Link::Hosted(c) => c.start(),
        }
        self.dropped = false;
    }

    async fn close(&self) {
        match &self.link {
            Link::Peer { client, .. } => client.close().await,
            Link::Hosted(c) => c.stop().await,
        }
        self.core.close();
    }
}

/// Counts for the log: never handles, keys or the invite.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub members: usize,
    pub online: usize,
    pub connecting: usize,
    pub offline: usize,
    pub refused: usize,
    /// Off line on purpose, for the pilot drop.
    pub dropped: usize,
    pub up: usize,
    pub downed: usize,
    pub dead: usize,
    pub respawned: usize,
}

impl std::fmt::Display for Counts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} members: {} online, {} connecting, {} offline, {} refused, {} dropped; {} up, {} downed, {} dead, {} respawned",
            self.members,
            self.online,
            self.connecting,
            self.offline,
            self.refused,
            self.dropped,
            self.up,
            self.downed,
            self.dead,
            self.respawned
        )
    }
}

pub struct Sim {
    invite: Option<String>,
    members: Vec<Member>,
    pilot_drop: Option<PilotDrop>,
    started: Instant,
}

fn member_dir(data: &Path, n: usize) -> PathBuf {
    data.join(format!("member-{:02}", n + 1))
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
}

impl Sim {
    /// Starts every member. Peer to peer, a member that joined this server before rejoins by
    /// key and a new one joins with the invite's code. Hosted, each member connects with its
    /// own invite's token, or by key once that token was spent here.
    ///
    /// # Panics
    /// Outside a tokio runtime.
    pub fn start(options: Options) -> io::Result<Self> {
        let (code_invite, hosted) = match &options.join {
            Join::Code(text) => {
                let invite: Invite = text
                    .parse()
                    .map_err(|e| invalid(format!("the invite: {e}")))?;
                if invite.code.is_none() {
                    return Err(invalid(
                        "the invite is a connection code (connect=): pass it with --connect or --connect-file",
                    ));
                }
                (Some((text.clone(), invite)), Vec::new())
            }
            Join::Hosted(invites) => (None, invites.clone()),
        };
        let count = if code_invite.is_some() {
            options.count
        } else {
            hosted.len()
        };
        if options.recordings.is_empty() {
            return Err(invalid("no recordings"));
        }
        if !(1..=MAX_MEMBERS).contains(&count) {
            return Err(invalid(format!("the members must be 1 to {MAX_MEMBERS}")));
        }
        let connector = match options.connector {
            Some(c) => c,
            None => Arc::new(TlsConnector::new()?),
        };
        let mut rng = Rng::new(options.seed);
        // About a third of the members follow the script by default, spread over the cycle.
        let scripted = options.scripted.unwrap_or(count.div_ceil(3)).min(count);
        let mut order: Vec<usize> = (0..count).collect();
        for i in (1..order.len()).rev() {
            let j = usize::try_from(rng.next_u64() % (i as u64 + 1)).unwrap_or(0);
            order.swap(i, j);
        }
        let seats = seat_members(&options.ships);
        let cycle_ms = options.cycle.as_secs_f64() * 1000.0;
        let mut members = Vec::with_capacity(count);
        for n in 0..count {
            let dir = member_dir(&options.data_dir, n);
            std::fs::create_dir_all(&dir)?;
            let me = identity::load_or_create(Some(&dir.join(identity::KEY_FILE)))?;
            let key = me.public();
            let recording = Arc::clone(&options.recordings[n % options.recordings.len()]);
            let offset = rng.fraction() * recording.duration_ms();
            let rank = order.iter().position(|&m| m == n).unwrap_or(0);
            let script = (rank < scripted).then(|| Script {
                cycle_ms,
                offset_ms: cycle_ms * rank as f64 / scripted as f64,
            });
            let seating = seats.get(n).cloned();
            let game = SimGame::new(
                recording,
                offset,
                options.frame_every,
                script,
                seating.clone(),
            );
            let core = Arc::new(ControlCore::new(ControlCoreOptions {
                game: Some(Arc::new(game) as Arc<dyn GameApi>),
                ..ControlCoreOptions::default()
            }));
            let consent_file = dir.join(CONSENT_FILE);
            let consent = ConsentStore::new(Some(consent_file.clone()));
            // Set on every start, so a member saved with an older, narrower consent shares
            // everything too.
            consent.set(options.consent.clone())?;
            let handle = handle(&options.handle_prefix, n);
            let link = match &code_invite {
                Some((text, invite)) => {
                    let hub = Arc::new(FleetHub::new(Arc::new(FleetViewStore::new(None)), key));
                    let fleets = FleetsStore::new(Some(dir.join(FLEETS_FILE)));
                    let known = fleets
                        .get()
                        .active_fleet()
                        .is_some_and(|f| f.key == invite.key);
                    let client = FleetClient::new(
                        Arc::clone(&hub),
                        Arc::clone(&core),
                        me,
                        fleets,
                        consent,
                        Arc::clone(&connector),
                    );
                    if known {
                        client.resume();
                    } else {
                        client.join(text, &handle).map_err(invalid)?;
                    }
                    Link::Peer {
                        client,
                        hub,
                        rejoined: !known,
                    }
                }
                None => {
                    let client = HostedClient::new(HostedOptions {
                        label: format!("member-{:02}", n + 1),
                        identity: me,
                        core: Arc::clone(&core),
                        consent,
                        connector: Arc::clone(&connector),
                        invite: hosted[n].clone(),
                        binding_file: dir.join(BINDING_FILE),
                        choose: options.choose.clone(),
                    });
                    client.start();
                    Link::Hosted(client)
                }
            };
            members.push(Member {
                handle,
                scripted: script.is_some(),
                seating,
                key,
                link,
                core,
                dropped: false,
            });
        }
        Ok(Self {
            invite: code_invite.map(|(text, _)| text),
            members,
            pilot_drop: options.pilot_drop.map(|down| PilotDrop {
                cycle: options.cycle,
                down,
            }),
            started: Instant::now(),
        })
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts {
            members: self.members.len(),
            ..Counts::default()
        };
        for m in &self.members {
            if m.dropped {
                c.dropped += 1;
                continue;
            }
            match m.link() {
                LinkState::Online => c.online += 1,
                LinkState::Connecting | LinkState::None => c.connecting += 1,
                LinkState::Offline => c.offline += 1,
                LinkState::Refused => c.refused += 1,
            }
            match m.status() {
                Some(Status::Up) => c.up += 1,
                Some(Status::Downed) => c.downed += 1,
                Some(Status::Dead) => c.dead += 1,
                Some(Status::Respawned) => c.respawned += 1,
                _ => {}
            }
        }
        c
    }

    /// Runs the pilot drop, and a peer-to-peer member whose rejoin by key was refused (the
    /// server no longer knows it, such as a new event behind the same server) joins once more
    /// with the invite's code.
    pub async fn tend(&mut self) {
        let elapsed = self.started.elapsed();
        if let Some(drop) = self.pilot_drop {
            let down = drop.dropped(elapsed);
            for m in self.members.iter_mut().filter(|m| m.is_pilot()) {
                if down && !m.dropped && m.link() != LinkState::Refused {
                    m.disconnect().await;
                } else if !down && m.dropped {
                    m.reconnect();
                }
            }
        }
        let Some(invite) = &self.invite else {
            return;
        };
        for m in &mut self.members {
            let state = m.link();
            if let Link::Peer {
                client, rejoined, ..
            } = &mut m.link
                && !*rejoined
                && state == LinkState::Refused
            {
                *rejoined = true;
                if let Err(e) = client.join(invite, &m.handle) {
                    tracing::warn!("a member could not join again: {e}");
                }
            }
        }
    }

    /// Takes member `n` (from 0) off line without leaving, as a dropped connection.
    pub async fn drop_member(&mut self, n: usize) -> Result<(), String> {
        let m = self.members.get_mut(n).ok_or("no such member")?;
        m.disconnect().await;
        Ok(())
    }

    /// Member `n` connects again: by key peer to peer, by its binding hosted.
    pub fn rejoin_member(&mut self, n: usize) -> Result<(), String> {
        let m = self.members.get_mut(n).ok_or("no such member")?;
        m.reconnect();
        Ok(())
    }

    /// Peer to peer: member `n` joins with `invite` (such as one with an old code).
    pub fn join_member(&mut self, n: usize, invite: &str) -> Result<(), String> {
        let m = self.members.get_mut(n).ok_or("no such member")?;
        let Link::Peer {
            client, rejoined, ..
        } = &mut m.link
        else {
            return Err("only a peer-to-peer member joins with an invite".to_owned());
        };
        *rejoined = true;
        m.dropped = false;
        client.join(invite, &m.handle)
    }

    /// Peer to peer: member `n` leaves the event.
    pub async fn leave_member(&mut self, n: usize) -> Result<(), String> {
        let m = self.members.get_mut(n).ok_or("no such member")?;
        let Link::Peer { client, .. } = &m.link else {
            return Err("only a peer-to-peer member leaves here".to_owned());
        };
        let key = self
            .invite
            .as_deref()
            .and_then(|i| i.parse::<Invite>().ok())
            .map(|i| i.key);
        match key {
            Some(k) => client.leave(&k.to_string()).await,
            None => Err("no invite".to_owned()),
        }
    }

    /// When every member is refused: the code that says the most (banned, then kicked).
    pub fn all_refused(&self) -> Option<Option<RefusalCode>> {
        if self.members.is_empty() || self.members.iter().any(|m| m.link() != LinkState::Refused) {
            return None;
        }
        let codes: Vec<Option<RefusalCode>> = self.members.iter().map(Member::refusal).collect();
        Some(
            [RefusalCode::Banned, RefusalCode::Kicked]
                .into_iter()
                .find(|c| codes.contains(&Some(*c)))
                .or_else(|| codes.iter().flatten().next().copied()),
        )
    }

    /// One JSON line for the received-cards log: per member its state and the cards it
    /// receives, as member ids and field ids. No handles or values.
    pub fn received_line(&self, at_ms: u128) -> String {
        let members: Vec<serde_json::Value> = self
            .members
            .iter()
            .enumerate()
            .map(|(n, m)| {
                let received: serde_json::Map<String, serde_json::Value> = m
                    .received()
                    .into_iter()
                    .map(|(id, fields)| (id, serde_json::json!(fields)))
                    .collect();
                serde_json::json!({
                    "member": format!("member-{:02}", n + 1),
                    "key": m.public_key().to_string(),
                    "link": m.link(),
                    "refusal": m.refusal(),
                    "received": received,
                })
            })
            .collect();
        serde_json::json!({ "atMs": at_ms, "members": members }).to_string()
    }

    /// Disconnects every member without leaving, so each stays in the event and rejoins by key
    /// on the next start.
    pub async fn close(self) {
        for m in &self.members {
            m.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_made_up_and_valid() {
        assert_eq!(handle("Demo", 0), "Demo Vega");
        assert_eq!(handle("Demo", 1), "Demo Rook");
        assert_eq!(handle("Demo", 16), "Demo Vega 2");
        assert_eq!(handle("", 2), "Kestrel");
        for n in 0..MAX_MEMBERS {
            assert!(fleet_proto::card::is_handle(&handle("Demo", n)));
        }
    }

    #[test]
    fn the_default_consent_is_every_field() {
        let c = default_consent();
        assert!(c.allows("ship.hull") && c.allows("char.health"));
        assert!(c.allows("detail.power") && c.allows("ship.crew_role"));
    }

    #[test]
    fn the_rng_is_repeatable() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert_eq!(a.next_u64(), b.next_u64());
        assert!((0.0..1.0).contains(&a.fraction()));
    }
}
