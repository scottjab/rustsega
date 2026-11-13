//! The one settings store the shell reads, and the JSON it keeps in
//! `localStorage` (M7-10, #143).
//!
//! Before this ticket the shell's settings were scattered signals: a
//! `RwSignal<Theme>` in `App`, an [`crate::audio`] pair for volume and mute, and a
//! [`crate::touch_events`] trio that #142 deliberately left unpersisted because
//! this ticket owns persistence. They are one struct here, and one
//! `RwSignal<Settings>` context - so the controls page, the top bar and the
//! drawer cannot disagree about what the volume is, and one `Effect` makes
//! all of it stick.
//!
//! Two rules hold the whole thing up, and both come from #387 - a panic inside
//! the animation-frame closure leaves the console `RefCell` borrowed and the page
//! is dead for good:
//!
//! * [`Settings::from_json`] never fails. Whatever a previous build, a hand
//!   edited profile, or a bit-rotted `localStorage` hands it, it returns a
//!   `Settings`; the browser half therefore has nothing to panic about at boot.
//! * Reading a field is not enough to be valid: [`Settings::from_json`] also
//!   clamps, so `volume` really is `0.0..=1.0` by the time anything multiplies a
//!   gain by it.
//!
//! The `#[serde(default)]` on every field is what makes "missing fields get
//! defaults" true per field rather than per document: a settings file written by
//! an older build, or one with a single wrongly typed field, keeps the fields
//! this build recognises and loses only the one it does not.

use serde::Deserialize;
use serde::Serialize;

use crate::input::Bindings;
use crate::input::PORTS;
use crate::input::SocdMode;
use crate::theme::Theme;

/// The `localStorage` key this whole struct lives under.
///
/// One key for one struct, versioned by [`Settings::version`] rather than by the
/// key, so a format change is a migration in `from_json` rather than a second
/// key nobody reads.
pub const STORAGE_KEY: &str = "rustsega.settings.v1";

/// The version written by this build, and the only one [`Settings::from_json`]
/// promises to understand field by field.
pub const SETTINGS_VERSION: u32 = 1;

/// The volume a new tab gets: loud enough to hear, quiet enough not to startle.
pub const DEFAULT_VOLUME: f64 = 0.8;

/// What kind of pad a port is pretending to be.
///
/// The Genesis reads a 6-button pad as a 3-button one until the game walks the
/// select line, so this is a property of the pad rather than of the game - and
/// [`crate::input`] currently reports the twelve buttons of a 6-button pad for
/// every port. [`genesis::Io::set_buttons_6`] is where a port's device type
/// actually lives, `Genesis` does not reach it, and that gap is #364; until it
/// closes this is a UI-only setting that the emulator does not act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadType {
    /// The original pad: d-pad, `A`, `B`, `Start`, and the Mode pin tied high.
    ThreeButton,
    /// The Mega Drive 2 pad, which adds `X`, `Y`, `Z` and `Mode`.
    #[default]
    SixButton,
}

impl PadType {
    /// Every pad type, in the order the settings select lists them.
    pub const ALL: [Self; 2] = [Self::ThreeButton, Self::SixButton];

    /// The label for this pad type in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::ThreeButton => "3-button",
            Self::SixButton => "6-button",
        }
    }
}

/// When the on-screen controller is shown.
///
/// The three-way form of what #142 (M7-09) stored as a `show` switch: `Auto` is
/// "what the browser said", `Always` is a desktop player trying the overlay on,
/// and `Never` is a touch machine whose player wants the picture back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchOverlayMode {
    /// Show it while the browser reports a coarse pointer.
    #[default]
    Auto,
    /// Show it whatever the pointer is.
    Always,
    /// Never show it.
    Never,
}

impl TouchOverlayMode {
    /// Every mode, in the order the settings select lists them.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Always, Self::Never];

    /// The label for this mode in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }
}

