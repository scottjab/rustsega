//! The I/O chip: the three controller ports and the 3- and 6-button pads on the
//! first two.
//!
//! The chip is small and completely unlike the rest of the console. It is sixteen
//! byte registers at 0xA10000-0xA1001F: one of them, the hardware version
//! register, is [`crate::Bus::version`] and not this module's (M3-04 #64); six are
//! the direction and pin latch of a port; and the remaining nine belong to a modem
//! nobody plugs in (plutiedev "I/O ports", which is the reference for everything
//! here and lists the registers with the assembly names repeated below).
//!
//! ```text
//! IoData1:  equ $A10003   IoCtrl1:     equ $A10009
//! IoData2:  equ $A10005   IoCtrl2:     equ $A1000B
//! IoDataExt: equ $A10007  IoCtrlExt:   equ $A1000D
//! IoTxData1: equ $A1000F  IoRxData1:   equ $A10011   IoSCtrl1: equ $A10013
//! IoTxData2: equ $A10015  IoRxData2:   equ $A10017   IoSCtrl2: equ $A10019
//! IoTxDataExt: equ $A1001B IoRxDataExt: equ $A1001D  IoSCtrlExt: equ $A1001F
//! ```
//!
//! ## The third port is not a pad
//!
//! `0xA10007`/`0xA1000D` are the port on the back of an early console, which
//! plutiedev "I/O ports" calls the modem port and "I/O port pinout" the EXT
//! port: the expansion peripheral connector. It is wired exactly like the two
//! pad ports and is modelled exactly like one, but nothing is ever attached to
//! it here, so it reads as an empty port.
//!
//! ## Byte registers behind a 16 bit bus
//!
//! Every register is one byte of a two byte pair and decodes without A0, so a
//! byte read at `0xA10002` answers as `0xA10003` does. [`crate::bus`] reaches this
//! module from its I/O region and owns the word rule that follows from this: see
//! `Bus::read_io_word`.
//!
//! ## Where a pad's time comes from
//!
//! A 6-button pad measures 1.5 ms of its own, so the chip has to be told what
//! time it is: [`Io::set_time`] carries a 68000 cycle count in, and that is all
//! the timing anything here does. There is no clock inside the chip, no scheduler
//! and no fake time for tests - M3-14 #75 is the ticket that runs the console
//! cycle by cycle and calls it.
//!
//! ## What is not here
//!
//! The `TH` interrupt (control register bit 7) is stored and not acted on; the
//! serial mode behind the last nine registers stores and returns what it is
//! given, because the modem is out of scope and nothing but a modem uses them
//! (plutiedev "I/O ports", "Serial mode"). The rest of what can be plugged into a
//! port - the mouse, the multitap, the light guns - is out of scope too, and
//! [`Device`] stays the place one of them would go.

/// Data port 1: the pin latch of the player 1 port (plutiedev "I/O ports",
/// `IoData1`).
const DATA_1: u32 = 0xA10003;
/// Data port 2, player 2 (`IoData2`).
const DATA_2: u32 = 0xA10005;
/// Data port 3, the expansion/modem port (`IoDataExt`). Not a pad port.
const DATA_3: u32 = 0xA10007;
/// Control port 1: the direction of player 1's pins (`IoCtrl1`).
const CTRL_1: u32 = 0xA10009;
/// Control port 2, player 2 (`IoCtrl2`).
const CTRL_2: u32 = 0xA1000B;
/// Control port 3, the expansion/modem port (`IoCtrlExt`).
const CTRL_3: u32 = 0xA1000D;
/// First and last of the serial mode registers, `IoTxData1` to `IoSCtrlExt`.
/// Nine byte registers, three per port, listed in plutiedev "I/O ports".
const SERIAL_START: u32 = 0xA1000F;
const SERIAL_END: u32 = 0xA1001F;

/// Data register bit 6: `TH`, the pin the pad's multiplexer selects with.
const TH: u8 = 0x40;
/// Data register bit 7. Not a pin on the connector at all: the register just
/// holds what was written to it, which is what a read returns
/// (segaretro "Control Pad (Mega Drive)", the register table: "Bit 7 latches the
/// value written to it").
const LATCH_BIT: u8 = 0x80;
/// Bits 5-0 of a pad's pin byte with nothing pressed: every button line idles
/// high (segaretro "Control Pad (Mega Drive)", "Technical information": the
/// line is pulled to +5 V through 10k and dragged to ground by the closed
/// switch).
const BUTTON_LINES_HIGH: u8 = 0x3F;
/// Every pin of an empty port reads high: the pad side of each line is pulled up
/// and nothing is pulling it down (plutiedev "Controllers", "Checking if there's
/// a controller": "If there's nothing in the port it should look as if no buttons
/// are pressed"). Bit 7 is clear because it is not a pin.
const PINS_HIGH: u8 = 0x7F;
/// The data register's power-on value: `TH` high and every pin line high, i.e.
/// a port that reads as nobody holding anything down.
///
/// Neither reference gives the power-on value of the port registers - plutiedev
/// "I/O ports" describes what the registers do and says nothing about their
/// initial state, and segaretro "Control Pad (Mega Drive)" is about the pad - so
/// this is the ticket's value, kept because it is the only one that cannot give a
/// game a phantom input. With `TH` high, an unconfigured port selects the pad bank
/// whose idle value is all ones; a 0 here would select the other bank, whose bits
/// 2 and 3 are wired to ground inside the pad, and a game reading the port before
/// it had set `TH` up would see Left and Right held down.
const DATA_RESET: u8 = 0x7F;
/// The control register's power-on value: 0 means input on every pin
/// (plutiedev "I/O ports": "Writing a 0 sets the pin to input"), and bit 7 low
/// means the external interrupt is off ("Make sure that bit 7 = 0 unless you need
/// to use external interrupts").
const CTRL_RESET: u8 = 0x00;
/// Number of serial mode registers: `0xA1000F-0xA1001F`, two bytes apart.
const SERIAL_COUNT: usize = 9;

/// One of the I/O chip's byte registers, by port or by serial slot.
///
/// The chip has one register per two bytes of the address space and A0 takes no
/// part in decoding it, which is why the pair `0xA10002`/`0xA10003` is one
/// register: plutiedev "I/O ports" lists them two bytes apart ("note how each
/// port address is two bytes apart"), and [`crate::bus`] says the same thing as
/// its `EVEN_LANE`.
enum Register {
    /// The pin latch of a port: `0xA10003`, `0xA10005`, `0xA10007`.
    Data(usize),
    /// The direction of a port's pins: `0xA10009`, `0xA1000B`, `0xA1000D`.
    Ctrl(usize),
    /// One of the serial mode registers, `0xA1000F-0xA1001F`, as a slot of
    /// [`Io::serial`].
    Serial(usize),
}

