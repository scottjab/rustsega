//! The envelope generator: how loud each operator is, and how that changes.
//!
//! An operator has a 10-bit attenuation - 0 is the loudest a chip gets and 0x3FF
//! is silent - and four registers per operator decide where that number goes:
//! `&50` the attack rate with its key scaling, `&60` the decay 1 rate, `&70` the
//! decay 2 rate the level sustains at, and `&80` the sustain level and the
//! release rate (YM2608 application manual "3. Register descriptions", `&40` to
//! `&80`). A key-on takes the envelope through attack and decay to sustain, and
//! a key-off sends it into release.
//!
//! The chip does not walk the 24 envelopes once a sample; it does so once every
//! three samples, against one chip-wide 12-bit counter that every rate reads its
//! update points off. Nemesis's "YM2612 envelope generator" post on SpritesMind
//! is the source of that counter, of the increment table and of the attack
//! formula; the rates, the sustain level and the total level are the manual's.
//!
//! The amplitude modulation enable of `&60` bit 7 is kept here, because that is
//! the register group it lives in, for `crate::lfo` to gate its AM with (M5-08,
//! #113). So what [`Envelope::output`] gives is the operator's attenuation
//! before the LFO adds to it.
//!
//! The SSG-EG of `&90` is here too (M5-10, #115). Enabled, it takes the
//! envelope away from the four states above: the decay, sustain and release
//! increments are four times as big up to half the range, and nothing at all
//! past it; and what happens at that halfway point is the `&90` hold/alternate
//! table of [`Envelope::ssg_restarting`], which is the repeated saw and the
//! triangle on one side and the four one-shot holds on the other. The output of
//! [`Envelope::output`] may then be the mirror of the envelope's own attenuation
//! about [`SSG_HALF`], which is why the channel reads its envelope there and not
//! the field. What is not modelled is the phase-generator reset the chip asks
//! for at the same moment in the two non-holding modes - Nuked-OPN2's
//! `eg_ssg_pgrst_latch` - because the envelopes cannot reach the phase generator
//! from here; the follow-up issue filed with this PR covers it.

use crate::phase::{self, CHANNELS, PhaseGen, SLOTS};
use crate::regs::{
    REG_AM_DECAY_1, REG_DECAY_2, REG_KEY_ON, REG_KEY_SCALE_ATTACK, REG_LEVEL_RELEASE, REG_SSG_EG,
    REG_TOTAL_LEVEL, operator_of_group,
};

// Serialising this chip state for save states (M6-07a, #126). Gated on the
// crate's `serde` feature so `cargo build --no-default-features` keeps the
// core with no dependencies at all, as docs/ARCHITECTURE.md asks of it.
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The attenuation of a silent operator: 10 bits, and the other end from it is
/// 0 (YM2608 application manual `&40`).
pub(crate) const SILENT: u16 = 0x3FF;

/// `&90` bit 3: the SSG-EG is enabled for this operator, and the four rules of
/// [`Envelope::ssg_restarting`] and the mirror of [`Envelope::ssg_mirrored`] take
/// over from the plain ADSR of #109 (YM2608 application manual "3. Register
/// descriptions", `&90`).
const SSG_ENABLE: u8 = 0x08;

/// `&90` bit 2, the attack bit: read the output mirrored, so that the operator
/// starts quiet and gets louder rather than the other way round (`&90`).
const SSG_ATTACK: u8 = 0x04;

/// `&90` bit 1, the alternate bit: flip the mirror every time the envelope
/// reaches [`SSG_HALF`] (`&90`).
const SSG_ALTERNATE: u8 = 0x02;

/// `&90` bit 0, the hold bit: hold the output at [`SSG_HALF`] instead of going
/// back for another swing (`&90`).
const SSG_HOLD: u8 = 0x01;

/// Half the envelope range, and the point an SSG-EG envelope is read at: the
/// chip tests bit 9 of the attenuation, and everything the `&90` bits have to say
/// is said there (YM2608 application manual `&90`; Nuked-OPN2's
/// `OPN2_EnvelopeSSGEG` tests `eg_level & 0x200`).
const SSG_HALF: u16 = 0x200;

/// The 10 bits the mirror wraps through: a level above [`SSG_HALF`] mirrored is
/// not negative, it is on the way round again - `ssg_level &= 0x3ff` of
/// Nuked-OPN2.
const SSG_MIRROR_MASK: u16 = 0x3FF;

/// What SSG-EG multiplies the decay, sustain and release increments by, and the
/// attack is not one of them (Nuked-OPN2's `inc <<= 2`, which is inside the
/// decay, sustain and release cases of `OPN2_EnvelopeADSR` and not the attack).
const SSG_INCREMENT_SCALE: u8 = 4;

/// The highest value of the chip-wide envelope counter, which is 12 bits and
/// runs 1 to this rather than 0 to it: the counter of Nemesis's description never
/// takes the value 0, and that is felt - the slowest rates, whose shift is the
/// whole 11 bits, are left with one update point a turn, counter 2048.
const COUNTER_MAX: u16 = 0x0FFF;

/// Output samples per envelope update: the envelopes are stepped a third as
/// often as the phase counters (Nemesis, "YM2612 envelope generator").
const SAMPLES_PER_UPDATE: u8 = 3;

/// The attack rates that do not count up at all: at 62 and above the operator
/// is at full level with the key-on itself, and not after an update (Nemesis,
/// "YM2612 envelope generator").
const INSTANT_RATE: u8 = 62;

/// Rates in the increment table: 6 bits, so 0 to 63.
const RATES: usize = 64;

/// The largest rate, and the top of [`effective_rate`].
const MAX_RATE: u8 = 63;

/// The eight phases one rate group is spread over: the low three bits of the
/// counter once its shift has taken the rest (Nemesis, "YM2612 envelope
/// generator").
const PHASES: usize = 8;

/// The widest a rate's shift gets: `11 - (rate >> 2)` before it is clamped at 0.
const MAX_SHIFT: u8 = 11;

/// The largest increment a rate can ask for, which is what the top four rates do
/// every phase of it.
const MAX_INCREMENT: u8 = 8;

/// The four increment patterns of the rates under 48, indexed by `rate & 3`:
/// each steps one more of the eight phases than the last, so the group gains a
/// quarter of an increment per rate (Nemesis, "YM2612 envelope generator").
const PATTERN: [[u8; PHASES]; 4] = [
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 0, 1, 1, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 1],
];

/// The same four patterns for the rates from 48, where the increment of a phase
/// is `PATTERN`-shaped again over a base of 1, 2 or 4 rather than of 0 and 1.
const SCALED_PATTERN: [[u8; PHASES]; 4] = [
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 2, 1, 1, 1, 2],
    [1, 2, 1, 2, 1, 2, 1, 2],
    [1, 2, 2, 2, 1, 2, 2, 2],
];

