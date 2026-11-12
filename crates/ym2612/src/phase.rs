//! The phase generator: how far each operator has got through its waveform.
//!
//! Four registers decide it. `&30` gives one operator its detune and multiple,
//! `&A0` and `&A4` give a channel its F-Num and block, and `&28` keys the four
//! operators of a channel on and off. From those the chip works out a 17-bit
//! phase increment and adds it to a 20-bit phase counter once per output sample;
//! the operator then reads its waveform at the top ten bits of that counter
//! (YM2608 application manual "3. Register descriptions", `&30`, `&A0`, `&A4`
//! and `&28`, with the manual's own key scale and detune tables).
//!
//! Only the counting is here: the envelope that decides whether an operator is
//! heard at all is M5-05 (#109). So a channel holds one F-Num and one block,
//! shared by its four operators - except for channel 3 in one of its special modes,
//! the M5-09 (#114) half of this module, where [`Special`] gives operators 1 to 3
//! F-Nums of their own and [`PhaseGen::frequency`] is what picks between the two.
//! The LFO of M5-08 (#113) is the one
//! exception to "only the counting": it reaches [`PhaseGen::tick`] as a PMS per
//! channel and a phase, and [`increment`] is where its offset is added.
//!
//! The frequency path is pure - [`keycode`] and [`increment`] - and is tested
//! as such. [`PhaseGen`] holds the state the writes leave behind: the F-Num and
//! block of each channel and their latch, and each operator's detune, multiple,
//! key state and phase counter.

use crate::regs;

// Serialising this chip state for save states (M6-07a, #126). Gated on the
// crate's `serde` feature so `cargo build --no-default-features` keeps the
// core with no dependencies at all, as docs/ARCHITECTURE.md asks of it.
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Channels on the chip: three in part I and three in part II.
pub(crate) const CHANNELS: usize = 6;

/// Operators per channel, the chip's S1 to S4.
pub(crate) const SLOTS: usize = 4;

/// The F-Num is 11 bits (YM2608 application manual `&A0`, `&A4`).
const FNUM_MASK: u16 = 0x07FF;

/// The doubled F-Num the LFO's phase modulation is added to is 12 bits, and the
/// adder wraps there rather than saturating (YM2608 application manual `&B4` and
/// [`increment`]).
const FNUM_LFO_MASK: i32 = 0x0FFF;

/// Width of the phase increment before the multiple: 17 bits, so a detune below
/// zero comes back at the top. See [`detune`].
const INCREMENT_MASK: u32 = 0x0001_FFFF;

/// Width of the phase counter: 20 bits, of which the top ten are the waveform
/// address the operator reads.
const PHASE_MASK: u32 = 0x000F_FFFF;

/// Operator index of the slot offset `&00`: operator 1.
///
/// The four indices follow the register offsets of a channel - `&30`, `&34`,
/// `&38` and `&3C` - which run S1, S3, S2, S4 rather than S1 to S4 (YM2608
/// application manual "3. Register descriptions"). Operator indexing, in
/// [`KEY_ON_BITS`] and in [`Channel`], uses this order.
pub(crate) const SLOT_S1: usize = 0;
/// Operator index of the slot offset `&04`: operator 3.
pub(crate) const SLOT_S3: usize = 1;
/// Operator index of the slot offset `&08`: operator 2.
pub(crate) const SLOT_S2: usize = 2;
/// Operator index of the slot offset `&0C`: operator 4.
pub(crate) const SLOT_S4: usize = 3;

