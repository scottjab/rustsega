//! Yamaha YM2612 FM synthesizer.
//!
//! What lives here today is the bus interface, the register file and the two
//! timers of M5-03 (#107), the phase generator of M5-04 (#108) and the envelope
//! generator of M5-05 (#109): the four ports the 68k and the Z80 reach the chip
//! through (`write`), the two 256-register banks behind them and the latched
//! address each one writes to (`regs`), the status byte a read of any port
//! answers with (`read_status`) with its BUSY flag and its two timer overflow
//! flags, timer A and timer B (`timers`), the F-Num, block, detune, multiple and
//! key-on behind the 24 phase counters (`phase`), and the attack, decay, sustain
//! and release behind the 24 attenuations (`envelope`). `clock` is the sample
//! step those timers, counters and envelopes run in, and the operators answer in
//! it - the quarter-wave log sine, the exponent and the sign of one operator's
//! output (`operator` and `tables`), wired through the eight algorithms and the
//! feedback of `&B0` and summed into the six channel mixes of M5-07a (#111)
//! (`channel`). The mix still does not reach the pins: the DAC output stage of
//! M5-07b (#112) is what turns a channel sum into a sample.
//!
//! Register bit documentation cites the Yamaha YM2608 (OPNA) application manual
//! by register number, which is the manual that documents the OPN register set
//! the Genesis part shares, and plutiedev's "YM2612 registers" page where that
//! one is terse.

mod channel;
mod envelope;
mod operator;
mod phase;
mod regs;
mod tables;
mod timers;

use channel::FmChannel;
use envelope::EnvelopeGen;
use phase::{CHANNELS, PhaseGen};
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

