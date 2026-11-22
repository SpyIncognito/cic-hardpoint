//! Dashboard themes. A theme is colour tokens plus a few fixed choices (bar style, leading zeros,
//! font) on a base: the built-in it comes from, whose shapes, title style, chevrons and widget
//! skins the page draws from its own CSS and SVG. Skins ship with the app only.
//!
//! Users make a theme by duplicating one and editing its colours and choices. Those are kept in
//! themes.json in the settings folder, never in the dashboard file, so an imported dashboard can
//! name a theme but never define one. Every colour is a plain hex colour and every choice a name
//! from a fixed set, so no value can become CSS of its own.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::{Value, json};
use tokio::sync::watch;

use sc_core::js;

use crate::json::{js_keys, js_string, to_pretty_text};

pub const THEME_SCHEMA_VERSION: u32 = 1;
pub const MAX_USER_THEMES: usize = 20;
/// In UTF-16 code units, as the page counts `length`.
pub const MAX_THEME_NAME: usize = 40;
pub const DEFAULT_THEME: &str = "rsi";

/// The colour tokens every theme sets, as CSS custom properties (`--bg` and so on).
#[rustfmt::skip]
pub const COLOR_TOKENS: &[&str] = &[
    "bg", "panel", "panel-2", "line", "fg", "dim", "faint", "value", "accent", "select", "select-fg",
    "title-bg", "title-fg", "bar", "ok", "warn", "bad", "off",
    "lamp-off-bg", "lamp-off-fg", "lamp-nominal-bg", "lamp-nominal-fg", "lamp-on-bg", "lamp-on-fg",
    "lamp-minor-bg", "lamp-minor-fg", "lamp-major-bg", "lamp-major-fg",
    "input-bg", "input-fg", "input-border", "input-placeholder", "input-focus",
];
/// Text fields, lists and their option lists. A theme saved before these existed takes them
/// from its own colours, so it still loads.
pub const INPUT_FALLBACKS: &[(&str, &str)] = &[
    ("input-bg", "bg"),
    ("input-fg", "fg"),
    ("input-border", "line"),
    ("input-placeholder", "dim"),
    ("input-focus", "accent"),
];
/// The bases: each fixes the shapes, title style, chevrons and widget skins.
pub const BASES: &[&str] = &["rsi", "drake"];
pub const FONTS: &[&str] = &["condensed", "mono", "sans"];
pub const BAR_STYLES: &[&str] = &["segmented", "solid"];

const FIELDS: &[&str] = &[
    "id",
    "name",
    "base",
    "colors",
    "bars",
    "leadingZeros",
    "font",
];

/// Colours in [`COLOR_TOKENS`] order, each a lower-case hex colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colors(Vec<String>);

impl Colors {
    pub fn get(&self, token: &str) -> Option<&str> {
        let i = COLOR_TOKENS.iter().position(|t| *t == token)?;
        self.0.get(i).map(String::as_str)
    }
}

impl Serialize for Colors {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (token, color) in COLOR_TOKENS.iter().zip(&self.0) {
            map.serialize_entry(token, color)?;
        }
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub id: String,
    pub name: String,
    /// One of [`BASES`].
    pub base: &'static str,
    pub colors: Colors,
    /// One of [`BAR_STYLES`].
    pub bars: &'static str,
    /// Numbers padded with dimmed zeros (`0042`), as on the RSI screens.
    pub leading_zeros: bool,
    /// One of [`FONTS`].
    pub font: &'static str,
}

#[rustfmt::skip]
fn built_in(id: &str, name: &str, bars: &'static str, leading_zeros: bool, font: &'static str, colors: [&str; 33]) -> Theme {
    let base = BASES.iter().find(|b| **b == id).expect("a built-in is its own base");
    Theme {
        id: id.to_owned(),
        name: name.to_owned(),
        base,
        colors: Colors(colors.iter().map(|c| (*c).to_owned()).collect()),
        bars,
        leading_zeros,
        font,
    }
}