/// The detune table, `[FD][key code]`: FD 0 is all zeros and the other three
/// rows rise with the key code (YM2608 application manual detune table, the same
/// 4x32 table MAME reads). `&30` bit 2 is the sign, so a row is subtracted
/// rather than added when it is set.
const DETUNE: [[u8; 32]; 4] = [
    [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    [
        0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8,
        8, 8,
    ],
    [
        1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14,
        16, 16, 16, 16,
    ],
    [
        2, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19,
        20, 22, 22, 22, 22,
    ],
];

/// The `&28` bit mask of each operator: bits 7-4 are S4, S3, S2 and S1 in that
/// order, which by the slot offsets above are the indices 3, 1, 2 and 0 (YM2608
/// application manual `&28`).
pub(crate) const KEY_ON_BITS: [(u8, usize); SLOTS] = [
    (0x10, SLOT_S1),
    (0x20, SLOT_S2),
    (0x40, SLOT_S3),
    (0x80, SLOT_S4),
];

/// The 5-bit key code of an F-Num and block: the coarse frequency class that
/// the detune table - and the key-scale attenuation of M5-05 (#109) - are read
/// at.
///
/// `kc = (BLK << 2) | (FN10 << 1) | N3`, with
/// `N3 = (FN10 & (FN9 | FN8 | FN7)) | (!FN10 & FN9 & FN8 & FN7)`
/// (YM2608 application manual, key scale table; FN10..FN7 are F-Num bits
/// 10..7). The carry term is what keeps the key code rising in step with the
/// frequency: at F-Num 0x380-0x3FF the top bit is still clear but the four high
/// bits have carried into it, so the key code takes the 1 rather than the 0 its
/// top two bits alone would give it.
pub(crate) fn keycode(fnum: u16, block: u8) -> u8 {
    let fnum = fnum & FNUM_MASK;
    let fn10 = fnum & 0x400 != 0;
    let fn9 = fnum & 0x200 != 0;
    let fn8 = fnum & 0x100 != 0;
    let fn7 = fnum & 0x080 != 0;
    let n3 = (fn10 & (fn9 | fn8 | fn7)) | (!fn10 & fn9 & fn8 & fn7);

    ((block & 0x07) << 2) | (u8::from(fn10) << 1) | u8::from(n3)
}

/// The phase increment of an operator, per output sample, 20 bits.
///
/// The base is `(F-Num << Block) >> 1`, a 17-bit value; the detune offset of
/// `dt` is applied to that at the key code, in 17 bits; and the multiple
/// finishes it, where multiple 0 is x0.5 (YM2608 application manual `&30`,
/// `&A0`, `&A4`). Detuning comes before the multiple, so detuning a channel
/// with a multiple on moves the frequency by `detune * multiple`.
///
/// `pm` is the LFO's phase modulation of M5-08 (#113): the signed offset of
/// [`crate::lfo::pm_offset`], in halves of an F-Num, and 0 for a channel with no
/// PMS. The F-Num reaches this path doubled, which is what makes the
/// `(F-Num << Block) >> 1` above the same shift the chip makes from its 12-bit
/// F-Num, and `pm` is added there and masked back to those 12 bits, so the
/// offset reaches the bottom of the increment rather than only whole F-Nums
/// (Nuked-OPN2's `OPN2_PhaseCalcIncrement`, and ymfm's `compute_phase_step`
/// agrees on the doubling, the offset and the mask).
///
/// What the offset does not reach is the key code: the detune row and the key
/// scaling of the envelope are still read at the key code of the F-Num that was
/// written, so PM is pitch and nothing else (YM2608 application manual `&B4`).
pub(crate) fn increment(fnum: u16, block: u8, dt: u8, mul: u8, pm: i32) -> u32 {
    let written = fnum & FNUM_MASK;
    let offset = ((i32::from(written) << 1) + pm) & FNUM_LFO_MASK;
    let base = ((offset as u32) << u32::from(block & 0x07)) >> 2;
    let detuned = detune(base, dt, keycode(written, block));
    let mul = u32::from(mul & 0x0F);
    let scaled = if mul == 0 {
        detuned >> 1
    } else {
        detuned * mul
    };
    scaled & PHASE_MASK
}

/// `inc` plus or minus the detune offset of `dt` at key code `kc`, in 17 bits.
///
/// The wrap is hardware, not a slip: the increment has no room above bit 16, so
/// taking a detune below zero comes out as a large positive increment - the
/// "detune overflow", which a real chip has and drivers hit at DT 4-7 with a
/// small F-Num. Keep it rather than clamping to zero.
fn detune(inc: u32, dt: u8, kc: u8) -> u32 {
    let amount = u32::from(DETUNE[usize::from(dt & 0x03)][usize::from(kc & 0x1F)]);
    if dt & 0x04 == 0 {
        (inc + amount) & INCREMENT_MASK
    } else {
        inc.wrapping_sub(amount) & INCREMENT_MASK
    }
}

/// The channel a `&28` channel field selects.
///
/// Bits 2-0 are absolute channel numbers, not part-relative ones: 0-2 are the
/// part I channels and 4-6 the part II ones, and 3 and 7 - the fourth channel
/// of each part - are not decoded at all (YM2608 application manual `&28`).
///
/// Shared with [`crate::envelope`], which reads the same bit of `&28` for its
/// state machine rather than for the phase counter.
pub(crate) fn key_on_channel(field: u8) -> Option<usize> {
    if field & 0x03 == 3 {
        return None;
    }
    Some(usize::from(if field < 3 { field } else { field - 1 }))
}

/// One operator's phase state: the per-operator half of a channel, which is what
/// a write to `&30` and `&28` leaves behind.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Operator {
    /// `&30` bits 6-4, the detune.
    pub(crate) dt: u8,
    /// `&30` bits 3-0, the multiple, where 0 is x0.5.
    pub(crate) mul: u8,
    /// The `&28` bit of this operator.
    pub(crate) key_on: bool,
    /// The 20-bit phase counter; the waveform address is its top ten bits.
    pub(crate) phase: u32,
}

