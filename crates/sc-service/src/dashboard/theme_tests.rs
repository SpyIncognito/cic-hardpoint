//! Themes: validation, the store and its file, the fallback, and theme names on the dashboard.

use serde_json::{Value, json};

use super::*;
use crate::json::to_js_value;

/// A user theme as the page sends it: a copy of a built-in under a user id.
fn user_theme(from: &str, id: &str, name: &str) -> Value {
    let mut t = to_js_value(built_in_theme(from).unwrap());
    t["id"] = json!(id);
    t["name"] = json!(name);
    t
}

fn problems(raw: &Value) -> Vec<String> {
    check_theme(raw).err().unwrap_or_default()
}

fn known_themes(names: &[&str]) -> Known {
    Known {
        themes: Some(names.iter().map(|s| (*s).to_owned()).collect()),
        ..Known::default()
    }
}

fn tab_with(extra: Value) -> Value {
    let mut t = json!({ "id": "test", "name": "Test", "rows": 14, "widgets": [] });
    t.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    t
}

#[test]
fn built_ins_are_complete_and_pass_the_user_checks() {
    assert_eq!(
        BUILT_IN_THEMES
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["rsi", "drake"]
    );
    assert_eq!(BUILT_IN_THEMES[0].id, DEFAULT_THEME);
    for t in BUILT_IN_THEMES.iter() {
        let copy = check_theme(&user_theme(&t.id, "theme-1", "Copy")).unwrap();
        assert_eq!(copy.colors, t.colors, "{}", t.id);
        assert_eq!(copy.base, t.id);
        for token in COLOR_TOKENS {
            assert!(
                is_hex_color(t.colors.get(token).unwrap()),
                "{} {token}",
                t.id
            );
        }
    }
    let rsi = &BUILT_IN_THEMES[0];
    let drake = &BUILT_IN_THEMES[1];
    assert_eq!(
        (rsi.font, rsi.bars, rsi.leading_zeros),
        ("condensed", "segmented", true)
    );
    assert_eq!(
        (drake.font, drake.bars, drake.leading_zeros),
        ("mono", "segmented", false)
    );
}

#[test]
fn takes_only_hex_colours() {
    for good in ["#fff", "#FFFFFF", "#0a1b2c", "#0a1b2c80"] {
        let mut t = user_theme("rsi", "theme-1", "Mine");
        t["colors"]["bg"] = json!(good);
        let theme = check_theme(&t).unwrap();
        assert_eq!(
            theme.colors.get("bg"),
            Some(good.to_ascii_lowercase().as_str())
        );
    }
    for bad in [
        json!("red"),
        json!("#ggg"),
        json!("#12345"),
        json!("#1234567"),
        json!(" #fff"),
        json!("#fff "),
        json!("red;background:url(x)"),
        json!("#fff;background:url(https://example.com/x)"),
        json!("</style><script>alert(1)</script>"),
        json!("expression(alert(1))"),
        json!("var(--bad)"),
        json!("rgb(1,2,3)"),
        json!(255),
        json!(null),
    ] {
        let mut t = user_theme("rsi", "theme-1", "Mine");
        t["colors"]["fg"] = bad.clone();
        let p = problems(&t);
        assert_eq!(p.len(), 1, "{bad}");
        assert!(
            p[0].starts_with("colour fg must be #rgb, #rrggbb or #rrggbbaa, not "),
            "{p:?}"
        );
    }
    let mut t = user_theme("rsi", "theme-1", "Mine");
    t["colors"].as_object_mut().unwrap().remove("bad");
    t["colors"]["background-image"] = json!("#fff");
    assert_eq!(
        problems(&t),
        [
            "colour \"background-image\" is not a theme colour",
            "colour bad is missing"
        ]
    );
    let mut t = user_theme("rsi", "theme-1", "Mine");
    t["colors"] = json!("#fff");
    assert_eq!(problems(&t), ["it has no colours"]);
}

