//! Bezel buttons read natively (gilrs on Windows), so they work while the game has the focus.
//! Each press goes through the tab's bezel mapping and becomes the page message a widget click
//! sends, on the app's own portal session; a confirm-group action is held and the service times
//! the hold, as for the page.
//!
//! # Controller identity
//!
//! A mapping is learned in a page, so its `gamepadId` is the browser Gamepad API's `id` string.
//! The forms seen, and what the app matches them against (gilrs `os_name`, vendor and product id):
//!
//! | Browser | `id` | Matched by |
//! |---|---|---|
//! | Chromium, Edge, WebView2 (HID) | `F16 MFD 1 (Vendor: 044f Product: b351)` | vendor and product |
//! | the same, standard mapping | `Name (STANDARD GAMEPAD Vendor: 045e Product: 0b12)` | vendor and product |
//! | Chromium, XInput | `Xbox 360 Controller (XInput STANDARD GAMEPAD)` | the one standard pad |
//! | Firefox | `044f-b351-F16 MFD 1` | vendor and product |
//!
//! When the ids give no unique match, the name before the brackets is compared with the gilrs
//! name, and a match by name alone is logged as a warning.
//!
//! # Button numbers
//!
//! A page numbers buttons by the browser's order. With no standard mapping that is the device's
//! own button order, which for a WGI raw controller is gilrs's raw button index (the low 16 bits
//! of the event code). With `STANDARD GAMEPAD` it is the W3C standard order, so the app uses the
//! standard index of the gilrs button instead.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::model::{BezelBind, BezelButton, DashState, Tab};

/// As the page: a little over the service's 1.5 s, so the service never sees it early.
pub const HOLD: Duration = Duration::from_millis(1550);

/// A connected controller as the app sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    /// gilrs's id for this session.
    pub id: usize,
    pub name: String,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    /// The OS maps it as a standard gamepad (a WGI `Gamepad`, so XInput class).
    pub standard: bool,
}

/// One button edge.
#[derive(Debug, Clone, PartialEq)]
pub struct PadButton {
    pub pad: usize,
    /// The device's own button index, when it has one.
    pub raw: Option<u32>,
    /// The W3C standard gamepad index, when the button has one.
    pub standard: Option<u32>,
    pub pressed: bool,
}

/// The parts of a browser Gamepad API `id`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrowserPadId {
    pub name: String,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    /// The browser applied the standard mapping, so button numbers are the standard order.
    pub standard: bool,
    pub xinput: bool,
}

fn hex4(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    u16::from_str_radix(s, 16).ok()
}

fn after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let rest = &text[text.find(key)? + key.len()..];
    Some(rest.split_whitespace().next().unwrap_or(""))
}

pub fn parse_browser_id(id: &str) -> BrowserPadId {
    let id = id.trim();
    if let Some(open) = id.rfind(" (")
        && id.ends_with(')')
    {
        let inner = &id[open + 2..id.len() - 1];
        return BrowserPadId {
            name: id[..open].trim().to_owned(),
            vendor: after(inner, "Vendor:").and_then(hex4),
            product: after(inner, "Product:").and_then(hex4),
            standard: inner.contains("STANDARD GAMEPAD"),
            xinput: inner.contains("XInput"),
        };
    }
    let mut parts = id.splitn(3, '-');
    if let (Some(v), Some(p), Some(name)) = (parts.next(), parts.next(), parts.next())
        && let (Some(vendor), Some(product)) = (hex4(v), hex4(p))
    {
        return BrowserPadId {
            name: name.trim().to_owned(),
            vendor: Some(vendor),
            product: Some(product),
            ..BrowserPadId::default()
        };
    }
    BrowserPadId {
        name: id.to_owned(),
        ..BrowserPadId::default()
    }
}

#[derive(Debug, PartialEq)]
pub enum PadMatch {
    /// By vendor and product (or the one standard pad for an XInput id).
    Id(usize),
    /// By name only; the caller warns.
    Name(usize),
    None,
}

fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_lowercase(), b.trim().to_lowercase());
    !a.is_empty() && a == b
}

