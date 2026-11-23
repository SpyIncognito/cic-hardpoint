//! The parts of the dashboard the app acts on: each tab's placement and bezel mapping, as the
//! service pushes them (PROTOCOL.md section 2a). Read leniently: the service validates the file,
//! and a field the app does not know is ignored, so a newer service still works.

use std::sync::LazyLock;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The page's Fleet tab: the fleet view, detachable like a dashboard tab but not one, so it has
/// an id no dashboard tab can have, no placement and no bezel, and is on every dashboard.
pub const FLEET_TAB: &str = "@fleet";

static FLEET: LazyLock<Tab> = LazyLock::new(|| Tab {
    id: FLEET_TAB.to_owned(),
    name: "Fleet".to_owned(),
    placement: None,
    bezel: None,
});

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Tab {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub placement: Option<Placement>,
    #[serde(default)]
    pub bezel: Option<Bezel>,
}

/// The display by its name, never its number.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub display_label: String,
    pub fullscreen: bool,
}

/// One game controller per tab; `gamepad_id` is the browser Gamepad API's `id` string.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bezel {
    pub gamepad_id: String,
    #[serde(default, deserialize_with = "known_buttons")]
    pub buttons: Vec<BezelButton>,
}

/// A button the app cannot read (a bind kind it does not know) is left out, not the whole tab.
fn known_buttons<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<BezelButton>, D::Error> {
    let raw = Vec::<Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|b| serde_json::from_value(b).ok())
        .collect())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BezelButton {
    pub button: u32,
    pub bind: BezelBind,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BezelBind {
    Action {
        id: String,
    },
    Sequence {
        id: String,
    },
    Toggle {
        id: String,
        #[serde(default)]
        to: Option<String>,
    },
    Mode {
        to: String,
    },
}

/// What the app keeps from the `dashboard` and `values` pushes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashState {
    /// The tabs of the dashboard on screen.
    pub tabs: Vec<Tab>,
    /// The dashboard on screen, by id (empty from a service without per-ship dashboards).
    pub active: String,
    /// The generic dashboard's tabs while a ship's dashboard is on screen: a detached window
    /// keeps a generic tab when the ship's dashboard has none of its name.
    pub generic: Vec<Tab>,
    pub confirm_group: Vec<String>,
    /// Per toggle id, the page message a widget click sends (`values.toggles[id].click`).
    pub toggle_clicks: Vec<(String, Value)>,
}

impl DashState {
    /// A tab by id, on screen or kept from the generic dashboard. Ids are unique across
    /// dashboards.
    pub fn tab(&self, id: &str) -> Option<&Tab> {
        if id == FLEET_TAB {
            return Some(&FLEET);
        }
        self.tabs.iter().chain(&self.generic).find(|t| t.id == id)
    }

    /// The tab a detached window shows after the dashboard changed: the one of the same name on
    /// the new dashboard, else the generic one of that name, else its own if still there.
    pub fn follow(&self, old: &Tab) -> Option<&Tab> {
        if old.id == FLEET_TAB {
            return Some(&FLEET);
        }
        let named = |t: &&Tab| t.name == old.name;
        self.tabs
            .iter()
            .find(named)
            .or_else(|| self.generic.iter().find(named))
            .or_else(|| self.tab(&old.id))
    }

    /// The placed tabs to open: the dashboard's own, and the generic ones whose name it lacks.
    pub fn restorable(&self) -> Vec<&Tab> {
        let kept = self
            .generic
            .iter()
            .filter(|g| !self.tabs.iter().any(|t| t.name == g.name));
        self.tabs
            .iter()
            .chain(kept)
            .filter(|t| t.placement.is_some())
            .collect()
    }

    pub fn toggle_click(&self, id: &str) -> Option<&Value> {
        self.toggle_clicks
            .iter()
            .find(|(t, _)| t == id)
            .map(|(_, c)| c)
    }

