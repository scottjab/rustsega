//! What the touch overlay means, as data (M7-09, #142).
//!
//! The touch half of the input model, and none of the browser: [`dpad_from_point`]
//! decides what a finger on the d-pad is, [`control_at`] decides what a finger
//! that has slid along the page is holding, and [`TouchTracker`] keeps the
//! pointer-identity of every finger so a second one on another button does not
//! disturb the first. All of it is pure so `cargo test -p web` runs the whole of
//! the overlay's policy on the host; [`crate::touch_events`] is the half that owns
//! `PointerEvent`s, and it does nothing but read one into the plain numbers and
//! [`Control`]s below and say what they came to.
//!
//! The rules here are the ones a thumb needs rather than the ones a mouse gets.
//! A finger is wide, it cannot see what it is covering, and it leaves the element
//! it pressed long before the player means to stop pressing it - which is why a
//! d-pad answer is not limited to the pad's own box, and why a slide past the edge
//! of a button keeps the button.

use super::PadButton;

/// How far from the d-pad's centre a finger has to be before it is a direction,
/// as a fraction of the pad's radius.
///
/// A thumb is about 45px wide against a 148px pad, so a dead zone a quarter of the
/// radius both keeps the pad from firing at a graze and leaves three quarters of
/// it for directions. The same quarter the Gamepad API half uses
/// ([`super::gamepad::DEADZONE`]) for a stick at rest, for the same reason.
pub const DEAD_ZONE: f64 = 0.25;

/// The tangent of half a direction's slice of the d-pad: eight directions, so each
/// one is 45 degrees wide and anything within 22.5 degrees of it is that direction.
///
/// Comparing a ratio rather than an angle, and the same constant as
/// [`super::gamepad::SECTOR_TAN`], so that a stick and a thumb at 45 degrees mean
/// the same thing.
const SECTOR_TAN: f64 = 0.414_213_56;

/// One of the eight positions of the d-pad, plus "the thumb is where the middle
/// is" - which a touch pad has and a real d-pad does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpadDir {
    /// No direction: the finger is inside the dead zone.
    Neutral,
    /// Straight up.
    Up,
    /// Up and to the right.
    UpRight,
    /// Straight right.
    Right,
    /// Down and to the right.
    DownRight,
    /// Straight down.
    Down,
    /// Down and to the left.
    DownLeft,
    /// Straight left.
    Left,
    /// Up and to the left.
    UpLeft,
}