/// The increment table of Nemesis's envelope description, `[rate][phase]`.
///
/// Rates 0 and 1 have no pattern of their own and stay all zeros, which is what
/// makes a rate of 0 or 1 stopped rather than merely slow. Rates 60 to 63 are
/// full at every phase: the pattern of `&3` would ask for more than the 8 the
/// envelope can add.
const INCREMENT: [[u8; PHASES]; RATES] = build_increment_table();

/// [`INCREMENT`], from [`PATTERN`] and [`SCALED_PATTERN`].
const fn build_increment_table() -> [[u8; PHASES]; RATES] {
    let mut table = [[0u8; PHASES]; RATES];

    let mut rate = 2usize;
    while rate < 48 {
        let pattern = PATTERN[rate & 3];
        let mut phase = 0;
        while phase < PHASES {
            table[rate][phase] = pattern[phase];
            phase += 1;
        }
        rate += 1;
    }

    let mut rate = 48usize;
    while rate < 60 {
        let pattern = SCALED_PATTERN[rate & 3];
        let scale = 1u8 << ((rate - 48) / 4);
        let mut phase = 0;
        while phase < PHASES {
            table[rate][phase] = pattern[phase] * scale;
            phase += 1;
        }
        rate += 1;
    }

    let mut rate = 60usize;
    while rate < RATES {
        let mut phase = 0;
        while phase < PHASES {
            table[rate][phase] = MAX_INCREMENT;
            phase += 1;
        }
        rate += 1;
    }

    table
}

/// How many counts of the envelope counter pass between the update points of
/// `rate`, as a shift: 11 for the slowest rates down to 0 from rate 44, where
/// every count is an update point (Nemesis, "YM2612 envelope generator").
fn shift(rate: u8) -> u8 {
    MAX_SHIFT.saturating_sub(rate >> 2)
}

/// What one envelope update adds at `rate` with the chip-wide counter at
/// `counter`.
///
/// Zero in two different cases: the counter has not reached one of this rate's
/// update points, or it has and the rate's row has nothing to add.
fn increment(rate: u8, counter: u16) -> u8 {
    let shift = shift(rate);
    if counter & ((1 << shift) - 1) != 0 {
        return 0;
    }
    INCREMENT[usize::from(rate)][usize::from((counter >> shift) & 7)]
}

/// The effective rate of a register rate `rate` at key scale `key_scale` (`&50`
/// bits 7-6) and key code `keycode`: `min(63, 2R + ks)` with `ks = kc >> (3 -
/// RS)`, and 0 for a rate of 0 - a rate of 0 is stopped rather than slow, and
/// key scaling cannot start it (YM2608 application manual `&50`; Nemesis, "YM2612
/// envelope generator").
pub(crate) fn effective_rate(rate: u8, key_scale: u8, keycode: u8) -> u8 {
    if rate == 0 {
        return 0;
    }
    (rate * 2 + (keycode >> (3 - key_scale))).min(MAX_RATE)
}

/// The attenuation a `&80` bits 7-4 sustain level asks for.
///
/// The level it asks for is 5 bits even though the register field is 4, so D1L
/// 15 is the all-ones of those 5 bits, 0x3E0, and not 15 << 5 = 0x1E0. Every
/// other value is the plain shift (Nemesis, "YM2612 envelope generator").
pub(crate) fn sustain_attenuation(level: u8) -> u16 {
    let steps = if level == 15 { 31 } else { u16::from(level) };
    steps << 5
}

/// `att` after one attack step of `inc`: `att += (~att * inc) >> 4` (Nemesis,
/// "YM2612 envelope generator").
///
/// The invert is the signed one, and that is what makes the step proportional to
/// what is left rather than to what has been gone through: 1023 takes 64 off at
/// `inc` 1 and 15 takes none, which is why a slow attack closes on 0 but stops
/// short of it, and a rate of [`INSTANT_RATE`] or more is the shortcut the chip
/// takes instead.
fn attack_step(att: u16, inc: u8) -> u16 {
    let att = i32::from(att);
    (att + ((!att * i32::from(inc)) >> 4)).clamp(0, i32::from(SILENT)) as u16
}

/// Where an operator's envelope is: the four states of a YM2612 envelope, in
/// the order a held key takes it (YM2608 application manual `&50` to `&80`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) enum State {
    /// Rising towards full level, exponentially.
    Attack,
    /// Falling towards the sustain level, linearly.
    Decay,
    /// At the sustain level, falling only as slowly as `&70` says.
    Sustain,
    /// Falling to silence after a key off.
    Release,
}

/// One operator's envelope: the state machine, and the register fields of the
/// five groups `&40` to `&80` that drive it.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Envelope {
    /// Where in the four states it is.
    state: State,
    /// The envelope's own 10-bit attenuation, which the total level of `&40` is
    /// added to by [`Envelope::output`].
    att: u16,
    /// The `&28` bit of this operator as this generator last saw it: what the
    /// envelope cares about is the edge, and a `&28` write that repeats the level
    /// it already had does nothing.
    key_on: bool,
    /// `&40` bits 6-0.
    total_level: u8,
    /// `&50` bits 7-6, the rate scaling.
    key_scale: u8,
    /// `&50` bits 4-0, the attack rate.
    attack_rate: u8,
    /// `&60` bits 4-0, the decay 1 rate.
    decay1_rate: u8,
    /// `&60` bit 7: whether the LFO's amplitude modulation reaches this operator
    /// at all, whatever its channel's AMS says (YM2608 application manual `&60`,
    /// and [`crate::lfo`]).
    am_enable: bool,
    /// `&70` bits 4-0, the decay 2 rate.
    decay2_rate: u8,
    /// `&80` bits 7-4, the sustain level.
    sustain_level: u8,
    /// `&80` bits 3-0, the release rate.
    release_rate: u8,
    /// `&90` bits 3-0: the SSG-EG mode, enable/attack/alternate/hold (YM2608
    /// application manual `&90`).
    ssg_eg: u8,
    /// Which way round the SSG-EG is reading this operator right now: Nuked-
    /// OPN2's `eg_ssg_dir`, the one bit of state the alternate and hold rules of
    /// `&90` leave behind between swings. The output is mirrored when this
    /// disagrees with the attack bit, and only while the key is on.
    ssg_direction: bool,
}

impl Envelope {
    /// A silent envelope that nothing has keyed: the state of a chip that has
    /// never been told to sound this operator.
    fn new() -> Self {
        Self {
            state: State::Release,
            att: SILENT,
            key_on: false,
            total_level: 0,
            key_scale: 0,
            attack_rate: 0,
            decay1_rate: 0,
            am_enable: false,
            decay2_rate: 0,
            sustain_level: 0,
            release_rate: 0,
            ssg_eg: 0,
            ssg_direction: false,
        }
    }