/// One channel: the F-Num and block its four operators share, the `&A4` latch
/// waiting for the next `&A0` write, and the operators themselves. Channel 3 in a
/// special mode is the exception to the shared half, and reads [`Special`] for
/// three of its operators instead (M5-09, #114).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Channel {
    /// The F-Num in force, 11 bits: `&A4` bits 2-0 over `&A0` bits 7-0.
    pub(crate) fnum: u16,
    /// The block in force, 3 bits: `&A4` bits 5-3.
    pub(crate) block: u8,
    /// `&A4` bits 2-0 as of the last `&A4` write, not yet applied.
    latched_fnum: u16,
    /// `&A4` bits 5-3 as of the last `&A4` write, not yet applied.
    latched_block: u8,
    /// Indexed by the slot order of [`SLOT_S1`].
    pub(crate) operators: [Operator; SLOTS],
}

impl Channel {
    /// A channel nothing has written to: F-Num and block 0, every operator off
    /// and every phase counter at 0.
    fn new() -> Self {
        Self {
            fnum: 0,
            block: 0,
            latched_fnum: 0,
            latched_block: 0,
            operators: [Operator {
                dt: 0,
                mul: 0,
                key_on: false,
                phase: 0,
            }; SLOTS],
        }
    }
}

/// Channel index of channel 3, the one with the special modes of M5-09 (#114):
/// part I's third channel, `&A2`/`&A6` of its operator 4 and `&A8`-`&AE` of the
/// other three.
pub(crate) const CHANNEL3: usize = 2;

/// The three extra frequency generators of channel 3, in the special frequency
/// mode and in CSM: the `&A8`-`&AE` registers of the chip (YM2608 application
/// manual `&A8`, and M5-09 #114).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Special {
    /// The F-Num of each operator, at the slot index of the operator it drives.
    /// [`SLOT_S4`] is neither written nor read: special mode leaves operator 4 on
    /// the channel's own `&A2`.
    fnum: [u16; SLOTS],
    /// The block of each operator, as [`Special::fnum`].
    block: [u8; SLOTS],
    /// `&AC` bits 2-0, waiting for one of `&A8`-`&AA` to apply it. One latch for
    /// the whole group, because the chip has one register there: `&AC`, `&AD` and
    /// `&AE` are the same write ([`regs::ch3_frequency_of`]).
    latched_fnum: u16,
    /// `&AC` bits 5-3, with [`Special::latched_fnum`].
    latched_block: u8,
}

impl Special {
    /// The state of the `&A8`-`&AE` group before anything wrote it: every operator
    /// at F-Num 0 and block 0, and the latch empty.
    fn new() -> Self {
        Self {
            fnum: [0; SLOTS],
            block: [0; SLOTS],
            latched_fnum: 0,
            latched_block: 0,
        }
    }

    /// A `&A8`-`&AE` write of `value` to operator `slot`: the half the `high` flag
    /// names. A high write fills the latch of the group and a low write applies
    /// it, the same order `&A4` and `&A0` make the channel F-Num take
    /// ([`regs::ch3_frequency_of`], and #108's rule for `&A0`).
    fn register_written(&mut self, slot: usize, high: bool, value: u8) {
        if high {
            self.latched_fnum = u16::from(value & 0x07);
            self.latched_block = (value >> 3) & 0x07;
        } else {
            self.fnum[slot] = (self.latched_fnum << 8) | u16::from(value);
            self.block[slot] = self.latched_block;
        }
    }
}

