//! Spatial navigation and menu actions, as data (M7-11, #144).
//!
//! The policy half of "everything in the UI can be used with a gamepad or a
//! keyboard": [`next_focus`] decides which control a d-pad press lands on,
//! [`RepeatTimer`] how fast a held direction walks, [`ui_action`] says what a key
//! or a pad button means to a menu, and [`MenuPause`] remembers whether the menu
//! or the player stopped the console. All of it is pure so `cargo test -p web`
//! runs the whole of the navigation policy on the host; [`crate::menu`] is the
//! half that owns the DOM and moves the real focus.

use crate::input::gamepad::DEADZONE;
use crate::input::gamepad::HOME;
use crate::input::gamepad::SELECT;
use crate::input::gamepad::START;

/// How much a candidate's distance off the line of travel counts against it.
///
/// A menu is walked in rows and columns, so a control dead ahead but a little
/// further away is a better next focus than one that is nearer overall but half a
/// button above the line: at 3x the off-axis distance, a candidate 8px off the
/// line loses to one 24px further along it. Below about this the walk drifts out
/// of a column as soon as two controls are of different widths; much above it and
/// a row of controls stops being reachable at all.
const OFF_AXIS: f64 = 3.0;

/// A box on the page, in CSS pixels, exactly as `getBoundingClientRect()` reports
/// it: `x` and `y` are the top left, and `y` grows downwards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// A box from its top left and its size, as the browser gives them.
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// The middle of the box, which is what a direction points at: two controls
    /// of different sizes are neighbours when their middles line up.
    pub fn centre(self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// One of the four directions a d-pad, a left stick or the arrow keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Towards the top of the page.
    Up,
    /// Towards the bottom of the page.
    Down,
    /// Towards the left of the page.
    Left,
    /// Towards the right of the page.
    Right,
}

impl Dir {
    /// The unit vector of this direction, in the page's coordinates (`y` down).
    fn unit(self) -> (f64, f64) {
        match self {
            Self::Up => (0.0, -1.0),
            Self::Down => (0.0, 1.0),
            Self::Left => (-1.0, 0.0),
            Self::Right => (1.0, 0.0),
        }
    }

    /// Whether a candidate seen from the current control at `(dx, dy)` is in this
    /// direction's half-plane.
    ///
    /// Strictly in it: a control directly above is neither left nor right, so it
    /// is not a candidate for a leftward press. That is what stops a walk from
    /// drifting sideways out of a column.
    fn faces(self, dx: f64, dy: f64) -> bool {
        let (ux, uy) = self.unit();
        dx * ux + dy * uy > 0.0
    }

    /// How far a candidate at `(dx, dy)` is from where the walk is going: the
    /// distance along this direction, plus [`OFF_AXIS`] times the distance off it.
    fn cost(self, dx: f64, dy: f64) -> f64 {
        let (ux, uy) = self.unit();
        let along = (dx * ux + dy * uy).abs();
        let off = (dx * uy - dy * ux).abs();
        along + OFF_AXIS * off
    }
}

/// The candidate to focus after `dir`, out of `candidates`, or `None` when
/// nothing is that way and the focus stays where it is.
///
/// Candidates are considered by the centre of their box - see [`Rect::centre`] -
/// and only those in `dir`'s half-plane count. The winner is the cheapest by
/// [`Dir::cost`], which is what makes a row of unevenly sized controls pick the
/// visually nearest one rather than the nearest left edge or the first in the
/// list. A tie goes to the earlier candidate, so the answer depends on the
/// geometry alone and not on the order two elements happen to be compared in.
pub fn next_focus(current: Rect, candidates: &[Rect], dir: Dir) -> Option<usize> {
    let (cx, cy) = current.centre();
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, rect)| {
            let (x, y) = rect.centre();
            let (dx, dy) = (x - cx, y - cy);
            dir.faces(dx, dy).then_some((index, dir.cost(dx, dy)))
        })
        .min_by(|(_, cost), (_, other)| cost.total_cmp(other))
        .map(|(index, _)| index)
}

