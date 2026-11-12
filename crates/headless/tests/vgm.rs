//! The `vgm` subcommand (M5-12, #117): a VGM built in memory, played through
//! [`ym2612::Ym2612`] and [`sn76489::Sn76489`], and what its PCM has to look
//! like for the goldens to mean something.
//!
//! The two `*_vgm_hash` tests pin FNV-1a 64 over the PCM bytes - the same shape
//! `run --audio-hash` hashes (M6-02, #121) - and they are only worth pinning if
//! the audio under the hash is the tone the registers asked for. So each one
//! also runs [`assert_tone`], which checks the output is loud, is not DC, and
//! has most of its energy where the chip's own F-Num / tone period says the
//! fundamental is. A golden pinned on silent or wrong-pitch audio locks in a
//! bug; those three assertions are what stop that happening here.
//!
//! The command encodings are the VGM 1.71 spec's: 44100 Hz "samples" as the unit
//! of time, `0x7n` a one-byte wait of n+1, `0x8n` a one-byte data-bank write to
//! `&2A` plus a wait of n, and `0x67 0x66 tt ssssssss` a data block of 7 + s
//! bytes with no checksum.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use headless::vgm::{OUTPUT_RATE, Vgm};

/// `0x0C` of a Genesis VGM: the PSG at the Z80's 3.58 MHz, i.e. master / 15.
const PSG_CLOCK: u32 = 3_579_545;
/// `0x2C` of a Genesis VGM: the 68k clock, so the FM's sample lands every 144
/// of them (docs/ARCHITECTURE.md, "Clocks and timing").
const YM_CLOCK: u32 = 7_670_470;

/// The version of the fixtures: 1.60, so the data offset at `0x34` is read.
const VERSION: u32 = 0x0160;

/// The PSG tone period of the fixture. The SN76489 flips its output every
/// `period` ticks of clock/16, so this is 3579545 / (32 * 254) = 440.3 Hz.
const PSG_PERIOD: u16 = 254;

/// The F-Num and block of the FM fixture: with multiple 1 and no detune the
/// phase increment is (F-Num << Block) >> 1 = 8656, which at 7670470 / 144 Hz is
/// 439.8 Hz - the same A4 the PSG fixture plays, so the two tests can share the
/// listening window.
const YM_FNUM: u16 = 1082;
const YM_BLOCK: u8 = 4;

/// How long the tone fixtures hold: 4410 samples, a tenth of a second.
const TONE_SAMPLES: u16 = 4410;

/// A VGM image: the 64-byte header with the fields the player reads, then
/// `commands`. The data offset is left at 0, which the spec spells as "the data
/// starts at 0x40".
fn vgm(commands: &[u8]) -> Vec<u8> {
    let mut file = vec![0_u8; 0x40];
    file[0..4].copy_from_slice(b"Vgm ");
    // EoF offset is relative to 0x04, i.e. file length - 4.
    let eof = (file.len() + commands.len() - 4) as u32;
    file[0x04..0x08].copy_from_slice(&eof.to_le_bytes());
    file[0x08..0x0C].copy_from_slice(&VERSION.to_le_bytes());
    file[0x0C..0x10].copy_from_slice(&PSG_CLOCK.to_le_bytes());
    file[0x2C..0x30].copy_from_slice(&YM_CLOCK.to_le_bytes());
    file.extend_from_slice(commands);
    file
}

/// FNV-1a 64 over the PCM as little-endian bytes: `testkit::fnv1a_64` over the
/// bytes a WAV's `data` chunk would hold, which is what `--audio-hash` means by
/// the hash of a run (M6-02, #121).
fn pcm_hash(samples: &[i16]) -> u64 {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    testkit::fnv1a_64(&bytes)
}

/// The frequency of the PSG fixture, from the SN76489's own divider: the chip
/// divides its clock by 16 and flips the output every `PSG_PERIOD` of those
/// ticks, so a full cycle is 32 * period clock ticks (SMS Power, "SN76489",
/// tone generator).
fn psg_expected_hz() -> f64 {
    f64::from(PSG_CLOCK) / (32.0 * f64::from(PSG_PERIOD))
}

/// The frequency of the FM fixture, from the YM2612's own phase counter: the
/// increment `(F-Num << Block) >> 1` at multiple 1 is that many twenty-bit
/// phases per FM sample, and the FM sample is `YM_CLOCK / 144` of a second
/// (YM2608 application manual `&30`, `&A0`, `&A4`).
fn ym_expected_hz() -> f64 {
    let increment = (u32::from(YM_FNUM) << YM_BLOCK) >> 1;
    f64::from(increment) * (f64::from(YM_CLOCK) / 144.0) / (1 << 20) as f64
}

