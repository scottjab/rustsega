//! Yamaha YM2612 FM synthesizer.
//!
//! What lives here today is the bus interface, the register file and the two
//! timers of M5-03 (#107): the four ports the 68k and the Z80 reach the chip
//! through (`write`), the two 256-register banks behind them and the latched
//! address each one writes to (`regs`), the status byte a read of any port
//! answers with (`read_status`) with its BUSY flag and its two timer overflow
//! flags, and timer A and timer B (`timers`). `clock` is the sample step those
//! timers count in and still returns silence: the operators, envelope generator
//! and slot output of M5-04 (#108) onwards are not here yet.
//!
//! Register bit documentation cites the Yamaha YM2608 (OPNA) application manual
//! by register number, which is the manual that documents the OPN register set
//! the Genesis part shares, and plutiedev's "YM2612 registers" page where that
//! one is terse.

mod regs;
mod timers;

use regs::{PART_I, PART_II};

/// Port 0: address, part I.
///
/// The four ports are the chip's two address lines - A0 selects data over
/// address and A1 selects part II - which the Genesis maps to the four bytes
/// `$A04000`-`$A04003` for the 68k and `$4000`-`$4003` for the Z80 (YM2608
/// application manual "3. Register descriptions"; plutiedev, "YM2612
/// registers").
const PORT_ADDRESS_1: u8 = 0;
/// Port 1: data, part I.
const PORT_DATA_1: u8 = 1;
/// Port 2: address, part II.
const PORT_ADDRESS_2: u8 = 2;
/// Port 3: data, part II.
const PORT_DATA_2: u8 = 3;

/// Status bit 7: the chip is still busy with the last write.
const STATUS_BUSY: u8 = 0x80;
/// Status bit 1: timer B overflowed with its enable bit set.
const STATUS_TIMER_B: u8 = 0x02;
/// Status bit 0: timer A overflowed with its enable bit set.
const STATUS_TIMER_A: u8 = 0x01;

/// Internal YM2612 cycles in one output sample.
///
/// [`Ym2612::clock`] is called once per sample, every 144 68k clocks, and the
/// chip's own FM step is a sixth of that: 24 internal cycles per sample
/// (docs/ARCHITECTURE.md, "Clocks and timing").
const INTERNAL_CYCLES_PER_SAMPLE: u32 = 24;

/// How long a write to a register holds [`STATUS_BUSY`], in internal cycles:
/// 32 of them, which is 1.33 samples, so BUSY is still set one sample after the
/// write and clear two samples after it.
///
/// INFERRED: the figure is the one #107 gives. It has not been checked against
/// Nemesis's busy-flag measurements, and [#325](https://github.com/scottjab/rustsega/issues/325)
/// is where that measurement is being collected.
const BUSY_INTERNAL_CYCLES: u32 = 32;

/// The YM2612: its four bus ports, its two register banks, its status byte and
/// its two timers.
pub struct Ym2612 {
    /// The two banks of registers and the address each part last latched.
    regs: regs::RegFile,
    /// Timer A and timer B, driven by the writes to `&24`-`&27` and read back
    /// through the two low bits of the status byte.
    timers: timers::Timers,
    /// Internal cycles left of the BUSY flag: any positive value reads as busy,
    /// and a write refills it with [`BUSY_INTERNAL_CYCLES`].
    busy: u32,
}

impl Ym2612 {
    /// A chip with zeroed registers, both parts latched on register `0x00`, both
    /// timers stopped and no flag set.
    pub fn new() -> Self {
        Self {
            regs: regs::RegFile::new(),
            timers: timers::Timers::new(),
            busy: 0,
        }
    }