/// The repeat rule of a held direction: one move at once, the next after
/// [`RepeatTimer::FIRST`], then one every [`RepeatTimer::EVERY`].
///
/// A keyboard needs none of this - the browser repeats the keydown - but a pad is
/// polled, and a held d-pad would otherwise walk the focus out of the drawer in
/// sixty moves a second. The two delays are the ones the ticket asks for and are
/// roughly what a person holding a direction expects: long enough to cross a
/// screen, short enough not to feel stuck.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RepeatTimer {
    /// When the next repeat is due, `None` while nothing is held.
    due: Option<f64>,
}

impl RepeatTimer {
    /// How long a held direction waits before it moves a second time, in ms.
    pub const FIRST: f64 = 400.0;
    /// How long it waits after that, in ms.
    pub const EVERY: f64 = 80.0;

    /// A direction has just been held at `now` (ms, from whatever clock the
    /// caller has - the animation frame's is fine).
    pub fn start(&mut self, now: f64) {
        self.due = Some(now + Self::FIRST);
    }

    /// The direction has been let go.
    pub fn stop(&mut self) {
        self.due = None;
    }

    /// Whether the held direction is owed another move at `now`, arming the next
    /// one [`EVERY`](Self::EVERY) later. A timer that was never started asks for
    /// nothing, however long it has been sitting there.
    pub fn poll(&mut self, now: f64) -> bool {
        let Some(due) = self.due else {
            return false;
        };
        if now < due {
            return false;
        }
        self.due = Some(now + Self::EVERY);
        true
    }
}

/// Which half of the page the shell is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiMode {
    /// The game has the controls: keys and pad buttons drive the pad.
    #[default]
    Playing,
    /// A menu is open: they move the focus instead, and the console is held.
    Menu,
}

/// What a menu is asked to do, by either device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiAction {
    /// Move the focus to the neighbouring control this way.
    Move(Dir),
    /// Activate the focused control.
    Confirm,
    /// Close the innermost open thing.
    Back,
    /// Open the menu, or leave it when one is already open.
    ToggleMenu,
}

/// One event that can move a menu, with nothing browser-shaped left in it: the
/// argument of [`ui_action`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiEvent<'a> {
    /// A `KeyboardEvent.code` - the physical key, as everywhere in
    /// [`crate::input::keyboard`].
    Key(&'a str),
    /// One Gamepad API button, by its standard index.
    PadButton(u8),
    /// The left stick, axes 0 and 1 of the standard mapping, `y` positive down.
    Stick { x: f64, y: f64 },
}

/// The bottom (B) of a Genesis pad in the standard gamepad mapping.
const PAD_B: u8 = 0;
/// The right (C) of a Genesis pad in the standard gamepad mapping.
const PAD_C: u8 = 1;
/// The four d-pad buttons of the standard gamepad mapping. They are not in
/// [`crate::input::gamepad`] because the Genesis buttons are rebindable and these
/// are not: a menu is walked by direction whatever the pad is bound to.
const PAD_UP: u8 = 12;
/// See [`PAD_UP`].
const PAD_DOWN: u8 = 13;
/// See [`PAD_UP`].
const PAD_LEFT: u8 = 14;
/// See [`PAD_UP`].
const PAD_RIGHT: u8 = 15;

