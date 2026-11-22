//! The TS dashboard.test.ts validation and store cases, against the port.

use serde_json::{Value, json};

use super::*;

fn known() -> Known {
    Known {
        actions: Some(
            ["v_lights", "v_power_set_off", "v_eject"]
                .map(String::from)
                .into(),
        ),
        sequences: Some(["landing_prep".to_owned()].into()),
        themes: Some(["rsi", "drake", "theme-1"].map(String::from).into()),
    }
}

fn lamp(id: &str, x: i64, y: i64, extra: Value) -> Value {
    let mut w = json!({ "id": id, "kind": "lamp", "x": x, "y": y, "w": 2, "h": 1, "bind": { "kind": "value", "id": "caution.pwr" }, "look": "square" });
    w.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    w
}

fn tab(widgets: Vec<Value>, extra: Value) -> Value {
    let mut t = json!({ "id": "test", "name": "Test", "rows": 14, "widgets": widgets });
    t.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    t
}

fn strict(t: &Value) -> String {
    check_tab(t, &known(), true).problems.join(",")
}

#[test]
fn accepts_the_starter_tab_and_every_kind_with_a_good_bind() {
    assert_eq!(strict(&crate::json::to_js_value(&starter_tab())), "");
    let all = vec![
        json!({ "id": "a", "kind": "action", "x": 0, "y": 0, "w": 2, "h": 2, "bind": { "kind": "action", "id": "v_lights" }, "look": "round" }),
        json!({ "id": "b", "kind": "sequence", "x": 2, "y": 0, "w": 2, "h": 2, "bind": { "kind": "sequence", "id": "landing_prep" }, "look": "square" }),
        json!({ "id": "c", "kind": "readout", "x": 4, "y": 0, "w": 3, "h": 2, "bind": { "kind": "value", "id": "actor.oxygen" }, "look": "bar", "label": "O2 & air" }),
        json!({ "id": "d", "kind": "lamp", "x": 7, "y": 0, "w": 1, "h": 1, "bind": { "kind": "value", "id": "power.engines" }, "look": "round" }),
    ];
    assert_eq!(strict(&tab(all, json!({}))), "");
}

#[test]
fn refuses_binds_not_on_the_known_lists_and_keybinds() {
    let bad = |bind: Value, kind: &str, look: &str| {
        strict(&tab(
            vec![
                json!({ "id": "x", "kind": kind, "x": 0, "y": 0, "w": 2, "h": 2, "bind": bind, "look": look }),
            ],
            json!({}),
        ))
    };
    assert!(
        bad(
            json!({ "kind": "value", "id": "ship.warp_core" }),
            "lamp",
            "square"
        )
        .contains("unknown value \"ship.warp_core\"")
    );
    assert!(
        bad(
            json!({ "kind": "value", "id": "ship.speed" }),
            "lamp",
            "square"
        )
        .contains("a lamp shows a caution or an on/off value")
    );
    assert!(
        bad(json!({ "kind": "toggle", "id": "cloak" }), "lamp", "square")
            .contains("unknown toggle")
    );
    assert!(
        bad(
            json!({ "kind": "action", "id": "v_self_destruct_now" }),
            "action",
            "square"
        )
        .contains("unknown game action")
    );
    assert!(
        bad(
            json!({ "kind": "action", "id": "javascript:alert(1)" }),
            "action",
            "square"
        )
        .contains("unknown game action")
    );
    assert!(
        bad(
            json!({ "kind": "sequence", "id": "nope" }),
            "sequence",
            "square"
        )
        .contains("unknown sequence")
    );
    assert!(
        bad(
            json!({ "kind": "keybind", "fn": "pips-engine-up" }),
            "action",
            "square"
        )
        .contains("cannot bind to \"keybind\"")
    );
    assert!(
        bad(
            json!({ "kind": "action", "id": "v_lights" }),
            "readout",
            "number"
        )
        .contains("a readout cannot bind to \"action\"")
    );
    assert!(
        bad(
            json!({ "kind": "value", "id": "caution.pwr", "url": "http://x" }),
            "lamp",
            "square"
        )
        .contains("bind field \"url\" is not allowed")
    );
}

#[test]
fn keeps_widgets_inside_the_grid_and_apart() {
    let one = |w: Value| strict(&tab(vec![w], json!({})));
    assert!(one(lamp("a", 23, 0, json!({}))).contains("does not fit the 24 x 14 grid"));
    assert!(one(lamp("a", 0, 14, json!({}))).contains("does not fit"));
    assert!(one(lamp("a", -1, 0, json!({}))).contains("does not fit"));
    assert!(one(lamp("a", 0, 0, json!({ "w": 1.5 }))).contains("does not fit"));
    assert!(
        strict(&tab(
            vec![lamp("a", 0, 0, json!({})), lamp("b", 1, 0, json!({}))],
            json!({})
        ))
        .contains("overlaps")
    );
    assert!(
        strict(&tab(
            vec![lamp("a", 0, 0, json!({})), lamp("a", 4, 0, json!({}))],
            json!({})
        ))
        .contains("used twice")
    );
    assert!(strict(&tab(vec![], json!({ "rows": 100 }))).contains("rows must be"));
}

