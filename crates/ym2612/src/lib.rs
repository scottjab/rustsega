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
//! (`channel`), and turned into the sample `clock` answers with by the 9-bit DAC
//! and the ladder of M5-07b (#112) (`output`). The LFO of M5-08 (#113) is the
//! wobble underneath all of it: one 7-bit counter of `&22`'s making (`lfo`), read
//! out once as an attenuation added to the envelopes and once as an offset on the
//! F-Nums behind the phase counters. The channel 3 modes of M5-09 (#114) are the
//! two exceptions on top of all that: `&27` bits 7-6 either give operators 1 to 3 of
//! channel 3 F-Nums of their own out of `&A8`-`&AE` (`phase`), or - in CSM mode -
//! key the four operators of the channel at every timer A overflow of `timers`,
//! which [`Ym2612::csm_step`] passes to `envelope`. The channel 6 PCM DAC and the
//! L/R routing of M5-11 (#116) are the last two steps to the pin: `&2B` bit 7 puts
//! `&2A` in place of the FM of channel 6 (`Ym2612::channel_sums`), and the two
//! enable bits of `&B4`-`&B6` decide which of the two output sides each of the six
//! converters reaches (`regs`, and `output`).
//!
//! Register bit documentation cites the Yamaha YM2608 (OPNA) application manual
//! by register number, which is the manual that documents the OPN register set
//! the Genesis part shares, and plutiedev's "YM2612 registers" page where that
//! one is terse.

mod channel;
mod envelope;
mod lfo;
mod operator;
mod output;
mod phase;
mod regs;
mod tables;
mod timers;

pub use output::ChipModel;

use channel::FmChannel;
use envelope::EnvelopeGen;
use lfo::Lfo;
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

