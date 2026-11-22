//! Displays and how a tab's remembered display is found again: by its identity, the monitor's
//! friendly name (from its EDID, as Windows reports it), never by its number, which Windows
//! renumbers. The GDI device name (`\\.\DISPLAY2`) is kept only to find the live monitor handle at
//! the moment a window opens; it is never stored or matched against a placement.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Display {
    /// The monitor's friendly name: what a placement's `displayLabel` holds.
    pub label: String,
    /// The GDI device name, for this session only.
    #[serde(skip)]
    pub device: String,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub scale: f32,
    pub primary: bool,
    pub builtin: bool,
}

#[derive(Debug, PartialEq)]
pub enum DisplayMatch<'a> {
    Found(&'a Display),
    /// No display has this name: it is not connected.
    Missing,
    /// More than one display has this name, so it cannot tell them apart; the app does not guess.
    Ambiguous(Vec<&'a Display>),
}

impl DisplayMatch<'_> {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Found(_) => "open",
            Self::Missing => "missing",
            Self::Ambiguous(_) => "ambiguous",
        }
    }
}

fn normalise(label: &str) -> String {
    label.trim().to_lowercase()
}

/// The display a placement names. An exact name wins; otherwise the name compared without case
/// or surrounding space (a placement remembered by a browser may differ only in that).
pub fn find<'a>(displays: &'a [Display], label: &str) -> DisplayMatch<'a> {
    let pick = |hits: Vec<&'a Display>| match hits.len() {
        0 => None,
        1 => Some(DisplayMatch::Found(hits[0])),
        _ => Some(DisplayMatch::Ambiguous(hits)),
    };
    // A display with no name has no identity to find it by.
    if label.trim().is_empty() {
        return DisplayMatch::Missing;
    }
    let exact = displays.iter().filter(|d| d.label == label).collect();
    if let Some(m) = pick(exact) {
        return m;
    }
    let wanted = normalise(label);
    let loose = displays
        .iter()
        .filter(|d| normalise(&d.label) == wanted)
        .collect();
    pick(loose).unwrap_or(DisplayMatch::Missing)
}

/// Labels two or more displays share; a placement on one of them cannot be restored.
pub fn shared_labels(displays: &[Display]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in displays {
        let n = displays.iter().filter(|o| o.label == d.label).count();
        if n > 1 && !out.contains(&d.label) {
            out.push(d.label.clone());
        }
    }
    out
}

#[cfg(test)]
pub(crate) fn display(label: &str, device: &str, left: i32) -> Display {
    Display {
        label: label.into(),
        device: device.into(),
        width: 1024,
        height: 768,
        left,
        top: 0,
        scale: 1.0,
        primary: left == 0,
        builtin: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_name_wherever_windows_numbered_it() {
        let before = [
            display("DELL U2720Q", r"\\.\DISPLAY1", 0),
            display("MFD Left", r"\\.\DISPLAY2", 3840),
        ];
        // After a replug Windows renumbers: the MFD is now DISPLAY1 and comes first.
        let after = [
            display("MFD Left", r"\\.\DISPLAY1", 3840),
            display("DELL U2720Q", r"\\.\DISPLAY2", 0),
        ];
        for set in [&before[..], &after[..]] {
            match find(set, "MFD Left") {
                DisplayMatch::Found(d) => assert_eq!(d.left, 3840),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn never_matches_by_number_or_device_name() {
        let set = [display("MFD Left", r"\\.\DISPLAY1", 0)];
        assert_eq!(find(&set, r"\\.\DISPLAY1"), DisplayMatch::Missing);
        assert_eq!(find(&set, "1"), DisplayMatch::Missing);
        assert_eq!(find(&set, "0"), DisplayMatch::Missing);
    }

    #[test]
    fn a_display_with_no_name_is_never_found() {
        let set = [display("", r"\\.\DISPLAY1", 0)];
        assert_eq!(find(&set, ""), DisplayMatch::Missing);
    }

    #[test]
    fn missing_display() {
        let set = [display("DELL U2720Q", r"\\.\DISPLAY1", 0)];
        assert_eq!(find(&set, "MFD Left"), DisplayMatch::Missing);
        assert_eq!(find(&[], "MFD Left"), DisplayMatch::Missing);
        assert_eq!(find(&set, "  "), DisplayMatch::Missing);
    }

    #[test]
    fn loose_match_only_when_no_exact_one() {
        let set = [
            display("mfd left", r"\\.\DISPLAY1", 0),
            display("MFD Left", r"\\.\DISPLAY2", 1024),
        ];
        assert_eq!(find(&set, "MFD Left"), DisplayMatch::Found(&set[1]));
        let one = [display("MFD Left", r"\\.\DISPLAY1", 0)];
        assert_eq!(find(&one, " mfd left "), DisplayMatch::Found(&one[0]));
    }

    #[test]
    fn two_displays_with_one_name_are_not_guessed() {
        let set = [
            display("Generic PnP Monitor", r"\\.\DISPLAY1", 0),
            display("Generic PnP Monitor", r"\\.\DISPLAY2", 1024),
        ];
        match find(&set, "Generic PnP Monitor") {
            DisplayMatch::Ambiguous(hits) => assert_eq!(hits.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(shared_labels(&set), vec!["Generic PnP Monitor".to_owned()]);
    }
}