/// Everything the shell remembers between loads.
///
/// Plain data, `Copy` where it can be, and `Serialize`/`Deserialize` under
/// `snake_case` names that are part of the file on disk: like `PadButton`'s
/// variant names (M7-06, #139), these strings outlive this build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The colour theme, whose `data-theme` attribute is written by
    /// [`crate::theme::Theme::apply_to_document`].
    #[serde(default)]
    pub theme: Theme,
    /// Output volume, `0.0` to `1.0` as the drawer's slider has it.
    #[serde(default = "default_volume")]
    pub volume: f64,
    /// Whether the output is muted, which holds [`Settings::volume`] rather than
    /// zeroing it.
    #[serde(default)]
    pub muted: bool,
    /// The keyboard and controller bindings of both players.
    #[serde(default)]
    pub bindings: Bindings,
    /// The pad type of each control port, indexed the way [`crate::input`] indexes
    /// ports.
    #[serde(default)]
    pub pad_type: [PadType; PORTS],
    /// How a port resolves two directions held on one axis.
    #[serde(default)]
    pub socd: SocdMode,
    /// When the on-screen controller is on screen.
    #[serde(default)]
    pub touch_overlay: TouchOverlayMode,
    /// `navigator.vibrate` on an overlay press, on browsers that have it.
    #[serde(default = "default_haptics")]
    pub haptics: bool,
    /// Hide the overlay while a physical controller is connected, which is what a
    /// player holding a pad wants (#142).
    #[serde(default = "default_hide_touch_with_controller")]
    pub hide_touch_with_controller: bool,
    /// Which build wrote the file, so a later one can migrate it.
    #[serde(default = "default_version")]
    pub version: u32,
}

impl Default for Settings {
    /// The defaults a new tab gets: the shell's own theme answer, the volume the
    /// ticket sets, the pad-one bindings of [`Bindings::default`], a 6-button pad
    /// in each port, and the touch behaviour #142 shipped with.
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            volume: DEFAULT_VOLUME,
            muted: false,
            bindings: Bindings::default(),
            pad_type: [PadType::default(); PORTS],
            socd: SocdMode::default(),
            touch_overlay: TouchOverlayMode::default(),
            haptics: true,
            hide_touch_with_controller: true,
            version: SETTINGS_VERSION,
        }
    }
}

/// [`Settings::volume`]'s default, named so `serde` can reach it.
fn default_volume() -> f64 {
    DEFAULT_VOLUME
}

/// [`Settings::haptics`]'s default: on, because a phone that cannot buzz you
/// about a press has no way to say the overlay worked at all.
fn default_haptics() -> bool {
    true
}

/// [`Settings::hide_touch_with_controller`]'s default, on for the same reason
/// `#142` gave for the switch.
fn default_hide_touch_with_controller() -> bool {
    true
}

/// [`Settings::version`]'s default.
fn default_version() -> u32 {
    SETTINGS_VERSION
}

impl Settings {
    /// Reads a stored `Settings`, **never failing**.
    ///
    /// Garbage is [`Settings::default`]; a document is read field by field so one
    /// wrongly typed field costs only itself (see [`Settings::field`]); and the
    /// result is clamped, so a `volume` of `99` arrives as `1.0`. There is no
    /// error to handle, which is the point: this runs at boot, on the same page
    /// that #387 makes unrecoverable.
    pub fn from_json(text: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return Self::default();
        };
        Self::from_value(&value).normalized()
    }

    /// The JSON this is stored as.
    ///
    /// A `Settings` is data all the way down, so encoding cannot fail; the
    /// `unwrap_or_default` keeps that promise rather than introducing a `Result`
    /// the caller could only ignore.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The gain a console should put on its output: `volume`, or silence while
    /// muted. Mute holds the volume rather than replacing it, which is what the
    /// slider still shows.
    pub fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume as f32 }
    }

    /// Whether the on-screen controller belongs on screen.
    ///
    /// Wanted by the mode, and not cancelled by a controller having turned up.
    /// The controller rule only applies while it is switched on. `coarse` is
    /// [`crate::touch_events::coarse_pointer`], read once by the shell.
    pub fn touch_visible(&self, coarse: bool, controller_connected: bool) -> bool {
        let wanted = match self.touch_overlay {
            TouchOverlayMode::Auto => coarse,
            TouchOverlayMode::Always => true,
            TouchOverlayMode::Never => false,
        };
        wanted && !(self.hide_touch_with_controller && controller_connected)
    }

    /// One field of a stored document, or its default for anything this build
    /// cannot make sense of - a missing key, a `null`, a value of the wrong
    /// shape, or an enum variant from a build that has not been written yet.
    fn field<T>(value: &serde_json::Value, name: &str, default: impl FnOnce() -> T) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        value
            .get(name)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_else(default)
    }

    fn from_value(value: &serde_json::Value) -> Self {
        Self {
            theme: Self::field(value, "theme", Theme::default),
            volume: Self::field(value, "volume", default_volume),
            muted: Self::field(value, "muted", bool::default),
            bindings: Self::field(value, "bindings", Bindings::default),
            pad_type: Self::field(value, "pad_type", <[PadType; PORTS]>::default),
            socd: Self::field(value, "socd", SocdMode::default),
            touch_overlay: Self::field(value, "touch_overlay", TouchOverlayMode::default),
            haptics: Self::field(value, "haptics", default_haptics),
            hide_touch_with_controller: Self::field(
                value,
                "hide_touch_with_controller",
                default_hide_touch_with_controller,
            ),
            version: Self::field(value, "version", default_version),
        }
    }

    /// Brings a read value inside what the rest of the shell assumes: a volume
    /// the audio code can multiply by, and the version this build wrote - a file
    /// from a newer build is rewritten as this one's, field by field, rather than
    /// being kept as a version nothing can read.
    fn normalized(mut self) -> Self {
        self.volume = self.volume.clamp(0.0, 1.0);
        self.version = SETTINGS_VERSION;
        self
    }

    /// The settings saved in this browser profile, or the defaults.
    ///
    /// Native builds - the ones `cargo test -p web` runs - have no
    /// `localStorage`, so they get the same answer as a browser that has never
    /// saved anything.
    pub fn load_local() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let text = storage().and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten());
            match text {
                Some(text) => Self::from_json(&text),
                None => Self::default(),
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Self::default()
    }

    /// Saves to this browser profile, ignoring a write the browser refused.
    ///
    /// Private browsing and a full quota are both real, and neither is worth
    /// losing the session's settings over: they still work until the tab closes.
    pub fn save_local(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(storage) = storage() {
            let _ = storage.set_item(STORAGE_KEY, &self.to_json());
        }
    }
}

