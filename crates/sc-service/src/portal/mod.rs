//! The portal and dashboard pages' WebSocket: the status model, the catalog, and the session
//! that runs every page message through one place.

pub mod policy;
pub mod session;
pub mod status;

use std::collections::HashSet;

use serde::Serialize;

use sc_core::ControlCore;
use sc_core::engine::{CONFIRM_GROUP, DEFAULT_SEQUENCES, Sequence, TOGGLES, toggle_for_action};
use sc_core::state::VALUES;
use sc_game::GameAction;

use crate::dashboard::{BezelBind, Bind, DashboardStore, Known, ThemeStore};

/// Game actions offered when the game is not running to list its own.
pub fn known_actions() -> Vec<GameAction> {
    let extra = [
        "v_master_mode_cycle",
        "v_master_mode_set_nav",
        "v_master_mode_set_scm",
        "v_flightready",
        "v_toggle_qdrive_engagement",
        "v_atc_request",
        "v_toggle_docking_request",
        "v_invoke_ping",
        "v_target_hail",
    ];
    TOGGLES
        .iter()
        .flat_map(|t| t.actions())
        .chain(extra)
        .map(|name| GameAction {
            name: name.to_owned(),
            category: "known".to_owned(),
            holdable: false,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogAction {
    pub name: String,
    pub category: String,
    pub toggle: Option<&'static str>,
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Named {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub game_actions: Vec<CatalogAction>,
    pub from_game: bool,
    pub toggles: Vec<Named>,
    pub values: Vec<Named>,
    pub sequences: Vec<Sequence>,
    pub defaults: Vec<Sequence>,
}

/// What a page offers to bind and edit: the game's actions (the known ones while it is closed),
/// toggles, values and sequences.
pub async fn catalog(core: &ControlCore) -> Catalog {
    let from_game = core.actions().await;
    let listed = !from_game.is_empty();
    let actions = if listed { from_game } else { known_actions() };
    Catalog {
        game_actions: actions
            .into_iter()
            .map(|a| CatalogAction {
                toggle: toggle_for_action(&a.name).map(|t| t.label),
                confirm: CONFIRM_GROUP.contains(&a.name.as_str()),
                name: a.name,
                category: a.category,
            })
            .collect(),
        from_game: listed,
        toggles: TOGGLES
            .iter()
            .map(|t| Named {
                id: t.id,
                label: t.label,
            })
            .collect(),
        values: VALUES
            .iter()
            .map(|v| Named {
                id: &v.id,
                label: &v.label,
            })
            .collect(),
        sequences: core.sequences().list(),
        defaults: DEFAULT_SEQUENCES.clone(),
    }
}

/// The lists a dashboard bind and theme name are checked against; actions already on the
/// dashboard, on a widget or a bezel button, stay valid while the game is closed.
pub async fn known(core: &ControlCore, dashboards: &DashboardStore, themes: &ThemeStore) -> Known {
    let bound: Vec<String> = dashboards
        .all_tabs()
        .into_iter()
        .flat_map(|t| {
            let widgets = t.widgets.into_iter().filter_map(|w| match w.bind {
                Bind::Action { id } => Some(id),
                _ => None,
            });
            let buttons = t.bezel.into_iter().flat_map(|b| b.buttons);
            let buttons = buttons.filter_map(|b| match b.bind {
                BezelBind::Action { id } => Some(id),
                _ => None,
            });
            widgets.chain(buttons).collect::<Vec<_>>()
        })
        .collect();
    let mut actions: HashSet<String> = core.actions().await.into_iter().map(|a| a.name).collect();
    actions.extend(known_actions().into_iter().map(|a| a.name));
    actions.extend(CONFIRM_GROUP.iter().map(|&a| a.to_owned()));
    actions.extend(bound);
    Known {
        actions: Some(actions),
        sequences: Some(core.sequences().list().into_iter().map(|s| s.id).collect()),
        themes: Some(themes.ids()),
    }
}
