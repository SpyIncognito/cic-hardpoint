use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use sc_core::engine::EngineOptions;
use sc_core::{ControlCore, ControlCoreOptions};
use sc_game::GameApi;

use crate::bus::BusServer;
use crate::dashboard::{DashboardStore, ThemeStore};
use crate::fleet::bans::{BANS_FILE, BanStore};
use crate::fleet::host::{self, FleetHost};
use crate::fleet::store::{CONSENT_FILE, FLEETS_FILE};
use crate::fleet::{
    Connector, ConsentStore, FleetClient, FleetHub, FleetLayoutStore, FleetViewStore, FleetsStore,
    TlsConnector, identity,
};
use crate::pages;
use crate::paths::{self, Env};
use crate::relay::{self, Mode, Relay, RelayOptions};
use crate::ship::support::SupportFile;
use crate::ship::{self, Ships};
use crate::ship_names::ShipNames;
use crate::time_settings::{self, TimeStore};
use crate::token::Token;

/// The pages' port, as the TS service's.
pub const DEFAULT_PORT: u16 = 47520;

pub struct ServiceOptions {
    /// The pages' port on 127.0.0.1; 0 picks a free one.
    pub port: u16,
    /// Where sequences.json, dashboards.json and themes.json are kept; in memory only when `None`.
    pub settings_dir: Option<PathBuf>,
    pub bus_file: PathBuf,
    /// The real local game by default.
    pub game: Option<Arc<dyn GameApi>>,
    pub engine: EngineOptions,
    /// How fleet connections are opened; TLS by default (tests may pass another).
    pub fleet_connector: Option<Arc<dyn Connector>>,
    /// The relay to or from another machine; its mode and port are also kept in
    /// app-settings.json, which wins for the port. Its `bind` is also where peer-to-peer
    /// hosting listens.
    pub relay: RelayOptions,
}

impl ServiceOptions {
    /// The TS service's files and port.
    pub fn standard() -> Self {
        let env = Env::current();
        Self {
            port: DEFAULT_PORT,
            settings_dir: Some(paths::config_dir(&env)),
            bus_file: paths::default_bus_file(&env),
            game: None,
            engine: EngineOptions::default(),
            fleet_connector: None,
            relay: RelayOptions::default(),
        }
    }
}

/// What the page handlers share.
pub struct App {
    pub core: Arc<ControlCore>,
    pub dashboards: Arc<DashboardStore>,
    /// The user's own dashboard themes.
    pub themes: Arc<ThemeStore>,
    pub token: Token,
    /// The port pages were served from, for the Origin check.
    pub port: u16,
    /// Turns true when the service stops; open sessions end.
    pub closing: watch::Receiver<bool>,
    /// True while the desktop app records telemetry; the dashboard shows REC.
    pub recording: watch::Sender<bool>,
    /// The fleet panel's cards, structure and the viewer's groups.
    pub fleet: Arc<FleetHub>,
    /// The connection to a fleet server.
    pub fleet_client: Arc<FleetClient>,
    /// Peer-to-peer hosting.
    pub fleet_host: Arc<FleetHost>,
    /// The Fleet tab's layouts: one per CIC event, one for peer-to-peer sessions.
    pub fleet_layouts: Arc<FleetLayoutStore>,
    /// The current ship, which of its functions the stream reports, and dashboard switching.
    pub ships: Arc<Ships>,
    /// How the dashboard shows times.
    pub time: Arc<TimeStore>,
    /// The names the user gave ships.
    pub ship_names: Arc<ShipNames>,
    /// Relay mode: this machine relays to a companion, or is one.
    pub relay: Arc<Relay>,
}

/// The running service: core, bus and pages.
pub struct Service {
    core: Arc<ControlCore>,
    app: Arc<App>,
    bus: BusServer,
    close: watch::Sender<bool>,
    http: JoinHandle<()>,
    ship_watch: JoinHandle<()>,
}

