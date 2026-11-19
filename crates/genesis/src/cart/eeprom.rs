//! The cart's serial EEPROM: a 24Cxx on two bits of the cartridge bus.
//!
//! A cart that keeps its save in EEPROM rather than in SRAM says so in the same
//! place at header `0x1B0`, with the type byte `$E8` of segaretro "ROM header"
//! (see [`super::sram`] for the eleven other bytes), and its window is a single
//! address - 0x200001 for nearly every cart, and 0x200000-0x200001 for the
//! Acclaim 32 Mbit carts, which put the two lines on the two halves of the bus.
//! Nothing switches it in: there is no `0xA130F1` for it, and `crate::bus`
//! decides the order in `read_rom_byte` and `write_rom_byte`.
//!
//! What is behind the window is not a memory but a chip with a protocol. Two
//! lines run from the 68k to it, and the program walks them by hand:
//!
//! * a **start** is the data line falling while the clock is held high, and a
//!   **stop** is the same transition the other way;
//! * the chip takes each bit of a byte on a **rising edge of the clock**, and
//!   the program may only move the data line while the clock is low (Atmel
//!   AT24C01D/02D, "Data Valid, Start and Stop Conditions");
//! * after the eight bits of every byte it receives, the chip answers with an
//!   **acknowledge** - it holds the data line low for the whole of a ninth clock
//!   (Atmel doc0180, "Device Addressing"; the same for the X24C01).
//!
//! The chip is an I2C slave, and the two lines are the cartridge data bus, so
//! which bit of a cart access is which line is a property of the cart's board
//! ([`Board`]) and not of the chip: 0x200000-0x3FFFFF is the area the chip lives
//! in, and the boards differ over which half of every access reaches it.
//!
//! Which cart has which board and which chip, with where each number comes from
//! and where the sources disagree, is `docs/eeprom-games.md`.

use super::header::parse_header;
use serde::{Deserialize, Serialize};

/// The extra-memory type byte of a cart whose save is a serial EEPROM
/// (segaretro "ROM header", and [`super::sram`], which leaves `$E8` to this
/// module).
const TYPE_EEPROM: u8 = 0xE8;
/// The last address of the cartridge area, as in [`super::sram`]: a window
/// outside it is not something this bus can reach.
const CARTRIDGE_END: u32 = 0x3F_FFFF;
/// Where the lines of a cart that declares no window live: 0x200000-0x200001,
/// which is what the carts that declare one declare, and what the boards that
/// Genesis Plus GX lists by serial (the Acclaim 32 Mbit and the EA carts) decode.
const DEFAULT_BASE: u32 = 0x20_0000;

/// The quadrant the Acclaim 32 Mbit board's PAL owns: $200000-$2FFFFF, the one
/// range its only address inputs - A20 and A21, of the 670125/670127 board's
/// custom chip (8BitWizard's teardown, spritesmind t=107) - can name, and the
/// range Genesis Plus GX installs `mapper_acclaim_32M_write8`/`write16` over.
/// Both its word-write latch ([`Eeprom::write_word`]) and its armed read
/// window ([`Eeprom::read`]) live here and nowhere else.
const ACCLAIM_WINDOW: std::ops::Range<u32> = 0x20_0000..0x30_0000;

/// Which half of a 16-bit cart access reaches a line.
///
/// The carts differ here and nowhere else: Sega's and EA's boards take the byte
/// the low half of the bus carries, the Acclaim 16 Mbit board does not look at
/// the byte lanes at all, and the Acclaim 32 Mbit board puts the clock on one
/// half of every access and the data on the other (Genesis Plus GX's
/// `mapper_i2c_sega_init`, `mapper_i2c_ea_init`, `mapper_i2c_acclaim_16M_init`
/// and `mapper_i2c_acclaim_32M_init`, and `mapper_acclaim_32M_write8` for the
/// split).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane {
    /// Either half: the board ignores `/LWR` and `/UWR`.
    Both,
    /// Only the odd byte of a pair.
    Odd,
    /// Only the even byte of a pair.
    Even,
}

/// One of the two lines of a cart board: which half of an access carries it and
/// which bit of that half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    lane: Lane,
    bit: u8,
}

impl Pin {
    const fn on(lane: Lane, bit: u8) -> Self {
        Self { lane, bit }
    }

    /// Does the access at `addr` drive this line?
    fn drives(self, addr: u32) -> bool {
        match self.lane {
            Lane::Both => true,
            Lane::Odd => addr & 1 == 1,
            Lane::Even => addr & 1 == 0,
        }
    }

    /// The level this line carries in the byte `value`.
    fn level(self, value: u8) -> bool {
        value & (1 << self.bit) != 0
    }
}

/// How a cart's board wires the clock and the data line to the cartridge bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// The clock, which the 68k drives.
    pub scl: Pin,
    /// The data line as the 68k drives it.
    pub sda_in: Pin,
    /// The data line as the chip drives it back: the same wire, and so nearly
    /// always the same bit, with the Acclaim 16 Mbit board the one cart that
    /// reads it on a bit of its own.
    pub sda_out: Pin,
}

impl Board {
    /// Sega's own carts - the 171-5878, 171-6111, 171-6304 and 171-6584 boards:
    /// D1 is the clock and D0 the data, on the odd half of the bus.
    pub const SEGA: Self = Self {
        scl: Pin::on(Lane::Odd, 1),
        sda_in: Pin::on(Lane::Odd, 0),
        sda_out: Pin::on(Lane::Odd, 0),
    };
    /// Electronic Arts' PWA P10003 and P10004 boards: D6 and D7.
    pub const EA: Self = Self {
        scl: Pin::on(Lane::Odd, 6),
        sda_in: Pin::on(Lane::Odd, 7),
        sda_out: Pin::on(Lane::Odd, 7),
    };
    /// The Acclaim 16 Mbit board (P/N 670120): the clock on D1 and the data in
    /// on D0 of either half of an access, and the answer back on D1.
    pub const ACCLAIM_16M: Self = Self {
        scl: Pin::on(Lane::Both, 1),
        sda_in: Pin::on(Lane::Both, 0),
        sda_out: Pin::on(Lane::Odd, 1),
    };
    /// The Acclaim 32 Mbit board (P/N 670125 and 670127): D0 of each half, the
    /// clock on the even one and the data on the odd one.
    pub const ACCLAIM_32M: Self = Self {
        scl: Pin::on(Lane::Even, 0),
        sda_in: Pin::on(Lane::Odd, 0),
        sda_out: Pin::on(Lane::Odd, 0),
    };
}

/// A chip's shape: how much it holds, how its first byte is read, and how
/// writes wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    /// Bytes of storage.
    pub size: usize,
    /// What the first byte of a transfer means, and how many bytes of word
    /// address follow it. Mode 1, the X24C01: the first byte of a write
    /// carries a 7-bit word address and the read/write bit, and there is no
    /// second address byte (Xicor/ICmicro X24C01, "Byte Write": "a start
    /// condition followed by a seven bit word address and a write bit"). Mode
    /// 2, every other one-address-byte 24Cxx: the first byte is the device
    /// address - `1010`, three address bits, and the read/write bit - and the
    /// word address follows as its own byte (Atmel doc0180 and
    /// AT24C01D/02D, "Device Addressing"). Mode 3, the 24C32 and up: the
    /// device address again, and the word address as two bytes, high half
    /// first, of which the array answers to only the low ones (Atmel
    /// AT24C65 datasheet, doc2631, "Byte Write": "a write operation requires
    /// two 8-bit data word addresses following the device address word";
    /// Genesis Plus GX's `i2c_specs` give these parts `address_bits` 16).
    pub mode: Mode,
    /// Bytes a page write holds. A write that runs off the end of a page
    /// continues at the start of that page rather than crossing into the next
    /// one, and the bits above the page are not incremented at all (X24C01:
    /// "the most significant five bits of the word address define the page
    /// address ... the address counter will roll over"; AT24C01D/02D, "Page
    /// Write": "the lower three bits of the word address are internally
    /// incremented ... the higher order address bits are not incremented").
    pub page: usize,
}