/// What `event` means to a menu, or `None` when it means nothing to one and the
/// browser may have the key.
///
/// `Escape` answers [`UiAction::ToggleMenu`] because that is what the fixed
/// hotkey table already says it is (see [`crate::input::keyboard::map_key`]);
/// inside an open menu the shell treats the two the same way up, since closing
/// the innermost thing is how you leave a menu - see [`crate::menu::Menu::run`]
/// for where that happens and [`UiAction::Back`] for the button that only ever
/// goes back.
pub fn ui_action(event: &UiEvent<'_>) -> Option<UiAction> {
    match event {
        UiEvent::Key(code) => match *code {
            "ArrowUp" => Some(UiAction::Move(Dir::Up)),
            "ArrowDown" => Some(UiAction::Move(Dir::Down)),
            "ArrowLeft" => Some(UiAction::Move(Dir::Left)),
            "ArrowRight" => Some(UiAction::Move(Dir::Right)),
            // `Space` is the code of the space bar, and it activates a focused
            // button in the browser - which is why the shell cancels the browser
            // rather than letting a button be pressed twice.
            "Enter" | "Space" => Some(UiAction::Confirm),
            "Escape" => Some(UiAction::ToggleMenu),
            "Backspace" => Some(UiAction::Back),
            _ => None,
        },
        UiEvent::PadButton(index) => match *index {
            PAD_B => Some(UiAction::Confirm),
            PAD_C => Some(UiAction::Back),
            PAD_UP => Some(UiAction::Move(Dir::Up)),
            PAD_DOWN => Some(UiAction::Move(Dir::Down)),
            PAD_LEFT => Some(UiAction::Move(Dir::Left)),
            PAD_RIGHT => Some(UiAction::Move(Dir::Right)),
            HOME => Some(UiAction::ToggleMenu),
            _ => None,
        },
        UiEvent::Stick { x, y } => stick_dir(*x, *y).map(UiAction::Move),
    }
}

/// The one direction a stick pushed to `(x, y)` is asking for, `None` when it is
/// at rest.
///
/// The deadzone is [`DEADZONE`], the same margin the pad itself is read with, so
/// a stick a menu walks and a stick a game sees agree about where the edge of
/// "at rest" is. Of the two axes the winner is the one further from the centre:
/// a menu moves one step at a time, so a diagonal is the direction the stick is
/// nearer the edge of rather than two moves in one frame.
pub fn stick_dir(x: f64, y: f64) -> Option<Dir> {
    if !x.is_finite() || !y.is_finite() || x.hypot(y) <= DEADZONE {
        return None;
    }
    if x.abs() >= y.abs() {
        Some(if x > 0.0 { Dir::Right } else { Dir::Left })
    } else {
        Some(if y > 0.0 { Dir::Down } else { Dir::Up })
    }
}

/// Everything one reading of a pad is asking a menu for, each action once.
///
/// `buttons` is indexed by the standard gamepad button index and `axes` by the
/// standard axis index, both read straight out of a `Gamepad` by
/// [`crate::gamepad_events`] - the same shape [`super::input::gamepad::poll_snapshot`]
/// is handed, and read by the menu instead of the game while a menu is open.
///
/// `select` + `start` is in here rather than in [`ui_action`] because it is two
/// buttons, and it is the way in for a pad with no `home` button (see
/// [`super::input::gamepad::poll_hotkey`], which owns the same combination while
/// a game is running).
pub fn pad_actions(buttons: &[bool], axes: &[f64]) -> Vec<UiAction> {
    let mut actions: Vec<UiAction> = Vec::new();
    let mut ask = |action: Option<UiAction>| {
        if let Some(action) = action
            && !actions.contains(&action)
        {
            actions.push(action);
        }
    };
    for (index, held) in buttons.iter().enumerate() {
        if *held {
            ask(ui_action(&UiEvent::PadButton(
                u8::try_from(index).unwrap_or(u8::MAX),
            )));
        }
    }
    ask(axes
        .first()
        .zip(axes.get(1))
        .and_then(|(x, y)| ui_action(&UiEvent::Stick { x: *x, y: *y })));
    if buttons.get(usize::from(SELECT)) == Some(&true)
        && buttons.get(usize::from(START)) == Some(&true)
    {
        ask(Some(UiAction::ToggleMenu));
    }
    actions
}

/// The actions of `current` that `previous` was not already asking for: the ones
/// to run once for this reading, where a held direction is not run again but
/// belongs to [`RepeatTimer`].
pub fn fresh_actions(previous: &[UiAction], current: &[UiAction]) -> Vec<UiAction> {
    current
        .iter()
        .copied()
        .filter(|action| !previous.contains(action))
        .collect()
}

