//! The `--dump-wav` writer (M6-02, #121).
//!
//! Hand-rolled rather than a crate: the file is one RIFF chunk holding a `fmt `
//! subchunk and a `data` subchunk, and the only shape this tool ever writes is
//! 16-bit signed PCM stereo, which is 44 bytes of header and a copy of what
//! [`genesis::Genesis::drain_audio`] handed back.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// `AudioFormat` 1 in the `fmt ` subchunk: PCM integer, the only thing we write.
const FORMAT_PCM: u16 = 1;
/// The Genesis mixes to stereo, so the sample stream is always L, R, L, R, ...
const CHANNELS: u16 = 2;
/// Sample width in bits; `drain_audio` yields `i16`.
const BITS_PER_SAMPLE: u16 = 16;
/// Bytes of one sample: `CHANNELS * BITS_PER_SAMPLE / 8`, i.e. one L,R pair.
const BLOCK_ALIGN: u16 = CHANNELS * BITS_PER_SAMPLE / 8;
/// Size of the `fmt ` subchunk's body for PCM: the five fields that follow it
/// (16 bytes total, no extension).
const FMT_SIZE: u32 = 16;
/// What the RIFF size field counts on top of the data: `"WAVE"` (4), the `fmt `
/// tag and size (8), the `fmt ` body (16) and the `data` tag and size (8).
const RIFF_OVERHEAD: u32 = 4 + 8 + FMT_SIZE + 8;
/// The most `data` a WAV can size: the RIFF size field is 32 bits, so just over
/// six hours of 48 kHz stereo (22369 s) - far past any run this CLI can be
/// given, but a wrapped size would be a file no player can read, so refuse it.
const MAX_DATA_SIZE: u32 = u32::MAX - RIFF_OVERHEAD;

/// Write `samples` - interleaved stereo `i16` at `rate`, exactly what
/// [`genesis::Genesis::drain_audio`] produces - to `path` as a PCM WAV file.
///
/// The layout is Microsoft's "New Wave Form": `RIFF`/`WAVE` wrapping a 16-byte
/// PCM `fmt ` subchunk and a `data` subchunk of little-endian `i16`s. The sizes
/// are 32-bit, so a dump too long for them ([`MAX_DATA_SIZE`]) is refused rather
/// than written with a size that wrapped.
///
/// Errors from the filesystem come back as errors, not panics: `--dump-wav`
/// pointed at a directory is a mistake, and the CLI prints `error: ...`.
pub fn write_wav(path: &Path, rate: u32, samples: &[i16]) -> io::Result<()> {
    let data_bytes = samples.len() as u64 * 2;
    if data_bytes > u64::from(MAX_DATA_SIZE) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} samples is more audio than a WAV can size",
                samples.len()
            ),
        ));
    }
    let data_size = data_bytes as u32;

    let mut file = BufWriter::new(File::create(path)?);
    // The RIFF size is the length of everything after these eight bytes.
    write_chunk(
        &mut file,
        b"RIFF",
        &(RIFF_OVERHEAD + data_size).to_le_bytes(),
    )?;
    file.write_all(b"WAVE")?;
    write_chunk(&mut file, b"fmt ", &FMT_SIZE.to_le_bytes())?;
    file.write_all(&FORMAT_PCM.to_le_bytes())?;
    file.write_all(&CHANNELS.to_le_bytes())?;
    file.write_all(&rate.to_le_bytes())?;
    // Byte rate = SampleRate * BlockAlign (MMFA): how fast a player burns data.
    file.write_all(&(rate * u32::from(BLOCK_ALIGN)).to_le_bytes())?;
    file.write_all(&BLOCK_ALIGN.to_le_bytes())?;
    file.write_all(&BITS_PER_SAMPLE.to_le_bytes())?;
    write_chunk(&mut file, b"data", &data_size.to_le_bytes())?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.flush()
}

/// A four-character tag followed by its little-endian size, the shape every
/// RIFF chunk and subchunk starts with.
fn write_chunk(file: &mut impl Write, tag: &[u8; 4], size: &[u8; 4]) -> io::Result<()> {
    file.write_all(tag)?;
    file.write_all(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A scratch file that cleans itself up. `testkit::tmp::TempDir` is
    /// test-only inside testkit, so this crate keeps its own line of it
    /// (`crates/headless/tests/cli.rs` has the directory version).
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "rustsega-wav-{tag}-{}-{nanos}.wav",
                std::process::id()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().expect("2 bytes"))
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    /// Write `samples` at `rate` and read the whole file back.
    fn dump(rate: u32, samples: &[i16]) -> Vec<u8> {
        let file = TempFile::new("dump");
        write_wav(file.path(), rate, samples).expect("write the wav");
        fs::read(file.path()).expect("read the wav back")
    }

    /// The 44-byte header of a PCM WAV, checked field by field against the
    /// format, plus the data length and a round trip of the samples themselves.
    #[test]
    fn wav_header_is_valid() {
        // 100 samples = 50 stereo frames, a ramp across the sign so a writer
        // that clamped, dropped the sign or used big-endian would show up.
        let samples: Vec<i16> = (0..100_i32).map(|i| (i * 650 - 32_000) as i16).collect();

        let bytes = dump(48_000, &samples);
        assert_eq!(bytes.len(), 44 + 200, "44-byte header plus 100 samples");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32_at(&bytes, 4),
            36 + 200,
            "RIFF size is the rest of the file"
        );
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32_at(&bytes, 16), 16, "the fmt body is 16 bytes of PCM");
        assert_eq!(u16_at(&bytes, 20), 1, "audio format 1 is PCM");
        assert_eq!(u16_at(&bytes, 22), 2, "stereo");
        assert_eq!(u32_at(&bytes, 24), 48_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 48_000 * 4, "byte rate = rate * 4");
        assert_eq!(u16_at(&bytes, 32), 4, "block align = one stereo sample");
        assert_eq!(u16_at(&bytes, 34), 16, "bits per sample");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(
            u32_at(&bytes, 40),
            200,
            "data chunk is the samples as i16 LE"
        );

        for (i, sample) in samples.iter().enumerate() {
            let got = i16::from_le_bytes([bytes[44 + 2 * i], bytes[44 + 2 * i + 1]]);
            assert_eq!(got, *sample, "sample {i} must round trip");
        }
    }

    /// A run with no audio at all still has to be a readable file, because
    /// `--frames 0 --dump-wav` is a thing a script does.
    #[test]
    fn wav_of_no_samples_is_a_bare_header() {
        let bytes = dump(48_000, &[]);
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4), 36);
        assert_eq!(u32_at(&bytes, 40), 0);
    }

    /// A rate other than 48 kHz is written, not assumed: `Config::sample_rate`
    /// is a host choice and the header has to report the one that was used.
    #[test]
    fn wav_records_the_rate_it_was_given() {
        let bytes = dump(22_050, &[0, 0]);
        assert_eq!(u32_at(&bytes, 24), 22_050);
        assert_eq!(u32_at(&bytes, 28), 22_050 * 4);
    }
}
