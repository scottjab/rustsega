//! The YM2612 register file: two banks of 256 registers, the address each part
//! last latched, and the decodes of the global registers that span more than one
//! byte.
//!
//! Bank 0 is part I, the half that carries slots 1 to 3 and all the global
//! registers, and bank 1 is part II, the half that carries slots 4 to 6. Which
//! half a write belongs to is one of the two address lines of the chip - port 0
//! and 1 are part I, port 2 and 3 are part II - and the Genesis maps them to
//! `$A04000`-`$A04003` for the 68k and `$4000`-`$4003` for the Z80 (YM2608
//! application manual "3. Register descriptions"; plutiedev, "YM2612 registers").
//!
//! The banks are raw bytes and stay that way: the chip's registers are
//! write-only, so there is no read decode to write, and a register that nothing
//! acts on yet is still stored, because the register file *is* the observable
//! state of a write-only chip. Of the global registers only the timer group of
//! [`RegFile::timer_config`] means anything at this stage; the LFO (`0x22`), the
//! key-on (`0x28`) and the DAC enable and value (`0x2A`, `0x2B`) are stored for
//! the sound tickets M5-04 (#108) and M5-11 (#116).

use crate::timers::TimerConfig;

/// Bank index of part I: slots 1 to 3 and the global registers.
pub(crate) const PART_I: usize = 0;

/// Bank index of part II: slots 4 to 6.
pub(crate) const PART_II: usize = 1;

/// One bank per part (YM2608 application manual "3. Register descriptions").
const PARTS: usize = 2;

/// Registers per bank: the address port is 8 bits wide, so a part has 256 of
/// them whether or not every number in the range is decoded.
const REGISTERS: usize = 256;

/// Register `&24`, timer A loader bits A0-A7: the low eight of its 10.
///
/// The ticket text of #107 had the two halves the other way round, "0x24: timer
/// A high 8 bits, 0x25 bits 1-0: timer A low 2 bits". The YM2608 application
/// manual's register descriptions, which #107 names as its reference, are
/// explicit that `&24` takes A0-A7 and `&25` bits 1-0 take A8-A9, and software
/// relies on it: a driver that wants a period of 1000 writes `$E8` then `$03`,
/// and reading those back as `0x03` in the high byte and `0xE8` in the low two
/// bits of a 2-bit field asks for 931 instead. Hardware order it is, and
/// `timer_a_low_half_is_register_0x24` in `lib.rs` pins it.
pub(crate) const REG_TIMER_A_LOW: u8 = 0x24;

/// Register `&25`, timer A loader bits A8-A9 in its bits 1-0: the high two of
/// the 10, the complement of [`REG_TIMER_A_LOW`].
pub(crate) const REG_TIMER_A_HIGH: u8 = 0x25;

/// Register `&26`, timer B loader, all 8 bits.
pub(crate) const REG_TIMER_B: u8 = 0x26;

/// Register `&27`: bits 7-6 the channel 3 mode, bit 5 reset timer B flag, bit 4
/// reset timer A flag, bit 3 timer B overflow enable, bit 2 timer A overflow
/// enable, bit 1 load (start) timer B, bit 0 load (start) timer A (YM2608
/// application manual "3. Register descriptions", `&27`).
///
/// The channel 3 mode is for the ADPCM/special modes of M5-08 and M5-09 and the
/// key-on of `&28` for M5-04 (#108); this ticket decodes the four timer bits.
pub(crate) const REG_CONTROL: u8 = 0x27;

/// `&27` bit 0: 0 stops timer A, 1 starts it.
pub(crate) const LOAD_A_BIT: u8 = 0x01;
/// `&27` bit 1: 0 stops timer B, 1 starts it.
pub(crate) const LOAD_B_BIT: u8 = 0x02;
/// `&27` bit 2: an overflow of timer A sets status bit 0.
pub(crate) const ENABLE_A_BIT: u8 = 0x04;
/// `&27` bit 3: an overflow of timer B sets status bit 1.
pub(crate) const ENABLE_B_BIT: u8 = 0x08;
/// `&27` bit 4: a write with this bit set clears status bit 0.
pub(crate) const RESET_A_BIT: u8 = 0x10;
/// `&27` bit 5: a write with this bit set clears status bit 1.
pub(crate) const RESET_B_BIT: u8 = 0x20;

/// The register file of one YM2612: two banks and the address each of the two
/// address ports last latched.
#[derive(Clone, Debug)]
pub(crate) struct RegFile {
    /// `banks[PART_I]` and `banks[PART_II]`, indexed by register number. Zeroed,
    /// which is not what a real chip powers up with - a YM2612 wakes with
    /// undefined registers - but is the only defined thing this model has.
    banks: [[u8; REGISTERS]; PARTS],
    /// One latch per part: `&27`-style register numbers written to one part's
    /// address port do not move the other part's.
    latched: [u8; PARTS],
}

impl RegFile {
    /// An all-zero register file with both parts latched on register `0x00`.
    pub(crate) fn new() -> Self {
        Self {
            banks: [[0; REGISTERS]; PARTS],
            latched: [0; PARTS],
        }
    }

    /// A write to a part's address port: latch the register number, write
    /// nothing else (YM2608 application manual "3. Register descriptions": the
    /// address is set up first, then the data port carries the value).
    pub(crate) fn latch(&mut self, part: usize, address: u8) {
        self.latched[part] = address;
    }

    /// The register number `part`'s data port writes to right now.
    pub(crate) fn address(&self, part: usize) -> u8 {
        self.latched[part]
    }

    /// A write to a part's data port: store `value` at that part's latched
    /// register.
    pub(crate) fn write(&mut self, part: usize, register: u8, value: u8) {
        self.banks[part][usize::from(register)] = value;
    }

    /// The stored byte of `register` of `part`. The chip itself cannot read its
    /// registers back; this is the emulator's view of what a write did.
    pub(crate) fn read(&self, part: usize, register: u8) -> u8 {
        self.banks[part][usize::from(register)]
    }

    /// Whether `register` is one of the four that [`crate::timers::Timers`] cares
    /// about, `&24` to `&27`.
    pub(crate) fn is_timer_register(register: u8) -> bool {
        (REG_TIMER_A_LOW..=REG_CONTROL).contains(&register)
    }

    /// Timer A as its 10 bits go: `&24` is A0-A7 and `&25` bits 1-0 are A8-A9
    /// (YM2608 application manual "3. Register descriptions", `&24` and `&25`).
    pub(crate) fn timer_a(&self) -> u16 {
        let high = u16::from(self.read(PART_I, REG_TIMER_A_HIGH) & 0x03) << 8;
        let low = u16::from(self.read(PART_I, REG_TIMER_A_LOW));
        high | low
    }

    /// The timer group decoded for [`crate::timers::Timers`]: the two loader
    /// values and the four timer bits of `&27`. Part I only, since the timers
    /// live there.
    pub(crate) fn timer_config(&self) -> TimerConfig {
        let control = self.read(PART_I, REG_CONTROL);
        TimerConfig {
            a_value: self.timer_a(),
            b_value: self.read(PART_I, REG_TIMER_B),
            a_enable: control & ENABLE_A_BIT != 0,
            b_enable: control & ENABLE_B_BIT != 0,
            a_load: control & LOAD_A_BIT != 0,
            b_load: control & LOAD_B_BIT != 0,
        }
    }
}
