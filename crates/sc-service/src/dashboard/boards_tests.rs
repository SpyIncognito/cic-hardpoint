//! Per-ship dashboards: the version 2 file, migration from version 1, and the dashboard verbs.

use serde_json::{Value, json};

use super::*;

fn ship(m: &str, model: &str, class: Option<&str>) -> ShipRef {
    ShipRef {
        manufacturer: m.into(),
        model: model.into(),
        class_name: class.map(Into::into),
    }
}

fn corsair() -> ShipRef {
    ship("Drake Interplanetary", "Corsair", Some("DRAK_Corsair"))
}

fn read(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_version_1_file_migrates_on_load_and_is_kept_as_bak_by_the_first_save() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let mut v1 = json!({ "schemaVersion": 1, "theme": "drake", "tabs": [crate::json::to_js_value(&starter_tab())] });
    v1["tabs"][0]["name"] = json!("Mine");
    let v1_text = crate::json::to_pretty_text(&v1);
    std::fs::write(&file, &v1_text).unwrap();

    let store = DashboardStore::new(Some(file.clone()));
    assert_eq!(store.get().tabs[0].name, "Mine");
    assert_eq!(store.get().theme.as_deref(), Some("drake"));
    let list = store.list();
    assert_eq!(
        (list.active.as_str(), list.dashboards.len()),
        (GENERIC_ID, 1)
    );
    // Reading does not write: the file is still version 1, and there is no .bak.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), v1_text);
    assert!(!bak_path(&file).exists());

    store.add_tab(None).unwrap();
    assert_eq!(std::fs::read_to_string(bak_path(&file)).unwrap(), v1_text);
    let v2 = read(&file);
    assert_eq!(v2["schemaVersion"], 2);
    assert_eq!(v2["switchOnShipChange"], false);
    assert_eq!(v2["dashboards"][0]["tabs"][0]["name"], "Mine");
    assert_eq!(v2["dashboards"][0]["theme"], "drake");
    // A second save leaves the .bak as the version 1 file.
    store.add_tab(None).unwrap();
    assert_eq!(std::fs::read_to_string(bak_path(&file)).unwrap(), v1_text);
    // And it reads back the same.
    let again = DashboardStore::new(Some(file));
    assert_eq!(again.file(), store.file());
}

#[test]
fn a_newer_file_is_never_read_or_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let text = r#"{ "schemaVersion": 3, "dashboards": [] }"#;
    std::fs::write(&file, text).unwrap();
    let store = DashboardStore::new(Some(file.clone()));
    assert!(store.file_problem().unwrap().contains("newer version"));
    assert_eq!(store.get().tabs[0].id, "main", "the starter, in memory");
    store.add_tab(None).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    assert!(!bak_path(&file).exists());
}

#[test]
fn reads_version_2_leniently() {
    let tab = |id: &str, name: &str| json!({ "id": id, "name": name, "rows": 14, "widgets": [] });
    let raw = json!({
        "schemaVersion": 2, "active": "drak", "switchOnShipChange": true, "extra": 1,
        "dashboards": [
            { "id": "drak", "name": "Corsair", "ship": { "manufacturer": "Drake Interplanetary", "model": "Corsair", "className": "DRAK_Corsair" }, "theme": "drake", "tabs": [tab("main", "Main")] },
            { "id": "generic", "name": "Generic", "tabs": [tab("main", "Main"), tab("mip", "MIP")] },
            { "id": "bad", "name": "<b>", "tabs": [tab("x", "X")] },
            { "id": "drak", "name": "Again", "tabs": [tab("y", "Y")] },
            { "id": "noship", "name": "No ship", "ship": { "model": "" }, "tabs": [tab("z", "Z")] },
            { "id": "empty", "name": "Empty", "tabs": [] },
        ]
    });
    let FileRead::Read {
        file,
        from_v1,
        skipped,
    } = read_file(&raw, &Known::default())
    else {
        panic!("read");
    };
    assert!(!from_v1);
    assert!(file.switch_on_ship_change);
    assert_eq!(file.active, "drak");
    let ids: Vec<&str> = file.dashboards.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["generic", "drak"], "generic first; bad ones left out");
    // The Corsair's "main" clashed with the generic one's and was renamed.
    assert_eq!(file.dashboards[1].tabs[0].id, "main-2");
    assert_eq!(skipped.len(), 6, "{skipped:#?}");
    assert!(matches!(
        read_file(&json!({ "schemaVersion": 2 }), &Known::default()),
        FileRead::Unreadable(_)
    ));
    // No generic dashboard: the starter is put first.
    let FileRead::Read { file, .. } = read_file(
        &json!({ "schemaVersion": 2, "dashboards": [{ "id": "a", "name": "A", "tabs": [tab("t", "T")] }] }),
        &Known::default(),
    ) else {
        panic!("read");
    };
    assert_eq!(file.dashboards[0].id, GENERIC_ID);
    assert_eq!(file.active, GENERIC_ID);
}