#[test]
fn takes_choices_from_the_fixed_sets_only() {
    let mut t = user_theme("rsi", "theme-1", "Mine");
    t["base"] = json!("cig");
    t["bars"] = json!("glow");
    t["font"] = json!("Comic Sans MS");
    t["leadingZeros"] = json!("yes");
    t["css"] = json!("body{display:none}");
    t["skin"] = json!("<svg onload=alert(1)>");
    assert_eq!(
        problems(&t),
        [
            "field \"css\" is not allowed",
            "field \"skin\" is not allowed",
            "the base must be one of rsi, drake",
            "bars must be one of segmented, solid",
            "the font must be one of condensed, mono, sans",
            "leading zeros must be true or false",
        ]
    );
    for (key, value) in [("bars", "solid"), ("font", "sans")] {
        let mut t = user_theme("drake", "theme-2", "Mine");
        t[key] = json!(value);
        assert!(check_theme(&t).is_ok(), "{key}");
    }
    assert_eq!(problems(&json!([])), ["it is not a theme"]);
}

#[test]
fn takes_plain_text_names_and_user_ids() {
    let name = |n: Value| {
        let mut t = user_theme("rsi", "theme-1", "x");
        t["name"] = n;
        problems(&t)
    };
    assert_eq!(name(json!("  Night ops  ")), Vec::<String>::new());
    assert_eq!(
        check_theme(&user_theme("rsi", "theme-1", "  Night ops  "))
            .unwrap()
            .name,
        "Night ops"
    );
    let too_long = "x".repeat(MAX_THEME_NAME + 1);
    for bad in [json!(""), json!("   "), json!(too_long), json!(7)] {
        assert_eq!(name(bad), ["the theme name must be 1 to 40 characters"]);
    }
    for bad in ["<b>x</b>", "</style>", "a\u{7}b", "line\nbreak"] {
        assert_eq!(
            name(json!(bad)),
            ["the theme name must be plain text (no < or >)"],
            "{bad:?}"
        );
    }
    let id = |id: Value| {
        let mut t = user_theme("rsi", "x", "Mine");
        t["id"] = id;
        problems(&t)
    };
    assert_eq!(
        id(json!("rsi")),
        ["\"rsi\" is a built-in theme and cannot be changed"]
    );
    for bad in [json!("Theme 1"), json!("a\"]{}"), json!(""), json!(1)] {
        assert_eq!(
            id(bad),
            ["the theme id must be 1 to 40 lower-case letters, digits, - or _"]
        );
    }
}

#[test]
fn css_is_built_from_tokens_and_hex_only() {
    let mut t = user_theme("drake", "theme-1", "Mine");
    t["colors"]["bg"] = json!("#ABC");
    let css = theme_css(&check_theme(&t).unwrap());
    assert!(css.starts_with("[data-theme=\"theme-1\"] { --bg: #abc; --panel: "));
    assert_eq!(css.matches(';').count(), COLOR_TOKENS.len());
    assert!(!css.contains('<') && !css.contains("url("));
    let all = built_in_css();
    assert!(all.contains("[data-theme=\"rsi\"]") && all.contains("[data-theme=\"drake\"]"));
}

