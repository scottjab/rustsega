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
//! state of a write-only chip. Of the global registers the timer group of
//! [`RegFile::timer_config`], the key-on of [`REG_KEY_ON`] and the LFO of
//! [`REG_LFO`] and [`REG_AMS_PMS`] and the channel 6 DAC of [`RegFile::dac_data`]
//! and [`RegFile::dac_enabled`] mean something now.

use crate::phase::{SLOT_S1, SLOT_S2, SLOT_S3};
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

/// Channels per part: a part carries three of the six, so a channel number is
/// only meaningful with its part (YM2608 application manual "3. Register
/// descriptions": part I is slots 1 to 3, part II slots 4 to 6).
pub(crate) const CHANNELS_PER_PART: usize = 3;

/// Register `&22`, the LFO: bit 3 the AM/PM enable and bits 2-0 the frequency
/// (YM2608 application manual "3. Register descriptions", `&22`). A part I
/// register like the timers: the LFO is one per chip.
pub(crate) const REG_LFO: u8 = 0x22;

/// Register `&28`, key on/off: bits 7-4 the operators S4, S3, S2, S1 and bits
/// 2-0 the channel, 0-2 the part I channels and 4-6 the part II ones.
///
/// The channel field is an absolute channel number rather than a part-relative
/// one, so `&28` of either part reaches the same operators (YM2608 application
/// manual `&28`).
pub(crate) const REG_KEY_ON: u8 = 0x28;

/// Register `&30`, the first of the detune and multiple group: one register per
/// operator, at `&30` + slot offset + channel, so a part's three channels fill
/// `&30`-`&3E` and `&33`, `&37`, `&3B` and `&3F` - the fourth channel of each
/// slot - are not decoded (YM2608 application manual `&30`).
pub(crate) const REG_DT_MUL: u8 = 0x30;

/// Register `&40`, the first of the total level group: one per operator, at
/// `&40` + slot offset + channel (YM2608 application manual `&40`).
pub(crate) const REG_TOTAL_LEVEL: u8 = 0x40;

/// Register `&50`, the first of the key scaling and attack rate group: bits 7-6
/// the rate scaling and bits 4-0 the attack rate (YM2608 application manual
/// `&50`).
pub(crate) const REG_KEY_SCALE_ATTACK: u8 = 0x50;

/// Register `&60`, the first of the AM and decay 1 rate group: bit 7 the
/// amplitude modulation of M5-08 (#113) and bits 4-0 the decay 1 rate (YM2608
/// application manual `&60`).
pub(crate) const REG_AM_DECAY_1: u8 = 0x60;

/// Register `&70`, the first of the decay 2 rate group: bits 4-0 the rate the
/// envelope decays at once it is sustaining (YM2608 application manual `&70`).
pub(crate) const REG_DECAY_2: u8 = 0x70;

/// Register `&80`, the first of the sustain level and release rate group: bits
/// 7-4 the sustain level and bits 3-0 the release rate (YM2608 application
/// manual `&80`).
pub(crate) const REG_LEVEL_RELEASE: u8 = 0x80;

/// Register `&90`, the first of the SSG-EG group: bits 3-0 of one operator's
/// SSG-EG mode - bit 3 the enable, bit 2 the attack inversion, bit 1 the
/// alternate and bit 0 the hold - and one register per operator like `&40` to
/// `&80` (YM2608 application manual "3. Register descriptions", `&90`, and see
/// `crate::envelope`).
pub(crate) const REG_SSG_EG: u8 = 0x90;

/// Register `&A0`, the first of the F-Num low group: `&A0` + channel, bits 7-0
/// of the 11-bit F-Num. This is also the write that applies a channel's pending
/// `&A4` (YM2608 application manual `&A0`).
pub(crate) const REG_FNUM_LOW: u8 = 0xA0;

/// Register `&A4`, the first of the block and F-Num high group: `&A4` +
/// channel, block in bits 5-3 and F-Num 10-8 in bits 2-0. `&A8`-`&AE` are the
/// channel 3 special mode of M5-09 (#114), not another channel (YM2608
/// application manual `&A4`).
pub(crate) const REG_BLOCK_FNUM: u8 = 0xA4;