/// Who is holding the console, so that leaving a menu lets go of only what it
/// picked up.
///
/// `emulator::Controls` has one notion of pause, and a menu that simply paused on
/// the way in and resumed on the way out would happily start a game the player
/// stopped on purpose (M7-11, #144).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuPause {
    /// Whether the menu is the one holding the console.
    by_menu: bool,
}

impl MenuPause {
    /// A menu is opening over a console that `running` says is going: says whether
    /// the menu has to hold it, and remembers that it did.
    pub fn enter(&mut self, running: bool) -> bool {
        self.by_menu = running;
        running
    }

    /// A menu is closing: says whether to let the console go, and forgets about it
    /// either way, so the next menu reads the console as it finds it.
    pub fn leave(&mut self) -> bool {
        let resume = self.by_menu;
        self.by_menu = false;
        resume
    }

    /// The player pressed the pause button, so the console is theirs again and the
    /// menu must stop planning to resume it.
    pub fn by_hand(&mut self) {
        self.by_menu = false;
    }
}

/// What the player last touched, which decides the glyphs of the hint bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LastDevice {
    /// The keyboard, which is also the answer until anything else happens.
    #[default]
    Keyboard,
    /// A Gamepad API pad.
    Gamepad,
}

/// One item of the hint bar: the glyph to draw, and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    /// The glyph, as text: a key name or a pad button letter.
    pub glyph: &'static str,
    /// What the glyph does here.
    pub meaning: &'static str,
}