/// `assert_tone` reads a channel out of the interleaved stereo `pcm`.
fn left(pcm: &[i16]) -> Vec<f64> {
    pcm.iter()
        .step_by(2)
        .map(|sample| f64::from(*sample))
        .collect()
}

/// The magnitude of `samples` at `freq`, by the definition of a DFT bin:
/// |sum x[n] (cos + i sin)(2 PI f n / rate)|. Slow and honest, and on a tenth of
/// a second of audio it is cheap enough to run in a test.
fn magnitude(samples: &[f64], freq: f64) -> f64 {
    let step = 2.0 * std::f64::consts::PI * freq / OUTPUT_RATE as f64;
    let mut re = 0.0;
    let mut im = 0.0;
    for (n, sample) in samples.iter().enumerate() {
        let phase = step * n as f64;
        re += sample * phase.cos();
        im += sample * phase.sin();
    }
    re.hypot(im)
}

/// The whole-number frequency between `low` and `high` with the most energy in
/// `samples`, scanned one hertz at a time.
fn loudest_freq(samples: &[f64], low: u32, high: u32) -> f64 {
    let mut best = 0.0;
    let mut best_freq = 0.0;
    for freq in low..=high {
        let energy = magnitude(samples, f64::from(freq));
        if energy > best {
            best = energy;
            best_freq = f64::from(freq);
        }
    }
    best_freq
}

/// What `headless vgm --out` has to have produced: a tenth of a second of a
/// tone at `expected_hz`, on both sides, that is far from silence.
///
/// This is the check that was run before the two hashes below were pinned, and
/// it runs every time the suite runs: peak and RMS say the chip is making a
/// sound, and the DFT says the sound is the pitch the registers asked for. The
/// window is a tenth of a second, so a scan can only resolve to about the 10 Hz
/// width of a DFT lobe; 6 Hz is what a correctly-pitched tone measures.
fn assert_tone(pcm: &[i16], expected_hz: f64) {
    let left = left(pcm);
    let peak = left
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
    let mean = left.iter().sum::<f64>() / left.len() as f64;
    let rms = (left.iter().map(|sample| sample * sample).sum::<f64>() / left.len() as f64).sqrt();
    let dominant = loudest_freq(&left, 200, 800);
    let at_expected = magnitude(&left, expected_hz);
    let octave_up = magnitude(&left, expected_hz * 2.0);

    eprintln!(
        "tone: peak={peak:.0} mean={mean:.1} rms={rms:.0} dominant={dominant:.1} Hz, \
         |X({expected_hz:.1})|={at_expected:.0} |X(2f)|={octave_up:.0}"
    );
    assert!(
        peak > 4_000.0,
        "peak {peak:.0} of a full-scale tone is too quiet"
    );
    assert!(rms > 1_000.0, "rms {rms:.0} of a tone is too quiet");
    assert!(
        mean.abs() < peak / 4.0,
        "mean {mean:.1} against a peak of {peak:.0} says this is DC, not a tone"
    );
    assert!(
        (dominant - expected_hz).abs() <= 6.0,
        "{dominant:.1} Hz is the loudest frequency, not {expected_hz:.1} Hz"
    );
    assert!(
        at_expected > octave_up,
        "the octave above the tone is louder than the tone itself: \
         |X(f)|={at_expected:.0} |X(2f)|={octave_up:.0}"
    );
}

#[test]
fn parse_header_minimal() {
    let file = vgm(&[0x66]);
    let song = Vgm::parse(&file).expect("a header and an end command is a VGM");
    assert_eq!(song.header().version, VERSION);
    assert_eq!(song.header().sn76489_clock, PSG_CLOCK);
    assert_eq!(song.header().ym2612_clock, YM_CLOCK);
    assert_eq!(
        song.samples(),
        0,
        "nothing is waited for, so nothing is played"
    );
    assert!(song.play(None).is_empty(), "and no samples come out either");
}

#[test]
fn wait_commands_count_samples() {
    // 0x62 is 735, 0x63 is 882 and 0x70 is 0 + 1 (VGM 1.71: `0x7n` waits n + 1
    // samples and takes no operand).
    let file = vgm(&[0x62, 0x63, 0x70, 0x66]);
    let song = Vgm::parse(&file).expect("three waits and an end");
    assert_eq!(song.samples(), 735 + 882 + 1);
    assert_eq!(
        song.play(None).len() / 2,
        735 + 882 + 1,
        "and playing makes that many stereo frames"
    );
}