#[test]
fn duplicates_saves_renames_and_removes_and_keeps_them_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cfg").join("themes.json");
    let store = ThemeStore::new(Some(file.clone()));
    assert!(store.user().is_empty());

    let a = store.duplicate("rsi", None).unwrap().theme.unwrap();
    assert_eq!(
        (a.id.as_str(), a.name.as_str(), a.base),
        ("theme-1", "RSI copy", "rsi")
    );
    let b = store.duplicate("rsi", None).unwrap().theme.unwrap();
    assert_eq!((b.id.as_str(), b.name.as_str()), ("theme-2", "RSI copy 2"));
    let c = store
        .duplicate("drake", Some(&json!(" Night amber ")))
        .unwrap()
        .theme
        .unwrap();
    assert_eq!(
        (c.id.as_str(), c.name.as_str(), c.base),
        ("theme-3", "Night amber", "drake")
    );
    assert_eq!(
        store
            .duplicate("drake", Some(&json!("night AMBER")))
            .unwrap()
            .problems,
        ["A theme is already named \"Night amber\""]
    );
    assert_eq!(
        store.duplicate("nope", None).unwrap().problems,
        ["No theme \"nope\""]
    );
    assert_eq!(
        store
            .duplicate("rsi", Some(&json!("<i>")))
            .unwrap()
            .problems,
        ["the theme name must be plain text (no < or >)"]
    );

    // Edit colours and choices, and rename.
    let mut edit = to_js_value(&a);
    edit["name"] = json!("Galaxy night");
    edit["colors"]["select"] = json!("#FF0000");
    edit["bars"] = json!("solid");
    edit["leadingZeros"] = json!(false);
    edit["font"] = json!("sans");
    let saved = store.save(&edit).unwrap();
    assert!(saved.problems.is_empty(), "{:?}", saved.problems);
    let saved = saved.theme.unwrap();
    assert_eq!(saved.colors.get("select"), Some("#ff0000"));
    assert_eq!(
        (saved.bars, saved.leading_zeros, saved.font),
        ("solid", false, "sans")
    );

    let mut clash = to_js_value(&b);
    clash["name"] = json!("drake");
    assert_eq!(
        store.save(&clash).unwrap().problems,
        ["A theme is already named \"Drake\""]
    );
    let mut rebase = to_js_value(&b);
    rebase["base"] = json!("drake");
    assert_eq!(
        store.save(&rebase).unwrap().problems,
        ["The base cannot be changed; duplicate the other built-in instead"]
    );
    assert_eq!(
        store
            .save(&user_theme("rsi", "theme-9", "New"))
            .unwrap()
            .problems,
        ["No theme \"theme-9\"; duplicate one to make a new theme"]
    );
    assert_eq!(
        store
            .save(&user_theme("rsi", "rsi", "RSI"))
            .unwrap()
            .problems,
        ["\"rsi\" is a built-in theme and cannot be changed"]
    );

    // The file holds the user themes only, and a new store reads them back the same.
    let text = std::fs::read_to_string(&file).unwrap();
    let on_disk: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(on_disk["schemaVersion"], 1);
    assert_eq!(
        on_disk["themes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["theme-1", "theme-2", "theme-3"]
    );
    let again = ThemeStore::new(Some(file.clone()));
    assert_eq!(again.user(), store.user());
    assert_eq!(again.get("theme-1").unwrap(), saved);

    assert_eq!(
        store.remove("rsi").unwrap().problems,
        ["\"rsi\" is a built-in theme and cannot be removed"]
    );
    assert!(store.remove("theme-2").unwrap().problems.is_empty());
    assert_eq!(
        store.remove("theme-2").unwrap().problems,
        ["No theme \"theme-2\""]
    );
    // The freed id is used again.
    assert_eq!(
        store.duplicate("rsi", None).unwrap().theme.unwrap().id,
        "theme-2"
    );
    assert_eq!(
        ThemeStore::new(Some(file)).ids(),
        ["rsi", "drake", "theme-1", "theme-2", "theme-3"]
            .map(String::from)
            .into()
    );
}

#[test]
fn caps_the_user_themes() {
    let store = ThemeStore::default();
    for _ in 0..MAX_USER_THEMES {
        assert!(store.duplicate("drake", None).unwrap().problems.is_empty());
    }
    assert_eq!(
        store.duplicate("drake", None).unwrap().problems,
        ["At most 20 themes of your own"]
    );
}

#[test]
fn leaves_out_bad_or_twice_used_themes_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("themes.json");
    let mut evil = user_theme("rsi", "theme-2", "Evil");
    evil["colors"]["bg"] = json!("red;background:url(x)");
    let doc = json!({ "schemaVersion": 1, "themes": [
        user_theme("rsi", "theme-1", "Good"),
        evil,
        user_theme("drake", "theme-1", "Twice"),
        user_theme("drake", "rsi", "Shadow"),
        { "id": "theme-4", "name": "Bare" },
    ] });
    std::fs::write(&file, doc.to_string()).unwrap();
    let store = ThemeStore::new(Some(file.clone()));
    let names: Vec<String> = store.user().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["Good"]);

    std::fs::write(&file, "{not json").unwrap();
    assert!(ThemeStore::new(Some(file)).user().is_empty());
}

