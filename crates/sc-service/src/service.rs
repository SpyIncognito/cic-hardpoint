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
use crate::fleet::demo::{self, Demo};
use crate::fleet::{FleetHub, FleetViewStore};
use crate::pages;
use crate::paths::{self, Env};
use crate::ship::support::SupportFile;
use crate::ship::{self, Ships};
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
    /// DEMO ONLY: mock fleet peers from recordings; no fleet when `None` (the default).
    pub fleet_demo: Option<Demo>,
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
            fleet_demo: None,
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
    /// The fleet panel's cards and the viewer's groups; only with a fleet (today the demo).
    pub fleet: Option<Arc<FleetHub>>,
    /// The current ship, which of its functions the stream reports, and dashboard switching.
    pub ships: Arc<Ships>,
}

/// The running service: core, bus and pages.
pub struct Service {
    core: Arc<ControlCore>,
    app: Arc<App>,
    bus: BusServer,
    close: watch::Sender<bool>,
    http: JoinHandle<()>,
    fleet_demo: Option<JoinHandle<()>>,
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
        let bus = BusServer::start(Arc::clone(&core), opts.bus_file).await?;

        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, opts.port)).await {
            Ok(listener) => listener,
            Err(e) => {
                bus.close().await;
                return Err(e);
            }
        };
        let (close, closing) = watch::channel(false);
        let mut fleet_demo = None;
        let fleet = opts.fleet_demo.map(|d| {
            let view = Arc::new(FleetViewStore::new(file("fleet-view.json")));
            let hub = Arc::new(FleetHub::new(view, true));
            fleet_demo = Some(demo::spawn(Arc::clone(&hub), d, closing.clone()));
            hub
        });
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
            ships,
        });
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
            fleet_demo,
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

    /// The fleet panel's hub, when the service runs a fleet (today only `--fleet-demo`).
    pub fn fleet(&self) -> Option<&Arc<FleetHub>> {
        self.app.fleet.as_ref()
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

    /// Tell the dashboard pages whether the app is recording telemetry.
    pub fn set_recording(&self, on: bool) {
        self.app
            .recording
            .send_if_modified(|was| std::mem::replace(was, on) != on);
    }

    /// Stop the pages and the bus (removing the bus file), then the core.
    pub async fn close(self) {
        let _ = self.close.send(true);
        let _ = self.http.await;
        if let Some(demo) = self.fleet_demo {
            let _ = demo.await;
        }
        let _ = self.ship_watch.await;
        self.bus.close().await;
        self.core.close();
    }
}