#[test]
fn psg_tone_vgm_hash() {
    let file = psg_tone_vgm();
    let song = Vgm::parse(&file).expect("the psg tone fixture parses");
    let pcm = song.play(None);
    assert_eq!(
        pcm.len(),
        TONE_SAMPLES as usize * 2,
        "one stereo pair per sample"
    );
    assert_tone(&pcm, psg_expected_hz());
    assert_eq!(pcm_hash(&pcm), 0x414b_02b9_7d3a_f4f1, "psg_tone_vgm_hash");
}

#[test]
fn ym_tone_vgm_hash() {
    let file = ym_tone_vgm();
    let song = Vgm::parse(&file).expect("the ym tone fixture parses");
    let pcm = song.play(None);
    assert_eq!(
        pcm.len(),
        TONE_SAMPLES as usize * 2,
        "one stereo pair per sample"
    );
    assert_tone(&pcm, ym_expected_hz());
    assert_eq!(pcm_hash(&pcm), 0x31a3_f5fc_f254_d0ed, "ym_tone_vgm_hash");
}

/// A `.vgz` is the same file gzipped, and the player is handed gzip'd bytes far
/// more often than raw ones, so the two have to agree sample for sample.
#[test]
fn gzipped_vgm_plays_identically() {
    let raw = psg_tone_vgm();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).expect("gzip the vgm");
    let compressed = encoder.finish().expect("finish the gzip");
    assert_eq!(
        &compressed[0..2],
        &[0x1f, 0x8b],
        "and the fixture really is gzip"
    );

    let raw_pcm = Vgm::parse(&raw).expect("raw parses").play(None);
    let gz_pcm = Vgm::load(&compressed).expect("gz parses").play(None);
    assert_eq!(gz_pcm, raw_pcm, "gzipping a VGM must not change its sound");
}