#[test]
fn falls_back_to_the_dashboard_theme_then_rsi() {
    let store = ThemeStore::default();
    let mine = store.duplicate("drake", None).unwrap().theme.unwrap();
    assert_eq!(store.resolve(Some(&mine.id), Some("rsi")).id, mine.id);
    assert_eq!(store.resolve(None, Some("drake")).id, "drake");
    assert_eq!(store.resolve(Some("gone"), Some("drake")).id, "drake");
    assert_eq!(store.resolve(Some("gone"), Some("gone-too")).id, "rsi");
    assert_eq!(store.resolve(None, None).id, "rsi");
    store.remove(&mine.id).unwrap();
    assert_eq!(store.resolve(Some(&mine.id), None).id, "rsi");
}

#[test]
fn a_tab_names_a_known_theme_only() {
    let known = known_themes(&["rsi", "drake", "theme-1"]);
    let strict = |extra: Value| check_tab(&tab_with(extra), &known, true);
    let ok = strict(json!({ "theme": "theme-1" }));
    assert!(ok.problems.is_empty());
    assert_eq!(ok.tab.unwrap().theme.as_deref(), Some("theme-1"));
    assert_eq!(
        strict(json!({ "theme": "neon" })).problems,
        ["Tab \"Test\": unknown theme \"neon\""]
    );
    let form =
        "Tab \"Test\": the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)";
    for bad in [
        json!("Drake"),
        json!("drake;color:red"),
        json!("</style>"),
        json!(null),
        json!({ "id": "x", "colors": { "bg": "#000" } }),
    ] {
        assert_eq!(strict(json!({ "theme": bad })).problems, [form], "{bad}");
    }

    let lenient = check_tab(&tab_with(json!({ "theme": "neon" })), &known, false);
    assert_eq!(
        lenient.skipped,
        ["Tab \"Test\": theme left out, unknown theme \"neon\""]
    );
    assert_eq!(lenient.tab.unwrap().theme, None);

    // The service's own file is read before the user themes are listed: the form is checked,
    // the name is kept, and the page falls back if the theme is gone.
    let own = check_tab(
        &tab_with(json!({ "theme": "theme-7" })),
        &Known::default(),
        false,
    );
    assert_eq!(own.tab.unwrap().theme.as_deref(), Some("theme-7"));
}

#[test]
fn an_import_names_themes_but_cannot_bring_any() {
    let known = known_themes(&["rsi", "drake", "theme-1"]);
    let file = json!({
        "schemaVersion": 1,
        "theme": "drake",
        "themes": [user_theme("rsi", "theme-9", "Smuggled")],
        "tabs": [
            tab_with(json!({ "id": "a", "name": "A", "theme": "theme-1" })),
            tab_with(json!({ "id": "b", "name": "B", "theme": "theme-9" })),
            tab_with(json!({ "id": "c", "name": "C", "theme": { "id": "theme-9", "colors": {} }, "themes": [] })),
        ],
    });
    let (dash, summary) = read_dashboard(Some(&file), &known);
    assert_eq!(
        summary.skipped,
        [
            "Ignored field \"themes\"",
            "Tab \"B\": theme left out, unknown theme \"theme-9\"",
            "Tab \"C\": ignored field \"themes\"",
            "Tab \"C\": theme left out, the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)",
        ]
    );
    let dash = dash.unwrap();
    assert_eq!(dash.theme.as_deref(), Some("drake"));
    let themes: Vec<Option<&str>> = dash.tabs.iter().map(|t| t.theme.as_deref()).collect();
    assert_eq!(themes, [Some("theme-1"), None, None]);
    let text = crate::json::to_text(&dash);
    assert!(!text.contains("Smuggled") && !text.contains("colors"));

    let (dash, summary) = read_dashboard(
        Some(&json!({ "schemaVersion": 1, "theme": "Neon<", "tabs": [tab_with(json!({}))] })),
        &known,
    );
    assert_eq!(
        summary.skipped,
        [
            "Dashboard theme left out, the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)"
        ]
    );
    assert_eq!(dash.unwrap().theme, None);
}

