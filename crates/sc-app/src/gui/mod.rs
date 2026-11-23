//! The Windows app: the service on a tokio runtime, the dashboard in a WebView2 window, one
//! borderless no-activate window per detached tab, bezel buttons from gilrs, and the tray icon.
//! All of it through safe APIs; the Win32 calls stay inside tao, wry, tray-icon, gilrs,
//! display-info and rfd.

mod pads;
mod relay;
mod screens;
mod splash;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Fullscreen, Window, WindowBuilder, WindowId};
use tokio::sync::{mpsc, watch};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebView, WebViewBuilder};

use sc_app::bezel::{Engine, Out, Pad, PadButton};
use sc_app::brand::{self, DISPLAY_NAME, Rgba};
use sc_app::bridge::{self, Host, Sender};
use sc_app::client::{self, Outgoing, Shared};
use sc_app::displays::Display;
use sc_app::instance::{self, Launch};
use sc_app::model::{DashState, Tab};
use sc_app::recorder::{self, DEFAULT_CAP, Recorder, StopReason, Summary};
use sc_app::settings::AppSettings;
use sc_service::Service;
use sc_service::paths::{self, Env};
use sc_service::relay::{Mode, Status as RelayStatus};

use crate::Args;
use relay::BarAction;
use splash::Splash;

const TITLE: &str = DISPLAY_NAME;
const LOG_FILE: &str = "hardpoint.log";
const HOLD_TICK: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum UserEvent {
    Ipc {
        from: Sender,
        text: String,
    },
    /// An unpatched page asked for a kiosk window with `window.open`.
    OpenKiosk {
        tab: String,
    },
    Client(client::Event),
    ToPage {
        tab: String,
        event: Value,
    },
    Menu(MenuId),
    Show,
    RecordingStarted(std::io::Result<Recorder>),
    /// The recorder stopped by itself (the cap, a failed write).
    RecordingEnded(Summary),
    /// The dashboard page in the main window has loaded.
    MainReady,
    /// The splash's 2 s and 6 s marks.
    SplashTick,
    /// The relay's status changed.
    Relay(Box<RelayStatus>),
}

enum Recording {
    Off,
    /// Waiting for the game's action list; `stop` when a stop came meanwhile.
    Starting {
        stop: bool,
    },
    On(Recorder),
}

/// To the bezel task.
pub enum EngineMsg {
    Connected(Pad),
    Disconnected(usize),
    Button(PadButton),
    /// The tabs whose windows are open and not mapping.
    Active(Vec<String>),
}

fn init_logging(dir: &std::path::Path) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    let _ = std::fs::create_dir_all(dir);
    match File::create(dir.join(LOG_FILE)) {
        Ok(file) => builder
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .init(),
        Err(_) => builder.init(),
    }
}

fn fail(text: &str) -> ExitCode {
    tracing::error!("{text}");
    rfd::MessageDialog::new()
        .set_title(TITLE)
        .set_description(text)
        .set_level(rfd::MessageLevel::Error)
        .show();
    ExitCode::FAILURE
}

/// The WebView2 profile folder, outside the exe's folder (which may not be writable).
fn webview_dir(settings: &std::path::Path) -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| {
            std::path::PathBuf::from(d)
                .join(DISPLAY_NAME)
                .join("WebView2")
        })
        .unwrap_or_else(|| settings.join("WebView2"))
}

