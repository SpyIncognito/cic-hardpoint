//! The displays as Windows reports them, through display-info (its unsafe Win32 calls stay in
//! that crate). The label is the monitor's friendly name from its EDID
//! (`DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorFriendlyDeviceName`), the name the Window Management
//! API in Edge and WebView2 is expected to give as `ScreenDetailed.label` (not checked on a real PC yet).

use sc_app::displays::Display;

/// display-info's fallback when Windows gives no name; it carries a handle number, which is not an
/// identity, so such a display gets no label and cannot be remembered.
const NO_NAME: &str = "Unknown Display ";

pub fn list() -> Vec<Display> {
    match display_info::DisplayInfo::all() {
        Ok(all) => all
            .into_iter()
            .map(|d| Display {
                label: if d.friendly_name.starts_with(NO_NAME) {
                    String::new()
                } else {
                    d.friendly_name
                },
                device: d.name,
                width: d.width,
                height: d.height,
                left: d.x,
                top: d.y,
                scale: d.scale_factor,
                primary: d.is_primary,
                builtin: d.is_builtin,
            })
            .collect(),
        Err(e) => {
            tracing::warn!("cannot list the displays: {e}");
            Vec::new()
        }
    }
}
