//! The bridge between the dashboard page and the app, over wry IPC. The app injects `window.scApp`
//! into its own webviews only; the page feature-detects it and falls back to browser windows in a
//! plain browser.
//!
//! Page to app: `window.ipc.postMessage(JSON.stringify({ id, type, ... }))`, answered by
//! `window.__scAppReply(id, result)`:
//!
//! | `type` | Fields | `result` |
//! |---|---|---|
//! | `displays` | | `{ displays: [{ label, width, height, left, top, scale, primary, builtin }], shared: string[] }` |
//! | `open` | `tab`, `displayLabel?`, `fullscreen?` | `{ ok: true }` or `{ ok: false, problem: 'tab' \| 'display' \| 'missing' \| 'ambiguous' \| 'window', text }` |
//! | `restore` | | `{ tabs: [{ tab, name, displayLabel, state: 'open' \| 'missing' \| 'ambiguous' \| 'failed', text? }] }` |
//! | `close` | | `{ ok: true }`: closes the sender's own window (a detached tab) |
//! | `dashboard` | | `{ ok: true }`: shows the main window |
//! | `mapping` | `active` | `{ ok: true }`: pauses the app's bezel buttons on the sender's tab while the page maps them |
//!
//! Without `displayLabel`, `open` uses the tab's remembered placement. Remembering a display stays
//! the page's job (`dash.setPlacement`), so the browser and the app keep one rule for it.
//!
//! App to page: `window.__scAppEvent(event)`, with `{ type: 'notice', text, tone }`,
//! `{ type: 'hold', label }` and `{ type: 'holdEnd' }` for the app's bezel buttons on that tab.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::displays::{self, Display, DisplayMatch};
use crate::model::{DashState, Tab};

/// Which window a message came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sender {
    Main,
    Tab(String),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Request {
    Displays,
    #[serde(rename_all = "camelCase")]
    Open {
        tab: String,
        #[serde(default)]
        display_label: Option<String>,
        #[serde(default)]
        fullscreen: Option<bool>,
    },
    Restore,
    Close,
    Dashboard,
    Mapping {
        active: bool,
    },
}

/// A page message: its id (to answer) and what it asks.
pub fn parse(text: &str) -> Result<(u64, Request), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let id = v.get("id").and_then(Value::as_u64).ok_or("no message id")?;
    let request = serde_json::from_value(v).map_err(|e| format!("message {id}: {e}"))?;
    Ok((id, request))
}

/// What the bridge needs from the app.
pub trait Host {
    fn displays(&self) -> Vec<Display>;
    fn dashboard(&self) -> DashState;
    fn open_tab(&mut self, tab: &Tab, display: &Display, fullscreen: bool) -> Result<(), String>;
    fn close_window(&mut self, who: &Sender);
    fn show_dashboard(&mut self);
    fn set_mapping(&mut self, tab: &str, active: bool);
}

fn problem(kind: &str, text: String) -> Value {
    json!({ "ok": false, "problem": kind, "text": text })
}

fn not_found(m: &DisplayMatch, label: &str) -> String {
    match m {
        DisplayMatch::Ambiguous(hits) => format!(
            "{} displays are named {label}, so the app cannot tell which one is meant",
            hits.len()
        ),
        _ => format!("{label} is not connected"),
    }
}

fn open(host: &mut impl Host, tab_id: &str, label: Option<String>, fs: Option<bool>) -> Value {
    let state = host.dashboard();
    let Some(tab) = state.tab(tab_id) else {
        return problem("tab", format!("There is no tab {tab_id}"));
    };
    let Some(label) = label.or_else(|| tab.placement.as_ref().map(|p| p.display_label.clone()))
    else {
        return problem(
            "display",
            format!("{} has no display yet; choose one", tab.name),
        );
    };
    let fullscreen = fs
        .or_else(|| tab.placement.as_ref().map(|p| p.fullscreen))
        .unwrap_or(true);
    let list = host.displays();
    let found = displays::find(&list, &label);
    let DisplayMatch::Found(display) = found else {
        return problem(found.state(), not_found(&found, &label));
    };
    match host.open_tab(tab, display, fullscreen) {
        Ok(()) => json!({ "ok": true }),
        Err(e) => problem("window", e),
    }
}

