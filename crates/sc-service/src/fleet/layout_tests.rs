//! Fleet tab layouts: the CIC board's checks (v3 nesting on each unit's own columns, cells, the
//! object cap, depth), the v1 and v2 upgrade, the keys, and the file.

use serde_json::{Value, json};

use super::*;

fn obj(id: &str, kind: &str, extra: Value) -> Value {
    let mut o = json!({ "id": id, "kind": kind, "x": 0, "y": 0, "w": 6, "h": 3 });
    o.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    o
}

fn v3(objects: Vec<Value>) -> Value {
    json!({ "schemaVersion": 3, "columns": 24, "cell": 72, "rows": 9, "objects": objects })
}

fn problems(layout: &Value) -> String {
    check_layout(layout).err().unwrap_or_default().join("; ")
}

/// Units nested `levels` deep, the innermost holding a member card.
fn nested(levels: usize) -> Value {
    let mut inner = obj("m", "member", json!({ "bind": "m1" }));
    for i in 0..levels {
        inner = obj(
            &format!("u{i}"),
            "unit",
            json!({ "bind": "w", "children": [inner] }),
        );
    }
    inner
}

#[test]
fn a_nested_layout_round_trips_and_an_emptied_grid_stays_apart_from_a_default_one() {
    let raw = v3(vec![
        obj(
            "a",
            "unit",
            json!({ "bind": "w", "look": "condensed", "w": 24, "h": 9, "children": [
                obj("b", "unit", json!({ "bind": "s1", "w": 12, "children": [obj("c", "member", json!({ "bind": "m1", "look": "full" }))] })),
                obj("d", "ship", json!({ "bind": "ship:f1", "x": 12, "w": 12 })),
            ] }),
        ),
        obj("e", "unit", json!({ "bind": "w2", "y": 9, "children": [] })),
        obj("f", "unit", json!({ "bind": "w3", "y": 12 })),
    ]);
    let layout = check_layout(&raw).unwrap();
    let kids = layout.objects[0].children.as_ref().unwrap();
    assert_eq!(kids[0].children.as_ref().unwrap()[0].id, "c");
    let text = serde_json::to_value(&layout).unwrap();
    assert_eq!(
        text["objects"][1]["children"],
        json!([]),
        "emptied on purpose"
    );
    assert!(
        text["objects"][2].get("children").is_none(),
        "the default grid"
    );
    assert_eq!(text["cell"], 72);
    assert_eq!(check_layout(&text).unwrap(), layout, "stable");
}

#[test]
fn a_child_fits_its_units_own_width_and_cells_are_fixed_sizes() {
    let child = obj("m", "member", json!({ "x": 8, "w": 4 }));
    let unit = |w: u32| {
        v3(vec![obj(
            "u",
            "unit",
            json!({ "w": w, "children": [child.clone()] }),
        )])
    };
    assert_eq!(problems(&unit(12)), "");
    assert_eq!(problems(&unit(10)), "an object is outside the grid");
    for cell in CELLS {
        let mut l = v3(vec![]);
        l["cell"] = json!(cell);
        assert_eq!(problems(&l), "", "{cell}");
    }
    let mut odd = v3(vec![]);
    odd["cell"] = json!(70);
    assert_eq!(problems(&odd), "cells are 48, 64, 72, 80 or 96 px");
    odd.as_object_mut().unwrap().remove("cell");
    assert_eq!(check_layout(&odd).unwrap().cell, DEFAULT_CELL);
}

#[test]
fn a_title_alignment_is_each_objects_own_center_or_left() {
    let l = v3(vec![obj(
        "w",
        "unit",
        json!({ "bind": "w", "w": 24, "align": "left", "children": [obj("s", "unit", json!({ "bind": "s", "w": 12, "align": "center" }))] }),
    )]);
    let kept = check_layout(&l).unwrap();
    let wing = &kept.objects[0];
    assert_eq!(wing.align, Some(Titles::Left));
    assert_eq!(
        wing.children.as_ref().unwrap()[0].align,
        Some(Titles::Center),
        "a squadron keeps its own"
    );
    assert!(
        !serde_json::to_string(&check_layout(&v3(vec![obj("u", "unit", json!({}))])).unwrap())
            .unwrap()
            .contains("align"),
        "unsaid stays unsaid"
    );
    let odd = v3(vec![obj("u", "unit", json!({ "align": "middle" }))]);
    assert!(
        problems(&odd).starts_with("not a layout"),
        "only center or left"
    );
}