impl DpadDir {
    /// The Genesis buttons this d-pad position is pressing: none for
    /// [`DpadDir::Neutral`], one for a side, and both for a corner.
    pub fn pad_buttons(self) -> Vec<PadButton> {
        let (up, down, left, right) = match self {
            Self::Neutral => (false, false, false, false),
            Self::Up => (true, false, false, false),
            Self::UpRight => (true, false, false, true),
            Self::Right => (false, false, false, true),
            Self::DownRight => (false, true, false, true),
            Self::Down => (false, true, false, false),
            Self::DownLeft => (false, true, true, false),
            Self::Left => (false, false, true, false),
            Self::UpLeft => (true, false, true, false),
        };
        [
            (up, PadButton::Up),
            (down, PadButton::Down),
            (left, PadButton::Left),
            (right, PadButton::Right),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .map(|(_, button)| button)
        .collect()
    }
}

/// The d-pad direction of a finger at `dx`, `dy` from the pad's centre, which is
/// `radius` wide in the same CSS pixels.
///
/// Screen coordinates, so a positive `dy` is down the page. `dx` and `dy` are
/// deliberately not limited to the pad: a finger that has slid 40px past the edge
/// of the pad is still telling the player which way it is pushing, and dropping
/// the direction there would stop a running game every time a thumb rolled off the
/// plastic. Only the dead zone is scaled by the radius, so the answer is a
/// direction for any distance outside it.
pub fn dpad_from_point(dx: f64, dy: f64, radius: f64) -> DpadDir {
    // A pad the browser cannot measure, or a point it cannot name, is not a
    // direction - the same answer as a stick the Gamepad API reports as `NaN`.
    if !dx.is_finite() || !dy.is_finite() {
        return DpadDir::Neutral;
    }
    // A distance rather than an axis, so a thumb cannot escape the dead zone by
    // landing on a corner.
    if dx.hypot(dy) <= DEAD_ZONE * radius.max(0.0) {
        return DpadDir::Neutral;
    }
    let (ax, ay) = (dx.abs(), dy.abs());
    // `ay <= ax * SECTOR_TAN` is "within 22.5 degrees of horizontal" and the other
    // way round the same for vertical. Neither is true for a corner, and a corner
    // is the third case: eight directions, so a side and a diagonal cannot both
    // apply and one of the two tests has to win.
    if ay <= ax * SECTOR_TAN {
        return if dx > 0.0 {
            DpadDir::Right
        } else {
            DpadDir::Left
        };
    }
    if ax <= ay * SECTOR_TAN {
        return if dy > 0.0 { DpadDir::Down } else { DpadDir::Up };
    }
    match (dx > 0.0, dy > 0.0) {
        (true, true) => DpadDir::DownRight,
        (true, false) => DpadDir::UpRight,
        (false, true) => DpadDir::DownLeft,
        (false, false) => DpadDir::UpLeft,
    }
}

/// One control of the overlay: what a finger can be lying on.
///
/// The nine are the pad minus `Select`-less extras - the 6-button pad's whole set
/// plus the d-pad as one place to put a thumb. The menu button is not here: it is
/// a shell control, not a pad one, and it opens the drawer rather than pressing
/// anything on port 0 (see [`crate::touch_events`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// The whole d-pad disc: which buttons it presses is [`DpadDir`]'s business.
    Dpad,
    /// The `A` button.
    A,
    /// The `B` button.
    B,
    /// The `C` button.
    C,
    /// The `X` button.
    X,
    /// The `Y` button.
    Y,
    /// The `Z` button.
    Z,
    /// The `Start` button.
    Start,
    /// The `Mode` button.
    Mode,
}

impl Control {
    /// Every control, in the order the overlay lists them.
    pub const ALL: [Self; 9] = [
        Self::Dpad,
        Self::A,
        Self::B,
        Self::C,
        Self::X,
        Self::Y,
        Self::Z,
        Self::Start,
        Self::Mode,
    ];

    /// The `data-touch` key this control wears in the markup, and the key
    /// [`Control::from_key`] reads back.
    pub fn key(self) -> &'static str {
        match self {
            Self::Dpad => "dpad",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::Start => "start",
            Self::Mode => "mode",
        }
    }

    /// The control a `data-touch` key names, `None` for anything else on the page
    /// that is not a control at all.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|control| control.key() == key)
    }

    /// The Genesis button this control is, `None` for the d-pad - which presses a
    /// direction rather than a button, and gets there through [`DpadDir`].
    pub fn pad_button(self) -> Option<PadButton> {
        match self {
            Self::Dpad => None,
            Self::A => Some(PadButton::A),
            Self::B => Some(PadButton::B),
            Self::C => Some(PadButton::C),
            Self::X => Some(PadButton::X),
            Self::Y => Some(PadButton::Y),
            Self::Z => Some(PadButton::Z),
            Self::Start => Some(PadButton::Start),
            Self::Mode => Some(PadButton::Mode),
        }
    }
}

/// One control's box on the page, in the CSS pixels of
/// `getBoundingClientRect` - viewport-relative, so the same space a
/// `PointerEvent`'s `clientX`/`clientY` are in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlBox {
    /// The control this box is.
    pub control: Control,
    /// The left edge.
    pub x: f64,
    /// The top edge.
    pub y: f64,
    /// Its width.
    pub width: f64,
    /// Its height.
    pub height: f64,
}

impl ControlBox {
    /// Whether `x`, `y` is inside this box.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// The d-pad direction of `x`, `y` from this box's centre, at this box's
    /// radius. Meaningful for the d-pad; for a face button it answers what a
    /// finger somewhere around that button's middle would mean, which is nobody's
    /// business but the d-pad's.
    pub fn direction_at(&self, x: f64, y: f64) -> DpadDir {
        dpad_from_point(
            x - (self.x + self.width / 2.0),
            y - (self.y + self.height / 2.0),
            self.width / 2.0,
        )
    }
}