pub fn run(args: Args) -> ExitCode {
    let env = Env::current();
    // Before the log file creates the new folder, which would stop the copy.
    let migration = paths::migrate_default_settings(&env);
    let settings = paths::config_dir(&env);
    init_logging(&settings);
    tracing::info!("{DISPLAY_NAME} {} starting", env!("CARGO_PKG_VERSION"));
    paths::log_migration(&env, &migration);
    let guard = match instance::launch(&settings) {
        Ok(Launch::First(guard)) => guard,
        Ok(Launch::Second) => return ExitCode::SUCCESS,
        Err(e) => return fail(&e.to_string()),
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return fail(&format!("cannot start the runtime: {e}")),
    };
    let options = match crate::service_options(&args) {
        Ok(options) => options,
        Err(e) => return fail(&e),
    };
    // The service starts on the runtime while the splash opens; neither waits for the other.
    let starting = runtime.spawn(Service::start(options));
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let mut context = WebContext::new(Some(webview_dir(&settings)));
    // A game PC that was relaying starts straight into the bar: no splash, no dashboard webview.
    let relaying = sc_service::relay::saved(Some(&settings)).0 == Mode::Relay;
    let splash = if args.splash && !relaying && AppSettings::load(&settings).splash {
        Splash::open(&event_loop, &mut context, &proxy)
            .inspect_err(|e| tracing::warn!("no splash: {e}"))
            .ok()
    } else {
        None
    };
    let service = match runtime.block_on(starting) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            drop(splash);
            return fail(&format!(
                "The control service could not start on port {}: {e}\n\nIs another {DISPLAY_NAME} already running?",
                args.port
            ));
        }
        Err(e) => {
            drop(splash);
            return fail(&format!("The control service could not start: {e}"));
        }
    };
    let port = service.port();
    tracing::info!(
        "control service on 127.0.0.1:{port}, bus port {}",
        service.bus_port()
    );

    {
        let proxy = proxy.clone();
        guard.serve(move || {
            let _ = proxy.send_event(UserEvent::Show);
        });
    }
    {
        let proxy = proxy.clone();
        let mut status = service.relay().subscribe();
        runtime.spawn(async move {
            loop {
                let now = status.borrow_and_update().clone();
                if proxy.send_event(UserEvent::Relay(Box::new(now))).is_err()
                    || status.changed().await.is_err()
                {
                    return;
                }
            }
        });
    }

    let state = Shared::default();
    let (out, outgoing) = mpsc::unbounded_channel::<Outgoing>();
    let (close, closing) = watch::channel(false);
    {
        let proxy = proxy.clone();
        runtime.spawn(client::run(
            port,
            service.token().as_str().to_owned(),
            Arc::clone(&state),
            outgoing,
            move |e| {
                let _ = proxy.send_event(UserEvent::Client(e));
            },
            closing.clone(),
        ));
    }
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    if args.native_bezel {
        runtime.spawn(bezel_task(
            Arc::clone(&state),
            engine_rx,
            out,
            proxy.clone(),
            closing,
        ));
        pads::spawn(engine_tx.clone());
    }

    let (tray, menus) = match build_tray(&proxy) {
        Ok((icon, menus)) => (Some(icon), Some(menus)),
        Err(e) => {
            tracing::warn!("no tray icon: {e}");
            (None, None)
        }
    };

    let mut app = App {
        port,
        settings: settings.clone(),
        runtime: runtime.handle().clone(),
        service: Some(service),
        tray,
        recording: Recording::Off,
        native_bezel: args.native_bezel,
        state,
        proxy,
        context,
        main: None,
        splash,
        kiosks: HashMap::new(),
        mapping: HashSet::new(),
        engine: engine_tx,
        restored: false,
        shown: DashState::default(),
        menus,
        bar: None,
        relay_status: None,
    };
    if let Err(e) = app.open_main(&event_loop, !relaying) {
        app.splash = None;
        return fail(&format!(
            "The dashboard window could not open: {e}\n\nIs the WebView2 runtime installed?"
        ));
    }
    if args.record {
        app.start_recording();
    }

    let mut runtime = Some(runtime);
    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        let quit = match event {
            Event::UserEvent(e) => app.on_user(e, target),
            Event::WindowEvent {
                window_id, event, ..
            } if app.is_bar(window_id) => {
                app.on_bar_event(&event);
                false
            }
            Event::RedrawRequested(window_id) if app.is_bar(window_id) => {
                app.draw_bar();
                false
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                app.on_close(window_id);
                false
            }
            _ => false,
        };
        if quit {
            app.stop_recording(StopReason::Quit);
            app.kiosks.clear();
            app.main = None;
            let _ = close.send(true);
            if let (Some(rt), Some(s)) = (runtime.take(), app.service.take()) {
                rt.block_on(s.close());
                rt.shutdown_timeout(Duration::from_secs(2));
            }
            *flow = ControlFlow::Exit;
        }
    });
}