/// Opens each placed tab on its display; the outcome per tab, as the `restore` result.
pub fn restore_all(host: &mut impl Host) -> Value {
    let state = host.dashboard();
    let list = host.displays();
    let mut tabs = Vec::new();
    for tab in state.restorable() {
        let Some(p) = &tab.placement else {
            continue;
        };
        let found = displays::find(&list, &p.display_label);
        let (state, text) = match &found {
            DisplayMatch::Found(d) => match host.open_tab(tab, d, p.fullscreen) {
                Ok(()) => ("open", None),
                Err(e) => ("failed", Some(e)),
            },
            other => (other.state(), Some(not_found(other, &p.display_label))),
        };
        tabs.push(
            json!({ "tab": tab.id, "name": tab.name, "displayLabel": p.display_label,
            "state": state, "text": text }),
        );
    }
    json!({ "tabs": tabs })
}

/// Runs one page message; the reply to send back, or why it was ignored.
pub fn handle(host: &mut impl Host, from: &Sender, text: &str) -> Result<(u64, Value), String> {
    let (id, request) = parse(text)?;
    let ok = json!({ "ok": true });
    let result = match request {
        Request::Displays => {
            let list = host.displays();
            json!({ "displays": list, "shared": displays::shared_labels(&list) })
        }
        Request::Open {
            tab,
            display_label,
            fullscreen,
        } => open(host, &tab, display_label, fullscreen),
        Request::Restore => restore_all(host),
        Request::Close => {
            host.close_window(from);
            ok
        }
        Request::Dashboard => {
            host.show_dashboard();
            ok
        }
        Request::Mapping { active } => match from {
            Sender::Tab(tab) => {
                host.set_mapping(tab, active);
                ok
            }
            Sender::Main => ok,
        },
    };
    Ok((id, result))
}

/// The script that answers a page message.
pub fn reply_script(id: u64, result: &Value) -> String {
    format!("window.__scAppReply && window.__scAppReply({id}, {result});")
}

/// The script that hands the page an app event.
pub fn event_script(event: &Value) -> String {
    format!("window.__scAppEvent && window.__scAppEvent({event});")
}

/// Injected before the page's own scripts in every app webview.
pub fn init_script(native_bezel: bool) -> String {
    INIT_SCRIPT.replace(
        "__NATIVE_BEZEL__",
        if native_bezel { "true" } else { "false" },
    )
}

const INIT_SCRIPT: &str = r#"(() => {
  if (window.scApp || !window.ipc) return;
  const waiting = new Map();
  const listeners = [];
  let next = 1;
  window.__scAppReply = (id, result) => {
    const done = waiting.get(id);
    if (done) { waiting.delete(id); done(result); }
  };
  window.__scAppEvent = (event) => {
    for (const f of listeners) { try { f(event); } catch (e) { console.error(e); } }
  };
  window.scApp = Object.freeze({
    version: 1,
    nativeBezel: __NATIVE_BEZEL__,
    request(type, fields) {
      const id = next++;
      return new Promise((resolve) => {
        waiting.set(id, resolve);
        window.ipc.postMessage(JSON.stringify({ ...(fields || {}), id, type }));
      });
    },
    on(f) { listeners.push(f); },
  });
})();"#;

/// The page origin the app serves; IPC and navigation are allowed from it only.
pub fn origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn is_own_page(url: &str, port: u16) -> bool {
    url.strip_prefix(&origin(port))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
}

/// The tab a `window.open` of a kiosk URL asks for (an unpatched page detaching), with `fs`.
pub fn kiosk_tab(url: &str, port: u16) -> Option<(String, bool)> {
    if !is_own_page(url, port) {
        return None;
    }
    let rest = &url[origin(port).len()..];
    let query = rest.strip_prefix("/dash?")?;
    let mut tab = None;
    let mut kiosk = false;
    let mut fs = false;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "tab" => tab = Some(percent_decode(v)?),
            "kiosk" => kiosk = v == "1",
            "fs" => fs = v == "1",
            _ => {}
        }
    }
    let tab = tab.filter(|t| !t.is_empty())?;
    kiosk.then_some((tab, fs))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// A kiosk URL for a tab, as the page builds it.