/// The three first-byte shapes [`Chip`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// The X24C01: the word address rides in the first byte with the
    /// read/write bit.
    Mode1,
    /// A device address byte, then one word address byte.
    Mode2,
    /// A device address byte, then the word address as two bytes.
    Mode3,
}

impl Chip {
    /// Xicor X24C01, 1 Kbit organized 128 x 8 with a 4-byte page.
    pub const X24C01: Self = Self {
        size: 128,
        mode: Mode::Mode1,
        page: 4,
    };
    /// Xicor X24C02, 2 Kbit: "Internally Organized 256 x 8" and "Four Byte Page
    /// Write Operation", which is mode 2 addressing - the device type "fixed as
    /// 1010" - with twice the X24C01's array and the same page.
    ///
    /// The 4 is the Xicor part's own, then, and not Genesis Plus GX's guess:
    /// Atmel gives its 2 Kbit part eight bytes a page (doc0180, "8-byte Page (1K,
    /// 2K), 16-byte Page (4K, 8K, 16K) Write Modes"), which is the disagreement
    /// `docs/eeprom-games.md` records.
    pub const X24C02: Self = Self {
        size: 256,
        mode: Mode::Mode2,
        page: 4,
    };
    /// Atmel 24C01, 1 Kbit, mode 2 with an 8-byte page (doc0180).
    pub const AT24C01: Self = Self {
        size: 128,
        mode: Mode::Mode2,
        page: 8,
    };
    /// Atmel 24C02, 2 Kbit, mode 2 with an 8-byte page (doc0180).
    pub const AT24C02: Self = Self {
        size: 256,
        mode: Mode::Mode2,
        page: 8,
    };
    /// Atmel 24C04, 4 Kbit, mode 2 with a 16-byte page (doc0180).
    pub const AT24C04: Self = Self {
        size: 512,
        mode: Mode::Mode2,
        page: 16,
    };
    /// Atmel 24C08, 8 Kbit, mode 2 with a 16-byte page (doc0180).
    pub const AT24C08: Self = Self {
        size: 1024,
        mode: Mode::Mode2,
        page: 16,
    };
    /// Atmel 24C16, 16 Kbit, mode 2 with a 16-byte page (doc0180).
    pub const AT24C16: Self = Self {
        size: 2048,
        mode: Mode::Mode2,
        page: 16,
    };
    /// Atmel 24C65, 8 KB x 8 with a 64-byte page, and the mode 3 addressing:
    /// the word address comes as two bytes of which the array answers to the
    /// low thirteen, the top three of the high byte being don't-care (Atmel
    /// AT24C65 datasheet, doc2631; Genesis Plus GX's `i2c_specs` row
    /// `{address_bits 16, size_mask 0x1FFF, pagewrite_mask 0x3F}`). The chip
    /// of the three EA carts - T-81476 Frank Thomas Big Hurt Baseball and
    /// T-81576 College Slam on the Acclaim 32 Mbit board, and T-120146 (Brian
    /// Lara Cricket 96 / Shane Warne Cricket), whose board is the J-CART and
    /// another ticket (#356).
    pub const AT24C65: Self = Self {
        size: 8192,
        mode: Mode::Mode3,
        page: 64,
    };

    /// The mask a word address is kept inside.
    fn mask(&self) -> usize {
        self.size - 1
    }
}

/// What a cart's EEPROM is: where its two lines are, which board wires them,
/// and what chip is behind them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EepromInfo {
    /// Even address of the pair the lines live on: the chip answers at `base`
    /// and `base + 1`, which is the whole of what a cart declares - the usual
    /// 0x200001, whose pair is 0x200000-0x200001, and the two-address
    /// 0x200000-0x200001 of the Acclaim 32 Mbit carts.
    ///
    /// A window whose two addresses are one address is not a length of one, and
    /// the chip's array is the size of the chip rather than of the window: the
    /// window says where the two lines are, not how much hangs off them.
    pub base: u32,
    pub board: Board,
    pub chip: Chip,
}

impl EepromInfo {
    /// The window the twelve raw extra-memory bytes declare for an EEPROM, or
    /// `None` for a header that declares something else: no `RA`, a save RAM
    /// type, a window that is backwards or outside the cartridge area.
    fn declared(raw: &[u8; 12]) -> Option<u32> {
        if &raw[0..2] != b"RA" || raw[2] != TYPE_EEPROM {
            return None;
        }
        let start = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let end = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]);
        (start <= end && end <= CARTRIDGE_END).then_some(start & !1)
    }
}

/// What the 32 Mbit board's own quadrant shows on a read: the image, or the
/// chip.
///
/// The board's PAL (Genesis Plus GX's "custom LZ95A53", the custom mapper chip
/// of the 670125/670127 boards) decodes only A20 and A21, so its whole read
/// window is the 1 MiB quadrant $200000-$2FFFFF and the word write that both
/// /LWR and /UWR assert latches one bit of it - bit 0 of the word - into this
/// choice (Genesis Plus GX's `mapper_acclaim_32M_write16`). It is a read mux
/// rather than a bank register: no bank number is latched and ROM is never
/// swapped for ROM, which is why it lives on the EEPROM with the lines rather
/// than in `super::mapper`, and `docs/eeprom-games.md` records why the
/// "64 KB bank shift" the earlier text called it is not what the board does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Window {
    /// The ROM answers the quadrant: the reset state, and what a word write
    /// with bit 0 set arms. The ROM's output drives bit 0 of the pair too, so
    /// the chip's line is nowhere on the bus.
    Rom,
    /// The chip's data line is bit 0 of every byte the quadrant reads (Genesis
    /// Plus GX's `mapper_i2c_generic_read8`), which is how a game samples /SDA
    /// without the ROM fighting it: the game arms this at the top of every
    /// EEPROM operation and restores [`Window::Rom`] on the way out (NBA Jam
    /// TE's wrapper: `move.w #0,(A0)` at $000ADA, `move.w #1,(A0)` at $000AB2).
    Serial,
}

/// Where the transfer has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    /// Nothing to do: no start condition since the last stop, and no clocks
    /// either.
    Idle,
    /// Clocking in the first byte of a transfer, which is the word address and
    /// the read/write bit of a mode 1 chip and the device address of a mode 2
    /// or mode 3 one.
    Address,
    /// Mode 2 only: the device address is in and acknowledged, and the word
    /// address byte is next.
    Word,
    /// Mode 3 only: the device address is in and acknowledged, and the word
    /// address comes as two bytes, of which this is the high one (Atmel
    /// AT24C65 datasheet, doc2631: "a write operation requires two 8-bit data
    /// word addresses following the device address word").
    WordHigh,
    /// Mode 3 only: the high address byte is in and acknowledged, and the low
    /// one is next.
    WordLow,
    /// Taking data bytes for the word address in hand.
    Write,
    /// Sending data bytes for the word address in hand.
    Read,
}

/// The ninth clock of a byte, the one the acknowledge rides on, as the bit counter
/// sees it: `bit` counts the eight data clocks down from 7, and this is what it
/// holds between the eighth clock and the ninth.
const ACK: u8 = 8;