const RECORD: &str = "Record telemetry";
const STOP_RECORDING: &str = "Stop recording";

#[derive(Clone)]
struct Menus {
    open: MenuId,
    restore: MenuId,
    /// Its text says whether a click starts or stops.
    record: MenuItem,
    recordings: MenuId,
    /// Enabled in relay mode only; the first one's text says whether it stops the relay or connects.
    relay_link: MenuItem,
    relay_settings: MenuItem,
    relay_exit: MenuItem,
    quit: MenuId,
}

fn build_tray(proxy: &EventLoopProxy<UserEvent>) -> Result<(TrayIcon, Menus), String> {
    let open = MenuItem::new("Open dashboard", true, None);
    let restore = MenuItem::new("Restore detached tabs", true, None);
    let record = MenuItem::new(RECORD, true, None);
    let recordings = MenuItem::new("Open recordings folder", true, None);
    let relay_link = MenuItem::new("Relay: stop relay", false, None);
    let relay_settings = MenuItem::new("Relay settings", false, None);
    let relay_exit = MenuItem::new("Exit relay mode", false, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &open,
        &restore,
        &PredefinedMenuItem::separator(),
        &record,
        &recordings,
        &PredefinedMenuItem::separator(),
        &relay_link,
        &relay_settings,
        &relay_exit,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| e.to_string())?;
    let menus = Menus {
        open: open.id().clone(),
        restore: restore.id().clone(),
        record,
        recordings: recordings.id().clone(),
        relay_link,
        relay_settings,
        relay_exit,
        quit: quit.id().clone(),
    };
    let Rgba {
        width,
        height,
        pixels,
    } = brand::tray_icon()?;
    let icon = tray_icon::Icon::from_rgba(pixels, width, height).map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(TITLE)
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(UserEvent::Menu(e.id));
    }));
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if matches!(e, TrayIconEvent::DoubleClick { .. }) {
            let _ = p.send_event(UserEvent::Show);
        }
    }));
    Ok((tray, menus))
}

/// Feeds the bezel engine and times its holds.
async fn bezel_task(
    state: Shared,
    mut rx: mpsc::UnboundedReceiver<EngineMsg>,
    out: mpsc::UnboundedSender<Outgoing>,
    proxy: EventLoopProxy<UserEvent>,
    mut closing: watch::Receiver<bool>,
) {
    let mut engine = Engine::default();
    let mut active: Vec<String> = Vec::new();
    let mut tick = tokio::time::interval(HOLD_TICK);
    let dispatch = |outs: Vec<Out>| {
        for o in outs {
            match o {
                Out::Send { tab, message } => {
                    let _ = out.send((message, Some(tab)));
                }
                Out::Notice { tab, text } => {
                    let event = json!({ "type": "notice", "text": text, "tone": "bad" });
                    let _ = proxy.send_event(UserEvent::ToPage { tab, event });
                }
                Out::HoldStarted { tab, label } => {
                    let event = json!({ "type": "hold", "label": label });
                    let _ = proxy.send_event(UserEvent::ToPage { tab, event });
                }
                Out::HoldEnded { tab } => {
                    let event = json!({ "type": "holdEnd" });
                    let _ = proxy.send_event(UserEvent::ToPage { tab, event });
                }
                Out::Warn(text) => tracing::warn!("{text}"),
            }
        }
    };
    loop {
        tokio::select! {
            _ = closing.wait_for(|c| *c) => return,
            msg = rx.recv() => match msg {
                None => return,
                Some(EngineMsg::Connected(pad)) => engine.connected(pad),
                Some(EngineMsg::Disconnected(id)) => dispatch(engine.disconnected(id)),
                Some(EngineMsg::Button(edge)) => {
                    let outs = engine.button(&client::lock(&state), &active, &edge, Instant::now());
                    dispatch(outs);
                }
                Some(EngineMsg::Active(now_active)) => {
                    for gone in active.iter().filter(|t| !now_active.contains(t)) {
                        dispatch(engine.cancel_tab(gone));
                    }
                    active = now_active;
                }
            },
            _ = tick.tick(), if engine.holding() => dispatch(engine.tick(Instant::now())),
        }
    }
}