/// The three things a player can do in a menu, as the device in `device` says
/// them: move, select, back.
///
/// The meanings are the same in both lists and only the glyphs change, which is
/// what the hint bar is for: nobody reads a bar that tells a pad player about
/// `Escape`.
pub fn hints(device: LastDevice) -> [Hint; 3] {
    match device {
        LastDevice::Keyboard => [
            Hint {
                glyph: "\u{2191}\u{2193}\u{2190}\u{2192}",
                meaning: "Move",
            },
            Hint {
                glyph: "Enter",
                meaning: "Select",
            },
            Hint {
                glyph: "Esc",
                meaning: "Back",
            },
        ],
        LastDevice::Gamepad => [
            Hint {
                glyph: "\u{271A}",
                meaning: "Move",
            },
            Hint {
                glyph: "B",
                meaning: "Select",
            },
            Hint {
                glyph: "C",
                meaning: "Back",
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3x3 grid of 10x10 boxes, indexed row by row: `grid()[4]` is the centre.
    ///
    /// The gap between two neighbours' centres is 10, so a score is comparable by
    /// eye here: an aligned neighbour scores 10 and a diagonal one 40.
    fn grid() -> Vec<Rect> {
        let mut cells = Vec::new();
        for row in 0..3 {
            for column in 0..3 {
                cells.push(Rect::new(
                    f64::from(column) * 10.0,
                    f64::from(row) * 10.0,
                    10.0,
                    10.0,
                ));
            }
        }
        cells
    }

    #[test]
    fn from_the_centre_each_direction_picks_the_grid_neighbour() {
        let grid = grid();
        let centre = grid[4];
        for (dir, expected) in [
            (Dir::Up, 1),
            (Dir::Down, 7),
            (Dir::Left, 3),
            (Dir::Right, 5),
        ] {
            assert_eq!(next_focus(centre, &grid, dir), Some(expected), "{dir:?}");
        }
    }

    #[test]
    fn moving_out_of_a_corner_finds_nothing() {
        let grid = grid();
        // Top left: nothing above it and nothing left of it.
        let corner = grid[0];
        assert_eq!(next_focus(corner, &grid, Dir::Up), None);
        assert_eq!(next_focus(corner, &grid, Dir::Left), None);
        // And the same for the corner at the other end of the grid.
        let far = grid[8];
        assert_eq!(next_focus(far, &grid, Dir::Down), None);
        assert_eq!(next_focus(far, &grid, Dir::Right), None);
    }

    #[test]
    fn an_uneven_row_picks_the_visually_nearest() {
        // A row of buttons of different widths, and one above them that is not in
        // any of their columns: the answer is the button whose centre is nearest,
        // not the first or the last in the list.
        let current = Rect::new(60.0, 0.0, 40.0, 20.0);
        let row = vec![
            Rect::new(0.0, 40.0, 30.0, 20.0),
            Rect::new(40.0, 40.0, 25.0, 20.0),
            Rect::new(120.0, 40.0, 60.0, 20.0),
        ];
        assert_eq!(next_focus(current, &row, Dir::Down), Some(1));
    }

    #[test]
    fn an_aligned_control_beats_a_near_one_off_to_the_side() {
        // The 3x off-axis weight: `near` is closer overall (10 along, 8 off) and
        // `far` is further along but dead ahead. A menu is walked in columns, so
        // the one ahead wins even though it is further away.
        let current = Rect::new(0.0, 0.0, 10.0, 10.0);
        let row = vec![
            Rect::new(10.0, 6.0, 10.0, 10.0),
            Rect::new(20.0, -1.0, 10.0, 10.0),
        ];
        assert_eq!(next_focus(current, &row, Dir::Right), Some(1));
    }

    #[test]
    fn nothing_in_the_other_half_plane_is_a_candidate() {
        // Straight above and straight below are not "left" or "right" at all: a
        // column of controls has to be walked with up and down, not drifted.
        let current = Rect::new(0.0, 0.0, 10.0, 10.0);
        let above = vec![Rect::new(0.0, -10.0, 10.0, 10.0)];
        assert_eq!(next_focus(current, &above, Dir::Left), None);
        assert_eq!(next_focus(current, &above, Dir::Right), None);
    }

    #[test]
    fn a_held_direction_repeats_after_400ms_then_every_80() {
        let mut timer = RepeatTimer::default();
        timer.start(1_000.0);
        assert!(!timer.poll(1_100.0));
        assert!(!timer.poll(1_399.0));
        assert!(timer.poll(1_400.0));
        assert!(!timer.poll(1_440.0));
        assert!(timer.poll(1_480.0));
        // And the cadence holds while nobody lets go, without restarting.
        assert!(timer.poll(1_560.0));
        assert!(timer.poll(1_640.0));
    }

    #[test]
    fn a_timer_that_never_started_asks_for_nothing() {
        assert!(!RepeatTimer::default().poll(100_000.0));
    }

    #[test]
    fn letting_go_and_pressing_again_starts_the_wait_again() {
        let mut timer = RepeatTimer::default();
        timer.start(0.0);
        timer.stop();
        assert!(!timer.poll(10_000.0));
        timer.start(20_000.0);
        assert!(!timer.poll(20_300.0));
        assert!(timer.poll(20_400.0));
    }

    #[test]
    fn arrow_keys_move_the_focus() {
        for (code, dir) in [
            ("ArrowUp", Dir::Up),
            ("ArrowDown", Dir::Down),
            ("ArrowLeft", Dir::Left),
            ("ArrowRight", Dir::Right),
        ] {
            assert_eq!(
                ui_action(&UiEvent::Key(code)),
                Some(UiAction::Move(dir)),
                "{code}"
            );
        }
    }

    #[test]
    fn enter_and_space_confirm() {
        assert_eq!(ui_action(&UiEvent::Key("Enter")), Some(UiAction::Confirm));
        assert_eq!(ui_action(&UiEvent::Key("Space")), Some(UiAction::Confirm));
    }

    #[test]
    fn backspace_goes_back() {
        assert_eq!(ui_action(&UiEvent::Key("Backspace")), Some(UiAction::Back));
    }

    #[test]
    fn escape_toggles_the_menu() {
        // The same answer the keyboard's fixed hotkey table gives
        // ([`crate::input::keyboard::map_key`]), and it is `Back` from inside the
        // drawer because the drawer is the outermost thing a menu has - see
        // [`crate::menu::Menu::run`].
        assert_eq!(
            ui_action(&UiEvent::Key("Escape")),
            Some(UiAction::ToggleMenu)
        );
    }

    #[test]
    fn a_key_that_means_nothing_to_a_menu_is_none() {
        // `Tab` in particular: the browser moves focus with it, and taking it
        // would be taking it away from the drawer.
        assert_eq!(ui_action(&UiEvent::Key("Tab")), None);
        assert_eq!(ui_action(&UiEvent::Key("KeyZ")), None);
        assert_eq!(ui_action(&UiEvent::Key("F1")), None);
    }

    #[test]
    fn the_pad_d_pad_moves_and_the_stick_moves() {
        // Standard mapping: 12 up, 13 down, 14 left, 15 right.
        for (index, dir) in [
            (12, Dir::Up),
            (13, Dir::Down),
            (14, Dir::Left),
            (15, Dir::Right),
        ] {
            assert_eq!(
                ui_action(&UiEvent::PadButton(index)),
                Some(UiAction::Move(dir)),
                "{index}"
            );
        }
        // The left stick, axes 0 and 1 with down positive.
        assert_eq!(
            ui_action(&UiEvent::Stick { x: 0.0, y: -1.0 }),
            Some(UiAction::Move(Dir::Up))
        );
        assert_eq!(
            ui_action(&UiEvent::Stick { x: 1.0, y: 0.0 }),
            Some(UiAction::Move(Dir::Right))
        );
    }

    #[test]
    fn pad_b_confirms_and_pad_c_goes_back() {
        assert_eq!(ui_action(&UiEvent::PadButton(0)), Some(UiAction::Confirm));
        assert_eq!(ui_action(&UiEvent::PadButton(1)), Some(UiAction::Back));
    }

    #[test]
    fn the_pad_can_open_and_close_the_menu() {
        assert_eq!(
            ui_action(&UiEvent::PadButton(16)),
            Some(UiAction::ToggleMenu)
        );
    }

    #[test]
    fn a_stick_at_rest_and_an_unbound_button_are_nothing() {
        assert_eq!(ui_action(&UiEvent::Stick { x: 0.0, y: 0.0 }), None);
        // Inside the deadzone is still at rest.
        assert_eq!(ui_action(&UiEvent::Stick { x: 0.2, y: -0.1 }), None);
        assert_eq!(ui_action(&UiEvent::PadButton(7)), None);
        assert_eq!(ui_action(&UiEvent::PadButton(2)), None);
    }

    #[test]
    fn a_stick_picks_the_axis_it_is_further_from_centre_on() {
        // One `Move` for a diagonal, and it is the axis the stick is nearer to the
        // edge of: the d-pad has four positions and a menu walks one at a time.
        assert_eq!(
            ui_action(&UiEvent::Stick { x: 0.9, y: 0.4 }),
            Some(UiAction::Move(Dir::Right))
        );
        assert_eq!(
            ui_action(&UiEvent::Stick { x: 0.4, y: 0.9 }),
            Some(UiAction::Move(Dir::Down))
        );
    }

    #[test]
    fn pad_actions_reports_everything_the_pad_is_asking_for_once() {
        let mut buttons = vec![false; 17];
        buttons[0] = true;
        buttons[15] = true;
        buttons[16] = true;
        assert_eq!(
            pad_actions(&buttons, &[0.0, 0.0]),
            vec![
                UiAction::Confirm,
                UiAction::Move(Dir::Right),
                UiAction::ToggleMenu
            ]
        );
        // Nothing held, nothing asked - and a stick is folded in on its own.
        assert!(pad_actions(&[], &[0.0, 0.0]).is_empty());
        assert_eq!(
            pad_actions(&[], &[0.0, -1.0]),
            vec![UiAction::Move(Dir::Up)]
        );
    }

    #[test]
    fn select_and_start_together_open_the_menu_and_neither_alone() {
        // The way in for a pad with no `home` button, and `start` alone is still
        // the Genesis Start (see `crate::input::gamepad::poll_hotkey`).
        let mut both = vec![false; 17];
        both[8] = true;
        both[9] = true;
        assert!(pad_actions(&both, &[0.0, 0.0]).contains(&UiAction::ToggleMenu));
        let mut start = vec![false; 17];
        start[9] = true;
        let mut select = vec![false; 17];
        select[8] = true;
        assert!(!pad_actions(&start, &[0.0, 0.0]).contains(&UiAction::ToggleMenu));
        assert!(!pad_actions(&select, &[0.0, 0.0]).contains(&UiAction::ToggleMenu));
    }

    #[test]
    fn fresh_actions_are_the_ones_that_were_not_held_last_frame() {
        let previous = [UiAction::Move(Dir::Right)];
        let current = [UiAction::Move(Dir::Right), UiAction::Confirm];
        assert_eq!(fresh_actions(&previous, &current), vec![UiAction::Confirm]);
        // A held direction is not a fresh action: the [`RepeatTimer`] repeats it.
        assert!(fresh_actions(&previous, &previous).is_empty());
    }

    #[test]
    fn the_menu_resumes_only_what_it_stopped() {
        // A running console: the menu holds it, so leaving lets it go.
        let mut pause = MenuPause::default();
        assert!(pause.enter(true));
        assert!(pause.leave());
        // And once it has let go it does not hold the grudge.
        assert!(!pause.leave());
    }

    #[test]
    fn a_console_the_player_held_by_hand_survives_the_menu() {
        // Already paused, so the menu takes no credit for the hold and must not
        // start the game again on the way out.
        let mut pause = MenuPause::default();
        assert!(!pause.enter(false));
        assert!(!pause.leave());
    }

    #[test]
    fn holding_the_console_by_hand_forgets_the_menus_hold() {
        // Pause with the pad on the way into the drawer, then pause again by hand
        // inside it: from then on the player owns the state, so Escape must not
        // contradict what the pause button in the top bar says.
        let mut pause = MenuPause::default();
        assert!(pause.enter(true));
        pause.by_hand();
        assert!(!pause.leave());
    }

    #[test]
    fn the_hint_bar_names_the_device_in_hand() {
        let keyboard = hints(LastDevice::Keyboard);
        let gamepad = hints(LastDevice::Gamepad);
        // Same meanings, different glyphs.
        for (here, there) in keyboard.iter().zip(gamepad.iter()) {
            assert_eq!(here.meaning, there.meaning);
            assert_ne!(here.glyph, there.glyph);
        }
        assert_eq!(gamepad[1].glyph, "B");
        assert_eq!(gamepad[2].glyph, "C");
        assert_eq!(keyboard[1].glyph, "Enter");
        assert_eq!(keyboard[2].glyph, "Esc");
    }

    /// The hint bar is the shell's one key legend, so both of its halves have to be
    /// spelled like the rest of the shell is spelled (M7-19, #152): the keyboard
    /// half with the names [`keycap_label`] prints on a binding cell, and the pad
    /// half with the names [`PadButton::label`] prints on the touch overlay.
    #[test]
    fn the_hint_glyphs_are_spelled_the_way_the_shell_spells_them() {
        use crate::input::PadButton;
        use crate::settings::keycap_label;

        let keyboard = hints(LastDevice::Keyboard);
        assert_eq!(
            keyboard[0].glyph,
            format!(
                "{}{}{}{}",
                keycap_label("ArrowUp"),
                keycap_label("ArrowDown"),
                keycap_label("ArrowLeft"),
                keycap_label("ArrowRight"),
            ),
            "the d-pad hint is not the four arrow keycaps"
        );
        assert_eq!(keyboard[1].glyph, keycap_label("Enter"));
        assert_eq!(keyboard[2].glyph, keycap_label("Escape"));

        // The cross of a d-pad is the one glyph that is not a button name: a pad
        // has one directional control where a keyboard has four keycaps.
        let gamepad = hints(LastDevice::Gamepad);
        assert_eq!(gamepad[0].glyph, "\u{271A}");
        for hint in &gamepad[1..] {
            assert!(
                PadButton::ALL
                    .iter()
                    .any(|button| button.label() == hint.glyph),
                "the pad hint {:?} is not a name the shell uses for a pad button",
                hint.glyph
            );
        }
    }
}
