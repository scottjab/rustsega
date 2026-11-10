//! The I/O chip: the three controller ports and the 3-button pad on the first two.
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
//! ## What is not here
//!
//! The `TH` interrupt (control register bit 7) is stored and not acted on; the
//! serial mode behind the last nine registers stores and returns what it is
//! given, because the modem is out of scope and nothing but a modem uses them
//! (plutiedev "I/O ports", "Serial mode"). 6-button pads are M3-06 (#66).

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
/// The eight the 3-button pad has are the whole of what M3-05 reads; `X`, `Y`,
/// `Z` and `MODE` are the ones the 6-button pad adds (plutiedev "Controllers",
/// "Button layout"), which M3-06 (#66) reads out of the same `Buttons`.
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
    /// The `X` button, 6-button pad only (M3-06 #66).
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
    // The 6-button pad goes here in M3-06 (#66), which is why this is an enum
    // rather than an `Option<Buttons>`: the two answer `TH` differently and the
    // 6-button one has to be detected out from under a game that only expects
    // the 3-button answers.
}

impl Device {
    /// The levels this device puts on the port's pins 6-0 when `TH` is at `th`.
    /// Bit 7 is never a pin and is filled in by [`Port::read_data`].
    fn pin_values(self, th: bool) -> u8 {
        match self {
            // Nothing pulls any of the lines down.
            Self::None => PINS_HIGH,
            Self::Pad3(buttons) => pad_pin_values(buttons, th),
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
    let mut press = |pin: u8, button: Buttons| {
        if buttons.contains(button) {
            pins &= !pin;
        }
    };

    // Up and Down are wired straight to their pins and read the same on both
    // phases (segaretro's Line 1/Line 2: "the only two direct connections").
    press(1 << 0, Buttons::UP);
    press(1 << 1, Buttons::DOWN);
    if th {
        press(1 << 2, Buttons::LEFT);
        press(1 << 3, Buttons::RIGHT);
        press(1 << 4, Buttons::B);
        press(1 << 5, Buttons::C);
    } else {
        press(1 << 4, Buttons::A);
        press(1 << 5, Buttons::START);
        // The multiplexer selects its grounded inputs: pins 2 and 3 read 0
        // whatever the buttons are doing.
        pins &= !((1 << 2) | (1 << 3));
    }
    pins
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
    /// end of the cable is putting on it.
    fn read_data(&self) -> u8 {
        // `TH` is bit 6 of the data register and is what the pad multiplexes on,
        // whether or not the direction register says the pin is an output. A real
        // pad never drives `TH` (segaretro: line 7 is the multiplexer's select
        // input), so the line carries the console's bit either way.
        let th = self.data & TH != 0;
        let sensed = self.device.pin_values(th) | (self.data & LATCH_BIT);
        (self.ctrl & self.data) | (!self.ctrl & sensed)
    }
}

/// The I/O chip.
#[derive(Debug)]
pub struct Io {
    /// Ports 1, 2 and the expansion port, in that order.
    ports: [Port; 3],
    /// The nine serial mode registers at `0xA1000F-0xA1001F`, by address order.
    serial: [u8; SERIAL_COUNT],
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
        }
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
            Some(Register::Data(port)) => Some(self.ports[port].read_data()),
            Some(Register::Ctrl(port)) => Some(self.ports[port].ctrl),
            Some(Register::Serial(slot)) => Some(self.serial[slot]),
            None => None,
        }
    }

    /// Write `value` to the register at `addr`. Writes to an address with no
    /// register, and to the read-only registers, are dropped.
    pub fn write(&mut self, addr: u32, value: u8) {
        match Register::decode(addr) {
            Some(Register::Data(port)) => self.ports[port].data = value,
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

    /// Restore the power-on values of every register. Devices stay attached: a
    /// reset of the console does not unplug anything.
    ///
    /// Reached from the 68000 `RESET` instruction through `Bus`'s
    /// [`m68k::Bus::reset_devices`], which is "used by the 68000 `RESET`
    /// instruction" that "restarts the whole console, not just the CPU" - the
    /// console's RAM survives that, so a device reset touches devices only.
    pub fn reset(&mut self) {
        for port in &mut self.ports {
            port.data = DATA_RESET;
            port.ctrl = CTRL_RESET;
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
}