/// This profile's `localStorage`, if there is a window to hang it on.
#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The keycap text for a `KeyboardEvent.code`: `KeyA` as `A`, `ArrowUp` as `↑`.
///
/// `code` names a physical key and is unwieldy on a button, so this is the
/// display form only - the value stored and matched on is always the `code`
/// itself. A code with no shorter name says itself; a keycap reading
/// `NumpadEnter` is legible, a blank one is not.
pub fn keycap_label(code: &str) -> String {
    if let Some((_, label)) = KEYCAPS.iter().find(|(key, _)| *key == code) {
        return (*label).to_string();
    }
    // The two prefixes that carry the character itself: `KeyA` is the key printed
    // `A` and `Digit5` the one printed `5`, whatever layout they are on.
    if let Some(letter) = code.strip_prefix("Key") {
        return letter.to_uppercase();
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit.to_string();
    }
    if let Some(key) = code.strip_prefix("Numpad") {
        return format!("Numpad {key}");
    }
    // The modifiers are named by side, so a binding to left Shift says which one.
    for side in ["Left", "Right"] {
        if let Some(base) = code.strip_suffix(side) {
            let base = match base {
                "Control" => "Ctrl",
                other => other,
            };
            return format!("{side} {base}");
        }
    }
    code.to_string()
}

/// The keys whose `code` is worth shorter than itself, as `(code, keycap)`.
const KEYCAPS: [(&str, &str); 24] = [
    ("ArrowUp", "↑"),
    ("ArrowDown", "↓"),
    ("ArrowLeft", "←"),
    ("ArrowRight", "→"),
    ("Space", "Space"),
    ("Enter", "Enter"),
    ("Escape", "Esc"),
    ("Tab", "Tab"),
    ("Backspace", "Backspace"),
    ("Delete", "Delete"),
    ("CapsLock", "Caps"),
    ("Comma", ","),
    ("Period", "."),
    ("Slash", "/"),
    ("Backslash", "\\"),
    ("Semicolon", ";"),
    ("Quote", "'"),
    ("Minus", "-"),
    ("Equal", "="),
    ("Backquote", "`"),
    ("Home", "Home"),
    ("End", "End"),
    ("PageUp", "Page Up"),
    ("PageDown", "Page Down"),
];

/// The name of a standard gamepad button index, for the Controller column.
///
/// The names are the W3C standard mapping of a `"gamepad"`-typed device
/// ("Gamepad API", "button and axis mapping"), which is the layout the
/// bindings of [`Bindings::gamepad`] key on - so `0` is the bottom button of
/// the right-hand cluster, whatever a particular vendor prints on it. Anything
/// past the fourteen the standard names gets its index, which is at least a
/// number the player can find on the pad.
pub fn gamepad_button_label(index: u8) -> String {
    GAMEPAD_BUTTONS
        .get(index as usize)
        .map_or_else(|| format!("B{index}"), |name| (*name).to_string())
}