/// The control under `x`, `y`: the first box that contains the point, in the order
/// the overlay measured them.
///
/// `None` is not "nothing is held". A pointer that was already on a control keeps
/// it when it moves to where no control is - see [`TouchTracker::move_to`] - so
/// this only answers "what did the finger land on", for a press and for a slide
/// from one control onto another.
pub fn control_at(boxes: &[ControlBox], x: f64, y: f64) -> Option<Control> {
    boxes
        .iter()
        .find(|control_box| control_box.contains(x, y))
        .map(|control_box| control_box.control)
}

/// Which finger is on which control.
///
/// Pointer identity is the whole point: the overlay has to know that the finger
/// that went down on `B` is the one that came up, because a second finger on `C`
/// in between must not release `B`. It is a `Vec` of a handful of live pointers
/// rather than a map because a finger has to be found by identity on every event
/// and eight entries is a scan, not a hash.
#[derive(Debug, Clone, Default)]
pub struct TouchTracker {
    /// Every finger on the overlay right now.
    touches: Vec<Touch>,
}

/// One finger: the pointer that identifies it, the control it is on, and - for the
/// d-pad, and only there - the direction it is holding.
#[derive(Debug, Clone, Copy)]
struct Touch {
    pointer: i32,
    control: Control,
    direction: DpadDir,
}

impl Touch {
    /// Whether this finger is holding `button`: a face button holds its own, and a
    /// finger on a d-pad corner holds both buttons of that corner.
    fn holds(&self, button: PadButton) -> bool {
        match self.control {
            Control::Dpad => self.direction.pad_buttons().contains(&button),
            other => other.pad_button() == Some(button),
        }
    }
}

impl TouchTracker {
    /// No fingers on the overlay.
    pub fn new() -> Self {
        Self::default()
    }

    /// A finger has gone down on `control`.
    ///
    /// `direction` is what [`dpad_from_point`] says about where on the d-pad it
    /// landed, and is ignored for every other control.
    ///
    /// Answers whether this control was not already held by another finger: a
    /// player chording `B` and `C` wants one buzz, and a second finger on a button
    /// that is already down is not news. A pointer that already had a finger on
    /// the overlay is replaced rather than doubled up, because a `pointerdown`
    /// with no `pointerup` before it means the browser lost one - and a control no
    /// finger is on would be held for the rest of the session.
    pub fn press(&mut self, pointer: i32, control: Control, direction: DpadDir) -> bool {
        self.touches.retain(|touch| touch.pointer != pointer);
        let news = !self.touches.iter().any(|touch| touch.control == control);
        self.touches.push(Touch {
            pointer,
            control,
            direction,
        });
        news
    }

    /// A finger has moved: `control` is what is under it now, `direction` where on
    /// the d-pad that is.
    ///
    /// A slide from `A` onto `B` is one move of one finger: `A` is released and
    /// `B` pressed, which is what the caller reads out of the difference between
    /// [`TouchTracker::snapshot`] before and after. A move to `None` keeps whatever
    /// the finger was on, because a thumb that has rolled off the edge of a button
    /// has not stopped meaning to press it.
    ///
    /// Answers whether anything changed, which for the d-pad it does when only the
    /// direction did.
    pub fn move_to(&mut self, pointer: i32, control: Option<Control>, direction: DpadDir) -> bool {
        let Some(touch) = self
            .touches
            .iter_mut()
            .find(|touch| touch.pointer == pointer)
        else {
            return false;
        };
        let control = control.unwrap_or(touch.control);
        let changed = touch.control != control || touch.direction != direction;
        touch.control = control;
        // Only the d-pad has a direction to remember; a face button held at the
        // edge is still just that button.
        touch.direction = if control == Control::Dpad {
            direction
        } else {
            DpadDir::Neutral
        };
        changed
    }

    /// A finger has come up, and returns the control it was on.
    ///
    /// `None` for a pointer this overlay never saw go down, which is every pointer
    /// that did not land on a control - so the caller does the same nothing for it
    /// as it did for the `pointerdown`.
    pub fn release(&mut self, pointer: i32) -> Option<Control> {
        let control = self.control_of(pointer)?;
        self.touches.retain(|touch| touch.pointer != pointer);
        Some(control)
    }