/// The CLI end of it: `headless vgm <file> --out <wav>` for a `.vgz`, which is
/// the acceptance criterion - the file a person can go and listen to.
#[test]
fn vgm_cli_writes_a_wav() {
    let dir = TempDir::new("vgm-cli");
    let song = dir.join("tone.vgz");
    let wav = dir.join("out.wav");
    fs::write(&song, gzip(&ym_tone_vgm()).expect("gzip the tone fixture")).expect("write the vgz");

    let out = headless()
        .args([
            "vgm",
            song.to_str().expect("utf-8 path"),
            "--out",
            wav.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("run the headless binary");
    assert!(
        out.status.success(),
        "vgm failed: {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = fs::read(&wav).expect("the wav is on disk");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    let rate = u32::from_le_bytes(bytes[24..28].try_into().expect("u32"));
    assert_eq!(
        rate, OUTPUT_RATE,
        "the player's rate is 44100, whatever the chips run at"
    );
    assert_eq!(&bytes[36..40], b"data");
    let data_len = u32::from_le_bytes(bytes[40..44].try_into().expect("u32")) as usize;
    assert_eq!(
        bytes.len(),
        44 + data_len,
        "and the data chunk is the whole rest of the file"
    );
    let pcm: Vec<i16> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert_tone(&pcm, ym_expected_hz());
}

/// A command stream of one PSG channel playing A4 for a tenth of a second:
/// latch channel 0's tone, its two halves, its volume to 0, wait, end.
fn psg_tone_commands() -> Vec<u8> {
    let mut commands = vec![
        0x50,
        0x80 | (PSG_PERIOD & 0x0F) as u8,
        0x50,
        ((PSG_PERIOD >> 4) & 0x3F) as u8,
        0x50,
        0x90,
    ];
    commands.extend([0x61, TONE_SAMPLES as u8, (TONE_SAMPLES >> 8) as u8]);
    commands.push(0x66);
    commands
}

/// The tone fixture for the PSG.
fn psg_tone_vgm() -> Vec<u8> {
    vgm(&psg_tone_commands())
}

/// The tone fixture for the FM: one operator of channel 1 as a carrier, the
/// other three turned all the way down so what comes out is a sine at the
/// channel's F-Num rather than a four-operator timbre. Registers are part I
/// (`0x52`), and the slot order of a channel is S1, S3, S2, S4, so operator 4 -
/// the carrier of algorithm 0 - is the `&0C` offset of each group (YM2608
/// application manual "3. Register descriptions").
fn ym_tone_commands() -> Vec<u8> {
    let mut commands: Vec<u8> = Vec::new();
    let mut write = |register: u8, value: u8| {
        commands.extend([0x52, register, value]);
    };
    // `&2B` bit 7 clear: channel 6 is FM, and nothing has written `&2B` anyway.
    write(0x2B, 0x00);
    // Algorithm 0 - the serial one, whose only carrier is operator 4 - with no
    // feedback.
    write(0xB0, 0x00);
    // Operators 1, 3 and 2 at the loudest and the fastest attack there is, so
    // the modulation they feed on is a constant and the carrier stays a sine.
    for slot in [0x00, 0x04, 0x08] {
        write(0x30 + slot, 0x01); // multiple 1, detune 0
        write(0x40 + slot, 0x7F); // total level 127: silence
        write(0x50 + slot, 0x1F); // attack rate 31
        write(0x60 + slot, 0x00); // decay 1 rate 0
        write(0x80 + slot, 0x00); // sustain level 0, release rate 0
        write(0x90 + slot, 0x00); // no SSG-EG
    }
    // Operator 4: the carrier.
    write(0x3C, 0x01);
    write(0x4C, 0x00); // total level 0
    write(0x5C, 0x1F);
    write(0x6C, 0x00);
    write(0x8C, 0x00);
    write(0x9C, 0x00);
    // And the pitch: block then F-Num, `&A4` latching the high bits for `&A0`.
    write(0xA4, (YM_BLOCK << 3) | (YM_FNUM >> 8) as u8);
    write(0xA0, YM_FNUM as u8);
    // Key on the four operators of channel 1. `&28` is bits 7-4 for the slots and
    // bits 2-0 for the channel field, which counts from 0 for the first channel
    // of part I (`crate::ym2612::phase::key_on_channel`) - keying 1 here would key
    // channel 2 and leave this one silent.
    write(0x28, 0xF0);
    commands.extend([0x61, TONE_SAMPLES as u8, (TONE_SAMPLES >> 8) as u8]);
    commands.push(0x66);
    commands
}

/// The tone fixture for the FM.
fn ym_tone_vgm() -> Vec<u8> {
    vgm(&ym_tone_commands())
}

fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// The headless binary, as `tests/cli.rs` runs it.
fn headless() -> Command {
    Command::new(env!("CARGO_BIN_EXE_headless"))
}

/// A scratch directory that cleans up after itself, as `tests/cli.rs` has one.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rustsega-headless-vgm-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every `.vgz` the owner has under `$RUSTSEGA_TEST_ROM_DIR/vgm/`, ten seconds
/// each, with no panics: the fixtures above prove the two chips and the
/// resampler, and this proves a rip off vgmrips parses, including its data
/// blocks, its loop and its GD3 tail.
///
/// There is no such directory in CI or in a fresh checkout, so this is `#[ignore]`d
/// exactly as the ticket says it should be; `just test-all` runs it.
#[test]
#[ignore = "plays the real .vgz files under $RUSTSEGA_TEST_ROM_DIR/vgm, ten seconds each"]
fn real_vgm_smoke() {
    let dir = testkit::test_rom_dir().join("vgm");
    let Ok(entries) = fs::read_dir(&dir) else {
        eprintln!("no {} to play", dir.display());
        return;
    };
    let mut played = 0;
    let mut files: Vec<Box<Path>> = entries
        .flatten()
        .map(|entry| entry.path().into_boxed_path())
        .filter(|path| {
            path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("vgz") || ext.eq_ignore_ascii_case("vgm")
            })
        })
        .collect();
    files.sort();

    for path in files {
        let mut file = fs::File::open(&path).expect("open the vgm");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read the vgm");
        let limit = 10 * OUTPUT_RATE as usize;
        let song = Vgm::load(&bytes)
            .unwrap_or_else(|err| panic!("{} does not parse: {err}", path.display()));
        let pcm = song.play(Some(limit));
        let peak = pcm
            .iter()
            .fold(0u16, |peak, sample| peak.max(sample.unsigned_abs()));
        eprintln!(
            "{}: {} samples of {} ms, peak {peak}",
            path.display(),
            pcm.len() / 2,
            pcm.len() as u64 * 1000 / (u64::from(OUTPUT_RATE) * 2)
        );
        assert!(
            pcm.len() <= limit * 2,
            "{} went past its limit",
            path.display()
        );
        played += 1;
    }
    eprintln!("played {played} real vgm files");
}
