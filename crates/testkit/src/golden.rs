//! Golden-value helpers: hash a frame, compare it with a checked-in golden
//! file, and dump a PNG so a human can look at it. See `docs/TESTING.md`
//! ("Golden files").
//!
//! Frame buffers arrive as raw `&[u32]` plus a size, never as a `vdp` type:
//! testkit must not depend on any chip crate, so that a core can depend on
//! testkit's helpers without dragging the console in. Pixels are RGBA8888 in
//! `0xAABBGGRR` order, i.e. the little-endian byte order R, G, B, A.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Environment variable that switches `assert_golden` from comparing to
/// recording. Only a ticket may say to use it.
const BLESS_ENV: &str = "RUSTSEGA_BLESS";

/// FNV-1a 64 over `bytes`, the hash every golden value in the repo is built
/// from - exposed for callers that hash something other than a frame, e.g.
/// `headless --audio-hash` over a run's PCM bytes (M6-02, #121).
///
/// Fowler-Noll-Vo "The FNV non-cryptographic hash algorithm", 32-bit offset
/// basis and prime widened to 64: per byte, `hash ^= byte` then
/// `hash = hash.wrapping_mul(FNV_PRIME)`.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    fnv1a_64_from(FNV_OFFSET, bytes.iter().copied())
}

/// The loop behind [`fnv1a_64`], starting from `hash` so [`frame_hash`] can run
/// several byte streams through one hash without buffering them together.
fn fnv1a_64_from(mut hash: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Stable 64-bit hash of a frame, for comparing against a golden file.
///
/// [`fnv1a_64`] over the width and height (each `u32` little endian) and then
/// the pixel bytes in order. The size is in the hash so two frames with the same
/// pixels in a different geometry, e.g. 4x2 and 8x1, cannot collide.
///
/// Pixels are `0xAABBGGRR`, so `to_le_bytes` yields R, G, B, A.
pub fn frame_hash(pixels: &[u32], width: usize, height: usize) -> u64 {
    debug_assert_eq!(
        pixels.len(),
        width * height,
        "frame_hash: {width}x{height} needs {} pixels, got {}",
        width * height,
        pixels.len()
    );
    // Width and height go in as u32, not usize, so the hash is the same on
    // every target.
    let header = (width as u32).to_le_bytes();
    let footer = (height as u32).to_le_bytes();
    fnv1a_64_from(
        FNV_OFFSET,
        header
            .into_iter()
            .chain(footer)
            .chain(pixels.iter().flat_map(|pixel| pixel.to_le_bytes())),
    )
}

/// Parse a golden value: `0x`-prefixed hex, or decimal if there is no prefix,
/// so a hand-edited file stays readable.
fn parse_value(raw: &str, key: &str, path: &Path) -> u64 {
    let trimmed = raw.trim();
    let parsed = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => trimmed.parse::<u64>(),
    };
    parsed.unwrap_or_else(|e| {
        panic!(
            "{}: golden key \"{key}\" has value \"{raw}\" which is not a number: {e}",
            path.display()
        )
    })
}

/// One golden file: a flat TOML table of `key = "0x0123456789abcdef"`.
///
/// Keys stay sorted so a diff shows only the value that moved, never a
/// reordering.
#[derive(Debug, Clone, Default)]
pub struct Golden {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Golden {
    /// Read `path`, treating a missing file as an empty set of goldens so the
    /// first bless run can create it.
    pub fn load(path: &Path) -> Golden {
        let values = match fs::read_to_string(path) {
            Ok(text) => toml::from_str::<BTreeMap<String, String>>(&text)
                .unwrap_or_else(|e| panic!("{}: not a valid golden file: {e}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => panic!("{}: cannot read golden file: {err}", path.display()),
        };
        Golden {
            path: path.to_path_buf(),
            values,
        }
    }

    /// Recorded value for `key`, or `None` if the file or the key is missing.
    pub fn get(&self, key: &str) -> Option<u64> {
        self.values
            .get(key)
            .map(|raw| parse_value(raw, key, &self.path))
    }

    /// Record `value` under `key`, overwriting whatever was there. Call
    /// [`Golden::save`] to write it out.
    pub fn set(&mut self, key: &str, value: u64) {
        self.values
            .insert(key.to_string(), format!("0x{value:016x}"));
    }

    /// Write the whole table back to the file this was loaded from, creating
    /// parent directories as needed.
    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| {
                panic!("{}: cannot create parent dir: {e}", self.path.display())
            });
        }
        let text = toml::to_string_pretty(&self.values)
            .unwrap_or_else(|e| panic!("{}: cannot format golden file: {e}", self.path.display()));
        fs::write(&self.path, text)
            .unwrap_or_else(|e| panic!("{}: cannot write golden file: {e}", self.path.display()));
    }