/// The connected controller a mapping's `gamepadId` names.
pub fn match_pad(gamepad_id: &str, pads: &[Pad]) -> PadMatch {
    let want = parse_browser_id(gamepad_id);
    let by_id: Vec<&Pad> = match (want.vendor, want.product) {
        (Some(v), Some(p)) => pads
            .iter()
            .filter(|d| d.vendor == Some(v) && d.product == Some(p))
            .collect(),
        _ if want.xinput => pads.iter().filter(|d| d.standard).collect(),
        _ => Vec::new(),
    };
    if let [one] = by_id[..] {
        return PadMatch::Id(one.id);
    }
    // Two of the same model: the name may still tell them apart.
    if let Some(d) = by_id.iter().find(|d| same_name(&d.name, &want.name)) {
        return PadMatch::Id(d.id);
    }
    let by_name: Vec<&Pad> = pads
        .iter()
        .filter(|d| same_name(&d.name, &want.name))
        .collect();
    match by_name[..] {
        [one] => PadMatch::Name(one.id),
        _ => PadMatch::None,
    }
}

/// The page message for a press of a mapped button that is not held.
fn message(bind: &BezelBind, state: &DashState) -> Result<Value, String> {
    Ok(match bind {
        BezelBind::Toggle { id, to: Some(to) } => json!({ "type": "set", "toggle": id, "to": to }),
        BezelBind::Toggle { id, to: None } => state
            .toggle_click(id)
            .cloned()
            .ok_or_else(|| format!("{id}: no data from the game"))?,
        BezelBind::Mode { to } => json!({ "type": "mode", "to": to }),
        BezelBind::Sequence { id } => json!({ "type": "run", "sequence": id }),
        BezelBind::Action { id } => json!({ "type": "press", "action": id }),
    })
}

/// What the engine asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A page message for the app's portal session, on behalf of a tab.
    Send {
        tab: String,
        message: Value,
    },
    /// Shown on the tab's window.
    Notice {
        tab: String,
        text: String,
    },
    HoldStarted {
        tab: String,
        label: String,
    },
    HoldEnded {
        tab: String,
    },
    /// Logged once.
    Warn(String),
}

#[derive(Debug)]
struct Hold {
    tab: String,
    pad: usize,
    button: u32,
    action: String,
    started: Instant,
}

#[derive(Debug, Default)]
pub struct Engine {
    pads: Vec<Pad>,
    holds: Vec<Hold>,
    warned: Vec<String>,
}

impl Engine {
    pub fn connected(&mut self, pad: Pad) {
        self.pads.retain(|p| p.id != pad.id);
        self.pads.push(pad);
    }

    pub fn disconnected(&mut self, id: usize) -> Vec<Out> {
        self.pads.retain(|p| p.id != id);
        self.end_holds(|h| h.pad == id)
    }

    pub fn pads(&self) -> &[Pad] {
        &self.pads
    }

    fn warn_once(&mut self, text: String, out: &mut Vec<Out>) {
        if !self.warned.contains(&text) {
            self.warned.push(text.clone());
            out.push(Out::Warn(text));
        }
    }

