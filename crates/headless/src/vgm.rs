//! The `vgm` subcommand: play a VGM file through the two audio chips, so audio
//! accuracy can be heard and hashed without a game (M5-12, #117).
//!
//! A VGM is a chip-level recording. Its header gives the chips' clock rates and
//! its body is a stream of commands that write the chips' registers and say how
//! long to wait between the writes, so there is no machine to run: what the file
//! needs is the two chips driven exactly as `genesis::Audio` drives them, read
//! out to the 44100 Hz the file counts time in (VGM 1.71 spec, "File
//! Specifics"; docs/ARCHITECTURE.md, "Audio chips").
//!
//! Three of the encodings are worth restating, because a first reading of the
//! command list gets all three wrong:
//!
//! * `0x7n` is **one byte** and waits n + 1 samples - there is no operand.
//! * `0x8n` is **one byte**, writes the *next byte of the data bank* to YM2612
//!   `&2A` - the sample is not in the command - and then waits n samples, where
//!   n is n and not n + 1.
//! * `0x67 0x66 tt ssssssss (data)` is a data block of 7 + s bytes: the tag, the
//!   `0x66` that stops older players parsing, a one-byte type, a 32-bit length
//!   and the bytes. Type `0x00` is the YM2612 PCM bank that `0x8n` reads.
//!
//! Out of scope, per the ticket: every other chip, and looping. A command for a
//! chip this player does not have stops the file with a message naming it,
//! because guessing an operand length would silently mis-decode everything after
//! it into noise.
//!
//! The mix is not [`genesis::Mixer`]: that is the console's own band-limited
//! resampler to the host rate, and a test tool needs a resampler small enough to
//! reason about when a hash moves. So each chip is clocked at its own native
//! rate - the YM at `clock / 144`, the PSG at `clock / 16`, both straight out of
//! the header - and read through [`Stream`], which is two taps and a linear
//! interpolation in integer arithmetic. Linear leaves the chips' images
//! unfiltered, which is fine for tests and is why [`YM_GAIN`] and [`PSG_GAIN`]
//! are the only two figures borrowed from the real mixer.

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use flate2::read::GzDecoder;
use sn76489::Sn76489;
use ym2612::Ym2612;

use crate::wav;

/// The rate of the output, which is the rate a VGM counts time in: "VGMs run
/// with a rate of 44100 Hz. The unit of time is called sample" (VGM 1.71 spec,
/// "File Specifics").
pub const OUTPUT_RATE: u32 = 44_100;

/// The two bytes a gzip stream starts with, which is how a `.vgz` says what it
/// is: the extension is a hint, these are the fact (VGM 1.71 spec,
/// "Introduction").
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
/// The version from which the data offset at `0x34` is written; before it the
/// data always starts at `0x40`.
const VERSION_1_50: u32 = 0x0150;
/// Bit 31 of the YM2612 clock field: set for the integrated YM3438 rather than a
/// discrete YM2612 (VGM 1.51+). It is a different DAC, not a different register
/// set, and this player models the discrete one for both - see
/// [`Header::ym2612_clock`].
const YM_IS_3438: u32 = 0x8000_0000;

/// YM2612 internal clocks in one output sample, so the chip's own sample rate is
/// `clock / 144` (docs/ARCHITECTURE.md, "Clocks and timing").
const YM_DIVISOR: u64 = 144;
/// The divider between an SN76489's clock input and the tick its tone and noise
/// generators run on (SMS Power, "SN76489", tone generator).
const PSG_DIVISOR: u64 = 16;

/// What an FM sample is multiplied by on its way into the mix: `genesis`'
/// `YM_OUTPUT_GAIN`, the `>> 5` of `ym2612`'s own output stage put back, so a
/// VGM is balanced against the PSG here the way it is on the console
/// (`crates/genesis/src/audio.rs`).
const YM_GAIN: i32 = 32;
/// What a PSG sample is multiplied by: `4200/8191`, which is `genesis`'s
/// `PSG_LEVEL_REFERENCE` at `Config::psg_level` 100 - the VA4 Model 1 PSG/FM
/// balance that core measured rather than guessed. Integer, like everything in
/// the mix, so a hash of this output means the same on every machine.
const PSG_GAIN_NUM: i32 = 4_200;
/// The denominator of [`PSG_GAIN_NUM`].
const PSG_GAIN_DEN: i32 = 8_191;