/// A window and the webview in it; the webview is dropped first.
struct View {
    webview: WebView,
    window: Window,
}

/// The dashboard window. Its webview exists only while the window is shown, so the browser
/// processes behind it exit while the app sits in the tray.
struct Main {
    webview: Option<WebView>,
    window: Window,
}

struct App {
    port: u16,
    settings: PathBuf,
    runtime: tokio::runtime::Handle,
    /// Taken at quit, to close it.
    service: Option<Service>,
    /// Kept for the life of the app; dropping it removes the icon.
    tray: Option<TrayIcon>,
    recording: Recording,
    native_bezel: bool,
    state: Shared,
    proxy: EventLoopProxy<UserEvent>,
    context: WebContext,
    main: Option<Main>,
    /// Open at launch until it is done; the main window is shown when it closes.
    splash: Option<Splash>,
    kiosks: HashMap<String, View>,
    /// Tabs whose page is mapping bezel buttons; the app leaves their buttons alone meanwhile.
    mapping: HashSet<String>,
    engine: mpsc::UnboundedSender<EngineMsg>,
    /// The placed tabs were opened once, at the first dashboard.
    restored: bool,
    /// The dashboard as last acted on, to follow detached windows when it changes.
    shown: DashState,
    menus: Option<Menus>,
    /// The relay bar, while the game PC is in relay mode.
    bar: Option<relay::Bar>,
    relay_status: Option<RelayStatus>,
}

impl App {
    fn webview(
        &mut self,
        window: &Window,
        url: &str,
        from: Sender,
        focused: bool,
    ) -> wry::Result<WebView> {
        let port = self.port;
        let open_proxy = self.proxy.clone();
        let ipc_proxy = self.proxy.clone();
        let load_proxy = self.proxy.clone();
        let main = from == Sender::Main;
        WebViewBuilder::new_with_web_context(&mut self.context)
            .with_url(url)
            .with_initialization_script(bridge::init_script(self.native_bezel))
            .with_focused(focused)
            .with_navigation_handler(move |url| bridge::is_own_page(&url, port))
            .with_new_window_req_handler(move |url, _features| {
                if let Some((tab, _)) = bridge::kiosk_tab(&url, port) {
                    let _ = open_proxy.send_event(UserEvent::OpenKiosk { tab });
                }
                NewWindowResponse::Deny
            })
            .with_ipc_handler(move |req| {
                let page = req.uri().to_string();
                if !bridge::is_own_page(&page, port) {
                    tracing::warn!("ignored a bridge message from {page}");
                    return;
                }
                let _ = ipc_proxy.send_event(UserEvent::Ipc {
                    from: from.clone(),
                    text: req.into_body(),
                });
            })
            .with_on_page_load_handler(move |event, _url| {
                if main && matches!(event, PageLoadEvent::Finished) {
                    let _ = load_proxy.send_event(UserEvent::MainReady);
                }
            })
            .build(window)
    }

    /// The dashboard window; hidden and without its webview until shown when `shown` is false.
    fn open_main(
        &mut self,
        target: &EventLoopWindowTarget<UserEvent>,
        shown: bool,
    ) -> Result<(), String> {
        let window = WindowBuilder::new()
            .with_title(TITLE)
            .with_inner_size(LogicalSize::new(1280.0, 860.0))
            .with_window_icon(window_icon())
            .with_taskbar_icon(taskbar_icon())
            // Behind the splash: shown when it closes.
            .with_visible(shown && self.splash.is_none())
            .build(target)
            .map_err(|e| e.to_string())?;
        let webview = if shown {
            Some(self.main_webview(&window).map_err(|e| e.to_string())?)
        } else {
            None
        };
        self.main = Some(Main { webview, window });
        Ok(())
    }

