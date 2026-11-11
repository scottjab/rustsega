//! SN76489 programmable sound generator (PSG).
//!
//! What lives here today is the register interface and the three square wave
//! tone channels of M5-01 (#105): the latch/data byte decoder that owns the
//! eight registers (`write`), the 4-bit attenuation with its 2 dB-per-step
//! table, and the per-channel tone generator that turns one /16 clock tick into
//! a signed sample (`clock`). The noise channel of M5-02 (#106) and the
//! resampling and mixing of M5-14 are not here yet, so channel 3 is a tone
//! channel and `clock` returns the raw sum of the four.
//!
//! Citations are to the SMS Power "SN76489" development document
//! (<https://www.smspower.org/Development/SN76489>) by section name, and to the
//! SpritesMind gendev forum thread 1132 for the behaviour of the Sega
//! 315-5313 integrated PSG - the part the Genesis carries - where it differs
//! from a discrete SN76489AN.

/// The four channels of the chip: three tone, plus the noise channel that
/// shares channel 3's output.
const CHANNELS: usize = 4;

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
    /// hardware or on a PSG test ROM.
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
    fn output(self) -> i16 {
        let level = VOLUME_TABLE[usize::from(self.attenuation)];
        if self.high { level } else { -level }
    }
}

/// Which register a data byte belongs to: the one the last latch byte named.
#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Debug)]
pub struct Sn76489 {
    channels: [Channel; CHANNELS],
    latch: Latch,
}

impl Sn76489 {
    /// A chip in its power-on state: every channel at attenuation 15, every
    /// tone period 0, the latch on channel 0's tone register.
    pub fn new() -> Self {
        Self {
            channels: [Channel::POWER_ON; CHANNELS],
            latch: Latch::POWER_ON,
        }
    }

    /// The tone period of `channel` as written, all 10 bits (channel 3 is the
    /// shared tone/noise register until #106 gives it a mode of its own).
    pub fn tone_period(&self, channel: usize) -> u16 {
        self.channels[channel].tone
    }

    /// The attenuation code of `channel`, 0 loudest to 15 silent.
    pub fn attenuation(&self, channel: usize) -> u8 {
        self.channels[channel].attenuation
    }

    /// The signed sample `channel` is driving into the sum right now.
    pub fn channel_output(&self, channel: usize) -> i16 {
        self.channels[channel].output()
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
    /// TODO(#106): channel 3 has one register shared by the tone period and the
    /// noise control, and once noise exists a data byte under a channel 3 latch
    /// writes the low four bits of the noise control instead of the high six bits
    /// of a period - and it is not ignored, which is how "Micro Machines" gets
    /// its noise right (SMS Power "SN76489", change log). Until then channel 3
    /// is a tone channel and follows the tone rule.
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
        let channel = &mut self.channels[self.latch.channel];
        if self.latch.volume {
            channel.attenuation = nibble;
        } else {
            channel.tone = (channel.tone & !0x0F) | u16::from(nibble);
        }
    }

    /// The data byte `0 x dddddd` of [`Sn76489::write`], for whichever register
    /// the last latch byte named: the high six bits of a tone period, the low
    /// four bits of a volume (SMS Power "SN76489", register writes).
    fn write_data_byte(&mut self, value: u8) {
        let channel = &mut self.channels[self.latch.channel];
        if self.latch.volume {
            channel.attenuation = value & 0x0F;
        } else {
            channel.tone = (channel.tone & 0x000F) | (u16::from(value & 0x3F) << 4);
        }
    }

    /// Advance every channel by one /16 clock tick - the Z80 clock divided by
    /// 16, about 223722 Hz (SMS Power "SN76489", tone generator) - and return
    /// the summed output of the four channels.
    pub fn clock(&mut self) -> i16 {
        let mut sum = 0i16;
        for channel in &mut self.channels {
            channel.tick();
            sum += channel.output();
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

    #[test]
    fn clock_returns_the_sum_of_the_channels() {
        let mut psg = Sn76489::new();
        for ch in 0..CHANNELS {
            psg.write(volume_latch(ch, 0));
            set_tone(&mut psg, ch, 1);
        }

        let mut previous = psg.clock();
        assert_eq!(previous.abs(), 4 * VOLUME_TABLE[0]);
        for _ in 0..7 {
            let sample = psg.clock();
            assert_eq!(sample.abs(), 4 * VOLUME_TABLE[0]);
            assert_eq!(sample, -previous);
            previous = sample;
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
}