/// The channel `&2B` bit 7 turns into a PCM DAC: channel 6, the sixth of the six
/// and the only one the register means (YM2608 application manual "3. Register
/// descriptions", `&2B`).
const DAC_CHANNEL: usize = CHANNELS - 1;

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
    /// The LFO of `&22`, and the per-channel AMS and PMS of `&B4` that decide how
    /// far its triangle reaches the envelopes and the phase increments (M5-08,
    /// #113).
    lfo: Lfo,
    /// The six channels' operator sums: the algorithm and the feedback of the
    /// `&B0` group, and the mix those leave behind (M5-07a, #111).
    channels: [FmChannel; CHANNELS],
    /// Which DAC to model: [`ChipModel::Ym2612`], the discrete chip with its
    /// ladder effect, until something says otherwise (M5-07b, #112).
    model: ChipModel,
    /// Whether channel 3 is keyed on by CSM right now: the pulse of a timer A
    /// overflow in CSM mode, held for the one sample and released by the first
    /// sample that overflows no more (M5-09, #114, and [`Ym2612::csm_step`]).
    csm_key: bool,
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
            lfo: Lfo::new(),
            channels: std::array::from_fn(|_| FmChannel::new()),
            model: ChipModel::Ym2612,
            csm_key: false,
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

        self.lfo.register_written(part, register, value);
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

    /// Say which chip's DAC [`Ym2612::clock`] models: [`ChipModel::Ym2612`] for
    /// the discrete part of a Model 1, with its ladder effect, or
    /// [`ChipModel::Ym3438`] for the integrated part of a Model 2 (M5-07b,
    /// #112). Nothing else about the chip differs between the two, and the
    /// default is [`ChipModel::Ym2612`].
    pub fn set_model(&mut self, model: ChipModel) {
        self.model = model;
    }

    /// Advance the chip by one output sample - 144 68k clocks, about 53267 Hz
    /// (docs/ARCHITECTURE.md, "Clocks and timing") - and return the `(left,
    /// right)` sample.
    ///
    /// What the step does is run the two timers, advance the 24 phase counters,
    /// step the envelopes - once every three samples - mix the four operators of
    /// each channel into the algorithm `&B0` names - channel 6 answered by `&2A`
    /// instead of its operators while `&2B` bit 7 is set - and let BUSY run down. The
    /// answer is the six channel sums through their DACs and added, in the 9-bit
    /// LSBs of `crate::output` (M5-07b, #112), each side taking the channels its
    /// `&B4` bits route to it (M5-11, #116) and what the mix is scaled and filtered
    /// to being the mixer of M5-14 (#119).
    /// A silent chip is therefore not at 0 on a [`ChipModel::Ym2612`] - six
    /// converters at their ladder offset is a DC level of 24.
    ///
    /// The order is the chip's own: an operator is read at the phase the phase
    /// generator has just arrived at and at the attenuation the envelope
    /// generator has just arrived at, so a `&B0`, `&40` or `&A0` write is heard
    /// from the very next sample (`crate::channel`).
    ///
    /// The LFO steps first, so the phase modulation it applies to `&A0` and the
    /// amplitude modulation it applies to `&40` are the ones that belong to this
    /// sample rather than the one before (M5-08, #113, and `crate::lfo`).
    pub fn clock(&mut self) -> (i16, i16) {
        let config = self.regs.timer_config();
        self.timers.tick(&config);
        self.csm_step();
        self.lfo.tick();
        self.phases.tick(self.lfo.pms(), self.lfo.pm_phase());
        self.envelopes.tick(&self.phases);

        for channel in 0..CHANNELS {
            let phase = std::array::from_fn(|slot| self.phases.phase(channel, slot));
            let att = std::array::from_fn(|slot| self.attenuation(channel, slot));
            self.channels[channel].step(phase, att);
        }

        self.busy = self.busy.saturating_sub(INTERNAL_CYCLES_PER_SAMPLE);
        let sums = self.channel_sums();
        let pan = std::array::from_fn(|channel| self.regs.pan(channel));
        output::mix(sums, pan, self.model)
    }

    /// The six channel sums the output stage reads this sample: the FM of each
    /// channel, except that `&2B` bit 7 puts the `&2A` sample in place of channel
    /// 6's (YM2608 application manual `&2A` and `&2B`, M5-11 #116).
    ///
    /// Only the sum is replaced. The operators of channel 6 keep their phase
    /// counters and their envelopes, because the chip walks all 24 slots whatever
    /// the output stage is listening to - which is how ymfm reads it too, clocking
    /// all six channels and dropping only the sixth out of the sum - and a driver
    /// that turns the DAC off again finds the FM of the channel where it left it.
    fn channel_sums(&self) -> [i16; CHANNELS] {
        let dac = self.regs.dac_enabled();
        let sample = output::pcm_sample(self.regs.dac_data());
        std::array::from_fn(|channel| {
            if dac && channel == DAC_CHANNEL {
                sample
            } else {
                self.channels[channel].output()
            }
        })
    }

    /// The CSM key of channel 3, one sample in.
    ///
    /// In CSM mode - `&27` 10 - every timer A overflow keys the four operators of
    /// channel 3 on, and the first sample that overflows no more keys them off
    /// again, which is what makes the mode a way of striking a drum: the driver
    /// runs timer A at the rate it wants the noise to hit and never writes `&28`
    /// for channel 3 at all (YM2608 application manual `&27`, M5-09 #114, and
    /// [`crate::envelope::EnvelopeGen::csm_key`] for the half of it the envelopes do
    /// and [`crate::phase::PhaseGen::key_state`] for the `&28` state that holds an
    /// operator on through the end of a pulse).
    ///
    /// The mode is read as a whole, so 11 - which the register lists call "CSM and
    /// frequency mode" - does not key anything either: like 01, it is only the
    /// frequency half for this model. Nothing here moves the phase counters, whose
    /// key state is `&28`'s alone.
    fn csm_step(&mut self) {
        let csm = self.regs.ch3_mode() == regs::CH3_MODE_CSM && self.timers.overflowed_a();
        if csm == self.csm_key {
            return;
        }
        self.csm_key = csm;
        self.envelopes.csm_key(&self.phases, csm);
    }

    /// What one operator is attenuated by as the channel mix reads it: its
    /// envelope, plus the LFO's AM attenuation for this channel when this
    /// operator's `&60` bit 7 asks for it (M5-08, #113, and [`crate::lfo`]).
    ///
    /// The AM joins outside [`crate::envelope::Envelope::output`] and is clamped
    /// at silence again. The chip adds it before the total level and clamps once;
    /// the two come to the same number, since an envelope already at silence
    /// stays there whatever the LFO does to it, and the envelope does not know
    /// the LFO exists (`&60`, `&40`).
    fn attenuation(&self, channel: usize, slot: usize) -> u16 {
        let envelope = self.envelopes.envelope(channel, slot);
        let am = self.lfo.amplitude(channel, envelope.am_enable());
        (envelope.output() + u16::from(am)).min(envelope::SILENT)
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

    /// `&28` bits 7-4 together: key on all four operators of a channel at once.
    const KEY_ON_ALL: u8 = 0xF0;

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

    /// Register `&B0`'s channel group, and the two frequency registers, named
    /// here because the whole-chip patch below writes all three of them for
    /// every channel of both parts.
    const REG_ALGORITHM: u8 = 0xB0;

    /// The whole chip at full voice: every operator of every channel of both
    /// parts at total level 0 with multiple 1 and an instant attack, every
    /// channel at block 4, F-Num 0x210 and algorithm 7, and all four operators
    /// of all six channels keyed on.
    ///
    /// Channels 1 to 3 are part I and 4 to 6 part II, each part numbering its
    /// own three channels from register offset 0 ([`regs::channel_of`]) while
    /// `&28` is part I's alone and carries the global channel number - 3 of
    /// which is not decoded ([`REG_KEY_ON`], `&30`, `&40`, `&50`, `&A0`, `&A4`,
    /// `&B0`). The block and F-Num are the patch of [`crate::channel`]'s tests:
    /// a cycle every 248 samples, so a few hundred of them carry the wave right
    /// across the clamp of the channel sum.
    fn all_six_channels_open(chip: &mut Ym2612) {
        for (addr, data) in [(ADDR_1, DATA_1), (ADDR_2, DATA_2)] {
            for channel in 0..3u8 {
                for slot in 0..4u8 {
                    // The per-operator groups are slot stride 4 and channel
                    // stride 1, not the other way round (`crate::regs::
                    // operator_of_group`, and the `+00` `+04` `+08` `+0C` of
                    // `crate::phase`).
                    let offset = slot * 4 + channel;
                    write_reg(chip, addr, data, 0x30 + offset, 0x01);
                    write_reg(chip, addr, data, 0x40 + offset, 0x00);
                    write_reg(chip, addr, data, 0x50 + offset, 0x1F);
                }
                // `&A4` first and `&A0` second, because `&A4` only latches its
                // two F-Num bits and its block and `&A0` is what applies them
                // (`crate::phase`).
                write_reg(chip, addr, data, 0xA4 + channel, 0x22);
                write_reg(chip, addr, data, 0xA0 + channel, 0x10);
                write_reg(chip, addr, data, REG_ALGORITHM + channel, 0x07);
            }
        }
        for channel in 0..CHANNELS as u8 {
            let field = channel + u8::from(channel > 2);
            write_reg(chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_ALL | field);
        }
    }

    /// One step of `chip`, checked against the six channel sums passed through
    /// [`output::dac`] by hand as it goes: the chip's own sum has to equal that
    /// exactly, which is what says it neither clamped nor wrapped (M5-07b,
    /// #112). The exact sum is what the caller then looks at.
    fn step_and_track(chip: &mut Ym2612, model: ChipModel) -> i32 {
        let (left, right) = chip.clock();
        let mut exact = 0i32;
        for channel in 0..CHANNELS {
            exact += output::dac(chip.channels[channel].output(), model);
        }
        assert_eq!(
            i32::from(left),
            exact,
            "the pin is the six DAC outputs added"
        );
        assert_eq!(
            left, right,
            "nothing here is panned off, so both sides are the whole mix (#116)"
        );
        exact
    }

    /// Six channels of algorithm 7 with every operator at full level is the
    /// loudest thing the chip can be told to do, and their sum is what the 9-bit
    /// DAC domain has to survive: six codes of at most 255 with at most 4 of
    /// ladder on top is 1554, well inside the i16 `clock` returns, so the sum
    /// needs no clamp and must not wrap either.
    #[test]
    fn sum_of_six_fits_i16() {
        for model in [ChipModel::Ym2612, ChipModel::Ym3438] {
            let mut chip = Ym2612::new();
            chip.set_model(model);
            all_six_channels_open(&mut chip);

            let mut loudest = 0;
            for _ in 0..256 {
                let exact = step_and_track(&mut chip, model);
                assert!(
                    exact.abs() <= 6 * 259,
                    "{model:?}: {exact} is past six DAC outputs of 259"
                );
                loudest = loudest.max(exact.abs());
            }

            // Algorithm 7, four carriers at total level 0, is pinned at the top
            // and the bottom of the channel clamp for most of its run, so a run
            // of it is nowhere near the 6 * 4 of a chip with nothing to say.
            assert!(loudest > 1000, "{model:?}: the six channels are not loud");
        }
    }

    /// Which DAC the model chooses is audible on a chip with nothing else going
    /// on: a YM2612 that has never been written to sits at the ladder offset of
    /// six silent converters, and a YM3438 at exactly nothing.
    #[test]
    fn chip_model_selects_the_ladder() {
        let mut discrete = Ym2612::new();
        assert_eq!(
            discrete.clock(),
            (24, 24),
            "six silent channels at +4 each: the default is the discrete chip"
        );

        let mut asic = Ym2612::new();
        asic.set_model(ChipModel::Ym3438);
        assert_eq!(asic.clock(), (0, 0), "and the 3438 has no offset to sit on");

        // Mid-run from the same state, too: the DAC is all that moved.
        discrete.set_model(ChipModel::Ym3438);
        assert_eq!(discrete.clock(), (0, 0));
        discrete.set_model(ChipModel::Ym2612);
        assert_eq!(discrete.clock(), (24, 24));
    }

    /// Register `&22`, the LFO: bit 3 the AM/PM enable and bits 2-0 the frequency
    /// (YM2608 application manual `&22`).
    const REG_LFO: u8 = 0x22;
    /// Register `&B4`, the first of the amplitude and phase modulation group:
    /// bits 5-4 AMS and bits 2-0 PMS (`&B4`, and `crate::lfo`).
    const REG_AMS_PMS: u8 = 0xB4;
    /// `&22` bit 3: the LFO runs.
    const LFO_ON: u8 = 0x08;
    /// `&60` bit 7: this operator takes part in the amplitude modulation.
    const AM_ENABLE: u8 = 0x80;

    /// Width of the phase counter, for reading an increment off two samples of it
    /// even when it wrapped in between (`crate::phase`).
    const PHASE_MASK: u32 = 0x000F_FFFF;

    /// Channel 1 held at full level with nothing moving: total level 0, an attack
    /// fast enough to be instant at key code 0, and decay 1 at rate 0, so its
    /// envelope attenuation is 0 and stays there and anything that moves the
    /// operator's attenuation afterwards moved it for a reason (`&40`, `&50`,
    /// `&60`).
    fn hold_channel_one_open(chip: &mut Ym2612) {
        write_reg(chip, ADDR_1, DATA_1, 0x40, 0x00);
        write_reg(chip, ADDR_1, DATA_1, 0x50, 0x1F);
        write_reg(chip, ADDR_1, DATA_1, 0x60, 0x00);
        write_reg(chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_S1);
    }

    /// Channel 1 at multiple 1, block 1 and F-Num 0x400, which is a phase
    /// increment of (0x400 << 1) >> 1 = 1024 a sample, and a PM offset therefore
    /// has somewhere obvious to move it from (`&30`, `&A4`, `&A0`).
    fn tune_channel_one(chip: &mut Ym2612) {
        write_reg(chip, ADDR_1, DATA_1, 0x30, 0x01);
        write_reg(chip, ADDR_1, DATA_1, 0xA4, 0x0C);
        write_reg(chip, ADDR_1, DATA_1, 0xA0, 0x00);
    }

    /// Operator 1's attenuation over four cycles of the LFO at `&22` 7 - a
    /// 5-sample step, so a reading every 20 samples is a reading every fourth
    /// step, which covers both halves of the triangle (`crate::lfo`).
    fn attenuation_over_cycles(chip: &mut Ym2612) -> Vec<u16> {
        (0..512)
            .map(|_| {
                clock_for(chip, 20);
                chip.attenuation(0, phase::SLOT_S1)
            })
            .collect()
    }

    /// Operator 1's phase increment over four cycles of the LFO at `&22` 7, one
    /// value per PM phase: 20 samples of a 5-sample step is four steps, which is
    /// exactly how often `crate::lfo` reads the counter for PM, so each reading is
    /// one PM phase and each of the 20 samples in the group ran at it.
    fn increment_over_cycles(chip: &mut Ym2612) -> Vec<u32> {
        let mut steps = vec![];
        let mut before = operator(chip, 0, phase::SLOT_S1).phase;
        for _ in 0..512 {
            clock_for(chip, 20);
            let after = operator(chip, 0, phase::SLOT_S1).phase;
            steps.push((after.wrapping_sub(before) & PHASE_MASK) / 20);
            before = after;
        }
        steps
    }

    /// AMS 3 - the deepest - with the operator's `&60` bit 7 clear is not amplitude
    /// modulation: the attenuation does not move over four cycles of the LFO, and
    /// the same channel with the bit set does move (`&60`, `&B4`, and #113).
    #[test]
    fn am_off_when_bit_clear() {
        let mut chip = Ym2612::new();
        hold_channel_one_open(&mut chip);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_AMS_PMS, 0x30);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_LFO, LFO_ON);

        let off = attenuation_over_cycles(&mut chip);
        assert!(
            off.iter().all(|att| *att == 0),
            "AMS 3 with `&60` bit 7 clear is silence from the LFO: {off:?}"
        );

        // The same operator, and the same decay rate: only the AM enable moved.
        write_reg(&mut chip, ADDR_1, DATA_1, 0x60, AM_ENABLE);
        let on = attenuation_over_cycles(&mut chip);
        assert!(
            on.iter().any(|att| *att != 0),
            "`&60` bit 7 set, and the LFO still reaches nothing: {on:?}"
        );
    }

    /// AMS 3 is 126 of attenuation at the peak of the triangle and nothing at the
    /// bottom: 11.8 dB of the 1024 of the envelope's range, which is the depth the
    /// YM2608 application manual's AM table gives AMS 3 (`&B4`, and #113).
    #[test]
    fn am_max_depth() {
        let mut chip = Ym2612::new();
        hold_channel_one_open(&mut chip);
        write_reg(&mut chip, ADDR_1, DATA_1, 0x60, AM_ENABLE);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_AMS_PMS, 0x30);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_LFO, LFO_ON);

        let on = attenuation_over_cycles(&mut chip);
        let deepest = *on.iter().max().expect("a cycle of readings");
        let shallowest = *on.iter().min().expect("a cycle of readings");
        assert_eq!(
            shallowest, 0,
            "the bottom of the triangle is the envelope alone"
        );
        assert_eq!(
            deepest, 126,
            "and the top is 126 of attenuation on top of it"
        );
    }

    /// PMS 0 leaves the phase increment alone: a channel with the LFO running at
    /// full speed and PMS 0 counts at exactly the rate it would with no LFO at all
    /// (`&B4`, and #113).
    #[test]
    fn pms_zero_no_vibrato() {
        let mut chip = Ym2612::new();
        tune_channel_one(&mut chip);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_AMS_PMS, 0x00);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_LFO, LFO_ON | 0x07);

        let steps = increment_over_cycles(&mut chip);
        assert!(
            steps.iter().all(|step| *step == 1024),
            "PMS 0 is a depth of zero, so nothing moves: {steps:?}"
        );
    }

    /// PMS 7 - the deepest - moves the increment over the LFO cycle, and it moves
    /// it symmetrically about the increment of PMS 0: the top of the swing is as
    /// far above 1024 as the bottom is below it, because PM offsets the F-Num
    /// rather than replacing it (`&B4`, `crate::lfo::pm_offset`, and #113).
    #[test]
    fn pm_changes_increment() {
        let mut chip = Ym2612::new();
        tune_channel_one(&mut chip);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_AMS_PMS, 0x07);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_LFO, LFO_ON | 0x07);

        let steps = increment_over_cycles(&mut chip);
        let highest = *steps.iter().max().expect("a cycle of readings");
        let lowest = *steps.iter().min().expect("a cycle of readings");
        assert!(
            highest > 1024 && lowest < 1024,
            "PMS 7 is not still: {steps:?}"
        );
        assert_eq!(
            highest - 1024,
            1024 - lowest,
            "the swing is symmetric about the F-Num that was written"
        );
        assert_eq!((highest, lowest), (1024 + 48, 1024 - 48));
    }

    /// Channel 3 of the two channel 3 modes of M5-09 (#114): part I's third
    /// channel, so channel index 2 and register offset 2.
    const CHANNEL3: usize = 2;

    /// `&27` bits 7-6, the channel 3 mode: 00 normal, 01 the special frequency
    /// mode, 10 CSM.
    const MODE_NORMAL: u8 = 0x00;
    /// `&27` 01: the special frequency mode, where operators 1 to 3 of channel 3
    /// read `&A8`-`&AE` instead of the channel's own `&A2`/`&A6` (YM2608
    /// application manual `&A8`).
    const MODE_SPECIAL: u8 = 0x40;
    /// `&27` 10: CSM, the special frequency mode and the timer A key-on of
    /// channel 3 on top of it (`&27`, and [`crate::timers`]).
    const MODE_CSM: u8 = 0x80;
    /// `&27` 11: the mode the register lists call "CSM and frequency mode". It is
    /// the special frequency mode alone, without the CSM key-on: the mode decode
    /// of #114 turns CSM on for mode 10 only, and
    /// [`csm_only_in_csm_mode_keys_on`] pins the difference.
    const MODE_SPECIAL_AND_CSM: u8 = 0xC0;

    /// Channel 3 at block 1, F-Num `fnum`, with every one of its operators at
    /// detune 0 and multiple 1: at block 1 with no detune and a multiple of 1 the
    /// phase increment of an operator is its F-Num unchanged
    /// ([`crate::phase::increment`]), so a measured increment reads back the
    /// frequency an operator was given (`&30`, `&A2`, `&A6`).
    fn tune_channel_three(chip: &mut Ym2612, fnum: u16) {
        for slot in 0..4u8 {
            // The `&30` group is slot stride 4 and channel stride 1, and channel 3
            // is offset 2 of part I (`crate::regs::operator_of`).
            write_reg(chip, ADDR_1, DATA_1, 0x32 + slot * 4, 0x01);
        }
        // `&A6` first and `&A2` second, because `&A6` latches and `&A2` applies
        // (#108).
        write_reg(
            chip,
            ADDR_1,
            DATA_1,
            0xA6,
            0x08 | ((fnum >> 8) as u8 & 0x07),
        );
        write_reg(chip, ADDR_1, DATA_1, 0xA2, fnum as u8);
    }

    /// One of channel 3's special-mode frequency pairs: `low` the `&A8`, `&A9` or
    /// `&AA` of the operator, whose partner is `low + 4` and which is latched the
    /// same way `&A4` latches `&A0` (YM2608 application manual `&A8`).
    fn set_special_fnum(chip: &mut Ym2612, low: u8, fnum: u16) {
        write_reg(
            chip,
            ADDR_1,
            DATA_1,
            low + 4,
            0x08 | ((fnum >> 8) as u8 & 0x07),
        );
        write_reg(chip, ADDR_1, DATA_1, low, fnum as u8);
    }

    /// The phase increment of channel 3's `slot` over the next sample: the two
    /// channel 3 modes of #114 are a mapping from registers to frequency
    /// generators and to keys, so this is what reading one means.
    fn ch3_step(chip: &mut Ym2612, slot: usize) -> u32 {
        let before = operator(chip, CHANNEL3, slot).phase;
        chip.clock();
        operator(chip, CHANNEL3, slot).phase.wrapping_sub(before) & PHASE_MASK
    }

    /// The envelope state of each of channel 3's four operators.
    fn ch3_states(chip: &Ym2612) -> [envelope::State; 4] {
        std::array::from_fn(|slot| chip.envelopes.envelope(CHANNEL3, slot).state())
    }

    /// Channel 3 special mode gives operators 1, 2 and 3 the frequencies of
    /// `&A9`/`&AD`, `&AA`/`&AE` and `&A8`/`&AC` and leaves operator 4 on the
    /// channel's own `&A2`/`&A6`; in normal mode all four operators share that one
    /// F-Num even when the special registers are written (YM2608 application
    /// manual `&A8`, and #114).
    #[test]
    fn special_mode_uses_per_op_fnum() {
        let mut chip = Ym2612::new();
        tune_channel_three(&mut chip, 0x300);
        set_special_fnum(&mut chip, 0xA9, 0x100);
        set_special_fnum(&mut chip, 0xAA, 0x200);
        set_special_fnum(&mut chip, 0xA8, 0x380);

        // Normal mode with the special registers already written: they are stored,
        // since the register file is the whole state of a write-only chip, but they
        // reach no operator.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_NORMAL);
        let normal = [
            ch3_step(&mut chip, phase::SLOT_S1),
            ch3_step(&mut chip, phase::SLOT_S2),
            ch3_step(&mut chip, phase::SLOT_S3),
            ch3_step(&mut chip, phase::SLOT_S4),
        ];
        assert_eq!(
            normal, [0x300; 4],
            "normal mode is one F-Num for all four operators"
        );

        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_SPECIAL);
        let special = [
            ch3_step(&mut chip, phase::SLOT_S1),
            ch3_step(&mut chip, phase::SLOT_S2),
            ch3_step(&mut chip, phase::SLOT_S3),
            ch3_step(&mut chip, phase::SLOT_S4),
        ];
        assert_eq!(
            special,
            [0x100, 0x200, 0x380, 0x300],
            "special mode is three extra frequency generators, and operator 4 keeps the channel"
        );

        // Mode 11 is the special frequency mode too: the extra registers are heard
        // and it is only the key-on of CSM that mode 10 adds.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_SPECIAL_AND_CSM);
        let both = [
            ch3_step(&mut chip, phase::SLOT_S1),
            ch3_step(&mut chip, phase::SLOT_S2),
            ch3_step(&mut chip, phase::SLOT_S3),
            ch3_step(&mut chip, phase::SLOT_S4),
        ];
        assert_eq!(both, special, "mode 11 has the frequencies of mode 01");
    }

    /// Which operator each of `&A8`-`&AE` belongs to: `&A8`/`&AC` is operator 3,
    /// so writing it moves operator 3 alone, and `&A3`/`&A7` - the pair a YM2612
    /// has no decode for at all - move nothing (YM2608 application manual `&A8`;
    /// the mapping is the one M5-09 #114 names).
    #[test]
    fn a8_maps_to_op3() {
        let mut chip = Ym2612::new();
        tune_channel_three(&mut chip, 0x300);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_SPECIAL);

        // Only the `&A8`/`&AC` pair, and nothing else of the special group: an
        // F-Num of 0 for the two operators that have their own and are untouched.
        set_special_fnum(&mut chip, 0xA8, 0x180);
        let steps = [
            ch3_step(&mut chip, phase::SLOT_S1),
            ch3_step(&mut chip, phase::SLOT_S2),
            ch3_step(&mut chip, phase::SLOT_S3),
            ch3_step(&mut chip, phase::SLOT_S4),
        ];
        assert_eq!(
            steps,
            [0, 0, 0x180, 0x300],
            "`&A8` is operator 3: not operator 1, and not operator 4, which keeps `&A2`"
        );

        // `&A3` and `&A7` are not the second pair of anything: the chip does not
        // decode them, so they leave the whole of channel 3 where it was.
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA7, 0x0F);
        write_reg(&mut chip, ADDR_1, DATA_1, 0xA3, 0xFF);
        let after = [
            ch3_step(&mut chip, phase::SLOT_S1),
            ch3_step(&mut chip, phase::SLOT_S2),
            ch3_step(&mut chip, phase::SLOT_S3),
            ch3_step(&mut chip, phase::SLOT_S4),
        ];
        assert_eq!(after, steps, "`&A3` and `&A7` are undecoded addresses");
    }

    /// CSM: a timer A overflow keys all four operators of channel 3 on, and the
    /// next sample keys them off again unless something else holds them, so a timer
    /// A period of three samples is an attack on every third sample and a release
    /// on the one after it. CSM does not ask for the status flag either: the timer
    /// A overflow enable of `&27` bit 2 stays clear throughout (YM2608 application
    /// manual `&27`, M5-09 #114, and `crate::timers`).
    #[test]
    fn csm_keys_on_at_timer_a_overflow() {
        let mut chip = Ym2612::new();
        // 1021 of the 1024 counts of timer A: an overflow on the third sample, the
        // sixth, the ninth (`&24`, `&25`, and `crate::timers`).
        set_timer_a(&mut chip, 1021);
        // CSM mode and the load bit of timer A together, and bit 2 - the overflow
        // enable - left clear.
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_CSM | LOAD_A);

        for _ in 0..4 {
            for _ in 0..2 {
                chip.clock();
                assert_eq!(
                    ch3_states(&chip),
                    [envelope::State::Release; 4],
                    "a sample with no overflow is a sample with no key on"
                );
            }
            chip.clock();
            assert_eq!(
                ch3_states(&chip),
                [envelope::State::Attack; 4],
                "the overflow sample keys the four operators of channel 3 on"
            );
            assert!(
                !bit(chip.read_status(), STATUS_TIMER_A),
                "CSM runs off the overflow, not off the status flag it does not set"
            );
        }
    }

    /// A `&28` key on holds channel 3 through CSM: the key off that follows an
    /// overflow ends the CSM pulse, and an operator the driver is holding on stays
    /// in its attack (`&28`, `&27`, and #114).
    #[test]
    fn csm_key_is_held_by_a_real_key_on() {
        let mut chip = Ym2612::new();
        set_timer_a(&mut chip, 1021);
        write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, MODE_CSM | LOAD_A);
        // All four operators of channel 3 keyed on by hand: channel field 2, which
        // is `&28`'s absolute number for the same channel (`crate::phase`).
        write_reg(&mut chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_ALL | 2);

        // Past one overflow and the sample after it, which is where the CSM pulse
        // would have released them on its own.
        clock_for(&mut chip, 5);
        assert_eq!(
            ch3_states(&chip),
            [envelope::State::Attack; 4],
            "`&28` is holding channel 3, so the end of a CSM pulse releases nothing"
        );
    }

    /// Only mode 10 keys anything on: normal mode and the special frequency modes
    /// leave channel 3's envelopes where they are through a hundred timer A
    /// overflows (YM2608 application manual `&27`, and #114 for the mode 11 that
    /// takes the frequencies of CSM and none of its key-on).
    #[test]
    fn csm_only_in_csm_mode_keys_on() {
        for mode in [MODE_NORMAL, MODE_SPECIAL, MODE_SPECIAL_AND_CSM] {
            let mut chip = Ym2612::new();
            set_timer_a(&mut chip, 1021);
            write_reg(&mut chip, ADDR_1, DATA_1, REG_CONTROL, mode | LOAD_A);

            clock_for(&mut chip, 30);
            assert_eq!(
                ch3_states(&chip),
                [envelope::State::Release; 4],
                "mode {:02b} is not a CSM mode",
                mode >> 6
            );
        }
    }

    // The channel 6 PCM DAC and the per-channel L/R routing of M5-11 (#116).

    /// Register `&2A`: the 8-bit PCM sample of the channel 6 DAC, part I like the
    /// timers and the LFO beside it (YM2608 application manual "3. Register
    /// descriptions", `&2A`).
    const REG_DAC_DATA: u8 = 0x2A;
    /// Register `&2B`: bit 7 the enable that puts `&2A` in place of the FM of
    /// channel 6 (`&2B`).
    const REG_DAC_ENABLE: u8 = 0x2B;
    /// `&2B` bit 7.
    const DAC_ON: u8 = 0x80;

    /// `&B4`-`&B6` bit 7: this channel reaches the left output (`&B4`).
    const PAN_LEFT: u8 = 0x80;
    /// `&B4`-`&B6` bit 6: this channel reaches the right output (`&B4`).
    const PAN_RIGHT: u8 = 0x40;

    /// `&2A` and `&2B` of part I, the part that carries the global registers: the
    /// DAC is one per chip, like the timers and the LFO (`&2A`, `&2B`).
    fn set_dac(chip: &mut Ym2612, data: u8, enable: bool) {
        write_reg(chip, ADDR_1, DATA_1, REG_DAC_DATA, data);
        write_reg(
            chip,
            ADDR_1,
            DATA_1,
            REG_DAC_ENABLE,
            if enable { DAC_ON } else { 0 },
        );
    }

    /// The `&B4` of all six channels at once, AMS and PMS left at 0 so that
    /// nothing but the routing of a test moves (`&B4`).
    fn pan_all(chip: &mut Ym2612, pan: u8) {
        for (addr, data) in [(ADDR_1, DATA_1), (ADDR_2, DATA_2)] {
            for channel in 0..3u8 {
                write_reg(chip, addr, data, REG_AMS_PMS + channel, pan);
            }
        }
    }

    /// Channel 6 on its own and loud: algorithm 7, every operator at total level
    /// 0 with multiple 1 and an instant attack, at block 4 and F-Num 0x210, and
    /// all four operators keyed on. One channel of [`all_six_channels_open`], so
    /// part II's channel offset 2 and the channel field 6 of `&28` (`&30`, `&40`,
    /// `&50`, `&A0`, `&B0`, `&28`).
    fn open_channel_six(chip: &mut Ym2612) {
        for slot in 0..4u8 {
            let offset = slot * 4 + 2;
            write_reg(chip, ADDR_2, DATA_2, 0x30 + offset, 0x01);
            write_reg(chip, ADDR_2, DATA_2, 0x40 + offset, 0x00);
            write_reg(chip, ADDR_2, DATA_2, 0x50 + offset, 0x1F);
        }
        write_reg(chip, ADDR_2, DATA_2, 0xA6, 0x22);
        write_reg(chip, ADDR_2, DATA_2, 0xA2, 0x10);
        write_reg(chip, ADDR_2, DATA_2, 0xB2, 0x07);
        write_reg(chip, ADDR_1, DATA_1, REG_KEY_ON, KEY_ON_ALL | 6);
    }

    /// The next `count` samples of `chip`, as the `(left, right)` pairs came out.
    fn samples(chip: &mut Ym2612, count: u32) -> Vec<(i16, i16)> {
        (0..count).map(|_| chip.clock()).collect()
    }

    /// `&2B` bit 7 replaces the FM of channel 6 in the mix: what reaches the 9-bit
    /// DAC of M5-07b (#112) is `&2A` and nothing the channel's operators are
    /// still doing (`&2A`, `&2B`, M5-11 #116).
    #[test]
    fn dac_replaces_ch6() {
        let mut chip = Ym2612::new();
        open_channel_six(&mut chip);

        // FM, and channel 6 alone is loud enough to see and moving enough to
        // tell apart from a level.
        let fm = samples(&mut chip, 64);
        assert!(
            fm.windows(2).any(|pair| pair[0] != pair[1]),
            "the FM of channel 6 is not on the pin: {fm:?}"
        );
        assert!(
            fm.iter().any(|(left, _)| left.abs() > 100),
            "and the patch is not quiet either: {fm:?}"
        );

        // DAC mode at the centre of `&2A`: channel 6 is a sum of 0 now, so the
        // pin drops to the DC level of six silent converters and stays there.
        set_dac(&mut chip, 0x80, true);
        for (left, right) in samples(&mut chip, 16) {
            assert_eq!(
                (left, right),
                (6 * 4, 6 * 4),
                "the ladder of #112, and no wave"
            );
        }

        // And the 8-bit sample is on the pin through the DAC of #112: `&2A` 0xFF
        // is the 9-bit code 254 and 0x00 the code -256, each with the step of
        // ladder its own sign buys it and the +4 of the five silent channels.
        for (data, want) in [(0xFFu8, 5 * 4 + 254 + 4), (0x00, 5 * 4 - 256 - 3)] {
            write_reg(&mut chip, ADDR_1, DATA_1, REG_DAC_DATA, data);
            let want = i16::try_from(want).expect("one 9-bit code and its ladder step");
            assert_eq!(chip.clock(), (want, want), "`&2A` {data:#04X}");
        }
    }

    /// `&2A` is an unsigned 8-bit sample whose centre is 0x80: `((data - 128) << 6)`
    /// of the 14-bit channel domain, which is the 9-bit code `(data - 128) << 1`
    /// of #112 - the bottom bit of that code is not wired to `&2A` at all, so a
    /// PCM sample only ever reaches the even codes and the whole range of the
    /// converter but its top code (YM2608 application manual `&2A`; plutiedev,
    /// "DAC"; and the `(write_data ^ 0x80) << 1` of Nuked-OPN2's `ym3438.c`, read
    /// 12 Nov 2026, which is the same number on the 9-bit code).
    #[test]
    fn dac_value_0x80_is_center() {
        for model in [ChipModel::Ym2612, ChipModel::Ym3438] {
            let mut chip = Ym2612::new();
            chip.set_model(model);
            // Nothing else running: channel 6 is the only one with anything to
            // say, and DAC mode has taken the whole of it over.
            set_dac(&mut chip, 0x80, true);

            let centre = if model == ChipModel::Ym2612 { 24 } else { 0 };
            assert_eq!(
                chip.clock(),
                (centre, centre),
                "{model:?}: 0x80 is as silent as a channel can be"
            );

            let ladder = if model == ChipModel::Ym2612 { 4 } else { 0 };
            for data in 0..=255u8 {
                write_reg(&mut chip, ADDR_1, DATA_1, REG_DAC_DATA, data);
                let code = (i32::from(data) - 128) * 2;
                let offset = if model != ChipModel::Ym2612 {
                    0
                } else if code < 0 {
                    -3
                } else {
                    4
                };
                let want = i16::try_from(5 * ladder + code + offset)
                    .expect("five silent channels and one code");
                assert_eq!(chip.clock(), (want, want), "{model:?}, `&2A` {data:#04X}");
            }
        }
    }

    /// `&2B` bit 7 clear is FM again, at the sample the disable lands on rather
    /// than from the start of the wave: the chip walks all 24 slots whatever the
    /// mix is listening to, so the operators of channel 6 keep their phase and
    /// their envelope while the DAC is in front of them (`&2B`, M5-11 #116).
    #[test]
    fn dac_disable_restores_fm() {
        let mut chip = Ym2612::new();
        open_channel_six(&mut chip);
        clock_for(&mut chip, 32);

        set_dac(&mut chip, 0x40, true);
        let during = samples(&mut chip, 40);
        set_dac(&mut chip, 0x40, false);

        // The same patch on a chip that was never in DAC mode, run to the same
        // point: 32 samples before and 40 during is 72 by the disable.
        let mut plain = Ym2612::new();
        open_channel_six(&mut plain);
        clock_for(&mut plain, 32 + 40);

        let after = samples(&mut chip, 64);
        let plain_after = samples(&mut plain, 64);
        // `&2A` 0x40 is the code (64 - 128) << 1 = -128, so -128 - 3 of ladder,
        // plus the +4 of each of the five silent channels: one level, held for the
        // whole 40 samples.
        assert!(
            during.iter().all(|pair| *pair == (-111, -111)),
            "`&2A` 0x40 is a level while the DAC holds: {during:?}"
        );
        assert!(
            after.windows(2).any(|pair| pair[0] != pair[1]),
            "the FM is on the pin again: {after:?}"
        );
        assert_eq!(
            after, plain_after,
            "and it is the FM of a channel 6 that never stopped"
        );
    }

    /// Bit 7 of `&B4`-`&B6` is the left and bit 6 the right, and a channel routed
    /// to one side only leaves the other side nothing but its step of ladder: the
    /// six channels of [`all_six_channels_open`] are patched identically, so on a
    /// discrete chip the routed side of every sample is six of one code and the
    /// un-routed side six of one ladder step (`&B4`, M5-11 #116).
    #[test]
    fn pan_left_only() {
        for model in [ChipModel::Ym2612, ChipModel::Ym3438] {
            let mut chip = Ym2612::new();
            chip.set_model(model);
            all_six_channels_open(&mut chip);
            pan_all(&mut chip, PAN_LEFT);
            let panned = samples(&mut chip, 256);

            // The same patch, routed to both sides, for the difference to be seen
            // against.
            let mut both = Ym2612::new();
            both.set_model(model);
            all_six_channels_open(&mut both);
            let routed = samples(&mut both, 256);

            let left = panned.iter().map(|(left, _)| *left).collect::<Vec<_>>();
            let right = panned.iter().map(|(_, right)| *right).collect::<Vec<_>>();
            let loud = left.iter().any(|sample| sample.abs() > 1000);
            assert!(
                loud,
                "{model:?}: panning is not supposed to mute the side it routes to: {left:?}"
            );
            assert_eq!(
                left,
                routed.iter().map(|(left, _)| *left).collect::<Vec<_>>(),
                "{model:?}: the routed side is the whole mix it always was"
            );

            if model == ChipModel::Ym3438 {
                assert!(
                    right.iter().all(|sample| *sample == 0),
                    "a 3438 has nothing to leave on an un-routed side: {right:?}"
                );
            } else {
                // Six channels at their ladder step of +4 or -4, and never a code:
                // -24 is six channels with a negative sum panned off, which is the
                // 4 of a muted channel rather than the 3 of a routed one.
                assert!(
                    right.iter().all(|sample| *sample == 24 || *sample == -24),
                    "the right side of a discrete chip is ladder only: {right:?}"
                );
                assert!(
                    right.iter().any(|sample| *sample == -24),
                    "and a panned-off channel with a negative sum is at -4: {right:?}"
                );
            }
        }
    }

    /// Nothing on either bit is nothing on either side: the chip goes quiet on a
    /// YM3438, and a discrete YM2612 keeps only the six converters' own steps -
    /// the `+4` and the `-4` of M5-07b (#112), never a code, so no wave.
    ///
    /// The `-4` of a channel panned off with a negative sum, rather than the `-3`
    /// of a routed one, is the `4 - pan` of the ladder this crate took from
    /// Genesis Plus GX in #112; Nuked-OPN2's `OPN2_ChOutput` puts the un-routed
    /// side of a channel at the sign of that channel's own value too, where ymfm
    /// leaves it at the discontinuity of a zero (`dac_discontinuity(0)`). Read
    /// 12 Nov 2026.
    #[test]
    fn pan_none_silences_channel() {
        for model in [ChipModel::Ym2612, ChipModel::Ym3438] {
            let mut chip = Ym2612::new();
            chip.set_model(model);
            all_six_channels_open(&mut chip);
            pan_all(&mut chip, 0);
            let panned = samples(&mut chip, 256);

            if model == ChipModel::Ym3438 {
                assert!(
                    panned.iter().all(|&(left, right)| left == 0 && right == 0),
                    "{model:?}: a channel routed nowhere is silent: {panned:?}"
                );
            } else {
                assert!(
                    panned
                        .iter()
                        .all(|&(left, right)| left == right && (left == 24 || left == -24)),
                    "{model:?}: only the six ladder steps survive: {panned:?}"
                );
            }
        }
    }

    /// The PCM of channel 6 is routed by `&B6` like any other channel's sum, which
    /// it is by the time the routing happens: routing it to the left alone puts
    /// the sample on the left and leaves the right with the channel's own step of
    /// ladder (`&B4`, `&2A`, M5-11 #116).
    #[test]
    fn dac_output_is_panned_like_any_channel() {
        let mut chip = Ym2612::new();
        set_dac(&mut chip, 0xFF, true);
        write_reg(&mut chip, ADDR_2, DATA_2, 0xB6, PAN_LEFT);
        // 0xFF is a channel sum above zero, so the un-routed right keeps the +4 of
        // a panned-off channel and the left the code of 254 with its own +4.
        assert_eq!(chip.clock(), (5 * 4 + 254 + 4, 5 * 4 + 4));

        let mut asic = Ym2612::new();
        asic.set_model(ChipModel::Ym3438);
        set_dac(&mut asic, 0xFF, true);
        write_reg(&mut asic, ADDR_2, DATA_2, 0xB6, PAN_RIGHT);
        assert_eq!(asic.clock(), (0, 254));
    }

    /// The routing a chip answers with before anything has written `&B4`-`&B6` is
    /// both sides, not silence: the register file starts every channel on L+R the
    /// way the chip's own reset does, so the whole of the existing FM model is
    /// still on the pin of a chip that was never told to pan (`&B4`, and the
    /// `pan_l[i] = pan_r[i] = 1` of Nuked-OPN2's `YM3438_reset`, read 12 Nov 2026).
    #[test]
    fn panning_defaults_to_both_sides() {
        let mut chip = Ym2612::new();
        all_six_channels_open(&mut chip);

        assert_eq!(chip.regs.read(PART_I, REG_AMS_PMS), PAN_LEFT | PAN_RIGHT);
        assert_eq!(chip.regs.read(PART_II, 0xB6), PAN_LEFT | PAN_RIGHT);

        for (left, right) in samples(&mut chip, 64) {
            assert_eq!(left, right, "and the two sides are still the same mix");
        }
    }
}
