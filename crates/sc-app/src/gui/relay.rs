//! Relay mode on the game PC: Hardpoint shrinks to a small native bar with no webview, so the game
//! keeps the GPU and the network. The bar shows the link's status (its title bar too) and three
//! buttons, drawn on the CPU (`sc_app::bar`) and shown with softbuffer: Stop relay (or Connect),
//! Settings and Exit relay mode. It repaints only when what it shows changes or Windows asks. The
//! same actions are in the tray menu and on keys in the bar (D, S, X; the close box exits too).
//! Settings opens the full window at its Relay section, with the webview made on demand; closing
//! it drops the webview again. Exit leaves relay mode and returns to the full window.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use tao::dpi::LogicalSize;
use tao::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use tao::event_loop::EventLoopWindowTarget;
use tao::keyboard::KeyCode;
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{Window, WindowBuilder, WindowId};

use sc_app::bar::{self, Button, Renderer, Tone};
use sc_service::relay::{LinkState, Mode, Relay, Status};

use super::{App, UserEvent, taskbar_icon, window_icon};

/// What the bar's buttons and keys and the tray's relay items do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    /// Disconnect while linked or trying, else connect.
    Link,
    Settings,
    Exit,
}

impl From<Button> for BarAction {
    fn from(b: Button) -> Self {
        match b {
            Button::Link => Self::Link,
            Button::Settings => Self::Settings,
            Button::Exit => Self::Exit,
        }
    }
}

pub fn key_action(event: &KeyEvent) -> Option<BarAction> {
    if event.state != ElementState::Pressed || event.repeat {
        return None;
    }
    match event.physical_key {
        KeyCode::KeyD => Some(BarAction::Link),
        KeyCode::KeyS => Some(BarAction::Settings),
        KeyCode::KeyX => Some(BarAction::Exit),
        _ => None,
    }
}

fn linked_or_trying(status: &Status) -> bool {
    matches!(status.state, LinkState::Up | LinkState::Connecting)
}

fn bar_text(status: &Status) -> String {
    match &status.notice {
        Some(n) => format!("{} ({n})", status.text),
        None => status.text.clone(),
    }
}

fn tone(state: LinkState) -> Tone {
    match state {
        LinkState::Up => Tone::Up,
        LinkState::Connecting | LinkState::Listening => Tone::Trying,
        LinkState::Stopped => Tone::Down,
        LinkState::Off => Tone::Off,
    }
}

/// The bar's window and what it last showed.
pub struct Bar {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    renderer: Renderer,
    state: bar::State,
    layout: Option<bar::Layout>,
    cursor: Option<(f64, f64)>,
}