/// The YM2612 register the data bank streams into: `&2A`, whose 8-bit PCM sample
/// is unsigned and centred on `0x80` (YM2608 application manual `&2A`).
const REG_DAC_DATA: u8 = 0x2A;
/// What to write to `&2A` once the bank has run out: `0x80` is the centre of the
/// PCM scale, so a file that streams past its own data goes quiet rather than
/// stepping to full scale.
const DAC_CENTRE: u8 = 0x80;
/// The `0x67` data block type that is the YM2612's PCM data bank; every other
/// type belongs to a chip this player does not have.
const BLOCK_YM2612_PCM: u8 = 0x00;

/// The `vgm` arguments, as clap sees them.
#[derive(Debug, Clone, Args)]
pub struct VgmArgs {
    /// VGM or VGZ file to play. A `.vgz` is inflated first, by its contents and
    /// not by its name.
    #[arg(value_name = "VGM")]
    pub file: PathBuf,
    /// Write what the file plays to `PATH` as a 16-bit stereo PCM WAV at
    /// [`OUTPUT_RATE`].
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// Play `args` and write the WAV: the whole file, to its own end command.
pub fn run(args: VgmArgs) -> Result<()> {
    let file =
        fs::read(&args.file).with_context(|| format!("cannot read VGM {}", quote(&args.file)))?;
    let song =
        Vgm::load(&file).with_context(|| format!("cannot play VGM {}", quote(&args.file)))?;
    let samples = song.play(None);

    if let Some(path) = &args.out {
        wav::write_wav(path, OUTPUT_RATE, &samples)
            .with_context(|| format!("cannot write WAV {}", quote(path)))?;
    }

    let header = song.header();
    eprintln!(
        "vgm version={} ym2612_clock={} sn76489_clock={} samples={} time={:.3}s",
        version_text(header.version),
        header.ym2612_clock,
        header.sn76489_clock,
        samples.len() / 2,
        samples.len() as f64 / f64::from(OUTPUT_RATE) / 2.0,
    );
    Ok(())
}

/// A path in quotes, so a name with spaces in it still reads as one item.
fn quote(path: &Path) -> String {
    format!("{:?}", path)
}

/// The BCD version of `0x08` as the text a person reads: 0x0000_0171 is 1.71
/// (VGM 1.71 spec: "Version number in BCD-Code").
fn version_text(version: u32) -> String {
    let digits = |byte: u8| u32::from(byte >> 4) * 10 + u32::from(byte & 0x0F);
    format!(
        "{}.{:02}",
        digits(((version >> 8) & 0xFF) as u8),
        digits((version & 0xFF) as u8),
    )
}

/// A parsed VGM: the clocks, the command stream, and how long it runs for.
pub struct Vgm {
    /// The header fields this player reads.
    header: Header,
    /// The command stream, with the header and anything past the EoF offset cut
    /// away from it.
    data: Vec<u8>,
    /// The sum of the file's waits, in [`OUTPUT_RATE`] samples: what the walk
    /// `parse` does to validate the stream counts.
    samples: usize,
}

/// The part of the VGM header this player reads (VGM 1.71 spec, "Header").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// `0x08`, in BCD: 0x0000_0171 is version 1.71.
    pub version: u32,
    /// `0x0C`: the SN76489's input clock in Hz, and 0 when the file has no PSG.
    pub sn76489_clock: u32,
    /// `0x2C`: the YM2612's input clock in Hz, and 0 when the file has no FM.
    ///
    /// Bit 31 of that field is not part of the rate: it says the machine has the
    /// integrated YM3438 rather than a discrete YM2612, which is a different DAC
    /// [`ym2612::ChipModel`] rather than a different register set. It is masked
    /// off here and the discrete model is used either way, because a VGM of a
    /// Model 2 has to come with a `Config` saying so to be heard differently and
    /// this command has no console to ask.
    pub ym2612_clock: u32,
}

impl Vgm {
    /// Parse an uncompressed VGM image.
    ///
    /// The command stream is walked here rather than at `play`, so that a
    /// truncated or corrupt one is an error from `parse` naming the byte it
    /// tripped on and nothing downstream has to cope with it again.
    pub fn parse(bytes: &[u8]) -> Result<Vgm> {
        let header = Header::read(bytes)?;
        let start = data_start(bytes, header.version)?;
        let end = end_of_data(bytes, start);
        if end < start {
            bail!(
                "VGM says its data ends at {start} and its file ends at {end}, \
                 so there is no command stream to play"
            );
        }

        let data = bytes[start..end].to_vec();
        let samples = walk(&data, |_| {})?;
        Ok(Self {
            header,
            data,
            samples,
        })
    }