/// The phase counters of all 24 operators, and the register writes that drive
/// them.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct PhaseGen {
    /// `channels[0]` to `channels[2]` are part I's, `channels[3]` to
    /// `channels[5]` part II's.
    channels: [Channel; CHANNELS],
    /// `&27` bits 7-6 as [`regs::RegFile::ch3_mode`] gives them: whether channel 3
    /// is in one of the two modes that give it its own frequency generators.
    mode: u8,
    /// The `&A8`-`&AE` frequencies, in force for channel 3 while [`PhaseGen::mode`]
    /// is not [`regs::CH3_MODE_NORMAL`].
    ch3: Special,
}

impl PhaseGen {
    /// A generator with every channel at F-Num 0, block 0 and every operator
    /// off with its counter at 0.
    pub(crate) fn new() -> Self {
        Self {
            channels: [Channel::new(); CHANNELS],
            mode: regs::CH3_MODE_NORMAL,
            ch3: Special::new(),
        }
    }

    /// A data port write of `value` to `register` of `part`, for the registers
    /// the phase generator reads: `&27`'s channel 3 mode, `&28` key on/off, `&30`-
    /// `&3E` detune and multiple, `&A0`-`&A6` the F-Num and block, and `&A8`-`&AE`
    /// the three extra F-Nums of channel 3's special modes, decoded by
    /// [`regs::ch3_frequency_of`] for M5-09 (#114).
    ///
    /// `&28` is decoded from either part: the channel field of the write is
    /// already an absolute channel number, and both halves of the chip route
    /// that number to the same key-on register.
    pub(crate) fn register_written(&mut self, part: usize, register: u8, value: u8) {
        if register == regs::REG_KEY_ON {
            self.key_on(value);
        } else if let Some((channel, slot)) = regs::operator_of(part, register) {
            let operator = &mut self.channels[channel].operators[slot];
            operator.dt = (value >> 4) & 0x07;
            operator.mul = value & 0x0F;
        } else if let Some(channel) = regs::channel_of_register(part, regs::REG_FNUM_LOW, register)
        {
            let channel = &mut self.channels[channel];
            channel.fnum = (channel.latched_fnum << 8) | u16::from(value);
            channel.block = channel.latched_block;
        } else if let Some(channel) =
            regs::channel_of_register(part, regs::REG_BLOCK_FNUM, register)
        {
            let channel = &mut self.channels[channel];
            channel.latched_fnum = u16::from(value & 0x07);
            channel.latched_block = (value >> 3) & 0x07;
        } else if part == regs::PART_I && register == regs::REG_CONTROL {
            self.mode = value >> regs::CH3_MODE_SHIFT;
        } else if let Some((slot, high)) = regs::ch3_frequency_of(part, register) {
            self.ch3.register_written(slot, high, value);
        }
    }

    /// `&28`: bits 7-4 select the operators and bits 2-0 the channel.
    ///
    /// A 0 to 1 on an operator's bit resets its phase counter to 0, which is
    /// the whole of what key-on does until the envelope generator of M5-05
    /// (#109) gives the edge anything else to start. A key-on of an operator
    /// that is already on is not a reset (YM2608 application manual `&28`).
    ///
    /// INFERRED: `&28` is named KEY ON/OFF, so a write with an operator's bit
    /// clear is stored as that operator being off. What the envelope does with
    /// that edge is M5-05's; either way the phase counter keeps its value
    /// through a key-off, since only a key-on resets it.
    fn key_on(&mut self, value: u8) {
        let Some(channel) = key_on_channel(value & 0x07) else {
            return;
        };
        let channel = &mut self.channels[channel];
        for &(mask, slot) in &KEY_ON_BITS {
            let key_on = value & mask != 0;
            let operator = &mut channel.operators[slot];
            if key_on && !operator.key_on {
                operator.phase = 0;
            }
            operator.key_on = key_on;
        }
    }