/// One cart's serial EEPROM: the array, the state of the two lines, and the byte
/// in progress.
#[derive(Debug, Serialize, Deserialize)]
pub struct Eeprom {
    pub info: EepromInfo,
    /// The array, which is what [`crate::Bus::sram`] hands a front end.
    data: Vec<u8>,
    dirty: bool,
    /// The two lines as the 68k last drove them. Both come up high: they are
    /// pull-up'd, and nothing driving them reads as high.
    scl: bool,
    sda: bool,
    /// The same two lines one cart access earlier, which is all that is needed
    /// to see a start, a stop, and a rising edge of the clock.
    was_scl: bool,
    was_sda: bool,
    state: State,
    /// Which clock the byte in hand is on: 7 to 0 for the eight bits of it, and
    /// [`ACK`] from the eighth clock's rising edge to the ninth's.
    bit: u8,
    /// The bits of the byte coming in, most significant first.
    coming: u8,
    /// The byte going out, which is shifted a word address at a time rather than
    /// a bit at a time: [`Eeprom::read`] is called with the clock already up.
    going: u8,
    /// What the chip is holding the data line at, or `None` when it is released
    /// and the 68k's own level is what a read sees.
    line: Option<bool>,
    /// The three address bits of a mode 2 device address, which are the top of
    /// the word address on a chip with more than one page of addresses and
    /// nothing at all on one with a single page (Atmel doc0180: "The next 3 bits
    /// are the A2, A1 and A0 device address bits ... These page addressing bits
    /// ... should be considered the most significant bits of the data word
    /// address which follows"). A mode 3 part's pins name the chip on the board
    /// instead and feed nothing into the address, whose whole two bytes follow
    /// (doc2631; Genesis Plus GX never lets them in either).
    select: u8,
    /// The word address the next byte is for.
    word: usize,
    /// What the 32 Mbit board's quadrant shows on a read, [`Window::Rom`] on
    /// every way in: the PAL's /RES input is what a reset reaches, and the
    /// games' wrappers restore the ROM window after every operation.
    window: Window,
}

impl Eeprom {
    /// A chip of `info`'s shape, with an empty array.
    ///
    /// The array comes up as 0xFF for the reasons
    /// [`super::sram::Sram::new`] gives for the save RAM: nothing to do with the
    /// chip, and a game looking for a save finds none.
    pub fn new(info: EepromInfo) -> Self {
        Self {
            info,
            data: vec![0xFF; info.chip.size],
            dirty: false,
            scl: true,
            sda: true,
            was_scl: true,
            was_sda: true,
            state: State::Idle,
            bit: ACK,
            coming: 0,
            going: 0,
            line: None,
            select: 0,
            word: 0,
            window: Window::Rom,
        }
    }

    /// The array itself, for [`crate::Bus::sram`].
    pub fn contents(&self) -> &[u8] {
        &self.data
    }

    /// Put an array back, as [`crate::Bus::load_sram`] does, refusing one of any
    /// other length whole for the reason [`super::sram::Sram::load`] gives.
    pub fn load(&mut self, data: &[u8]) {
        if data.len() != self.data.len() {
            return;
        }
        self.data.copy_from_slice(data);
    }

    /// Has the array changed since this was last asked, clearing it? Set by every
    /// byte the chip takes, which is what the 68k wrote to it.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    /// Restart the chip: the lines come back to idle and the transfer in hand is
    /// abandoned, because a reset is the 68k stopping in the middle of walking
    /// them, and the 32 Mbit board's read window goes back to ROM, which is
    /// where the PAL's /RES input puts it. The array and its dirty flag are not
    /// touched, as for [`super::sram::Sram::reset`].
    pub fn reset(&mut self) {
        self.scl = true;
        self.sda = true;
        self.was_scl = true;
        self.was_sda = true;
        self.state = State::Idle;
        self.bit = ACK;
        self.line = None;
        self.window = Window::Rom;
    }

    /// A cart access at `addr` writing `value`: what the 68k put on the bus is
    /// the two lines, and the chip answers it.
    ///
    /// Answers whether the access drove either line, which is `false` for every
    /// address outside the chip's pair of addresses and for the half of that pair
    /// a board puts neither line on.
    pub fn write(&mut self, addr: u32, value: u8) -> bool {
        if !self.covers(addr) {
            return false;
        }
        let board = self.info.board;
        // The data line first, and the clock second: a cart that sets both with
        // one access means the data to be there for the edge, and one look at
        // the pair after both is what lets it.
        if board.sda_in.drives(addr) {
            self.sda = board.sda_in.level(value);
        }
        if board.scl.drives(addr) {
            self.scl = board.scl.level(value);
        }
        if (self.scl, self.sda) == (self.was_scl, self.was_sda) {
            return true;
        }
        self.tick();
        true
    }

    /// The 32 Mbit board's word write: the read-window latch, and nothing else
    /// ([`Window`]). Answers whether the board consumed the word, which is only
    /// ever true for this one board and only inside its own quadrant
    /// ([`ACCLAIM_WINDOW`]) - [`crate::Bus::write_rom_word`] hands the word
    /// here first and skips its usual two byte writes when the board took it,
    /// because with both /LWR and /UWR asserted this board drives no line at
    /// all (Genesis Plus GX's `mapper_acclaim_32M_write16` against its
    /// `mapper_acclaim_32M_write8`).
    pub fn write_word(&mut self, addr: u32, value: u16) -> bool {
        if self.info.board != Board::ACCLAIM_32M || !ACCLAIM_WINDOW.contains(&addr) {
            return false;
        }
        self.window = if value & 1 == 0 {
            Window::Serial
        } else {
            Window::Rom
        };
        true
    }

    /// A cart access reading `addr`, of which `image` is what the cartridge slot
    /// answers there: the chip replaces the one bit of that byte its data line
    /// is on, and leaves every other address of the cart alone.
    ///
    /// On the 32 Mbit board "the one bit" is not a matter of the pair but of
    /// the whole quadrant, and only while the game's word write has armed the
    /// serial window ([`Window::Serial`]); in the ROM window the image byte is
    /// the whole answer, the ROM driving bit 0 of the pair included.
    ///
    /// `None` when the chip holds nothing for this address, which is every
    /// address but the one its board reads back on.
    pub fn read(&self, addr: u32, image: u8) -> Option<u8> {
        let pin = self.info.board.sda_out;
        let answers = if self.info.board == Board::ACCLAIM_32M {
            // The window latch, not the lane: the PAL drives D0 over its whole
            // quadrant for any read, even and odd alike, which is what Genesis
            // Plus GX's quadrant-wide `mapper_i2c_generic_read8` models.
            ACCLAIM_WINDOW.contains(&addr) && self.window == Window::Serial
        } else {
            self.covers(addr) && pin.drives(addr)
        };
        if !answers {
            return None;
        }
        // Released, the line keeps the level the 68k left it at, which is what a
        // cart reads back between the chip's own answers.
        let level = self.line.unwrap_or(self.sda);
        let bit = u8::from(level) << pin.bit;
        Some((image & !(1 << pin.bit)) | bit)
    }

    /// Is `addr` one of the two addresses the chip's lines live on?
    fn covers(&self, addr: u32) -> bool {
        (addr & !1) == self.info.base
    }

    /// One change of the two lines.
    fn tick(&mut self) {
        let (scl, sda) = (self.scl, self.sda);
        if self.was_scl && scl {
            // The clock is being held high, so a move of the data line is not a
            // bit: down is a start and up is a stop.
            if self.was_sda && !sda {
                self.begin();
            } else if !self.was_sda && sda {
                self.state = State::Idle;
                self.line = None;
            }
        } else if !self.was_scl && scl {
            self.rising();
        }
        self.was_scl = scl;
        self.was_sda = sda;
    }