    /// Parse a VGM file's bytes, inflating them first if they are gzip - which
    /// is all a `.vgz` is, and the commonest thing anyone will hand this
    /// command.
    pub fn load(bytes: &[u8]) -> Result<Vgm> {
        if !bytes.starts_with(&GZIP_MAGIC) {
            return Self::parse(bytes);
        }
        let mut inflated = Vec::new();
        GzDecoder::new(bytes)
            .read_to_end(&mut inflated)
            .context("cannot inflate this .vgz")?;
        Self::parse(&inflated).map_err(|err| err.context("broken after inflating the .vgz"))
    }

    /// The header the clocks came from.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// How long the file runs for, in [`OUTPUT_RATE`] samples: the sum of its
    /// waits, which is what a file's own `0x38` sample count claims to be.
    pub fn samples(&self) -> usize {
        self.samples
    }

    /// Play the file: both chips clocked at the header's rates, mixed and
    /// resampled to interleaved stereo `i16` at [`OUTPUT_RATE`].
    ///
    /// `limit` caps the output at that many samples - `Some(4410)` is a tenth of
    /// a second - and `None` plays to the file's own end command.
    pub fn play(&self, limit: Option<usize>) -> Vec<i16> {
        let mut player = Player::new(self, limit.unwrap_or(usize::MAX));
        // `parse` walked this stream and accepted it, so the only failure left
        // is one `Player` stopped by its own limit, which is not an error.
        let _ = walk(&self.data, |command| player.command(command));
        player.out
    }
}

impl Header {
    /// Read `0x08`, `0x0C` and `0x2C`, having checked the magic and the length.
    fn read(bytes: &[u8]) -> Result<Header> {
        if bytes.len() < 0x40 {
            bail!("VGM is {} bytes, and its header alone is 64", bytes.len());
        }
        if &bytes[0..4] != b"Vgm " {
            bail!("not a VGM: it starts with {:?}, not \"Vgm \"", &bytes[0..4]);
        }
        Ok(Self {
            version: u32_at(bytes, 0x08),
            sn76489_clock: u32_at(bytes, 0x0C),
            ym2612_clock: u32_at(bytes, 0x2C) & !YM_IS_3438,
        })
    }
}

/// Where a VGM's command stream starts: the field at `0x34` is relative to
/// `0x34` and only written from 1.50, so before that - and whenever it is 0,
/// which no file means as "the header" - the data is at `0x40` (VGM 1.71 spec,
/// "0x34: VGM data offset").
fn data_start(bytes: &[u8], version: u32) -> Result<usize> {
    let offset = u32_at(bytes, 0x34);
    if version < VERSION_1_50 || offset == 0 {
        return Ok(0x40);
    }
    let start = 0x34_usize
        .checked_add(usize::try_from(offset).unwrap_or(usize::MAX))
        .ok_or_else(|| anyhow!("VGM data offset {offset:#x} is out of range here"))?;
    if start > bytes.len() {
        bail!(
            "VGM data offset {start:#x} is past the end of a {} byte file",
            bytes.len()
        );
    }
    Ok(start)
}

/// Where a VGM's command stream ends. `0x04` is the EoF offset relative to
/// `0x04` - file length minus 4 - so that the next track of a concatenated
/// player stub can be found after this one, and it is a courtesy rather than a
/// promise: a file that leaves it zero, or points it before its own data, gets
/// the end of the buffer instead (VGM 1.71 spec, "0x04: Eof offset").
fn end_of_data(bytes: &[u8], start: usize) -> usize {
    let declared = 4 + usize::try_from(u32_at(bytes, 0x04)).unwrap_or(usize::MAX - 4);
    std::cmp::min(std::cmp::max(declared, start), bytes.len())
}

