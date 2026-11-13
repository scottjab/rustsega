//! ROM loading: the half that needs no browser.
//!
//! [`load_rom`] turns the bytes of a picked or dropped file into a
//! [`LoadedRom`]: it size-checks them, reads the cartridge header with the
//! [`genesis`] crate rather than a second parser of its own, and gives the
//! image a stable identity. Everything in here is pure, so it runs natively
//! under `cargo test -p web`; [`crate::rom_picker`] is the browser half that
//! calls it.

use genesis::cart::{LoadError, RomHeader, parse_header};
use sha1_smol::Sha1;

/// The largest file [`load_rom`] will take.
///
/// Not a hardware limit: the console sees 4 MiB of cartridge, and the mapper
/// games are larger files than that (see [`genesis::cart::mapper`]), so this is
/// only a guard against the wrong file - an immediate answer rather than a
/// 700 MiB `Vec` inside a browser tab.
pub const MAX_ROM_BYTES: usize = 10 * 1024 * 1024;

/// What the shell shows when a load fails, whatever the reason was.
///
/// One plain sentence and no internals: the variants of [`RomError`] say what
/// actually went wrong to anyone reading a log, and the user only needs to know
/// that this file was not a ROM.
pub const NOT_A_ROM: &str = "That file doesn't look like a Genesis ROM";

/// A ROM image the shell has accepted, with what the header says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedRom {
    /// The image exactly as it was read, unnormalised.
    pub bytes: Vec<u8>,
    /// Lowercase hex SHA-1 of [`Self::bytes`]: the key later tickets store
    /// SRAM and save states under.
    pub id: String,
    /// The title to display: the header's, or the file name when the header
    /// does not carry one. Never empty.
    pub title: String,
    /// The regions the header claims, spelled out for the top bar, or
    /// `"Unknown"` when it claims none.
    pub region: String,
    /// The name the file arrived with, kept for the library view (M7-16).
    pub file_name: String,
}

/// Why a file could not be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomError {
    /// Too short to hold the header at 0x100, so there is nothing to read.
    TooSmall,
    /// Past [`MAX_ROM_BYTES`], so it is not a ROM and should not be copied
    /// around in memory either.
    TooLarge,
}

impl core::fmt::Display for RomError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooSmall => write!(f, "file is too small to hold a header at 0x100"),
            Self::TooLarge => write!(
                f,
                "file is larger than the {} MiB this will load",
                MAX_ROM_BYTES / (1024 * 1024)
            ),
        }
    }
}

impl std::error::Error for RomError {}

/// Checks a picked or dropped file and turns it into a [`LoadedRom`].
///
/// `file_name` is only ever a display name: what the ROM *is* comes from
/// `bytes`, since a dropped file keeps whatever name its owner gave it.
pub fn load_rom(file_name: &str, bytes: Vec<u8>) -> Result<LoadedRom, RomError> {
    if bytes.len() > MAX_ROM_BYTES {
        return Err(RomError::TooLarge);
    }
    let header = match parse_header(&bytes) {
        Ok(header) => header,
        // Under 0x200 bytes there is no header to read. `LoadError::TooSmall`
        // is the same fact as `RomError::TooSmall`, named in the core crate.
        Err(LoadError::TooSmall) => return Err(RomError::TooSmall),
    };
    // Computed before `bytes` moves into the struct.
    let id = rom_id(&bytes);
    let title = title_of(&header, file_name);
    let region = region_of(&header);
    Ok(LoadedRom {
        bytes,
        id,
        title,
        region,
        file_name: file_name.to_string(),
    })
}

/// The identity of a ROM image: lowercase hex SHA-1 of its bytes.
///
/// SRAM and save states are keyed by this in later tickets, so it has to stay
/// SHA-1 of the image alone, with nothing else mixed in.
fn rom_id(bytes: &[u8]) -> String {
    // `Digest`'s `Display` is the digest as 40 lower-case hex digits, which is
    // what `id_is_the_lowercase_hex_sha1_of_the_bytes` holds it to.
    Sha1::from(bytes).digest().to_string()
}