/// RSI (Galaxy MFDs): dark slate panels, white condensed capitals, orange for selected, yellow
/// edge accents, white segmented bars, dimmed leading zeros. Drake (Corsair): a monochrome amber
/// CRT, monospaced wide capitals, a pale-yellow title bar with dark text, orange values.
#[rustfmt::skip]
pub static BUILT_IN_THEMES: LazyLock<[Theme; 2]> = LazyLock::new(|| [
    built_in("rsi", "RSI", "segmented", true, "condensed", [
        "#0c1318", "#18232b", "#202d36", "#364955", "#eef3f5", "#8fa1ab", "#4a5d68", "#ffffff", "#f5c518", "#ff8a1e", "#170b02",
        "#e8edf0", "#0b1217", "#e8edf0", "#55d494", "#ffb020", "#ff4d40", "#6b7c86",
        "#0e161b", "#6f818b", "#17232b", "#b8c8d0", "#17442f", "#8ff2bf",
        "#5c3e05", "#ffc94a", "#621510", "#ff7466",
        "#081014", "#eef3f5", "#4f6573", "#8fa1ab", "#f5c518",
    ]),
    built_in("drake", "Drake", "segmented", false, "mono", [
        "#140d03", "#1b1305", "#251a08", "#7a5c18", "#ffdc82", "#d4ad52", "#7d6124", "#ffa43a", "#ffd257", "#ffe7a1", "#1a1104",
        "#ffe7a1", "#1a1104", "#ffe08f", "#ffdc82", "#ff9d2e", "#ff6233", "#a58137",
        "#170f04", "#a58137", "#251a08", "#e6c061", "#4d3c10", "#ffe7a1",
        "#4d2f05", "#ffad42", "#4d1606", "#ff7a4d",
        "#0d0802", "#ffdc82", "#8a6a22", "#b08b3c", "#ffd257",
    ]),
]);

pub fn built_in_theme(id: &str) -> Option<&'static Theme> {
    BUILT_IN_THEMES.iter().find(|t| t.id == id)
}

pub fn is_built_in(id: &str) -> bool {
    built_in_theme(id).is_some()
}

/// The CSS for one theme's colours. Only ever called on a built-in or a theme that passed
/// [`check_theme`]: the id is `[a-z0-9_-]` and every colour plain hex.
pub fn theme_css(t: &Theme) -> String {
    let tokens: Vec<String> = COLOR_TOKENS
        .iter()
        .zip(&t.colors.0)
        .map(|(k, v)| format!("--{k}: {v};"))
        .collect();
    format!("[data-theme=\"{}\"] {{ {} }}", t.id, tokens.join(" "))
}

