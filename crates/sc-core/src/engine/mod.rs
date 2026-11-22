pub mod catalog;
mod executor;
mod result;
pub mod sequences;

pub use catalog::{
    CONFIRM_GROUP, MASTER_MODE, MasterMode, MasterModeDef, TOGGLES, ToggleDef, ToggleView,
    game_action_name, needs_confirm, toggle_by_id, toggle_for_action, view_for_action, view_toggle,
};
pub use executor::{Engine, EngineOptions};
pub use result::{ActionResult, FailStatus, Failure, OkStatus, ProgressStatus, SequenceProgress};
pub use sequences::{
    DEFAULT_SEQUENCES, MAX_NAME, MAX_PAUSE_S, MAX_STEPS, MAX_TIMEOUT_S, OnOff, Sequence,
    SequenceError, SequenceStore, Step, TargetMode, parse_sequence, validate,
};
