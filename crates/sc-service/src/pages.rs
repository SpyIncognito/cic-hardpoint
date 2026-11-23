//! The HTTP side on 127.0.0.1: the portal and dashboard pages with the per-launch token put in,
//! the vendored grid library, and the portal WebSocket for pages this server served (checked by
//! Origin here and by the token in the hello).
//!
//! The pages are pages/ at the repository root, embedded at build time. The MFD page (`/mfd`) and
//! its `/mip` socket are not served.
//!
//! The dashboard page's built-in theme colours are put in here from [`built_in_css`], in place of
//! the block between the theme markers (the RSI colours).

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection};
use axum::extract::{State, ws::WebSocket};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};
use percent_encoding::percent_decode_str;

use crate::dashboard::built_in_css;
use crate::portal;
use crate::relay::{Mode, companion};
use crate::service::App;

static PUBLIC: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../pages");

const THEMES_START: &str = "/*__THEMES__*/";
const THEMES_END: &str = "/*__END_THEMES__*/";

/// The page with the built-in themes' CSS in place of the marked block; unchanged without one.
fn with_themes(html: &str) -> String {
    let start = html.find(THEMES_START);
    let end = html.find(THEMES_END).map(|i| i + THEMES_END.len());
    match (start, end) {
        (Some(start), Some(end)) if start < end => {
            format!("{}{}{}", &html[..start], built_in_css(), &html[end..])
        }
        _ => html.to_owned(),
    }
}

/// Room for an imported dashboard file (the session caps the file itself).
pub const MAX_PAGE_MESSAGE_BYTES: usize = 1 << 20;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(portal_page))
        .route("/portal", get(portal_route))
        .route("/dash", get(dash_page))
        .route("/vendor/{*path}", get(vendor))
        .route("/unit-grid.js", get(unit_grid))
        .fallback(not_found)
        .with_state(app)
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn page(name: &str, app: &App, headers: &HeaderMap) -> Response {
    // A WebSocket is only taken at /portal.
    if headers.contains_key(header::UPGRADE) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(html) = PUBLIC.get_file(name).and_then(|f| f.contents_utf8()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let html = with_themes(html).replacen("__TOKEN__", app.token.as_str(), 1);
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::CONTENT_SECURITY_POLICY, "frame-ancestors 'none'"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ];
    (headers, html).into_response()
}

async fn portal_page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    page("portal.html", &app, &headers)
}

async fn dash_page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    page("dash.html", &app, &headers)
}

async fn portal_route(
    State(app): State<Arc<App>>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !headers.contains_key(header::UPGRADE) {
        return page("portal.html", &app, &headers);
    }
    let allowed = [
        format!("http://127.0.0.1:{}", app.port),
        format!("http://localhost:{}", app.port),
    ];
    let origin_ok = headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .is_some_and(|o| allowed.iter().any(|a| a == o));
    match upgrade {
        Ok(ws) if origin_ok && uri.query().is_none() => ws
            .max_message_size(MAX_PAGE_MESSAGE_BYTES)
            .max_frame_size(MAX_PAGE_MESSAGE_BYTES)
            .on_upgrade(move |socket: WebSocket| async move {
                // On a companion every page is the game PC's, over the relay link.
                if app.relay.mode() == Mode::Companion {
                    companion::bridge(Arc::clone(&app.relay), socket, app.token.clone()).await;
                } else {
                    portal::session::serve(socket, app).await;
                }
            }),
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// The Fleet tab's layout rules, shared with the CIC Live board (copied from cic-panel).
async fn unit_grid() -> Response {
    let Some(file) = PUBLIC.get_file("unit-grid.js") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let headers = [
        (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
        (header::CACHE_CONTROL, "no-cache"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ];
    (headers, file.contents()).into_response()
}

/// A file under public/vendor, or 404 for anything else (including `..` and encoded escapes).
async fn vendor(uri: Uri) -> Response {
    let Some(rel) = uri.path().strip_prefix("/vendor/") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(rel) = percent_decode_str(rel).decode_utf8() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let safe = rel.split('/').all(|seg| {
        !seg.is_empty() && seg != "." && seg != ".." && !seg.contains(['\\', ':', '\0'])
    });
    let content_type = match Path::new(rel.as_ref()).extension().and_then(|e| e.to_str()) {
        None | Some("txt") => "text/plain; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(file) = PUBLIC.get_file(format!("vendor/{rel}")).filter(|_| safe) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let headers = [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
    ];
    (headers, file.contents()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::{BUILT_IN_THEMES, theme_css};

    fn dash_html() -> &'static str {
        PUBLIC
            .get_file("dash.html")
            .and_then(|f| f.contents_utf8())
            .expect("dash.html is embedded")
    }

    #[test]
    fn the_page_fallback_is_the_rsi_theme_and_the_service_puts_in_every_built_in() {
        let html = dash_html();
        let start = html.find(THEMES_START).expect("start marker") + THEMES_START.len();
        let end = html.find(THEMES_END).expect("end marker");
        assert_eq!(html[start..end].trim(), theme_css(&BUILT_IN_THEMES[0]));
        let served = with_themes(html);
        assert!(!served.contains(THEMES_START) && !served.contains(THEMES_END));
        for t in BUILT_IN_THEMES.iter() {
            assert_eq!(served.matches(&theme_css(t)).count(), 1, "{}", t.id);
        }
    }

    #[test]
    fn widget_css_names_no_colour_of_its_own() {
        // Every colour comes from the theme tokens: none is written in the page's CSS or script
        // outside the marked fallback block.
        let html = dash_html();
        let start = html.find(THEMES_START).expect("start marker");
        let end = html.find(THEMES_END).expect("end marker");
        let rest = format!("{}{}", &html[..start], &html[end..]);
        let hex = |s: &str| {
            s.char_indices().any(|(i, c)| {
                let after = &s[i + c.len_utf8()..];
                let digits = after.chars().take_while(char::is_ascii_hexdigit).count();
                let next = after[digits..].chars().next();
                c == '#'
                    && matches!(digits, 3 | 4 | 6 | 8)
                    && !next.is_some_and(|n| n.is_alphanumeric() || n == '-' || n == '_')
            })
        };
        let lines: Vec<&str> = rest
            .lines()
            .filter(|l| hex(l) || l.contains("rgb(") || l.contains("rgba(") || l.contains("hsl("))
            .collect();
        assert!(lines.is_empty(), "colours outside the theme: {lines:#?}");
    }
}
