//! SN76489 programmable sound generator (PSG).
//!
//! What lives here today is the register interface, the three square wave tone
//! channels of M5-01 (#105) and the noise channel of M5-02 (#106): the
//! latch/data byte decoder that owns the eight registers (`write`), the 4-bit
//! attenuation with its 2 dB-per-step table, the per-channel tone generator and
//! the 16-bit noise shift register that together turn one /16 clock tick into a
//! signed sample (`clock`). The resampling and mixing of M5-14 are not here
//! yet, so `clock` returns the raw sum of the four channels.
//!
//! Citations are to the SMS Power "SN76489" development document
//! (<https://www.smspower.org/Development/SN76489>) by section name, and to the
//! SpritesMind gendev forum thread 1132 for the behaviour of the Sega
//! 315-5313 integrated PSG - the part the Genesis carries - where it differs
//! from a discrete SN76489AN.

// Serialising the chip state for save states (M6-07a, #126). Gated on the
// crate's `serde` feature so `cargo build --no-default-features` keeps the core
// with no dependencies at all, as docs/ARCHITECTURE.md asks of it.
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The four channels of the chip: three tone, plus the noise channel that
/// shares channel 3's output.
const CHANNELS: usize = 4;

/// Channel whose output pin the noise generator drives and whose tone register
/// it shares: `1 1 0 dddd` is a write to both, and the chip cannot tell them
/// apart (SMS Power "SN76489", noise).
const NOISE_CHANNEL: usize = 3;

/// Channel whose tone period becomes the noise shift period when the noise
/// control register selects shift rate 3 (#106).
const NOISE_RATE_TONE_CHANNEL: usize = 2;

/// Bit 2 of the noise control register: 1 for white noise, 0 for periodic noise
/// (SMS Power "SN76489", noise).
const NOISE_WHITE_BIT: u8 = 0b100;

/// The noise control register is three bits: `N H H`.
const NOISE_CONTROL_BITS: u8 = 0b111;

/// Shift period of noise shift rates 0, 1 and 2; rate 3 takes
/// [`NOISE_RATE_TONE_CHANNEL`]'s tone period instead (#106).
const NOISE_SHIFT_PERIODS: [u16; 3] = [0x10, 0x20, 0x40];

/// White noise feedback taps: bits 0 and 3. The Genesis part uses the same bit
/// pattern as the SMS and Game Gear parts (SMS Power "SN76489", change log).
const NOISE_WHITE_TAPS: u16 = 0x0009;

/// What every write to the noise register leaves in the shift register, and so
/// the state the noise cycle is measured from (#106).
const NOISE_RESET: u16 = 0x8000;

/// Bit 7 of a write: set means the byte latches channel and register type.
const LATCH_BIT: u8 = 0x80;
/// Where `cc`, the channel of the latch byte `1 cc t dddd`, sits.
const CHANNEL_SHIFT: u32 = 5;
/// Bit 4 of the latch byte, the `t` of `1 cc t dddd`: 0 selects the tone
/// register of that channel, 1 the volume register.
const VOLUME_BIT: u8 = 0x10;

/// Linear amplitude of each of the sixteen attenuation codes, 0 the loudest
/// and 15 silent.
///
/// `MAX * 10^(-2 * i / 20)` rounded to an integer with `MAX = 8191`, except
/// step 15, which is forced to 0 because "15" means silence rather than
/// "2 dB below step 14" (SMS Power "SN76489", volume control). Per-channel
/// `MAX` is 8191 so that the four channels of [`Sn76489::clock`] sum to at most
/// 32764 and cannot overflow the `i16` the mixer is handed; the mixer scales
/// later (M5-14).
const VOLUME_TABLE: [i16; 16] = [
    8191, 6506, 5168, 4105, 3261, 2590, 2057, 1634, 1298, 1031, 819, 651, 517, 411, 326, 0,
];

/// One channel: the tone generator and the attenuation applied to its output
/// bit.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct Channel {
    /// The 10-bit tone period as written by the register interface. Channel 3
    /// shares this one register with the noise control of #106.
    tone: u16,
    /// Ticks left before the output bit flips: 0 means it flips on the next
    /// [`Channel::tick`].
    counter: u16,
    /// The output bit: true drives `+volume`, false `-volume`.
    high: bool,
    /// Attenuation code, 0 loudest to 15 silent.
    attenuation: u8,
}

