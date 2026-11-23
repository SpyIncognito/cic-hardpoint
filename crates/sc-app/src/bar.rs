//! The relay bar's pixels: the link's status and three buttons (Stop relay or Connect, Settings,
//! Exit relay mode), drawn on the CPU into a 0RGB buffer that softbuffer shows on Windows. Platform
//! free, so the layout, the hit-testing and the picture are tested anywhere. The text is Barlow
//! Semi Condensed 600, the dashboard's font, rasterised with ab_glyph.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

const FONT: &[u8] = include_bytes!("../assets/font/barlow-semi-condensed-latin-600.ttf");

/// The client area in logical pixels; the OS title bar above it also carries the status.
pub const WIDTH: f64 = 640.0;
pub const HEIGHT: f64 = 44.0;

const PAD: f32 = 8.0;
const BUTTON_H: f32 = 28.0;
const BUTTON_PAD: f32 = 12.0;
const BUTTON_GAP: f32 = 6.0;
const TEXT_PX: f32 = 15.0;
const DOT: f32 = 5.0;

/// The RSI theme's colours, as the dashboard has them.
mod color {
    pub const BG: u32 = 0x0c1318;
    pub const PANEL: u32 = 0x18232b;
    pub const PANEL_2: u32 = 0x202d36;
    pub const LINE: u32 = 0x364955;
    pub const FG: u32 = 0xeef3f5;
    pub const ACCENT: u32 = 0xf5c518;
    pub const SELECT_FG: u32 = 0x170b02;
    pub const OK: u32 = 0x55d494;
    pub const WARN: u32 = 0xffb020;
    pub const BAD: u32 = 0xff4d40;
    pub const OFF: u32 = 0x6b7c86;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// Stop relay while linked or trying, else Connect.
    Link,
    Settings,
    Exit,
}

/// The dot before the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Up,
    Trying,
    Down,
    Off,
}

/// Everything the bar shows; a redraw happens only when this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub text: String,
    pub tone: Tone,
    /// Linked or trying: the first button stops the relay.
    pub linked: bool,
    pub hover: Option<Button>,
    pub pressed: Option<Button>,
}

impl State {
    pub fn label(&self, button: Button) -> &'static str {
        match button {
            Button::Link if self.linked => "Stop relay",
            Button::Link => "Connect",
            Button::Settings => "Settings",
            Button::Exit => "Exit relay mode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Where things go, in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub status: Rect,
    /// Left to right: Link, Settings, Exit.
    pub buttons: Vec<(Button, Rect)>,
}

impl Layout {
    pub fn hit(&self, x: f64, y: f64) -> Option<Button> {
        let (x, y) = (x as f32, y as f32);
        self.buttons
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(b, _)| *b)
    }
}

