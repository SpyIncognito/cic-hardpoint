//! The launch splash: a borderless dark window, centred on the main display, with the Orbit
//! Reticle icon and the working name. It opens while the service, the dashboard window and the
//! tray start, and never waits for them; the app closes it (see [`brand::splash_done`]).

use std::time::Instant;

use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Window, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

use sc_app::brand::{self, APP_NAME, SPLASH_MAX, SPLASH_MIN};

use super::UserEvent;

const WIDTH: f64 = 480.0;
const HEIGHT: f64 = 300.0;
/// The page's own background, so the window is never white before the page draws.
const BACKGROUND: (u8, u8, u8, u8) = (0x0b, 0x14, 0x26, 0xff);

pub struct Splash {
    // Dropped in this order: the page, then its window.
    _webview: WebView,
    _window: Window,
    opened: Instant,
    dashboard_ready: bool,
}

impl Splash {
    pub fn open(
        target: &EventLoopWindowTarget<UserEvent>,
        context: &mut WebContext,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let mut builder = WindowBuilder::new()
            .with_title(APP_NAME)
            .with_window_icon(super::window_icon())
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top(true)
            .with_skip_taskbar(true)
            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT));
        if let Some(m) = target.primary_monitor() {
            let size = LogicalSize::new(WIDTH, HEIGHT).to_physical::<i32>(m.scale_factor());
            let (pos, area) = (m.position(), m.size());
            builder = builder.with_position(PhysicalPosition::new(
                pos.x + (i32::try_from(area.width).unwrap_or(0) - size.width) / 2,
                pos.y + (i32::try_from(area.height).unwrap_or(0) - size.height) / 2,
            ));
        }
        let window = builder.build(target).map_err(|e| e.to_string())?;
        let webview = WebViewBuilder::new_with_web_context(context)
            .with_html(brand::splash_html())
            .with_background_color(BACKGROUND)
            .with_focused(false)
            .build(&window)
            .map_err(|e| e.to_string())?;
        let opened = Instant::now();
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SPLASH_MIN);
            let _ = proxy.send_event(UserEvent::SplashTick);
            std::thread::sleep(SPLASH_MAX - SPLASH_MIN);
            let _ = proxy.send_event(UserEvent::SplashTick);
        });
        Ok(Self {
            _webview: webview,
            _window: window,
            opened,
            dashboard_ready: false,
        })
    }

    pub fn dashboard_ready(&mut self) {
        self.dashboard_ready = true;
    }

    pub fn done(&self) -> bool {
        brand::splash_done(self.opened.elapsed(), self.dashboard_ready)
    }
}