    /// The mapped button an edge is for on this tab, if the tab's controller is that pad.
    fn resolve<'t>(
        &mut self,
        tab: &'t Tab,
        edge: &PadButton,
        out: &mut Vec<Out>,
    ) -> Option<&'t BezelButton> {
        let bezel = tab.bezel.as_ref()?;
        let pad = match match_pad(&bezel.gamepad_id, &self.pads) {
            PadMatch::Id(id) => id,
            PadMatch::Name(id) => {
                let name = self
                    .pads
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone());
                self.warn_once(
                    format!(
                        "tab {}: controller \"{}\" matched by name only, as {}",
                        tab.id,
                        bezel.gamepad_id,
                        name.unwrap_or_default()
                    ),
                    out,
                );
                id
            }
            PadMatch::None => return None,
        };
        if pad != edge.pad {
            return None;
        }
        let index = if parse_browser_id(&bezel.gamepad_id).standard {
            edge.standard
        } else {
            edge.raw
        }?;
        bezel.buttons.iter().find(|b| b.button == index)
    }

    /// A button edge, for the tabs whose windows are open and not mapping.
    pub fn button(
        &mut self,
        state: &DashState,
        active: &[String],
        edge: &PadButton,
        now: Instant,
    ) -> Vec<Out> {
        let mut out = Vec::new();
        for tab_id in active {
            let Some(tab) = state.tab(tab_id) else {
                continue;
            };
            let Some(mapped) = self.resolve(tab, edge, &mut out) else {
                continue;
            };
            let button = mapped.button;
            if !edge.pressed {
                let ended =
                    self.end_holds(|h| h.tab == tab.id && h.pad == edge.pad && h.button == button);
                out.extend(ended);
                continue;
            }
            match &mapped.bind {
                BezelBind::Action { id } if state.is_confirm(id) => {
                    out.extend(self.end_holds(|h| h.tab == tab.id));
                    out.push(Out::Send {
                        tab: tab.id.clone(),
                        message: json!({ "type": "hold.start", "action": id }),
                    });
                    out.push(Out::HoldStarted {
                        tab: tab.id.clone(),
                        label: mapped.label.clone().unwrap_or_else(|| id.clone()),
                    });
                    self.holds.push(Hold {
                        tab: tab.id.clone(),
                        pad: edge.pad,
                        button,
                        action: id.clone(),
                        started: now,
                    });
                }
                bind => match message(bind, state) {
                    Ok(message) => out.push(Out::Send {
                        tab: tab.id.clone(),
                        message,
                    }),
                    Err(text) => out.push(Out::Notice {
                        tab: tab.id.clone(),
                        text,
                    }),
                },
            }
        }
        out
    }

    /// Holds that reached [`HOLD`] send their confirmed press.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        let mut out = Vec::new();
        let (done, held): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.holds)
            .into_iter()
            .partition(|h| now.duration_since(h.started) >= HOLD);
        self.holds = held;
        for h in done {
            out.push(Out::Send {
                tab: h.tab.clone(),
                message: json!({ "type": "press", "action": h.action, "confirmed": true }),
            });
            out.push(Out::HoldEnded { tab: h.tab });
        }
        out
    }

    /// Cancels a tab's holds, when its window closes or starts mapping.
    pub fn cancel_tab(&mut self, tab: &str) -> Vec<Out> {
        self.end_holds(|h| h.tab == tab)
    }

    pub fn holding(&self) -> bool {
        !self.holds.is_empty()
    }

    fn end_holds(&mut self, which: impl Fn(&Hold) -> bool) -> Vec<Out> {
        let mut out = Vec::new();
        let (ended, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.holds)
            .into_iter()
            .partition(|h| which(h));
        self.holds = kept;
        for h in ended {
            out.push(Out::Send {
                tab: h.tab.clone(),
                message: json!({ "type": "hold.end", "action": h.action }),
            });
            out.push(Out::HoldEnded { tab: h.tab });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bezel, Placement};

    const MFD: &str = "F16 MFD 1 (Vendor: 044f Product: b351)";

    fn pad(id: usize, name: &str, vendor: u16, product: u16) -> Pad {
        Pad {
            id,
            name: name.into(),
            vendor: Some(vendor),
            product: Some(product),
            standard: false,
        }
    }

    fn mapped(button: u32, bind: BezelBind) -> BezelButton {
        BezelButton {
            button,
            bind,
            label: None,
        }
    }

    fn state(gamepad_id: &str) -> DashState {
        DashState {
            tabs: vec![Tab {
                id: "mip".into(),
                name: "MIP".into(),
                placement: Some(Placement {
                    display_label: "MFD Left".into(),
                    fullscreen: true,
                }),
                bezel: Some(Bezel {
                    gamepad_id: gamepad_id.into(),
                    buttons: vec![
                        mapped(
                            0,
                            BezelBind::Action {
                                id: "v_lights".into(),
                            },
                        ),
                        mapped(
                            1,
                            BezelBind::Action {
                                id: "v_self_destruct".into(),
                            },
                        ),
                        mapped(
                            2,
                            BezelBind::Toggle {
                                id: "gear".into(),
                                to: Some("off".into()),
                            },
                        ),
                        mapped(
                            3,
                            BezelBind::Toggle {
                                id: "gear".into(),
                                to: None,
                            },
                        ),
                        mapped(4, BezelBind::Mode { to: "NAV".into() }),
                        mapped(
                            5,
                            BezelBind::Sequence {
                                id: "power-up".into(),
                            },
                        ),
                    ],
                }),
            }],
            confirm_group: vec!["v_self_destruct".into()],
            toggle_clicks: vec![(
                "gear".into(),
                json!({ "type": "set", "toggle": "gear", "to": "on" }),
            )],
            ..DashState::default()
        }
    }

    fn edge(pad: usize, raw: u32, pressed: bool) -> PadButton {
        PadButton {
            pad,
            raw: Some(raw),
            standard: None,
            pressed,
        }
    }

    fn engine() -> Engine {
        let mut e = Engine::default();
        e.connected(pad(0, "Xbox Controller", 0x045e, 0x0b12));
        e.connected(pad(3, "F16 MFD 1", 0x044f, 0xb351));
        e
    }

    fn sends(out: &[Out]) -> Vec<Value> {
        out.iter()
            .filter_map(|o| match o {
                Out::Send { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_browser_ids() {
        let chromium = parse_browser_id(MFD);
        assert_eq!(chromium.name, "F16 MFD 1");
        assert_eq!(
            (chromium.vendor, chromium.product),
            (Some(0x044f), Some(0xb351))
        );
        assert!(!chromium.standard);
        let std = parse_browser_id(
            "Xbox Wireless Controller (STANDARD GAMEPAD Vendor: 045e Product: 0b13)",
        );
        assert!(std.standard && !std.xinput);
        assert_eq!(std.product, Some(0x0b13));
        let xinput = parse_browser_id("Xbox 360 Controller (XInput STANDARD GAMEPAD)");
        assert!(xinput.xinput && xinput.standard);
        assert_eq!(xinput.vendor, None);
        let firefox = parse_browser_id("044f-b351-F16 MFD 1");
        assert_eq!(firefox.name, "F16 MFD 1");
        assert_eq!(firefox.vendor, Some(0x044f));
        let plain = parse_browser_id("My Box (rev 2)");
        assert_eq!(plain.name, "My Box");
        assert_eq!(plain.vendor, None);
    }

    #[test]
    fn matches_pads_by_id_then_name() {
        let pads = [
            pad(0, "Xbox Controller", 0x045e, 0x0b12),
            pad(3, "F16 MFD 1", 0x044f, 0xb351),
            pad(4, "F16 MFD 2", 0x044f, 0xb352),
        ];
        assert_eq!(match_pad(MFD, &pads), PadMatch::Id(3));
        assert_eq!(match_pad("044f-b352-F16 MFD 2", &pads), PadMatch::Id(4));
        // Ids that do not line up (another driver), the name still does.
        assert_eq!(
            match_pad("F16 MFD 2 (Vendor: 1234 Product: 5678)", &pads),
            PadMatch::Name(4)
        );
        assert_eq!(
            match_pad("Throttle (Vendor: 1234 Product: 5678)", &pads),
            PadMatch::None
        );
        let mut x = pads.to_vec();
        x[0].standard = true;
        assert_eq!(
            match_pad("Xbox 360 Controller (XInput STANDARD GAMEPAD)", &x),
            PadMatch::Id(0)
        );
    }

    #[test]
    fn two_of_one_model_split_by_name_or_not_at_all() {
        let pads = [
            pad(1, "MFD A", 0x044f, 0xb351),
            pad(2, "MFD B", 0x044f, 0xb351),
        ];
        assert_eq!(
            match_pad("MFD B (Vendor: 044f Product: b351)", &pads),
            PadMatch::Id(2)
        );
        assert_eq!(
            match_pad("MFD (Vendor: 044f Product: b351)", &pads),
            PadMatch::None
        );
    }

    #[test]
    fn presses_send_what_a_widget_click_sends() {
        let s = state(MFD);
        let active = vec!["mip".to_owned()];
        let mut e = engine();
        let now = Instant::now();
        let press = |e: &mut Engine, raw| sends(&e.button(&s, &active, &edge(3, raw, true), now));
        assert_eq!(
            press(&mut e, 0),
            vec![json!({ "type": "press", "action": "v_lights" })]
        );
        assert_eq!(
            press(&mut e, 2),
            vec![json!({ "type": "set", "toggle": "gear", "to": "off" })]
        );
        assert_eq!(
            press(&mut e, 3),
            vec![json!({ "type": "set", "toggle": "gear", "to": "on" })]
        );
        assert_eq!(
            press(&mut e, 4),
            vec![json!({ "type": "mode", "to": "NAV" })]
        );
        assert_eq!(
            press(&mut e, 5),
            vec![json!({ "type": "run", "sequence": "power-up" })]
        );
        assert!(
            press(&mut e, 9).is_empty(),
            "an unmapped button does nothing"
        );
    }

    #[test]
    fn other_pads_inactive_tabs_and_releases_do_nothing() {
        let s = state(MFD);
        let mut e = engine();
        let now = Instant::now();
        let active = vec!["mip".to_owned()];
        assert!(e.button(&s, &active, &edge(0, 0, true), now).is_empty());
        assert!(e.button(&s, &[], &edge(3, 0, true), now).is_empty());
        assert!(e.button(&s, &active, &edge(3, 0, false), now).is_empty());
        let missing = vec!["gone".to_owned()];
        assert!(e.button(&s, &missing, &edge(3, 0, true), now).is_empty());
    }

    #[test]
    fn toggle_without_data_is_a_notice() {
        let mut s = state(MFD);
        s.toggle_clicks.clear();
        let mut e = engine();
        let out = e.button(&s, &["mip".to_owned()], &edge(3, 3, true), Instant::now());
        assert_eq!(
            out,
            vec![Out::Notice {
                tab: "mip".into(),
                text: "gear: no data from the game".into()
            }]
        );
    }

    #[test]
    fn confirm_action_is_held_then_pressed_confirmed() {
        let s = state(MFD);
        let active = vec!["mip".to_owned()];
        let mut e = engine();
        let t0 = Instant::now();
        let out = e.button(&s, &active, &edge(3, 1, true), t0);
        assert_eq!(
            sends(&out),
            vec![json!({ "type": "hold.start", "action": "v_self_destruct" })]
        );
        assert!(out.contains(&Out::HoldStarted {
            tab: "mip".into(),
            label: "v_self_destruct".into()
        }));
        assert!(
            e.tick(t0 + Duration::from_millis(1500)).is_empty(),
            "not before 1.55 s"
        );
        let out = e.tick(t0 + HOLD);
        assert_eq!(
            sends(&out),
            vec![json!({ "type": "press", "action": "v_self_destruct", "confirmed": true })]
        );
        assert!(!e.holding());
        assert!(sends(&e.button(&s, &active, &edge(3, 1, false), t0 + HOLD)).is_empty());
    }

    #[test]
    fn early_release_cancels_the_hold() {
        let s = state(MFD);
        let active = vec!["mip".to_owned()];
        let mut e = engine();
        let t0 = Instant::now();
        e.button(&s, &active, &edge(3, 1, true), t0);
        let out = e.button(
            &s,
            &active,
            &edge(3, 1, false),
            t0 + Duration::from_millis(800),
        );
        assert_eq!(
            sends(&out),
            vec![json!({ "type": "hold.end", "action": "v_self_destruct" })]
        );
        assert!(e.tick(t0 + HOLD).is_empty());
    }

    #[test]
    fn closing_the_window_or_unplugging_cancels_the_hold() {
        let s = state(MFD);
        let active = vec!["mip".to_owned()];
        let mut e = engine();
        let t0 = Instant::now();
        e.button(&s, &active, &edge(3, 1, true), t0);
        assert_eq!(sends(&e.cancel_tab("mip")).len(), 1);
        e.button(&s, &active, &edge(3, 1, true), t0);
        assert_eq!(sends(&e.disconnected(3)).len(), 1);
        assert!(e.tick(t0 + HOLD).is_empty());
    }

    #[test]
    fn standard_mapping_uses_the_standard_index() {
        let s = state("Pad (STANDARD GAMEPAD Vendor: 045e Product: 0b12)");
        let mut e = engine();
        let active = vec!["mip".to_owned()];
        // Raw index 7 but standard index 4 (the left bumper): the mapping learned 4.
        let edge = PadButton {
            pad: 0,
            raw: Some(7),
            standard: Some(4),
            pressed: true,
        };
        assert_eq!(
            sends(&e.button(&s, &active, &edge, Instant::now())),
            vec![json!({ "type": "mode", "to": "NAV" })]
        );
    }

    #[test]
    fn name_only_match_warns_once() {
        let s = state("F16 MFD 1 (Vendor: 0001 Product: 0002)");
        let mut e = engine();
        let active = vec!["mip".to_owned()];
        let now = Instant::now();
        let first = e.button(&s, &active, &edge(3, 0, true), now);
        assert!(matches!(first[0], Out::Warn(_)));
        assert_eq!(sends(&first).len(), 1);
        let again = e.button(&s, &active, &edge(3, 0, true), now);
        assert!(!again.iter().any(|o| matches!(o, Out::Warn(_))));
    }
}