    /// A finger has been taken over by the browser - a scroll gesture it decided to
    /// own, a system menu, a hand leaving the screen mid-press.
    ///
    /// A release, and only a release: the alternative is a button held by a finger
    /// that no longer exists, which is the one thing an on-screen pad cannot allow.
    pub fn cancel(&mut self, pointer: i32) -> Option<Control> {
        self.release(pointer)
    }

    /// Every finger off the overlay at once, for a window that has lost focus: no
    /// `pointerup` arrives for a finger that went up over another tab.
    pub fn clear(&mut self) {
        self.touches.clear();
    }

    /// The controls with a finger on them right now, deduplicated: what the
    /// overlay marks pressed.
    pub fn held(&self) -> Vec<Control> {
        let mut held = Vec::new();
        for touch in &self.touches {
            if !held.contains(&touch.control) {
                held.push(touch.control);
            }
        }
        held
    }

    /// The Genesis buttons the overlay is pressing right now, in
    /// [`PadButton::ALL`] order, so two snapshots of the overlay can be compared
    /// by name.
    pub fn snapshot(&self) -> Vec<PadButton> {
        PadButton::ALL
            .into_iter()
            .filter(|button| self.touches.iter().any(|touch| touch.holds(*button)))
            .collect()
    }

    /// The control a pointer is on, if it is on one.
    ///
    /// How the browser half knows whether a `pointermove` belongs to the overlay at
    /// all, and what a slid finger was holding before it slid.
    pub fn control_of(&self, pointer: i32) -> Option<Control> {
        self.touches
            .iter()
            .find(|touch| touch.pointer == pointer)
            .map(|touch| touch.control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputState;
    use crate::input::PORTS;
    use crate::input::Source;
    use crate::input::gamepad;
    use genesis::Buttons;

    /// The radius every d-pad test uses: a 148px disc, as the overlay draws it.
    const RADIUS: f64 = 74.0;

    /// The unit vector at `degrees`, clockwise from straight right, at `RADIUS`:
    /// so a test can say "45 degrees down-right" without doing trigonometry in its
    /// assertions.
    fn at(degrees: f64) -> (f64, f64) {
        let radians = degrees * std::f64::consts::PI / 180.0;
        (radians.cos() * RADIUS, radians.sin() * RADIUS)
    }

    /// `tracker` as the page's only touch source: move `input`'s port 0 to what it
    /// is holding, the way [`crate::touch_events`] does, and return what port 0
    /// says now.
    fn drive(tracker: &TouchTracker, input: &mut InputState, previous: &mut Vec<PadButton>) {
        let next = tracker.snapshot();
        gamepad::apply_snapshot(input, Source::Touch, 0, previous, &next);
        *previous = next;
    }

    #[test]
    fn the_middle_of_the_dpad_is_neutral() {
        assert_eq!(dpad_from_point(0.0, 0.0, RADIUS), DpadDir::Neutral);
        // The dead zone is a quarter of the radius, so anything inside 18.5px of
        // the middle is still the middle - on a diagonal too, because it is a
        // distance and not two axes.
        assert_eq!(dpad_from_point(10.0, 0.0, RADIUS), DpadDir::Neutral);
        assert_eq!(dpad_from_point(10.0, 10.0, RADIUS), DpadDir::Neutral);
        // The edge of the dead zone is still inside it, and the next pixel is not.
        assert_eq!(
            dpad_from_point(0.25 * RADIUS, 0.0, RADIUS),
            DpadDir::Neutral
        );
        assert_eq!(dpad_from_point(0.26 * RADIUS, 0.0, RADIUS), DpadDir::Right);
    }

    #[test]
    fn straight_right_on_the_dpad_is_right() {
        assert_eq!(dpad_from_point(40.0, 0.0, RADIUS), DpadDir::Right);
        // And straight every other way, at 0/90/180/270 degrees.
        assert_eq!(at(0.0), (RADIUS, 0.0));
        assert_eq!(
            dpad_from_point(at(0.0).0, at(0.0).1, RADIUS),
            DpadDir::Right
        );
        assert_eq!(
            dpad_from_point(at(90.0).0, at(90.0).1, RADIUS),
            DpadDir::Down
        );
        assert_eq!(
            dpad_from_point(at(180.0).0, at(180.0).1, RADIUS),
            DpadDir::Left
        );
        assert_eq!(
            dpad_from_point(at(270.0).0, at(270.0).1, RADIUS),
            DpadDir::Up
        );
    }

    #[test]
    fn forty_five_degrees_down_right_on_the_dpad_is_down_right() {
        let (dx, dy) = at(45.0);
        assert!(dy > 0.0, "screen y grows downwards, so 45 degrees is down");
        assert_eq!(dpad_from_point(dx, dy, RADIUS), DpadDir::DownRight);
        assert_eq!(
            dpad_from_point(at(135.0).0, at(135.0).1, RADIUS),
            DpadDir::DownLeft
        );
        assert_eq!(
            dpad_from_point(at(225.0).0, at(225.0).1, RADIUS),
            DpadDir::UpLeft
        );
        assert_eq!(
            dpad_from_point(at(315.0).0, at(315.0).1, RADIUS),
            DpadDir::UpRight
        );
    }

    #[test]
    fn a_point_outside_the_dpad_radius_is_still_a_direction() {
        // Sliding off the pad mid-game must not drop the direction: twice the
        // radius to the right is still Right, and a long way down-right is still
        // that corner.
        assert_eq!(dpad_from_point(RADIUS * 2.0, 0.0, RADIUS), DpadDir::Right);
        let (dx, dy) = at(45.0);
        assert_eq!(
            dpad_from_point(dx * 4.0, dy * 4.0, RADIUS),
            DpadDir::DownRight
        );
    }

    #[test]
    fn the_dpad_sectors_are_forty_five_degrees_wide() {
        // 22.4 degrees is still Right's; 22.6 is the corner's.
        assert_eq!(
            dpad_from_point(at(22.4).0, at(22.4).1, RADIUS),
            DpadDir::Right
        );
        assert_eq!(
            dpad_from_point(at(22.6).0, at(22.6).1, RADIUS),
            DpadDir::DownRight
        );
        // Symmetric the other side of Right, and around the pad.
        assert_eq!(
            dpad_from_point(at(337.6).0, at(337.6).1, RADIUS),
            DpadDir::Right
        );
        assert_eq!(
            dpad_from_point(at(337.4).0, at(337.4).1, RADIUS),
            DpadDir::UpRight
        );
    }

    #[test]
    fn a_dpad_point_the_browser_cannot_read_is_neutral() {
        assert_eq!(dpad_from_point(f64::NAN, 0.0, RADIUS), DpadDir::Neutral);
        assert_eq!(
            dpad_from_point(0.0, f64::INFINITY, RADIUS),
            DpadDir::Neutral
        );
        // A pad measured before the browser laid it out has no dead zone to hide
        // behind, but it still answers a direction rather than nothing.
        assert_eq!(dpad_from_point(10.0, 0.0, 0.0), DpadDir::Right);
    }

    #[test]
    fn a_dpad_corner_is_two_buttons_and_the_middle_is_none() {
        assert_eq!(DpadDir::Neutral.pad_buttons(), Vec::<PadButton>::new());
        assert_eq!(DpadDir::Right.pad_buttons(), vec![PadButton::Right]);
        assert_eq!(
            DpadDir::DownRight.pad_buttons(),
            vec![PadButton::Down, PadButton::Right]
        );
        assert_eq!(
            DpadDir::UpLeft.pad_buttons(),
            vec![PadButton::Up, PadButton::Left]
        );
    }

    #[test]
    fn the_control_keys_are_unique_and_round_trip() {
        // `data-touch` is the only thing tying a measured box to a control, so two
        // controls called the same thing would be one control that is both.
        for control in Control::ALL {
            assert_eq!(Control::from_key(control.key()), Some(control));
        }
        let keys: Vec<&str> = Control::ALL.iter().map(|control| control.key()).collect();
        let unique: std::collections::HashSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), Control::ALL.len());
        // And the nine controls are the twelve pad buttons: every button of a
        // Genesis pad is reachable by thumb.
        let buttons: Vec<PadButton> = Control::ALL
            .iter()
            .copied()
            .filter_map(Control::pad_button)
            .collect();
        assert_eq!(buttons.len(), 8);
        for button in PadButton::ALL {
            let reachable = matches!(
                button,
                PadButton::Up | PadButton::Down | PadButton::Left | PadButton::Right
            ) || buttons.contains(&button);
            assert!(reachable, "{button:?} has no control on the overlay");
        }
    }