impl Register {
    /// Which register `addr` is, or [`None`] where the chip has none. The
    /// version register at `0xA10000`/`0xA10001` is deliberately `None` too: it
    /// is `Bus::version` (#64), not a register of the I/O chip.
    fn decode(addr: u32) -> Option<Self> {
        // A0 takes no part in the decode. Every register is documented at the odd
        // byte of its pair, so setting A0 maps either byte of the pair onto the
        // address the references list.
        match addr | 1 {
            DATA_1 => Some(Self::Data(0)),
            DATA_2 => Some(Self::Data(1)),
            DATA_3 => Some(Self::Data(2)),
            CTRL_1 => Some(Self::Ctrl(0)),
            CTRL_2 => Some(Self::Ctrl(1)),
            CTRL_3 => Some(Self::Ctrl(2)),
            addr @ (SERIAL_START..=SERIAL_END) => {
                Some(Self::Serial(((addr - SERIAL_START) >> 1) as usize))
            }
            _ => None,
        }
    }
}

/// The buttons of a Mega Drive pad, one bit each, as a front end reports them.
///
/// A hand-rolled bit set: `bitflags` would be a new dependency and
/// AGENTS.md forbids adding one to a core crate for this. Only the operations a
/// front end merging inputs and the pad model reading buttons need are here.
///
/// The eight the 3-button pad has are the whole of what [`Device::Pad3`] reads;
/// `X`, `Y`, `Z` and `MODE` are the ones the 6-button pad adds (plutiedev
/// "Controllers", "Button layout"), read out of the same `Buttons` by
/// [`Device::Pad6`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Buttons {
    bits: u16,
}

impl Buttons {
    /// D-pad up.
    pub const UP: Self = Self { bits: 1 << 0 };
    /// D-pad down.
    pub const DOWN: Self = Self { bits: 1 << 1 };
    /// D-pad left.
    pub const LEFT: Self = Self { bits: 1 << 2 };
    /// D-pad right.
    pub const RIGHT: Self = Self { bits: 1 << 3 };
    /// The `B` button.
    pub const B: Self = Self { bits: 1 << 4 };
    /// The `C` button.
    pub const C: Self = Self { bits: 1 << 5 };
    /// The `A` button.
    pub const A: Self = Self { bits: 1 << 6 };
    /// The `Start` button.
    pub const START: Self = Self { bits: 1 << 7 };
    /// The `X` button, 6-button pad only ([`Device::Pad6`]).
    pub const X: Self = Self { bits: 1 << 8 };
    /// The `Y` button, 6-button pad only.
    pub const Y: Self = Self { bits: 1 << 9 };
    /// The `Z` button, 6-button pad only.
    pub const Z: Self = Self { bits: 1 << 10 };
    /// The `Mode` button, 6-button pad only.
    pub const MODE: Self = Self { bits: 1 << 11 };

    /// No button held down.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Are all of `other`'s buttons held down?
    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }
}

impl std::ops::BitOr for Buttons {
    type Output = Self;

    /// Two button sets held at once, the way a front end merges a keyboard and a
    /// gamepad (docs/ARCHITECTURE.md, "Input: one `InputState` that merges
    /// keyboard, Gamepad API, and touch overlay into `Buttons` per port").
    fn bitor(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }
}

impl std::ops::BitOrAssign for Buttons {
    fn bitor_assign(&mut self, other: Self) {
        self.bits |= other.bits;
    }
}

/// What is plugged into a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// Nothing. The port reads as an idle one.
    None,
    /// The original 3-button pad (segaretro "Control Pad (Mega Drive)"), with
    /// these buttons held down.
    Pad3(Buttons),
    /// The 6-button pad (plutiedev "Controllers", "6-button controller"), with
    /// these buttons held down and its `TH` pulse counter where it is.
    ///
    /// Physically the 3-button pad's multiplexer plus a counter that `TH` clocks.
    /// The counter has four values and steps on each `TH` low-to-high edge, and a
    /// read answers one of the eight rows below: the counter picks the pair and
    /// the `TH` pin the row inside it, which is how eight rows come from a counter
    /// of four (see [`pad6_pin_values`]).
    ///
    /// ```text
    /// 1. TH=1  ?1CBRLDU     5. TH=1  ?1CBRLDU
    /// 2. TH=0  ?0SA00DU     6. TH=0  ?0SA0000   <- the 6-button detection step
    /// 3. TH=1  ?1CBRLDU     7. TH=1  ?1CBMXYZ   (bit 0 Z, bit 1 Y, bit 2 X, bit 3 Mode)
    /// 4. TH=0  ?0SA00DU     8. TH=0  ?0SA1111
    /// ```
    ///
    /// Then it wraps to 1. `?` is bit 7, the latch, which is not a pin
    /// ([`LATCH_BIT`]). The first five rows are what a 3-button pad answers, and
    /// that is what keeps old games working; the last three are this pad saying
    /// what it is. Row 6 is row 2 with its low nibble held low inside the pad,
    /// where a 3-button pad answers Up and Down, and that one difference is what a
    /// game checks (plutiedev "Controllers", "6-button controller"); row 7
    /// carries `X`/`Y`/`Z`/`Mode`; and row 8 holds the same four lines high, so
    /// that the zeros of row 6 cannot be a pad that is simply not driving them.
    Pad6(Pad6State),
}

impl Device {
    /// The levels this device puts on the port's pins 6-0 when `TH` is at `th`
    /// and the bus is at cycle `now`. Bit 7 is never a pin and is filled in by
    /// [`Port::read_data`].
    ///
    /// `&mut self` for [`Device::Pad6`]: a 6-button pad's answer depends on its
    /// counter, and looking at the counter is also when the pad's 1.5 ms timeout
    /// is noticed (see [`Pad6State::pin_values`]).
    fn pin_values(&mut self, now: u64, th: bool) -> u8 {
        match self {
            // Nothing pulls any of the lines down.
            Self::None => PINS_HIGH,
            Self::Pad3(buttons) => pad_pin_values(*buttons, th),
            Self::Pad6(pad) => pad.pin_values(now, th),
        }
    }
}