impl Bar {
    pub fn open(
        target: &EventLoopWindowTarget<UserEvent>,
        status: &Status,
    ) -> Result<Self, String> {
        let window = WindowBuilder::new()
            .with_title(status.title())
            .with_inner_size(LogicalSize::new(bar::WIDTH, bar::HEIGHT))
            .with_resizable(false)
            .with_maximizable(false)
            .with_window_icon(window_icon())
            .with_taskbar_icon(taskbar_icon())
            .build(target)
            .map_err(|e| e.to_string())?;
        let window = Rc::new(window);
        let context = Context::new(Rc::clone(&window)).map_err(|e| e.to_string())?;
        let surface = Surface::new(&context, Rc::clone(&window)).map_err(|e| e.to_string())?;
        Ok(Self {
            window,
            surface,
            renderer: Renderer::new(),
            state: bar::State {
                text: bar_text(status),
                tone: tone(status.state),
                linked: linked_or_trying(status),
                hover: None,
                pressed: None,
            },
            layout: None,
            cursor: None,
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    fn update(&mut self, change: impl FnOnce(&mut bar::State)) {
        let mut next = self.state.clone();
        change(&mut next);
        if next != self.state {
            if next.linked != self.state.linked {
                self.layout = None;
            }
            self.state = next;
            self.window.request_redraw();
        }
    }

    pub fn set_status(&mut self, status: &Status) {
        self.window.set_title(&status.title());
        self.update(|s| {
            s.text = bar_text(status);
            s.tone = tone(status.state);
            s.linked = linked_or_trying(status);
        });
    }

    fn layout(&mut self) -> Option<&bar::Layout> {
        let size = self.window.inner_size();
        let scale = self.window.scale_factor() as f32;
        let stale = self
            .layout
            .as_ref()
            .is_none_or(|l| (l.width, l.height) != (size.width, size.height) || l.scale != scale);
        if stale && size.width > 0 && size.height > 0 {
            self.layout = Some(
                self.renderer
                    .layout(&self.state, size.width, size.height, scale),
            );
        }
        self.layout.as_ref()
    }

    fn hit(&mut self) -> Option<Button> {
        let (x, y) = self.cursor?;
        self.layout()?.hit(x, y)
    }

    pub fn draw(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            return;
        };
        let (Some(w), Some(h)) = (
            NonZeroU32::new(layout.width),
            NonZeroU32::new(layout.height),
        ) else {
            return;
        };
        let drawn = self.surface.resize(w, h).and_then(|()| {
            let mut buffer = self.surface.buffer_mut()?;
            self.renderer.draw(&self.state, &layout, &mut buffer);
            buffer.present()
        });
        if let Err(e) = drawn {
            tracing::warn!("the relay bar could not draw: {e}");
        }
    }

    /// The pointer over the bar; a click is a press and a release on the same button.
    pub fn on_window_event(&mut self, event: &WindowEvent) -> Option<BarAction> {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some((position.x, position.y));
                let hover = self.hit();
                self.update(|s| s.hover = hover);
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.update(|s| {
                    s.hover = None;
                    s.pressed = None;
                });
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let at = self.hit();
                let was = self.state.pressed;
                match state {
                    ElementState::Pressed => self.update(|s| s.pressed = at),
                    _ => {
                        self.update(|s| s.pressed = None);
                        if at.is_some() && at == was {
                            return at.map(BarAction::from);
                        }
                    }
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.layout = None;
                self.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => return key_action(event),
            _ => {}
        }
        None
    }
}

impl App {
    fn relay(&self) -> Option<Arc<Relay>> {
        self.service.as_ref().map(|s| Arc::clone(s.relay()))
    }

    pub(super) fn is_bar(&self, id: WindowId) -> bool {
        self.bar.as_ref().is_some_and(|b| b.id() == id)
    }

    pub(super) fn in_relay_mode(&self) -> bool {
        self.relay_status
            .as_ref()
            .is_some_and(|s| s.mode == Mode::Relay)
    }

    /// A new relay status: into the bar, out of it, or new status on it.
    pub(super) fn on_relay(&mut self, status: Status, target: &EventLoopWindowTarget<UserEvent>) {
        let was = self.in_relay_mode();
        let now = status.mode == Mode::Relay;
        if now && !was {
            // Nothing that renders stays open: the detached tabs close and the dashboard hides.
            let tabs: Vec<String> = self.kiosks.keys().cloned().collect();
            for tab in tabs {
                self.close_kiosk(&tab);
            }
            self.hide_main();
            match Bar::open(target, &status) {
                Ok(bar) => self.bar = Some(bar),
                Err(e) => tracing::error!("the relay bar could not open: {e}"),
            }
        }
        if was && !now {
            self.bar = None;
            self.relay_status = Some(status.clone());
            self.show_main();
            self.restore(target);
        }
        if let Some(bar) = &mut self.bar {
            bar.set_status(&status);
        }
        if let Some(menus) = &self.menus {
            menus.relay_link.set_enabled(now);
            menus.relay_settings.set_enabled(now);
            menus.relay_exit.set_enabled(now);
            menus.relay_link.set_text(if linked_or_trying(&status) {
                "Relay: stop relay"
            } else {
                "Relay: connect"
            });
        }
        self.relay_status = Some(status);
        self.show_recording();
    }

    /// A window event on the bar: pointer, keys, repaints and the close box.
    pub(super) fn on_bar_event(&mut self, event: &WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            self.bar_action(BarAction::Exit);
            return;
        }
        let action = self.bar.as_mut().and_then(|b| b.on_window_event(event));
        if let Some(action) = action {
            self.bar_action(action);
        }
    }

    pub(super) fn draw_bar(&mut self) {
        if let Some(bar) = &mut self.bar {
            bar.draw();
        }
    }

    pub(super) fn bar_action(&mut self, action: BarAction) {
        let Some(relay) = self.relay() else {
            return;
        };
        match action {
            BarAction::Link => {
                let disconnect = self.relay_status.as_ref().is_some_and(linked_or_trying);
                self.runtime.spawn(async move {
                    if disconnect {
                        relay.disconnect().await;
                    } else if let Err(e) = relay.connect().await {
                        tracing::warn!("[relay] {e}");
                    }
                });
            }
            BarAction::Settings => self.show_main(),
            BarAction::Exit => {
                self.runtime.spawn(async move {
                    if let Err(e) = relay.set_mode(Mode::Off, true).await {
                        tracing::warn!("[relay] {e}");
                    }
                });
            }
        }
    }
}