    #[test]
    fn the_control_under_a_point_is_the_one_whose_box_contains_it() {
        let boxes = vec![
            ControlBox {
                control: Control::Dpad,
                x: 16.0,
                y: 700.0,
                width: 148.0,
                height: 148.0,
            },
            ControlBox {
                control: Control::B,
                x: 800.0,
                y: 700.0,
                width: 64.0,
                height: 64.0,
            },
        ];
        assert_eq!(control_at(&boxes, 100.0, 760.0), Some(Control::Dpad));
        assert_eq!(control_at(&boxes, 830.0, 730.0), Some(Control::B));
        assert_eq!(control_at(&boxes, 500.0, 400.0), None);
        // The bottom and right edges belong to the next pixel, so two boxes that
        // touch share no point.
        assert!(!boxes[0].contains(164.0, 760.0));
        assert!(boxes[0].contains(16.0, 700.0));
    }

    #[test]
    fn the_dpad_direction_of_a_point_comes_from_its_centre() {
        let dpad = ControlBox {
            control: Control::Dpad,
            x: 100.0,
            y: 700.0,
            width: 148.0,
            height: 148.0,
        };
        // Middle: neutral. Bottom-right of the disc: the corner.
        assert_eq!(dpad.direction_at(174.0, 774.0), DpadDir::Neutral);
        assert_eq!(dpad.direction_at(240.0, 840.0), DpadDir::DownRight);
        assert_eq!(dpad.direction_at(110.0, 690.0), DpadDir::UpLeft);
    }