/// The byte a 3-button pad presents with `TH` at `th`: pins 5-0 with a pressed
/// button pulling its bit low, and bit 6 the `TH` line itself.
///
/// Inside the pad is a 74HC157 multiplexer (segaretro "Control Pad (Mega Drive)",
/// "Technical information"): `TH` is its select, and each of the four lines it
/// drives has one input tied to ground and the other to a button. So with `TH`
/// low, pins 2 and 3 are the two tied to ground and read 0 whatever the buttons
/// do, while `A` and `Start` appear on pins 4 and 5; with `TH` high those two
/// lines carry `Left` and `Right` and pins 4 and 5 carry `B` and `C`. Both
/// references agree on the table: plutiedev "Controllers", "Reading the buttons",
/// and the `TH = 0` / `TH = 1` rows of segaretro's table.
fn pad_pin_values(buttons: Buttons, th: bool) -> u8 {
    // Every button line idles high: the line is pulled to +5 V through 10k and
    // dragged to ground by the closed switch (segaretro "Control Pad (Mega
    // Drive)", "Technical information"). Bit 6 is the `TH` line itself.
    let mut pins = if th {
        BUTTON_LINES_HIGH | TH
    } else {
        BUTTON_LINES_HIGH
    };

    // Up and Down are wired straight to their pins and read the same on both
    // phases (segaretro's Line 1/Line 2: "the only two direct connections").
    press(&mut pins, 1 << 0, buttons, Buttons::UP);
    press(&mut pins, 1 << 1, buttons, Buttons::DOWN);
    if th {
        press(&mut pins, 1 << 2, buttons, Buttons::LEFT);
        press(&mut pins, 1 << 3, buttons, Buttons::RIGHT);
        press(&mut pins, 1 << 4, buttons, Buttons::B);
        press(&mut pins, 1 << 5, buttons, Buttons::C);
    } else {
        press(&mut pins, 1 << 4, buttons, Buttons::A);
        press(&mut pins, 1 << 5, buttons, Buttons::START);
        // The multiplexer selects its grounded inputs: pins 2 and 3 read 0
        // whatever the buttons are doing.
        pins &= !((1 << 2) | (1 << 3));
    }
    pins
}

/// `TH` pulse counter values of a 6-button pad: the four states of the counter,
/// in the order the console clocks it through them. Each is a pair of rows of the
/// table on [`Device::Pad6`], in that order - `STEP_FIRST` is the 1st and 2nd
/// steps, `STEP_DETECT` the 5th and 6th - and the `TH` pin chooses which row of
/// the pair a read gets.
///
/// The counter value the pad starts at, and the one it comes back to after the
/// timeout: rows 1 and 2, which answer exactly what a 3-button pad answers - as
/// does the next value, `1`, for rows 3 and 4. That is not a coincidence to
/// optimise away either: it is what a game that has never heard of the 6-button
/// pad reads on its way through a frame.
const STEP_FIRST: u8 = 0;
/// The counter value of the 5th and 6th steps, the 6th of which holds pins 3-0
/// low to tell the console that this is a 6-button pad.
const STEP_DETECT: u8 = 2;
/// The counter value of the 7th and 8th steps: `X`/`Y`/`Z`/`Mode`, and then pins
/// 3-0 all high.
const STEP_EXTRA: u8 = 3;
/// One past [`STEP_EXTRA`]: the counter is four wide and wraps round to
/// [`STEP_FIRST`] rather than running on to values no game has a table for.
const STEP_COUNT: u8 = 4;

/// The four pin lines that carry the 6-button pad's extra buttons on the 7th
/// step, and that the 6th step holds low and the 8th holds high: pins 3-0, i.e.
/// Up/Down/Left/Right to a 3-button pad and `Z`/`Y`/`X`/`Mode` here.
const EXTRA_BUTTON_LINES: u8 = 0x0F;

/// How long `TH` may stay where it is before a 6-button pad forgets its counter:
/// about 1.5 ms, which at the 68000's ~7.67 MHz (docs/ARCHITECTURE.md, "Clocks and
/// timing": master / 7) is 11_500 cycles.
///
/// MAME's 6-button pad uses the same figure to the microsecond
/// (`attotime::from_usec(1500)`, `ioport_out_6button` in
/// `src/mame/sega/megadriv.cpp`); BlastEm's `TH_TIMEOUT` in `io.c` is the same
/// order. A pad is not a precise device and no game can depend on the boundary,
/// since a game's own 6-button walk is a few dozen cycles long.
const SIX_BUTTON_TIMEOUT_CYCLES: u64 = 11_500;

/// One button's line: held down, the switch connects it to ground and the pin
/// reads low; released, the pull-up leaves it high (segaretro "Control Pad (Mega
/// Drive)", "Technical information").
fn press(pins: &mut u8, pin: u8, buttons: Buttons, button: Buttons) {
    if buttons.contains(button) {
        *pins &= !pin;
    }
}

/// The byte a 6-button pad presents with its `TH` counter at `step` and `TH` at
/// `th`: the table of [`Device::Pad6`], as a function of the counter state and
/// the `TH` pin that chooses inside its pair of rows.
fn pad6_pin_values(buttons: Buttons, step: u8, th: bool) -> u8 {
    // What a 3-button pad would answer in this position, which is what this pad
    // answers in five of its eight positions.
    let three = pad_pin_values(buttons, th);
    match (step, th) {
        // 6th step, `?0SA0000`: the four lines of `EXTRA_BUTTON_LINES` are wired to
        // ground inside the pad on this phase, so a game that reads 0000 here -
        // with Up and Down released, where a 3-button pad reads 0b0011 - knows it
        // has a 6-button pad (plutiedev "Controllers", "6-button controller").
        (STEP_DETECT, false) => three & !EXTRA_BUTTON_LINES,
        // 7th step, `?1CBMXYZ`.
        (STEP_EXTRA, true) => pad6_extra_pin_values(buttons),
        // 8th step, `?0SA1111`: the same four lines, held high, so the 0s of the
        // 6th step cannot be a pad that is simply not driving them at all.
        (STEP_EXTRA, false) => three & !EXTRA_BUTTON_LINES | EXTRA_BUTTON_LINES,
        // 1st, 2nd, 3rd, 4th and 5th steps: the 3-button pad's own two banks,
        // alternating.
        _ => three,
    }
}

/// The 7th step, `?1CBMXYZ`: pins 3-0 are `Mode`/`X`/`Y`/`Z`, pins 5-4 are still
/// `C` and `B`, and `TH` is high because this step is a write of `$40`.
///
/// `X` is pin 2 and `Z` is pin 0. plutiedev "Controllers" calls the swap out
/// twice, in the table and again under "Troubleshooting": "X is in bit 2 and Z in
/// bit 0. It's easy to miss this detail."
fn pad6_extra_pin_values(buttons: Buttons) -> u8 {
    let mut pins = BUTTON_LINES_HIGH | TH;
    press(&mut pins, 1 << 0, buttons, Buttons::Z);
    press(&mut pins, 1 << 1, buttons, Buttons::Y);
    press(&mut pins, 1 << 2, buttons, Buttons::X);
    press(&mut pins, 1 << 3, buttons, Buttons::MODE);
    press(&mut pins, 1 << 4, buttons, Buttons::B);
    press(&mut pins, 1 << 5, buttons, Buttons::C);
    pins
}