impl Channel {
    /// The power-on state of one channel of the Sega integrated part: tone data
    /// 0 and volume 15, which is why a fresh chip is silent rather than buzzing
    /// (SMS Power "SN76489", "The Sega integrated versions seem to start
    /// initialised with zeroes in the tone/noise registers and ones in the
    /// volume registers"). A discrete SN76489 wakes up with random registers
    /// and a tone; that is not the part the Genesis has, and the boot click is
    /// out of scope.
    const POWER_ON: Self = Self {
        tone: 0,
        counter: 0,
        high: false,
        attenuation: 15,
    };

    /// Advance the tone generator by one /16 clock tick.
    ///
    /// The counter counts down and reloads from the tone register when it
    /// reaches zero, flipping the output bit at that moment (SMS Power
    /// "SN76489", tone generator). Reloading with `period - 1` is what makes a
    /// period of N flip once every N ticks; reloading with `period` would flip
    /// every N + 1 ticks and would not give the period 1 behaviour below.
    ///
    /// `period` is never 0. On the Sega 315-5313 integrated PSG a tone of 0
    /// inverts on every internal clock cycle, exactly like a tone of 1: "setting
    /// the tone data to 0 produces the same result as setting the tone data to
    /// 1, namely, the square wave inverts on each internal clock cycle ... and
    /// all current emulators seem to use this wrong information" (SpritesMind
    /// gendev thread 1132). SMS Power's own tone-generator text says a tone of
    /// zero or one gives a constant +1 output, which is the discrete-chip
    /// behaviour and wrong here - do not "fix" this back to a constant level.
    ///
    /// INFERRED: neither the ticket nor the SMS Power text says what a register
    /// write does to a counter that is already running, so a write is treated as
    /// changing the reload value only - it sets the length of the next half
    /// period, not the phase of the current one. TODO(#105): pin this on
    /// hardware or on a PSG test ROM, follow-up filed as #321.
    fn tick(&mut self) {
        let period = self.tone.max(1);
        if self.counter == 0 {
            self.counter = period - 1;
            self.high = !self.high;
        } else {
            self.counter -= 1;
        }
    }

    /// The channel's sample: the attenuation level of the output bit, bipolar
    /// around 0. The model is the two output levels of the pin rather than the
    /// wave-shaped real one - the chip's output "is centred around zero" and
    /// the leakage that shapes the real wave is out of scope (SMS Power
    /// "SN76489", output), and the mixer removes the DC either way.
    /// The channel's sample for an explicit output bit: the two levels of the
    /// pin are plus and minus the attenuation level. Channel 3 uses this with
    /// the noise bit once [`Sn76489::noise_connected`] has connected the noise
    /// generator to its output.
    fn level(self, high: bool) -> i16 {
        let level = VOLUME_TABLE[usize::from(self.attenuation)];
        if high { level } else { -level }
    }
}

/// The noise generator: a 16-bit shift register clocked by its own divider, and
/// the three-bit control register that sets its feedback and its shift rate.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct Noise {
    /// The noise control register: bit 2 = 1 white / 0 periodic, bits 1-0 =
    /// shift rate, as written by `1 1 0 dddd`.
    control: u8,
    /// The shift register; bit 0 is the output.
    lfsr: u16,
    /// Ticks left before the divider flips, counted the way [`Channel::counter`]
    /// is.
    counter: u16,
    /// The divider's output bit; the shift register moves on its low to high
    /// transition.
    high: bool,
}

impl Noise {
    /// The power-on state: a zero control register, which is periodic noise at
    /// the slowest shift rate (SMS Power "SN76489": the Sega integrated part
    /// starts with zeroes in the noise registers). The value of the shift
    /// register is not specified anywhere and is not observable, because channel
    /// 3 does not carry the noise generator until its register has been written
    /// and that write reloads it with [`NOISE_RESET`].
    const POWER_ON: Self = Self {
        control: 0,
        lfsr: NOISE_RESET,
        counter: 0,
        high: false,
    };