    /// In relay mode the dashboard opens at its Relay settings.
    fn main_webview(&mut self, window: &Window) -> wry::Result<WebView> {
        let at = if self.in_relay_mode() { "#relay" } else { "" };
        let url = format!("{}/dash{at}", bridge::origin(self.port));
        self.webview(window, &url, Sender::Main, true)
    }

    fn show_main(&mut self) {
        let Some(mut main) = self.main.take() else {
            return;
        };
        if main.webview.is_none() {
            match self.main_webview(&main.window) {
                Ok(webview) => main.webview = Some(webview),
                Err(e) => tracing::error!("the dashboard could not load again: {e}"),
            }
        }
        main.window.set_visible(true);
        main.window.set_minimized(false);
        main.window.set_focus();
        self.main = Some(main);
    }

    /// Closed to the tray. The app's own service session (bezel actions) lives in `client`,
    /// not in this webview, so it keeps running.
    fn hide_main(&mut self) {
        if let Some(main) = &mut self.main {
            main.window.set_visible(false);
            main.webview = None;
        }
    }

    fn page(&self, from: &Sender) -> Option<&WebView> {
        match from {
            Sender::Main => self.main.as_ref()?.webview.as_ref(),
            Sender::Tab(t) => self.kiosks.get(t).map(|v| &v.webview),
        }
    }

    fn eval(&self, from: &Sender, script: &str) {
        if let Some(webview) = self.page(from)
            && let Err(e) = webview.evaluate_script(script)
        {
            tracing::warn!("cannot reach the page: {e}");
        }
    }

    fn start_recording(&mut self) {
        if !matches!(self.recording, Recording::Off) {
            return;
        }
        let Some(service) = &self.service else {
            return;
        };
        let core = Arc::clone(service.core());
        let folder = recorder::folder(&self.settings);
        let proxy = self.proxy.clone();
        self.recording = Recording::Starting { stop: false };
        self.show_recording();
        self.runtime.spawn(async move {
            let actions = core.actions().await;
            let ended = proxy.clone();
            let started = Recorder::start(
                core.telemetry(),
                &folder,
                &actions,
                DEFAULT_CAP,
                move |summary| {
                    let _ = ended.send_event(UserEvent::RecordingEnded(summary.clone()));
                },
            );
            let _ = proxy.send_event(UserEvent::RecordingStarted(started));
        });
    }

    fn on_recording_started(&mut self, started: std::io::Result<Recorder>) {
        let stop = match self.recording {
            Recording::Starting { stop } => stop,
            _ => return,
        };
        match started {
            Ok(recorder) => {
                tracing::info!("recording telemetry to {}", recorder.path().display());
                self.recording = Recording::On(recorder);
                if stop {
                    self.stop_recording(StopReason::Asked);
                }
            }
            Err(e) => {
                self.recording = Recording::Off;
                tracing::error!("telemetry recording could not start: {e}");
                self.notice(
                    &Sender::Main,
                    &format!("Telemetry recording could not start: {e}"),
                );
            }
        }
        self.show_recording();
    }

    fn stop_recording(&mut self, why: StopReason) {
        match std::mem::replace(&mut self.recording, Recording::Off) {
            Recording::Off => {}
            Recording::Starting { .. } if why == StopReason::Quit => {}
            Recording::Starting { .. } => self.recording = Recording::Starting { stop: true },
            Recording::On(recorder) => self.report(&recorder.stop(why)),
        }
        self.show_recording();
    }

    fn on_recording_ended(&mut self, ended: &Summary) {
        if let Recording::On(recorder) = &self.recording
            && recorder.path() == ended.path
        {
            self.recording = Recording::Off;
            self.report(ended);
            self.show_recording();
        }
    }

    fn report(&self, s: &Summary) {
        let name = s
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = format!(
            "Telemetry recording stopped: {}. {} frames in {name}.",
            s.why.text(),
            s.frames
        );
        if s.why.is_unasked() {
            tracing::warn!("{text}");
            self.notice(&Sender::Main, &text);
        } else {
            tracing::info!("{text}");
        }
    }