/// What a 6-button pad adds to a port: the buttons, and the state of the counter
/// that `TH` clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad6State {
    /// The buttons as the front end reports them, `X`/`Y`/`Z`/`Mode` included.
    buttons: Buttons,
    /// Which pair of the table on [`Device::Pad6`] the pad is on: one of
    /// [`STEP_FIRST`], [`STEP_DETECT`] or [`STEP_EXTRA`] (the other value, the 3rd
    /// and 4th steps, answers the same as `STEP_FIRST` and so needs no name).
    step: u8,
    /// The cycle of the last `TH` edge, which is what
    /// [`SIX_BUTTON_TIMEOUT_CYCLES`] is measured from. [`Io::set_time`] is what
    /// moves it, so a test that never moves the clock never times out.
    last_toggle: u64,
}

impl Pad6State {
    /// A pad with `buttons` held, its counter at the start of the sequence.
    ///
    /// `last_toggle` starts at 0 because the console it is attached to has just
    /// been powered on and its clock has too; the first `TH` edge, or
    /// [`Pad6State::reset`], puts a real stamp on it.
    pub fn new(buttons: Buttons) -> Self {
        Self {
            buttons,
            step: STEP_FIRST,
            last_toggle: 0,
        }
    }

    /// The levels this pad puts on its pins with `TH` at `th` at cycle `now`.
    fn pin_values(&mut self, now: u64, th: bool) -> u8 {
        self.reset_counter_after_idle(now);
        pad6_pin_values(self.buttons, self.step, th)
    }

    /// `TH` has just moved, to `high`.
    ///
    /// The counter steps on the low-to-high edge and not on the high-to-low one:
    /// MAME (`ioport_out_6button`, `src/mame/sega/megadriv.cpp`) and BlastEm
    /// (`io_data_write`, `io.c`) both advance their counter on the edge that ends
    /// with `TH` high and never on the other, and that is what makes plutiedev's
    /// 1st-7th step table come out right - the counter moves on the writes of the
    /// 3rd, 5th and 7th steps, not on those of the 2nd, 4th and 6th, and the
    /// 7th step's extra buttons are three counter steps after the console sets a
    /// port up.
    ///
    /// Every edge refreshes the timeout, which counts `TH` being toggled at all
    /// rather than being raised: the 3-button read toggles `TH` twice a frame and
    /// both of them are the pad being talked to.
    fn th_toggled(&mut self, now: u64, high: bool) {
        self.reset_counter_after_idle(now);
        if high {
            self.step = (self.step + 1) % STEP_COUNT;
        }
        self.last_toggle = now;
    }

    /// The counter restarts with the console: `RESET` is the whole console
    /// starting over, and a pad left mid-sequence by whatever just lost control
    /// would answer `X`/`Y`/`Z`/`Mode` to the first read of the new program.
    fn reset(&mut self, now: u64) {
        self.step = STEP_FIRST;
        self.last_toggle = now;
    }

    /// Put the counter back at the start if `TH` has not moved for
    /// [`SIX_BUTTON_TIMEOUT_CYCLES`].
    ///
    /// Done at the moment the counter is looked at, on both the read path and the
    /// `TH` edge, rather than from a timer or a callback: the pad does have a
    /// 1.5 ms one-shot, but nothing in the emulator wakes up when it fires, and a
    /// read or an edge is the only time the answer can differ from what it would
    /// have been without the timeout. BlastEm does the same in both places
    /// (`io_data_write` and `io_data_read` in `io.c`), and this is also what keeps
    /// the pad testable without a scheduler: [`Io::set_time`] moves the clock, and
    /// the pad notices when it is next asked a question.
    ///
    /// `saturating_sub` so that a `now` that is somehow behind `last_toggle` reads
    /// as "no time has passed" rather than wrapping into an instant timeout.
    fn reset_counter_after_idle(&mut self, now: u64) {
        if now.saturating_sub(self.last_toggle) > SIX_BUTTON_TIMEOUT_CYCLES {
            self.step = STEP_FIRST;
        }
    }
}

/// One controller port: the two registers of the I/O chip that drive it, and
/// whatever is plugged into it.
#[derive(Debug, Clone, Copy)]
pub struct Port {
    /// The data register: the level the console drives on each pin, and the
    /// latch bit 7 reads back.
    data: u8,
    /// The control register: a set bit makes that pin an output driven by
    /// `data` (plutiedev "I/O ports", "Basic usage").
    ctrl: u8,
    /// The peripheral on the other end of the cable.
    device: Device,
}

impl Port {
    /// The value the data register reports (plutiedev "I/O ports": "Reading back
    /// from the data port returns the current values of all pins (both input and
    /// output)").
    ///
    /// A pin the control register says is an output reads back the level the
    /// console is driving, and every other pin reads what the device on the far
    /// end of the cable is putting on it. `now` is the chip's cycle stamp,
    /// needed by the 6-button pad's timeout ([`Io::set_time`]).
    fn read_data(&mut self, now: u64) -> u8 {
        // `TH` is bit 6 of the data register and is what the pad multiplexes on,
        // whether or not the direction register says the pin is an output. A real
        // pad never drives `TH` (segaretro: line 7 is the multiplexer's select
        // input), so the line carries the console's bit either way.
        let th = self.data & TH != 0;
        let sensed = self.device.pin_values(now, th) | (self.data & LATCH_BIT);
        (self.ctrl & self.data) | (!self.ctrl & sensed)
    }

    /// The console wrote the data register.
    ///
    /// For a 6-button pad the interesting thing is not the byte but whether `TH`
    /// moved: the counter is clocked by the edge, so writing the same level again
    /// is not a step of the sequence. `was_high` is read out of the register
    /// before it is updated, and out of `data` rather than out of the driven
    /// level, because `TH` carries the console's bit either way (see
    /// [`Port::read_data`]).
    fn write_data(&mut self, value: u8, now: u64) {
        let was_high = self.data & TH != 0;
        let is_high = value & TH != 0;
        if let Device::Pad6(pad) = &mut self.device
            && was_high != is_high
        {
            pad.th_toggled(now, is_high);
        }
        self.data = value;
    }
}

/// The I/O chip.
#[derive(Debug)]
pub struct Io {
    /// Ports 1, 2 and the expansion port, in that order.
    ports: [Port; 3],
    /// The nine serial mode registers at `0xA1000F-0xA1001F`, by address order.
    serial: [u8; SERIAL_COUNT],
    /// The 68000 cycle count the pads measure their `TH` timeouts against, put
    /// there by [`Io::set_time`] and 0 until the first call.
    now: u64,
}