/// The standard mapping's button names, indexed by `GamepadButton` index.
const GAMEPAD_BUTTONS: [&str; 17] = [
    "A", "B", "X", "Y", "LB", "RB", "LT", "RT", "Select", "Start", "L3", "R3", "↑", "↓", "←", "→",
    "Home",
];

#[cfg(test)]
mod tests {
    use super::*;

    use crate::input::PadButton;

    /// The storage key is a contract with whatever a previous build wrote, so it
    /// is pinned by value rather than by prose.
    #[test]
    fn the_storage_key_is_the_one_the_ticket_names() {
        assert_eq!(STORAGE_KEY, "rustsega.settings.v1");
    }

    #[test]
    fn the_default_settings_are_the_ones_the_ticket_lists() {
        let settings = Settings::default();
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert_eq!(settings.volume, 0.8);
        assert!(!settings.muted);
        assert_eq!(settings.bindings, Bindings::default());
        assert_eq!(settings.pad_type, [PadType::SixButton, PadType::SixButton]);
        assert_eq!(settings.socd, SocdMode::Neutral);
        assert_eq!(settings.touch_overlay, TouchOverlayMode::Auto);
        assert!(settings.haptics);
        assert!(settings.hide_touch_with_controller);
        assert_eq!(settings.version, SETTINGS_VERSION);
    }

    #[test]
    fn garbage_settings_json_gives_the_defaults() {
        for garbage in [
            "",
            "   ",
            "{{{not json",
            "null",
            "[]",
            "\"a string\"",
            r#"{"volume":"#,
        ] {
            assert_eq!(
                Settings::from_json(garbage),
                Settings::default(),
                "from_json did not survive {garbage:?}"
            );
        }
    }

    #[test]
    fn a_missing_field_keeps_its_default() {
        let settings = Settings::from_json(r#"{"theme":"dark"}"#);
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.volume, DEFAULT_VOLUME);
        assert_eq!(settings.bindings, Bindings::default());
        assert_eq!(settings.socd, SocdMode::Neutral);
        assert_eq!(settings.version, SETTINGS_VERSION);
    }