#[test]
fn the_dashboard_keeps_its_theme_and_drops_a_removed_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dashboards.json");
    let store = DashboardStore::new(Some(file.clone()));
    let known = known_themes(&["rsi", "drake", "theme-1"]);
    assert_eq!(
        store
            .set_theme(Some(&json!("neon")), &known)
            .unwrap()
            .problems,
        ["unknown theme \"neon\""]
    );
    assert!(
        store
            .set_theme(Some(&json!("theme-1")), &known)
            .unwrap()
            .problems
            .is_empty()
    );
    let mut main = crate::json::to_js_value(&store.get().tabs[0]);
    main["theme"] = json!("theme-1");
    assert!(store.save_tab(&main, &known).unwrap().is_empty());
    let mut other = tab_with(json!({ "id": "other", "name": "Other", "theme": "drake" }));
    assert!(store.save_tab(&other, &known).unwrap().is_empty());
    let copy = store.duplicate_tab("main").unwrap().tab.unwrap();
    assert_eq!(copy.theme.as_deref(), Some("theme-1"));

    let text = std::fs::read_to_string(&file).unwrap();
    let on_disk: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        on_disk.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "schemaVersion",
            "active",
            "switchOnShipChange",
            "dashboards"
        ]
    );
    let generic = &on_disk["dashboards"][0];
    assert_eq!(
        generic.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["id", "name", "theme", "tabs"]
    );
    assert_eq!(generic["tabs"][0]["theme"], "theme-1");
    let again = DashboardStore::new(Some(file.clone()));
    assert_eq!(again.get(), store.get());

    store.drop_theme("theme-1").unwrap();
    let d = store.get();
    assert_eq!(d.theme, None);
    let themes: Vec<Option<&str>> = d.tabs.iter().map(|t| t.theme.as_deref()).collect();
    assert_eq!(themes, [None, None, Some("drake")]);

    // A saved tab without a theme follows the dashboard's again.
    other["theme"] = Value::Null;
    other.as_object_mut().unwrap().remove("theme");
    assert!(store.save_tab(&other, &known).unwrap().is_empty());
    assert_eq!(store.tab("other").unwrap().theme, None);
    assert!(
        store
            .set_theme(Some(&Value::Null), &known)
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(store.get().theme, None);
}

#[test]
fn text_fields_are_readable_in_every_built_in_theme() {
    // WCAG 2: 4.5:1 for text, 3:1 for the placeholder and the focus ring (non-text contrast).
    for t in BUILT_IN_THEMES.iter() {
        let c = |token: &str| t.colors.get(token).unwrap();
        let ratio = |a, b| contrast(c(a), c(b)).unwrap();
        assert!(
            ratio("input-fg", "input-bg") >= 4.5,
            "{}: text {:.2}",
            t.id,
            ratio("input-fg", "input-bg")
        );
        assert!(
            ratio("input-placeholder", "input-bg") >= 3.0,
            "{}: placeholder {:.2}",
            t.id,
            ratio("input-placeholder", "input-bg")
        );
        assert!(
            ratio("input-focus", "input-bg") >= 3.0,
            "{}: focus {:.2}",
            t.id,
            ratio("input-focus", "input-bg")
        );
        // The field stands out from the panel it sits on, and from the page.
        assert!(
            ratio("input-focus", "panel") >= 3.0,
            "{}: focus on panel",
            t.id
        );
    }
    assert!((contrast("#000", "#fff").unwrap() - 21.0).abs() < 1e-9);
    assert!((contrast("#777", "#777").unwrap() - 1.0).abs() < 1e-9);
}

#[test]
fn a_theme_saved_before_the_input_colours_takes_them_from_its_own() {
    let mut t = user_theme("drake", "theme-1", "Old");
    for (token, _) in INPUT_FALLBACKS {
        t["colors"].as_object_mut().unwrap().remove(*token);
    }
    let read = check_theme(&t).unwrap();
    for (token, from) in INPUT_FALLBACKS {
        assert_eq!(read.colors.get(token), read.colors.get(from), "{token}");
    }
}