impl Io {
    /// A chip with nothing attached to any port and every register at its
    /// power-on value.
    pub fn new() -> Self {
        Self {
            ports: [const {
                Port {
                    data: DATA_RESET,
                    ctrl: CTRL_RESET,
                    device: Device::None,
                }
            }; 3],
            serial: [0; SERIAL_COUNT],
            now: 0,
        }
    }

    /// Tell the chip what cycle the 68000 is at, so that a 6-button pad can
    /// measure the 1.5 ms of quiet that resets its `TH` counter
    /// ([`SIX_BUTTON_TIMEOUT_CYCLES`]).
    ///
    /// A cycle count passed in by whoever is running the machine, rather than a
    /// clock the chip owns or shares by reference, because the chip is not the
    /// thing that knows what time it is: there is no scheduler yet, and the
    /// [`crate::Bus`] has no cycle counter either. Nothing here needs to advance
    /// time to be correct - a pad only ever asks "how long since `TH` moved?" at
    /// the moment it is read or written - so the chip stores the number and
    /// nothing else, and a test can be a sequence of writes, reads and cycle
    /// stamps with no machine behind it.
    ///
    /// M3-14 #75, the scheduler, is what calls this in production: once per cycle
    /// slice, as `bus.io.set_time(cycles)`. A test that never calls it leaves the
    /// clock at 0, which is what the tests that do not care about the timeout
    /// want, and time only moves when a test says it does.
    pub fn set_time(&mut self, cycles: u64) {
        self.now = cycles;
    }

    /// The byte at I/O address `addr`, or [`None`] where the chip has no
    /// register and the caller should read open bus. `addr` is already masked to
    /// 24 bits, and the caller has already decided it is in the I/O area.
    ///
    /// Not every address in the area is this module's: `0xA10000`/`0xA10001` is
    /// the hardware version register, which lives on [`crate::Bus::version`]
    /// (M3-04 #64) and which the bus answers before asking here.
    pub fn read(&mut self, addr: u32) -> Option<u8> {
        match Register::decode(addr) {
            Some(Register::Data(port)) => Some(self.ports[port].read_data(self.now)),
            Some(Register::Ctrl(port)) => Some(self.ports[port].ctrl),
            Some(Register::Serial(slot)) => Some(self.serial[slot]),
            None => None,
        }
    }

    /// Write `value` to the register at `addr`. Writes to an address with no
    /// register, and to the read-only registers, are dropped.
    pub fn write(&mut self, addr: u32, value: u8) {
        match Register::decode(addr) {
            Some(Register::Data(port)) => self.ports[port].write_data(value, self.now),
            Some(Register::Ctrl(port)) => {
                // Bit 7 is the `TH` interrupt enable. It is stored and nothing
                // more: the external interrupt it arms is IRQ 2, taken from pin 6
                // going low, which only a light gun uses (plutiedev "I/O ports",
                // "External interrupt"). TH interrupts are out of #65's scope, so
                // the bit is kept for the ticket that implements it rather than
                // thrown away here.
                self.ports[port].ctrl = value;
            }
            Some(Register::Serial(slot)) => self.serial[slot] = value,
            None => {}
        }
    }

    /// Attach a 3-button pad to port `port` (0 = player 1, 1 = player 2,
    /// 2 = the expansion port) reporting `buttons`.
    ///
    /// This is the front end's entry point: `Buttons` is what it merges keyboard,
    /// gamepad and touch into, and it is called every frame with the result
    /// (docs/ARCHITECTURE.md, "Input").
    ///
    /// A caller naming port 2 here is asking to put a pad on the expansion port,
    /// which is the connector a modem or a Head Setting goes into; nothing in the
    /// emulator does that, and until some peripheral is modelled for it the port is
    /// read as an empty one. Panics for a port that does not exist.
    pub fn set_buttons(&mut self, port: usize, buttons: Buttons) {
        self.ports[port].device = Device::Pad3(buttons);
    }

    /// Attach a 6-button pad to port `port` (0 = player 1, 1 = player 2,
    /// 2 = the expansion port) reporting `buttons`, whose `X`, `Y`, `Z` and
    /// `MODE` bits the pad reads out on the 7th step of its sequence
    /// ([`Device::Pad6`]).
    ///
    /// Which of the two pads a port has is the front end's config choice, which is
    /// what #66 exposes in place of the pad's own compatibility mode: a real
    /// 6-button pad held down in `Mode` while the console powers up answers as a
    /// 3-button one for the rest of its power cycle (plutiedev "Controllers",
    /// "6-button controller": "Do not do anything that requires holding down the
    /// Mode button when turning on the console, since that's how 6-button
    /// controllers go into 3-button compatibility mode"), and the emulator has no
    /// power-on moment to hang that on. Attaching a `Pad3` is that mode, chosen
    /// per port and per run.
    ///
    /// Panics for a port that does not exist, as [`Io::set_buttons`] does.
    pub fn set_buttons_6(&mut self, port: usize, buttons: Buttons) {
        self.ports[port].device = Device::Pad6(Pad6State::new(buttons));
    }

    /// Restore the power-on values of every register. Devices stay attached: a
    /// reset of the console does not unplug anything. A 6-button pad's `TH`
    /// counter is device state rather than a register, and it restarts too.
    ///
    /// Reached from the 68000 `RESET` instruction through `Bus`'s
    /// [`m68k::Bus::reset_devices`], which is "used by the 68000 `RESET`
    /// instruction" that "restarts the whole console, not just the CPU" - the
    /// console's RAM survives that, so a device reset touches devices only.
    pub fn reset(&mut self) {
        for port in &mut self.ports {
            port.data = DATA_RESET;
            port.ctrl = CTRL_RESET;
            // Device state, not register state, and it goes back too: a pad left
            // mid-sequence by the program that just lost the console would answer
            // `X`/`Y`/`Z`/`Mode` to the first read of the next one.
            if let Device::Pad6(pad) = &mut port.device {
                pad.reset(self.now);
            }
        }
        self.serial = [0; SERIAL_COUNT];
    }
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port 1 with a 3-button pad held in state `buttons`, set up the way a game
    /// sets a pad up: `$40` to the control port so `TH` is an output and every
    /// other pin an input, and `th` to the data port (plutiedev "Controllers",
    /// "Setting up").
    fn pad_on_port_1(buttons: Buttons, th: bool) -> Io {
        let mut io = Io::new();
        io.set_buttons(0, buttons);
        io.write(CTRL_1, 0x40);
        io.write(DATA_1, if th { TH } else { 0 });
        io
    }

