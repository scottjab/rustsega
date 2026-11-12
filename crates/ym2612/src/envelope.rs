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
//! The amplitude modulation of `&60` bit 7 is LFO work and comes in M5-08
//! (#113), and the SSG-EG of `&90` in M5-10 (#115). Neither is read here, so
//! what [`Envelope::output`] gives is the operator's attenuation as far as this
//! ticket takes it.

use crate::phase::{self, CHANNELS, PhaseGen, SLOTS};
use crate::regs::{
    REG_AM_DECAY_1, REG_DECAY_2, REG_KEY_ON, REG_KEY_SCALE_ATTACK, REG_LEVEL_RELEASE,
    REG_TOTAL_LEVEL, operator_of_group,
};

/// The attenuation of a silent operator: 10 bits, and the other end from it is
/// 0 (YM2608 application manual `&40`).
pub(crate) const SILENT: u16 = 0x3FF;

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
    /// `&70` bits 4-0, the decay 2 rate.
    decay2_rate: u8,
    /// `&80` bits 7-4, the sustain level.
    sustain_level: u8,
    /// `&80` bits 3-0, the release rate.
    release_rate: u8,
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
            decay2_rate: 0,
            sustain_level: 0,
            release_rate: 0,
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
        self.key_on = on;
        self.state = if on { State::Attack } else { State::Release };
        if on && effective_rate(self.attack_rate, self.key_scale, keycode) >= INSTANT_RATE {
            self.att = 0;
        }
    }

    /// One envelope update at key code `keycode`, with the chip-wide counter at
    /// `counter`.
    fn update(&mut self, counter: u16, keycode: u8) {
        // The states move first: an attack that has reached 0 is decay, and a
        // decay that has reached its sustain level is sustain.
        if self.state == State::Attack && self.att == 0 {
            self.state = State::Decay;
        }
        if self.state == State::Decay && self.att >= sustain_attenuation(self.sustain_level) {
            self.state = State::Sustain;
        }

        let inc = increment(self.rate(keycode), counter);
        if inc == 0 {
            return;
        }

        if self.state == State::Attack {
            self.att = attack_step(self.att, inc);
            if self.att == 0 {
                self.state = State::Decay;
            }
        } else {
            // Decay, sustain and release are all linear, and release is not
            // stopped by the sustain level: it is only ever clamped at silence.
            self.att = (self.att + u16::from(inc)).min(SILENT);
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

    /// What the operator contributes to the channel: its own attenuation plus
    /// the total level of `&40` at 8 attenuation counts a step, clamped at
    /// silence (YM2608 application manual `&40`).
    ///
    /// `#[cfg(test)]` until the slot output of the next ticket reads it, for the
    /// same reason [`PhaseGen::channel`] is: a YM2612 cannot be read back. The
    /// amplitude modulation of M5-08 (#113) is added on top there, not here.
    #[cfg(test)]
    pub(crate) fn output(&self) -> u16 {
        (self.att + (u16::from(self.total_level) << 3)).min(SILENT)
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
        let keycode = phases.keycode(channel);
        for &(mask, slot) in &phase::KEY_ON_BITS {
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

        for channel in 0..CHANNELS {
            let keycode = phases.keycode(channel);
            for slot in 0..SLOTS {
                self.envelopes[channel][slot].update(self.counter, keycode);
            }
        }
    }

    /// What one operator's envelope holds right now, for `Ym2612`'s tests: the
    /// slot indices of [`phase::SLOT_S1`] and friends.
    #[cfg(test)]
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
    /// `&60` bits 4-0 the decay 1 rate; bit 7 is the amplitude modulation of
    /// M5-08 (#113) and is not read here.
    Decay1Rate,
    /// `&70` bits 4-0 the decay 2, sustaining, rate.
    Decay2Rate,
    /// `&80` bits 7-4 the sustain level and 3-0 the release rate.
    LevelRelease,
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
            Self::Decay1Rate => envelope.decay1_rate = value & 0x1F,
            Self::Decay2Rate => envelope.decay2_rate = value & 0x1F,
            Self::LevelRelease => {
                envelope.sustain_level = (value >> 4) & 0x0F;
                envelope.release_rate = value & 0x0F;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::PhaseGen;
    use crate::phase::SLOT_S1;
    use crate::regs::PART_I;

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
}