    /// Takes a `dashboard` push; false when it is not one the app can read.
    pub fn apply_dashboard(&mut self, push: &Value) -> bool {
        let Some(tabs) = push.pointer("/dashboard/tabs").and_then(Value::as_array) else {
            return false;
        };
        // A tab the app cannot read is skipped, not fatal: the page still shows it.
        let read = |tabs: &[Value]| -> Vec<Tab> {
            tabs.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        };
        self.tabs = read(tabs);
        self.generic = push
            .pointer("/generic/tabs")
            .and_then(Value::as_array)
            .map(|t| read(t))
            .unwrap_or_default();
        self.active = push
            .pointer("/board/id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.confirm_group = push
            .get("confirmGroup")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        true
    }

    /// Takes a `values` push: only each toggle's `click` is kept.
    pub fn apply_values(&mut self, push: &Value) {
        let Some(toggles) = push.get("toggles").and_then(Value::as_object) else {
            return;
        };
        self.toggle_clicks = toggles
            .iter()
            .filter_map(|(id, t)| t.get("click").map(|c| (id.clone(), c.clone())))
            .collect();
    }

    pub fn is_confirm(&self, action: &str) -> bool {
        self.confirm_group.iter().any(|a| a == action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_placement_bezel_and_skips_unknown_fields_and_binds() {
        let mut s = DashState::default();
        let push = json!({
            "type": "dashboard",
            "dashboard": { "schemaVersion": 1, "tabs": [
                { "id": "main", "name": "Main", "rows": 14, "widgets": [] },
                { "id": "mip", "name": "MIP", "rows": 14, "widgets": [], "future": 1,
                  "placement": { "displayLabel": "MFD Left", "fullscreen": true },
                  "bezel": { "gamepadId": "F16 MFD 1 (Vendor: 044f Product: b351)", "buttons": [
                    { "button": 0, "bind": { "kind": "action", "id": "v_self_destruct" } },
                    { "button": 3, "bind": { "kind": "toggle", "id": "gear", "to": "on" }, "label": "Gear" },
                    { "button": 4, "bind": { "kind": "mode", "to": "SCM" } },
                    { "button": 5, "bind": { "kind": "sequence", "id": "power-up" } },
                    { "button": 6, "bind": { "kind": "keybind", "fn": "pips-engine-up" } }
                  ] } },
                { "id": 7 }
            ] },
            "confirmGroup": ["v_self_destruct"]
        });
        assert!(s.apply_dashboard(&push));
        assert_eq!(s.tabs.len(), 2, "the unreadable tab is skipped");
        let mip = s.tab("mip").unwrap();
        assert_eq!(mip.placement.as_ref().unwrap().display_label, "MFD Left");
        let bezel = mip.bezel.as_ref().unwrap();
        // The keybind bind (removed: the app is API only) is left out; the tab is kept.
        assert_eq!(bezel.buttons.len(), 4);
        assert_eq!(bezel.buttons[3].button, 5);
        assert_eq!(
            bezel.buttons[1].bind,
            BezelBind::Toggle {
                id: "gear".into(),
                to: Some("on".into())
            }
        );
        assert!(s.is_confirm("v_self_destruct"));
        assert!(!s.is_confirm("v_gear"));
    }

    #[test]
    fn the_fleet_tab_is_on_every_dashboard_and_never_follows_a_tab_named_fleet() {
        let mut s = DashState::default();
        assert_eq!(s.tab(FLEET_TAB).unwrap().name, "Fleet");
        let fleet = s.tab(FLEET_TAB).unwrap().clone();
        assert!(s.apply_dashboard(&json!({
            "dashboard": { "tabs": [tab("fleet", "Fleet", true)] },
            "board": { "id": "drak" },
        })));
        assert_eq!(s.follow(&fleet).unwrap().id, FLEET_TAB);
        assert_eq!(s.tab("fleet").unwrap().id, "fleet");
        let restore: Vec<&str> = s.restorable().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            restore,
            ["fleet"],
            "the Fleet tab has no remembered display"
        );
    }

    fn tab(id: &str, name: &str, placed: bool) -> Value {
        let mut t = json!({ "id": id, "name": name, "rows": 14, "widgets": [] });
        if placed {
            t["placement"] = json!({ "displayLabel": "MFD", "fullscreen": true });
        }
        t
    }

    #[test]
    fn detached_windows_follow_by_tab_name_and_keep_a_generic_tab_otherwise() {
        let generic = || {
            vec![
                tab("main", "Main", false),
                tab("mip", "MIP", true),
                tab("ufc", "UFC", true),
            ]
        };
        let mut s = DashState::default();
        assert!(s.apply_dashboard(
            &json!({ "dashboard": { "tabs": generic() }, "board": { "id": "generic" } })
        ));
        let mip = s.tab("mip").unwrap().clone();
        let ufc = s.tab("ufc").unwrap().clone();
        assert_eq!(s.restorable().len(), 2);
        // The Corsair's dashboard has a MIP but no UFC.
        assert!(s.apply_dashboard(&json!({
            "dashboard": { "tabs": [tab("drak-main", "Main", false), tab("drak-mip", "MIP", true)] },
            "board": { "id": "drak" },
            "generic": { "tabs": generic() },
        })));
        assert_eq!(s.active, "drak");
        assert_eq!(s.follow(&mip).unwrap().id, "drak-mip");
        assert_eq!(
            s.follow(&ufc).unwrap().id,
            "ufc",
            "kept from the generic dashboard"
        );
        let restore: Vec<&str> = s.restorable().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(restore, ["drak-mip", "ufc"]);
        // Back to the generic one: the Corsair's MIP window goes to the generic MIP.
        let drak_mip = s.tab("drak-mip").unwrap().clone();
        assert!(s.apply_dashboard(
            &json!({ "dashboard": { "tabs": generic() }, "board": { "id": "generic" } })
        ));
        assert_eq!(s.follow(&drak_mip).unwrap().id, "mip");
        assert!(s.follow(&tab_of("gone", "Gone")).is_none());
    }

    fn tab_of(id: &str, name: &str) -> Tab {
        serde_json::from_value(tab(id, name, false)).unwrap()
    }

    #[test]
    fn keeps_toggle_clicks_from_values() {
        let mut s = DashState::default();
        s.apply_values(&json!({ "type": "values", "toggles": {
            "gear": { "id": "gear", "click": { "type": "set", "toggle": "gear", "to": "off" } },
            "lights": { "id": "lights" }
        } }));
        assert_eq!(s.toggle_click("gear").unwrap()["to"], "off");
        assert!(s.toggle_click("lights").is_none());
    }
}