    /// The setup half of the ticket's first test: an idle pad with `TH` high
    /// reads all ones. `0x7F` because bits 5-0 are the unpressed buttons and bit
    /// 6 is `TH` itself, both high, and bit 7 is the latch nobody has written to.
    #[test]
    fn idle_pad_reads_all_ones_th_high() {
        let mut io = pad_on_port_1(Buttons::empty(), true);
        assert_eq!(io.read(DATA_1), Some(0x7F));
    }

    /// `TH` high: bits 5-0 are `C B Right Left Down Up`, and nothing else
    /// (plutiedev "Controllers", the 1st step row of the table; segaretro's
    /// `TH = 1` row).
    #[test]
    fn th_high_reports_b_c_dpad() {
        // Up and C down, everything else released.
        let mut io = pad_on_port_1(Buttons::UP | Buttons::C, true);
        assert_eq!(io.read(DATA_1), Some(0x5E));

        // All six pins of this bank held at once leaves only `TH` high.
        let all =
            Buttons::UP | Buttons::DOWN | Buttons::LEFT | Buttons::RIGHT | Buttons::B | Buttons::C;
        let mut io = pad_on_port_1(all, true);
        assert_eq!(io.read(DATA_1), Some(TH));

        // And the two buttons of the other bank are not readable here: this is
        // what a game that never drops `TH` sees, and it is "no input pressed".
        let mut io = pad_on_port_1(Buttons::A | Buttons::START, true);
        assert_eq!(io.read(DATA_1), Some(0x7F));
    }

    /// `TH` low: bits 5-0 are `Start A 0 0 Down Up`, and bits 2 and 3 are 0
    /// however hard Left and Right are held, because the multiplexer has those
    /// two lines tied to ground on this phase (plutiedev "Controllers", 2nd step
    /// row; segaretro "Control Pad (Mega Drive)", `TH = 0` row).
    #[test]
    fn th_low_reports_a_start() {
        let held =
            Buttons::A | Buttons::START | Buttons::LEFT | Buttons::RIGHT | Buttons::B | Buttons::C;
        let mut io = pad_on_port_1(held, false);
        // bit0 Up 1, bit1 Down 1, bits 2-3 forced 0, bit4 A 0, bit5 Start 0,
        // bit6 TH 0, bit 7 never written.
        assert_eq!(io.read(DATA_1), Some(0x03));
    }

    /// A pin the control register says is an output reads back what the console
    /// wrote, not what the pad is driving (plutiedev "I/O ports", "Basic
    /// usage").
    #[test]
    fn output_pins_read_back_written_value() {
        // Ports 1's pins 0 and 5 are outputs as well as `TH`. Pin 0 is `Up`,
        // which the pad is holding low, and pin 5 is `C`'s line, which it is
        // holding high, so both directions of the read-back are observable.
        let mut io = Io::new();
        io.set_buttons(0, Buttons::UP | Buttons::LEFT);
        io.write(CTRL_1, 0x61);
        io.write(DATA_1, 0x41);
        // 0x5B = 0b0101_1011: bit0 the 1 that was written rather than the pad's
        // 0, bit5 the 0 that was written rather than the pad's 1, bit6 `TH` = 1,
        // bit2 `Left` - still an input - still the pad holding it down, and the
        // remaining input pins high.
        assert_eq!(io.read(DATA_1), Some(0x5B));

        // Driving pin 5 high moves that read bit with the write.
        io.write(DATA_1, 0x61);
        assert_eq!(io.read(DATA_1), Some(0x7B));
    }

    /// Bit 7 of the data register is not a pin: it is a latch, and a read returns
    /// the last bit written to it even though the direction register says the pin
    /// is an input.
    ///
    /// The verification the ticket asked for ("many emulators return 0").
    /// segaretro "Control Pad (Mega Drive)", the table under "Technical
    /// information": "Bit 7 latches the value written to it."
    #[test]
    fn data_bit7_reads_back_written_value() {
        let mut io = pad_on_port_1(Buttons::empty(), true);
        assert_eq!(io.read(DATA_1).map(|value| value & LATCH_BIT), Some(0));

        io.write(DATA_1, TH | LATCH_BIT);
        assert_eq!(
            io.read(DATA_1).map(|value| value & LATCH_BIT),
            Some(LATCH_BIT)
        );
    }

    /// The two ports are separate: separate directions, separate `TH`, separate
    /// pads.
    #[test]
    fn port2_independent_of_port1() {
        let mut io = Io::new();
        io.set_buttons(0, Buttons::empty());
        io.set_buttons(1, Buttons::UP);
        io.write(CTRL_1, 0x40);
        io.write(CTRL_2, 0x40);
        io.write(DATA_1, 0x00);
        io.write(DATA_2, TH);

        // Port 1 is on the `TH` low bank: bits 2-3 read 0 and A/Start are not
        // pressed. Port 2 is on the `TH` high bank with Up held.
        assert_eq!(io.read(DATA_1), Some(0x33));
        assert_eq!(io.read(DATA_2), Some(0x7E));
    }

    /// The serial mode registers are the rest of the byte registers of the block,
    /// `0xA1000F-0xA1001F`, and they keep what is written to them and do nothing
    /// else (plutiedev "I/O ports", "Serial mode"; the modem that uses them is
    /// out of scope).
    #[test]
    fn serial_registers_store_values() {
        let mut io = Io::new();
        let mut written = Vec::new();
        let mut value = 0x11u8;
        for addr in (SERIAL_START..=SERIAL_END).step_by(2) {
            io.write(addr, value);
            written.push((addr, value));
            value = value.wrapping_add(7);
        }
        assert_eq!(written.len(), SERIAL_COUNT);

        for (addr, value) in written {
            assert_eq!(io.read(addr), Some(value), "serial register {addr:#08x}");
            // A0 is not part of the decode, so either byte of the pair reaches
            // the same register.
            assert_eq!(io.read(addr ^ 1), Some(value), "pair of {addr:#08x}");
        }
    }

    /// `0xA10007`/`0xA1000D` are the expansion port. It behaves like a port, and
    /// nothing is attached to it, so it never reports a pad: no `TH` phase ever
    /// produces the forced 0s in bits 2-3 that a pad produces.
    #[test]
    fn expansion_port_reads_high_with_nothing_attached() {
        let mut io = Io::new();
        io.write(CTRL_3, 0x40);
        io.write(DATA_3, TH);
        assert_eq!(io.read(DATA_3), Some(0x7F));

        // `TH` low, and only the `TH` bit follows: bits 5-0 are still an empty
        // port, not a pad on its second phase.
        io.write(DATA_3, 0x00);
        assert_eq!(io.read(DATA_3), Some(0x3F));
    }