    /// A start condition: the transfer in hand is abandoned and a byte is
    /// starting. The word address is *not* reset, because reading the address the
    /// chip stopped at is what a mode 2 cart does: a start, the device address
    /// with the write bit to load the address, and a second start with the read
    /// bit to have it back.
    fn begin(&mut self) {
        self.bit = ACK - 1;
        self.coming = 0;
        self.line = None;
        self.state = State::Address;
    }

    /// A rising edge of the clock, which is where the chip takes a bit
    /// (Atmel AT24C01D/02D, "Serial Data Transfer": "input ... on the rising edge
    /// of SCL"). The falling edge carries nothing, which is why nothing counts
    /// it: a byte is complete on the rising edge of its ninth clock, and so a
    /// stop condition issued as soon as the acknowledge is seen still follows a
    /// byte that has been taken.
    fn rising(&mut self) {
        match self.state {
            State::Idle => {}
            State::Read => {
                if self.bit == ACK {
                    // The ninth clock of a byte going out is the master's
                    // answer: an acknowledge means another byte, and nothing
                    // means it has finished reading (X24C01, "Sequential Read":
                    // the read "is terminated by the master by not responding
                    // with an acknowledge"). The array is what runs off the end:
                    // "the address counter will roll over to 0".
                    if !self.sda {
                        self.word = (self.word + 1) & self.info.chip.mask();
                        self.going = self.data[self.word];
                    } else {
                        self.state = State::Idle;
                    }
                    self.line = None;
                } else {
                    self.line = Some(self.going & (1 << self.bit) != 0);
                }
            }
            _ => {
                if self.bit == ACK {
                    self.accept();
                    // The acknowledge: held for the whole of this clock (Atmel
                    // AT24C01D/02D: "a Logic 0 during the entire high period of
                    // the ninth clock cycle").
                    self.line = Some(false);
                } else {
                    self.coming |= u8::from(self.sda) << self.bit;
                    self.line = None;
                }
            }
        }
        self.bit = if self.bit == 0 { ACK } else { self.bit - 1 };
    }

    /// The ninth clock of a byte coming in: the chip has the byte, and says what
    /// it means by it.
    fn accept(&mut self) {
        let byte = self.coming;
        self.coming = 0;
        let mask = self.info.chip.mask();
        match self.state {
            State::Address => {
                let read = byte & 1 != 0;
                match self.info.chip.mode {
                    Mode::Mode1 => {
                        self.word = (byte >> 1) as usize & mask;
                        self.state = if read { State::Read } else { State::Write };
                    }
                    Mode::Mode2 => {
                        self.select = (byte >> 1) & 0b111;
                        self.state = if read { State::Read } else { State::Word };
                    }
                    // The device bits name the chip, not a part of the
                    // address: the whole word address follows in two bytes.
                    Mode::Mode3 => {
                        self.state = if read { State::Read } else { State::WordHigh };
                    }
                }
                if read {
                    self.going = self.data[self.word];
                }
            }
            State::Word => {
                self.word = (((self.select as usize) << 8) | byte as usize) & mask;
                self.state = State::Write;
            }
            State::WordHigh => {
                // The top bits above the array are don't-care - the 24C65
                // answers to 13 of the 16 bits its two address bytes carry
                // (doc2631, and Genesis Plus GX's `i2c_specs` size_mask - its
                // GET_WORD_ADR_HIGH never latches a bit above it) - and the
                // mask here is what ignores them.
                self.word = (byte as usize) << 8;
                self.state = State::WordLow;
            }
            State::WordLow => {
                self.word = (self.word | byte as usize) & mask;
                self.state = State::Write;
            }
            State::Write => {
                self.data[self.word] = byte;
                self.dirty = true;
                // Within a page the address wraps, and the page itself does not.
                let page = self.info.chip.page - 1;
                self.word = (self.word & !page) | ((self.word + 1) & page);
            }
            State::Read | State::Idle => {}
        }
    }
}
/// The carts whose EEPROM is not written in their own header, and so has to be
/// written down here: serial, board, and chip.
///
/// The list is Genesis Plus GX's `i2c_database` minus one group: the Codemasters
/// J-CART carts, whose EEPROM shares its registers with the J-CART bank
/// switching and is written at a different window from where it is read - which
/// is also where the third 24C65 cart, `T-120146` (Brian Lara Cricket 96 /
/// Shane Warne Cricket), waits: its chip is mode 3 now, but its board is the
/// J-CART (#356). `docs/eeprom-games.md` has the whole table, the source for
/// each row, and what is left over and why.
pub const EEPROM_GAMES: &[(&str, Board, Chip)] = &[
    // Sega's boards, all X24C01.
    ("T-12046", Board::SEGA, Chip::X24C01), // Mega Man: The Wily Wars
    ("T-12053", Board::SEGA, Chip::X24C01), // Rockman: Mega World
    ("MK-1215", Board::SEGA, Chip::X24C01), // Evander "Real Deal" Holyfield's Boxing
    ("MK-1228", Board::SEGA, Chip::X24C01), // Greatest Heavyweights of the Ring
    ("G-5538", Board::SEGA, Chip::X24C01),  // Greatest Heavyweights of the Ring (J)
    ("PR-1993", Board::SEGA, Chip::X24C01), // Greatest Heavyweights of the Ring (proto)
    ("G-4060", Board::SEGA, Chip::X24C01),  // Wonder Boy in Monster World
    (
        "00001211",
        Board::SEGA,
        Chip::X24C01, /* Sports Talk Baseball */
    ),
    (
        "00004076",
        Board::SEGA,
        Chip::X24C01, /* Honoo no Toukyuuji: Dodge Danpei */
    ),
    ("G-4524", Board::SEGA, Chip::X24C01), // Ninja Burai Densetsu
    ("00054503", Board::SEGA, Chip::X24C01), // Game Toshokan
    // Electronic Arts' boards, all X24C01 and all with no extra memory in the
    // header at all.
    ("T-50176", Board::EA, Chip::X24C01), // Rings of Power
    ("T-50396", Board::EA, Chip::X24C01), // NHLPA Hockey 93
    ("T-50446", Board::EA, Chip::X24C01), // John Madden Football 93
    (
        "T-50516",
        Board::EA,
        Chip::X24C01, /* John Madden Football 93 (Championship Edition) */
    ),
    (
        "T-50606",
        Board::EA,
        Chip::X24C01, /* Bill Walsh College Football, whose save RAM header is bad */
    ),
    // The Acclaim 16 Mbit board, X24C02.
    ("T-81033", Board::ACCLAIM_16M, Chip::X24C02), // NBA Jam (J)
    ("T-081326", Board::ACCLAIM_16M, Chip::X24C02), // NBA Jam (UE)
    // The Acclaim 32 Mbit board, which declares 0x200000-0x200001 rather than
    // nothing only for some of them.
    ("T-081276", Board::ACCLAIM_32M, Chip::AT24C02), // NFL Quarterback Club
    ("T-81406", Board::ACCLAIM_32M, Chip::AT24C04),  // NBA Jam TE
    ("T-081586", Board::ACCLAIM_32M, Chip::AT24C16), // NFL Quarterback Club '96
    // The two 24C65 carts, mode 3: Genesis Plus GX's i2c_database has both on
    // mapper_i2c_acclaim_32M_init, and both declare the same window in their
    // own headers as well ("RA" $E8 0x200000-0x200001).
    (
        "T-81476",
        Board::ACCLAIM_32M,
        Chip::AT24C65, /* Frank Thomas Big Hurt Baseball */
    ),
    ("T-81576", Board::ACCLAIM_32M, Chip::AT24C65), // College Slam
];