    /// The tray item, the tray tooltip and the dashboard's REC mark.
    fn show_recording(&self) {
        let on = !matches!(self.recording, Recording::Off);
        if let Some(menus) = &self.menus {
            menus
                .record
                .set_text(if on { STOP_RECORDING } else { RECORD });
        }
        if let Some(tray) = &self.tray {
            let relaying = self
                .relay_status
                .as_ref()
                .filter(|s| s.mode == Mode::Relay)
                .map(RelayStatus::title);
            let tip = match (relaying, on) {
                (Some(title), true) => format!("{title}, REC"),
                (Some(title), false) => title,
                (None, true) => format!("{TITLE}: REC"),
                (None, false) => TITLE.to_owned(),
            };
            if let Err(e) = tray.set_tooltip(Some(tip)) {
                tracing::warn!("tray tooltip: {e}");
            }
        }
        if let Some(service) = &self.service {
            service.set_recording(on);
        }
    }

    fn open_recordings(&self) {
        let folder = recorder::folder(&self.settings);
        let opened = std::fs::create_dir_all(&folder)
            .and_then(|()| std::process::Command::new("explorer").arg(&folder).spawn());
        if let Err(e) = opened {
            tracing::warn!("cannot open {}: {e}", folder.display());
        }
    }

    fn notice(&self, from: &Sender, text: &str) {
        self.eval(
            from,
            &bridge::event_script(&json!({ "type": "notice", "text": text, "tone": "bad" })),
        );
    }

    fn tell_engine(&self) {
        let active = self
            .kiosks
            .keys()
            .filter(|t| !self.mapping.contains(*t))
            .cloned()
            .collect();
        let _ = self.engine.send(EngineMsg::Active(active));
    }

    fn on_close(&mut self, id: WindowId) {
        if self.main.as_ref().is_some_and(|v| v.window.id() == id) {
            // The app keeps running in the tray; Quit is there.
            self.hide_main();
            return;
        }
        let tab = self
            .kiosks
            .iter()
            .find(|(_, v)| v.window.id() == id)
            .map(|(t, _)| t.clone());
        if let Some(tab) = tab {
            self.close_kiosk(&tab);
        }
    }

    fn close_kiosk(&mut self, tab: &str) {
        self.kiosks.remove(tab);
        self.mapping.remove(tab);
        self.tell_engine();
    }

    /// True to quit.
    fn on_user(&mut self, event: UserEvent, target: &EventLoopWindowTarget<UserEvent>) -> bool {
        match event {
            UserEvent::Ipc { from, text } => {
                let answer = bridge::handle(&mut Ctx { app: self, target }, &from, &text);
                match answer {
                    Ok((id, result)) => self.eval(&from, &bridge::reply_script(id, &result)),
                    Err(e) => tracing::warn!("bridge: {e}"),
                }
            }
            UserEvent::OpenKiosk { tab } => self.open_unpatched(&tab, target),
            UserEvent::Client(client::Event::Dashboard) => self.on_dashboard(target),
            UserEvent::Client(client::Event::Refused { tab, text }) => {
                if let Some(tab) = tab {
                    self.notice(&Sender::Tab(tab), &text);
                }
            }
            UserEvent::ToPage { tab, event } => {
                self.eval(&Sender::Tab(tab), &bridge::event_script(&event));
            }
            UserEvent::Show => self.show_main(),
            UserEvent::MainReady => {
                if let Some(s) = &mut self.splash {
                    s.dashboard_ready();
                }
                self.close_splash_when_done();
            }
            UserEvent::SplashTick => self.close_splash_when_done(),
            UserEvent::Relay(status) => self.on_relay(*status, target),
            UserEvent::RecordingStarted(started) => self.on_recording_started(started),
            UserEvent::RecordingEnded(summary) => self.on_recording_ended(&summary),
            UserEvent::Menu(id) => {
                let Some(menus) = self.menus.clone() else {
                    return false;
                };
                if id == menus.open {
                    self.show_main();
                } else if id == menus.restore {
                    self.restore(target);
                } else if id == *menus.record.id() {
                    if matches!(self.recording, Recording::Off) {
                        self.start_recording();
                    } else {
                        self.stop_recording(StopReason::Asked);
                    }
                } else if id == menus.recordings {
                    self.open_recordings();
                } else if id == *menus.relay_link.id() {
                    self.bar_action(BarAction::Link);
                } else if id == *menus.relay_settings.id() {
                    self.bar_action(BarAction::Settings);
                } else if id == *menus.relay_exit.id() {
                    self.bar_action(BarAction::Exit);
                } else if id == menus.quit {
                    return true;
                }
            }
        }
        false
    }