/// Every built-in's CSS, one rule a line, as the dashboard page gets it.
pub fn built_in_css() -> String {
    BUILT_IN_THEMES
        .iter()
        .map(theme_css)
        .collect::<Vec<_>>()
        .join("\n    ")
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, and nothing else: no names, functions, `url()` or `;`.
pub fn is_hex_color(s: &str) -> bool {
    s.strip_prefix('#').is_some_and(|hex| {
        matches!(hex.len(), 3 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// `/^[a-z0-9_-]{1,40}$/`, as tab and widget ids.
pub fn is_theme_id(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn quote(v: Option<&Value>) -> String {
    quote_str(&js_string(v))
}

fn quote_str(s: &str) -> String {
    serde_json::to_string(&js::utf16_prefix(s, 60)).expect("a string serializes")
}

fn is_plain(s: &str) -> bool {
    !s.chars().any(|c| c == '<' || c == '>' || c.is_control())
}

fn one_of(v: Option<&Value>, set: &'static [&'static str]) -> Option<&'static str> {
    let s = v?.as_str()?;
    set.iter().find(|x| **x == s).copied()
}

pub fn check_theme_name(name: Option<&Value>) -> Vec<String> {
    match name.and_then(Value::as_str) {
        Some(s) if !js::trim(s).is_empty() && js::utf16_len(s) <= MAX_THEME_NAME => {
            if is_plain(s) {
                vec![]
            } else {
                vec!["the theme name must be plain text (no < or >)".to_owned()]
            }
        }
        _ => vec![format!(
            "the theme name must be 1 to {MAX_THEME_NAME} characters"
        )],
    }
}

/// Rebuild a user theme from known fields only; any problem refuses it.
pub fn check_theme(raw: &Value) -> Result<Theme, Vec<String>> {
    let Some(obj) = raw.as_object() else {
        return Err(vec!["it is not a theme".to_owned()]);
    };
    let mut problems: Vec<String> = js_keys(obj)
        .into_iter()
        .filter(|k| !FIELDS.contains(k))
        .map(|k| format!("field {} is not allowed", quote_str(k)))
        .collect();
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| is_theme_id(id));
    match id {
        None => problems
            .push("the theme id must be 1 to 40 lower-case letters, digits, - or _".to_owned()),
        Some(id) if is_built_in(id) => problems.push(format!(
            "{} is a built-in theme and cannot be changed",
            quote_str(id)
        )),
        Some(_) => {}
    }
    problems.extend(check_theme_name(obj.get("name")));
    let base = one_of(obj.get("base"), BASES);
    if base.is_none() {
        problems.push(format!("the base must be one of {}", BASES.join(", ")));
    }
    let bars = one_of(obj.get("bars"), BAR_STYLES);
    if bars.is_none() {
        problems.push(format!("bars must be one of {}", BAR_STYLES.join(", ")));
    }
    let font = one_of(obj.get("font"), FONTS);
    if font.is_none() {
        problems.push(format!("the font must be one of {}", FONTS.join(", ")));
    }
    let leading_zeros = obj.get("leadingZeros").and_then(Value::as_bool);
    if leading_zeros.is_none() {
        problems.push("leading zeros must be true or false".to_owned());
    }
    let mut colors = Vec::with_capacity(COLOR_TOKENS.len());
    match obj.get("colors").and_then(Value::as_object) {
        None => problems.push("it has no colours".to_owned()),
        Some(map) => {
            problems.extend(
                js_keys(map)
                    .into_iter()
                    .filter(|k| !COLOR_TOKENS.contains(k))
                    .map(|k| format!("colour {} is not a theme colour", quote_str(k))),
            );
            for token in COLOR_TOKENS {
                let fallback = INPUT_FALLBACKS
                    .iter()
                    .find(|(t, _)| t == token)
                    .and_then(|(_, from)| COLOR_TOKENS.iter().position(|t| t == from))
                    .and_then(|i| colors.get(i).cloned());
                match map.get(*token) {
                    None if fallback.is_some() => colors.extend(fallback),
                    None => problems.push(format!("colour {token} is missing")),
                    Some(Value::String(c)) if is_hex_color(c) => {
                        colors.push(c.to_ascii_lowercase())
                    }
                    Some(v) => problems.push(format!(
                        "colour {token} must be #rgb, #rrggbb or #rrggbbaa, not {}",
                        quote(Some(v))
                    )),
                }
            }
        }
    }
    match (id, base, bars, font, leading_zeros) {
        (Some(id), Some(base), Some(bars), Some(font), Some(leading_zeros))
            if problems.is_empty() =>
        {
            Ok(Theme {
                id: id.to_owned(),
                name: js::trim(obj.get("name").and_then(Value::as_str).unwrap_or_default())
                    .to_owned(),
                base,
                colors: Colors(colors),
                bars,
                leading_zeros,
                font,
            })
        }
        _ => Err(problems),
    }
}

/// What a theme change answers: the reasons it was refused, and the theme it made or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeChange {
    pub problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
}

impl ThemeChange {
    fn refused(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
            theme: None,
        }
    }

    fn done(theme: Option<Theme>) -> Self {
        Self {
            problems: vec![],
            theme,
        }
    }
}

/// The user's own themes, kept as themes.json in the settings folder. Changes are made in memory
/// first; an `Err` means only the file write failed.
#[derive(Debug)]
pub struct ThemeStore {
    themes: Mutex<Vec<Theme>>,
    file: Option<PathBuf>,
    changes: watch::Sender<u64>,
}

impl Default for ThemeStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ThemeStore {
    /// Kept in `file` when given, in memory only otherwise.
    pub fn new(file: Option<PathBuf>) -> Self {
        let themes = file.as_deref().map(load).unwrap_or_default();
        Self {
            themes: Mutex::new(themes),
            file,
            changes: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Theme>> {
        self.themes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes after every commit.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn user(&self) -> Vec<Theme> {
        self.lock().clone()
    }

    /// Every theme name a dashboard may use: the built-ins and the user's.
    pub fn ids(&self) -> HashSet<String> {
        BUILT_IN_THEMES
            .iter()
            .map(|t| t.id.clone())
            .chain(self.lock().iter().map(|t| t.id.clone()))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Theme> {
        built_in_theme(id)
            .cloned()
            .or_else(|| self.lock().iter().find(|t| t.id == id).cloned())
    }

    /// The theme a tab is drawn in: its own, else the dashboard's, else RSI. A name that is not
    /// known (a removed theme) is passed over. The page draws by the same rule.
    pub fn resolve(&self, tab: Option<&str>, dashboard: Option<&str>) -> Theme {
        tab.and_then(|id| self.get(id))
            .or_else(|| dashboard.and_then(|id| self.get(id)))
            .unwrap_or_else(|| BUILT_IN_THEMES[0].clone())
    }

    /// What the dashboard page needs to list, draw and edit themes.
    pub fn model(&self) -> Value {
        json!({
            "builtIn": &*BUILT_IN_THEMES,
            "user": self.user(),
            "defaultTheme": DEFAULT_THEME,
            "colorTokens": COLOR_TOKENS,
            "bases": BASES,
            "fonts": FONTS,
            "bars": BAR_STYLES,
            "limits": { "themes": MAX_USER_THEMES, "name": MAX_THEME_NAME },
        })
    }

    /// Change an existing user theme: its colours, choices or name. The base stays the one it
    /// was made from.
    pub fn save(&self, raw: &Value) -> io::Result<ThemeChange> {
        let theme = match check_theme(raw) {
            Ok(theme) => theme,
            Err(problems) => {
                return Ok(ThemeChange {
                    problems,
                    theme: None,
                });
            }
        };
        let mut themes = self.lock();
        let Some(i) = themes.iter().position(|t| t.id == theme.id) else {
            return Ok(ThemeChange::refused(format!(
                "No theme {}; duplicate one to make a new theme",
                quote_str(&theme.id)
            )));
        };
        if themes[i].base != theme.base {
            return Ok(ThemeChange::refused(
                "The base cannot be changed; duplicate the other built-in instead",
            ));
        }
        if let Some(clash) = name_taken(&themes, &theme.name, Some(&theme.id)) {
            return Ok(ThemeChange::refused(clash));
        }
        themes[i] = theme.clone();
        self.commit(themes)?;
        Ok(ThemeChange::done(Some(theme)))
    }

    /// A new user theme copied from any theme, named "<name> copy" (then "copy 2" and up) unless
    /// a name is given.
    pub fn duplicate(&self, id: &str, name: Option<&Value>) -> io::Result<ThemeChange> {
        let Some(src) = self.get(id) else {
            return Ok(ThemeChange::refused(format!("No theme {}", quote_str(id))));
        };
        let mut themes = self.lock();
        if themes.len() >= MAX_USER_THEMES {
            return Ok(ThemeChange::refused(format!(
                "At most {MAX_USER_THEMES} themes of your own"
            )));
        }
        let given = name
            .and_then(Value::as_str)
            .map(js::trim)
            .filter(|s| !s.is_empty());
        let wanted = match given {
            Some(given) => {
                let problems = check_theme_name(Some(&Value::String(given.to_owned())));
                if !problems.is_empty() {
                    return Ok(ThemeChange {
                        problems,
                        theme: None,
                    });
                }
                if let Some(clash) = name_taken(&themes, given, None) {
                    return Ok(ThemeChange::refused(clash));
                }
                given.to_owned()
            }
            None => {
                let stem = js::utf16_prefix(&src.name, MAX_THEME_NAME - 8);
                let mut wanted = format!("{stem} copy");
                let mut n = 2;
                while name_taken(&themes, &wanted, None).is_some() {
                    wanted = format!("{stem} copy {n}");
                    n += 1;
                }
                wanted
            }
        };
        let mut n = 1;
        while is_built_in(&format!("theme-{n}"))
            || themes.iter().any(|t| t.id == format!("theme-{n}"))
        {
            n += 1;
        }
        let theme = Theme {
            id: format!("theme-{n}"),
            name: wanted,
            ..src
        };
        themes.push(theme.clone());
        self.commit(themes)?;
        Ok(ThemeChange::done(Some(theme)))
    }

    /// Remove a user theme. The caller drops the dashboard's references to it.
    pub fn remove(&self, id: &str) -> io::Result<ThemeChange> {
        if is_built_in(id) {
            return Ok(ThemeChange::refused(format!(
                "{} is a built-in theme and cannot be removed",
                quote_str(id)
            )));
        }
        let mut themes = self.lock();
        let Some(i) = themes.iter().position(|t| t.id == id) else {
            return Ok(ThemeChange::refused(format!("No theme {}", quote_str(id))));
        };
        themes.remove(i);
        self.commit(themes)?;
        Ok(ThemeChange::done(None))
    }

    fn commit(&self, themes: MutexGuard<'_, Vec<Theme>>) -> io::Result<()> {
        if let Some(file) = &self.file {
            save(file, &themes)?;
        }
        drop(themes);
        self.changes.send_modify(|n| *n += 1);
        Ok(())
    }
}

/// The clash message when `name` (case-insensitive) is another theme's, built-in or user.
fn name_taken(user: &[Theme], name: &str, except: Option<&str>) -> Option<String> {
    let lower = js::trim(name).to_lowercase();
    BUILT_IN_THEMES
        .iter()
        .chain(user)
        .find(|t| Some(t.id.as_str()) != except && t.name.to_lowercase() == lower)
        .map(|t| format!("A theme is already named {}", quote_str(&t.name)))
}

fn load(file: &Path) -> Vec<Theme> {
    // An unreadable file means no user themes; the next change rewrites it.
    let Some(value) = fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return vec![];
    };
    let list = value.get("themes").and_then(Value::as_array);
    let mut out: Vec<Theme> = Vec::new();
    for raw in list.into_iter().flatten().take(MAX_USER_THEMES) {
        match check_theme(raw) {
            Ok(t) if !out.iter().any(|o| o.id == t.id) => out.push(t),
            Ok(_) => tracing::warn!(
                "[themes] {}: left out a theme, its id is used twice",
                file.display()
            ),
            Err(problems) => tracing::warn!(
                "[themes] {}: left out a theme, {}",
                file.display(),
                problems.join("; ")
            ),
        }
    }
    out
}

fn save(file: &Path, themes: &[Theme]) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(".tmp");
    let doc = json!({ "schemaVersion": THEME_SCHEMA_VERSION, "themes": themes });
    fs::write(&tmp, to_pretty_text(&doc))?;
    fs::rename(&tmp, file)
}

/// WCAG 2 relative luminance of `#rgb` or `#rrggbb` (alpha ignored).
pub fn luminance(hex: &str) -> Option<f64> {
    let h = hex.strip_prefix('#')?;
    let full: String = match h.len() {
        3 | 4 => h.chars().take(3).flat_map(|c| [c, c]).collect(),
        6 | 8 => h[..6].to_owned(),
        _ => return None,
    };
    let channel = |i: usize| -> Option<f64> {
        let v = f64::from(u8::from_str_radix(&full[i..i + 2], 16).ok()?) / 255.0;
        Some(if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        })
    };
    Some(0.2126 * channel(0)? + 0.7152 * channel(2)? + 0.0722 * channel(4)?)
}

/// WCAG 2 contrast ratio of two colours, 1 to 21.
pub fn contrast(a: &str, b: &str) -> Option<f64> {
    let (x, y) = (luminance(a)?, luminance(b)?);
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    Some((hi + 0.05) / (lo + 0.05))
}