/// Register `&A8`, the first of the channel 3 special-mode F-Num low group of
/// M5-09 (#114): `&A8`, `&A9` and `&AA` are the low bytes of operators 3, 1 and 2
/// of channel 3, and the chip takes them in groups of four addresses like the
/// `&A0` group does - which is why `&AB` and `&AF`, the fourth of each, are not
/// decoded and `&A3`/`&A7` are not registers of anything (YM2608 application
/// manual `&A8`, and see [`ch3_frequency_of`]).
pub(crate) const REG_CH3_FNUM_LOW: u8 = 0xA8;

/// Register `&AC`, the block and F-Num high of the channel 3 special-mode group:
/// `&AC`, `&AD` and `&AE` are the three addresses of one latch, the `&A4` of this
/// group, which any of `&A8`-`&AA` applies (YM2608 application manual `&AC`).
pub(crate) const REG_CH3_BLOCK_FNUM: u8 = 0xAC;

/// Register `&B0`, the first of the algorithm and feedback group: `&B0` +
/// channel, bits 5-3 the feedback of operator 1 and bits 2-0 the connection mode
/// of the channel's four operators. One register per channel rather than one per
/// operator, so `&B0`-`&B2` are decoded and `&B3` is not - and `&B4` to `&B6` are
/// the [next channel group](`REG_AMS_PMS`), not more of this one (YM2608
/// application manual "3. Register descriptions", `&B0`).
pub(crate) const REG_ALGORITHM_FEEDBACK: u8 = 0xB0;

/// Register `&B4`, the first of the amplitude and phase modulation group: `&B4` +
/// channel, bits 5-4 the AMS - how far down the LFO's AM triangle that channel's
/// amplitude-modulated operators are read at - and bits 2-0 the PMS, its PM depth.
/// Bits 7-6 are the two output enables of the channel, [`PAN_LEFT`] and
/// [`PAN_RIGHT`], which reach the DACs of [`crate::output`] through
/// [`RegFile::pan`] (M5-11, #116). Like [`REG_ALGORITHM_FEEDBACK`], three of a part
/// are decoded and the fourth, `&B7`, is not (YM2608 application manual "3.
/// Register descriptions", `&B4`; see `crate::lfo`).
pub(crate) const REG_AMS_PMS: u8 = 0xB4;

/// `&B4`-`&B6` bit 7: this channel's converter is routed to the left output.
pub(crate) const PAN_LEFT: u8 = 0x80;

/// `&B4`-`&B6` bit 6: this channel's converter is routed to the right output.
pub(crate) const PAN_RIGHT: u8 = 0x40;

/// Both enables together, the routing a channel starts on.
///
/// The two bits together are what a write-only register cannot say, since a chip
/// wakes with undefined registers and the register file wakes zeroed: every
/// address of the [`REG_AMS_PMS`] group is therefore initialised to this, so an
/// untouched chip puts all six channels on both sides. Nuked-OPN2's `YM3438_reset`
/// does the same for its `pan_l`/`pan_r` (read 12 Nov 2026) and it is what the
/// drivers of a Genesis assume: FM is mono on the board, so a game that never
/// writes `&B4` is not a game that expects silence.
pub(crate) const PAN_BOTH: u8 = PAN_LEFT | PAN_RIGHT;

/// Registers in one per-operator group: the four slot offsets of a channel
/// group, `&30` to `&3F` for the detune and multiple of [`REG_DT_MUL`].
const OPERATOR_GROUP_WIDTH: u8 = 0x10;

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

/// Register `&2A`, the 8-bit sample of the channel 6 PCM DAC: one write per
/// sample, unsigned, with `&2B` bit 7 deciding whether channel 6 answers with it
/// at all (YM2608 application manual "3. Register descriptions", `&2A`; and
/// plutiedev, "YM2612 DAC", for the 0x80 centre that [`crate::output::pcm_sample`]
/// applies). Part I like [`REG_TIMER_A_LOW`] and [`REG_LFO`]: the DAC is one per
/// chip, and part II does not decode this address at all.
///
/// `&2C` bit 3, the PCM test bit that hardware wires to the bottom bit of the
/// same converter, is not modelled.
pub(crate) const REG_DAC_DATA: u8 = 0x2A;

/// Register `&2B`, the DAC control: bit 7 the enable that replaces the FM of
/// channel 6 with [`REG_DAC_DATA`], the other bits the test mode of a YM2612 that
/// nothing here models (YM2608 application manual "3. Register descriptions",
/// `&2B`).
pub(crate) const REG_DAC_ENABLE: u8 = 0x2B;