/// The title to show for a ROM: see [`LoadedRom::title`].
///
/// The overseas field first: it is the one an exported cart carries, and the
/// shell shows one title rather than two (plutiedev "Game title"). The domestic
/// field next, for the carts that filled only that one. Then the file name, so
/// that an image with no usable header still has a name to show.
fn title_of(header: &RomHeader, file_name: &str) -> String {
    if !header.overseas_title.is_empty() {
        return header.overseas_title.clone();
    }
    if !header.domestic_title.is_empty() {
        return header.domestic_title.clone();
    }
    file_title(file_name)
}

/// `file_name` without its extension. A leading dot is part of a name rather
/// than an extension, so `.md` loses nothing.
fn file_title(file_name: &str) -> String {
    match file_name.rfind('.') {
        None | Some(0) => file_name.to_string(),
        Some(dot) => file_name[..dot].to_string(),
    }
}

/// The regions a header claims, spelled out for the top bar.
///
/// In a fixed order whatever order the field lists them in, so one ROM always
/// reads the same way. A header that claims nothing - homebrew, and a
/// surprising number of dumps - reads `"Unknown"` rather than nothing at all
/// (plutiedev "Region support").
fn region_of(header: &RomHeader) -> String {
    let claimed = [
        (header.regions.japan, "Japan"),
        (header.regions.americas, "Americas"),
        (header.regions.europe, "Europe"),
    ];
    let names: Vec<&str> = claimed
        .into_iter()
        .filter(|(is_claimed, _name)| *is_claimed)
        .map(|(_is_claimed, name)| name)
        .collect();
    if names.is_empty() {
        "Unknown".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two system type strings carts actually carry (plutiedev "ROM header
    /// reference"), padded to their 16 byte field.
    const MEGA_DRIVE: &str = "SEGA MEGA DRIVE ";
    const GENESIS: &str = "SEGA GENESIS    ";

    /// A ROM image of exactly 512 bytes: zero, then 0x100..0x200 filled with
    /// the space padding real headers use and written over with the four
    /// fields these tests care about. `load_rom` judges a ROM by its bytes and
    /// its length alone, so nothing here needs a plausible vector table.
    fn synthetic(system_type: &str, domestic: &str, overseas: &str, region: &str) -> Vec<u8> {
        let mut rom = vec![0u8; 0x200];
        rom[0x100..0x200].fill(b' ');
        put(&mut rom, 0x100, system_type.as_bytes());
        put(&mut rom, 0x120, domestic.as_bytes());
        put(&mut rom, 0x150, overseas.as_bytes());
        put(&mut rom, 0x1F0, region.as_bytes());
        rom
    }

    fn put(rom: &mut [u8], at: usize, bytes: &[u8]) {
        rom[at..at + bytes.len()].copy_from_slice(bytes);
    }

    #[test]
    fn empty_input_is_too_small() {
        let error = load_rom("empty.md", Vec::new()).unwrap_err();
        assert_eq!(error, RomError::TooSmall);
    }

    /// One byte under the header is the other half of the same boundary: the
    /// header ends at 0x200, so that is where "too small" stops.
    #[test]
    fn one_byte_under_a_header_is_too_small() {
        let error = load_rom("short.md", vec![0u8; 0x1FF]).unwrap_err();
        assert_eq!(error, RomError::TooSmall);
    }

    /// Both system types are load-bearing, because both are what carts carry:
    /// the title is the trimmed overseas field and the id is SHA-1 of exactly
    /// these bytes. The expected ids are pinned from outside this crate, from
    /// the same recipe run through `sha1sum`.
    #[test]
    fn mega_drive_header_gives_the_title_and_id() {
        let bytes = synthetic(MEGA_DRIVE, "", "SONIC THE HEDGEHOG", "JUE");
        let rom = load_rom("sonic.md", bytes.clone()).expect("a 512 byte ROM loads");
        assert_eq!(rom.title, "SONIC THE HEDGEHOG");
        assert_eq!(rom.id, "a0d352592ccac31e635688ec87aa50549d61d08d");
        assert_eq!(rom.file_name, "sonic.md");
        assert_eq!(rom.bytes, bytes);
    }

    #[test]
    fn genesis_header_gives_the_title_and_id() {
        let bytes = synthetic(GENESIS, "", "SONIC THE HEDGEHOG", "JUE");
        let rom = load_rom("sonic.md", bytes.clone()).expect("a 512 byte ROM loads");
        assert_eq!(rom.title, "SONIC THE HEDGEHOG");
        assert_eq!(rom.id, "a623b1d31bfa051d74bf2cd6278f547b09b09bee");
    }

    /// Homebrew and old dumps often fill only the domestic field, and a title
    /// padded with spaces must not come back as 48 spaces.
    #[test]
    fn domestic_title_is_used_when_the_overseas_one_is_blank() {
        let bytes = synthetic(MEGA_DRIVE, "SONIC THE HEDGEHOG II", "", "J");
        let rom = load_rom("homebrew.md", bytes).expect("a 512 byte ROM loads");
        assert_eq!(rom.title, "SONIC THE HEDGEHOG II");
    }

    /// No title in the header at all: the file name, extension dropped, is the
    /// only name there is. This is the case every dump without a header lands
    /// in, so it has to read well - the file names in a ROM set are of the
    /// form `Sonic The Hedgehog (USA, Europe).md`.
    #[test]
    fn blank_header_title_falls_back_to_the_file_name() {
        let cases = [
            (
                "Sonic The Hedgehog (USA, Europe).md",
                "Sonic The Hedgehog (USA, Europe)",
            ),
            ("ctr52416-32x.bin", "ctr52416-32x"),
            ("no-extension", "no-extension"),
        ];
        for (file_name, want) in cases {
            let bytes = synthetic(MEGA_DRIVE, "", "", "");
            let rom = load_rom(file_name, bytes).expect("a 512 byte ROM loads");
            assert_eq!(rom.title, want, "file name {file_name:?}");
        }
    }

    #[test]
    fn a_file_past_the_limit_is_too_large() {
        let bytes = vec![0u8; MAX_ROM_BYTES + 1];
        let error = load_rom("enormous.md", bytes).unwrap_err();
        assert_eq!(error, RomError::TooLarge);
    }

    /// The limit itself is a size a ROM may reach, so it must load rather than
    /// be rejected on a boundary.
    #[test]
    fn the_limit_itself_loads() {
        let bytes = vec![0u8; MAX_ROM_BYTES];
        let rom = load_rom("huge.md", bytes).expect("10 MiB exactly is allowed");
        assert_eq!(rom.bytes.len(), MAX_ROM_BYTES);
    }

    /// A 512 byte image counting from 0, whose SHA-1 is pinned from outside
    /// this crate: `python3 -c 'import sys; sys.stdout.buffer.write(bytes(range(256)) * 2)' | sha1sum`.
    /// The point is that the id is the SHA-1 of the bytes and of nothing else,
    /// checked against a vector this code did not produce.
    #[test]
    fn id_is_the_lowercase_hex_sha1_of_the_bytes() {
        let bytes: Vec<u8> = (0u16..0x200).map(|byte| byte as u8).collect();
        let rom = load_rom("counting.md", bytes).expect("a 512 byte ROM loads");
        assert_eq!(rom.id, "dbe649daba340bce7a44b809016d914839b99f10");
        assert!(
            rom.id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    /// Region text for the top bar: the header's own claim, in a fixed order
    /// whatever order the field lists them in (plutiedev "Region support").
    #[test]
    fn regions_are_named_in_a_fixed_order() {
        let cases = [
            ("JUE", "Japan, Americas, Europe"),
            ("EUJ", "Japan, Americas, Europe"),
            ("U E", "Americas, Europe"),
            ("J", "Japan"),
            // No claim, which is homebrew's default and not an error.
            ("", "Unknown"),
        ];
        for (field, want) in cases {
            let bytes = synthetic(MEGA_DRIVE, "", "TITLE", field);
            let rom = load_rom("regions.md", bytes).expect("a 512 byte ROM loads");
            assert_eq!(rom.region, want, "region field {field:?}");
        }
    }

    #[test]
    fn a_load_error_says_what_went_wrong() {
        assert_eq!(
            RomError::TooSmall.to_string(),
            "file is too small to hold a header at 0x100"
        );
        assert_eq!(
            RomError::TooLarge.to_string(),
            "file is larger than the 10 MiB this will load"
        );
    }
}