/// The YM2612: its four bus ports, its two register banks, its status byte, its
/// two timers and its phase counters.
pub struct Ym2612 {
    /// The two banks of registers and the address each part last latched.
    regs: regs::RegFile,
    /// Timer A and timer B, driven by the writes to `&24`-`&27` and read back
    /// through the two low bits of the status byte.
    timers: timers::Timers,
    /// The 24 phase counters of the operators, driven by the writes to `&28`,
    /// `&30`-`&3E` and `&A0`-`&A6`.
    phases: PhaseGen,
    /// The 24 envelopes of the operators, driven by the writes to `&28` and
    /// `&40`-`&80` and stepped a third as often as the phase counters.
    envelopes: EnvelopeGen,
    /// The six channels' operator sums: the algorithm and the feedback of the
    /// `&B0` group, and the mix those leave behind (M5-07a, #111).
    channels: [FmChannel; CHANNELS],
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
            phases: PhaseGen::new(),
            envelopes: EnvelopeGen::new(),
            channels: std::array::from_fn(|_| FmChannel::new()),
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
    /// register, then tell the timers and the phase generator if that register
    /// was one of theirs.
    fn write_data(&mut self, part: usize, value: u8) {
        let register = self.regs.address(part);
        self.regs.write(part, register, value);

        if part == PART_I && regs::RegFile::is_timer_register(register) {
            let config = self.regs.timer_config();
            self.timers.register_written(register, value, &config);
        }

        self.phases.register_written(part, register, value);
        self.envelopes
            .register_written(&self.phases, part, register, value);

        if let Some(channel) =
            regs::channel_of_register(part, regs::REG_ALGORITHM_FEEDBACK, register)
        {
            self.channels[channel].register_written(value);
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
    /// Both are 0 for now: the FM sum goes left and the DAC of M5-11 (#116)
    /// goes right, and the output stage of M5-07b (#112) is what turns the six
    /// channel sums into a sample. What the step does today is run the two
    /// timers, advance the 24 phase counters, step the envelopes - once every
    /// three samples - mix the four operators of each channel into the algorithm
    /// `&B0` names, and let BUSY run down.
    ///
    /// The order is the chip's own: an operator is read at the phase the phase
    /// generator has just arrived at and at the attenuation the envelope
    /// generator has just arrived at, so a `&B0`, `&40` or `&A0` write is heard
    /// from the very next sample (`crate::channel`).
    pub fn clock(&mut self) -> (i16, i16) {
        let config = self.regs.timer_config();
        self.timers.tick(&config);
        self.phases.tick();
        self.envelopes.tick(&self.phases);

        for channel in 0..CHANNELS {
            let phase = std::array::from_fn(|slot| self.phases.phase(channel, slot));
            let att = std::array::from_fn(|slot| self.envelopes.envelope(channel, slot).output());
            self.channels[channel].step(phase, att);
        }

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

    /// Register `&28`: bits 7-4 the key-on of operators S4, S3, S2, S1, bits
    /// 2-0 the channel (YM2608 application manual "3. Register descriptions",
    /// `&28`).
    const REG_KEY_ON: u8 = 0x28;

    /// `&28` bit 4: key on operator 1.
    const KEY_ON_S1: u8 = 0x10;
    /// `&28` bit 5: key on operator 2.
    const KEY_ON_S2: u8 = 0x20;
    /// `&28` bit 6: key on operator 3.
    const KEY_ON_S3: u8 = 0x40;

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

    /// What the phase generator holds for one operator: `slot` of `channel`,
    /// with the slot indices of [`phase::SLOT_S1`] and friends.
    fn operator(chip: &Ym2612, channel: usize, slot: usize) -> phase::Operator {
        chip.phases.channel(channel).operators[slot]
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

    #[test]
    fn slot_offset_mapping() {
        let mut chip = Ym2612::new();

        // A per-operator register number is first + slot offset + channel, and
        // the slot offsets of the group run S1, S3, S2, S4: +0x0 is operator 1,
        // +0x4 is operator 3, +0x8 is operator 2 and +0xC is operator 4. So +4
        // has to land on operator 3 and leave operator 2 alone (YM2608
        // application manual "3. Register descriptions").
        write_reg(&mut chip, ADDR_1, DATA_1, 0x34, 0x12);
        let op3 = operator(&chip, 0, phase::SLOT_S3);
        assert_eq!((op3.dt, op3.mul), (1, 2));
        assert_eq!(operator(&chip, 0, phase::SLOT_S2).mul, 0);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).mul, 0);

        write_reg(&mut chip, ADDR_1, DATA_1, 0x38, 0x23);
        let op2 = operator(&chip, 0, phase::SLOT_S2);
        assert_eq!((op2.dt, op2.mul), (2, 3));

        // The channel is the low two bits of the offset, and a part's channels
        // are the global channels 0-2 for part I and 3-5 for part II.
        write_reg(&mut chip, ADDR_1, DATA_1, 0x31, 0x07);
        assert_eq!(operator(&chip, 1, phase::SLOT_S1).mul, 7);
        write_reg(&mut chip, ADDR_2, DATA_2, 0x34, 0x05);
        assert_eq!(operator(&chip, 3, phase::SLOT_S3).mul, 5);
        assert_eq!(operator(&chip, 0, phase::SLOT_S3).mul, 2);
    }

    #[test]
    fn fnum_latch_applies_on_a0() {
        let mut chip = Ym2612::new();

        // &A4 is block in bits 5-3 and F-Num 10-8 in bits 2-0. It goes to a
        // latch and nothing else: the pair reaches the phase generator with the
        // next &A0 write (YM2608 application manual "3. Register descriptions",
        // `&A0` and `&A4`).
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA4, 0x24);
        let before = chip.phases.channel(0);
        assert_eq!(
            (before.fnum, before.block),
            (0, 0),
            "&A4 alone changes nothing"
        );

        write_reg(&mut chip, ADDR_1, DATA_1, 0xA0, 0x69);
        let after = chip.phases.channel(0);
        assert_eq!((after.fnum, after.block), (0x469, 4));

        // A second &A0 write applies the latch it still holds and only replaces
        // the low eight bits.
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA0, 0x01);
        let again = chip.phases.channel(0);
        assert_eq!((again.fnum, again.block), (0x401, 4));

        // The latch is per channel: writing the next channel of part I leaves the
        // channel written above alone.
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA5, 0x08);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA1, 0x10);
        assert_eq!(chip.phases.channel(1).fnum, 0x010);
        assert_eq!(chip.phases.channel(0).fnum, 0x401);
    }

    #[test]
    fn phase_counts_the_increment_every_sample() {
        // The smallest interesting increment: mul 1, block 0 and F-Num 2 is
        // (2 << 0) >> 1 = 1, so the phase is the sample count.
        let mut chip = Ym2612::new();
        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x01);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA0, 0x02);

        for expected in 1..=5u32 {
            chip.clock();
            assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, expected);
        }
    }

    #[test]
    fn phase_wraps_at_20_bits() {
        let mut chip = Ym2612::new();
        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x0F);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA4, 0x3F);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA0, 0xFF);

        clock_for(&mut chip, 1);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, 0xDFC40);
        clock_for(&mut chip, 1);
        assert_eq!(
            operator(&chip, 0, phase::SLOT_S1).phase,
            2 * 0xDFC40 - 0x10_0000,
            "the counter is 20 bits, so it wraps rather than saturating"
        );
    }

    #[test]
    fn keyon_resets_phase() {
        let mut chip = Ym2612::new();
        // Channel 1 operator 1: mul 1, block 4, F-Num 0x469, which is an
        // increment of (0x469 << 4) >> 1 = 0x2348 a sample.
        write_reg(&mut chip, ADDR_1, DATA_1, 0x30, 0x01);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA4, 0x24);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA0, 0x69);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, 0);

        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1);
        clock_for(&mut chip, 3);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, 3 * 0x2348);

        // Keying an operator that is already on is not a key-on: the phase runs
        // on from where it is.
        let phase = operator(&chip, 0, phase::SLOT_S1).phase;
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, phase);

        // Only a key-on of an operator that is off restarts it at 0.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, 0);
        assert!(!operator(&chip, 0, phase::SLOT_S1).key_on);
        clock_for(&mut chip, 2);
        assert_ne!(operator(&chip, 0, phase::SLOT_S1).phase, 0);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1);
        assert_eq!(operator(&chip, 0, phase::SLOT_S1).phase, 0);
    }

    #[test]
    fn keyon_channel_field_selects_the_six_channels() {
        let mut chip = Ym2612::new();

        // &28 bits 2-0 are absolute channel numbers, not part-relative ones:
        // 0-2 are the part I channels, 4-6 the part II ones (YM2608 application
        // manual `&28`). 3 and 7 are the undecoded slots of each part and key
        // nothing at all.
        for (field, channel) in [(0, 0), (1, 1), (2, 2), (4, 3), (5, 4), (6, 5)] {
            write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1 | field);
            assert!(
                operator(&chip, channel, phase::SLOT_S1).key_on,
                "channel field {field} keys on channel {channel}"
            );
        }

        let mut chip = Ym2612::new();
        for field in [3, 7] {
            write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1 | field);
            for channel in 0..6 {
                assert!(
                    !operator(&chip, channel, phase::SLOT_S1).key_on,
                    "channel field {field} is not decoded"
                );
            }
        }
    }

    #[test]
    fn keyon_operator_bits_are_s4_s3_s2_s1() {
        let mut chip = Ym2612::new();

        // &28 bits 7-4 are operators S4, S3, S2 and S1 in that order, while the
        // register offsets of a channel run S1, S3, S2, S4: so bit 5 is the
        // operator that lives at +0x8 and bit 6 the one at +0x4.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S2 | KEY_ON_S3);
        assert!(operator(&chip, 0, phase::SLOT_S2).key_on);
        assert!(operator(&chip, 0, phase::SLOT_S3).key_on);
        assert!(!operator(&chip, 0, phase::SLOT_S1).key_on);
        assert!(!operator(&chip, 0, phase::SLOT_S4).key_on);
    }

    /// The five per-operator groups of the envelope, `&40` to `&80`, at the slot
    /// offsets of a channel: +0 is operator 1, +4 operator 3, +8 operator 2 and
    /// +C operator 4, with the channel in the low two bits and a part's fourth
    /// address - `&43` and friends - not decoded at all (YM2608 application
    /// manual "3. Register descriptions").
    #[test]
    fn envelope_registers_reach_their_operator_in_slot_order() {
        let mut chip = Ym2612::new();

        // The slot offsets of channel 1 with the total level to write there.
        let slots: [(u8, usize, u8); 4] = [
            (0x0, phase::SLOT_S1, 0x00),
            (0x4, phase::SLOT_S3, 0x10),
            (0x8, phase::SLOT_S2, 0x20),
            (0xC, phase::SLOT_S4, 0x30),
        ];
        // AR 31 is rate 62 at key code 0, so a key on takes an operator to full
        // level with the write itself: what each one contributes after that is
        // its `&40` total level and nothing else (`&50`, `&40`).
        for (offset, _, total_level) in slots {
            write_reg(&mut chip, ADDR_1, DATA_1, 0x50 + offset, 0x1F);
            write_reg(&mut chip, ADDR_1, DATA_1, 0x40 + offset, total_level);
        }
        // `&43` is the undecoded fourth channel of part I's `&40` group. Writing
        // it may not reach the operator part II keeps at the same number.
        write_reg(&mut chip, ADDR_1, DATA_1, 0x43, 0x7F);
        // Part II's operator 1, keyed on with the channel field 4 of `&28`, with
        // a total level of its own only if `&43` leaked into it.
        write_reg(&mut chip, ADDR_2, DATA_2, 0x50, 0x1F);

        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, 0xF0);
        write_reg(&mut chip, ADDR_2, DATA_2, REG_KEY_ON, KEY_ON_S1 | 4);

        for (offset, slot, total_level) in slots {
            let envelope = chip.envelopes.envelope(0, slot);
            assert_eq!(
                envelope.state(),
                envelope::State::Attack,
                "slot offset {offset:#X}"
            );
            assert_eq!(envelope.level(), 0, "an attack of rate 62 is instant");
            assert_eq!(envelope.output(), u16::from(total_level) << 3);
        }
        assert_eq!(
            chip.envelopes.envelope(3, phase::SLOT_S1).output(),
            0,
            "the `&43` write went nowhere, so part II's operator 1 is at full level"
        );
    }

    /// `&28` and `clock` together: a key on starts the attack, a key off starts
    /// the release, and neither runs at sample rate but at a third of it
    /// (Nemesis, "YM2612 envelope generator").
    #[test]
    fn key_on_and_off_steps_the_envelope_every_third_sample() {
        let mut chip = Ym2612::new();

        // AR 24 is rate 48 at key code 0: a shift of 0 and a row of all ones, so
        // every envelope update takes exactly one step of the attack.
        write_reg(&mut chip, ADDR_1, DATA_1, 0x50, 24);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1);

        let level = |chip: &Ym2612| chip.envelopes.envelope(0, phase::SLOT_S1).level();
        let mut level_now = level(&chip);
        assert_eq!(
            level_now,
            envelope::SILENT,
            "nothing keyed on has moved yet"
        );

        for _ in 0..4 {
            clock_for(&mut chip, 2);
            assert_eq!(level(&chip), level_now, "two samples are not an update");
            chip.clock();
            let after = level(&chip);
            assert!(
                after < level_now,
                "the third sample of the group takes the step"
            );
            level_now = after;
        }

        // A key off goes to release, and RR 15 is the release rate 31: a shift of
        // 4, so its update points are 16 counts of the envelope counter apart and
        // its row is 0 at only one of the eight phases it spreads those over.
        // 32 updates - 96 samples - therefore cannot miss a step (YM2608 `&80`).
        write_reg(&mut chip, ADDR_1, DATA_1, 0x80, 0x0F);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, 0);
        assert_eq!(
            chip.envelopes.envelope(0, phase::SLOT_S1).state(),
            envelope::State::Release
        );

        clock_for(&mut chip, 3 * 32);
        assert!(level(&chip) > level_now, "the release is on its way up");
    }
}
