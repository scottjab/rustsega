//! Icon geometry, as inline SVG path data.
//!
//! Every icon is the `d` attribute of a `<path>` inside a 24x24 viewBox, drawn
//! stroke-first (`fill: none`, `stroke: currentColor`, 20px box, rounded caps
//! and joins - see `.icon` in `style.css`). They are plain data so the whole
//! set can be validated by a native test, and so no `.svg` file or `<script>`
//! is needed to render one.

/// One icon: the name the UI calls it by, and its path data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icon {
    pub name: &'static str,
    pub path: &'static str,
}

pub const OPEN: &str = "M3 7a2 2 0 0 1 2-2h4l2 3h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
pub const SETTINGS: &str = "M12 9a3 3 0 0 1 0 6 3 3 0 0 1 0-6 M12 3v2 M12 19v2 M3 12h2 M19 12h2 M5.6 5.6l1.5 1.5 M16.9 16.9l1.5 1.5 M18.4 5.6l-1.5 1.5 M7.1 16.9l-1.5 1.5";
pub const FULLSCREEN: &str = "M4 9V4h5 M20 9V4h-5 M4 15v5h5 M20 15v5h-5";
pub const PAUSE: &str = "M9 5v14 M15 5v14";
pub const PLAY: &str = "M7 4l13 8-13 8z";
pub const RESET: &str = "M20 12a8 8 0 1 1-2.4-5.7 M20 4v5h-5";
pub const SAVE: &str = "M6 4h10l3 3v13H6z M9 4v5h6 M9 20v-6h6v6";
pub const VOLUME: &str = "M4 9v6h4l5 4V5L8 9H4z M16.5 9.5a3.5 3.5 0 0 1 0 5 M19 7a7 7 0 0 1 0 10";
pub const VOLUME_OFF: &str = "M4 9v6h4l5 4V5L8 9H4z M16 9l5 6 M21 9l-5 6";
pub const GAMEPAD: &str = "M6 8h12a4 4 0 0 1 0 8c-1.5 0-2.3-.8-3-2H9c-.7 1.2-1.5 2-3 2a4 4 0 0 1 0-8z M7 11h4 M9 9v4 M16 12h.01 M18 10h.01";
pub const KEYBOARD: &str =
    "M3 7h18v10H3z M6.5 10.5h.01 M10 10.5h.01 M13.5 10.5h.01 M17 10.5h.01 M8 13.5h8";
pub const CLOSE: &str = "M6 6l12 12 M18 6 6 18";

/// Every icon the shell has, for validation and lookup.
pub const ALL: &[Icon] = &[
    Icon {
        name: "open",
        path: OPEN,
    },
    Icon {
        name: "settings",
        path: SETTINGS,
    },
    Icon {
        name: "fullscreen",
        path: FULLSCREEN,
    },
    Icon {
        name: "pause",
        path: PAUSE,
    },
    Icon {
        name: "play",
        path: PLAY,
    },
    Icon {
        name: "reset",
        path: RESET,
    },
    Icon {
        name: "save",
        path: SAVE,
    },
    Icon {
        name: "volume",
        path: VOLUME,
    },
    Icon {
        name: "volume-off",
        path: VOLUME_OFF,
    },
    Icon {
        name: "gamepad",
        path: GAMEPAD,
    },
    Icon {
        name: "keyboard",
        path: KEYBOARD,
    },
    Icon {
        name: "close",
        path: CLOSE,
    },
];

/// Looks an icon up by name.
pub fn by_name(name: &str) -> Option<&'static Icon> {
    ALL.iter().find(|icon| icon.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters allowed in path data: the SVG path commands, digits, a
    /// decimal point, a comma (argument separator) and a minus sign (negative
    /// coordinates). Anything else - a quote, a `<`, a URL - is not path data.
    fn is_path_char(c: char) -> bool {
        matches!(
            c,
            'M' | 'm'
                | 'L'
                | 'l'
                | 'H'
                | 'h'
                | 'V'
                | 'v'
                | 'C'
                | 'c'
                | 'S'
                | 's'
                | 'Q'
                | 'q'
                | 'T'
                | 't'
                | 'A'
                | 'a'
                | 'Z'
                | 'z'
                | '0'..='9' | ' ' | '.' | ',' | '-'
        )
    }

    #[test]
    fn every_icon_is_non_empty() {
        assert!(!ALL.is_empty(), "the icon set should not be empty");
        for icon in ALL {
            assert!(
                !icon.path.trim().is_empty(),
                "icon {} has no path data",
                icon.name
            );
        }
    }

    #[test]
    fn every_icon_uses_only_svg_path_characters() {
        for icon in ALL {
            for c in icon.path.chars() {
                assert!(
                    is_path_char(c),
                    "icon {} has path data outside the allowed set: {:?} in {:?}",
                    icon.name,
                    c,
                    icon.path
                );
            }
        }
    }

    #[test]
    fn every_icon_starts_with_a_move_command() {
        for icon in ALL {
            assert!(
                icon.path.starts_with('M') || icon.path.starts_with('m'),
                "icon {} should start with a move command",
                icon.name
            );
        }
    }

    #[test]
    fn the_icons_the_shell_needs_are_present() {
        for name in [
            "open",
            "settings",
            "fullscreen",
            "pause",
            "play",
            "reset",
            "save",
            "volume",
            "volume-off",
            "gamepad",
            "keyboard",
            "close",
        ] {
            assert!(by_name(name).is_some(), "missing icon: {name}");
        }
    }

    #[test]
    fn icon_names_are_unique() {
        let mut names: Vec<&str> = ALL.iter().map(|icon| icon.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two icons share a name");
    }
}