    /// Whether the control register selects white noise.
    fn white(self) -> bool {
        self.control & NOISE_WHITE_BIT != 0
    }

    /// The output bit: bit 0 of the shift register.
    fn bit(self) -> bool {
        self.lfsr & 1 != 0
    }

    /// Advance the divider by one /16 tick. It flips like a tone channel does,
    /// and the shift register moves on the flip to high only, so a divider
    /// period of N shifts the register once every 2N ticks (#106).
    fn tick(&mut self, period: u16) {
        if self.counter == 0 {
            self.counter = period - 1;
            self.high = !self.high;
            if self.high {
                self.shift();
            }
        } else {
            self.counter -= 1;
        }
    }

    /// One shift of the register: white noise feeds back the parity of
    /// [`NOISE_WHITE_TAPS`], periodic noise feeds back bit 0 unchanged, and
    /// either way the feedback enters at bit 15 (#106).
    fn shift(&mut self) {
        let feedback = if self.white() {
            (self.lfsr & NOISE_WHITE_TAPS).count_ones() as u16 & 1
        } else {
            self.lfsr & 1
        };
        self.lfsr = (self.lfsr >> 1) | (feedback << 15);
    }
}

/// Which register a data byte belongs to: the one the last latch byte named.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct Latch {
    /// Channel the next data byte writes, 0-3.
    channel: usize,
    /// True for the volume register of that channel, false for its tone
    /// register (the noise control on channel 3, once #106 gives it one).
    volume: bool,
}

impl Latch {
    /// The latch state of a fresh chip: channel 0, tone register.
    const POWER_ON: Self = Self {
        channel: 0,
        volume: false,
    };
}

/// The PSG itself, seen from the Z80 write port and from the /16 clock.
///
/// ```text
/// let mut psg = sn76489::Sn76489::new();
/// psg.write(0x8F); // latch channel 0 tone, low four bits of the period
/// psg.write(0x0F); // and the high six
/// let sample = psg.clock(); // one /16 clock tick
/// ```
///
/// The `serde` derive carries every field below, so a restored chip resumes
/// mid-tick rather than restarting a channel's divider (M6-07a, #126). Nothing
/// is `#[serde(skip)]` here: the one table the chip has, `VOLUME_TABLE`, is a
/// constant rather than state, so there is nothing to rebuild on restore.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Sn76489 {
    channels: [Channel; CHANNELS],
    latch: Latch,
    noise: Noise,
    /// Whether channel 3's output pin carries the noise generator rather than
    /// channel 3's tone generator. Writing the shared channel 3 register
    /// connects it (SMS Power "SN76489", noise).
    ///
    /// INFERRED: nothing in the ticket or the retrieved facts says what, if
    /// anything, disconnects it again, so nothing does - a write that intends
    /// channel 3's tone period is indistinguishable from one that intends the
    /// noise control, and this model picks the noise generator for both. The
    /// chip has one register and two consumers, so the alternative reading is
    /// that both are mixed. TODO(#106): pin on hardware or on a PSG test ROM,
    /// filed as #323.
    noise_connected: bool,
}

impl Sn76489 {
    /// A chip in its power-on state: every channel at attenuation 15, every
    /// tone period 0, the latch on channel 0's tone register.
    pub fn new() -> Self {
        Self {
            channels: [Channel::POWER_ON; CHANNELS],
            latch: Latch::POWER_ON,
            noise: Noise::POWER_ON,
            noise_connected: false,
        }
    }

    /// The tone period of `channel` as written, all 10 bits.
    ///
    /// Channel 3 has one register shared by the tone period and the noise
    /// control, so for `channel == 3` this is that raw shared register: the low
    /// nibble of every noise write lands in it, exactly as it does in the chip.
    pub fn tone_period(&self, channel: usize) -> u16 {
        self.channels[channel].tone
    }

    /// The noise control register: bit 2 = 1 white / 0 periodic noise, bits 1-0
    /// = shift rate.
    pub fn noise_control(&self) -> u8 {
        self.noise.control
    }