/// The board and chip of the cart `serial`, from [`EEPROM_GAMES`].
///
/// A substring match rather than the whole of the serial, because the header's
/// serial carries the `"GM "` software type and a `"-00"` revision on either
/// side of the part that names the cart, and because that is how every other
/// emulator's list matches too.
pub fn eeprom_override(serial: &str) -> Option<(Board, Chip)> {
    EEPROM_GAMES
        .iter()
        .find(|(known, _, _)| serial.contains(known))
        .map(|(_, board, chip)| (*board, *chip))
}

/// What EEPROM the ROM image `rom` carries, or `None` for a cart that has
/// neither an EEPROM in its header or [`EEPROM_GAMES`] in its serial.
///
/// A cart that declares battery-backed save RAM keeps it, and that is the whole
/// of why [`super::sram`] and this module cannot both answer: the two are
/// decoded from one field, and the carts that shipped with a save RAM window
/// and a serial EEPROM are carts whose EEPROM had been replaced by a save RAM
/// hack. Genesis Plus GX does the same thing in the other order - it trusts its
/// list over the header, and marks the entries where that matters by a leading
/// space (T-12046, T-12053 and G-4060, "warning: SRAM hack exists").
///
/// The board and the chip come from [`EEPROM_GAMES`] when the cart is in it,
/// and from the Sega board and the X24C01 otherwise, which is what Genesis Plus
/// GX assumes for a cart that only says `$E8`.
pub fn detect(rom: &[u8]) -> Option<EepromInfo> {
    let header = parse_header(rom).ok()?;
    if super::sram::detect(rom).is_some() {
        return None;
    }
    let declared = EepromInfo::declared(&header.extra_memory);
    let known = eeprom_override(&header.serial);
    match (declared, known) {
        (Some(base), Some((board, chip))) => Some(EepromInfo { base, board, chip }),
        (Some(base), None) => Some(EepromInfo {
            base,
            board: Board::SEGA,
            chip: Chip::X24C01,
        }),
        (None, Some((board, chip))) => Some(EepromInfo {
            base: DEFAULT_BASE,
            board,
            chip,
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chip of `chip`'s shape on Sega's board at the address every cart that
    /// declares one declares.
    fn chip_on_sega(chip: Chip) -> EepromInfo {
        EepromInfo {
            base: DEFAULT_BASE,
            board: Board::SEGA,
            chip,
        }
    }

    /// The two lines of a board, walked by hand the way a 68k program does it:
    /// the data line where the chip can see it, the clock on its own edge, and
    /// the answer read back from the bit the board reads on.
    struct Master {
        chip: Eeprom,
        /// The byte last written to each half of the pair, so that moving one
        /// line leaves the other where it was.
        bus: [u8; 2],
    }

    impl Master {
        fn new(info: EepromInfo) -> Self {
            Self {
                chip: Eeprom::new(info),
                bus: [0xFF, 0xFF],
            }
        }

        fn lane(pin: Pin) -> usize {
            match pin.lane {
                Lane::Even => 0,
                Lane::Odd | Lane::Both => 1,
            }
        }

        fn set(&mut self, pin: Pin, level: bool) {
            let lane = Self::lane(pin);
            let byte = &mut self.bus[lane];
            *byte = if level {
                *byte | (1 << pin.bit)
            } else {
                *byte & !(1 << pin.bit)
            };
            let addr = self.chip.info.base + lane as u32;
            assert!(self.chip.write(addr, *byte), "the board's own line");
        }

        fn scl(&mut self, level: bool) {
            let pin = self.chip.info.board.scl;
            self.set(pin, level);
        }

        fn sda(&mut self, level: bool) {
            let pin = self.chip.info.board.sda_in;
            self.set(pin, level);
        }

        /// The data line as the chip is holding it. Read with every other bit of
        /// the byte clear so that an answer can only be the line.
        fn line(&self) -> bool {
            let pin = self.chip.info.board.sda_out;
            let addr = self.chip.info.base + Self::lane(pin) as u32;
            self.chip.read(addr, 0).expect("the read-back line") & (1 << pin.bit) != 0
        }

        fn start(&mut self) {
            self.scl(true);
            self.sda(true);
            self.sda(false);
            self.scl(false);
        }

        fn stop(&mut self) {
            self.sda(false);
            self.scl(true);
            self.sda(true);
        }

        /// One clock pulse, and what the chip held the line at through it.
        fn clock(&mut self, level: bool) -> bool {
            self.sda(level);
            self.scl(true);
            let answer = self.line();
            self.scl(false);
            answer
        }

        /// A byte to the chip, and whether it acknowledged: an acknowledge is the
        /// line held low for the ninth clock, which is a nothing from the chip's
        /// point of view and so the inverse of what the clock answers.
        fn send(&mut self, byte: u8) -> bool {
            for bit in (0..8).rev() {
                self.clock(byte & (1 << bit) != 0);
            }
            // The ninth clock, with the line released for the chip's answer.
            !self.clock(true)
        }

        /// A byte from the chip, answered with `ack` to keep the reading going or
        /// with nothing to end it: an acknowledge is the master holding the line
        /// low for the ninth clock, so it is the level below the one that means
        /// "enough".
        fn recv(&mut self, ack: bool) -> u8 {
            let mut byte = 0u8;
            for bit in (0..8).rev() {
                if self.clock(true) {
                    byte |= 1 << bit;
                }
            }
            self.clock(!ack);
            byte
        }

        /// Write `bytes` to `word`, and stop.
        fn write(&mut self, word: usize, bytes: &[u8]) {
            let first = if self.chip.info.chip.mode == Mode::Mode1 {
                (word << 1) as u8
            } else {
                0xA0
            };
            self.start();
            assert!(self.send(first), "the chip acknowledges its address");
            if self.chip.info.chip.mode == Mode::Mode3 {
                assert!(self.send((word >> 8) as u8), "the high address byte");
            }
            if self.chip.info.chip.mode != Mode::Mode1 {
                assert!(self.send(word as u8), "the word address");
            }
            for byte in bytes {
                assert!(self.send(*byte));
            }
            self.stop();
        }

        /// `count` bytes read from `word`.
        fn read(&mut self, word: usize, count: usize) -> Vec<u8> {
            let mode1 = self.chip.info.chip.mode == Mode::Mode1;
            self.start();
            assert!(self.send(if mode1 { ((word << 1) | 1) as u8 } else { 0xA0 }));
            if !mode1 {
                // The address, then the device address again with the read
                // bit behind a second start: a random read.
                if self.chip.info.chip.mode == Mode::Mode3 {
                    assert!(self.send((word >> 8) as u8), "the high address byte");
                }
                assert!(self.send(word as u8), "the word address");
                self.stop();
                self.start();
                assert!(self.send(0xA1), "the chip acknowledges a reading");
            }
            let mut out = Vec::new();
            for i in 0..count {
                out.push(self.recv(i + 1 < count));
            }
            self.stop();
            out
        }
    }

    /// The raw extra-memory bytes of a header, as [`super::sram`] builds them for
    /// its own tests.
    fn extra_memory(kind: u8, start: u32, end: u32) -> [u8; 12] {
        let mut raw = [0u8; 12];
        raw[0..2].copy_from_slice(b"RA");
        raw[2] = kind;
        raw[3] = 0x20;
        raw[4..8].copy_from_slice(&start.to_be_bytes());
        raw[8..12].copy_from_slice(&end.to_be_bytes());
        raw
    }

    /// A ROM image `len` bytes long with `serial` at 0x180 and `raw` at 0x1B0.
    fn rom(serial: &str, raw: &[u8; 12], len: usize) -> Vec<u8> {
        let mut rom = vec![0u8; len];
        rom[0x180..0x180 + serial.len()].copy_from_slice(serial.as_bytes());
        rom[0x1B0..0x1BC].copy_from_slice(raw);
        rom
    }

    #[test]
    fn start_and_stop_detected() {
        let mut master = Master::new(chip_on_sega(Chip::X24C01));
        // A word address and a data byte, clocked at the chip with no start in
        // front of either: the chip is in standby and takes none of it, which is
        // what the absence of an acknowledge says.
        assert!(!master.send(0x02), "no acknowledge without a start");
        assert!(!master.send(0x5A));
        assert!(!master.chip.take_dirty(), "and nothing stored");

        // The same two bytes behind a start are a write.
        master.start();
        assert!(master.send(0x02), "and one with a start");
        assert!(master.send(0x5A));
        master.stop();
        assert!(master.chip.take_dirty());
        assert_eq!(master.chip.contents()[1], 0x5A);

        // A stop ends the transfer: the byte after it is standby again.
        assert!(!master.send(0x02));
        assert!(!master.send(0xA5), "and nothing more is taken");
        assert!(!master.chip.take_dirty());
    }

    #[test]
    fn write_then_read_byte_mode1() {
        // The X24C01 takes the word address in the first byte with the read and
        // write bit, which is the whole of its addressing.
        let mut master = Master::new(chip_on_sega(Chip::X24C01));
        master.write(7, &[0x5A]);
        assert_eq!(master.chip.contents()[7], 0x5A);
        assert_eq!(master.read(7, 1), vec![0x5A]);
        // 0 is a word address like any other, and the top of the 128 bytes is
        // the top of the 7 bits that address it.
        master.write(0, &[0x11]);
        master.write(127, &[0x22]);
        assert_eq!(master.read(0, 1), vec![0x11]);
        assert_eq!(master.read(127, 1), vec![0x22]);
    }

    #[test]
    fn write_then_read_byte_mode2() {
        let mut master = Master::new(chip_on_sega(Chip::AT24C02));
        master.write(5, &[0x7B]);
        assert_eq!(master.chip.contents()[5], 0x7B);
        assert_eq!(master.read(5, 1), vec![0x7B]);
        assert_eq!(master.read(0, 1), vec![0xFF], "nothing else moved");

        // The three address bits of the device byte are the top of the word
        // address on a chip with more than one page of it: $A0 and $A2 are the
        // low and the high 256 bytes of a 24C04 (Atmel doc0180).
        let mut master = Master::new(chip_on_sega(Chip::AT24C04));
        master.start();
        assert!(master.send(0xA2));
        assert!(master.send(0x00));
        assert!(master.send(0x9C));
        master.stop();
        assert_eq!(master.chip.contents()[256], 0x9C);
        assert_eq!(master.chip.contents()[0], 0xFF, "and not the bottom page");
    }

    /// Mode 3, the two-byte word address of the 24C65: a device byte, then the
    /// word address as a high byte and a low byte, then (or before, for a read)
    /// the data. The 24C65s of the three EA carts take their whole array this
    /// way, and no part of it fits the one address byte the other modes carry.
    #[test]
    fn write_then_read_byte_mode3() {
        let mut master = Master::new(chip_on_sega(Chip::AT24C65));

        // A byte write at $1FFF, two address bytes and all.
        master.start();
        assert!(master.send(0xA0), "the chip acknowledges its address");
        assert!(master.send(0x1F), "the high address byte");
        assert!(master.send(0xFF), "the low address byte");
        assert!(master.send(0x5A));
        master.stop();
        assert_eq!(master.chip.contents()[0x1FFF], 0x5A);

        // A random read of it: the address loaded by a write-shaped sequence,
        // then the device address again with the read bit.
        master.start();
        assert!(master.send(0xA0));
        assert!(master.send(0x1F));
        assert!(master.send(0xFF));
        master.stop();
        master.start();
        assert!(master.send(0xA1), "the chip acknowledges a reading");
        assert_eq!(master.recv(false), 0x5A);
        master.stop();

        // A sequential read across a 256-byte boundary mid-array, the place
        // the two address bytes hand the count over: the low byte rolls from
        // $FD through $FF to $00 and the high byte it sits on goes with it.
        let array: Vec<u8> = (0..8192).map(|i| i as u8).collect();
        master.chip.load(&array);
        master.start();
        assert!(master.send(0xA0));
        assert!(master.send(0x00));
        assert!(master.send(0xFD));
        master.stop();
        master.start();
        assert!(master.send(0xA1));
        assert_eq!(master.recv(true), 0xFD);
        assert_eq!(master.recv(true), 0xFE);
        assert_eq!(master.recv(true), 0xFF);
        assert_eq!(master.recv(true), 0x00, "across the boundary");
        assert_eq!(master.recv(false), 0x01);
        master.stop();

        // The top address bits are don't-care: the 24C65 uses 13 of the 16
        // bits two address bytes carry, and a bit above the array is ignored
        // rather than wrapped or made part of the address (Atmel AT24C65
        // datasheet, doc2631; Genesis Plus GX's i2c_specs for the 24C65 are
        // {address_bits 16, size_mask 0x1FFF}, and its GET_WORD_ADR_HIGH never
        // latches a bit above size_mask). A high byte with the top three bits
        // set is therefore the same address as without them.
        master.start();
        assert!(master.send(0xA0));
        assert!(
            master.send(0x9F),
            "the same address as $1F, spelled with garbage above the array"
        );
        assert!(master.send(0xFF));
        assert!(master.send(0x5A));
        master.stop();
        assert_eq!(master.chip.contents()[0x1FFF], 0x5A);
        master.start();
        assert!(master.send(0xA0));
        assert!(master.send(0x1F));
        assert!(master.send(0xFF));
        assert!(master.send(0x2B));
        master.stop();
        assert_eq!(
            master.chip.contents()[0x1FFF],
            0x2B,
            "the same $1FFF, overwritten"
        );
    }

    /// The same four bytes on the wire, $A0 $1F $FF $5A, mean something else to
    /// every addressing: on the X24C01 the first byte *is* the address and a
    /// write bit, so $1F $FF $5A are data at $50; on a one-address-byte part
    /// the word address is $1F, $FF is the data there, and $5A wraps with the
    /// X24C02's four-byte page onto $1C, the page's first address; on the
    /// 24C65 of the test above the bytes name $1FFF. A sequence is not an
    /// address until the chip's mode says what to count.
    #[test]
    fn a_mode3_sequence_means_something_else_to_the_other_modes() {
        let sequence = [0xA0, 0x1F, 0xFF, 0x5A];
        let walk = |chip: Chip| {
            let mut master = Master::new(chip_on_sega(chip));
            master.start();
            for byte in sequence {
                assert!(master.send(byte), "the chip acknowledges {byte:#04x}");
            }
            master.stop();
            master
        };

        // Mode 1: the first byte is a word address and a write bit.
        let master = walk(Chip::X24C01);
        assert_eq!(&master.chip.contents()[0x50..0x53], &[0x1F, 0xFF, 0x5A]);

        // Mode 2: the first byte is the device address, the second the whole
        // word address, and the rest data - with $1F the last byte of the
        // X24C02's four-byte page, the second data byte wraps onto $1C (Atmel
        // doc0180, "Page Write").
        let master = walk(Chip::X24C02);
        assert_eq!(master.chip.contents()[0x1F], 0xFF);
        assert_eq!(
            master.chip.contents()[0x1C],
            0x5A,
            "the second data byte wrapped with the page"
        );

        // Mode 3: the first byte is the device address and the next two the
        // word address, so nothing lands until $1FFF itself.
        let master = walk(Chip::AT24C65);
        assert_eq!(master.chip.contents()[0x1FFF], 0x5A);
        assert_eq!(
            master.chip.contents()[0x1F],
            0xFF,
            "and the mode-2 addresses are empty"
        );
        assert_eq!(master.chip.contents()[0x1C], 0xFF);
    }

    #[test]
    fn page_write_wraps_within_page() {
        // Five bytes behind one word address on the X24C01, whose page is four
        // bytes: the fifth wraps to the start of the page and overwrites the
        // first, and the byte after the page is untouched (X24C01, "Page Write").
        let mut master = Master::new(chip_on_sega(Chip::X24C01));
        master.write(0, &[0xA1, 0xA2, 0xA3, 0xA4, 0xA5]);
        assert_eq!(
            &master.chip.contents()[0..5],
            &[0xA5, 0xA2, 0xA3, 0xA4, 0xFF],
            "one byte, wrapped and overwritten, and nothing past the page"
        );

        // Eight bytes is a page on the Atmel parts, and nine is one over.
        let mut master = Master::new(chip_on_sega(Chip::AT24C02));
        let page: Vec<u8> = (0xA0..0xA8).collect();
        master.write(0, &page);
        master.write(0, &[0xAF]);
        assert_eq!(
            &master.chip.contents()[0..9],
            &[0xAF, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xFF]
        );

        // The 24C65's page is sixty-four bytes (Genesis Plus GX's i2c_specs
        // give it pagewrite_mask $3F): sixty-five bytes behind one word
        // address fill the page and wrap its last one back onto the first.
        let mut master = Master::new(chip_on_sega(Chip::AT24C65));
        let page: Vec<u8> = (0..65).map(|i| 0xB0 + i as u8).collect();
        master.write(0, &page);
        assert_eq!(
            master.chip.contents()[0],
            0xF0,
            "the 65th byte wrapped onto the first"
        );
        assert_eq!(
            master.chip.contents()[63],
            0xEF,
            "the 64th is the page's last"
        );
        assert_eq!(
            master.chip.contents()[64],
            0xFF,
            "and the next page is untouched"
        );
    }

    #[test]
    fn sequential_read() {
        // A read the master keeps acknowledging walks the array: the chip counts
        // the address up by itself and answers each clock with the next byte.
        let mut master = Master::new(chip_on_sega(Chip::X24C01));
        master.write(0, &[0x11, 0x22, 0x33]);
        assert_eq!(master.read(0, 3), vec![0x11, 0x22, 0x33]);
        assert_eq!(master.read(1, 2), vec![0x22, 0x33]);

        // And it rolls over at the end of the array rather than at a page.
        let array: Vec<u8> = (0..128).map(|word| word as u8).collect();
        master.chip.load(&array);
        assert_eq!(master.read(126, 4), vec![126, 127, 0, 1]);

        // A reading ended with a nothing is ended: the chip has let go of the
        // line, so a clock more asks for a bit of nothing rather than for the
        // byte that would have followed. `line` is the master's own level, and
        // the byte that would have come next is 0x33 with its top bit clear.
        master.start();
        assert!(master.send(0xfc | 1), "word 126 of the array, reading");
        assert_eq!(master.recv(true), 126);
        assert_eq!(master.recv(false), 127);
        assert!(master.clock(true), "and the line is the master's again");
        master.stop();
    }

    #[test]
    fn board_bit_positions() {
        // Every board in the table, walked by the same master: what the test
        // proves is that the lines are where the board says they are, because
        // the chip only hears what the board's bits carry.
        for board in [
            Board::SEGA,
            Board::EA,
            Board::ACCLAIM_16M,
            Board::ACCLAIM_32M,
        ] {
            let mut master = Master::new(EepromInfo {
                base: DEFAULT_BASE,
                board,
                chip: Chip::X24C01,
            });
            // The 32 Mbit board's read-back rides on the window latch, and the
            // games arm it at the top of every EEPROM operation (the word
            // write #0 of the driver wrapper): walk the board the way a game
            // does, latch first.
            if board == Board::ACCLAIM_32M {
                assert!(master.chip.write_word(DEFAULT_BASE, 0x0000));
            }
            master.write(3, &[0x69]);
            assert_eq!(master.chip.contents()[3], 0x69, "{board:?} carried a write");
            assert_eq!(master.read(3, 1), vec![0x69], "{board:?} read back");
        }
    }

    /// The 32 Mbit board's word write is the board's read-window latch, not a
    /// line write: bit 0 of the word selects what the quadrant reads show, the
    /// ROM image (reset, and bit 0 = 1) or the chip's data line (bit 0 = 0),
    /// and a byte write moves a line without touching the latch (Genesis Plus
    /// GX's `mapper_acclaim_32M_write16`, and the games' own wrappers: NBA Jam
    /// TE arms with `move.w #0,(A0)` at $000ADA and restores with
    /// `move.w #1,(A0)` at $000AB2).
    #[test]
    fn acclaim_32m_word_write_latches_the_read_window() {
        let mut chip = Eeprom::new(EepromInfo {
            base: DEFAULT_BASE,
            board: Board::ACCLAIM_32M,
            chip: Chip::AT24C02,
        });
        // Reset is the ROM window: every read in the quadrant, the pair
        // included, is the image's own byte.
        assert_eq!(chip.read(0x20_0000, 0x40), None);
        assert_eq!(chip.read(0x20_0001, 0x40), None);
        assert_eq!(chip.read(0x20_0004, 0x80), None);

        // A word write with bit 0 clear arms the serial window: the chip's
        // line, idle-high until a byte write moves it, is bit 0 of every byte
        // in the quadrant now.
        assert!(chip.write_word(0x20_0000, 0x0000));
        assert_eq!(chip.read(0x20_0004, 0x80), Some(0x81));
        assert_eq!(chip.read(0x20_0000, 0x40), Some(0x41));
        assert_eq!(chip.read(0x2F_FFFF, 0xFE), Some(0xFF));

        // A byte write is still a line write, and it does not move the latch:
        // driving /SDA low leaves the window armed with the line low.
        assert!(chip.write(0x20_0001, 0x00), "the board's own line");
        assert_eq!(chip.read(0x20_0004, 0x80), Some(0x80));

        // A word write with bit 0 set restores the ROM window, whatever the
        // lines are doing.
        assert!(chip.write_word(0x20_0000, 0x0001));
        assert_eq!(chip.read(0x20_0004, 0x80), None);
        assert_eq!(chip.read(0x20_0001, 0x40), None);

        // And a second latch write arms it again, which is what a game's next
        // EEPROM operation does. The line is where the last byte write left
        // it - nothing restores /SDA but the game.
        assert!(chip.write_word(0x20_0000, 0x0000));
        assert_eq!(chip.read(0x20_0004, 0x80), Some(0x80));
    }

    /// The latch is the board's, not the pair's: it answers across the whole
    /// quadrant its PAL decodes - the only address inputs on the 670125/670127
    /// board's custom chip are A20 and A21 (8BitWizard's teardown, spritesmind
    /// t=107) - and the reset input of that chip is what puts the ROM window
    /// back, which is what [`Eeprom::reset`] does.
    #[test]
    fn acclaim_32m_window_is_the_quadrant_and_reset_restores_rom() {
        let mut chip = Eeprom::new(EepromInfo {
            base: DEFAULT_BASE,
            board: Board::ACCLAIM_32M,
            chip: Chip::AT24C02,
        });
        assert!(
            chip.write_word(0x2F_FFFE, 0x0000),
            "the latch is quadrant-wide"
        );
        assert_eq!(chip.read(0x20_0000, 0x00), Some(0x01));
        assert_eq!(chip.read(0x2F_FFFF, 0xFE), Some(0xFF));
        // And outside the quadrant nothing answers, armed or not: the byte
        // below is plain ROM, the bytes above are nothing the chip drives.
        assert_eq!(chip.read(0x1F_FFFF, 0x00), None);
        assert_eq!(chip.read(0x30_0000, 0x00), None);
        assert_eq!(chip.read(0x3F_FFFF, 0x00), None);
        assert!(
            !chip.write_word(0x30_0000, 0x0000),
            "the write window ends at $2FFFFF"
        );

        // Reset: /RES is an input of the PAL, and the window it arms comes up
        // showing ROM.
        chip.reset();
        assert_eq!(chip.read(0x20_0000, 0x00), None);
        assert_eq!(chip.read(0x2F_FFFF, 0xFE), None);
    }

    /// A word write drives no line on this board: with both /LWR and /UWR
    /// asserted the PAL latches, and the two lines stay where the byte writes
    /// left them. The word $0002 is the pin - its bit 0 says "stay armed", and
    /// its two byte halves, driven as a word write is on a board without the
    /// latch, would have put both lines low.
    #[test]
    fn acclaim_32m_word_write_drives_no_line() {
        let mut chip = Eeprom::new(EepromInfo {
            base: DEFAULT_BASE,
            board: Board::ACCLAIM_32M,
            chip: Chip::AT24C02,
        });
        assert!(chip.write_word(0x20_0000, 0x0000));
        assert_eq!(
            chip.read(0x20_0001, 0x00),
            Some(0x01),
            "the lines idle high"
        );
        assert!(chip.write_word(0x20_0000, 0x0002));
        assert_eq!(
            chip.read(0x20_0001, 0x00),
            Some(0x01),
            "still armed, and the lines did not move"
        );
    }

    /// No other board has the latch: a word write is nothing to them, and
    /// their pair answers as it always has.
    #[test]
    fn other_boards_have_no_window_latch() {
        let mut chip = Eeprom::new(chip_on_sega(Chip::X24C01));
        assert!(!chip.write_word(0x20_0000, 0x0000));
        assert_eq!(
            chip.read(0x20_0004, 0x80),
            None,
            "the quadrant is not a window"
        );
    }

    /// The window a cart declares is where the two lines are, and a cart that
    /// declares one address has declared a pair rather than a length of one:
    /// `start == end` is 0x200001, whose pair is 0x200000 and 0x200001.
    #[test]
    fn single_address_window_is_a_pair() {
        assert_eq!(
            EepromInfo::declared(&extra_memory(0xE8, 0x20_0001, 0x20_0001)),
            Some(0x20_0000)
        );
        // The two-address window of the Acclaim 32 Mbit carts is the same pair.
        assert_eq!(
            EepromInfo::declared(&extra_memory(0xE8, 0x20_0000, 0x20_0001)),
            Some(0x20_0000)
        );
        // And the array is the size of the chip whatever the window says.
        let chip = Eeprom::new(EepromInfo {
            base: 0x20_0000,
            board: Board::SEGA,
            chip: Chip::X24C01,
        });
        assert_eq!(chip.contents().len(), 128);
        // Nothing to declare: no `RA`, a save RAM type, and a window the bus
        // cannot reach.
        assert_eq!(EepromInfo::declared(&[0u8; 12]), None);
        assert_eq!(
            EepromInfo::declared(&extra_memory(0xF8, 0x20_0001, 0x20_FFFF)),
            None
        );
        assert_eq!(
            EepromInfo::declared(&extra_memory(0xE8, 0x20_0001, 0x20_0000)),
            None,
            "backwards"
        );
        assert_eq!(
            EepromInfo::declared(&extra_memory(0xE8, 0x40_0001, 0x7E_0000)),
            None,
            "outside the cartridge area"
        );
    }

    /// The four things `detect` can be asked about a cart: an EEPROM in the
    /// header and nothing else, save RAM in the header, both in the header and
    /// the list, and neither.
    #[test]
    fn known_game_serial_enables_eeprom() {
        // A cart that only says `$E8` gets the Sega board and the X24C01, which
        // is what every Sega-board cart in the list has.
        let declaring = rom(
            "GM T-123456-00",
            &extra_memory(0xE8, 0x20_0001, 0x20_0001),
            0x1_0000,
        );
        assert_eq!(
            detect(&declaring),
            Some(EepromInfo {
                base: 0x20_0000,
                board: Board::SEGA,
                chip: Chip::X24C01,
            })
        );

        // NBA Jam is on Acclaim's 16 Mbit board and says `$E8` too, and the list
        // is what knows the difference.
        let nba_jam = rom(
            "GM T-081326 00",
            &extra_memory(0xE8, 0x20_0001, 0x20_0001),
            0x1_0000,
        );
        assert_eq!(
            detect(&nba_jam).map(|info| (info.base, info.board, info.chip)),
            Some((0x20_0000, Board::ACCLAIM_16M, Chip::X24C02))
        );

        // The EA carts and NBA Jam TE declare nothing at all, and the serial is
        // the only thing that says they have a chip.
        let ea = rom(
            "GM T-50176 -00",
            &[
                b' ', b' ', 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            ],
            0x1_0000,
        );
        assert_eq!(
            detect(&ea).map(|info| (info.base, info.board, info.chip)),
            Some((0x20_0000, Board::EA, Chip::X24C01))
        );

        // The two 24C65 carts declare $E8 over the pair *and* are in the list,
        // which is what tells their Acclaim 32 Mbit board and their mode 3
        // chip from the default a bare $E8 would get.
        for (serial, chip) in [
            ("GM T-81476 -00", Chip::AT24C65), // Frank Thomas Big Hurt Baseball
            ("GM T-81576 -00", Chip::AT24C65), // College Slam
        ] {
            let cart = rom(serial, &extra_memory(0xE8, 0x20_0000, 0x20_0001), 0x1_0000);
            assert_eq!(
                detect(&cart).map(|info| (info.base, info.board, info.chip)),
                Some((0x20_0000, Board::ACCLAIM_32M, chip)),
                "{serial}"
            );
        }
    }

    /// A cart has one save, which is why the two detectors can never both answer
    /// for it: the carts that shipped with a save RAM window and a serial EEPROM
    /// are carts whose EEPROM had been replaced by a save RAM hack.
    #[test]
    fn eeprom_and_sram_never_attach_at_once() {
        for (serial, raw) in [
            ("GM T-12046 -00", extra_memory(0xE8, 0x20_0001, 0x20_0001)),
            ("GM T-12046 -00", extra_memory(0xF8, 0x20_0001, 0x20_FFFF)),
            ("GM T-999999 -00", extra_memory(0xF8, 0x20_0001, 0x20_FFFF)),
            ("GM T-999999 -00", extra_memory(0xE8, 0x20_0001, 0x20_0001)),
        ] {
            let cart = rom(serial, &raw, 0x1_0000);
            assert_ne!(
                detect(&cart).is_some(),
                super::super::sram::detect(&cart).is_some(),
                "{serial} {raw:02X?} declares one save, not two or none",
            );
        }
    }

    /// [`super::sram::Sram::load`] refuses a save of another cart's size, and so
    /// does the chip: the shape comes from the chip and not from the window.
    #[test]
    fn load_is_all_or_nothing_and_dirty_is_an_edge() {
        let mut chip = Eeprom::new(chip_on_sega(Chip::AT24C01));
        chip.load(&[0x11; 127]);
        assert!(chip.contents().iter().all(|&b| b == 0xFF), "too short");
        chip.load(&[0x11; 128]);
        assert!(chip.contents().iter().all(|&b| b == 0x11));
        assert!(!chip.take_dirty(), "restoring is not a change");

        let mut master = Master::new(chip_on_sega(Chip::AT24C01));
        assert!(!master.chip.take_dirty());
        master.write(0, &[0x01]);
        assert!(master.chip.take_dirty());
        assert!(!master.chip.take_dirty(), "and once only");
    }
}
