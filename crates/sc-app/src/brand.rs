//! The app's working name and artwork: the Orbit Reticle icon (window, taskbar and tray) and the
//! splash page. The PNGs are the SVGs in `assets/` rasterised with Chromium at these sizes.

use std::io::Cursor;
use std::time::Duration;

/// The name, in capitals as the splash shows it. Kept here only, so the name can change in one place.
pub const APP_NAME: &str = "HARDPOINT";
/// The name as window titles, the tray and dialogs show it.
pub const DISPLAY_NAME: &str = "Hardpoint";

const ICON_SVG: &str = include_str!("../assets/orbit-reticle.svg");
const SPLASH_HTML: &str = include_str!("../assets/splash.html");
const ICON_32: &[u8] = include_bytes!("../assets/icon-32.png");
const ICON_256: &[u8] = include_bytes!("../assets/icon-256.png");
/// The mono variant, white on transparent, for the tray.
const TRAY_32: &[u8] = include_bytes!("../assets/tray-32.png");

/// An icon as tao and tray-icon take it: 8-bit RGBA rows, top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn decode(png_bytes: &[u8]) -> Result<Rgba, String> {
    let mut reader = png::Decoder::new(Cursor::new(png_bytes))
        .read_info()
        .map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("the image is too large")?;
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    if (info.color_type, info.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return Err(format!(
            "expected 8-bit RGBA, got {:?} {:?}",
            info.color_type, info.bit_depth
        ));
    }
    pixels.truncate(info.buffer_size());
    Ok(Rgba {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// The title bar icon (Windows `ICON_SMALL`).
pub fn window_icon() -> Result<Rgba, String> {
    decode(ICON_32)
}

/// The taskbar and Alt+Tab icon (Windows `ICON_BIG`).
pub fn taskbar_icon() -> Result<Rgba, String> {
    decode(ICON_256)
}

pub fn tray_icon() -> Result<Rgba, String> {
    decode(TRAY_32)
}

/// The splash shows at least this long, so it is seen.
pub const SPLASH_MIN: Duration = Duration::from_secs(2);
/// And at most this long, even when the dashboard has not loaded.
pub const SPLASH_MAX: Duration = Duration::from_secs(6);

/// Whether the splash closes now: 2 s after it opened or once the dashboard has loaded, whichever
/// is later, and never after 6 s.
pub fn splash_done(shown_for: Duration, dashboard_ready: bool) -> bool {
    shown_for >= SPLASH_MAX || (shown_for >= SPLASH_MIN && dashboard_ready)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The splash page: self-contained (the icon inline, no external resources), dark.
pub fn splash_html() -> String {
    let svg = ICON_SVG.trim();
    SPLASH_HTML
        .replace("{{ICON}}", svg)
        .replace("{{NAME}}", &escape(APP_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icons_decode_at_their_sizes() {
        for (icon, size) in [
            (window_icon(), 32),
            (taskbar_icon(), 256),
            (tray_icon(), 32),
        ] {
            let icon = icon.unwrap();
            assert_eq!((icon.width, icon.height), (size, size));
            assert_eq!(icon.pixels.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn the_tray_icon_is_the_mono_variant() {
        let icon = tray_icon().unwrap();
        let opaque: Vec<&[u8]> = icon.pixels.chunks(4).filter(|p| p[3] > 200).collect();
        assert!(!opaque.is_empty());
        assert!(opaque.iter().all(|p| p[..3] == [0xff, 0xff, 0xff]));
        // The corners are transparent: no background square.
        assert_eq!(icon.pixels[3], 0);
    }

    #[test]
    fn the_splash_closes_after_2_s_and_the_dashboard_whichever_is_later_by_6_s_at_most() {
        let s = Duration::from_secs;
        assert!(!splash_done(Duration::ZERO, true));
        assert!(!splash_done(s(1), true));
        assert!(!splash_done(s(2), false));
        assert!(splash_done(s(2), true));
        assert!(!splash_done(Duration::from_millis(5999), false));
        assert!(splash_done(s(6), false));
    }

    #[test]
    fn the_splash_page_is_self_contained_and_named_once() {
        let html = splash_html();
        assert!(!html.contains("{{"));
        assert!(html.contains("<svg") && html.contains(APP_NAME));
        // The only URL is the SVG namespace, which is a name, not a fetch.
        assert_eq!(
            html.matches("://").count(),
            html.matches("http://www.w3.org/2000/svg").count()
        );
        assert!(!html.contains("<script") && !html.contains("<link") && !html.contains("src="));
    }
}