#[test]
fn caps_widgets_per_tab_and_tabs_per_file() {
    let many: Vec<Value> = (0..MAX_WIDGETS as i64 + 5)
        .map(|i| lamp(&format!("w{i}"), (i % 12) * 2, i / 12, json!({})))
        .collect();
    assert!(strict(&tab(many.clone(), json!({ "rows": 48 }))).contains("at most 200 widgets"));
    let lenient = check_tab(&tab(many, json!({ "rows": 48 })), &known(), false);
    assert_eq!(lenient.tab.unwrap().widgets.len(), MAX_WIDGETS);
    assert!(
        lenient
            .skipped
            .join(",")
            .contains("5 widgets over the cap of 200 left out")
    );

    let tabs: Vec<Value> = (0..MAX_TABS + 3).map(|i| json!({ "id": format!("t{i}"), "name": format!("T {i}"), "rows": 14, "widgets": [] })).collect();
    let (dashboard, summary) =
        read_dashboard(Some(&json!({ "schemaVersion": 1, "tabs": tabs })), &known());
    assert_eq!(dashboard.unwrap().tabs.len(), MAX_TABS);
    assert!(
        summary
            .skipped
            .contains(&"3 tabs over the cap of 20 left out".to_owned())
    );
}

#[test]
fn takes_plain_text_only_and_nothing_beyond_the_known_fields() {
    let one = |w: Value, extra: Value| strict(&tab(vec![w], extra));
    assert!(
        one(
            lamp(
                "a",
                0,
                0,
                json!({ "label": "<img src=x onerror=alert(1)>" })
            ),
            json!({})
        )
        .contains("plain text")
    );
    assert!(
        one(
            lamp("a", 0, 0, json!({ "label": "x".repeat(41) })),
            json!({})
        )
        .contains("at most 40")
    );
    assert!(strict(&tab(vec![], json!({ "name": "<b>Main</b>" }))).contains("plain text"));
    assert!(
        one(lamp("a", 0, 0, json!({ "style": "color:red" })), json!({}))
            .contains("field \"style\" is not allowed")
    );
    assert!(
        strict(&tab(vec![], json!({ "svg": "<svg/>" }))).contains("field \"svg\" is not allowed")
    );
    assert!(
        one(
            lamp("a", 0, 0, json!({ "look": "url(http://x)" })),
            json!({})
        )
        .contains("is not one of square, round")
    );

    let lenient = check_tab(
        &tab(
            vec![lamp("a", 0, 0, json!({ "style": "color:red" }))],
            json!({ "css": "x" }),
        ),
        &known(),
        false,
    );
    let text = crate::json::to_text(&lenient.tab.unwrap());
    assert!(!text.contains("style") && !text.contains("css"));
    assert_eq!(
        lenient.skipped,
        [
            "Tab \"Test\": ignored field \"css\"",
            "Tab \"Test\", widget 1 (\"a\"): ignored \"style\""
        ]
    );
}

#[test]
fn imports_a_newer_or_odd_file_leniently() {
    let file = json!({
        "schemaVersion": 3,
        "theme": "neon",
        "tabs": [
            tab(vec![
                lamp("ok", 0, 0, json!({})),
                json!({ "id": "faces", "kind": "shieldFaces", "x": 0, "y": 2, "w": 6, "h": 6, "bind": { "kind": "value", "id": "ship.shields" }, "look": "outline" }),
                lamp("evil", 4, 0, json!({ "label": "<script>alert(1)</script>" })),
                json!({ "id": "off", "kind": "action", "x": 6, "y": 0, "w": 2, "h": 2, "bind": { "kind": "action", "id": "v_power_set_off" }, "look": "square" }),
                json!({ "id": "eject", "kind": "action", "x": 8, "y": 0, "w": 2, "h": 2, "bind": { "kind": "action", "id": "v_eject" }, "look": "round" }),
            ], json!({})),
            { "id": "broken", "name": "", "widgets": [] },
        ],
    });
    let (dashboard, summary) = read_dashboard(Some(&file), &known());
    assert_eq!(
        crate::json::to_js_value(&summary),
        json!({
            "ok": true, "tabs": 1, "widgets": 3,
            "skipped": [
                "Dashboard theme left out, unknown theme \"neon\"",
                "Tab \"Test\", widget 2 (\"faces\"): skipped, unknown widget kind \"shieldFaces\"",
                "Tab \"Test\", widget 3 (\"evil\"): skipped, its label must be plain text (no < or >)",
                "Tab \"broken\": the name must be 1 to 40 characters; tab skipped",
            ],
            "unknownKinds": ["shieldFaces"], "confirmButtons": 2,
            "bezelButtons": 0, "confirmBezelButtons": 0, "newerVersion": 3,
        })
    );
    let dashboard = dashboard.unwrap();
    let ids: Vec<&str> = dashboard.tabs[0]
        .widgets
        .iter()
        .map(|w| w.id.as_str())
        .collect();
    assert_eq!(ids, ["ok", "off", "eject"]);
    let text = crate::json::to_text(&dashboard);
    assert!(!text.contains("script") && !text.contains("neon") && !text.contains("shieldFaces"));
}