/// One decoded VGM command, as far as this player is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command<'a> {
    /// `0x50 dd`: one byte to the SN76489's data port.
    PsgWrite(u8),
    /// `0x52`/`0x53 aa dd`: `register` = `value` on YM2612 part `part`, which is
    /// 0 for part I and 1 for part II.
    FmWrite { part: u8, register: u8, value: u8 },
    /// `0x61`/`0x62`/`0x63`/`0x7n`: wait `n` samples of the 44100 Hz timeline.
    Wait(u32),
    /// A `0x67` data block: its type and its bytes. Type [`BLOCK_YM2612_PCM`]
    /// is the YM2612's PCM data bank.
    DataBlock { kind: u8, data: &'a [u8] },
    /// `0x8n`: write the next byte of the YM2612 data bank to `&2A`, then wait
    /// `wait` samples.
    PcmWrite { wait: u32 },
    /// `0xE0 dddddddd`: move the YM2612 data bank's read position.
    PcmSeek(u32),
    /// `0x66`: end of sound data.
    End,
}

/// Walk a command stream, handing every command to `command`, and return the
/// total of its waits in [`OUTPUT_RATE`] samples.
///
/// This is the whole of this file's knowledge of the VGM command set (VGM 1.71
/// spec, "Commands"). A command it cannot place is an error rather than a skip:
/// a command of unknown length mis-decodes the rest of the file into noise, and
/// noise is a worse answer than a message.
fn walk<'a, F>(data: &'a [u8], mut command: F) -> Result<usize>
where
    F: FnMut(Command<'a>),
{
    let mut samples = 0usize;
    let mut rest = data;
    let mut at = 0usize;
    loop {
        let Some((&code, operands)) = rest.split_first() else {
            // A stream that runs out without an end command is a short file
            // rather than a broken one: stop where the data stopped.
            return Ok(samples);
        };
        let (decoded, used) = decode(code, operands, at)?;
        rest = &operands[used..];
        at += used + 1;
        if decoded == Command::End {
            return Ok(samples);
        }
        samples += match decoded {
            Command::Wait(n) => usize::try_from(n).unwrap_or(usize::MAX),
            Command::PcmWrite { wait } => usize::try_from(wait).unwrap_or(usize::MAX),
            _ => 0,
        };
        command(decoded);
    }
}

/// The command `code` and its length in bytes including the opcode itself,
/// reading its operands off `operands` and naming `at` - the command's own
/// offset - when the file is too short for them.
fn decode<'a>(code: u8, operands: &'a [u8], at: usize) -> Result<(Command<'a>, usize)> {
    let short =
        || anyhow!("VGM command {code:#04x} at offset {at:#x} is cut short by the end of the file");
    let need = |n| operands.get(..n).ok_or_else(short);

    let decoded = match code {
        // "no-operation, no file should contain these, but players should just
        // skip them" (VGM 1.71 spec, "Commands").
        0x00 => return Ok((Command::Wait(0), 0)),
        0x50 => (Command::PsgWrite(need(1)?[0]), 1),
        0x52 | 0x53 => {
            let operands = need(2)?;
            (
                Command::FmWrite {
                    part: code - 0x52,
                    register: operands[0],
                    value: operands[1],
                },
                2,
            )
        }
        0x61 => (Command::Wait(u32::from(u16_at(need(2)?, 0))), 2),
        // The two frame waits, "a shortcut for 0x61 0xdf 0x02" and its 50 Hz
        // twin.
        0x62 => (Command::Wait(735), 0),
        0x63 => (Command::Wait(882), 0),
        0x66 => (Command::End, 0),
        0x67 => {
            let head = need(6)?;
            if head[0] != 0x66 {
                bail!(
                    "VGM data block at offset {:#x} does not carry the 0x66 marker, \
                     so it cannot be trusted to be a data block",
                    at
                );
            }
            let length = usize::try_from(u32_at(&head[2..], 0)).map_err(|_| {
                anyhow!("VGM data block at offset {at:#x} is too big for this machine")
            })?;
            let data = operands.get(6..6 + length).ok_or_else(short)?;
            (
                Command::DataBlock {
                    kind: head[1],
                    data,
                },
                6 + length,
            )
        }
        0x70..=0x7f => (Command::Wait(u32::from(code & 0x0F) + 1), 0),
        0x80..=0x8f => (
            Command::PcmWrite {
                wait: u32::from(code & 0x0F),
            },
            0,
        ),
        0xe0 => (Command::PcmSeek(u32_at(need(4)?, 0)), 4),
        other => bail!(
            "VGM command {other:#04x} at offset {at:#x} is not one this player knows: \
             it plays the YM2612 and the SN76489, and no other chip"
        ),
    };
    Ok(decoded)
}

