//! The user's dashboards, kept as dashboards.json in the settings folder: the generic dashboard
//! and one per ship (schema version 2). A version 1 file (the TypeScript service's format, one
//! dashboard) is read as the generic dashboard; it is kept as dashboards.json.bak when the first
//! change writes version 2. A file newer than this build is never read or written over.
//!
//! Tab changes act on the dashboard on screen; a change to one tab finds it by id in any
//! dashboard, since tab ids are unique across them (a detached window names one tab).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

use sc_core::js;

use super::boards::{FileRead, free_id, read_file, slug, starter_for};
use super::model::{
    Board, DEFAULT_ROWS, DashFile, Dashboard, GENERIC_ID, MAX_BOARDS, MAX_LABEL, MAX_TABS, ShipRef,
    Tab, manufacturer_theme, starter_file,
};
use super::themes::DEFAULT_THEME;
use super::validate::{
    Known, check_bezel, check_cell, check_name, check_placement, check_tab, check_theme_ref,
};
use crate::json::{int_in, to_pretty_text};

/// What a tab change answers: the reasons it was refused, and the tab it made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<Tab>,
}

impl Change {
    fn refused(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
            tab: None,
        }
    }

    fn done(tab: Option<Tab>) -> Self {
        Self {
            problems: vec![],
            tab,
        }
    }
}

/// A dashboard as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ship: Option<ShipRef>,
    /// The ship's friendly name, for the picker.
    #[serde(rename = "shipName", skip_serializing_if = "Option::is_none")]
    pub ship_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub generic: bool,
    pub tabs: usize,
}

impl BoardInfo {
    fn of(b: &Board) -> Self {
        Self {
            id: b.id.clone(),
            name: b.name.clone(),
            ship: b.ship.clone(),
            ship_name: b.ship.as_ref().map(ShipRef::derived_name),
            theme: b.theme.clone(),
            generic: b.is_generic(),
            tabs: b.tabs.len(),
        }
    }
}

/// What a dashboard change answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardChange {
    pub problems: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<BoardInfo>,
}

impl BoardChange {
    fn refused(problem: impl Into<String>) -> Self {
        Self {
            problems: vec![problem.into()],
            dashboard: None,
        }
    }

    fn done(board: Option<&Board>) -> Self {
        Self {
            problems: vec![],
            dashboard: board.map(BoardInfo::of),
        }
    }
}

/// The picker's model: every dashboard, the one on screen and the switching setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardList {
    pub dashboards: Vec<BoardInfo>,
    pub active: String,
    pub switch_on_ship_change: bool,
    /// The dashboard was picked by hand since the last ship change.
    pub pinned: bool,
}

fn quote(s: &str) -> String {
    serde_json::to_string(&js::utf16_prefix(s, 60)).expect("a string serializes")
}

fn too_many_tabs() -> String {
    format!("At most {MAX_TABS} tabs")
}

fn no_board(id: &str) -> String {
    format!("No dashboard {}", quote(id))
}

#[derive(Debug)]
struct Inner {
    file: DashFile,
    pinned: bool,
    /// The version 1 file as read, until it is kept as .bak by the first write.
    v1_text: Option<String>,
}

impl Inner {
    fn active_index(&self) -> usize {
        self.file
            .dashboards
            .iter()
            .position(|b| b.id == self.file.active)
            .unwrap_or(0)
    }

    fn active(&self) -> &Board {
        &self.file.dashboards[self.active_index()]
    }

    fn active_mut(&mut self) -> &mut Board {
        let i = self.active_index();
        &mut self.file.dashboards[i]
    }

    fn board(&self, id: &str) -> Option<&Board> {
        self.file.dashboards.iter().find(|b| b.id == id)
    }

    /// The dashboard and position of a tab, in any dashboard.
    fn find_tab(&self, id: &str) -> Option<(usize, usize)> {
        self.file
            .dashboards
            .iter()
            .enumerate()
            .find_map(|(b, d)| d.tabs.iter().position(|t| t.id == id).map(|t| (b, t)))
    }