    /// The `&28` bit of this operator: on goes to attack, off goes to release,
    /// and a bit that has not changed does nothing at all (YM2608 application
    /// manual `&28`).
    ///
    /// A key on does not reset the attenuation. An operator that has fully
    /// released is at silence already, and re-striking one that has not starts
    /// the attack from where it stands, which is what a real chip does with a
    /// note cut into a release.
    fn set_key(&mut self, on: bool, keycode: u8) {
        if self.key_on == on {
            return;
        }
        if !on {
            // A key off releases an SSG-EG envelope from where its mirror was
            // reading, and takes the mirror off with it: Nuked-OPN2 does the
            // first with `if (koff_event) level = ssg_level` of
            // `OPN2_EnvelopeADSR` and the second with the `& eg_kon[slot]` on
            // both the direction and the mirror (`&90`).
            if self.ssg_mirrored() {
                self.att = SSG_HALF.wrapping_sub(self.att) & SSG_MIRROR_MASK;
            }
            self.ssg_direction = false;
        }
        self.key_on = on;
        self.state = if on { State::Attack } else { State::Release };
        if on && effective_rate(self.attack_rate, self.key_scale, keycode) >= INSTANT_RATE {
            self.att = 0;
        }
    }

    /// One envelope update at key code `keycode`, with the chip-wide counter at
    /// `counter`.
    fn update(&mut self, counter: u16, keycode: u8) {
        // The SSG-EG reads the attenuation the envelope starts the update with,
        // which is the order Nuked-OPN2 runs `OPN2_EnvelopeSSGEG` and the ADSR
        // step in. `off` is its `eg_off`: past halfway an SSG-EG envelope has no
        // increment at all, and may have stopped being an envelope (`&90`).
        let off = self.ssg_enabled() && self.att >= SSG_HALF;
        let holding_loud = self.ssg_holding_loud();

        // The states move first: an attack that has reached 0 is decay, and a
        // decay that has reached its sustain level is sustain.
        if self.state == State::Attack && self.att == 0 {
            self.state = State::Decay;
        }
        if self.state == State::Decay && self.att >= sustain_attenuation(self.sustain_level) {
            self.state = State::Sustain;
        }
        if self.ssg_restarting() {
            // A repeat of `&90` is a key on of the envelope: it is the attack
            // that runs again, from wherever the attenuation has got to, and the
            // envelope off below has nothing to say about it (Nuked's
            // `kon_event`, which is `okon && eg_ssg_repeat_latch`).
            self.state = State::Attack;
        }

        let mut inc = increment(self.rate(keycode), counter);
        if self.ssg_enabled() && self.state != State::Attack {
            // Past the halfway point an SSG-EG envelope adds nothing at all, and
            // short of it it adds four times what the plain envelope would
            // (Nuked-OPN2's `if (!eg_off && chip->eg_inc)`, with its `inc <<= 2`
            // inside). The attack is not one of these states and is not gated:
            // an SSG-EG envelope that has just turned round attacks from past
            // halfway, and it has to be able to get back down.
            inc = if off {
                0
            } else {
                inc.saturating_mul(SSG_INCREMENT_SCALE)
            };
        }

        if inc != 0 {
            if self.state == State::Attack {
                self.att = attack_step(self.att, inc);
                if self.att == 0 {
                    self.state = State::Decay;
                }
            } else {
                // Decay, sustain and release are all linear, and release is not
                // stopped by the sustain level: it is only ever clamped at
                // silence.
                self.att = (self.att + u16::from(inc)).min(SILENT);
            }
        }

        // And the envelope off itself: an SSG-EG envelope past halfway that is
        // not attacking and is not one of the two `&90` modes that hold at full
        // level goes silent and stays there - Nuked's `nextstate = release,
        // nextlevel = 0x3ff`, which is what makes 9 and 15 hold silent while 11
        // and 13 hold loud (`&90`, and the manual's SSG-EG diagram).
        if off && self.state != State::Attack && !holding_loud {
            self.state = State::Release;
            self.att = SILENT;
        }
    }

    /// The rate of the state it is in, key-scaled: `&50` for attack, `&60` for
    /// decay, `&70` for sustain and `&80` for release, the last of these a 4-bit
    /// rate with 1 added so that RR 0 releases at the slowest rate instead of not
    /// at all (YM2608 application manual `&50` to `&80`; Nemesis, "YM2612
    /// envelope generator").
    fn rate(&self, keycode: u8) -> u8 {
        match self.state {
            State::Attack => effective_rate(self.attack_rate, self.key_scale, keycode),
            State::Decay => effective_rate(self.decay1_rate, self.key_scale, keycode),
            State::Sustain => effective_rate(self.decay2_rate, self.key_scale, keycode),
            State::Release => effective_rate(self.release_rate * 2 + 1, self.key_scale, keycode),
        }
    }

    /// Whether the SSG-EG of `&90` runs this operator at all, which is all the
    /// plain ADSR of #109 is without it (YM2608 application manual `&90`).
    fn ssg_enabled(&self) -> bool {
        self.ssg_eg & SSG_ENABLE != 0
    }

    /// What the `&90` bits make of an envelope that starts this update at or past
    /// [`SSG_HALF`], and whether it goes back for another swing:
    ///
    /// | hold | alternate | at the halfway point             |
    /// |------|-----------|----------------------------------|
    /// | 0    | 0         | back to the attack               |
    /// | 0    | 1         | flip the mirror, back to attack  |
    /// | 1    | 1         | mirror on, and stay on           |
    /// | 1    | 0         | nothing: the output holds        |
    ///
    /// The table is the manual's SSG-EG diagram of `&90` 8 to 15 read the other
    /// way round, and its source is Nuked-OPN2's `OPN2_EnvelopeSSGEG`: the two
    /// rows that restart are its `eg_ssg_repeat_latch`, `(ssg_eg & 0x01) == 0`,
    /// and the mirror is its `direction ^= 1` and `direction = 1`. A repeat is a
    /// key on of the envelope, so the key state gates the lot: a released
    /// envelope has no attack to go back to.
    fn ssg_restarting(&mut self) -> bool {
        if !self.ssg_enabled() || !self.key_on || self.att < SSG_HALF {
            return false;
        }
        let hold = self.ssg_eg & SSG_HOLD != 0;
        if self.ssg_eg & SSG_ALTERNATE != 0 {
            self.ssg_direction = if hold { true } else { !self.ssg_direction };
        }
        !hold
    }

    /// The two `&90` modes that hold at full level rather than going silent: the
    /// hold bit with the attack and the alternate disagreeing, which is Nuked's
    /// `eg_ssg_hold_up_latch` of `(mask & 7) == 5 || (mask & 7) == 3`. Its key
    /// is `eg_kon_latch`, so this is a held-key-only rule like the rest of `&90`.
    fn ssg_holding_loud(&self) -> bool {
        let mode = self.ssg_eg & (SSG_ATTACK | SSG_ALTERNATE | SSG_HOLD);
        let alternating = SSG_ALTERNATE | SSG_HOLD;
        let attacking = SSG_ATTACK | SSG_HOLD;
        self.key_on && (mode == alternating || mode == attacking)
    }

    /// Whether the output of this operator is read mirrored: the running
    /// direction of `&90` and its attack bit disagreeing, and only while the key
    /// is on - Nuked's `eg_ssg_inv`, `(direction ^ (attack & enable)) &
    /// eg_kon` (`&90`).
    fn ssg_mirrored(&self) -> bool {
        self.ssg_enabled() && self.key_on && (self.ssg_direction != (self.ssg_eg & SSG_ATTACK != 0))
    }

