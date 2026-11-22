//! Dashboards: tabs of widgets on a fixed 24-column grid, the store that keeps them, and the
//! values model the dashboard page draws from.

mod boards;
mod model;
mod store;
mod themes;
mod validate;
mod values;

pub use boards::{FileRead, check_ship, free_id, read_file, slug};
pub use model::{
    Bezel, BezelBind, BezelButton, Bind, BindKind, Board, COLUMNS, DEFAULT_ROWS, DashFile,
    Dashboard, FILE_SCHEMA_VERSION, GENERIC_ID, KindDef, KindTable, LEVER_SIZE, LOOK_ONLY_FOR,
    LookOnlyFor, MANUFACTURER_THEMES, MAX_BEZEL_BUTTONS, MAX_BOARDS, MAX_BUTTON_INDEX,
    MAX_CLASS_NAME, MAX_DISPLAY_LABEL, MAX_GAMEPAD_ID, MAX_LABEL, MAX_ROWS, MAX_SHIP_TEXT,
    MAX_TABS, MAX_WIDGETS, MIN_ROWS, Mode, OnOff, Placement, SCHEMA_VERSION, ShipRef, Size, Tab,
    WIDGET_KINDS, Widget, kind_def, lamp_values, look_only_for, manufacturer_theme, starter,
    starter_file, starter_tab,
};
pub use store::{BoardChange, BoardInfo, BoardList, Change, DashboardStore, bak_path};
pub use themes::{
    BAR_STYLES, BASES, BUILT_IN_THEMES, COLOR_TOKENS, Colors, DEFAULT_THEME, FONTS,
    INPUT_FALLBACKS, MAX_THEME_NAME, MAX_USER_THEMES, THEME_SCHEMA_VERSION, Theme, ThemeChange,
    ThemeStore, built_in_css, built_in_theme, check_theme, check_theme_name, contrast, is_built_in,
    is_hex_color, theme_css,
};
pub use validate::{
    BezelCheck, ImportSummary, Known, PlacementCheck, TabCheck, WidgetCheck, check_bezel,
    check_name, check_placement, check_tab, check_theme_ref, check_widget, read_dashboard,
};
pub use values::{LampState, build_values, build_values_with};

#[cfg(test)]
mod boards_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod theme_tests;