    /// Advance every phase counter by its increment: one output sample.
    ///
    /// The increment is recomputed from the registers rather than cached, so a
    /// write to any of them takes effect on the very next sample.
    ///
    /// INFERRED: nothing gates the counter on the key state. The envelope of
    /// M5-05 (#109) is what decides whether an operator is heard, and an
    /// operator that has never been keyed on only has a phase nothing can see.
    ///
    /// `pms` is each channel's `&B4` bits 2-0 and `pm_phase` the LFO's current PM
    /// phase, which together offset the F-Num before it is turned into an
    /// increment ([`crate::lfo`] and [`increment`]). The LFO stops its work by
    /// holding `pm_phase` at 0 and a PMS of 0 offsets nothing, so a chip with no
    /// LFO ticks exactly as it did before either existed.
    pub(crate) fn tick(&mut self, pms: &[u8; CHANNELS], pm_phase: u8) {
        // Every frequency of the chip first, since reading one needs `self` and the
        // counters below are being moved. A normal channel has one F-Num for its four
        // operators and only channel 3 of the two special modes differs (#114).
        let frequency: [[(u16, u8); SLOTS]; CHANNELS] = std::array::from_fn(|channel| {
            std::array::from_fn(|slot| self.frequency(channel, slot))
        });
        for (index, channel) in self.channels.iter_mut().enumerate() {
            for (slot, operator) in channel.operators.iter_mut().enumerate() {
                let (fnum, block) = frequency[index][slot];
                let pm = crate::lfo::pm_offset(fnum, pms[index], pm_phase);
                let increment = increment(fnum, block, operator.dt, operator.mul, pm);
                operator.phase = (operator.phase + increment) & PHASE_MASK;
            }
        }
    }

    /// The F-Num and block one operator counts at: its channel's `&A0`/`&A4`,
    /// except for operators 1 to 3 of channel 3 while one of the special modes is
    /// on, which read the `&A8`-`&AE` pair of their own. Operator 4 of that channel
    /// is the exception to the exception and stays on `&A2`/`&A6` (YM2608
    /// application manual `&A8`, and M5-09 #114).
    fn frequency(&self, channel: usize, slot: usize) -> (u16, u8) {
        if channel == CHANNEL3 && self.mode != regs::CH3_MODE_NORMAL && slot != SLOT_S4 {
            return (self.ch3.fnum[slot], self.ch3.block[slot]);
        }
        (self.channels[channel].fnum, self.channels[channel].block)
    }

    /// The key code of one operator as its own F-Num and block stand now: what the
    /// key scaling of the envelope generator reads it for, and therefore per
    /// operator rather than per channel - the two are the same thing until channel 3
    /// goes into a special mode and gives three of its operators frequencies of
    /// their own ([`PhaseGen::frequency`], YM2608 application manual key scale
    /// table, and `crate::envelope`).
    pub(crate) fn keycode(&self, channel: usize, slot: usize) -> u8 {
        let (fnum, block) = self.frequency(channel, slot);
        keycode(fnum, block)
    }

    /// The `&28` key state of one operator: what the chip was last told to hold,
    /// with nothing of CSM in it. CSM reads it to know whether a driver is holding
    /// channel 3 on through the end of its own key pulse (M5-09, #114).
    ///
    /// `key_state` rather than `key_on`, since [`PhaseGen::key_on`] is the `&28`
    /// write that puts the state there.
    pub(crate) fn key_state(&self, channel: usize, slot: usize) -> bool {
        self.channels[channel].operators[slot].key_on
    }

    /// What one channel holds right now: the F-Num and block its four operators
    /// share, and each operator's detune, multiple, key state and phase counter.
    ///
    /// `#[cfg(test)]` because a YM2612 cannot be read back: only the model and
    /// its tests have this view of the chip.
    #[cfg(test)]
    pub(crate) fn channel(&self, channel: usize) -> &Channel {
        &self.channels[channel]
    }