#[test]
fn refuses_what_is_not_a_dashboard_file() {
    let read = |v: Value| read_dashboard(Some(&v), &known()).1;
    assert!(
        read(json!("hello"))
            .problem
            .unwrap()
            .contains("not a dashboard file")
    );
    assert!(!read(json!({ "tabs": [] })).ok);
    assert!(
        read(json!({ "schemaVersion": 1, "tabs": [{ "id": "x" }] }))
            .problem
            .unwrap()
            .contains("No tab")
    );
    assert!(
        read_dashboard(None, &known())
            .1
            .problem
            .unwrap()
            .contains("not a dashboard file")
    );
}

#[test]
fn persists_every_change_and_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let a = DashboardStore::new(Some(file.clone()));
    assert_eq!(
        a.get()
            .tabs
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["main"]
    );
    let added = a.add_tab(Some(&json!("Combat"))).unwrap().tab.unwrap();
    let mut saved = crate::json::to_js_value(&added);
    saved["widgets"] = json!([lamp("l1", 0, 0, json!({}))]);
    assert_eq!(a.save_tab(&saved, &known()).unwrap(), Vec::<String>::new());
    let copy = a.duplicate_tab("main").unwrap().tab.unwrap();
    assert_eq!(
        (copy.id.as_str(), copy.name.as_str()),
        ("main-copy", "Main copy")
    );
    assert!(
        a.move_tab(&added.id, Some(&json!(0)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(!dir.path().join("dashboards.json.tmp").exists());

    let b = DashboardStore::new(Some(file.clone()));
    assert_eq!(b.get(), a.get());
    assert_eq!(
        b.get()
            .tabs
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Combat", "Main", "Main copy"]
    );
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["schemaVersion"], 2);
    assert_eq!(written["dashboards"][0]["id"], "generic");
}

#[test]
fn refuses_a_bad_save_keeps_one_tab_and_resets_the_starter() {
    let s = DashboardStore::default();
    let main = crate::json::to_js_value(&s.tab("main").unwrap());
    let mut bad = main.clone();
    bad["widgets"] = json!([lamp("a", 30, 0, json!({}))]);
    assert!(!s.save_tab(&bad, &known()).unwrap().is_empty());
    assert_eq!(
        s.remove_tab("main").unwrap().problems,
        ["The last tab cannot be removed"]
    );
    let mut mine = main;
    mine["name"] = json!("Mine");
    mine["widgets"] = json!([]);
    assert!(s.save_tab(&mine, &known()).unwrap().is_empty());
    s.reset_starter().unwrap();
    assert_eq!(s.tab("main").unwrap(), starter_tab());
    s.add_tab(None).unwrap();
    assert!(s.remove_tab("main").unwrap().problems.is_empty());
    s.reset_starter().unwrap();
    assert_eq!(s.get().tabs[0].id, "main");
}

#[test]
fn stops_at_the_tab_cap() {
    let s = DashboardStore::default();
    for _ in 1..MAX_TABS {
        assert!(s.add_tab(None).unwrap().problems.is_empty());
    }
    assert_eq!(s.add_tab(None).unwrap().problems, ["At most 20 tabs"]);
    assert_eq!(
        s.duplicate_tab("main").unwrap().problems,
        ["At most 20 tabs"]
    );
}

#[test]
fn the_kind_table_is_what_the_page_gets() {
    let kinds = crate::json::to_js_value(&KindTable);
    assert_eq!(
        kinds["lamp"],
        json!({ "name": "Lamp", "looks": ["square", "round"], "binds": ["value", "toggle"], "size": { "w": 2, "h": 1 }, "min": { "w": 1, "h": 1 } })
    );
    assert_eq!(
        kinds["shields"],
        json!({ "name": "Shield faces", "looks": ["outline"], "binds": ["shields"], "size": { "w": 6, "h": 6 }, "min": { "w": 6, "h": 6 } })
    );
    assert_eq!(kinds["power"]["looks"], json!(["columns"]));
    assert_eq!(
        kinds["toggle"]["looks"],
        json!(["button", "switch", "lever"])
    );
    assert_eq!(
        kinds["sequence"]["looks"],
        json!(["push", "square", "round"])
    );
    let names: Vec<&String> = kinds.as_object().unwrap().keys().collect();
    assert_eq!(
        names,
        [
            "lamp", "readout", "toggle", "action", "sequence", "mode", "status", "shields",
            "power", "weapons", "fuel", "config"
        ]
    );
}

// ---------------------------------------------------------------- D2 kinds and looks

#[test]
fn accepts_the_d2_kinds_and_looks_with_their_own_binds() {
    let d2 = vec![
        json!({ "id": "sh", "kind": "shields", "x": 0, "y": 0, "w": 6, "h": 6, "bind": { "kind": "shields" }, "look": "outline" }),
        json!({ "id": "pw", "kind": "power", "x": 6, "y": 0, "w": 6, "h": 4, "bind": { "kind": "power" }, "look": "columns" }),
        json!({ "id": "wp", "kind": "weapons", "x": 12, "y": 0, "w": 6, "h": 4, "bind": { "kind": "weapons" }, "look": "list" }),
        json!({ "id": "fu", "kind": "fuel", "x": 18, "y": 0, "w": 2, "h": 3, "bind": { "kind": "fuel" }, "look": "bars" }),
        json!({ "id": "cf", "kind": "config", "x": 20, "y": 0, "w": 4, "h": 4, "bind": { "kind": "config" }, "look": "list" }),
        json!({ "id": "gear", "kind": "toggle", "x": 6, "y": 4, "w": 2, "h": 4, "bind": { "kind": "toggle", "id": "gear" }, "look": "lever" }),
        json!({ "id": "sw", "kind": "toggle", "x": 8, "y": 4, "w": 2, "h": 2, "bind": { "kind": "toggle", "id": "lights", "invert": true }, "look": "switch" }),
        json!({ "id": "pb", "kind": "sequence", "x": 10, "y": 4, "w": 2, "h": 2, "bind": { "kind": "sequence", "id": "landing_prep" }, "look": "push" }),
        json!({ "id": "rb", "kind": "action", "x": 12, "y": 4, "w": 2, "h": 2, "bind": { "kind": "action", "id": "v_lights" }, "look": "round" }),
    ];
    let t = tab(d2, json!({}));
    assert_eq!(strict(&t), "");
    let saved = crate::json::to_js_value(&check_tab(&t, &known(), true).tab.unwrap());
    assert_eq!(saved, t);
}

#[test]
fn keeps_the_d2_kinds_to_their_minimum_sizes_binds_and_looks() {
    let one = |w: Value| {
        let mut base = json!({ "id": "x", "x": 0, "y": 0 });
        base.as_object_mut()
            .unwrap()
            .extend(w.as_object().unwrap().clone());
        strict(&tab(vec![base], json!({})))
    };
    let cases = [
        (
            json!({ "kind": "shields", "w": 5, "h": 6, "bind": { "kind": "shields" }, "look": "outline" }),
            "the shield faces widget is at least 6 x 6 cells",
        ),
        (
            json!({ "kind": "power", "w": 6, "h": 3, "bind": { "kind": "power" }, "look": "columns" }),
            "the power widget is at least 6 x 4 cells",
        ),
        (
            json!({ "kind": "weapons", "w": 5, "h": 4, "bind": { "kind": "weapons" }, "look": "list" }),
            "at least 6 x 4",
        ),
        (
            json!({ "kind": "shields", "w": 6, "h": 6, "bind": { "kind": "power" }, "look": "outline" }),
            "cannot bind to \"power\"",
        ),
        (
            json!({ "kind": "weapons", "w": 6, "h": 4, "bind": { "kind": "value", "id": "weapons.preset" }, "look": "list" }),
            "cannot bind to \"value\"",
        ),
        (
            json!({ "kind": "fuel", "w": 2, "h": 3, "bind": { "kind": "fuel" }, "look": "outline" }),
            "is not one of bars",
        ),
        (
            json!({ "kind": "toggle", "w": 2, "h": 4, "bind": { "kind": "toggle", "id": "vtol" }, "look": "lever" }),
            "the lever look is for the gear toggle only",
        ),
        (
            json!({ "kind": "action", "w": 2, "h": 2, "bind": { "kind": "action", "id": "v_lights" }, "look": "push" }),
            "is not one of square, round",
        ),
        (
            json!({ "kind": "toggle", "w": 2, "h": 2, "bind": { "kind": "toggle", "id": "gear" }, "look": "constructor" }),
            "is not one of button, switch, lever",
        ),
        (
            json!({ "kind": "config", "w": 4, "h": 4, "bind": { "kind": "config", "url": "x" }, "look": "list" }),
            "bind field \"url\" is not allowed",
        ),
    ];
    for (w, want) in cases {
        let got = one(w);
        assert!(got.contains(want), "{got} lacks {want}");
    }

    let file = json!({ "schemaVersion": 1, "tabs": [tab(vec![
        json!({ "id": "ok", "kind": "toggle", "x": 0, "y": 0, "w": 2, "h": 4, "bind": { "kind": "toggle", "id": "gear" }, "look": "lever" }),
        json!({ "id": "bad", "kind": "toggle", "x": 2, "y": 0, "w": 2, "h": 4, "bind": { "kind": "toggle", "id": "vtol" }, "look": "lever" }),
    ], json!({}))] });
    let (dashboard, summary) = read_dashboard(Some(&file), &known());
    let ids: Vec<String> = dashboard.unwrap().tabs[0]
        .widgets
        .iter()
        .map(|w| w.id.clone())
        .collect();
    assert_eq!(ids, ["ok"]);
    assert_eq!(
        summary.skipped,
        ["Tab \"Test\", widget 2 (\"bad\"): skipped, the lever look is for the gear toggle only"]
    );
}

// ---------------------------------------------------------------- D3 placement and bezel

const PAD: &str = "Winwing MIP left (Vendor: 4098 Product: be62)";

fn btn(button: Value, bind: Value, extra: Value) -> Value {
    let mut b = json!({ "button": button, "bind": bind });
    b.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    b
}

fn bezel_problems(bezel: &Value) -> String {
    check_bezel(Some(bezel), &known(), true).problems.join(",")
}

#[test]
fn keeps_a_placement_by_display_name_only_as_plain_text_within_the_cap() {
    let p = |v: Value| check_placement(Some(&v));
    let good = p(json!({ "displayLabel": " DELL U2720Q ", "fullscreen": true }));
    assert_eq!(
        good.placement,
        Some(Placement {
            display_label: "DELL U2720Q".into(),
            fullscreen: true
        })
    );
    let problems = |v: Value| p(v).problems.join(",");
    let long = "x".repeat(101);
    assert_eq!(
        problems(json!({ "displayLabel": "", "fullscreen": true })),
        "the display name must be 1 to 100 characters"
    );
    assert_eq!(
        problems(json!({ "displayLabel": long, "fullscreen": true })),
        "the display name must be 1 to 100 characters"
    );
    assert_eq!(
        problems(json!({ "displayLabel": "<img src=x>", "fullscreen": true })),
        "the display name must be plain text (no < or >)"
    );
    assert_eq!(
        problems(json!({ "displayLabel": "A\u{7}B", "fullscreen": true })),
        "the display name must be plain text (no < or >)"
    );
    assert_eq!(
        problems(json!({ "displayLabel": "MIP", "fullscreen": "yes" })),
        "full screen must be true or false"
    );
    assert_eq!(
        problems(json!("MIP")),
        "the placement is not a display placement"
    );
    assert_eq!(
        strict(&tab(
            vec![],
            json!({ "placement": { "displayLabel": "MIP", "fullscreen": true, "index": 2 } })
        )),
        "Tab \"Test\": placement field \"index\" is not allowed"
    );
    assert_eq!(
        strict(&tab(
            vec![],
            json!({ "placement": { "displayLabel": "MIP", "fullscreen": false } })
        )),
        ""
    );
}

#[test]
fn maps_bezel_buttons_to_known_binds_only() {
    let good = check_bezel(
        Some(&json!({ "gamepadId": PAD, "buttons": [
            btn(json!(0), json!({ "kind": "toggle", "id": "gear" }), json!({})),
            btn(json!(1), json!({ "kind": "toggle", "id": "lights", "to": "on" }), json!({ "label": " Lights on " })),
            btn(json!(2), json!({ "kind": "mode", "to": "NAV" }), json!({})),
            btn(json!(3), json!({ "kind": "sequence", "id": "landing_prep" }), json!({})),
            btn(json!(4), json!({ "kind": "action", "id": "v_power_set_off" }), json!({})),
        ] })),
        &known(),
        true,
    );
    assert_eq!(good.problems, Vec::<String>::new());
    assert_eq!(
        crate::json::to_js_value(&good.bezel.unwrap()),
        json!({ "gamepadId": PAD, "buttons": [
            { "button": 0, "bind": { "kind": "toggle", "id": "gear" } },
            { "button": 1, "bind": { "kind": "toggle", "id": "lights", "to": "on" }, "label": "Lights on" },
            { "button": 2, "bind": { "kind": "mode", "to": "NAV" } },
            { "button": 3, "bind": { "kind": "sequence", "id": "landing_prep" } },
            { "button": 4, "bind": { "kind": "action", "id": "v_power_set_off" } },
        ] })
    );
    let bad = |bind: Value| {
        bezel_problems(&json!({ "gamepadId": PAD, "buttons": [btn(json!(0), bind, json!({}))] }))
    };
    let cases = [
        (
            json!({ "kind": "action", "id": "v_self_destruct_now" }),
            "button 1 (index 0): unknown game action \"v_self_destruct_now\"",
        ),
        (
            json!({ "kind": "sequence", "id": "nope" }),
            "button 1 (index 0): unknown sequence \"nope\"",
        ),
        (
            json!({ "kind": "toggle", "id": "cloak" }),
            "button 1 (index 0): unknown toggle \"cloak\"",
        ),
        (
            json!({ "kind": "toggle", "id": "gear", "to": "up" }),
            "button 1 (index 0): to must be on or off, on a toggle",
        ),
        (
            json!({ "kind": "action", "id": "v_lights", "to": "on" }),
            "button 1 (index 0): to must be on or off, on a toggle",
        ),
        (
            json!({ "kind": "mode", "to": "QT" }),
            "button 1 (index 0): master mode must be set to NAV or SCM",
        ),
        (
            json!({ "kind": "mode", "to": "NAV", "id": "x" }),
            "button 1 (index 0): a master mode bind has no id",
        ),
        (
            json!({ "kind": "value", "id": "ship.speed" }),
            "button 1 (index 0): a bezel button cannot bind to \"value\"",
        ),
        (
            json!({ "kind": "shields" }),
            "button 1 (index 0): a bezel button cannot bind to \"shields\"",
        ),
        (
            json!({ "kind": "keybind", "fn": "pips-reset" }),
            "button 1 (index 0): the keybind bind was removed, the app is API only (keybinds stay on the Stream Deck plugin)",
        ),
        (
            json!({ "kind": "action", "id": "v_lights", "url": "x" }),
            "button 1 (index 0): bind field \"url\" is not allowed",
        ),
        (
            json!({ "kind": "action" }),
            "button 1 (index 0): the bind has no id",
        ),
    ];
    for (bind, want) in cases {
        assert_eq!(bad(bind), want);
    }
}

#[test]
fn keeps_button_indices_in_bounds_and_unique_and_checks_the_controller_id() {
    let lights = || json!({ "kind": "action", "id": "v_lights" });
    let one = |b: Value| bezel_problems(&json!({ "gamepadId": PAD, "buttons": [b] }));
    let range = "the button index must be a whole number from 0 to 127";
    assert_eq!(
        one(btn(json!(-1), lights(), json!({}))),
        format!("button 1 (index -1): {range}")
    );
    assert_eq!(
        one(btn(json!(128), lights(), json!({}))),
        format!("button 1 (index 128): {range}")
    );
    assert_eq!(
        one(btn(json!(1.5), lights(), json!({}))),
        format!("button 1: {range}")
    );
    assert_eq!(
        bezel_problems(
            &json!({ "gamepadId": PAD, "buttons": [btn(json!(3), lights(), json!({})), btn(json!(3), lights(), json!({}))] })
        ),
        "button 2 (index 3): its index is mapped twice"
    );
    assert_eq!(
        one(btn(json!(3), lights(), json!({ "label": "<b>x</b>" }))),
        "button 1 (index 3): its label must be plain text (no < or >)"
    );
    assert_eq!(
        one(btn(json!(3), lights(), json!({ "onpress": "x" }))),
        "button 1 (index 3): field \"onpress\" is not allowed"
    );
    assert_eq!(one(json!("x")), "button 1: it is not a button");
    let id = "the controller id must be 1 to 200 characters";
    assert_eq!(
        bezel_problems(&json!({ "gamepadId": "", "buttons": [] })),
        id
    );
    assert_eq!(
        bezel_problems(&json!({ "gamepadId": "x".repeat(201), "buttons": [] })),
        id
    );
    assert_eq!(
        bezel_problems(&json!({ "gamepadId": "<script>", "buttons": [] })),
        "the controller id must be plain text (no < or >)"
    );
    assert_eq!(
        bezel_problems(&json!({ "gamepadId": PAD })),
        "it has no button list"
    );
    assert_eq!(
        bezel_problems(&json!(null)),
        "the bezel mapping is not a mapping"
    );
    assert_eq!(
        bezel_problems(&json!({ "gamepadId": PAD, "buttons": [], "x": 1 })),
        "field \"x\" is not allowed"
    );
    let many: Vec<Value> = (0..66)
        .map(|i| btn(json!(i), lights(), json!({})))
        .collect();
    let many = json!({ "gamepadId": PAD, "buttons": many });
    assert_eq!(bezel_problems(&many), "at most 64 bezel buttons");
    let lenient = check_bezel(Some(&many), &known(), false);
    assert_eq!(lenient.bezel.unwrap().buttons.len(), MAX_BEZEL_BUTTONS);
    assert_eq!(lenient.skipped, ["2 buttons over the cap of 64 left out"]);
}

#[test]
fn imports_placements_and_mappings_leniently_and_counts_bezel_buttons_and_holds() {
    let action = |id: &str| json!({ "kind": "action", "id": id });
    let file = json!({ "schemaVersion": 1, "tabs": [
        tab(vec![], json!({ "id": "a", "name": "A", "placement": { "displayLabel": "MIP left", "fullscreen": true, "index": 1 }, "bezel": { "gamepadId": PAD, "extra": 1, "buttons": [
            btn(json!(0), action("v_power_set_off"), json!({})),
            btn(json!(1), action("v_eject"), json!({ "colour": "red" })),
            btn(json!(2), action("v_lights"), json!({})),
            btn(json!(3), action("v_not_real"), json!({})),
            btn(json!(200), action("v_lights"), json!({})),
        ] } })),
        tab(vec![], json!({ "id": "b", "name": "B", "placement": { "displayLabel": "<b>", "fullscreen": true }, "bezel": { "gamepadId": 7, "buttons": [] } })),
    ] });
    let (dashboard, summary) = read_dashboard(Some(&file), &known());
    assert_eq!(
        (
            summary.ok,
            summary.tabs,
            summary.bezel_buttons,
            summary.confirm_bezel_buttons
        ),
        (true, 2, 3, 2)
    );
    assert_eq!(
        summary.skipped,
        [
            "Tab \"A\": placement ignored \"index\"",
            "Tab \"A\", bezel ignored field \"extra\"",
            "Tab \"A\", bezel button 2 (index 1): ignored \"colour\"",
            "Tab \"A\", bezel button 4 (index 3): skipped, unknown game action \"v_not_real\"",
            "Tab \"A\", bezel button 5 (index 200): skipped, the button index must be a whole number from 0 to 127",
            "Tab \"B\": display placement left out, the display name must be plain text (no < or >)",
            "Tab \"B\": bezel mapping left out, the controller id must be 1 to 200 characters",
        ]
    );
    let tabs = dashboard.unwrap().tabs;
    assert_eq!(
        tabs[0].placement.as_ref().unwrap().display_label,
        "MIP left"
    );
    assert_eq!(tabs[0].bezel.as_ref().unwrap().buttons.len(), 3);
    assert_eq!((&tabs[1].placement, &tabs[1].bezel), (&None, &None));
}

#[test]
fn sets_and_clears_placement_and_bezel_and_a_layout_save_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let store = DashboardStore::new(Some(file.clone()));
    let mip = json!({ "displayLabel": "MIP left", "fullscreen": true });
    assert!(
        store
            .set_placement("main", Some(&mip))
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(
        store
            .set_placement(
                "main",
                Some(&json!({ "displayLabel": "MIP left", "fullscreen": true, "index": 1 }))
            )
            .unwrap()
            .problems,
        ["placement field \"index\" is not allowed"]
    );
    assert_eq!(
        store
            .set_placement("nope", Some(&Value::Null))
            .unwrap()
            .problems,
        ["No tab \"nope\""]
    );
    let eject = json!({ "gamepadId": PAD, "buttons": [btn(json!(4), json!({ "kind": "action", "id": "v_eject" }), json!({}))] });
    assert!(
        store
            .set_bezel("main", Some(&eject), &known())
            .unwrap()
            .problems
            .is_empty()
    );
    let nope = json!({ "gamepadId": PAD, "buttons": [btn(json!(4), json!({ "kind": "action", "id": "v_nope" }), json!({}))] });
    assert_eq!(
        store
            .set_bezel("main", Some(&nope), &known())
            .unwrap()
            .problems,
        ["button 1 (index 4): unknown game action \"v_nope\""]
    );
    // A stale layout save from another window does not undo the mapping or the display.
    let mut stale = crate::json::to_js_value(&starter_tab());
    stale["name"] = json!("Renamed");
    stale["bezel"] = json!({ "gamepadId": "old", "buttons": [] });
    assert_eq!(
        store.save_tab(&stale, &known()).unwrap(),
        Vec::<String>::new()
    );
    let reread = DashboardStore::new(Some(file)).tab("main").unwrap();
    assert_eq!(reread.name, "Renamed");
    assert_eq!(reread.placement.as_ref().unwrap().display_label, "MIP left");
    assert_eq!(reread.bezel.as_ref().unwrap().gamepad_id, PAD);
    let copy = store.duplicate_tab("main").unwrap().tab.unwrap();
    assert_eq!(copy.placement, None);
    assert_eq!(copy.bezel.unwrap().buttons.len(), 1);
    store.set_placement("main", Some(&Value::Null)).unwrap();
    store
        .set_bezel("main", Some(&Value::Null), &known())
        .unwrap();
    let main = store.tab("main").unwrap();
    assert_eq!((main.placement, main.bezel), (None, None));
}

#[test]
fn writes_a_placement_set_after_the_bezel_after_it_as_the_ts_object_does() {
    let store = DashboardStore::default();
    let bezel = json!({ "gamepadId": PAD, "buttons": [] });
    let mip = json!({ "displayLabel": "MIP", "fullscreen": false });
    let keys = |store: &DashboardStore| -> Vec<String> {
        crate::json::to_js_value(&store.tab("main").unwrap())
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    store.set_bezel("main", Some(&bezel), &known()).unwrap();
    store.set_placement("main", Some(&mip)).unwrap();
    assert_eq!(
        keys(&store),
        ["id", "name", "rows", "widgets", "bezel", "placement"]
    );
    store
        .set_bezel("main", Some(&Value::Null), &known())
        .unwrap();
    store.set_bezel("main", Some(&bezel), &known()).unwrap();
    assert_eq!(
        keys(&store),
        ["id", "name", "rows", "widgets", "placement", "bezel"]
    );
    store.set_placement("main", Some(&Value::Null)).unwrap();
    store.set_placement("main", Some(&mip)).unwrap();
    let saved = crate::json::to_js_value(&store.tab("main").unwrap());
    assert_eq!(
        keys(&store),
        ["id", "name", "rows", "widgets", "bezel", "placement"]
    );
    store.save_tab(&saved, &known()).unwrap();
    assert_eq!(
        keys(&store),
        ["id", "name", "rows", "widgets", "placement", "bezel"]
    );
}

const GONE: &str = "was removed, the app is API only (keybinds stay on the Stream Deck plugin)";

#[test]
fn a_save_refuses_the_removed_keybind_button_looks_and_bezel_binds() {
    let one = |w: Value| strict(&tab(vec![w], json!({})));
    assert_eq!(
        one(
            json!({ "id": "k", "kind": "keybind", "x": 0, "y": 0, "w": 2, "h": 2, "bind": { "kind": "keybind", "fn": "pips-reset" }, "look": "round" })
        ),
        format!("Tab \"Test\", widget 1 (\"k\"): the keybind button {GONE}")
    );
    assert_eq!(
        one(
            json!({ "id": "p", "kind": "power", "x": 0, "y": 0, "w": 9, "h": 5, "bind": { "kind": "power" }, "look": "pips" })
        ),
        format!("Tab \"Test\", widget 1 (\"p\"): the pips look {GONE}")
    );
    assert_eq!(
        one(
            json!({ "id": "s", "kind": "shields", "x": 0, "y": 0, "w": 6, "h": 6, "bind": { "kind": "shields" }, "look": "controls" })
        ),
        format!("Tab \"Test\", widget 1 (\"s\"): the controls look {GONE}")
    );
    // A look of one kind is not removed on another.
    assert!(
        one(
            json!({ "id": "f", "kind": "fuel", "x": 0, "y": 0, "w": 4, "h": 6, "bind": { "kind": "fuel" }, "look": "pips" })
        )
        .contains("look \"pips\" is not one of bars")
    );
    assert!(
        one(
            json!({ "id": "a", "kind": "action", "x": 0, "y": 0, "w": 2, "h": 2, "bind": { "kind": "keybind", "fn": "pips-reset" }, "look": "round" })
        )
        .contains("action button cannot bind to \"keybind\"")
    );
    let bezel = json!({ "gamepadId": PAD, "buttons": [{ "button": 2, "bind": { "kind": "keybind", "fn": "pips-weapons-max" } }] });
    assert_eq!(
        check_bezel(Some(&bezel), &known(), true).problems,
        [format!("button 1 (index 2): the keybind bind {GONE}")]
    );
}

/// A dashboards.json saved while the app still had keybind functions.
const OLD_FILE: &str = r#"{
  "schemaVersion": 1,
  "tabs": [
    {
      "id": "main",
      "name": "Main",
      "rows": 14,
      "widgets": [
        { "id": "lamp-1", "kind": "lamp", "x": 0, "y": 0, "w": 2, "h": 1, "bind": { "kind": "value", "id": "caution.pwr" }, "look": "square" },
        { "id": "kb", "kind": "keybind", "x": 2, "y": 0, "w": 3, "h": 3, "bind": { "kind": "keybind", "fn": "pips-reset" }, "look": "round" },
        { "id": "pw", "kind": "power", "x": 5, "y": 0, "w": 9, "h": 5, "bind": { "kind": "power" }, "look": "pips" },
        { "id": "pw2", "kind": "power", "x": 14, "y": 0, "w": 9, "h": 5, "bind": { "kind": "power" }, "look": "columns" },
        { "id": "sh", "kind": "shields", "x": 0, "y": 5, "w": 6, "h": 6, "bind": { "kind": "shields" }, "look": "controls" }
      ],
      "bezel": {
        "gamepadId": "F16 MFD 1 (Vendor: 044f Product: b351)",
        "buttons": [
          { "button": 1, "bind": { "kind": "keybind", "fn": "shield-raise-back" } },
          { "button": 2, "bind": { "kind": "action", "id": "v_lights" } }
        ]
      }
    }
  ]
}"#;

#[test]
fn an_old_dashboards_file_with_keybind_parts_loads_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    std::fs::write(&file, OLD_FILE).unwrap();
    let main = DashboardStore::new(Some(file)).tab("main").unwrap();
    let ids: Vec<&str> = main.widgets.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, ["lamp-1", "pw2"]);
    let bezel = main.bezel.unwrap();
    assert_eq!(bezel.buttons.len(), 1);
    assert_eq!(
        bezel.buttons[0].bind,
        BezelBind::Action {
            id: "v_lights".into()
        }
    );

    // The same file as an import: each left-out part is listed with why, and none counts as a
    // kind from a newer version.
    let (_, summary) = read_dashboard(Some(&serde_json::from_str(OLD_FILE).unwrap()), &known());
    assert_eq!(
        summary.skipped,
        [
            format!("Tab \"Main\", widget 2 (\"kb\"): skipped, the keybind button {GONE}"),
            format!("Tab \"Main\", widget 3 (\"pw\"): skipped, the pips look {GONE}"),
            format!("Tab \"Main\", widget 5 (\"sh\"): skipped, the controls look {GONE}"),
            format!("Tab \"Main\", bezel button 1 (index 1): skipped, the keybind bind {GONE}"),
        ]
    );
    assert_eq!(summary.unknown_kinds, Vec::<String>::new());
    assert_eq!((summary.widgets, summary.bezel_buttons), (2, 1));
}