pub struct Renderer {
    font: FontRef<'static>,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            font: FontRef::try_from_slice(FONT).expect("the embedded font parses"),
        }
    }

    fn text_width(&self, text: &str, px: f32) -> f32 {
        let font = self.font.as_scaled(PxScale::from(px));
        let mut width = 0.0;
        let mut last = None;
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = last {
                width += font.kern(prev, id);
            }
            width += font.h_advance(id);
            last = Some(id);
        }
        width
    }

    /// The bar at `width` x `height` physical pixels and `scale` physical pixels per logical one.
    pub fn layout(&self, state: &State, width: u32, height: u32, scale: f32) -> Layout {
        let px = TEXT_PX * scale;
        let h = BUTTON_H * scale;
        let y = ((height as f32 - h) / 2.0).round();
        let mut right = width as f32 - PAD * scale;
        let mut buttons = vec![];
        for b in [Button::Exit, Button::Settings, Button::Link] {
            let w = (self.text_width(state.label(b), px) + 2.0 * BUTTON_PAD * scale).ceil();
            let x = (right - w).round();
            buttons.push((b, Rect { x, y, w, h }));
            right = x - BUTTON_GAP * scale;
        }
        buttons.reverse();
        let left = PAD * scale;
        let status = Rect {
            x: left,
            y,
            w: (right - BUTTON_GAP * scale - left).max(0.0),
            h,
        };
        Layout {
            width,
            height,
            scale,
            status,
            buttons,
        }
    }

    /// Draws the whole bar into `buf`, one 0RGB `u32` per pixel, rows top to bottom.
    pub fn draw(&self, state: &State, layout: &Layout, buf: &mut [u32]) {
        let mut canvas = Canvas {
            buf,
            width: layout.width as usize,
            height: layout.height as usize,
        };
        canvas.fill(
            Rect {
                x: 0.0,
                y: 0.0,
                w: layout.width as f32,
                h: layout.height as f32,
            },
            color::BG,
        );
        let s = layout.scale;
        let px = TEXT_PX * s;
        let st = layout.status;
        let dot = match state.tone {
            Tone::Up => color::OK,
            Tone::Trying => color::WARN,
            Tone::Down => color::BAD,
            Tone::Off => color::OFF,
        };
        let r = DOT * s;
        canvas.disc(st.x + r, st.y + st.h / 2.0, r, dot);
        let text_x = st.x + 2.0 * r + 8.0 * s;
        let room = st.x + st.w - text_x;
        let text = self.fit(&state.text, px, room);
        self.text(&mut canvas, &text, text_x, st.y + st.h / 2.0, px, color::FG);
        for (b, rect) in &layout.buttons {
            let (fill, border, fg) = if state.pressed == Some(*b) {
                (color::ACCENT, color::ACCENT, color::SELECT_FG)
            } else if state.hover == Some(*b) {
                (color::PANEL_2, color::ACCENT, color::FG)
            } else {
                (color::PANEL, color::LINE, color::FG)
            };
            canvas.fill(*rect, border);
            let w = s.round().max(1.0);
            canvas.fill(
                Rect {
                    x: rect.x + w,
                    y: rect.y + w,
                    w: rect.w - 2.0 * w,
                    h: rect.h - 2.0 * w,
                },
                fill,
            );
            let label = state.label(*b);
            let tw = self.text_width(label, px);
            let tx = (rect.x + (rect.w - tw) / 2.0).round();
            self.text(&mut canvas, label, tx, rect.y + rect.h / 2.0, px, fg);
        }
    }

    /// `text`, cut with an ellipsis to fit in `room`.
    fn fit(&self, text: &str, px: f32, room: f32) -> String {
        if self.text_width(text, px) <= room {
            return text.to_owned();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let cut: String = chars.iter().collect::<String>().trim_end().to_owned() + "...";
            if self.text_width(&cut, px) <= room {
                return cut;
            }
        }
        String::new()
    }

    /// One line, its x-height centred on `mid_y`.
    fn text(&self, canvas: &mut Canvas, text: &str, x: f32, mid_y: f32, px: f32, rgb: u32) {
        let font = self.font.as_scaled(PxScale::from(px));
        let baseline = (mid_y + (font.ascent() + font.descent()) / 2.0).round();
        let mut pen = x;
        let mut last = None;
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = last {
                pen += font.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(px, point(pen, baseline));
            pen += font.h_advance(id);
            last = Some(id);
            let Some(outline) = self.font.outline_glyph(glyph) else {
                continue;
            };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cover| {
                canvas.blend(
                    b.min.x as i64 + gx as i64,
                    b.min.y as i64 + gy as i64,
                    rgb,
                    cover,
                );
            });
        }
    }
}

struct Canvas<'a> {
    buf: &'a mut [u32],
    width: usize,
    height: usize,
}

impl Canvas<'_> {
    fn fill(&mut self, r: Rect, rgb: u32) {
        let x0 = r.x.max(0.0) as usize;
        let y0 = r.y.max(0.0) as usize;
        let x1 = ((r.x + r.w).max(0.0) as usize).min(self.width);
        let y1 = ((r.y + r.h).max(0.0) as usize).min(self.height);
        for y in y0..y1 {
            self.buf[y * self.width + x0.min(x1)..y * self.width + x1].fill(rgb);
        }
    }

    fn blend(&mut self, x: i64, y: i64, rgb: u32, cover: f32) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let at = &mut self.buf[y * self.width + x];
        *at = mix(*at, rgb, cover.clamp(0.0, 1.0));
    }

    /// A filled circle, its edge smoothed over one pixel.
    fn disc(&mut self, cx: f32, cy: f32, r: f32, rgb: u32) {
        let (x0, x1) = ((cx - r - 1.0).floor() as i64, (cx + r + 1.0).ceil() as i64);
        let (y0, y1) = ((cy - r - 1.0).floor() as i64, (cy + r + 1.0).ceil() as i64);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                self.blend(x, y, rgb, r + 0.5 - d);
            }
        }
    }
}

fn mix(under: u32, over: u32, a: f32) -> u32 {
    let ch = |v: u32, shift: u32| ((v >> shift) & 0xff) as f32;
    let one = |shift: u32| {
        let c = ch(under, shift) + (ch(over, shift) - ch(under, shift)) * a;
        (c.round() as u32).min(255) << shift
    };
    one(16) | one(8) | one(0)
}