    fn tab_mut(&mut self, id: &str) -> Option<&mut Tab> {
        let (b, t) = self.find_tab(id)?;
        Some(&mut self.file.dashboards[b].tabs[t])
    }

    fn tab_taken(&self, id: &str) -> bool {
        self.find_tab(id).is_some()
    }
}

/// Changes are made in memory first; an `Err` means only the file write failed.
#[derive(Debug)]
pub struct DashboardStore {
    inner: Mutex<Inner>,
    file: Option<PathBuf>,
    /// Why the file is not written: it is from a newer version. Shown on the page.
    problem: Option<String>,
    changes: watch::Sender<u64>,
}

impl Default for DashboardStore {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DashboardStore {
    /// Kept in `file` when given, in memory only otherwise.
    pub fn new(file: Option<PathBuf>) -> Self {
        let (loaded, problem) = file.as_deref().map_or((None, None), load);
        let (dash, v1_text) = loaded.unwrap_or_else(|| (starter_file(), None));
        Self {
            inner: Mutex::new(Inner {
                file: dash,
                pinned: false,
                v1_text,
            }),
            file,
            problem,
            changes: watch::Sender::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The dashboard on screen, in the version 1 shape the page draws and the export writes.
    pub fn get(&self) -> Dashboard {
        self.lock().active().dashboard()
    }

    /// The whole file as it is written.
    pub fn file(&self) -> DashFile {
        self.lock().file.clone()
    }

    pub fn active_board(&self) -> Board {
        self.lock().active().clone()
    }

    /// The generic dashboard.
    pub fn generic(&self) -> Board {
        self.lock().file.dashboards[0].clone()
    }

    /// Why dashboards.json is not read or written (a newer version's file), when it is not.
    pub fn file_problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn list(&self) -> BoardList {
        let inner = self.lock();
        BoardList {
            dashboards: inner.file.dashboards.iter().map(BoardInfo::of).collect(),
            active: inner.active().id.clone(),
            switch_on_ship_change: inner.file.switch_on_ship_change,
            pinned: inner.pinned,
        }
    }

    /// Every tab of every dashboard.
    pub fn all_tabs(&self) -> Vec<Tab> {
        self.lock()
            .file
            .dashboards
            .iter()
            .flat_map(|b| b.tabs.iter().cloned())
            .collect()
    }

    /// A tab by id, in any dashboard.
    pub fn tab(&self, id: &str) -> Option<Tab> {
        let inner = self.lock();
        let (b, t) = inner.find_tab(id)?;
        Some(inner.file.dashboards[b].tabs[t].clone())
    }

    /// Changes after every commit.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Replace a tab's layout, or add the tab to the dashboard on screen when its id is new. A
    /// saved tab keeps its stored placement and bezel mapping: those change only through
    /// `set_placement` and `set_bezel`, so a window editing the layout never undoes a mapping
    /// learned in another. Returns the reasons it was refused.
    pub fn save_tab(&self, raw: &Value, known: &Known) -> io::Result<Vec<String>> {
        let check = check_tab(raw, known, true);
        let Some(tab) = check.tab else {
            return Ok(check.problems);
        };
        let mut inner = self.lock();
        if let Some(old) = inner.tab_mut(&tab.id) {
            *old = Tab {
                placement: old.placement.take(),
                bezel: old.bezel.take(),
                placement_after_bezel: false,
                ..tab
            };
        } else if inner.active().tabs.len() >= MAX_TABS {
            return Ok(vec![too_many_tabs()]);
        } else {
            inner.active_mut().tabs.push(tab);
        }
        self.commit(inner)?;
        Ok(vec![])
    }

    /// Set or forget (`null`) where a detached tab goes; nothing else on the tab changes.
    pub fn set_placement(&self, id: &str, raw: Option<&Value>) -> io::Result<Change> {
        let mut inner = self.lock();
        let Some(tab) = inner.tab_mut(id) else {
            return Ok(Change::refused(format!("No tab {}", quote(id))));
        };
        if raw == Some(&Value::Null) {
            tab.placement = None;
            tab.placement_after_bezel = false;
        } else {
            let check = check_placement(raw);
            let mut problems = check.problems;
            problems.extend(
                check
                    .ignored
                    .iter()
                    .map(|k| format!("placement field {} is not allowed", quote(k))),
            );
            let (Some(placement), true) = (check.placement, problems.is_empty()) else {
                return Ok(Change {
                    problems,
                    tab: None,
                });
            };
            if tab.placement.is_none() {
                tab.placement_after_bezel = tab.bezel.is_some();
            }
            tab.placement = Some(placement);
        }
        let tab = tab.clone();
        self.commit(inner)?;
        Ok(Change::done(Some(tab)))
    }

    /// Set or clear (`null`) a tab's bezel mapping; nothing else on the tab changes.
    pub fn set_bezel(&self, id: &str, raw: Option<&Value>, known: &Known) -> io::Result<Change> {
        let mut inner = self.lock();
        let Some(tab) = inner.tab_mut(id) else {
            return Ok(Change::refused(format!("No tab {}", quote(id))));
        };
        if raw == Some(&Value::Null) {
            tab.bezel = None;
            tab.placement_after_bezel = false;
        } else {
            let check = check_bezel(raw, known, true);
            let Some(bezel) = check.bezel else {
                return Ok(Change {
                    problems: check.problems,
                    tab: None,
                });
            };
            if tab.bezel.is_none() {
                tab.placement_after_bezel = false;
            }
            tab.bezel = Some(bezel);
        }
        let tab = tab.clone();
        self.commit(inner)?;
        Ok(Change::done(Some(tab)))
    }

    pub fn add_tab(&self, name: Option<&Value>) -> io::Result<Change> {
        self.add_tab_in(name, None, &Known::default())
    }

    /// A new tab drawn in `theme`, the theme in use where it was added: the tab keeps it as its
    /// own only when the dashboard's theme is another.
    pub fn add_tab_in(
        &self,
        name: Option<&Value>,
        theme: Option<&Value>,
        known: &Known,
    ) -> io::Result<Change> {
        let theme = match theme {
            None | Some(Value::Null) => None,
            Some(raw) => {
                let problems = check_theme_ref(raw, known);
                if !problems.is_empty() {
                    return Ok(Change {
                        problems,
                        tab: None,
                    });
                }
                raw.as_str().map(str::to_owned)
            }
        };
        let mut inner = self.lock();
        if inner.active().tabs.len() >= MAX_TABS {
            return Ok(Change::refused(too_many_tabs()));
        }
        let mut n = inner.active().tabs.len() + 1;
        while inner.tab_taken(&format!("tab-{n}")) {
            n += 1;
        }
        let wanted = name
            .and_then(Value::as_str)
            .map(js::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| format!("Tab {n}"), str::to_owned);
        let problems = check_name(Some(&Value::String(wanted.clone())), "The name");
        if !problems.is_empty() {
            return Ok(Change {
                problems,
                tab: None,
            });
        }
        let board_theme = inner.active().theme.as_deref().unwrap_or(DEFAULT_THEME);
        let tab = Tab {
            id: format!("tab-{n}"),
            name: wanted,
            rows: DEFAULT_ROWS,
            theme: theme.filter(|t| t != board_theme),
            ..Tab::default()
        };
        inner.active_mut().tabs.push(tab.clone());
        self.commit(inner)?;
        Ok(Change::done(Some(tab)))
    }

    pub fn remove_tab(&self, id: &str) -> io::Result<Change> {
        let mut inner = self.lock();
        let Some((b, t)) = inner.find_tab(id) else {
            return Ok(Change::refused(format!("No tab {}", quote(id))));
        };
        let board = &mut inner.file.dashboards[b];
        if board.tabs.len() == 1 {
            return Ok(Change::refused("The last tab cannot be removed"));
        }
        board.tabs.remove(t);
        self.commit(inner)?;
        Ok(Change::done(None))
    }

    /// A copy right after the original, named "<name> copy".
    pub fn duplicate_tab(&self, id: &str) -> io::Result<Change> {
        let mut inner = self.lock();
        let Some((b, i)) = inner.find_tab(id) else {
            return Ok(Change::refused(format!("No tab {}", quote(id))));
        };
        if inner.file.dashboards[b].tabs.len() >= MAX_TABS {
            return Ok(Change::refused(too_many_tabs()));
        }
        let src = inner.file.dashboards[b].tabs[i].clone();
        // Ids are ASCII (checked on the way in), so a byte cut is a character cut.
        let base = &src.id[..src.id.len().min(34)];
        let mut new_id = format!("{base}-copy");
        let mut n = 2;
        while inner.tab_taken(&new_id) {
            new_id = format!("{base}-copy{n}");
            n += 1;
        }
        // The copy keeps the bezel mapping but not the display, so a restore never stacks two
        // tabs on one display.
        let copy = Tab {
            id: new_id,
            name: format!("{} copy", js::utf16_prefix(&src.name, MAX_LABEL - 5)),
            placement: None,
            placement_after_bezel: false,
            ..src
        };
        inner.file.dashboards[b].tabs.insert(i + 1, copy.clone());
        self.commit(inner)?;
        Ok(Change::done(Some(copy)))
    }

    pub fn move_tab(&self, id: &str, to: Option<&Value>) -> io::Result<Change> {
        let mut inner = self.lock();
        let Some((b, from)) = inner.find_tab(id) else {
            return Ok(Change::refused(format!("No tab {}", quote(id))));
        };
        let tabs = &mut inner.file.dashboards[b].tabs;
        let last = i64::try_from(tabs.len()).unwrap_or(i64::MAX) - 1;
        let Some(to) = int_in(to, 0, last).and_then(|t| usize::try_from(t).ok()) else {
            return Ok(Change::refused("No such tab position"));
        };
        let tab = tabs.remove(from);
        tabs.insert(to, tab);
        self.commit(inner)?;
        Ok(Change::done(None))
    }

    /// Set the theme of the dashboard on screen by name, or go back to the default (`null`). The
    /// default is written as its name, so the stored theme is always the one the picker shows.
    pub fn set_theme(&self, raw: Option<&Value>, known: &Known) -> io::Result<Change> {
        let raw = raw.unwrap_or(&Value::Null);
        let theme = if raw.is_null() {
            Some(DEFAULT_THEME.to_owned())
        } else {
            let problems = check_theme_ref(raw, known);
            if !problems.is_empty() {
                return Ok(Change {
                    problems,
                    tab: None,
                });
            }
            raw.as_str().map(str::to_owned)
        };
        let mut inner = self.lock();
        inner.active_mut().theme = theme;
        self.commit(inner)?;
        Ok(Change::done(None))
    }

    /// Set the cell size of the dashboard on screen, in px (one of [`CELLS`]).
    pub fn set_cell(&self, raw: Option<&Value>) -> io::Result<Change> {
        let cell = match check_cell(raw.unwrap_or(&Value::Null)) {
            Ok(c) => c,
            Err(p) => return Ok(Change::refused(p)),
        };
        let mut inner = self.lock();
        if inner.active().cell == Some(cell) {
            return Ok(Change::done(None));
        }
        inner.active_mut().cell = Some(cell);
        self.commit(inner)?;
        Ok(Change::done(None))
    }

    /// Forget every reference to a removed theme, so those tabs fall back to their dashboard's
    /// theme, then RSI.
    pub fn drop_theme(&self, id: &str) -> io::Result<()> {
        let mut inner = self.lock();
        let mut changed = false;
        for board in &mut inner.file.dashboards {
            if board.theme.as_deref() == Some(id) {
                board.theme = None;
                changed = true;
            }
            for tab in &mut board.tabs {
                if tab.theme.as_deref() == Some(id) {
                    tab.theme = None;
                    changed = true;
                }
            }
        }
        if changed {
            self.commit(inner)?;
        }
        Ok(())
    }

    /// Put the starter tab back as shipped on the dashboard on screen, in its place or first if
    /// it was removed.
    pub fn reset_starter(&self) -> io::Result<Change> {
        let mut inner = self.lock();
        let (at, starter) = starter_for(inner.active(), |id| inner.tab_taken(id));
        let board = inner.active_mut();
        match at {
            Some(i) => board.tabs[i] = starter.clone(),
            None if board.tabs.len() >= MAX_TABS => return Ok(Change::refused(too_many_tabs())),
            None => board.tabs.insert(0, starter.clone()),
        }
        self.commit(inner)?;
        Ok(Change::done(Some(starter)))
    }

    /// Replace the dashboard on screen with one already read by `read_dashboard`. A tab id that
    /// another dashboard uses is renamed.
    pub fn replace(&self, next: Dashboard) -> io::Result<()> {
        let mut inner = self.lock();
        let i = inner.active_index();
        let others: Vec<String> = inner
            .file
            .dashboards
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .flat_map(|(_, b)| b.tabs.iter().map(|t| t.id.clone()))
            .collect();
        let mut tabs = next.tabs;
        let mut used: Vec<String> = tabs.iter().map(|t| t.id.clone()).collect();
        for tab in &mut tabs {
            if others.contains(&tab.id) {
                let id = free_id(&tab.id, |x| others.iter().chain(&used).any(|y| y == x));
                tracing::info!("[dashboards] import: tab {} is now {id}", tab.id);
                used.push(id.clone());
                tab.id = id;
            }
        }
        let board = &mut inner.file.dashboards[i];
        board.theme = next.theme;
        board.cell = next.cell;
        board.tabs = tabs;
        self.commit(inner)
    }

    /// Show a dashboard. `manual`: picked by hand, so it stays until the next ship change.
    pub fn select(&self, id: &str, manual: bool) -> io::Result<BoardChange> {
        let mut inner = self.lock();
        if inner.board(id).is_none() {
            return Ok(BoardChange::refused(no_board(id)));
        }
        inner.pinned = manual;
        if inner.file.active == id {
            drop(inner);
            self.changes.send_modify(|n| *n += 1);
            return Ok(BoardChange::done(None));
        }
        inner.file.active = id.to_owned();
        self.commit(inner)?;
        Ok(BoardChange::done(None))
    }

    /// A new ship's dashboard is no longer a hand pick.
    pub fn unpin(&self) {
        let mut inner = self.lock();
        if inner.pinned {
            inner.pinned = false;
            drop(inner);
            self.changes.send_modify(|n| *n += 1);
        }
    }

    pub fn set_switch(&self, raw: Option<&Value>) -> io::Result<BoardChange> {
        let Some(on) = raw.and_then(Value::as_bool) else {
            return Ok(BoardChange::refused("on must be true or false"));
        };
        let mut inner = self.lock();
        inner.file.switch_on_ship_change = on;
        self.commit(inner)?;
        Ok(BoardChange::done(None))
    }

    pub fn switch_on_ship_change(&self) -> bool {
        self.lock().file.switch_on_ship_change
    }

    /// The first dashboard for this ship: by its exact class name, else by manufacturer and
    /// model (case does not matter).
    pub fn match_ship(&self, ship: &ShipRef) -> Option<String> {
        let inner = self.lock();
        let boards = &inner.file.dashboards;
        let exact = boards.iter().find(|b| {
            b.ship
                .as_ref()
                .is_some_and(|s| s.class_name.is_some() && s.class_name == ship.class_name)
        });
        let loose = || {
            boards.iter().find(|b| {
                b.ship.as_ref().is_some_and(|s| {
                    s.model.eq_ignore_ascii_case(&ship.model)
                        && s.manufacturer.eq_ignore_ascii_case(&ship.manufacturer)
                })
            })
        };
        exact.or_else(loose).map(|b| b.id.clone())
    }

    /// A dashboard for a ship, copied from `from` (the generic one by default) with its tab
    /// names, and shown. The theme is the manufacturer's where there is one, else the copy's.
    pub fn create_for_ship(
        &self,
        ship: ShipRef,
        from: Option<&str>,
        name: Option<&Value>,
        known: &Known,
    ) -> io::Result<BoardChange> {
        let mut inner = self.lock();
        let from = from.unwrap_or(GENERIC_ID);
        let Some(src) = inner.board(from).cloned() else {
            return Ok(BoardChange::refused(no_board(from)));
        };
        if inner.file.dashboards.len() >= MAX_BOARDS {
            return Ok(BoardChange::refused(format!(
                "At most {MAX_BOARDS} dashboards"
            )));
        }
        let default_name = ship.derived_name();
        let name = match name_or(name, &default_name) {
            Ok(n) => n,
            Err(p) => {
                return Ok(BoardChange {
                    problems: p,
                    dashboard: None,
                });
            }
        };
        let theme_known = |t: &str| known.themes.as_ref().is_none_or(|k| k.contains(t));
        let theme = manufacturer_theme(&ship)
            .filter(|t| theme_known(t))
            .map(str::to_owned)
            .or(src.theme.clone());
        let base = slug(ship.class_name.as_deref().unwrap_or(&default_name));
        let board = self.copy_board(&inner, &src, &base, name, Some(ship), theme);
        inner.file.active.clone_from(&board.id);
        inner.pinned = true;
        inner.file.dashboards.push(board.clone());
        self.commit(inner)?;
        Ok(BoardChange::done(Some(&board)))
    }

    /// A copy of a dashboard, for the same ship, named "<name> copy", and shown.
    pub fn duplicate(&self, id: &str, name: Option<&Value>) -> io::Result<BoardChange> {
        let mut inner = self.lock();
        let Some(src) = inner.board(id).cloned() else {
            return Ok(BoardChange::refused(no_board(id)));
        };
        if inner.file.dashboards.len() >= MAX_BOARDS {
            return Ok(BoardChange::refused(format!(
                "At most {MAX_BOARDS} dashboards"
            )));
        }
        let default_name = format!("{} copy", js::utf16_prefix(&src.name, MAX_LABEL - 5));
        let name = match name_or(name, &default_name) {
            Ok(n) => n,
            Err(p) => {
                return Ok(BoardChange {
                    problems: p,
                    dashboard: None,
                });
            }
        };
        let base = format!("{}-copy", &src.id[..src.id.len().min(30)]);
        let board = self.copy_board(
            &inner,
            &src,
            &base,
            name,
            src.ship.clone(),
            src.theme.clone(),
        );
        let at = inner
            .file
            .dashboards
            .iter()
            .position(|b| b.id == id)
            .unwrap_or(0)
            + 1;
        inner.file.active.clone_from(&board.id);
        inner.pinned = true;
        inner.file.dashboards.insert(at, board.clone());
        self.commit(inner)?;
        Ok(BoardChange::done(Some(&board)))
    }

    /// The copy keeps every tab's name, layout, display and bezel mapping; each tab gets an id
    /// of its own, `<dashboard>-<tab>`.
    #[allow(clippy::unused_self)]
    fn copy_board(
        &self,
        inner: &Inner,
        src: &Board,
        base: &str,
        name: String,
        ship: Option<ShipRef>,
        theme: Option<String>,
    ) -> Board {
        let id = free_id(base, |id| inner.board(id).is_some());
        let mut taken: Vec<String> = Vec::new();
        let tabs = src
            .tabs
            .iter()
            .map(|t| {
                let wanted = format!(
                    "{}-{}",
                    &id[..id.len().min(19)],
                    &t.id[..t.id.len().min(20)]
                );
                let tab_id = free_id(&wanted, |x| {
                    inner.tab_taken(x) || taken.iter().any(|y| y == x)
                });
                taken.push(tab_id.clone());
                Tab {
                    id: tab_id,
                    ..t.clone()
                }
            })
            .collect();
        Board {
            id,
            name,
            ship,
            theme,
            cell: src.cell,
            tabs,
        }
    }

    pub fn rename(&self, id: &str, name: Option<&Value>) -> io::Result<BoardChange> {
        let problems = check_name(name, "The name");
        if !problems.is_empty() {
            return Ok(BoardChange {
                problems,
                dashboard: None,
            });
        }
        let mut inner = self.lock();
        let Some(board) = inner.file.dashboards.iter_mut().find(|b| b.id == id) else {
            return Ok(BoardChange::refused(no_board(id)));
        };
        board.name = js::trim(name.and_then(Value::as_str).unwrap_or_default()).to_owned();
        let info = BoardInfo::of(board);
        self.commit(inner)?;
        Ok(BoardChange {
            problems: vec![],
            dashboard: Some(info),
        })
    }

    /// Remove a ship's dashboard; the generic one stays. Removing the one on screen shows the
    /// generic one.
    pub fn remove(&self, id: &str) -> io::Result<BoardChange> {
        if id == GENERIC_ID {
            return Ok(BoardChange::refused(
                "The generic dashboard cannot be removed",
            ));
        }
        let mut inner = self.lock();
        let Some(i) = inner.file.dashboards.iter().position(|b| b.id == id) else {
            return Ok(BoardChange::refused(no_board(id)));
        };
        inner.file.dashboards.remove(i);
        if inner.file.active == id {
            inner.file.active = GENERIC_ID.to_owned();
        }
        self.commit(inner)?;
        Ok(BoardChange::done(None))
    }

    /// Write the file (temp file and rename), then tell the listeners, after the lock is released.
    /// A version 1 file is first kept as dashboards.json.bak. Nothing is written over a newer
    /// version's file.
    fn commit(&self, mut inner: MutexGuard<'_, Inner>) -> io::Result<()> {
        if let (Some(file), None) = (&self.file, &self.problem) {
            if let Some(v1) = &inner.v1_text {
                write_atomic(&bak_path(file), v1)?;
            }
            write_atomic(file, &to_pretty_text(&inner.file))?;
            inner.v1_text = None;
        }
        drop(inner);
        self.changes.send_modify(|n| *n += 1);
        Ok(())
    }
}

fn name_or(name: Option<&Value>, default: &str) -> Result<String, Vec<String>> {
    match name
        .and_then(Value::as_str)
        .map(js::trim)
        .filter(|s| !s.is_empty())
    {
        Some(n) => {
            let problems = check_name(Some(&Value::String(n.to_owned())), "The name");
            if problems.is_empty() {
                Ok(n.to_owned())
            } else {
                Err(problems)
            }
        }
        None => Ok(js::trim(&js::utf16_prefix(default, MAX_LABEL)).to_owned()),
    }
}

pub fn bak_path(file: &Path) -> PathBuf {
    let mut bak = OsString::from(file.as_os_str());
    bak.push(".bak");
    PathBuf::from(bak)
}

type Loaded = (Option<(DashFile, Option<String>)>, Option<String>);

/// The file and, for a version 1 file, its text (kept as .bak by the first write); or why it is
/// not to be written (a newer version's file).
fn load(file: &Path) -> Loaded {
    // An unreadable file falls back to the starter; the next change rewrites it.
    let Ok(text) = fs::read_to_string(file) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        tracing::warn!(
            "[dashboards] {}: not JSON; the starter is used",
            file.display()
        );
        return (None, None);
    };
    match read_file(&value, &Known::default()) {
        FileRead::Read {
            file: dash,
            from_v1,
            skipped,
        } => {
            if !skipped.is_empty() {
                tracing::warn!(
                    "[dashboards] {}: left out {}:\n  {}",
                    file.display(),
                    skipped.len(),
                    skipped.join("\n  ")
                );
            }
            (Some((dash, from_v1.then_some(text))), None)
        }
        FileRead::Newer(problem) => {
            tracing::warn!("[dashboards] {}: {problem}", file.display());
            (None, Some(problem))
        }
        FileRead::Unreadable(problem) => {
            tracing::warn!(
                "[dashboards] {}: {problem}; the starter is used",
                file.display()
            );
            (None, None)
        }
    }
}

fn write_atomic(file: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(file.as_os_str());
    tmp.push(".tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, file)
}
