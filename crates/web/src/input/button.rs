//! [`PadButton`], the per-button key of the input model (M7-06, #139).
//!
//! The Genesis buttons themselves live in [`genesis::Buttons`], which is what
//! [`crate::emulator`] hands the console. A `Buttons` is a set with no way to
//! take one button back out of it, so the held-button table is keyed by this
//! enum instead and builds a `Buttons` on the way out with [`PadButton::buttons`].

use genesis::Buttons;
use serde::Deserialize;
use serde::Serialize;

/// One button of a Mega Drive pad, as a binding and a held-button table name it.
///
/// Named by what the pad calls it, matching [`genesis::Buttons`] and the label
/// on the physical pad (segaretro "Control Pad (Mega Drive)"; the four the
/// 6-button pad adds are documented in plutiedev "Controllers", "Button
/// layout"). An enum rather than a bit: a binding table and a press table need
/// a key, and a bit is a set.
///
/// The `serde` names are the ones stored in `localStorage` by the settings UI
/// (M7-10, #143), so they are snake_case and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadButton {
    /// D-pad up.
    Up,
    /// D-pad down.
    Down,
    /// D-pad left.
    Left,
    /// D-pad right.
    Right,
    /// The `A` button.
    A,
    /// The `B` button.
    B,
    /// The `C` button.
    C,
    /// The `Start` button.
    Start,
    /// The `X` button, 6-button pad only.
    X,
    /// The `Y` button, 6-button pad only.
    Y,
    /// The `Z` button, 6-button pad only.
    Z,
    /// The `Mode` button, 6-button pad only.
    Mode,
}

impl PadButton {
    /// Every button of a 6-button pad, in the order the enum declares them.
    ///
    /// The one order worth using for "all of them": a held-button snapshot lists
    /// its buttons in it, so two snapshots of the same pad can be compared by
    /// name (see [`super::gamepad::apply_snapshot`]).
    pub const ALL: [Self; 12] = [
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::A,
        Self::B,
        Self::C,
        Self::Start,
        Self::X,
        Self::Y,
        Self::Z,
        Self::Mode,
    ];

    /// The [`genesis::Buttons`] bit this button is.
    ///
    /// Written out per variant, and the test below checks all twelve: the bit
    /// order of `B`, `C` and `A` in `Buttons` is `B`/`C`/`A`, not alphabetical,
    /// so there is no arithmetic here that would be allowed to generate it.
    pub fn buttons(self) -> Buttons {
        match self {
            Self::Up => Buttons::UP,
            Self::Down => Buttons::DOWN,
            Self::Left => Buttons::LEFT,
            Self::Right => Buttons::RIGHT,
            Self::A => Buttons::A,
            Self::B => Buttons::B,
            Self::C => Buttons::C,
            Self::Start => Buttons::START,
            Self::X => Buttons::X,
            Self::Y => Buttons::Y,
            Self::Z => Buttons::Z,
            Self::Mode => Buttons::MODE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_pad_button_is_the_genesis_bit_named_after_it() {
        // The trap this pins: `Buttons::B` is 1 << 4, `C` is 1 << 5 and `A` is
        // 1 << 6, so a mapping written from an alphabetical list is wrong.
        assert_eq!(PadButton::Up.buttons(), Buttons::UP);
        assert_eq!(PadButton::Down.buttons(), Buttons::DOWN);
        assert_eq!(PadButton::Left.buttons(), Buttons::LEFT);
        assert_eq!(PadButton::Right.buttons(), Buttons::RIGHT);
        assert_eq!(PadButton::A.buttons(), Buttons::A);
        assert_eq!(PadButton::B.buttons(), Buttons::B);
        assert_eq!(PadButton::C.buttons(), Buttons::C);
        assert_eq!(PadButton::Start.buttons(), Buttons::START);
        assert_eq!(PadButton::X.buttons(), Buttons::X);
        assert_eq!(PadButton::Y.buttons(), Buttons::Y);
        assert_eq!(PadButton::Z.buttons(), Buttons::Z);
        assert_eq!(PadButton::Mode.buttons(), Buttons::MODE);
    }
}