    /// Power-on and `RESET` values: the data register at [`DATA_RESET`], the
    /// control register at [`CTRL_RESET`], the serial registers at 0.
    #[test]
    fn reset_restores_power_on_register_values() {
        let mut io = Io::new();
        io.write(CTRL_1, 0xFF);
        io.write(DATA_1, 0x00);
        io.write(CTRL_2, 0x80);
        io.write(SERIAL_START, 0x5A);
        io.write(SERIAL_END, 0xA5);

        io.reset();

        assert_eq!(io.read(CTRL_1), Some(CTRL_RESET), "every pin an input");
        assert_eq!(io.read(CTRL_2), Some(CTRL_RESET));
        assert_eq!(io.read(SERIAL_START), Some(0));
        assert_eq!(io.read(SERIAL_END), Some(0));

        // `TH` high again, and an idle port reads all ones.
        io.write(CTRL_1, 0x40);
        assert_eq!(io.read(DATA_1), Some(0x7F));
    }

    /// `0xA10000`/`0xA10001`, the hardware version register, is not the I/O
    /// chip's: it is `Bus::version` (#64), and `Io` answers nothing there.
    #[test]
    fn version_register_is_not_mine() {
        let mut io = Io::new();
        io.write(0xA10001, 0x11);
        assert_eq!(io.read(0xA10000), None);
        assert_eq!(io.read(0xA10001), None);
        // And the area past the register block is nothing's either.
        assert_eq!(io.read(SERIAL_END + 2), None);
    }

    /// A 6-button pad on port 1, set up the way plutiedev "Controllers", "Setting
    /// up" sets a port up: `$40` to the control port so `TH` is an output, and
    /// `$40` to the data port so `TH` starts high. That write is not a `TH`
    /// transition - the register already held `TH` high from [`DATA_RESET`] - so
    /// the pad's counter is left at the start of its sequence, which is what a
    /// console that has just been powered on looks like from the pad's side.
    fn pad6_on_port_1(buttons: Buttons) -> Io {
        let mut io = Io::new();
        io.set_buttons_6(0, buttons);
        io.write(CTRL_1, 0x40);
        io.write(DATA_1, TH);
        io
    }

    /// The 6-button walk, as `(write this to the data port, read this back)`, for
    /// a pad with `X` and `Y` held down and bit 7 never written.
    ///
    /// The rows are plutiedev "Controllers", "6-button controller": its 1st-7th
    /// step, plus the read of the write that comes after the 7th and wraps the
    /// counter round again. Steps 1-5 are the two banks of the 3-button pad
    /// ([`pad_pin_values`]) alternating, and step 6 differs from steps 2 and 4
    /// only in bits 3-0 - which is the whole of how a game finds out that it is
    /// talking to a 6-button pad.
    const SIX_BUTTON_STEPS: [(u8, u8); 8] = [
        (TH, 0x7F), // 1: ?1CBRLDU
        (0, 0x33),  // 2: ?0SA00DU
        (TH, 0x7F), // 3: ?1CBRLDU
        (0, 0x33),  // 4: ?0SA00DU
        (TH, 0x7F), // 5: ?1CBRLDU
        (0, 0x30),  // 6: ?0SA0000, bits 3-0 held low by the pad
        (TH, 0x79), // 7: ?1CBMXYZ, with X and Y held: bits 2 and 1 low
        (0, 0x3F),  // 8: ?0SA1111
    ];

    /// Do step `index` of [`SIX_BUTTON_STEPS`] on port 1 at bus time `cycles` and
    /// return what the port answers.
    fn step_six_button(io: &mut Io, index: usize, cycles: u64) -> u8 {
        io.set_time(cycles);
        io.write(DATA_1, SIX_BUTTON_STEPS[index].0);
        io.read(DATA_1).expect("port 1 has a data register")
    }

    /// The whole 6-button sequence, and the wrap round.
    ///
    /// Bus time does not move here, so nothing in this test is allowed to time
    /// out; [`counter_does_not_reset_before_timeout`] and
    /// [`counter_resets_after_timeout`] are the same walk with the clock running.
    #[test]
    fn six_button_sequence_full_cycle() {
        let mut io = pad6_on_port_1(Buttons::X | Buttons::Y);

        for (index, (_, expected)) in SIX_BUTTON_STEPS.iter().enumerate() {
            assert_eq!(
                step_six_button(&mut io, index, 0),
                *expected,
                "step {} of the 6-button sequence",
                index + 1
            );
        }

        // The 9th write is the 1st step again: the counter is 4 wide, not 8, so it
        // does not run on into values a game has no table for.
        assert_eq!(
            step_six_button(&mut io, 0, 0),
            SIX_BUTTON_STEPS[0].1,
            "the step after the 8th wraps to the 1st"
        );
    }

    /// Step 6 is how a game knows a 6-button pad is on the port: bits 3-0 are all
    /// low even though Up and Down are released, where a 3-button pad answers step
    /// 2/4/6 with bits 1-0 high (plutiedev "Controllers", "6-button controller":
    /// "6th step is the important one: it tells you if a 3-button controller is in
    /// use. If bits 3-0 are not 0000, then you can assume it's a 3-button
    /// controller").
    #[test]
    fn six_button_detect_step_has_low_nibble_zero() {
        // Nothing pressed, so the four lines of the low nibble are the only things
        // the two pads can disagree about: Up and Down read high on a 3-button pad
        // here, and are wired low inside a 6-button one.
        let mut io = pad6_on_port_1(Buttons::empty());
        for index in 0..5 {
            step_six_button(&mut io, index, 0);
        }
        let detect = step_six_button(&mut io, 5, 0);
        assert_eq!(detect & 0x0F, 0, "bits 3-0 of the 6th step are 0000");
        assert_eq!(detect, 0x30, "the whole byte of the 6th step");

        // Two steps on, the same four lines are all high. The pair of answers -
        // 0000 here and 1111 two steps later - is what a game walks to, and a
        // 3-button pad keeps Up and Down high at both points instead.
        step_six_button(&mut io, 6, 0);
        assert_eq!(
            step_six_button(&mut io, 7, 0) & 0x0F,
            0x0F,
            "bits 3-0 of the 8th step are 1111"
        );

        // The 3-button pad, walked to the 6th step with Up and Down released,
        // answers 0b0011 in the low nibble, and that is what the game reads as
        // "3-button pad, stop here".
        let mut three = pad_on_port_1(Buttons::empty(), true);
        for index in 0..5 {
            step_six_button(&mut three, index, 0);
        }
        let same_step = step_six_button(&mut three, 5, 0);
        assert_eq!(
            same_step & 0x0F,
            0x03,
            "a 3-button pad's 6th step keeps Up and Down"
        );
    }