/// A little-endian `u32` at `at`, where every caller has already checked the
/// bytes are there.
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut value = [0_u8; 4];
    value.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(value)
}

/// A little-endian `u16` at `at`, same promise as [`u32_at`].
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// The two chips, their streams, and the output they are filling.
struct Player<'a> {
    /// The FM, at `ym2612_clock / 144`.
    ym: Ym2612,
    /// The PSG, at `sn76489_clock / 16`.
    psg: Sn76489,
    /// The FM read at [`OUTPUT_RATE`], and the PSG likewise.
    ym_stream: Stream<(i16, i16)>,
    psg_stream: Stream<i16>,
    /// The YM2612 PCM data bank a `0x67` loaded, and where `0x8n` has got to.
    bank: &'a [u8],
    bank_pos: usize,
    /// The interleaved stereo output so far.
    out: Vec<i16>,
    /// Output samples produced, which is where the two streams read next.
    at: u64,
    /// The most samples [`Vgm::play`] will produce.
    limit: usize,
}

impl<'a> Player<'a> {
    /// Chips and streams for `song`, with `limit` output samples of headroom.
    ///
    /// A chip whose clock the header leaves at 0 is not there, and its stream
    /// then never clocks it: `Stream::read` answers silence instead.
    fn new(song: &'a Vgm, limit: usize) -> Self {
        let header = song.header();
        Self {
            ym: Ym2612::new(),
            psg: Sn76489::new(),
            ym_stream: Stream::new(u64::from(header.ym2612_clock), YM_DIVISOR),
            psg_stream: Stream::new(u64::from(header.sn76489_clock), PSG_DIVISOR),
            bank: &[],
            bank_pos: 0,
            // The parse counted the file's waits, so the buffer can start at that
            // size instead of growing into it. `1 << 20` frames is the most it
            // will reserve on the strength of that count: a corrupt file whose
            // own waits add up to two hours is not worth an allocation for.
            out: Vec::with_capacity(limit.min(song.samples).min(1 << 20).saturating_mul(2)),
            at: 0,
            limit,
        }
    }

    /// Apply one decoded command. Everything that moves time goes through
    /// [`Player::wait`], which is where the chips are clocked.
    fn command(&mut self, command: Command<'a>) {
        match command {
            Command::PsgWrite(value) => self.psg.write(value),
            Command::FmWrite {
                part,
                register,
                value,
            } => {
                // VGM's part is the chip's A1 line and `0x52`/`0x53` say which
                // of the two data strobes it is on, so part 0 is ports 0 and 1
                // and part 1 is ports 2 and 3 (`ym2612::Ym2612::write`).
                self.ym.write(part * 2, register);
                self.ym.write(part * 2 + 1, value);
            }
            Command::Wait(samples) => self.wait(samples),
            Command::DataBlock { kind, data } => {
                if kind == BLOCK_YM2612_PCM {
                    self.bank = data;
                    self.bank_pos = 0;
                }
            }
            Command::PcmWrite { wait } => {
                let value = self.bank.get(self.bank_pos).copied().unwrap_or(DAC_CENTRE);
                self.bank_pos += 1;
                self.ym.write(0, REG_DAC_DATA);
                self.ym.write(1, value);
                self.wait(wait);
            }
            Command::PcmSeek(offset) => {
                self.bank_pos = usize::try_from(offset).unwrap_or(usize::MAX)
            }
            Command::End => {}
        }
    }

    /// Run both chips for `samples` more output samples.
    fn wait(&mut self, samples: u32) {
        for _ in 0..samples {
            if self.out.len() / 2 >= self.limit {
                return;
            }
            self.mix_one();
            self.at += 1;
        }
    }

    /// One output sample: each chip where the 44100 Hz grid wants it, the two of
    /// them added at the console's levels, and both sides of the pair.
    fn mix_one(&mut self) {
        let Self {
            ym,
            psg,
            ym_stream,
            psg_stream,
            out,
            at,
            ..
        } = self;

        let (fm_left, fm_right) = ym_stream.read(*at, &mut || ym.clock());
        let psg_sample = psg_stream.read(*at, &mut || psg.clock());
        out.push(shape(fm_left, psg_sample));
        out.push(shape(fm_right, psg_sample));
    }
}