    fn close_splash_when_done(&mut self) {
        if self.splash.as_ref().is_some_and(Splash::done) {
            self.splash = None;
            self.show_main();
        }
    }

    fn restore(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        // Relay mode renders nothing; the tabs come back when it ends.
        if self.in_relay_mode() {
            return;
        }
        let result = bridge::restore_all(&mut Ctx { app: self, target });
        let left: Vec<String> = result["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["state"] != "open")
            .map(|t| {
                format!(
                    "{}: {}",
                    t["name"].as_str().unwrap_or(""),
                    t["text"].as_str().unwrap_or("")
                )
            })
            .collect();
        if !left.is_empty() {
            tracing::warn!("not restored: {}", left.join("; "));
            self.notice(&Sender::Main, &format!("Not restored: {}", left.join("; ")));
        }
    }

    fn on_dashboard(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        let state = client::lock(&self.state).clone();
        let old = std::mem::replace(&mut self.shown, state.clone());
        // Another dashboard (the ship changed, or a pick): each window goes to the tab of the
        // same name, else keeps a generic one. Otherwise a window whose tab is gone closes.
        let switched = old.active != state.active;
        let open: Vec<String> = self.kiosks.keys().cloned().collect();
        for id in open {
            let next = match old.tab(&id) {
                Some(t) if switched => state.follow(t),
                _ => state.tab(&id),
            };
            match next.map(|t| (t.id.clone(), t.name.clone())) {
                None => self.close_kiosk(&id),
                Some((to, name)) if to != id => self.retarget(&id, &to, &name),
                Some(_) => {}
            }
        }
        if !self.restored {
            self.restored = true;
            self.restore(target);
        }
    }

    /// Show another tab in a detached window, on the same display: a new page on the window,
    /// so the page's messages name the new tab.
    fn retarget(&mut self, from: &str, to: &str, name: &str) {
        let Some(View { webview, window }) = self.kiosks.remove(from) else {
            return;
        };
        drop(webview);
        self.mapping.remove(from);
        let url = bridge::kiosk_url(self.port, to, false);
        match self.webview(&window, &url, Sender::Tab(to.to_owned()), false) {
            Ok(webview) => {
                window.set_title(&format!("{TITLE}: {name}"));
                self.kiosks.insert(to.to_owned(), View { webview, window });
            }
            Err(e) => tracing::warn!("cannot show {to} in the window of {from}: {e}"),
        }
        self.tell_engine();
    }

    /// A kiosk URL from a page without the bridge: on the tab's display, else beside the dashboard.
    fn open_unpatched(&mut self, tab_id: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let state = client::lock(&self.state).clone();
        let Some(tab) = state.tab(tab_id) else {
            return;
        };
        let mut ctx = Ctx { app: self, target };
        if tab.placement.is_some() {
            let msg = json!({ "id": 0, "type": "open", "tab": tab_id }).to_string();
            if let Ok((_, r)) = bridge::handle(&mut ctx, &Sender::Main, &msg)
                && r["ok"] == true
            {
                return;
            }
        }
        let here = ctx
            .app
            .main
            .as_ref()
            .and_then(|v| v.window.current_monitor())
            .and_then(|m| m.name());
        let list = screens::list();
        let Some(display) = list.iter().find(|d| Some(&d.device) == here.as_ref()) else {
            return;
        };
        if let Err(e) = ctx.open_tab(tab, display, false) {
            ctx.app.notice(&Sender::Main, &e);
        }
    }
}

fn tao_icon(icon: Result<Rgba, String>) -> Option<tao::window::Icon> {
    let Rgba {
        width,
        height,
        pixels,
    } = icon.inspect_err(|e| tracing::warn!("icon: {e}")).ok()?;
    tao::window::Icon::from_rgba(pixels, width, height).ok()
}

/// The title bar icon.
fn window_icon() -> Option<tao::window::Icon> {
    tao_icon(brand::window_icon())
}

/// The taskbar and Alt+Tab icon.
fn taskbar_icon() -> Option<tao::window::Icon> {
    tao_icon(brand::taskbar_icon())
}

/// The app plus the event loop, for the bridge.
struct Ctx<'a> {
    app: &'a mut App,
    target: &'a EventLoopWindowTarget<UserEvent>,
}