/// `&2B` bit 7: 1 puts [`REG_DAC_DATA`] in place of the FM of channel 6.
pub(crate) const DAC_ENABLE_BIT: u8 = 0x80;

/// Shift from `&27`'s bits 7-6, the channel 3 mode of M5-09 (#114), to their value.
pub(crate) const CH3_MODE_SHIFT: u8 = 6;

/// `&27` 00: channel 3 like any other channel, one F-Num for its four operators.
pub(crate) const CH3_MODE_NORMAL: u8 = 0;

/// `&27` 10: CSM, the special frequency mode with a key-on of all four operators
/// of channel 3 at every timer A overflow (`crate::timers`, and M5-09 #114).
///
/// The other three values of the field get no constant of their own, because none of
/// them is a decision: 01 is the special frequency mode, where operators 1 to 3 of
/// channel 3 take their F-Nums from `&A8`-`&AE` and operator 4 keeps
/// `&A2`/`&A6` ([`ch3_frequency_of`], [`crate::phase::Special`]), and 11 - what the
/// register lists call "CSM and frequency mode" - is 01 as far as this model goes.
/// Anything but [`CH3_MODE_NORMAL`] has the extra F-Nums; only this value has the
/// key-on.
pub(crate) const CH3_MODE_CSM: u8 = 2;

/// The `(channel, operator)` a per-operator register number of `part` addresses.
///
/// The number is `group + slot offset + channel`: the detune and multiple group
/// of [`REG_DT_MUL`], the slot offset in bits 3-2 of the low nibble in the chip's
/// S1, S3, S2, S4 order (see `crate::phase::SLOT_S1`), and the channel within the
/// part in bits 1-0, which the part carries over into the global channel number
/// (YM2608 application manual "3. Register descriptions").
///
/// The detune group only, that is: [`operator_of_group`] is the same decode at
/// another `first`. Either way `None` covers `&33`, `&37`, `&3B` and `&3F`, the
/// undecoded fourth channel of each slot.
pub(crate) fn operator_of(part: usize, register: u8) -> Option<(usize, usize)> {
    operator_of_group(part, REG_DT_MUL, register)
}

/// The `(channel, operator)` a register of the per-operator group starting at
/// `first` addresses, the same `first` + slot offset + channel layout as the
/// detune group of [`REG_DT_MUL`]: the five envelope groups `&40`, `&50`, `&60`,
/// `&70` and `&80` are laid out exactly like it (YM2608 application manual "3.
/// Register descriptions").
pub(crate) fn operator_of_group(part: usize, first: u8, register: u8) -> Option<(usize, usize)> {
    let offset = register.checked_sub(first)?;
    if offset >= OPERATOR_GROUP_WIDTH {
        return None;
    }
    let slot = usize::from((offset & 0x0C) >> 2);
    Some((channel_of(part, offset & 0x03)?, slot))
}

/// The channel of `part` that a channel-level register addresses: `first` +
/// channel, with the part's fourth address - `&A3`, `&A7` and everything above
/// `&A8` - not decoded (YM2608 application manual `&A0`, `&A4`). `&A8` to `&AE`
/// of part I are the channel 3 registers of [`ch3_frequency_of`], not a third and
/// fourth channel of these two groups.
pub(crate) fn channel_of_register(part: usize, first: u8, register: u8) -> Option<usize> {
    channel_of(part, register.checked_sub(first)?)
}

/// The channel 3 operator that `register` of the special-mode group addresses, and
/// whether it is the F-Num low or the block and F-Num high half of that operator's
/// pair: `&A8`/`&AC` is operator 3, `&A9`/`&AD` operator 1 and `&AA`/`&AE`
/// operator 2, while operator 4 has no pair of its own and keeps the `&A2`/`&A6` of
/// the channel itself. `&AB` and `&AF`, the fourth address of each half, are not
/// decoded, and neither is anything of part II (YM2608 application manual `&A8`;
/// M5-09 #114).
///
/// The decode is the chip's own: it takes the channel number from the low two bits
/// of the address and the half from `address & 0xFC`, which is what folds `&A8`,
/// `&A9` and `&AA` onto one register number and `&AC`, `&AD` and `&AE` onto the
/// next. One consequence worth naming is that the three high addresses are one
/// latch, not three: `&AD` is a way of writing the `&AC` of operator 3 as much as
/// of operator 1, exactly as one `&A4` latches for all of `&A0`-`&A2`.
pub(crate) fn ch3_frequency_of(part: usize, register: u8) -> Option<(usize, bool)> {
    if part != PART_I {
        return None;
    }
    let slot = match register & 0x03 {
        0 => SLOT_S3,
        1 => SLOT_S1,
        2 => SLOT_S2,
        _ => return None,
    };
    let high = match register & 0xFC {
        REG_CH3_FNUM_LOW => false,
        REG_CH3_BLOCK_FNUM => true,
        _ => return None,
    };
    Some((slot, high))
}