    /// The attenuation code of `channel`, 0 loudest to 15 silent.
    pub fn attenuation(&self, channel: usize) -> u8 {
        self.channels[channel].attenuation
    }

    /// The signed sample `channel` is driving into the sum right now.
    pub fn channel_output(&self, channel: usize) -> i16 {
        self.channels[channel].level(self.output_bit(channel))
    }

    /// The output bit `channel` is driving. For channel 3 that is the noise
    /// shift register once the noise register has been written (#106).
    fn output_bit(&self, channel: usize) -> bool {
        if channel == NOISE_CHANNEL && self.noise_connected {
            self.noise.bit()
        } else {
            self.channels[channel].high
        }
    }

    /// Write one byte to the PSG data port.
    ///
    /// A byte with bit 7 set is a latch/data byte, `1 cc t dddd`: `cc` (bits
    /// 6-5) is the channel and `t` (bit 4) whether it is that channel's volume
    /// (1) or tone register (0, the noise control on channel 3); the byte
    /// latches both and writes its own low four bits as the low four bits of
    /// that register. A byte with bit 7 clear is a data byte for the latched
    /// register: the high six bits of a tone period, the low four bits of a
    /// volume (or of the noise control) (SMS Power "SN76489", register writes).
    ///
    /// Tone registers take effect on the byte that writes them and do not wait
    /// for the other half of the 10 bits, so the four bits of a latch byte are
    /// a complete period as far as the tone generator is concerned (SMS Power
    /// "SN76489", register writes, "tones update immediately after latch byte").
    ///
    /// Channel 3's register is shared by its tone period and the noise control,
    /// so a write to it is a write to both: the chip has no way to tell them
    /// apart, and a data byte under a channel 3 latch updates the low four bits
    /// of the noise control as well as the high six of the period. That data
    /// byte is not ignored - "Micro Machines" depends on it (SMS Power
    /// "SN76489", change log).
    pub fn write(&mut self, value: u8) {
        if value & LATCH_BIT != 0 {
            self.write_latch_byte(value);
        } else {
            self.write_data_byte(value);
        }
    }

    /// The latch byte `1 cc t dddd` of [`Sn76489::write`]: latch the channel and
    /// register type, then write this byte's own low four bits as the low four
    /// bits of that register (SMS Power "SN76489", register writes).
    fn write_latch_byte(&mut self, value: u8) {
        self.latch = Latch {
            channel: usize::from((value >> CHANNEL_SHIFT) & 0b11),
            volume: value & VOLUME_BIT != 0,
        };
        let nibble = value & 0x0F;
        let channel = self.latch.channel;
        if self.latch.volume {
            self.channels[channel].attenuation = nibble;
            return;
        }
        self.channels[channel].tone = (self.channels[channel].tone & !0x0F) | u16::from(nibble);
        if channel == NOISE_CHANNEL {
            self.write_noise(nibble);
        }
    }

    /// The data byte `0 x dddddd` of [`Sn76489::write`], for whichever register
    /// the last latch byte named: the high six bits of a tone period, the low
    /// four bits of a volume (SMS Power "SN76489", register writes).
    fn write_data_byte(&mut self, value: u8) {
        let channel = self.latch.channel;
        if self.latch.volume {
            self.channels[channel].attenuation = value & 0x0F;
            return;
        }
        self.channels[channel].tone =
            (self.channels[channel].tone & 0x000F) | (u16::from(value & 0x3F) << 4);
        if channel == NOISE_CHANNEL {
            self.write_noise(value & 0x0F);
        }
    }

    /// Write the noise control register and reset the shift register to
    /// [`NOISE_RESET`] (#106). Only the low three bits are the register; bit 3
    /// of the nibble is not part of it.
    ///
    /// INFERRED: neither the ticket nor the retrieved facts say whether a write
    /// also restarts the shift divider, so the divider keeps running and only
    /// the register is reloaded. TODO(#106): pin on hardware or on a PSG test
    /// ROM, filed as #323.
    fn write_noise(&mut self, control: u8) {
        self.noise.control = control & NOISE_CONTROL_BITS;
        self.noise.lfsr = NOISE_RESET;
        self.noise_connected = true;
    }