    #[test]
    fn an_out_of_range_volume_is_clamped() {
        assert_eq!(Settings::from_json(r#"{"volume":99}"#).volume, 1.0);
        assert_eq!(Settings::from_json(r#"{"volume":-3}"#).volume, 0.0);
        assert_eq!(Settings::from_json(r#"{"volume":0.35}"#).volume, 0.35);
        // And a volume that is not a number at all is a missing volume.
        assert_eq!(
            Settings::from_json(r#"{"volume":"loud"}"#).volume,
            DEFAULT_VOLUME
        );
    }

    #[test]
    fn a_wrongly_typed_field_costs_only_that_field() {
        let settings =
            Settings::from_json(r#"{"volume":99,"bindings":null,"version":999,"theme":"light"}"#);
        assert_eq!(settings.theme, Theme::Light, "the good field survived");
        assert_eq!(settings.volume, 1.0);
        assert_eq!(settings.bindings, Bindings::default());
        assert_eq!(settings.version, SETTINGS_VERSION);
    }

    #[test]
    fn an_unknown_enum_value_falls_back_to_its_default() {
        let settings = Settings::from_json(
            r#"{"theme":"sepia","pad_type":["SixButton","Duck"],"socd":"teleport"}"#,
        );
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.pad_type, [PadType::SixButton, PadType::SixButton]);
        assert_eq!(settings.socd, SocdMode::Neutral);
    }

    #[test]
    fn settings_survive_a_json_round_trip() {
        let mut settings = Settings {
            theme: Theme::Light,
            volume: 0.35,
            muted: true,
            pad_type: [PadType::ThreeButton, PadType::SixButton],
            socd: SocdMode::LastWins,
            touch_overlay: TouchOverlayMode::Always,
            haptics: false,
            ..Settings::default()
        };
        settings.bindings.rebind_key(1, PadButton::Start, "Space");
        let json = settings.to_json();
        assert_eq!(Settings::from_json(&json), settings);
    }

    #[test]
    fn mute_holds_the_volume_rather_than_replacing_it() {
        let mut settings = Settings {
            volume: 0.4,
            ..Settings::default()
        };
        assert_eq!(settings.gain(), 0.4);
        settings.muted = true;
        assert_eq!(settings.gain(), 0.0);
        assert_eq!(settings.volume, 0.4, "the slider still says 0.4");
        settings.muted = false;
        assert_eq!(settings.gain(), 0.4);
    }

    #[test]
    fn auto_shows_the_overlay_for_a_finger_only() {
        let settings = Settings::default();
        assert!(settings.touch_visible(true, false));
        assert!(!settings.touch_visible(false, false));
    }

    #[test]
    fn always_shows_the_overlay_on_a_desktop_too() {
        let settings = Settings {
            touch_overlay: TouchOverlayMode::Always,
            ..Settings::default()
        };
        assert!(settings.touch_visible(false, false));
    }

    #[test]
    fn never_hides_the_overlay_even_on_a_phone() {
        let settings = Settings {
            touch_overlay: TouchOverlayMode::Never,
            ..Settings::default()
        };
        assert!(!settings.touch_visible(true, false));
    }

    #[test]
    fn a_connected_controller_hides_the_overlay_by_default() {
        let mut settings = Settings::default();
        assert!(!settings.touch_visible(true, true));
        // A player asking for the overlay is not the same as a phone reporting one.
        settings.touch_overlay = TouchOverlayMode::Always;
        assert!(!settings.touch_visible(false, true));
        // ... unless they have also said the overlay stays.
        settings.hide_touch_with_controller = false;
        assert!(settings.touch_visible(false, true));
        // And a controller changes nothing when there is no overlay to hide.
        settings.touch_overlay = TouchOverlayMode::Auto;
        assert!(!settings.touch_visible(false, true));
    }

    #[test]
    fn keycaps_are_shorter_than_the_codes_they_stand_for() {
        assert_eq!(keycap_label("KeyA"), "A");
        assert_eq!(keycap_label("KeyQ"), "Q");
        assert_eq!(keycap_label("Digit5"), "5");
        assert_eq!(keycap_label("ArrowUp"), "↑");
        assert_eq!(keycap_label("ArrowDown"), "↓");
        assert_eq!(keycap_label("ArrowLeft"), "←");
        assert_eq!(keycap_label("ArrowRight"), "→");
    }

    #[test]
    fn keycaps_name_the_keys_that_have_no_glyph() {
        assert_eq!(keycap_label("Space"), "Space");
        assert_eq!(keycap_label("Enter"), "Enter");
        assert_eq!(keycap_label("Escape"), "Esc");
        assert_eq!(keycap_label("Tab"), "Tab");
        assert_eq!(keycap_label("ShiftRight"), "Right Shift");
        assert_eq!(keycap_label("ShiftLeft"), "Left Shift");
        assert_eq!(keycap_label("ControlLeft"), "Left Ctrl");
        assert_eq!(keycap_label("AltRight"), "Right Alt");
        assert_eq!(keycap_label("Numpad7"), "Numpad 7");
        assert_eq!(keycap_label("Comma"), ",");
        assert_eq!(keycap_label("Slash"), "/");
    }

    #[test]
    fn an_unknown_code_is_its_own_keycap() {
        assert_eq!(keycap_label("Again"), "Again");
        assert_eq!(keycap_label(""), "");
    }

    #[test]
    fn gamepad_buttons_are_named_from_the_standard_mapping() {
        assert_eq!(gamepad_button_label(0), "A");
        assert_eq!(gamepad_button_label(1), "B");
        assert_eq!(gamepad_button_label(2), "X");
        assert_eq!(gamepad_button_label(3), "Y");
        assert_eq!(gamepad_button_label(4), "LB");
        assert_eq!(gamepad_button_label(5), "RB");
        assert_eq!(gamepad_button_label(6), "LT");
        assert_eq!(gamepad_button_label(7), "RT");
        assert_eq!(gamepad_button_label(8), "Select");
        assert_eq!(gamepad_button_label(9), "Start");
        assert_eq!(gamepad_button_label(12), "↑");
        assert_eq!(gamepad_button_label(16), "Home");
    }

    #[test]
    fn a_gamepad_button_past_the_standard_mapping_is_named_by_index() {
        assert_eq!(gamepad_button_label(17), "B17");
        assert_eq!(gamepad_button_label(255), "B255");
    }
}