pub fn kiosk_url(port: u16, tab: &str, fullscreen: bool) -> String {
    let mut id = String::new();
    for b in tab.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            id.push(b as char);
        } else {
            id.push_str(&format!("%{b:02X}"));
        }
    }
    format!(
        "{}/dash?tab={id}&kiosk=1{}",
        origin(port),
        if fullscreen { "&fs=1" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::display;
    use crate::model::Placement;

    #[derive(Default)]
    struct FakeHost {
        displays: Vec<Display>,
        state: DashState,
        opened: Vec<(String, String, bool)>,
        closed: Vec<Sender>,
        shown: usize,
        mapping: Vec<(String, bool)>,
        fail: bool,
    }

    impl Host for FakeHost {
        fn displays(&self) -> Vec<Display> {
            self.displays.clone()
        }
        fn dashboard(&self) -> DashState {
            self.state.clone()
        }
        fn open_tab(&mut self, tab: &Tab, d: &Display, fs: bool) -> Result<(), String> {
            if self.fail {
                return Err("no window".into());
            }
            self.opened.push((tab.id.clone(), d.label.clone(), fs));
            Ok(())
        }
        fn close_window(&mut self, who: &Sender) {
            self.closed.push(who.clone());
        }
        fn show_dashboard(&mut self) {
            self.shown += 1;
        }
        fn set_mapping(&mut self, tab: &str, active: bool) {
            self.mapping.push((tab.into(), active));
        }
    }

    fn tab(id: &str, placed_on: Option<&str>) -> Tab {
        Tab {
            id: id.into(),
            name: id.to_uppercase(),
            placement: placed_on.map(|l| Placement {
                display_label: l.into(),
                fullscreen: false,
            }),
            bezel: None,
        }
    }

    fn host() -> FakeHost {
        FakeHost {
            displays: vec![
                display("DELL U2720Q", r"\\.\DISPLAY1", 0),
                display("MFD Left", r"\\.\DISPLAY2", 3840),
                display("Twin", r"\\.\DISPLAY3", 5000),
                display("Twin", r"\\.\DISPLAY4", 6000),
            ],
            state: DashState {
                tabs: vec![
                    tab("main", None),
                    tab("mip", Some("MFD Left")),
                    tab("gone", Some("Old Screen")),
                    tab("twin", Some("Twin")),
                ],
                ..DashState::default()
            },
            ..FakeHost::default()
        }
    }

    fn run(h: &mut FakeHost, from: &Sender, text: &str) -> Value {
        handle(h, from, text).unwrap().1
    }

    #[test]
    fn parses_messages() {
        assert_eq!(
            parse(r#"{"id":4,"type":"displays"}"#).unwrap(),
            (4, Request::Displays)
        );
        assert_eq!(
            parse(r#"{"id":5,"type":"open","tab":"mip","displayLabel":"X","fullscreen":true}"#)
                .unwrap()
                .1,
            Request::Open {
                tab: "mip".into(),
                display_label: Some("X".into()),
                fullscreen: Some(true)
            }
        );
        assert_eq!(
            parse(r#"{"id":6,"type":"mapping","active":true}"#)
                .unwrap()
                .1,
            Request::Mapping { active: true }
        );
        assert!(parse(r#"{"type":"displays"}"#).is_err(), "no id");
        assert!(parse(r#"{"id":1,"type":"format-disk"}"#).is_err());
        assert!(
            parse(r#"{"id":1,"type":"open"}"#).is_err(),
            "open needs a tab"
        );
        assert!(parse("nope").is_err());
    }

    #[test]
    fn lists_displays_without_device_names() {
        let mut h = host();
        let r = run(&mut h, &Sender::Main, r#"{"id":1,"type":"displays"}"#);
        assert_eq!(r["displays"].as_array().unwrap().len(), 4);
        assert_eq!(r["displays"][1]["label"], "MFD Left");
        assert_eq!(r["displays"][1]["left"], 3840);
        assert!(r["displays"][1].get("device").is_none());
        assert_eq!(r["shared"], json!(["Twin"]));
    }

    #[test]
    fn opens_on_the_chosen_or_remembered_display() {
        let mut h = host();
        let r = run(
            &mut h,
            &Sender::Main,
            r#"{"id":1,"type":"open","tab":"main","displayLabel":"MFD Left","fullscreen":true}"#,
        );
        assert_eq!(r, json!({ "ok": true }));
        let r = run(
            &mut h,
            &Sender::Main,
            r#"{"id":2,"type":"open","tab":"mip"}"#,
        );
        assert_eq!(r["ok"], true);
        assert_eq!(
            h.opened,
            vec![
                ("main".into(), "MFD Left".into(), true),
                ("mip".into(), "MFD Left".into(), false)
            ]
        );
    }

    #[test]
    fn open_reports_each_problem() {
        let mut h = host();
        let p = |h: &mut FakeHost, m: &str| run(h, &Sender::Main, m)["problem"].clone();
        assert_eq!(p(&mut h, r#"{"id":1,"type":"open","tab":"nope"}"#), "tab");
        assert_eq!(
            p(&mut h, r#"{"id":1,"type":"open","tab":"main"}"#),
            "display"
        );
        assert_eq!(
            p(&mut h, r#"{"id":1,"type":"open","tab":"gone"}"#),
            "missing"
        );
        assert_eq!(
            p(&mut h, r#"{"id":1,"type":"open","tab":"twin"}"#),
            "ambiguous"
        );
        h.fail = true;
        assert_eq!(p(&mut h, r#"{"id":1,"type":"open","tab":"mip"}"#), "window");
        assert!(h.opened.is_empty());
    }

    #[test]
    fn restore_opens_each_placed_tab_and_lists_the_rest() {
        let mut h = host();
        let r = run(&mut h, &Sender::Main, r#"{"id":9,"type":"restore"}"#);
        let states: Vec<(&str, &str)> = r["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["tab"].as_str().unwrap(), t["state"].as_str().unwrap()))
            .collect();
        assert_eq!(
            states,
            vec![("mip", "open"), ("gone", "missing"), ("twin", "ambiguous")]
        );
        assert_eq!(h.opened, vec![("mip".into(), "MFD Left".into(), false)]);
    }

    #[test]
    fn close_dashboard_and_mapping_act_on_the_sender() {
        let mut h = host();
        let mip = Sender::Tab("mip".into());
        run(&mut h, &mip, r#"{"id":1,"type":"close"}"#);
        run(&mut h, &mip, r#"{"id":2,"type":"dashboard"}"#);
        run(&mut h, &mip, r#"{"id":3,"type":"mapping","active":true}"#);
        run(
            &mut h,
            &Sender::Main,
            r#"{"id":4,"type":"mapping","active":true}"#,
        );
        assert_eq!(h.closed, vec![mip]);
        assert_eq!(h.shown, 1);
        assert_eq!(h.mapping, vec![("mip".into(), true)]);
    }

    #[test]
    fn scripts_carry_json() {
        assert_eq!(
            reply_script(3, &json!({ "ok": true })),
            r#"window.__scAppReply && window.__scAppReply(3, {"ok":true});"#
        );
        let s = event_script(&json!({ "type": "notice", "text": "</script>\u{2028}" }));
        assert!(s.contains(r#""text":"</script>"#));
        assert!(init_script(true).contains("nativeBezel: true,"));
        assert!(init_script(false).contains("nativeBezel: false,"));
    }

    #[test]
    fn only_own_pages() {
        assert!(is_own_page("http://127.0.0.1:47520/dash", 47520));
        assert!(is_own_page("http://127.0.0.1:47520", 47520));
        assert!(!is_own_page("http://127.0.0.1:475201/dash", 47520));
        assert!(!is_own_page("http://127.0.0.1:47520.evil.test/", 47520));
        assert!(!is_own_page(
            "https://example.com/?http://127.0.0.1:47520/",
            47520
        ));
    }

    #[test]
    fn kiosk_urls_round_trip() {
        assert_eq!(
            kiosk_tab("http://127.0.0.1:47520/dash?tab=mip&kiosk=1&fs=1", 47520),
            Some(("mip".into(), true))
        );
        assert_eq!(
            kiosk_tab("http://127.0.0.1:47520/dash?tab=a%2Db&kiosk=1", 47520),
            Some(("a-b".into(), false))
        );
        assert_eq!(
            kiosk_tab("http://127.0.0.1:47520/dash?tab=mip", 47520),
            None
        );
        assert_eq!(
            kiosk_tab("http://127.0.0.1:47520/portal?tab=mip&kiosk=1", 47520),
            None
        );
        assert_eq!(
            kiosk_tab("http://127.0.0.1:1/dash?tab=mip&kiosk=1", 47520),
            None
        );
        assert_eq!(
            kiosk_tab("http://127.0.0.1:47520/dash?tab=%zz&kiosk=1", 47520),
            None
        );
        let url = kiosk_url(47520, "shield-faces_2", true);
        assert_eq!(
            url,
            "http://127.0.0.1:47520/dash?tab=shield-faces_2&kiosk=1&fs=1"
        );
        assert_eq!(
            kiosk_tab(&url, 47520),
            Some(("shield-faces_2".into(), true))
        );
    }
}