    /// What the operator contributes to the channel: its own attenuation plus
    /// the total level of `&40` at 8 attenuation counts a step, clamped at
    /// silence (YM2608 application manual `&40`).
    ///
    /// Read by the slot output of the channel mix of M5-07a (#111), which is why
    /// it is not `#[cfg(test)]`: the chip reads its own envelope here, it just
    /// cannot be made to read it back out. The amplitude modulation of M5-08
    /// (#113) is added on top there, not here.
    pub(crate) fn output(&self) -> u16 {
        (self.attenuation() + (u16::from(self.total_level) << 3)).min(SILENT)
    }

    /// The envelope's own attenuation as the channel reads it: the level itself,
    /// or its mirror about [`SSG_HALF`] while an SSG-EG inversion is running -
    /// Nuked-OPN2's `ssg_level = 512 - level` (`&90`, [`Envelope::ssg_mirrored`]).
    fn attenuation(&self) -> u16 {
        if self.ssg_mirrored() {
            SSG_HALF.wrapping_sub(self.att) & SSG_MIRROR_MASK
        } else {
            self.att
        }
    }

    /// Whether the LFO's amplitude modulation reaches this operator at all: bit 7
    /// of the `&60` write (YM2608 application manual `&60`). Read with
    /// [`Envelope::output`] by `crate::lfo`'s caller in `crate::Ym2612::clock`.
    pub(crate) fn am_enable(&self) -> bool {
        self.am_enable
    }

    /// The state of this envelope, for the model's own tests and for
    /// `Ym2612`'s.
    #[cfg(test)]
    pub(crate) fn state(&self) -> State {
        self.state
    }

    /// This envelope's own attenuation, without the total level.
    #[cfg(test)]
    pub(crate) fn level(&self) -> u16 {
        self.att
    }
}

/// The envelopes of the 24 operators, and the counter they all read their update
/// points off.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct EnvelopeGen {
    /// `[channel][slot]`, the channels of [`crate::regs::PART_I`] first and the
    /// slot order of [`phase::SLOT_S1`].
    envelopes: [[Envelope; SLOTS]; CHANNELS],
    /// The chip-wide 12-bit envelope counter: 1 to [`COUNTER_MAX`], never 0, and
    /// one step every [`SAMPLES_PER_UPDATE`] samples.
    counter: u16,
    /// Samples taken since that step, 0 to [`SAMPLES_PER_UPDATE`] - 1.
    sample: u8,
}

impl EnvelopeGen {
    /// An envelope generator with every operator silent, its counter at
    /// [`COUNTER_MAX`] - the wrap point, so that the first update uses 1 and the
    /// counter is never the 0 its update rule has no answer for - and its sample
    /// divider at 0, so the first envelope update lands on the third sample.
    pub(crate) fn new() -> Self {
        Self {
            envelopes: [[Envelope::new(); SLOTS]; CHANNELS],
            counter: COUNTER_MAX,
            sample: 0,
        }
    }

    /// A data port write of `value` to `register` of `part`, for the registers
    /// the envelope reads: the key on/off of [`REG_KEY_ON`] and the five
    /// per-operator groups `&40`, `&50`, `&60`, `&70` and `&80`.
    pub(crate) fn register_written(
        &mut self,
        phases: &PhaseGen,
        part: usize,
        register: u8,
        value: u8,
    ) {
        if register == REG_KEY_ON {
            self.key_on(value, phases);
            return;
        }

        let Some(field) = Field::of(register) else {
            return;
        };
        let Some((channel, slot)) = operator_of_group(part, field.first(), register) else {
            return;
        };
        field.apply(&mut self.envelopes[channel][slot], value);
    }

    /// `&28`: the key state of the four operators of a channel, from either
    /// part, since the channel field of the write is already an absolute channel
    /// number (YM2608 application manual `&28`).
    fn key_on(&mut self, value: u8, phases: &PhaseGen) {
        let Some(channel) = phase::key_on_channel(value & 0x07) else {
            return;
        };
        for &(mask, slot) in &phase::KEY_ON_BITS {
            let keycode = phases.keycode(channel, slot);
            self.envelopes[channel][slot].set_key(value & mask != 0, keycode);
        }
    }

    /// Advance by one output sample.
    ///
    /// Nothing happens on two samples in three: the envelopes go a third as
    /// often as the phase counters do, and all 24 of them are stepped against
    /// the one counter of the chip (Nemesis, "YM2612 envelope generator").
    pub(crate) fn tick(&mut self, phases: &PhaseGen) {
        self.sample += 1;
        if self.sample < SAMPLES_PER_UPDATE {
            return;
        }
        self.sample = 0;

        self.counter = if self.counter == COUNTER_MAX {
            1
        } else {
            self.counter + 1
        };

        // The key code per operator, since the two are one thing in a normal
        // channel and separate ones in the special mode of channel 3 (#114).
        for channel in 0..CHANNELS {
            for slot in 0..SLOTS {
                let keycode = phases.keycode(channel, slot);
                self.envelopes[channel][slot].update(self.counter, keycode);
            }
        }
    }

    /// The CSM key of channel 3: `on` is the pulse of a timer A overflow, held for
    /// the one sample it overflows on, and the `false` that follows it on the next
    /// sample is what releases the four operators again - unless `&28` is holding
    /// one of them, which is why this reads the key state of the phase generator
    /// rather than the pulse alone (YM2608 application manual `&27`, M5-09 #114,
    /// and [`crate::phase::PhaseGen::key_state`]).
    ///
    /// The pulse says nothing to the phase counters: it is the envelope of channel
    /// 3 that CSM strikes, and the counters keep counting through it.
    pub(crate) fn csm_key(&mut self, phases: &PhaseGen, on: bool) {
        for slot in 0..SLOTS {
            let keycode = phases.keycode(phase::CHANNEL3, slot);
            let held = on || phases.key_state(phase::CHANNEL3, slot);
            self.envelopes[phase::CHANNEL3][slot].set_key(held, keycode);
        }
    }

    /// What one operator's envelope holds right now, which is what the slot
    /// output of the channel mix of M5-07a (#111) asks for: the slot indices of
    /// [`phase::SLOT_S1`] and friends.
    pub(crate) fn envelope(&self, channel: usize, slot: usize) -> &Envelope {
        &self.envelopes[channel][slot]
    }
}

/// One of the five per-operator register groups of the envelope, named for what
/// it holds.
#[derive(Clone, Copy, Debug)]
enum Field {
    /// `&40` bits 6-0: the total level, which is added to the attenuation.
    TotalLevel,
    /// `&50` bits 7-6 the rate scaling and 4-0 the attack rate.
    KeyScaleAttack,
    /// `&60` bits 4-0 the decay 1 rate and bit 7 the amplitude modulation enable,
    /// which [`Envelope`] keeps for [`crate::lfo`] to gate its AM with.
    Decay1Rate,
    /// `&70` bits 4-0 the decay 2, sustaining, rate.
    Decay2Rate,
    /// `&80` bits 7-4 the sustain level and 3-0 the release rate.
    LevelRelease,
    /// `&90` bits 3-0: the SSG-EG mode this operator runs its envelope by
    /// (YM2608 application manual `&90`).
    SsgEg,
}