/// One FM sample and one PSG sample into one output sample, at the levels the
/// console mixes them at and clamped rather than wrapped: six FM channels at
/// full tilt is more than an `i16` can say, exactly as in `genesis`' `Mixer`.
fn shape(fm: i16, psg: i16) -> i16 {
    let fm = i32::from(fm) * YM_GAIN;
    let psg = i32::from(psg) * PSG_GAIN_NUM / PSG_GAIN_DEN;
    (fm + psg).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// The chip samples [`Stream`] can resample: the PSG's one `i16` and the FM's
/// `(left, right)` pair.
trait Sample: Copy {
    /// The sample of a chip that is not there.
    const SILENCE: Self;

    /// `self + (other - self) * part / whole`, per channel, in integer
    /// arithmetic so the resampler is bit-for-bit the same on every machine -
    /// which matters, because two of this crate's tests hash what it produces.
    fn lerp(self, other: Self, part: u32, whole: u32) -> Self;
}

impl Sample for i16 {
    const SILENCE: Self = 0;

    fn lerp(self, other: Self, part: u32, whole: u32) -> Self {
        // `i64`, because the gap between two `i16`s times a `part` of 6350400 is
        // not an `i32`. `part < whole`, so the answer still lies between the two
        // inputs and cannot leave the `i16` range however the divide rounds.
        let step = (i64::from(other) - i64::from(self)) * i64::from(part);
        (i64::from(self) + step / i64::from(whole)) as i16
    }
}

impl Sample for (i16, i16) {
    const SILENCE: Self = (0, 0);

    fn lerp(self, other: Self, part: u32, whole: u32) -> Self {
        (
            self.0.lerp(other.0, part, whole),
            self.1.lerp(other.1, part, whole),
        )
    }
}

/// One chip's native-rate sample stream, read at [`OUTPUT_RATE`] by linear
/// interpolation.
///
/// The chip's rate is kept as the exact fraction the header gives - the YM at
/// `clock / 144` Hz, the PSG at `clock / 16` Hz, over 44100 - and output sample
/// `n` is read from native position `n * num / den`, so nothing about where a
/// chip's samples land depends on floating point or on the order the waits in a
/// file happened to come in.
///
/// The chip is clocked lazily, from inside [`Stream::read`], and never more than
/// one native sample ahead of where the read is: one sample of a YM is 18.8 us
/// and one of a PSG 4.5 us, which is finer than this resampler can resolve and
/// close enough for a write that lands on a sample boundary of the file.
struct Stream<S> {
    /// The chip's clock in Hz: what `at` is multiplied by to give a position in
    /// native samples, before [`Stream::den`] puts it over the output rate.
    num: u64,
    /// The chip's divider - 144 for the FM, 16 for the PSG - times
    /// [`OUTPUT_RATE`], so that `at * num / den` is the native sample the read
    /// sits on.
    den: u64,
    /// The native samples the chip has produced that the read has not walked
    /// past: `window[0]` is the sample at index `base`.
    window: VecDeque<S>,
    /// The native index of `window[0]`.
    base: u64,
}

impl<S: Sample> Stream<S> {
    /// A stream for the chip that runs at `clock / divisor` Hz.
    fn new(clock: u64, divisor: u64) -> Self {
        Self {
            num: clock,
            den: divisor * u64::from(OUTPUT_RATE),
            window: VecDeque::new(),
            base: 0,
        }
    }

    /// The chip's value at output sample `at`, clocking it with `clock` as far
    /// as the interpolation needs.
    fn read(&mut self, at: u64, clock: &mut impl FnMut() -> S) -> S {
        if self.num == 0 {
            return S::SILENCE;
        }

        let scaled = at as u128 * u128::from(self.num);
        let index = (scaled / u128::from(self.den)) as u64;
        let part = (scaled % u128::from(self.den)) as u32;

        // Fill before dropping, so that the two taps are always two samples the
        // chip really produced rather than whatever was left in the window when
        // the read ran ahead of it.
        while self.base + self.window.len() as u64 <= index + 1 {
            let sample = clock();
            self.window.push_back(sample);
        }
        while self.base < index {
            self.window.pop_front();
            self.base += 1;
        }
        // Both taps are there because the loop above stopped with the window
        // holding the samples at `index` and `index + 1`, and the loop below
        // only takes away the ones below `index`.
        let (before, after) = (self.window[0], self.window[1]);
        before.lerp(after, part, self.den as u32)
    }
}