/// 0RGB pixels as 8-bit RGBA bytes, for a PNG.
pub fn to_rgba(buf: &[u32]) -> Vec<u8> {
    buf.iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(linked: bool) -> State {
        State {
            text: "relaying to laptop".to_owned(),
            tone: Tone::Up,
            linked,
            hover: None,
            pressed: None,
        }
    }

    fn size(scale: f32) -> (u32, u32) {
        (
            (WIDTH as f32 * scale).round() as u32,
            (HEIGHT as f32 * scale).round() as u32,
        )
    }

    #[test]
    fn buttons_sit_right_in_order_inside_the_bar_without_overlap() {
        let r = Renderer::new();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let (w, h) = size(scale);
            let l = r.layout(&state(true), w, h, scale);
            let order: Vec<Button> = l.buttons.iter().map(|(b, _)| *b).collect();
            assert_eq!(order, [Button::Link, Button::Settings, Button::Exit]);
            let mut x = l.status.x + l.status.w;
            for (_, b) in &l.buttons {
                assert!(b.x > x, "{scale}: a gap before each button");
                assert!(b.y >= 0.0 && b.y + b.h <= h as f32);
                x = b.x + b.w;
            }
            assert!(
                x <= w as f32 - PAD * scale + 0.5,
                "{scale}: inside the right edge"
            );
            assert!(l.status.w > 200.0 * scale, "{scale}: room for the status");
        }
    }

    #[test]
    fn hit_testing_finds_each_button_and_nothing_between() {
        let r = Renderer::new();
        let (w, h) = size(1.5);
        let l = r.layout(&state(true), w, h, 1.5);
        for (b, rect) in &l.buttons {
            let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            assert_eq!(l.hit(f64::from(cx), f64::from(cy)), Some(*b));
            assert_eq!(l.hit(f64::from(rect.x), f64::from(rect.y)), Some(*b));
            assert_eq!(l.hit(f64::from(rect.x + rect.w), f64::from(cy)), None);
            assert_eq!(l.hit(f64::from(cx), f64::from(rect.y - 1.0)), None);
        }
        let settings = l.buttons[1].1;
        assert_eq!(
            l.hit(f64::from(settings.x - 2.0), f64::from(settings.y + 4.0)),
            None
        );
        assert_eq!(
            l.hit(20.0, f64::from(h) / 2.0),
            None,
            "the status is not a button"
        );
        assert_eq!(l.hit(-1.0, -1.0), None);
    }

    #[test]
    fn the_first_button_stops_or_connects_and_is_sized_to_its_label() {
        let r = Renderer::new();
        let (w, h) = size(1.0);
        let on = r.layout(&state(true), w, h, 1.0);
        let off = r.layout(&state(false), w, h, 1.0);
        assert_eq!(state(true).label(Button::Link), "Stop relay");
        assert_eq!(state(false).label(Button::Link), "Connect");
        assert!(on.buttons[0].1.w > off.buttons[0].1.w);
        assert_eq!(on.buttons[2], off.buttons[2], "Exit does not move");
    }

    #[test]
    fn a_long_status_is_cut_to_fit_and_hover_and_press_change_the_picture() {
        let r = Renderer::new();
        let px = TEXT_PX;
        let long =
            "connecting to a-companion-with-a-very-long-machine-name (retry in 30 s) and more";
        let cut = r.fit(long, px, 200.0);
        assert!(
            cut.ends_with("...") && r.text_width(&cut, px) <= 200.0,
            "{cut}"
        );
        assert_eq!(r.fit("short", px, 200.0), "short");

        let (w, h) = size(1.0);
        let mut s = state(true);
        let l = r.layout(&s, w, h, 1.0);
        let draw = |s: &State| {
            let mut buf = vec![0; (w * h) as usize];
            r.draw(s, &l, &mut buf);
            buf
        };
        let plain = draw(&s);
        assert!(plain.contains(&color::OK), "the status dot");
        assert!(plain.contains(&color::FG), "text is drawn");
        s.hover = Some(Button::Settings);
        let hover = draw(&s);
        assert_ne!(plain, hover);
        s.pressed = Some(Button::Settings);
        let pressed = draw(&s);
        assert!(pressed.contains(&color::ACCENT));
        assert_ne!(hover, pressed);
    }

    /// Writes the bar as PNGs for a visual check: `BAR_PNG_DIR=<dir> cargo test -p sc-app bar_png`.
    #[test]
    fn bar_png() {
        let Some(dir) = std::env::var_os("BAR_PNG_DIR") else {
            return;
        };
        let r = Renderer::new();
        let shots = [
            (
                "relay-bar-linked-hover",
                1.0,
                true,
                Tone::Up,
                "relaying to laptop",
                Some(Button::Link),
                None,
            ),
            (
                "relay-bar-connecting-150",
                1.5,
                true,
                Tone::Trying,
                "connecting to laptop (retry in 4 s)",
                None,
                Some(Button::Settings),
            ),
            (
                "relay-bar-stopped-200",
                2.0,
                false,
                Tone::Down,
                "not connected",
                Some(Button::Exit),
                None,
            ),
        ];
        for (name, scale, linked, tone, text, hover, pressed) in shots {
            let s = State {
                text: text.to_owned(),
                tone,
                linked,
                hover,
                pressed,
            };
            let (w, h) = size(scale);
            let l = r.layout(&s, w, h, scale);
            let mut buf = vec![0; (w * h) as usize];
            r.draw(&s, &l, &mut buf);
            let file =
                std::fs::File::create(std::path::Path::new(&dir).join(format!("{name}.png")))
                    .unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&to_rgba(&buf))
                .unwrap();
        }
    }
}