#[test]
fn a_layout_wide_titles_setting_moves_onto_each_unit_once() {
    let mut l = v3(vec![obj(
        "w",
        "unit",
        json!({ "bind": "w", "w": 24, "children": [obj("s", "unit", json!({ "bind": "s", "w": 12, "align": "left" })), obj("r", "rollup", json!({ "x": 12, "w": 4 }))] }),
    )]);
    l["titles"] = json!("center");
    let kept = check_layout(&l).unwrap();
    assert_eq!(kept.titles, None);
    let wing = &kept.objects[0];
    let kids = wing.children.as_ref().unwrap();
    assert_eq!(
        (wing.align, kids[0].align, kids[1].align),
        (Some(Titles::Center), Some(Titles::Left), None),
        "units without their own take it; a unit's own stays; other objects stay left"
    );
    assert!(!serde_json::to_string(&kept).unwrap().contains("titles"));
    l["titles"] = json!("middle");
    assert!(
        problems(&l).starts_with("not a layout"),
        "only center or left"
    );
}

#[test]
fn versions_1_and_2_are_upgraded_and_other_versions_refused() {
    let mut v1 = v3(vec![obj(
        "a",
        "unit",
        json!({ "bind": "w", "y": 5, "h": 4 }),
    )]);
    v1["schemaVersion"] = json!(1);
    v1["cell"] = json!(64);
    let up = check_layout(&v1).unwrap();
    assert_eq!((up.schema_version, up.cell), (LAYOUT_SCHEMA, DEFAULT_CELL));
    assert_eq!(
        (up.objects[0].y, up.objects[0].h),
        (4, 3),
        "54 px rows in 72 px cells"
    );
    // A v2 wing 12 wide with its children on 24 columns: halved onto its own 12.
    let v2 = json!({ "schemaVersion": 2, "columns": 24, "rows": 9, "objects": [
        obj("w", "unit", json!({ "w": 12, "children": [
            obj("s", "ship", json!({ "x": 12, "w": 12, "h": 1, "bind": "ship:u2" })),
            obj("m", "member", json!({ "x": 23, "w": 1 })),
        ] })),
    ] });
    let up = check_layout(&v2).unwrap();
    let got: Vec<(u32, u32, u32)> = up.objects[0]
        .children
        .as_ref()
        .unwrap()
        .iter()
        .map(|o| (o.x, o.w, o.h))
        .collect();
    assert_eq!(got, vec![(6, 6, 1), (11, 1, 2)]);
    let mut kept = v3(vec![]);
    kept["cell"] = json!(80);
    assert_eq!(
        check_layout(&kept).unwrap().cell,
        80,
        "a version 3 layout keeps its cell size"
    );
    v1["schemaVersion"] = json!(4);
    assert_eq!(problems(&v1), "schemaVersion must be 3");
    let mut cols = v3(vec![]);
    cols["columns"] = json!(12);
    assert_eq!(problems(&cols), "columns must be 24");
}

#[test]
fn depth_is_three_levels_and_a_container_two() {
    // Board unit (0) > unit (1) > unit (2) > member (3): the deepest allowed.
    assert_eq!(problems(&v3(vec![nested(3)])), "");
    let deep = problems(&v3(vec![nested(4)]));
    assert!(deep.contains("at most 2 levels inside others"), "{deep}");
    assert!(deep.contains("at most 3 levels inside others"), "{deep}");
}

#[test]
fn the_object_cap_counts_every_level() {
    let cards = |n: usize, from: usize| -> Vec<Value> {
        (0..n)
            .map(|i| obj(&format!("c{}", i + from), "member", json!({ "bind": "m1" })))
            .collect()
    };
    let board = |n: usize| {
        let mut objects = cards(60, 0);
        objects.push(obj(
            "box",
            "unit",
            json!({ "bind": "w", "children": cards(n, 100) }),
        ));
        v3(objects)
    };
    assert_eq!(problems(&board(39)), "");
    assert_eq!(
        problems(&board(40)),
        "at most 100 objects, counting those inside units"
    );
}