impl Service {
    pub async fn start(opts: ServiceOptions) -> io::Result<Self> {
        let file = |name| opts.settings_dir.as_ref().map(|d| d.join(name));
        let core = Arc::new(ControlCore::new(ControlCoreOptions {
            game: opts.game,
            sequences_file: file("sequences.json"),
            engine: opts.engine,
            ..ControlCoreOptions::default()
        }));
        let dashboards = Arc::new(DashboardStore::new(file("dashboards.json")));
        let themes = Arc::new(ThemeStore::new(file("themes.json")));
        let key_file = file(identity::KEY_FILE);
        let me = identity::load_or_create(key_file.as_deref()).or_else(|e| {
            tracing::error!(
                "[fleet] the fleet key cannot be read ({e}); using one for this run only"
            );
            identity::load_or_create(None)
        })?;
        let fleet = Arc::new(FleetHub::new(
            Arc::new(FleetViewStore::new(file("fleet-view.json"))),
            me.public(),
        ));
        let fleet_host = FleetHost::new(
            &me,
            BanStore::new(file(BANS_FILE)),
            Arc::clone(&fleet),
            opts.relay.bind,
            host::Limits::default(),
            file(crate::app_settings::FILE),
        );
        let connector = match opts.fleet_connector {
            Some(c) => c,
            None => Arc::new(TlsConnector::new()?),
        };
        let fleet_client = FleetClient::new(
            Arc::clone(&fleet),
            Arc::clone(&core),
            me,
            FleetsStore::new(file(FLEETS_FILE)),
            ConsentStore::new(file(CONSENT_FILE)),
            connector,
        );
        // A session hosted before a restart is over; never rejoin it.
        fleet_client.forget(&fleet_host.key());
        let (mode, port) = relay::saved(opts.settings_dir.as_deref());
        let relay_options = RelayOptions {
            port: port.unwrap_or(opts.relay.port),
            ..opts.relay
        };
        let relay = Arc::new(Relay::new(relay_options, opts.settings_dir.as_deref())?);
        let bus = BusServer::start_with(Arc::clone(&core), opts.bus_file, Some(Arc::clone(&fleet)))
            .await?;

        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, opts.port)).await {
            Ok(listener) => listener,
            Err(e) => {
                bus.close().await;
                return Err(e);
            }
        };
        let (close, closing) = watch::channel(false);
        let ships = Arc::new(Ships::new(SupportFile::new(file("ship-support.json"))));
        let ship_watch = tokio::spawn(ship::watch(
            Arc::clone(&core),
            Arc::clone(&ships),
            Arc::clone(&dashboards),
            closing.clone(),
        ));
        let app = Arc::new(App {
            core: Arc::clone(&core),
            dashboards,
            themes,
            token: Token::generate(),
            port: listener.local_addr()?.port(),
            closing: closing.clone(),
            recording: watch::channel(false).0,
            fleet,
            fleet_client,
            fleet_host,
            fleet_layouts: Arc::new(FleetLayoutStore::new(file(crate::fleet::layout::FILE))),
            ships,
            time: Arc::new(TimeStore::new(file(time_settings::FILE))),
            ship_names: Arc::new(ShipNames::new(
                file(time_settings::FILE),
                Arc::clone(core.state()),
            )),
            relay,
        });
        app.relay.attach(&app);
        // A companion shows the game PC's fleet; it keeps no fleet link of its own.
        if mode != Mode::Companion {
            app.fleet_client.resume();
        }
        app.relay.resume(mode).await;
        let router = pages::router(Arc::clone(&app));
        let mut stop = closing;
        let http = tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = stop.wait_for(|closed| *closed).await;
            });
            if let Err(e) = serve.await {
                tracing::error!("page server stopped: {e}");
            }
        });
        Ok(Self {
            core,
            app,
            bus,
            close,
            http,
            ship_watch,
        })
    }

    pub fn core(&self) -> &Arc<ControlCore> {
        &self.core
    }

    pub fn port(&self) -> u16 {
        self.app.port
    }

    pub fn bus_port(&self) -> u16 {
        self.bus.port
    }

    /// The fleet panel's hub.
    pub fn fleet(&self) -> &Arc<FleetHub> {
        &self.app.fleet
    }

    pub fn fleet_client(&self) -> &Arc<FleetClient> {
        &self.app.fleet_client
    }

    pub fn fleet_host(&self) -> &Arc<FleetHost> {
        &self.app.fleet_host
    }

    /// The token put into the pages.
    pub fn token(&self) -> &Token {
        &self.app.token
    }

    /// The current ship and its greyed functions.
    pub fn ships(&self) -> &Arc<Ships> {
        &self.app.ships
    }

    pub fn dashboards(&self) -> &Arc<DashboardStore> {
        &self.app.dashboards
    }

    /// Relay mode, both sides.
    pub fn relay(&self) -> &Arc<Relay> {
        &self.app.relay
    }

    /// Tell the dashboard pages whether the app is recording telemetry.
    pub fn set_recording(&self, on: bool) {
        self.app
            .recording
            .send_if_modified(|was| std::mem::replace(was, on) != on);
    }

    /// Stop the pages and the bus (removing the bus file), then the core.
    pub async fn close(self) {
        let _ = self.close.send(true);
        self.app.relay.close().await;
        let _ = self.http.await;
        host::stop_hosting(&self.app.fleet_host, &self.app.fleet_client).await;
        self.app.fleet_client.close().await;
        let _ = self.ship_watch.await;
        self.bus.close().await;
        self.core.close();
    }
}
