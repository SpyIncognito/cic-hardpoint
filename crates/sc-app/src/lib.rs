//! Hardpoint, the desktop app: the control service in-process, the dashboard in a native window,
//! detached tabs in native windows on their displays, bezel buttons read natively, and a tray icon.
//!
//! The windowing (tao, wry on WebView2, tray-icon, gilrs) is Windows only and lives in the binary.
//! Everything it decides with is here, platform free and tested: which display a tab goes on, which
//! controller and button a bezel mapping means, the page bridge, the app's portal session, the
//! telemetry recorder, the single-instance guard, the app's own settings file and its artwork.

pub mod bezel;
pub mod brand;
pub mod bridge;
pub mod client;
pub mod displays;
pub mod instance;
pub mod model;
pub mod recorder;
pub mod settings;