    /// The 20-bit phase counter of one operator, which is what the slot output of
    /// the channel mix reads as its waveform address (M5-07a, #111).
    ///
    /// Not `#[cfg(test)]`, for that reason: unlike [`PhaseGen::channel`] this one
    /// is how the chip reads itself, not how the model does.
    pub(crate) fn phase(&self, channel: usize, slot: usize) -> u32 {
        self.channels[channel].operators[slot].phase
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `keycode` at the values the ticket names. Every expected key code is
    /// worked out by hand from the manual's rule
    ///
    /// ```text
    /// kc = (BLK << 2) | (FN10 << 1) | N3
    /// N3 = (FN10 & (FN9 | FN8 | FN7)) | (!FN10 & FN9 & FN8 & FN7)
    /// ```
    #[test]
    fn keycode_values() {
        // block 4, fnum 0x400: FN10 = 1 and FN9..FN7 = 0, so N3 = (1 & 0) |
        // (0 & ...) = 0 and kc = (4 << 2) | (1 << 1) | 0 = 0b10010.
        assert_eq!(keycode(0x400, 4), 0b10010);
        // FN10 = 1, FN9 = 1: the first term of N3 fires -> 0b10011.
        assert_eq!(keycode(0x600, 4), 0b10011);
        // FN10 = 0 but FN9, FN8 and FN7 all 1: the carry term fires, so the key
        // code runs on into 1 rather than jumping to 2.
        assert_eq!(keycode(0x380, 4), 0b10001);
        // FN10 = 0 and FN7 = 0: neither term, so the low two bits are 0.
        assert_eq!(keycode(0x300, 4), 0b10000);
        // Block alone: 0x200 is not enough to move N3.
        assert_eq!(keycode(0x200, 2), 0b01000);
        // Bottom and top of the range: kc is 5 bits and 31 is reachable.
        assert_eq!(keycode(0x000, 0), 0);
        assert_eq!(keycode(0x7FF, 7), 31);
        // The low seven bits of the F-Num are not part of the key code.
        assert_eq!(keycode(0x3FF, 4), keycode(0x380, 4));
    }

    #[test]
    fn increment_mul1_dt0() {
        // The plain case: (0x269 << 4) >> 1.
        assert_eq!(increment(0x269, 4, 0, 1, 0), (0x269 << 4) >> 1);
        assert_eq!(increment(0x269, 4, 0, 1, 0), 0x1348);
        // Block 0 still shifts right by one, so the smallest F-Nums stop.
        assert_eq!(increment(0x001, 0, 0, 1, 0), 0);
        assert_eq!(increment(0x002, 0, 0, 1, 0), 1);
    }

    #[test]
    fn mul0_halves() {
        // MUL 0 is x0.5, which is the same shift the base increment does again.
        assert_eq!(increment(0x269, 4, 0, 0, 0), 0x09A4);
        assert_eq!(
            increment(0x269, 4, 0, 0, 0),
            increment(0x269, 4, 0, 1, 0) >> 1
        );
        assert_eq!(increment(0x001, 0, 0, 0, 0), 0);
        // And the other multiples really multiply.
        assert_eq!(increment(0x269, 4, 0, 2, 0), 0x2690);
        assert_eq!(increment(0x269, 4, 0, 15, 0), 0x1348 * 15);
    }

    #[test]
    fn detune_negative_wraps_17_bits() {
        // fnum 1, block 0: the base increment is (1 << 0) >> 1 = 0, and DT = 7
        // is row 3 taken negative with DETUNE[3][kc 0] = 2. A real chip has no
        // sign bit on the 17-bit increment, so it wraps to 0x1FFFE instead of
        // clamping at 0: the detune overflow, which is kept on purpose.
        assert_eq!(increment(1, 0, 7, 1, 0), 0x1FFFE);
        // Row 1 is 0 at kc 0, so there is nothing to subtract and no wrap.
        assert_eq!(increment(1, 0, 5, 1, 0), 0);
    }

    #[test]
    fn detune_row_is_dt_bits_1_0_and_sign_is_bit_2() {
        // block 4, fnum 0x300: kc = 0b10000 = 16 and the base increment is
        // (0x300 << 4) >> 1 = 0x1800 = 6144.
        let base: u32 = 0x1800;
        assert_eq!(increment(0x300, 4, 0, 1, 0), base);
        assert_eq!(increment(0x300, 4, 1, 1, 0), base + 2);
        assert_eq!(increment(0x300, 4, 2, 1, 0), base + 5);
        assert_eq!(increment(0x300, 4, 3, 1, 0), base + 8);
        // Bit 2 is the sign, and the rows do not move when it is set alone.
        assert_eq!(increment(0x300, 4, 4, 1, 0), base);
        assert_eq!(increment(0x300, 4, 5, 1, 0), base - 2);
        assert_eq!(increment(0x300, 4, 6, 1, 0), base - 5);
        assert_eq!(increment(0x300, 4, 7, 1, 0), base - 8);
        // The row is read at the key code, not at a fixed index: kc 17 is the
        // next entry of the FD=1 row.
        assert_eq!(increment(0x380, 4, 1, 1, 0), 0x1C00 + 3);
        // Detuning comes before the multiple, so the offset is scaled too.
        assert_eq!(increment(0x300, 4, 1, 2, 0), (base + 2) * 2);
    }

    #[test]
    fn increment_masks_to_20_bits() {
        // The widest the frequency side can ask for: (0x7FF << 7) >> 1 =
        // 0x1FFC0, times 15 is 0x1DFC40, and the increment is 20 bits, so the
        // top bits fall off.
        assert_eq!(increment(0x7FF, 7, 0, 15, 0), 0xDFC40);
    }
}