    /// Where this was loaded from, and where [`Golden::save`] writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fail the test unless `path` records `actual` under `key`.
///
/// With `RUSTSEGA_BLESS=1` in the environment, records the value instead of
/// comparing, which is how a golden gets created or updated. Only bless when a
/// ticket says the golden is wrong, and look at a [`dump_png`] dump first.
pub fn assert_golden(path: &Path, key: &str, actual: u64) {
    let bless = std::env::var_os(BLESS_ENV).is_some_and(|value| value == "1");
    assert_golden_inner(path, key, actual, bless);
}

/// [`assert_golden`] with the bless decision passed in rather than read from
/// the environment, so tests can cover blessing without touching process
/// globals.
fn assert_golden_inner(path: &Path, key: &str, actual: u64, bless: bool) {
    let mut golden = Golden::load(path);
    match golden.get(key) {
        Some(expected) if expected == actual => {}
        Some(_) if bless => {
            golden.set(key, actual);
            golden.save();
        }
        Some(expected) => panic!(
            "golden mismatch for key \"{key}\" in {}: expected 0x{expected:016x}, got \
             0x{actual:016x}. Dump the PNG and look at it first, then run with \
             RUSTSEGA_BLESS=1 only if the ticket says the golden is wrong.",
            path.display()
        ),
        None if bless => {
            golden.set(key, actual);
            golden.save();
        }
        None => panic!(
            "missing golden key \"{key}\" in {}. Dump the PNG, check it by eye, then \
             run with RUSTSEGA_BLESS=1 to record this value.",
            path.display()
        ),
    }
}

/// Write a frame out as an 8-bit RGBA PNG for a human to inspect.
///
/// `0xAABBGGRR` in memory is already R, G, B, A on disk, so the bytes go
/// straight to the encoder.
pub fn dump_png(path: &Path, pixels: &[u32], width: usize, height: usize) {
    debug_assert_eq!(
        pixels.len(),
        width * height,
        "dump_png: {width}x{height} needs {} pixels, got {}",
        width * height,
        pixels.len()
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("{}: cannot create parent dir: {e}", path.display()));
    }

    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        rgba.extend_from_slice(&pixel.to_le_bytes());
    }

    let file = fs::File::create(path)
        .unwrap_or_else(|e| panic!("{}: cannot create png: {e}", path.display()));
    let mut encoder = png::Encoder::new(file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .unwrap_or_else(|e| panic!("{}: cannot write png header: {e}", path.display()));
    writer
        .write_image_data(&rgba)
        .unwrap_or_else(|e| panic!("{}: cannot write png pixels: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp::TempDir;
    use std::fs;

    /// Opaque red, green, blue, then semi-transparent white, then black,
    /// white, and two mixed values, so a hash that ignores any byte order or
    /// any pixel shows up as a change.
    fn sample_pixels() -> Vec<u32> {
        vec![
            0xFF00_00FF, // opaque red
            0xFF00_FF00, // opaque green
            0xFFFF_0000, // opaque blue
            0x80FF_FFFF, // half-transparent white
            0x0000_0000, // black
            0xFFFF_FFFF, // white
            0x1234_5678,
            0x9ABC_DEF0,
        ]
    }

    /// The published FNV-1a 64 test vectors, so the hash the audio hash reuses
    /// is the real FNV-1a and not something that merely looks like it.
    #[test]
    fn fnv1a_64_matches_the_reference_vectors() {
        // Computed independently of this file, straight from the FNV-1a 64
        // definition: 0xcbf29ce484222325 offset, 0x100000001b3 prime.
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn frame_hash_is_stable() {
        // FNV-1a 64 over the 8-byte width/height header then the 32 pixel
        // bytes, computed independently in Python straight from the constants
        // in this file: 0xcbf29ce484222325 offset, 0x100000001b3 prime, per
        // byte h ^= b; h *= prime. If this constant has to change, the hash
        // function changed, which invalidates every golden file in the repo.
        let expected = 0x960b_ccb3_19ba_88be;
        assert_eq!(frame_hash(&sample_pixels(), 4, 2), expected);
    }

    #[test]
    fn frame_hash_depends_on_size() {
        // Same pixels, same byte length, different geometry: the header is
        // what keeps a 4x2 frame from hashing equal to an 8x1 one.
        let pixels = sample_pixels();
        let wide = frame_hash(&pixels, 8, 1);
        let tall = frame_hash(&pixels, 4, 2);
        assert_ne!(wide, tall, "width and height must feed the hash");
    }

    #[test]
    fn frame_hash_depends_on_every_pixel() {
        let mut pixels = sample_pixels();
        let before = frame_hash(&pixels, 4, 2);
        pixels[7] ^= 1; // last pixel, lowest byte
        assert_ne!(frame_hash(&pixels, 4, 2), before);
    }

    #[test]
    fn golden_roundtrip() {
        let dir = TempDir::new("golden-roundtrip");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("ntsc_frame_0", 0x0123_4567_89ab_cdef);
        golden.set("ntsc_frame_1", 0xfedc_ba98_7654_3210);
        golden.save();

        let reloaded = Golden::load(&path);
        assert_eq!(reloaded.get("ntsc_frame_0"), Some(0x0123_4567_89ab_cdef));
        assert_eq!(reloaded.get("ntsc_frame_1"), Some(0xfedc_ba98_7654_3210));
    }

    #[test]
    fn golden_writes_sorted_keys_and_hex_values() {
        let dir = TempDir::new("golden-sorted");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("z_frame", 0xa);
        golden.set("a_frame", 0xff);
        golden.save();

        let text = fs::read_to_string(&path).expect("read golden file");
        assert!(
            text.contains("a_frame = \"0x00000000000000ff\""),
            "values must be 0x-padded hex, got: {text}"
        );
        let a = text.find("a_frame").expect("a_frame present");
        let z = text.find("z_frame").expect("z_frame present");
        assert!(a < z, "keys must be sorted, got: {text}");
    }

    #[test]
    fn golden_load_missing_file_is_empty() {
        let dir = TempDir::new("golden-missing-file");
        let golden = Golden::load(&dir.join("nope.toml"));
        assert_eq!(golden.get("anything"), None);
    }

    #[test]
    fn golden_set_replaces_existing_key() {
        let dir = TempDir::new("golden-replace");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("frame", 1);
        golden.set("frame", 2);
        golden.save();
        assert_eq!(Golden::load(&path).get("frame"), Some(2));
    }

    #[test]
    fn assert_golden_passes_on_match() {
        let dir = TempDir::new("golden-pass");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("frame_0", 42);
        golden.save();

        assert_golden_inner(&path, "frame_0", 42, false);
    }

    #[test]
    #[should_panic(expected = "golden mismatch")]
    fn assert_golden_panics_on_mismatch() {
        let dir = TempDir::new("golden-mismatch");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("frame_0", 42);
        golden.save();

        assert_golden_inner(&path, "frame_0", 43, false);
    }

    #[test]
    #[should_panic(expected = "missing golden key \"frame_0\"")]
    fn assert_golden_panics_on_missing_key() {
        let dir = TempDir::new("golden-no-key");
        let path = dir.join("frame.toml");
        Golden::load(&path).save();

        assert_golden_inner(&path, "frame_0", 42, false);
    }

    #[test]
    fn bless_mode_writes_value() {
        let dir = TempDir::new("golden-bless");
        let path = dir.join("frame.toml");
        let mut golden = Golden::load(&path);
        golden.set("frame_0", 42);
        golden.save();

        // Blessing a mismatch updates the file instead of panicking.
        assert_golden_inner(&path, "frame_0", 43, true);
        assert_eq!(Golden::load(&path).get("frame_0"), Some(43));

        // A brand new key gets recorded too, and the file is created if the
        // whole golden file was missing.
        let fresh = dir.join("fresh.toml");
        assert!(!fresh.exists());
        assert_golden_inner(&fresh, "frame_0", 7, true);
        assert_eq!(Golden::load(&fresh).get("frame_0"), Some(7));
    }

    #[test]
    fn png_dump_writes_file() {
        let dir = TempDir::new("png");
        let path = dir.join("frame.png");
        // Opaque red, green, blue, white.
        let pixels = [0xFF00_00FFu32, 0xFF00_FF00, 0xFFFF_0000, 0xFFFF_FFFF];
        dump_png(&path, &pixels, 2, 2);

        let file = fs::File::open(&path).expect("open dumped png");
        let decoder = png::Decoder::new(std::io::BufReader::new(file));
        let mut reader = decoder.read_info().expect("read png header");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("png size")];
        let info = reader.next_frame(&mut buf).expect("decode png pixels");
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);

        assert_eq!(&buf[..4], &[0xFF, 0x00, 0x00, 0xFF], "first pixel");
        assert_eq!(&buf[4..8], &[0x00, 0xFF, 0x00, 0xFF], "second pixel");
        assert_eq!(&buf[12..16], &[0xFF, 0xFF, 0xFF, 0xFF], "last pixel");
    }
}