impl Field {
    /// The field a write to `register` belongs to, which is its `&N0` group: the
    /// envelope's five groups are whole decades, with the slot offset and the
    /// channel in the low nibble (YM2608 application manual "3. Register
    /// descriptions").
    fn of(register: u8) -> Option<Self> {
        match register & 0xF0 {
            REG_TOTAL_LEVEL => Some(Self::TotalLevel),
            REG_KEY_SCALE_ATTACK => Some(Self::KeyScaleAttack),
            REG_AM_DECAY_1 => Some(Self::Decay1Rate),
            REG_DECAY_2 => Some(Self::Decay2Rate),
            REG_LEVEL_RELEASE => Some(Self::LevelRelease),
            REG_SSG_EG => Some(Self::SsgEg),
            _ => None,
        }
    }

    /// The first register number of the group, which
    /// [`crate::regs::operator_of_group`] reads the slot and channel off.
    fn first(self) -> u8 {
        match self {
            Self::TotalLevel => REG_TOTAL_LEVEL,
            Self::KeyScaleAttack => REG_KEY_SCALE_ATTACK,
            Self::Decay1Rate => REG_AM_DECAY_1,
            Self::Decay2Rate => REG_DECAY_2,
            Self::LevelRelease => REG_LEVEL_RELEASE,
            Self::SsgEg => REG_SSG_EG,
        }
    }