    /// The counter goes back to the start when `TH` has not moved for longer than
    /// [`SIX_BUTTON_TIMEOUT_CYCLES`], which is what makes a 3-button game's
    /// once-a-frame `TH` writes land on the first step every frame.
    #[test]
    fn counter_resets_after_timeout() {
        let mut io = pad6_on_port_1(Buttons::X | Buttons::Y);

        // One frame's walk, as a 6-button game does it, ending on the step that
        // reports X/Y/Z.
        for (index, (_, expected)) in SIX_BUTTON_STEPS.iter().enumerate().take(7) {
            assert_eq!(
                step_six_button(&mut io, index, 0),
                *expected,
                "step {} before the timeout",
                index + 1
            );
        }

        // `TH` stays high and the port is left alone for longer than the timeout.
        let after = SIX_BUTTON_TIMEOUT_CYCLES + 500;
        io.set_time(after);
        assert_eq!(
            io.read(DATA_1),
            Some(SIX_BUTTON_STEPS[0].1),
            "with `TH` high and no toggle for longer than the timeout, the pad is back at the 1st step"
        );

        // And the whole sequence runs again from the 1st step: the steps a 3-button
        // pad answers come round before this pad says anything about extra buttons.
        for (index, (write, expected)) in SIX_BUTTON_STEPS.iter().enumerate() {
            io.set_time(after + index as u64 + 1);
            io.write(DATA_1, *write);
            assert_eq!(
                io.read(DATA_1),
                Some(*expected),
                "step {} of the sequence after the timeout",
                index + 1
            );
        }
    }

    /// The timeout is 1.5 ms, and a frame's 6-button walk is a few dozen cycles:
    /// 5_000 cycles between every step of the walk is under the timeout, so the
    /// counter keeps counting and the X/Y/Z step still arrives. Were the timeout
    /// anything like 5_000 cycles, the pad would answer the 3-button bank here
    /// (0x7F) instead of the extra buttons (0x79).
    #[test]
    fn counter_does_not_reset_before_timeout() {
        let mut io = pad6_on_port_1(Buttons::X | Buttons::Y);

        for (index, (write, expected)) in SIX_BUTTON_STEPS.iter().enumerate() {
            // 5_000 cycles between steps, 40_000 across the whole walk: the pad
            // has been toggled often enough that none of it counts as idle.
            let cycles = 5_000 * (index as u64 + 1);
            io.set_time(cycles);
            io.write(DATA_1, *write);
            assert_eq!(
                io.read(DATA_1),
                Some(*expected),
                "step {} with 5_000 cycles between steps",
                index + 1
            );
        }
    }

    /// The compatibility guarantee of the whole protocol: a game that only ever
    /// does the 3-button read - `TH` high, read, `TH` low, read, once per frame,
    /// and frames are ~128_000 cycles apart - gets 3-button answers out of a
    /// 6-button pad every frame (plutiedev "Controllers": "Do not read the
    /// controllers more than once per frame! Doing so will break 6-button
    /// controllers which rely on timing to provide their backwards compatibility").
    #[test]
    fn three_button_game_reads_still_correct() {
        // Left and A: one button of each of the 3-button banks, so both reads of a
        // frame say something.
        let held = Buttons::LEFT | Buttons::A;
        let mut io = pad6_on_port_1(held);

        for frame in 0..4u64 {
            let start = frame * 128_000;
            io.set_time(start);
            io.write(DATA_1, TH);
            assert_eq!(
                io.read(DATA_1),
                Some(0x7B),
                "frame {frame}, `TH` high: C B Right Left Down Up with Left held"
            );
            io.set_time(start + 100);
            io.write(DATA_1, 0);
            assert_eq!(
                io.read(DATA_1),
                Some(0x23),
                "frame {frame}, `TH` low: Start A 0 0 Down Up with A held"
            );
        }

        // And `Device::Pad3` ignores the counter entirely: a 6-button game's
        // toggling of the line cannot move a pad that has no counter, however
        // many edges it makes and however far apart they are.
        let mut io = pad_on_port_1(held, true);
        for toggle in 0..20u64 {
            let th = if toggle % 2 == 0 { 0 } else { TH };
            io.set_time(toggle * 1_000);
            io.write(DATA_1, th);
            assert_eq!(
                io.read(DATA_1),
                Some(if toggle % 2 == 0 { 0x23 } else { 0x7B }),
                "3-button pad, toggle {toggle}"
            );
        }
    }

    /// `Device::Pad6` is chosen per port: walking port 1 through its extra steps
    /// leaves port 2 a 3-button pad, on its own `TH` and its own counter.
    #[test]
    fn six_button_pad_is_selected_per_port() {
        let mut io = Io::new();
        io.set_buttons_6(0, Buttons::X | Buttons::Y);
        // `X` on a 3-button pad is not a button at all: a 3-button pad has nowhere
        // to report it (plutiedev "Controllers", "Button layout").
        io.set_buttons(1, Buttons::X);
        io.write(CTRL_1, 0x40);
        io.write(CTRL_2, 0x40);
        io.write(DATA_1, TH);
        io.write(DATA_2, TH);

        for (index, (write, expected)) in SIX_BUTTON_STEPS.iter().enumerate() {
            io.write(DATA_1, *write);
            assert_eq!(
                io.read(DATA_1),
                Some(*expected),
                "port 1, 6-button pad, step {}",
                index + 1
            );

            io.write(DATA_2, *write);
            let three_button = if *write == TH { 0x7F } else { 0x33 };
            assert_eq!(
                io.read(DATA_2),
                Some(three_button),
                "port 2, 3-button pad, step {}",
                index + 1
            );
        }
    }

    /// `RESET` restarts a 6-button pad's counter along with the registers: the
    /// console has begun again, and a pad left mid-sequence would report X/Y/Z to
    /// a game that has only done the first step.
    #[test]
    fn reset_restarts_the_six_button_counter() {
        let mut io = pad6_on_port_1(Buttons::X | Buttons::Y);
        for index in 0..7 {
            step_six_button(&mut io, index, 0);
        }
        assert_eq!(
            io.read(DATA_1),
            Some(0x79),
            "mid-sequence, `TH` high reads the extra buttons"
        );

        io.reset();
        io.write(CTRL_1, 0x40);
        io.write(DATA_1, TH);
        assert_eq!(
            io.read(DATA_1),
            Some(0x7F),
            "after `RESET` the same read is the 1st step again"
        );

        // And the counter still counts after the reset.
        for (index, (write, expected)) in SIX_BUTTON_STEPS.iter().enumerate().skip(1) {
            io.write(DATA_1, *write);
            assert_eq!(
                io.read(DATA_1),
                Some(*expected),
                "step {} after the reset",
                index + 1
            );
        }
    }
}