    #[test]
    fn two_fingers_on_two_buttons_both_hold() {
        let mut tracker = TouchTracker::new();
        assert!(tracker.press(1, Control::B, DpadDir::Neutral));
        assert!(tracker.press(2, Control::C, DpadDir::Neutral));
        assert_eq!(
            tracker.snapshot(),
            vec![PadButton::B, PadButton::C],
            "both buttons are down at once"
        );
        assert_eq!(tracker.held(), vec![Control::B, Control::C]);

        // And the page sees them as one set of buttons on port 0.
        let mut input = InputState::new(PORTS);
        let mut previous = Vec::new();
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::B | Buttons::C);
    }

    #[test]
    fn releasing_one_finger_keeps_the_other() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::B, DpadDir::Neutral);
        tracker.press(2, Control::C, DpadDir::Neutral);
        let mut input = InputState::new(PORTS);
        let mut previous = Vec::new();
        drive(&tracker, &mut input, &mut previous);

        assert_eq!(tracker.release(1), Some(Control::B));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::C, "C is still held by 2");
        assert_eq!(tracker.held(), vec![Control::C]);

        assert_eq!(tracker.release(2), Some(Control::C));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::empty());
    }

    #[test]
    fn sliding_from_a_to_b_releases_a_and_presses_b() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::A, DpadDir::Neutral);
        let mut input = InputState::new(PORTS);
        let mut previous = Vec::new();
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::A);

        // One finger, one move, onto the next control over.
        let changed = tracker.move_to(1, Some(Control::B), DpadDir::Neutral);
        assert!(changed);
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::B, "A went out with the slide");
        assert_eq!(tracker.held(), vec![Control::B]);

        // And letting go there releases B, not A.
        assert_eq!(tracker.release(1), Some(Control::B));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::empty());
    }

    #[test]
    fn sliding_past_the_edge_of_a_control_keeps_it() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::C, DpadDir::Neutral);
        let changed = tracker.move_to(1, None, DpadDir::Neutral);
        assert!(!changed, "nothing changed, so nothing to report");
        assert_eq!(tracker.snapshot(), vec![PadButton::C]);
    }

    #[test]
    fn sliding_around_the_dpad_changes_the_direction_not_the_finger() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::Dpad, DpadDir::Right);
        let mut input = InputState::new(PORTS);
        let mut previous = Vec::new();
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::RIGHT);

        assert!(tracker.move_to(1, Some(Control::Dpad), DpadDir::DownRight));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::DOWN | Buttons::RIGHT);

        assert!(tracker.move_to(1, Some(Control::Dpad), DpadDir::Up));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::UP);

        // A thumb back on the middle of the pad is pressing nothing, while still
        // being the same finger on the same control.
        assert!(tracker.move_to(1, Some(Control::Dpad), DpadDir::Neutral));
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::empty());
        assert_eq!(tracker.held(), vec![Control::Dpad]);
    }

    #[test]
    fn a_finger_on_the_dpad_and_one_on_a_button_do_not_interfere() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::Dpad, DpadDir::Up);
        tracker.press(2, Control::A, DpadDir::Neutral);
        assert_eq!(tracker.snapshot(), vec![PadButton::Up, PadButton::A]);
        assert!(tracker.move_to(1, Some(Control::Dpad), DpadDir::DownRight));
        assert_eq!(
            tracker.snapshot(),
            vec![PadButton::Down, PadButton::Right, PadButton::A]
        );
        assert_eq!(tracker.release(2), Some(Control::A));
        assert_eq!(
            tracker.snapshot(),
            vec![PadButton::Down, PadButton::Right],
            "the d-pad finger is untouched"
        );
    }

    #[test]
    fn a_cancelled_finger_is_a_released_finger() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::B, DpadDir::Neutral);
        tracker.press(2, Control::C, DpadDir::Neutral);
        assert_eq!(tracker.cancel(1), Some(Control::B));
        assert_eq!(tracker.snapshot(), vec![PadButton::C]);
        // A pointer the overlay never saw - a finger that went down off the
        // controls - cancels nothing.
        assert_eq!(tracker.cancel(7), None);
        assert_eq!(tracker.snapshot(), vec![PadButton::C]);
    }

    #[test]
    fn clearing_the_tracker_releases_every_finger() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::Start, DpadDir::Neutral);
        tracker.press(2, Control::Dpad, DpadDir::Left);
        tracker.clear();
        assert_eq!(tracker.snapshot(), Vec::<PadButton>::new());
        assert_eq!(tracker.held(), Vec::<Control>::new());
    }

    #[test]
    fn a_second_finger_on_a_control_already_held_is_not_a_new_press() {
        let mut tracker = TouchTracker::new();
        assert!(tracker.press(1, Control::B, DpadDir::Neutral));
        // Same control, other finger: held, but no second buzz.
        assert!(!tracker.press(2, Control::B, DpadDir::Neutral));
        assert_eq!(tracker.held(), vec![Control::B]);
        // The first finger letting go leaves the second one holding it, which is
        // what the per-pointer table buys.
        assert_eq!(tracker.release(1), Some(Control::B));
        assert_eq!(tracker.held(), vec![Control::B]);
        assert_eq!(tracker.snapshot(), vec![PadButton::B]);
        assert_eq!(tracker.release(2), Some(Control::B));
        assert_eq!(tracker.snapshot(), Vec::<PadButton>::new());
    }

    #[test]
    fn a_lost_pointerup_does_not_hold_a_button_forever() {
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::B, DpadDir::Neutral);
        // The browser lost the `pointerup`, and has now reused the pointer id.
        tracker.press(1, Control::C, DpadDir::Neutral);
        assert_eq!(tracker.held(), vec![Control::C]);
        assert_eq!(tracker.snapshot(), vec![PadButton::C]);
    }

    #[test]
    fn touch_releases_only_ever_touch_own_hold() {
        // The keyboard is holding `B` and the overlay takes a finger off a `B` of
        // its own: the key is not the overlay's to let go of.
        let mut input = InputState::new(PORTS);
        input.set(Source::Keyboard, 0, PadButton::B, true);
        let mut tracker = TouchTracker::new();
        tracker.press(1, Control::B, DpadDir::Neutral);
        let mut previous = Vec::new();
        drive(&tracker, &mut input, &mut previous);
        tracker.release(1);
        drive(&tracker, &mut input, &mut previous);
        assert_eq!(input.buttons(0), Buttons::B, "still held by the keyboard");
    }
}