    /// How often the noise divider reloads: one of [`NOISE_SHIFT_PERIODS`], or
    /// the tone period of [`NOISE_RATE_TONE_CHANNEL`] for shift rate 3 (#106).
    /// At least 1 for the reason given on [`Channel::tick`].
    fn noise_shift_period(&self) -> u16 {
        match self.noise.control & 0b11 {
            3 => self.channels[NOISE_RATE_TONE_CHANNEL].tone.max(1),
            rate => NOISE_SHIFT_PERIODS[usize::from(rate)],
        }
    }

    /// Advance every channel by one /16 clock tick - the Z80 clock divided by
    /// 16, about 223722 Hz (SMS Power "SN76489", tone generator) - and return
    /// the summed output of the four channels.
    pub fn clock(&mut self) -> i16 {
        let period = self.noise_shift_period();
        self.noise.tick(period);

        let mut sum = 0i16;
        for channel in 0..CHANNELS {
            self.channels[channel].tick();
            let bit = self.output_bit(channel);
            sum += self.channels[channel].level(bit);
        }
        sum
    }
}

impl Default for Sn76489 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attenuation code that means silence (SMS Power "SN76489": 0 is the
    /// loudest, 15 the quietest step of a 2 dB-per-step table).
    const SILENT: u8 = 15;

    /// A chip with `channel` alone audible at attenuation `attenuation`: the
    /// other three are left at power-on attenuation, which is silence, so a
    /// `clock()` sum is that one channel's sample.
    fn lone_channel(channel: usize, attenuation: u8) -> Sn76489 {
        let mut psg = Sn76489::new();
        for ch in 0..CHANNELS {
            let att = if ch == channel { attenuation } else { SILENT };
            psg.write(volume_latch(ch, att));
        }
        psg
    }

    /// Latch byte selecting the volume register of `channel` and giving it
    /// `attenuation`: `1 cc 1 dddd` with `cc` in bits 6-5 (SMS Power
    /// "SN76489", register writes).
    fn volume_latch(channel: usize, attenuation: u8) -> u8 {
        0x90 | ((channel as u8) << 5) | attenuation
    }

    /// Select the tone register of `channel` and write all ten bits of
    /// `period`: the latch byte carries the low four, the data byte the high
    /// six.
    fn set_tone(psg: &mut Sn76489, channel: usize, period: u16) {
        psg.write(0x80 | ((channel as u8) << 5) | (period & 0x0F) as u8);
        psg.write(((period >> 4) & 0x3F) as u8);
    }

    /// How many times the output of `channel` changes sign over the next
    /// `ticks` ticks, counting the first tick against what the channel output
    /// before any of them.
    fn count_flips(psg: &mut Sn76489, channel: usize, ticks: usize) -> usize {
        let mut previous = psg.channel_output(channel);
        let mut flips = 0;
        for _ in 0..ticks {
            psg.clock();
            let output = psg.channel_output(channel);
            if output != previous {
                flips += 1;
            }
            previous = output;
        }
        flips
    }

    #[test]
    fn latch_sets_low_bits() {
        let mut psg = Sn76489::new();
        psg.write(0x8E);
        assert_eq!(psg.tone_period(0) & 0x0F, 0xE);
    }

    #[test]
    fn data_sets_high_bits() {
        let mut psg = Sn76489::new();
        psg.write(0x8E);
        psg.write(0x0F);
        assert_eq!(psg.tone_period(0), (0x0F << 4) | 0x0E);
    }

    #[test]
    fn volume_write() {
        let mut psg = Sn76489::new();
        psg.write(0x9F);
        assert_eq!(psg.attenuation(0), 15);
    }

    #[test]
    fn data_byte_after_volume_latch_updates_volume() {
        let mut psg = Sn76489::new();
        psg.write(0x90);
        assert_eq!(psg.attenuation(0), 0);
        psg.write(0x05);
        assert_eq!(psg.attenuation(0), 5);
    }

    #[test]
    fn latch_byte_only_touches_its_own_channel_and_register() {
        let mut psg = Sn76489::new();
        psg.write(volume_latch(2, 3));
        psg.write(0x80 | (1 << 5) | 0x7);

        assert_eq!(psg.attenuation(2), 3);
        assert_eq!(psg.tone_period(1), 7);
        assert_eq!(psg.attenuation(0), SILENT);
        assert_eq!(psg.attenuation(1), SILENT);
        assert_eq!(psg.tone_period(0), 0);
        assert_eq!(psg.tone_period(2), 0);
    }

    #[test]
    fn volume_table_2db_steps() {
        assert_eq!(VOLUME_TABLE[0], 8191);
        assert!((VOLUME_TABLE[1] - 6506).abs() <= 1);
        assert_eq!(VOLUME_TABLE[15], 0);
        for step in 1..VOLUME_TABLE.len() {
            assert!(
                VOLUME_TABLE[step] < VOLUME_TABLE[step - 1],
                "step {step} is not quieter than step {}",
                step - 1
            );
        }
    }

    #[test]
    fn volume_table_matches_the_two_db_formula() {
        for (step, table) in VOLUME_TABLE.iter().enumerate().take(15) {
            let want = (8191.0f64 * 10f64.powf(-2.0 * step as f64 / 20.0)).round() as i16;
            assert_eq!(*table, want, "step {step}");
        }
    }

    #[test]
    fn tone_period_10_toggles_every_10_ticks() {
        let mut psg = lone_channel(0, 0);
        set_tone(&mut psg, 0, 10);
        assert_eq!(count_flips(&mut psg, 0, 100), 10);
    }

    #[test]
    fn tone_period_1_toggles_on_every_tick() {
        let mut psg = lone_channel(0, 0);
        set_tone(&mut psg, 0, 1);
        assert_eq!(count_flips(&mut psg, 0, 16), 16);
    }

    #[test]
    fn tone_period_zero_behaves_like_period_one() {
        let mut zero = lone_channel(0, 0);
        set_tone(&mut zero, 0, 0);
        let mut one = lone_channel(0, 0);
        set_tone(&mut one, 0, 1);

        for _ in 0..64 {
            assert_eq!(zero.clock(), one.clock());
        }
    }

    #[test]
    fn tone_register_updates_from_the_latch_byte_alone() {
        let mut psg = lone_channel(0, 0);
        psg.write(0x80 | 6);
        assert_eq!(psg.tone_period(0), 6);
        assert_eq!(count_flips(&mut psg, 0, 6), 1);
    }

    #[test]
    fn tone_data_byte_takes_effect_immediately() {
        let mut psg = lone_channel(0, 0);
        set_tone(&mut psg, 0, 1);
        assert_eq!(count_flips(&mut psg, 0, 8), 8);

        psg.write(0x80);
        psg.write(0x20);
        assert_eq!(psg.tone_period(0), 0x200);

        let flips = count_flips(&mut psg, 0, 8);
        assert!(
            flips <= 1,
            "the 512 period is not live: {flips} flips in 8 ticks"
        );
    }

    #[test]
    fn channel_output_is_the_signed_volume_table_entry() {
        let mut psg = Sn76489::new();
        psg.write(volume_latch(0, 5));

        let mut previous = psg.channel_output(0);
        for _ in 0..8 {
            psg.clock();
            let output = psg.channel_output(0);
            assert_eq!(output.abs(), VOLUME_TABLE[5]);
            assert_eq!(output, -previous);
            previous = output;
        }
    }

    /// `clock` is the sum of the four channels, with every channel pinned: the
    /// three tone channels at full scale and channel 3 on the noise generator.
    ///
    /// This test of #105 used to expect plus or minus four times
    /// `VOLUME_TABLE[0]` on every tick, which asked channel 3 to keep being a
    /// tone channel. It cannot: the noise control register is channel 3's tone
    /// register, so the `set_tone(_, 3, _)` below is a write to the noise
    /// register, and that connects the noise generator to channel 3's output for
    /// the life of the chip (#106). Measured before the change: 16382 where
    /// 32764 was expected.
    #[test]
    fn clock_returns_the_sum_of_the_channels() {
        let mut psg = Sn76489::new();
        for ch in 0..CHANNELS {
            psg.write(volume_latch(ch, 0));
            set_tone(&mut psg, ch, 1);
        }

        // Power-on output bit low for every channel, and channel 3 is the noise
        // bit 0 of the shift register the last write left at 0x8000.
        let mut tone = -VOLUME_TABLE[0];
        let noise = -VOLUME_TABLE[0];

        for _ in 0..8 {
            let sample = psg.clock();

            assert_eq!(
                sample,
                (0..CHANNELS).map(|ch| psg.channel_output(ch)).sum::<i16>(),
                "clock is not the sum of the channels"
            );

            tone = -tone;
            for ch in 0..3 {
                assert_eq!(psg.channel_output(ch), tone, "tone channel {ch}");
            }

            assert_eq!(
                psg.channel_output(3),
                noise,
                "channel 3 is on the noise generator, not on the tone period it was written as"
            );
        }
    }

    #[test]
    fn silent_on_power_on() {
        let mut psg = Sn76489::new();
        for _ in 0..1000 {
            assert_eq!(psg.clock(), 0);
        }
    }

    #[test]
    fn power_on_state_is_zero_tone_and_silent_volume() {
        let psg = Sn76489::new();
        for ch in 0..CHANNELS {
            assert_eq!(psg.attenuation(ch), SILENT);
            assert_eq!(psg.tone_period(ch), 0);
            assert_eq!(psg.channel_output(ch), 0);
        }
    }

    // Noise channel (#106).

    /// Longest wait for the noise shift register to move before a test gives up:
    /// the slowest fixed shift rate is 0x40 and a shift is due within twice
    /// that, so a register that has not moved in this many ticks is stopped
    /// rather than slow. Bounded so a wrong tap fails a named test instead of
    /// hanging CI.
    const MAX_SHIFT_TICKS: usize = 4096;

    /// Hard cap on walking a 16-bit shift register round: there are 65536
    /// states, so a cycle that has not closed by here never will.
    const MAX_SHIFT_CYCLE: usize = 1 << 16;

    /// Latch byte writing `control` to the noise control register: `1 1 0 dddd`,
    /// the channel 3 tone register that the noise generator shares.
    fn noise_latch(control: u8) -> u8 {
        0xE0 | (control & 0x0F)
    }

    /// Clock until the noise shift register moves - one shift - and report how
    /// many /16 ticks that took. Hard-capped: a shift register that has stopped
    /// must fail this test, not hang the run.
    fn ticks_to_noise_shift(psg: &mut Sn76489) -> usize {
        let start = psg.noise.lfsr;
        for tick in 1..=MAX_SHIFT_TICKS {
            psg.clock();
            if psg.noise.lfsr != start {
                return tick;
            }
        }
        panic!("the noise shift register did not move in {MAX_SHIFT_TICKS} ticks");
    }

    /// The output bit of the noise generator, sampled once at each of the next
    /// `count` shifts.
    fn noise_bits(psg: &mut Sn76489, count: usize) -> Vec<bool> {
        (0..count)
            .map(|_| {
                ticks_to_noise_shift(psg);
                psg.noise.lfsr & 1 != 0
            })
            .collect()
    }

    #[test]
    fn noise_write_resets_lfsr() {
        let mut psg = Sn76489::new();
        psg.write(noise_latch(0b101));
        ticks_to_noise_shift(&mut psg);
        assert_ne!(psg.noise.lfsr, NOISE_RESET, "the shift register must run");

        psg.write(noise_latch(0b100));
        assert_eq!(psg.noise.lfsr, NOISE_RESET);
    }

    /// SMS Power "SN76489": setting the noise register to 101 and then updating
    /// it to 100 with a plain data byte has to take effect - the data byte is
    /// not ignored - or "Micro Machines" gets the wrong sound on its noise
    /// channel.
    #[test]
    fn noise_data_byte_is_not_ignored() {
        let mut psg = Sn76489::new();
        psg.write(noise_latch(0b101));
        assert_eq!(psg.noise_control(), 0b101);
        ticks_to_noise_shift(&mut psg);
        assert_ne!(psg.noise.lfsr, NOISE_RESET);

        psg.write(0x04);

        assert_eq!(psg.noise_control(), 0b100);
        assert_eq!(
            psg.noise.lfsr, NOISE_RESET,
            "a noise update through a data byte resets the shift register too"
        );
    }

    /// The three fixed shift rates of the noise control register: 0 -> 0x10,
    /// 1 -> 0x20, 2 -> 0x40, counted in ticks between two shifts of a divider
    /// that shifts once per two periods (#106).
    #[test]
    fn noise_shift_rates() {
        for (rate, period) in NOISE_SHIFT_PERIODS.iter().enumerate() {
            let mut psg = Sn76489::new();
            psg.write(noise_latch(rate as u8));
            ticks_to_noise_shift(&mut psg);
            assert_eq!(ticks_to_noise_shift(&mut psg), 2 * usize::from(*period));
        }
    }

    #[test]
    fn periodic_noise_period_16() {
        let mut psg = Sn76489::new();
        psg.write(noise_latch(0b001));

        let bits = noise_bits(&mut psg, 48);
        for index in 0..32 {
            assert_eq!(
                bits[index],
                bits[index + 16],
                "periodic noise bit {index} did not repeat after 16 shifts"
            );
        }
        let one = bits[..16].iter().any(|bit| *bit);
        let zero = bits[..16].iter().any(|bit| !*bit);
        assert!(one && zero, "periodic noise is a constant, not a cycle");
    }

    #[test]
    fn white_noise_first_values() {
        let mut psg = Sn76489::new();
        psg.write(noise_latch(0b100));

        let mut want = Vec::new();
        let mut lfsr = NOISE_RESET;
        for _ in 0..32 {
            let feedback = (lfsr & NOISE_WHITE_TAPS).count_ones() as u16 & 1;
            lfsr = (lfsr >> 1) | (feedback << 15);
            want.push(lfsr & 1 != 0);
        }

        assert_eq!(noise_bits(&mut psg, 32), want);
    }

    /// The cycle of the white noise shift register, walked to its end: 57337
    /// shifts before the register is back at the value a write left it at
    /// (#106, from SMS Power's noise channel figures).
    ///
    /// A good check on the taps, because the number is a property of them: the
    /// feedback polynomial of this register factors into an irreducible of
    /// degree 13 and one of degree 3, so the cycle the reset value 0x8000 sits
    /// in is lcm(8191, 7) = 57337 shifts, and any other tap pair gives some
    /// other number. The walk is capped at one pass over the 16-bit state space,
    /// so a wrong tap fails this test by name rather than hanging.
    #[test]
    fn white_noise_period_57337() {
        let mut psg = Sn76489::new();
        psg.write(noise_latch(0b100));
        let start = psg.noise.lfsr;

        let mut shifts = 0;
        loop {
            shifts += 1;
            assert!(
                shifts <= MAX_SHIFT_CYCLE,
                "white noise had not returned to {start:#06x} after {shifts} shifts"
            );
            ticks_to_noise_shift(&mut psg);
            if psg.noise.lfsr == start {
                break;
            }
        }

        assert_eq!(shifts, 57337, "white noise cycle length");
    }

    #[test]
    fn rate3_follows_tone2() {
        let mut psg = Sn76489::new();
        set_tone(&mut psg, 2, 8);
        psg.write(noise_latch(0b011));

        ticks_to_noise_shift(&mut psg);
        assert_eq!(ticks_to_noise_shift(&mut psg), 2 * 8);

        set_tone(&mut psg, 2, 32);
        ticks_to_noise_shift(&mut psg);
        assert_eq!(ticks_to_noise_shift(&mut psg), 2 * 32);
    }

    /// Channel 3's pin carries the noise bit once the noise register has been
    /// written, through channel 3's own attenuator (#106).
    #[test]
    fn noise_drives_channel_3_output() {
        let mut psg = Sn76489::new();
        psg.write(volume_latch(3, 0));
        psg.write(noise_latch(0b100));

        assert_eq!(psg.channel_output(3), -VOLUME_TABLE[0]);
        ticks_to_noise_shift(&mut psg);
        assert_eq!(psg.channel_output(3) > 0, psg.noise.lfsr & 1 != 0);
    }
}