/// The global channel number of a part's channel 0-2: part I is channels 0-2 and
/// part II channels 3-5.
fn channel_of(part: usize, channel: u8) -> Option<usize> {
    if channel >= CHANNELS_PER_PART as u8 {
        return None;
    }
    Some(part * CHANNELS_PER_PART + usize::from(channel))
}

/// The register file of one YM2612: two banks and the address each of the two
/// address ports last latched.
#[derive(Clone, Debug)]
pub(crate) struct RegFile {
    /// `banks[PART_I]` and `banks[PART_II]`, indexed by register number. Zeroed
    /// apart from the exception of [`RegFile::new`], which is not what a real chip
    /// powers up with - a YM2612 wakes with undefined registers - but is the only
    /// defined thing this model has.
    banks: [[u8; REGISTERS]; PARTS],
    /// One latch per part: `&27`-style register numbers written to one part's
    /// address port do not move the other part's.
    latched: [u8; PARTS],
}

impl RegFile {
    /// An all-zero register file with both parts latched on register `0x00`, and
    /// the [`REG_AMS_PMS`] group of both parts at [`PAN_BOTH`]: a chip that has
    /// never been told to pan has all six channels on both sides, which is the one
    /// register value this model has to start away from zero because a zeroed
    /// `&B4` would mean a chip that hears nothing at all (see [`PAN_BOTH`]).
    pub(crate) fn new() -> Self {
        let mut banks = [[0; REGISTERS]; PARTS];
        for part in 0..PARTS {
            for channel in 0..CHANNELS_PER_PART {
                banks[part][usize::from(REG_AMS_PMS) + channel] = PAN_BOTH;
            }
        }
        Self {
            banks,
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

    /// The channel 3 mode, `&27` bits 7-6 as one number: [`CH3_MODE_NORMAL`],
    /// [`CH3_MODE_SPECIAL`] or [`CH3_MODE_CSM`]. Part I only, like the timers
    /// beside it (YM2608 application manual `&27`, and M5-09 #114).
    ///
    /// A mode of 3 is the special frequency mode as far as this model is
    /// concerned: it is not [`CH3_MODE_CSM`], so the CSM key-on of
    /// [`crate::Ym2612`] does not run in it even though the register lists name
    /// that value "CSM and frequency mode".
    pub(crate) fn ch3_mode(&self) -> u8 {
        self.read(PART_I, REG_CONTROL) >> CH3_MODE_SHIFT
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

    /// Whether channel 6 is a PCM DAC right now: `&2B` bit 7, part I like the rest
    /// of the globals (`&2B`).
    pub(crate) fn dac_enabled(&self) -> bool {
        self.read(PART_I, REG_DAC_ENABLE) & DAC_ENABLE_BIT != 0
    }

    /// The 8-bit PCM sample of `&2A` as written, unsigned and centred on 0x80:
    /// [`crate::output::pcm_sample`] is what turns it into a channel sum
    /// (YM2608 application manual `&2A`).
    pub(crate) fn dac_data(&self) -> u8 {
        self.read(PART_I, REG_DAC_DATA)
    }

    /// The routing of channel `channel`: [`PAN_LEFT`] and [`PAN_RIGHT`] as the
    /// `&B4` + channel of its part were last written, so a channel of part II is
    /// read out of part II's bank and channel 6 out of `&B6` (YM2608 application
    /// manual `&B4`, and M5-11 #116).
    pub(crate) fn pan(&self, channel: usize) -> u8 {
        let part = channel / CHANNELS_PER_PART;
        let offset = channel % CHANNELS_PER_PART;
        self.read(part, REG_AMS_PMS + offset as u8) & PAN_BOTH
    }
}