    /// Write one byte to one of the four ports: [`PORT_ADDRESS_1`] and
    /// [`PORT_ADDRESS_2`] latch a register number for their part, and
    /// [`PORT_DATA_1`] and [`PORT_DATA_2`] write a value to the number their
    /// part latched (YM2608 application manual "3. Register descriptions").
    ///
    /// Only the data write is a write to the chip, so only the data write sets
    /// BUSY. Whether an address-port write sets it too is not modelled here, and
    /// nothing can see the difference from the 68k side without a bus cycle
    /// count.
    pub fn write(&mut self, port: u8, value: u8) {
        match port {
            PORT_ADDRESS_1 => self.regs.latch(PART_I, value),
            PORT_ADDRESS_2 => self.regs.latch(PART_II, value),
            PORT_DATA_1 => self.write_data(PART_I, value),
            PORT_DATA_2 => self.write_data(PART_II, value),
            // A1/A0 are the only two lines the ports decode: there is nothing
            // else on the chip to reach.
            _ => {}
        }
    }

    /// A data port write to `part`: store the byte at that part's latched
    /// register, then tell the timers if that register was one of theirs.
    fn write_data(&mut self, part: usize, value: u8) {
        let register = self.regs.address(part);
        self.regs.write(part, register, value);

        if part == PART_I && regs::RegFile::is_timer_register(register) {
            let config = self.regs.timer_config();
            self.timers.register_written(register, value, &config);
        }

        self.busy = BUSY_INTERNAL_CYCLES;
    }

    /// The status byte, which is what a read of any of the four ports answers:
    /// bit 7 BUSY, bit 1 the timer B overflow flag, bit 0 the timer A overflow
    /// flag (YM2608 application manual "3. Register descriptions", and plutiedev,
    /// "YM2612 status" - which notes that only port 0 is guaranteed to return it
    /// on a discrete chip, while every port does on the integrated part and for
    /// every game that reads it).
    ///
    /// Reading does not clear anything: only a `&27` write with its reset bits
    /// set clears the two timer flags, and BUSY comes down by itself.
    pub fn read_status(&self) -> u8 {
        let busy = if self.busy > 0 { STATUS_BUSY } else { 0 };
        let timer_b = if self.timers.flag_b() {
            STATUS_TIMER_B
        } else {
            0
        };
        let timer_a = if self.timers.flag_a() {
            STATUS_TIMER_A
        } else {
            0
        };
        busy | timer_b | timer_a
    }

    /// Advance the chip by one output sample - 144 68k clocks, about 53267 Hz
    /// (docs/ARCHITECTURE.md, "Clocks and timing") - and return the `(left,
    /// right)` sample.
    ///
    /// Both are 0 for now: the FM sum of M5-04 (#108) and on goes left and the
    /// DAC of M5-11 (#116) goes right. What the step does today is run the two
    /// timers and let BUSY run down.
    pub fn clock(&mut self) -> (i16, i16) {
        let config = self.regs.timer_config();
        self.timers.tick(&config);
        self.busy = self.busy.saturating_sub(INTERNAL_CYCLES_PER_SAMPLE);
        (0, 0)
    }
}