    /// What a write of `value` leaves in `envelope`.
    fn apply(self, envelope: &mut Envelope, value: u8) {
        match self {
            Self::TotalLevel => envelope.total_level = value & 0x7F,
            Self::KeyScaleAttack => {
                envelope.key_scale = value >> 6;
                envelope.attack_rate = value & 0x1F;
            }
            Self::Decay1Rate => {
                envelope.decay1_rate = value & 0x1F;
                envelope.am_enable = value & 0x80 != 0;
            }
            Self::Decay2Rate => envelope.decay2_rate = value & 0x1F,
            Self::LevelRelease => {
                envelope.sustain_level = (value >> 4) & 0x0F;
                envelope.release_rate = value & 0x0F;
            }
            // Only the low nibble: `&90` bits 7-4 are not decoded, and the mode
            // is the whole of what the SSG-EG reads (YM2608 application manual
            // `&90`). Nuked-OPN2 masks the same 0x0F.
            Self::SsgEg => envelope.ssg_eg = value & 0x0F,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::PhaseGen;
    use crate::phase::{SLOT_S1, SLOT_S2, SLOT_S3, SLOT_S4};
    use crate::regs::{PART_I, REG_SSG_EG};

    /// `&28` bit 4: key on operator 1, the operator the tests here work on.
    const KEY_ON_S1: u8 = 0x10;

    /// `count` envelope updates for `envelope` at key code `keycode`, with the
    /// shared counter running the 1 to 4095 cycle that [`EnvelopeGen::tick`]
    /// gives it.
    fn drive(envelope: &mut Envelope, count: u32, keycode: u8) {
        for step in 0..count {
            let counter = u16::try_from(1 + step % u32::from(COUNTER_MAX)).expect("12-bit counter");
            envelope.update(counter, keycode);
        }
    }

    #[test]
    fn rate_zero_never_moves() {
        // A 0 rate is not a slow rate but a stopped one, whatever the key
        // scaling adds (YM2608 application manual `&50`, `&60`, `&70`).
        assert_eq!(effective_rate(0, 0, 0), 0);
        assert_eq!(
            effective_rate(0, 3, 31),
            0,
            "key scaling cannot rescue a 0 rate"
        );

        // The bottom two rows of the increment table are all zeros as well, so
        // a rate of 0 or 1 has nothing to add even on its own update points.
        assert_eq!(increment(0, 0), 0);
        assert_eq!(increment(0, 1024), 0);
        assert_eq!(increment(1, 1024), 0);

        // And at the level of one operator: every envelope register 0, so no
        // attack, no decay and no release, over four turns of the counter.
        let mut envelope = Envelope::new();
        envelope.set_key(true, 0);
        drive(&mut envelope, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(envelope.att, SILENT);
        assert_eq!(
            envelope.state,
            State::Attack,
            "no attack, so no decay either"
        );

        envelope.set_key(false, 0);
        drive(&mut envelope, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(envelope.att, SILENT, "RR 0 is the stopped release rate 1");
    }

    #[test]
    fn effective_rate_with_key_scaling() {
        // `ks = kc >> (3 - RS)`, `rate = min(63, 2R + ks)` (YM2608 application
        // manual `&50`; Nemesis, "YM2612 envelope generator"). The named case of
        // the ticket: AR 10, RS 3, key code 31 is 20 + 31.
        assert_eq!(effective_rate(10, 3, 31), 51);
        // The same AR at the other key scales: ks is 31 >> 3, 31 >> 2, 31 >> 1.
        assert_eq!(effective_rate(10, 0, 31), 23);
        assert_eq!(effective_rate(10, 1, 31), 27);
        assert_eq!(effective_rate(10, 2, 31), 35);
        // And with no key scaling to add at all.
        assert_eq!(effective_rate(10, 3, 0), 20);

        // AR 31 asks for 62 plus the key scale, and the rate is 6 bits: at any
        // key scale there is a key code that clamps it.
        for key_scale in 0..4 {
            assert_eq!(effective_rate(31, key_scale, 31), 63);
        }
        // At key code 0 there is nothing to add, so 31 is 62 and not 63.
        assert_eq!(effective_rate(31, 3, 0), 62);
    }

    #[test]
    fn attack_rate_62_is_instant() {
        // Rate 62 and above does not count up at all: the operator is at full
        // level with the key-on itself, before any envelope update (Nemesis,
        // "YM2612 envelope generator").
        let mut envelope = Envelope::new();
        envelope.attack_rate = 31; // 62 at key code 0
        envelope.set_key(true, 0);
        assert_eq!(envelope.state, State::Attack);
        assert_eq!(envelope.att, 0, "rate 62 does not wait for an update");

        // One rate down, and the attack has to be counted from silence.
        let mut counted = Envelope::new();
        counted.attack_rate = 30; // 60 at key code 0
        counted.set_key(true, 0);
        assert_eq!(counted.att, SILENT);

        // It is the effective rate that decides: AR 30 with full key scaling
        // and key code 4 is 60 + 4, which clamps to 63 and so is instant.
        let mut scaled = Envelope::new();
        scaled.attack_rate = 30;
        scaled.key_scale = 3;
        scaled.set_key(true, 4);
        assert_eq!(scaled.att, 0);
    }

    #[test]
    fn attack_reaches_zero_and_goes_to_decay() {
        // AR 20 at key code 0 is rate 40: an update every two counts of the
        // counter, and an increment of 0 or 1 on each of those. The attack is a
        // decreasing exponential, so it does close on 0; and 0 is where the
        // envelope hands over to decay (YM2608 application manual `&50`,
        // `&60`).
        let mut envelope = Envelope::new();
        envelope.attack_rate = 20;
        envelope.set_key(true, 0);
        assert_eq!(envelope.state, State::Attack);

        // Stepped one update at a time rather than a turn of `drive`, because the
        // hand-over itself is what is under test: D1L is 0 here, so the sustain
        // level is 0 and decay has ended again by the end of the turn it started
        // in.
        let mut counter = 1;
        let mut updates = 0;
        while envelope.state == State::Attack {
            envelope.update(counter, 0);
            counter = if counter == COUNTER_MAX {
                1
            } else {
                counter + 1
            };
            updates += 1;
            assert!(updates < 8 * u32::from(COUNTER_MAX), "still attacking");
        }
        assert_eq!(
            envelope.att, 0,
            "the attack closed on 0 in {updates} updates"
        );
        assert_eq!(envelope.state, State::Decay, "and decay from there on");
    }

    #[test]
    fn decay_stops_at_sustain_level() {
        // D1L 4 is 4 << 5 = 0x80 of attenuation (YM2608 application manual
        // `&80`), and decay holds there until the key says otherwise.
        assert_eq!(sustain_attenuation(4), 0x80);

        let mut envelope = Envelope::new();
        envelope.att = 0; // as if the attack had finished: decay raises the level
        envelope.state = State::Decay;
        envelope.decay1_rate = 8; // rate 16 at key code 0
        envelope.sustain_level = 4;

        drive(&mut envelope, u32::from(COUNTER_MAX), 0);
        assert!(
            envelope.att < 0x80,
            "one turn of the counter is 16 counts or so"
        );
        assert_eq!(envelope.state, State::Decay);

        let mut turns = 1;
        while envelope.state != State::Sustain && turns < 16 {
            drive(&mut envelope, u32::from(COUNTER_MAX), 0);
            turns += 1;
        }
        assert!(envelope.att >= 0x80, "decay stops at the sustain level");
        assert_eq!(envelope.state, State::Sustain);

        // D2R 0, so it stays exactly there.
        let att = envelope.att;
        drive(&mut envelope, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(
            envelope.att, att,
            "nothing to add with the sustain rate at 0"
        );
        assert_eq!(envelope.state, State::Sustain);
    }

    #[test]
    fn d1l_15_means_0x3e0() {
        // D1L is 4 bits but the sustain level it addresses is 5: D1L 15 is the
        // 5-bit all-ones, 0x3E0, and not 15 << 5 = 0x1E0 (Nemesis, "YM2612
        // envelope generator"). The other 15 values are the plain shift.
        assert_eq!(sustain_attenuation(0), 0);
        assert_eq!(sustain_attenuation(1), 0x20);
        assert_eq!(sustain_attenuation(4), 0x80);
        assert_eq!(sustain_attenuation(14), 0x1C0);
        assert_eq!(sustain_attenuation(15), 0x3E0);

        // Which is observable: a decay with D1L 15 runs on past 0x1E0 and stops
        // just short of silence, rather than a quarter of the way down.
        let mut envelope = Envelope::new();
        envelope.att = 0; // as if the attack had finished
        envelope.state = State::Decay;
        envelope.decay1_rate = 8;
        envelope.sustain_level = 15;
        drive(&mut envelope, 96 * u32::from(COUNTER_MAX), 0);
        assert!(envelope.att > 0x1E0, "D1L 15 is not D1L 15 << 5");
        assert!(envelope.att >= 0x3E0, "and it holds at 0x3E0");
        assert!(envelope.att < SILENT, "which is not silence");
        assert_eq!(envelope.state, State::Sustain);
    }

    #[test]
    fn release_after_key_off() {
        let mut envelope = Envelope::new();
        envelope.att = 0; // as if the attack had finished
        envelope.release_rate = 8; // release is RR * 2 + 1 = 17
        envelope.set_key(true, 0);
        assert_eq!(envelope.state, State::Attack);

        envelope.set_key(false, 0);
        assert_eq!(envelope.state, State::Release);

        drive(&mut envelope, u32::from(COUNTER_MAX), 0);
        assert!(
            envelope.att > 0,
            "a key off starts the release on its way up"
        );
        assert_eq!(envelope.state, State::Release);

        // The sustain level does not stop it, and silence is as far as it goes.
        drive(&mut envelope, 64 * u32::from(COUNTER_MAX), 0);
        assert_eq!(envelope.att, SILENT);
        assert_eq!(envelope.state, State::Release);
    }

    #[test]
    fn tl_adds_shifted_by_3() {
        // `&40` bits 6-0 are the total level, which adds 8 attenuation counts a
        // step to whatever the envelope itself has reached (YM2608 application
        // manual `&40`).
        let mut envelope = Envelope::new();
        envelope.att = 0;

        envelope.total_level = 0;
        assert_eq!(envelope.output(), 0);
        envelope.total_level = 1;
        assert_eq!(envelope.output(), 8);
        envelope.total_level = 8;
        assert_eq!(envelope.output(), 0x40);
        envelope.total_level = 0x7F;
        assert_eq!(envelope.output(), 0x3F8);

        // The two add, and the sum is clamped at silence rather than wrapped.
        envelope.att = 0x100;
        envelope.total_level = 0x10;
        assert_eq!(envelope.output(), 0x180);
        envelope.att = SILENT;
        assert_eq!(envelope.output(), SILENT);
    }

    #[test]
    fn eg_runs_every_third_sample() {
        // The envelope is not a per-sample thing: the chip updates it once every
        // three samples (Nemesis, "YM2612 envelope generator"), so of every
        // three calls to `tick` it is the third that can move anything.
        //
        // AR 24 at key code 0 is rate 48, whose row is all ones with a shift of
        // 0, so every envelope update adds exactly one step to the attack:
        // whatever does not move did not get its turn.
        let phases = PhaseGen::new();
        let mut eg = EnvelopeGen::new();
        eg.register_written(&phases, PART_I, REG_KEY_SCALE_ATTACK, 24);
        eg.register_written(&phases, PART_I, REG_KEY_ON, KEY_ON_S1);

        let mut previous = eg.envelopes[0][SLOT_S1].att;
        assert_eq!(previous, SILENT);
        for sample in 0..12 {
            eg.tick(&phases);
            let att = eg.envelopes[0][SLOT_S1].att;
            if sample % 3 == 2 {
                assert!(att < previous, "sample {sample} is the third of its group");
            } else {
                assert_eq!(
                    att, previous,
                    "sample {sample} is not the third of its group"
                );
            }
            previous = att;
        }
    }

    #[test]
    fn counter_runs_one_to_4095_and_skips_zero() {
        // The counter the update rule is read against runs 1 to 4095 and never 0
        // (Nemesis, "YM2612 envelope generator", and the ticket).
        let phases = PhaseGen::new();
        let mut eg = EnvelopeGen::new();
        eg.register_written(&phases, PART_I, REG_KEY_SCALE_ATTACK, 1);
        eg.register_written(&phases, PART_I, REG_KEY_ON, KEY_ON_S1);

        // And the consequence the skip is there for: the shift of AR 1 - rate 2
        // at key code 0 - is the whole 11 bits, so of the 4095 counter values one
        // turn visits, exactly one answers it: 2048, the only other one being the
        // 0 the counter does not take. One attack step of increment 1 is
        // `att + ((~att) >> 4)`, which takes 0x3FF to 0x3BF to 0x383 to 0x34A.
        const STEPS: [u16; 3] = [0x3BF, 0x383, 0x34A];

        let mut lowest = u16::MAX;
        for step in STEPS {
            for _ in 0..3 * usize::from(COUNTER_MAX) {
                eg.tick(&phases);
                lowest = lowest.min(eg.counter);
            }
            assert_eq!(
                eg.envelopes[0][SLOT_S1].att, step,
                "one attack step per turn of the counter, no more"
            );
        }
        assert_eq!(lowest, 1, "the counter wraps to 1, not to 0");
    }

    /// An envelope at full level and in its decay, for the SSG-EG tests: AR, DR
    /// and D2R 24 are rate 48 at key code 0, which adds exactly one step per
    /// envelope update, and D1L 15 leaves the decay room to run all the way down
    /// to the halfway point the SSG-EG reads (`&50`, `&60`, `&70`, `&80`).
    fn ssg_at_full_level(mode: u8) -> Envelope {
        let mut envelope = Envelope::new();
        envelope.attack_rate = 24;
        envelope.decay1_rate = 24;
        envelope.decay2_rate = 24;
        envelope.sustain_level = 15;
        envelope.ssg_eg = mode;
        envelope.key_on = true;
        envelope.att = 0;
        envelope.state = State::Decay;
        envelope
    }

    /// `count` envelope updates for `envelope`, one at a time, recording what the
    /// channel would read at each: the envelope's own level, the attenuation
    /// [`Envelope::output`] gives it with the total level at 0, and whether the
    /// SSG-EG mirror was over that output.
    fn drive_output(envelope: &mut Envelope, count: u32) -> Vec<(u16, u16, bool)> {
        (0..count)
            .map(|step| {
                let counter =
                    u16::try_from(1 + step % u32::from(COUNTER_MAX)).expect("12-bit counter");
                envelope.update(counter, 0);
                (envelope.att, envelope.output(), envelope.ssg_mirrored())
            })
            .collect()
    }

    /// The mirror the SSG-EG reads through: `512 - level`, 10 bits, of
    /// Nuked-OPN2's `OPN2_EnvelopeADSR` (YM2608 application manual `&90`).
    fn mirror(att: u16) -> u16 {
        SSG_HALF.wrapping_sub(att) & SSG_MIRROR_MASK
    }

    #[test]
    fn ssg_increment_x4() {
        // With SSG-EG enabled the decay, sustain and release increments are four
        // times the normal ones, and stop dead once the attenuation is past the
        // halfway point (YM2608 application manual `&90`, and Nuked-OPN2's
        // `inc <<= 2` inside `!eg_off`).
        let mut plain = ssg_at_full_level(0);
        drive(&mut plain, 8, 0);
        assert_eq!(
            plain.att, 8,
            "without SSG-EG, one step an update at rate 48"
        );

        let mut ssg = ssg_at_full_level(SSG_ENABLE);
        drive(&mut ssg, 8, 0);
        assert_eq!(ssg.att, 32, "with it, four");

        // The release is one of the quadrupled states too. RR 5 is release rate
        // 22, whose shift is 6, so of the counters 1 to 64 only the 64th answers
        // it - at phase 1 of a row that has 1 to give there. One step along in
        // 64 updates plain, and four with the SSG-EG on.
        let mut released = ssg_at_full_level(0);
        released.release_rate = 5;
        released.set_key(false, 0);
        drive(&mut released, 64, 0);
        assert_eq!(
            released.att, 1,
            "one update point of one step in 64 updates"
        );

        let mut ssg_released = ssg_at_full_level(SSG_ENABLE);
        ssg_released.release_rate = 5;
        ssg_released.set_key(false, 0);
        drive(&mut ssg_released, 64, 0);
        assert_eq!(
            ssg_released.att, 4,
            "and the SSG-EG release is four of those"
        );

        // Four times nothing, past halfway: 0x1FC is two updates short of 0x200
        // and 0x200 is where the envelope stops rather than the point it slows
        // down to. The mode is the holding one, so nothing restarts it here.
        let mut past = ssg_at_full_level(SSG_ENABLE | SSG_ALTERNATE | SSG_HOLD);
        past.att = 0x1FC;
        drive(&mut past, 8, 0);
        assert_eq!(
            past.att, 0x200,
            "the increment stops at 0x200, not after it"
        );
    }

    #[test]
    fn ssg_disabled_matches_plain_eg() {
        // With bit 3 of `&90` clear nothing of the plain envelope of #109
        // changes: one step an update at rate 48 rather than four, no special
        // meaning for the halfway point, and a decay that runs all the way down
        // to silence instead of turning round or freezing there.
        let mut envelope = ssg_at_full_level(0);
        let levels = drive_output(&mut envelope, 1024);
        assert_eq!(levels[7].0, 8, "one step per envelope update");
        assert!(
            levels
                .iter()
                .all(|(att, out, mirrored)| att == out && !mirrored),
            "nothing is mirrored with the SSG-EG off"
        );

        let half = levels
            .iter()
            .position(|(att, _, _)| *att == 0x200)
            .expect("a plain decay passes 0x200 on its way to silence");
        assert_eq!(
            levels[half + 1].0,
            0x201,
            "and keeps the one-step it has been taking, past the halfway point the SSG-EG would stop at"
        );
        assert_eq!(levels.last().expect("1024 updates").0, SILENT);
    }

    #[test]
    fn ssg8_repeats() {
        // `&90` 8 is the enable with neither hold nor alternate: the attenuation
        // runs up to the halfway point and the envelope goes back to its attack
        // from there, over and over, rather than on to silence (YM2608
        // application manual `&90` and its SSG-EG diagram).
        let mut envelope = ssg_at_full_level(SSG_ENABLE);

        // Four an update, so 128 updates is exactly 0x200, and it is the update
        // that finds the envelope there which turns it round.
        drive(&mut envelope, 128, 0);
        assert_eq!(envelope.att, 0x200, "0x200 in 128 updates of four");
        assert_eq!(envelope.state, State::Decay);
        drive(&mut envelope, 1, 0);
        assert_eq!(envelope.state, State::Attack, "and it restarts on attack");

        // On through a thousand more updates: the top of the envelope is the
        // halfway point, and the attack brings it back to full level again and
        // again.
        let levels = drive_output(&mut envelope, 1000);
        assert_eq!(
            levels.iter().map(|(att, _, _)| *att).max().expect("1000"),
            0x200,
            "the turn round is at 0x200 and nowhere past it"
        );
        let repeats = levels
            .windows(2)
            .filter(|window| window[0].0 != 0 && window[1].0 == 0)
            .count();
        assert!(repeats >= 3, "back at full level {repeats} times");
    }

    #[test]
    fn ssg10_alternates_inversion() {
        // `&90` 10 is the enable with the alternate bit: every time the envelope
        // reaches the halfway point the inversion flips, so the output alternates
        // between the level and its mirror about 0x200 (YM2608 application manual
        // `&90`, and Nuked-OPN2's `direction ^= 1`).
        let mut envelope = ssg_at_full_level(SSG_ENABLE | SSG_ALTERNATE);
        let levels = drive_output(&mut envelope, 1000);

        for (att, out, _) in &levels {
            assert!(
                *out == *att || *out == mirror(*att),
                "{att:#04x} is neither itself nor its mirror, it is {out:#04x}"
            );
        }

        // And the mirror really does alternate: on for one swing, off for the
        // next, from a chip that starts unmirrored.
        let mut swings: Vec<bool> = levels.iter().map(|(_, _, mirrored)| *mirrored).collect();
        swings.dedup();
        assert!(
            swings.len() >= 4,
            "the mirror settled into {} swings, not alternation",
            swings.len()
        );
        assert_eq!(&swings[..4], &[false, true, false, true]);
        assert!(
            levels.iter().any(|(att, out, mirrored)| {
                *mirrored && *att < SSG_HALF && *out == mirror(*att) && mirror(*att) != *att
            }),
            "and it is the output that moves, below the halfway point"
        );
    }

    #[test]
    fn ssg9_holds_silent() {
        // `&90` 9 is the enable with the hold bit alone: one swing down to the
        // halfway point and then nothing at all - the envelope is off there, and
        // off is silence (YM2608 application manual `&90`).
        let mut envelope = ssg_at_full_level(SSG_ENABLE | SSG_HOLD);
        drive(&mut envelope, 128, 0);
        assert_eq!(envelope.att, 0x200, "the swing reaches the halfway point");

        drive(&mut envelope, 1, 0);
        assert_eq!(envelope.att, SILENT, "and the envelope is off from there");
        assert_eq!(envelope.state, State::Release);
        assert_eq!(envelope.output(), SILENT);

        drive(&mut envelope, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(envelope.output(), SILENT, "and it holds there");
    }

    #[test]
    fn ssg11_holds_loud() {
        // `&90` 11 is the enable with the alternate and the hold bits: the swing
        // to the halfway point, the inversion on, and a hold - which with the
        // inversion means full level, not silence (YM2608 application manual
        // `&90`, and Nuked-OPN2's `eg_ssg_hold_up_latch`, which is what keeps
        // this mode off the silence the plain hold of 9 goes to).
        let mut envelope = ssg_at_full_level(SSG_ENABLE | SSG_ALTERNATE | SSG_HOLD);
        // 129, not 128: it is the update that finds the envelope at the halfway
        // point that switches the mirror on, as it is for the 9 above.
        drive(&mut envelope, 129, 0);
        assert_eq!(envelope.att, 0x200, "the swing stops at the halfway point");
        assert_eq!(envelope.output(), 0, "which the mirror reads as full level");

        drive(&mut envelope, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(
            envelope.level(),
            0x200,
            "the level stays at the halfway point"
        );
        assert_eq!(envelope.output(), 0, "and so does the output");
    }

    #[test]
    fn ssg13_holds_loud_and_ssg15_holds_silent() {
        // The other two holding modes are those two seen through the attack bit:
        // 13 is the inverted 11 and so also ends at full level, and 15 is the
        // inverted 9 and so ends at silence (YM2608 application manual `&90`).
        let mut loud = ssg_at_full_level(SSG_ENABLE | SSG_ATTACK | SSG_HOLD);
        drive(&mut loud, 129, 0);
        assert_eq!(loud.output(), 0, "13 holds at full level");
        drive(&mut loud, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(loud.output(), 0, "and holds there");

        let mut silent = ssg_at_full_level(SSG_ENABLE | SSG_ATTACK | SSG_ALTERNATE | SSG_HOLD);
        drive(&mut silent, 129, 0);
        assert_eq!(silent.output(), SILENT, "15 holds at silence");
        drive(&mut silent, 4 * u32::from(COUNTER_MAX), 0);
        assert_eq!(silent.output(), SILENT, "and holds there");
    }

    #[test]
    fn ssg_mirrors_the_output_and_a_key_off_does_not() {
        // `&90` 12 is the enable with the attack bit, the mirror of the 8 that is
        // the plain repeated saw: the output is read mirrored about 0x200, which
        // is what makes a saw of a decay an upward saw of an attack (YM2608
        // application manual `&90`, and Nuked-OPN2's `ssg_level = 512 - level`).
        let mut envelope = ssg_at_full_level(SSG_ENABLE | SSG_ATTACK);
        for (att, out) in [(0, 0x200), (0x100, 0x100), (0x200, 0), (0x300, 0x300)] {
            envelope.att = att;
            assert_eq!(envelope.output(), out, "mirrored {att:#04x}");
        }

        // The total level still adds, and still clamps at silence.
        envelope.att = 0x200;
        envelope.total_level = 8;
        assert_eq!(envelope.output(), 0x40);

        // A key off forces the mirror off, and releases from where the mirror was
        // reading: Nuked applies `ssg_level` to the level itself on the key off,
        // so a mode 12 released at full level releases from 0x200.
        envelope.total_level = 0;
        envelope.att = 0;
        envelope.set_key(false, 0);
        assert_eq!(envelope.att, 0x200, "released from the mirrored level");
        assert_eq!(envelope.output(), 0x200, "and no longer through the mirror");
    }

    #[test]
    fn ssg_eg_register_reaches_its_operator() {
        // `&90` is a per-operator register laid out like `&40` to `&80`: the slot
        // offset in bits 3-2 and the channel in bits 1-0, so +0 is operator 1, +4
        // operator 3, +8 operator 2, +C operator 4 - and `&93`, the fourth address
        // of a part's group, is not decoded at all (YM2608 application manual
        // `&90`).
        let phases = PhaseGen::new();
        let mut eg = EnvelopeGen::new();
        eg.register_written(&phases, PART_I, REG_SSG_EG, SSG_ENABLE);
        eg.register_written(&phases, PART_I, REG_SSG_EG + 0x04, SSG_ENABLE | SSG_HOLD);
        eg.register_written(&phases, PART_I, REG_SSG_EG + 0x03, SSG_ENABLE);

        assert_eq!(eg.envelopes[0][SLOT_S1].ssg_eg, SSG_ENABLE);
        assert_eq!(eg.envelopes[0][SLOT_S3].ssg_eg, SSG_ENABLE | SSG_HOLD);
        assert_eq!(eg.envelopes[0][SLOT_S2].ssg_eg, 0);
        assert_eq!(
            eg.envelopes[0][SLOT_S4].ssg_eg, 0,
            "`&93` is the undecoded fourth channel of the group"
        );
    }
}