#[test]
fn a_ship_dashboard_copies_tab_names_with_ids_of_its_own_and_the_makers_theme() {
    let store = DashboardStore::default();
    store.add_tab(Some(&json!("MIP"))).unwrap();
    store
        .set_placement(
            "tab-2",
            Some(&json!({ "displayLabel": "MFD Left", "fullscreen": true })),
        )
        .unwrap();
    let made = store
        .create_for_ship(corsair(), None, None, &Known::default())
        .unwrap();
    assert!(made.problems.is_empty());
    let info = made.dashboard.unwrap();
    assert_eq!(info.id, "drak_corsair");
    assert_eq!(info.name, "Drake Corsair");
    assert_eq!(info.ship_name.as_deref(), Some("Drake Corsair"));
    assert_eq!(
        ship("Drake Interplanetary", "Cutlass Black", None).derived_name(),
        "Drake Cutlass Black"
    );
    assert_eq!(ship("", "Mystery", None).derived_name(), "Mystery");
    assert_eq!(
        ship("Nobody Inc", "X1", None).derived_name(),
        "Nobody Inc X1"
    );
    assert_eq!(info.theme.as_deref(), Some("drake"));
    assert_eq!(store.list().active, info.id, "shown at once");
    assert!(store.list().pinned);
    let board = store.active_board();
    let names: Vec<&str> = board.tabs.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Main", "MIP"]);
    let ids: Vec<&str> = board.tabs.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["drak_corsair-main", "drak_corsair-tab-2"]);
    assert_eq!(
        board.tabs[1].placement.as_ref().unwrap().display_label,
        "MFD Left"
    );
    // Tab changes act on the dashboard on screen; a tab anywhere is found by its id.
    store.add_tab(None).unwrap();
    assert_eq!(store.active_board().tabs.len(), 3);
    assert_eq!(store.generic().tabs.len(), 2);
    assert!(
        store
            .set_placement("main", Some(&Value::Null))
            .unwrap()
            .problems
            .is_empty()
    );
    // Matching: the exact class name first, else manufacturer and model.
    assert_eq!(
        store.match_ship(&corsair()).as_deref(),
        Some("drak_corsair")
    );
    assert_eq!(
        store
            .match_ship(&ship(
                "drake interplanetary",
                "corsair",
                Some("DRAK_Corsair_PU")
            ))
            .as_deref(),
        Some("drak_corsair")
    );
    assert_eq!(
        store.match_ship(&ship("Anvil Aerospace", "Arrow", Some("ANVL_Arrow"))),
        None
    );
    // A ship without a theme of its own keeps the copy's.
    store.select(GENERIC_ID, true).unwrap();
    let known = Known {
        themes: Some(["rsi".to_owned(), "drake".to_owned(), "theme-1".to_owned()].into()),
        ..Known::default()
    };
    store.set_theme(Some(&json!("theme-1")), &known).unwrap();
    let arrow = store
        .create_for_ship(
            ship("Anvil Aerospace", "Arrow", Some("ANVL_Arrow")),
            None,
            Some(&json!("My Arrow")),
            &known,
        )
        .unwrap()
        .dashboard
        .unwrap();
    assert_eq!(
        (arrow.name.as_str(), arrow.theme.as_deref()),
        ("My Arrow", Some("theme-1"))
    );
    // Copy another ship's dashboard.
    let c2 = store
        .create_for_ship(
            ship("Drake Interplanetary", "Cutlass Black", None),
            Some("drak_corsair"),
            None,
            &known,
        )
        .unwrap()
        .dashboard
        .unwrap();
    assert_eq!(c2.tabs, 3);
    assert_eq!(
        c2.theme.as_deref(),
        Some("drake"),
        "by the manufacturer's name"
    );
    assert_eq!(
        store
            .create_for_ship(corsair(), Some("nope"), None, &known)
            .unwrap()
            .problems,
        ["No dashboard \"nope\""]
    );
}