impl Ctx<'_> {
    fn place(window: &Window, monitor: &tao::monitor::MonitorHandle, fullscreen: bool) {
        if fullscreen {
            window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))));
        } else {
            window.set_fullscreen(None);
            window.set_outer_position(monitor.position());
        }
    }
}

impl Host for Ctx<'_> {
    fn displays(&self) -> Vec<Display> {
        screens::list()
    }

    fn dashboard(&self) -> DashState {
        client::lock(&self.app.state).clone()
    }

    fn open_tab(&mut self, tab: &Tab, display: &Display, fullscreen: bool) -> Result<(), String> {
        let monitor = self
            .target
            .available_monitors()
            .find(|m| m.name().as_deref() == Some(display.device.as_str()))
            .ok_or_else(|| format!("{} is no longer connected", display.label))?;
        if let Some(v) = self.app.kiosks.get(&tab.id) {
            Self::place(&v.window, &monitor, fullscreen);
            return Ok(());
        }
        // No-activate (WS_EX_NOACTIVATE through tao's focusable flag): a touch on the screen never
        // takes the focus from the game.
        let window = WindowBuilder::new()
            .with_title(format!("{TITLE}: {}", tab.name))
            .with_window_icon(window_icon())
            .with_decorations(!fullscreen)
            .with_focused(false)
            .with_focusable(false)
            .with_skip_taskbar(true)
            .with_position(monitor.position())
            .with_inner_size(if fullscreen {
                monitor.size().to_logical::<f64>(monitor.scale_factor())
            } else {
                LogicalSize::new(1024.0, 768.0)
            })
            .with_fullscreen(fullscreen.then(|| Fullscreen::Borderless(Some(monitor.clone()))))
            .build(self.target)
            .map_err(|e| e.to_string())?;
        // The native window is already full screen, so the page is not asked to go full screen.
        let url = bridge::kiosk_url(self.app.port, &tab.id, false);
        let webview = self
            .app
            .webview(&window, &url, Sender::Tab(tab.id.clone()), false)
            .map_err(|e| e.to_string())?;
        self.app
            .kiosks
            .insert(tab.id.clone(), View { webview, window });
        self.app.tell_engine();
        Ok(())
    }

    fn close_window(&mut self, who: &Sender) {
        match who {
            Sender::Main => self.app.hide_main(),
            Sender::Tab(tab) => self.app.close_kiosk(tab),
        }
    }

    fn show_dashboard(&mut self) {
        self.app.show_main();
    }

    fn set_mapping(&mut self, tab: &str, active: bool) {
        if active {
            self.app.mapping.insert(tab.to_owned());
        } else {
            self.app.mapping.remove(tab);
        }
        self.app.tell_engine();
    }
    fn open_browser(&mut self, url: &str) -> Result<(), String> {
        // By full path, so no explorer.exe beside the app or on PATH is run instead.
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::process::Command::new(PathBuf::from(root).join("explorer.exe"))
            .arg(url)
            .spawn()
            .map(drop)
            .map_err(|e| format!("Could not open the browser: {e}"))
    }
}