#[test]
fn refuses_what_the_cic_board_refuses() {
    let inside = |kind: &str| {
        v3(vec![obj(
            "a",
            "unit",
            json!({ "children": [obj("b", kind, json!({}))] }),
        )])
    };
    for kind in ["header", "unassigned", "timeline"] {
        assert!(
            problems(&inside(kind)).contains("goes on the board"),
            "{kind}"
        );
    }
    assert!(problems(&inside("rollup")).is_empty());
    let cases = [
        (
            obj(
                "a",
                "rollup",
                json!({ "children": [obj("b", "member", json!({}))] }),
            ),
            "only a unit may hold other objects",
        ),
        (
            obj("a", "rollup", json!({ "children": [] })),
            "only a unit may hold other objects",
        ),
        (
            obj(
                "a",
                "unit",
                json!({ "children": [obj("a", "member", json!({}))] }),
            ),
            "object ids must be unique and plain",
        ),
        (
            obj("a b", "member", json!({})),
            "object ids must be unique and plain",
        ),
        (
            obj(
                "a",
                "unit",
                json!({ "children": [obj("b", "member", json!({ "x": 20 }))] }),
            ),
            "an object is outside the grid",
        ),
        (
            obj("a", "member", json!({ "h": 0 })),
            "an object is outside the grid",
        ),
        (
            obj("a", "member", json!({ "y": 1000 })),
            "an object is outside the grid",
        ),
        (
            obj("a", "member", json!({ "bind": "<x>" })),
            "binds are ids",
        ),
        (obj("a", "ship", json!({ "bind": "f1" })), "binds are ids"),
        (
            obj("a", "unassigned", json!({ "bind": "w" })),
            "binds are ids",
        ),
        (
            obj("a", "rollup", json!({ "look": "full" })),
            "a look the object does not have",
        ),
        (
            obj("a", "member", json!({ "label": "<b>" })),
            "labels are plain text",
        ),
        (
            obj("a", "member", json!({ "label": "x".repeat(41) })),
            "labels are plain text",
        ),
    ];
    for (o, want) in cases {
        let got = problems(&v3(vec![o.clone()]));
        assert!(got.contains(want), "{o}: {got}");
    }
    assert!(
        problems(&v3(vec![obj("a", "member", json!({ "card": {} }))])).starts_with("not a layout")
    );
    assert!(problems(&v3(vec![obj("a", "widget", json!({}))])).starts_with("not a layout"));
    let big = v3(vec![obj(
        "a",
        "member",
        json!({ "label": "x".repeat(70 * 1024) }),
    )]);
    assert_eq!(problems(&big), "a layout is at most 64 KiB");
}

#[test]
fn keys_are_p2p_or_an_event() {
    assert!(is_key("p2p") && is_key("event:op-1"));
    for bad in ["", "event:", "event:a b", "P2P", "cic:op-1", "event:op-1/x"] {
        assert!(!is_key(bad), "{bad}");
    }
}

#[test]
fn saves_one_per_event_and_one_for_peer_to_peer_and_reads_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fleet-layout.json");
    let store = FleetLayoutStore::new(Some(file.clone()));
    let mut seen = store.subscribe();
    let a = v3(vec![nested(2)]);
    let b = v3(vec![obj("x", "unassigned", json!({}))]);
    assert!(
        store
            .save("event:op-1", Some(&a), 1)
            .unwrap()
            .problems
            .is_empty()
    );
    assert!(store.save("p2p", Some(&b), 2).unwrap().problems.is_empty());
    assert!(seen.has_changed().unwrap());
    seen.mark_unchanged();
    // The same layout again is no change.
    assert!(store.save("p2p", Some(&b), 3).unwrap().problems.is_empty());
    assert!(!seen.has_changed().unwrap());
    assert_eq!(
        store.save("event:op 1", Some(&a), 4).unwrap().problems,
        ["the key is p2p or event:<event id>"]
    );
    let refused = store.save("p2p", Some(&v3(vec![nested(4)])), 5).unwrap();
    assert!(!refused.problems.is_empty());
    assert_eq!(store.get("p2p").unwrap(), check_layout(&b).unwrap(), "kept");
    assert_eq!(store.get("event:op-2"), None);

    let again = FleetLayoutStore::new(Some(file.clone()));
    assert_eq!(again.file(), store.file());
    assert_eq!(again.get("event:op-1").unwrap(), check_layout(&a).unwrap());

    assert!(store.save("p2p", None, 6).unwrap().problems.is_empty());
    assert_eq!(store.get("p2p"), None);
    assert_eq!(FleetLayoutStore::new(Some(file)).file().layouts.len(), 1);
}

#[test]
fn keeps_the_latest_layouts_and_reads_the_file_leniently() {
    let store = FleetLayoutStore::new(None);
    let l = v3(vec![obj("a", "member", json!({}))]);
    for i in 0..=MAX_SAVED {
        let key = format!("event:e{i}");
        let now = i64::try_from(i).unwrap();
        assert!(store.save(&key, Some(&l), now).unwrap().problems.is_empty());
    }
    let file = store.file();
    assert_eq!(file.layouts.len(), MAX_SAVED);
    assert!(store.get("event:e0").is_none(), "the oldest went");
    assert!(store.get(&format!("event:e{MAX_SAVED}")).is_some());

    let mut v1 = v3(vec![obj("a", "unit", json!({ "bind": "w" }))]);
    v1["schemaVersion"] = json!(1);
    let raw = json!({ "schemaVersion": 1, "layouts": [
        { "key": "event:op-1", "savedMs": 5, "layout": v1 },
        { "key": "event:op-1", "savedMs": 6, "layout": v1 },
        { "key": "nope", "layout": v1 },
        { "key": "p2p", "layout": v3(vec![nested(4)]) },
    ] });
    let (read, skipped) = read_file(&raw);
    assert_eq!(read.layouts.len(), 1);
    assert_eq!(
        read.layouts[0].layout.schema_version, LAYOUT_SCHEMA,
        "upgraded"
    );
    assert_eq!(skipped.len(), 3, "{skipped:?}");
}