#[test]
fn duplicate_rename_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let store = DashboardStore::new(Some(file.clone()));
    let c = store
        .create_for_ship(corsair(), None, None, &Known::default())
        .unwrap()
        .dashboard
        .unwrap();
    let copy = store.duplicate(&c.id, None).unwrap().dashboard.unwrap();
    assert_eq!(copy.name, "Drake Corsair copy");
    assert_eq!(copy.id, "drak_corsair-copy");
    assert_eq!(copy.ship, c.ship, "for the same ship");
    let order: Vec<String> = store.list().dashboards.into_iter().map(|b| b.id).collect();
    assert_eq!(order, ["generic", "drak_corsair", "drak_corsair-copy"]);
    // The first in the list is the one a ship change picks.
    assert_eq!(
        store.match_ship(&corsair()).as_deref(),
        Some("drak_corsair")
    );
    assert_eq!(
        store
            .rename(&copy.id, Some(&json!("  Cargo  ")))
            .unwrap()
            .dashboard
            .unwrap()
            .name,
        "Cargo"
    );
    assert_eq!(
        store
            .rename(&copy.id, Some(&json!("<b>")))
            .unwrap()
            .problems,
        ["The name must be plain text (no < or >)"]
    );
    assert_eq!(
        store.remove(GENERIC_ID).unwrap().problems,
        ["The generic dashboard cannot be removed"]
    );
    assert_eq!(store.list().active, copy.id);
    assert!(store.remove(&copy.id).unwrap().problems.is_empty());
    assert_eq!(
        store.list().active,
        GENERIC_ID,
        "the generic one, when the shown one goes"
    );
    assert!(
        store
            .set_switch(Some(&json!(true)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(
        store.set_switch(Some(&json!("yes"))).unwrap().problems,
        ["on must be true or false"]
    );
    let again = DashboardStore::new(Some(file));
    assert_eq!(again.list().dashboards.len(), 2);
    assert!(again.switch_on_ship_change());
    assert_eq!(again.list().active, GENERIC_ID);
}

#[test]
fn an_import_replaces_the_dashboard_on_screen_and_keeps_tab_ids_unique() {
    let store = DashboardStore::default();
    store
        .create_for_ship(corsair(), None, None, &Known::default())
        .unwrap();
    let (dash, _) = read_dashboard(
        Some(
            &json!({ "schemaVersion": 1, "tabs": [{ "id": "main", "name": "Imported", "rows": 14, "widgets": [] }] }),
        ),
        &Known::default(),
    );
    store.replace(dash.unwrap()).unwrap();
    let board = store.active_board();
    assert_eq!(board.tabs[0].name, "Imported");
    assert_eq!(
        board.tabs[0].id, "main-2",
        "main is the generic dashboard's"
    );
    assert_eq!(store.generic().tabs[0].id, "main");
    // Reset starter on a ship's dashboard replaces its Main by name, keeping its id.
    let (_, ship_main) = (0, store.active_board().tabs[0].id.clone());
    store
        .save_tab(
            &json!({ "id": ship_main, "name": "Main", "rows": 14, "widgets": [] }),
            &Known::default(),
        )
        .unwrap();
    let reset = store.reset_starter().unwrap().tab.unwrap();
    assert_eq!(reset.id, "main-2");
    assert_eq!(store.active_board().tabs.len(), 1);
}

#[test]
fn a_widget_may_hide_when_the_ship_lacks_it() {
    let tab = json!({ "id": "t", "name": "T", "rows": 14, "widgets": [
        { "id": "v", "kind": "toggle", "x": 0, "y": 0, "w": 3, "h": 3, "bind": { "kind": "toggle", "id": "vtol" }, "look": "button", "hideUnsupported": true },
        { "id": "g", "kind": "toggle", "x": 3, "y": 0, "w": 3, "h": 3, "bind": { "kind": "toggle", "id": "gear" }, "look": "button", "hideUnsupported": false },
    ] });
    let c = check_tab(&tab, &Known::default(), true);
    assert!(c.problems.is_empty(), "{:?}", c.problems);
    let t = c.tab.unwrap();
    assert!(t.widgets[0].hide_unsupported && !t.widgets[1].hide_unsupported);
    let out = crate::json::to_js_value(&t);
    assert_eq!(out["widgets"][0]["hideUnsupported"], true);
    assert!(
        out["widgets"][1].get("hideUnsupported").is_none(),
        "false is left out"
    );
    let mut bad = tab.clone();
    bad["widgets"][0]["hideUnsupported"] = json!("yes");
    assert_eq!(
        check_tab(&bad, &Known::default(), true).problems,
        ["Tab \"T\", widget 1 (\"v\"): hideUnsupported must be true or false"]
    );
}

#[test]
fn ids_and_slugs() {
    assert_eq!(slug("DRAK_Corsair"), "drak_corsair");
    assert_eq!(slug("Anvil Aerospace Arrow!"), "anvil-aerospace-arrow");
    assert_eq!(slug("<<>>"), "ship");
    assert_eq!(free_id("a", |id| id == "a" || id == "a-2"), "a-3");
    assert!(check_ship(Some(&json!({ "model": "Arrow", "extra": 1 }))).is_err());
    assert_eq!(
        check_ship(Some(&json!({ "model": "Arrow" })))
            .unwrap()
            .manufacturer,
        ""
    );
}

#[test]
fn a_tab_is_deleted_but_never_a_dashboards_last_one_or_the_fleet_tab() {
    let store = DashboardStore::new(None);
    let extra = store.add_tab(None).unwrap().tab.unwrap();
    let ship = store
        .create_for_ship(corsair(), None, None, &Known::default())
        .unwrap()
        .dashboard
        .unwrap();
    let ship_tabs: Vec<String> = store.get().tabs.iter().map(|t| t.id.clone()).collect();
    assert_eq!(ship_tabs.len(), 2, "copied from the generic dashboard");
    assert!(store.remove_tab(&ship_tabs[0]).unwrap().problems.is_empty());
    assert_eq!(
        store.remove_tab(&ship_tabs[1]).unwrap().problems,
        ["The last tab cannot be removed"],
        "{}'s last tab, while the generic one has two",
        ship.name
    );
    assert_eq!(
        store.remove_tab("@fleet").unwrap().problems,
        ["No tab \"@fleet\""]
    );
    // The generic dashboard's own tabs are its own to delete.
    assert!(store.remove_tab(&extra.id).unwrap().problems.is_empty());
    assert_eq!(
        store.remove_tab("main").unwrap().problems,
        ["The last tab cannot be removed"]
    );
    assert_eq!(store.generic().tabs.len(), 1);
}

#[test]
fn the_cell_size_is_per_dashboard_and_kept_in_the_file_copies_and_exports() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let store = DashboardStore::new(Some(file.clone()));
    assert_eq!(
        store.get().cell,
        None,
        "an existing dashboard loads at the default"
    );
    assert!(
        !crate::json::to_text(&store.get()).contains("cell"),
        "nothing written for it"
    );
    for bad in [json!(70), json!("80"), Value::Null, json!(80.5)] {
        assert_eq!(
            store.set_cell(Some(&bad)).unwrap().problems,
            ["the cell size is 48, 64, 72, 80 or 96 px"],
            "{bad}"
        );
    }
    assert!(
        store
            .set_cell(Some(&json!(96)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(store.get().cell, Some(96));
    assert_eq!(read(&file)["dashboards"][0]["cell"], 96);
    // A ship's dashboard starts as a copy, then has its own size.
    store
        .create_for_ship(corsair(), None, None, &Known::default())
        .unwrap();
    assert_eq!(store.get().cell, Some(96));
    assert!(
        store
            .set_cell(Some(&json!(48)))
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(store.generic().cell, Some(96));
    assert_eq!(DashboardStore::new(Some(file)).get().cell, Some(48));
    // The export carries it, and an import takes it or leaves a bad one out.
    let mut export = crate::json::to_js_value(&store.get());
    assert_eq!(export["cell"], 48);
    let (dash, _) = read_dashboard(Some(&export), &Known::default());
    assert_eq!(dash.unwrap().cell, Some(48));
    export["cell"] = json!(50);
    let (dash, summary) = read_dashboard(Some(&export), &Known::default());
    assert_eq!(dash.unwrap().cell, None);
    assert_eq!(
        summary.skipped,
        ["Cell size left out, the cell size is 48, 64, 72, 80 or 96 px"]
    );
}