impl Default for Ym2612 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four ports of the chip, numbered by their `A1 A0` lines: A0 = 1 is a
    /// data write and A1 = 1 is part II, the half that carries slots 4 to 6
    /// (YM2608 application manual "3. Register descriptions"; plutiedev,
    /// "YM2612 registers").
    const ADDR_1: u8 = 0;
    const DATA_1: u8 = 1;
    const ADDR_2: u8 = 2;
    const DATA_2: u8 = 3;

    /// The global registers, which live in part I only.
    const REG_TIMER_A_LOW: u8 = 0x24;
    const REG_TIMER_A_HIGH: u8 = 0x25;
    const REG_TIMER_B: u8 = 0x26;
    const REG_CONTROL: u8 = 0x27;

    /// Register 0x27 bit 0: start timer A.
    const LOAD_A: u8 = 0x01;
    /// Register 0x27 bit 1: start timer B.
    const LOAD_B: u8 = 0x02;
    /// Register 0x27 bit 2: set the status flag on a timer A overflow.
    const ENABLE_A: u8 = 0x04;
    /// Register 0x27 bit 3: set the status flag on a timer B overflow.
    const ENABLE_B: u8 = 0x08;
    /// Register 0x27 bit 4: reset the timer A overflow flag.
    const RESET_A: u8 = 0x10;
    /// Register 0x27 bit 5: reset the timer B overflow flag.
    const RESET_B: u8 = 0x20;

    /// Status bit 7: the chip is still chewing on the last write.
    const STATUS_BUSY: u8 = 0x80;
    /// Status bit 1: timer B overflowed while its enable bit was set.
    const STATUS_TIMER_B: u8 = 0x02;
    /// Status bit 0: timer A overflowed while its enable bit was set.
    const STATUS_TIMER_A: u8 = 0x01;

    /// Whether `bit` is set in `status`.
    fn bit(status: u8, bit: u8) -> bool {
        status & bit != 0
    }

    /// Write `value` to register `reg` of the part addressed by `addr`/`data`:
    /// the address port latches first, the data port writes to what it latched.
    fn write_reg(chip: &mut Ym2612, addr: u8, data: u8, reg: u8, value: u8) {
        chip.write(addr, reg);
        chip.write(data, value);
    }

    /// Load the 10-bit timer A value: `&24` takes A0-A7 and `&25` bits 1-0 take
    /// A8-A9 (YM2608 application manual, `&24` and `&25`).
    fn set_timer_a(chip: &mut Ym2612, value: u16) {
        chip.write(ADDR_1, REG_TIMER_A_LOW);
        chip.write(DATA_1, value as u8);
        chip.write(ADDR_1, REG_TIMER_A_HIGH);
        chip.write(DATA_1, (value >> 8) as u8);
    }

    /// Load the 8-bit timer B value (`&26`).
    fn set_timer_b(chip: &mut Ym2612, value: u8) {
        write_reg(chip, ADDR_1, DATA_1, REG_TIMER_B, value);
    }

    /// Advance `chip` by `samples` output samples.
    fn clock_for(chip: &mut Ym2612, samples: u32) {
        for _ in 0..samples {
            chip.clock();
        }
    }

    #[test]
    fn address_then_data_stores_register() {
        let mut chip = Ym2612::new();

        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x91);
        assert_eq!(chip.regs.read(PART_I, 0x30), 0x91);

        // The address port only latches: it stores nothing of its own, and the
        // data port writes whatever the latch is holding now.
        chip.write(ADDR_1, 0x31);
        assert_eq!(chip.regs.read(PART_I, 0x31), 0);
        chip.write(DATA_1, 0x1F);
        assert_eq!(chip.regs.read(PART_I, 0x31), 0x1F);
        assert_eq!(
            chip.regs.read(PART_I, 0x30),
            0x91,
            "latching 0x31 leaves the register written before it alone"
        );
    }

    #[test]
    fn part2_separate_storage() {
        let mut chip = Ym2612::new();

        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x11);
        write_reg(&mut chip, ADDR_2, DATA_2, 0x30, 0x22);

        assert_eq!(chip.regs.read(PART_I, 0x30), 0x11);
        assert_eq!(chip.regs.read(PART_II, 0x30), 0x22);

        // Each part latches on its own: part II's data port writes part II's
        // address, not the address part I last latched.
        chip.write(ADDR_2, 0x31);
        chip.write(DATA_2, 0x33);
        assert_eq!(chip.regs.read(PART_II, 0x31), 0x33);
        assert_eq!(chip.regs.read(PART_I, 0x31), 0);
    }

    #[test]
    fn busy_set_after_write_and_clears() {
        let mut chip = Ym2612::new();
        assert!(!bit(chip.read_status(), STATUS_BUSY));

        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x01);
        assert!(
            bit(chip.read_status(), STATUS_BUSY),
            "a data write sets BUSY"
        );

        // BUSY is held for 32 internal cycles and a sample is 24 of them, so it
        // survives one sample and is gone by the next.
        chip.clock();
        assert!(bit(chip.read_status(), STATUS_BUSY), "24 cycles < 32");
        chip.clock();
        assert!(!bit(chip.read_status(), STATUS_BUSY), "48 cycles > 32");
    }

    #[test]
    fn timer_a_overflow_after_1024_minus_ta_samples() {
        let mut chip = Ym2612::new();
        set_timer_a(&mut chip, 1000);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, LOAD_A | ENABLE_A);

        clock_for(&mut chip, 23);
        assert!(
            !bit(chip.read_status(), STATUS_TIMER_A),
            "23 of the 24 ticks timer A needs to reach 1024"
        );
        chip.clock();
        assert!(bit(chip.read_status(), STATUS_TIMER_A));
    }

    #[test]
    fn timer_b_overflow_after_16x() {
        let mut chip = Ym2612::new();
        set_timer_b(&mut chip, 250);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, LOAD_B | ENABLE_B);

        // Timer B is 1/16 the rate of timer A, so 6 ticks to 256 is 6 * 16.
        clock_for(&mut chip, 95);
        assert!(
            !bit(chip.read_status(), STATUS_TIMER_B),
            "95 samples is 5 ticks and a bit"
        );
        chip.clock();
        assert!(bit(chip.read_status(), STATUS_TIMER_B));
    }

    #[test]
    fn timer_flag_needs_enable_bit() {
        let mut chip = Ym2612::new();
        set_timer_a(&mut chip, 1000);
        set_timer_b(&mut chip, 250);
        // Both timers load and run, neither is enabled to raise a flag.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, LOAD_A | LOAD_B);

        clock_for(&mut chip, 100);
        let status = chip.read_status();
        assert!(!bit(status, STATUS_TIMER_A), "timer A ran, 4 times over");
        assert!(!bit(status, STATUS_TIMER_B), "timer B ran, 1 time over");
    }

    #[test]
    fn reset_flag_bits_clear_status() {
        let mut chip = Ym2612::new();
        set_timer_a(&mut chip, 1000);
        set_timer_b(&mut chip, 250);
        write_reg(
            &mut chip,
            ADDR_1,
            DATA_1,
            REG_CONTROL,
            LOAD_A | LOAD_B | ENABLE_A | ENABLE_B,
        );
        clock_for(&mut chip, 96);
        let status = chip.read_status();
        assert!(bit(status, STATUS_TIMER_A));
        assert!(bit(status, STATUS_TIMER_B));

        // 0x27 without its reset bits keeps both flags.
        write_reg(
            &mut chip,
            ADDR_1,
            DATA_1,
            REG_CONTROL,
            LOAD_A | LOAD_B | ENABLE_A | ENABLE_B,
        );
        assert!(bit(chip.read_status(), STATUS_TIMER_A));
        assert!(bit(chip.read_status(), STATUS_TIMER_B));

        // Bit 5 resets B alone, bit 4 resets A alone, and a write of both clears
        // both.
        write_reg(
            &mut chip,
            ADDR_1,
            DATA_1,
            REG_CONTROL,
            LOAD_A | LOAD_B | ENABLE_A | ENABLE_B | RESET_B,
        );
        assert!(bit(chip.read_status(), STATUS_TIMER_A), "A is not B");
        assert!(!bit(chip.read_status(), STATUS_TIMER_B));

        write_reg(
            &mut chip,
            ADDR_1,
            DATA_1,
            REG_CONTROL,
            LOAD_A | LOAD_B | ENABLE_A | ENABLE_B | RESET_A,
        );
        assert!(!bit(chip.read_status(), STATUS_TIMER_A));
    }

    #[test]
    fn timer_a_low_half_is_register_0x24() {
        // The manual's order, `&24` A0-A7 and `&25` bits 1-0 A8-A9, written byte
        // by byte rather than through `set_timer_a`: `$E8` then `$03` is 1000,
        // where the two halves the other way round is 3, which would take 1021
        // samples to overflow instead of 24 (YM2608 application manual, `&24`
        // and `&25`). Nothing else here would notice a swap, because every other
        // timer test only pushes a value through `set_timer_a`.
        let mut chip = Ym2612::new();
        write_reg(&mut chip, ADDR_1, DATA_1, REG_TIMER_A_LOW, 0xE8);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_TIMER_A_HIGH, 0x03);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, LOAD_A | ENABLE_A);

        assert_eq!(chip.regs.timer_a(), 1000);
        clock_for(&mut chip, 23);
        assert!(!bit(chip.read_status(), STATUS_TIMER_A));
        chip.clock();
        assert!(bit(chip.read_status(), STATUS_TIMER_A));
    }
}
