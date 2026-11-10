//! Decoding of the cartridge header at 0x100..0x200.
//!
//! The header is how a cartridge describes itself: name, region, declared ROM
//! and RAM ranges, the device letters, and a checksum. It is the only part of
//! a ROM the console itself reads (TMSS looks for the `SEGA` system type), and
//! the input to the region, save-RAM and mapper decisions made in later
//! tickets.
//!
//! Reference: plutiedev "ROM header reference", cross-checked against Sega
//! Retro "ROM header". Where the two are quoted below they agree. All
//! multi-byte fields are big endian:
//!
//! ```text
//! 0x100  16  system type       0x190  16  device support
//! 0x110  16  copyright/date    0x1A0   4  ROM range start
//! 0x120  48  title (domestic)  0x1A4   4  ROM range end
//! 0x150  48  title (overseas)  0x1A8   4  RAM range start
//! 0x180  14  serial            0x1AC   4  RAM range end
//! 0x18E   2  checksum          0x1B0  12  extra memory
//! 0x1F0   3  region support
//! ```

use super::LoadError;

/// First byte of the header.
const HEADER_START: usize = 0x100;
/// One past the last header byte, so a ROM must be at least this long.
const HEADER_END: usize = 0x200;
/// First byte covered by [`compute_checksum`], which sums the data after the
/// header rather than the header itself.
const CHECKSUM_DATA_START: usize = 0x200;

/// Regions the software declares itself compatible with, from the field at
/// 0x1F0.
///
/// All three `false` means the header says nothing usable, which is the common
/// case for homebrew and for badly built dumps; it is not an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regions {
    /// Japan, NTSC.
    pub japan: bool,
    /// Americas, NTSC.
    pub americas: bool,
    /// Europe, PAL.
    pub europe: bool,
}

/// The cartridge header, decoded from 0x100..0x200.
///
/// Every field is informational: nothing in here is trusted enough to refuse a
/// load, including [`RomHeader::checksum`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomHeader {
    /// 0x100: `"SEGA MEGA DRIVE "`, `"SEGA GENESIS      "`, `"SEGA 32X      "`,
    /// ... Only the `"SEGA"` prefix is meaningful: TMSS refuses a ROM without
    /// it, and nothing else looks at the rest.
    pub system_type: String,
    /// 0x110: `"(C)SEGA 1991.APR"` - publisher code, release year, month.
    pub copyright: String,
    /// 0x120: the Japanese-market title. Often Shift-JIS, kept as Latin-1.
    pub domestic_title: String,
    /// 0x150: the export title.
    pub overseas_title: String,
    /// 0x180: `"GM 00001009-00"` - software type, serial, revision.
    pub serial: String,
    /// 0x18E: the checksum the cartridge claims, big endian.
    pub checksum: u16,
    /// 0x190: device support letters, `"J"` for a 3-button pad, `"J6"` when
    /// the game uses the extra buttons, and so on.
    pub io_support: String,
    /// 0x1A0: first address covered by the cartridge, always 0 outside mappers.
    pub rom_start: u32,
    /// 0x1A4: last address covered by the cartridge, usually ROM size - 1.
    pub rom_end: u32,
    /// 0x1A8: RAM start, always 0xFF0000 on the Mega Drive.
    pub ram_start: u32,
    /// 0x1AC: RAM end, always 0xFFFFFF on the Mega Drive.
    pub ram_end: u32,
    /// 0x1B0: the 12 raw extra-memory bytes (`"RA"`, type, 0x20, start, end).
    /// Decoding them - SRAM or EEPROM, and which kind - is M3-11 (#71).
    pub extra_memory: [u8; 12],
    /// 0x1F0: region support.
    pub regions: Regions,
}

/// Parse the header of a Genesis ROM image.
///
/// Only the length is checked: a header full of nonsense still parses, because
/// the fields drive display and heuristics, never correctness, and a game with
/// a mangled header still has to run.
pub fn parse_header(rom: &[u8]) -> Result<RomHeader, LoadError> {
    if rom.len() < HEADER_END {
        return Err(LoadError::TooSmall);
    }
    // One whole header, so the slices below are all in bounds. Their offsets
    // are the ones in the layout table at the top of this module, less 0x100.
    let h = &rom[HEADER_START..HEADER_END];
    Ok(RomHeader {
        system_type: text(&h[0x00..0x10]),
        copyright: text(&h[0x10..0x20]),
        domestic_title: text(&h[0x20..0x50]),
        overseas_title: text(&h[0x50..0x80]),
        serial: text(&h[0x80..0x8E]),
        checksum: be_u16(&h[0x8E..0x90]),
        io_support: text(&h[0x90..0xA0]),
        rom_start: be_u32(&h[0xA0..0xA4]),
        rom_end: be_u32(&h[0xA4..0xA8]),
        ram_start: be_u32(&h[0xA8..0xAC]),
        ram_end: be_u32(&h[0xAC..0xB0]),
        extra_memory: extra_memory(&h[0xB0..0xBC]),
        regions: parse_regions(&h[0xF0..0xF3]),
    })
}

impl RomHeader {
    /// Does [`Self::checksum`] agree with [`compute_checksum`] for `rom`?
    ///
    /// Informational only, and it must stay that way: plenty of shipped games
    /// carry a wrong checksum, so a mismatch never refuses a load, it only
    /// tells the frontend that what it is showing is the cartridge's own claim
    /// rather than a verified value.
    pub fn checksum_ok(&self, rom: &[u8]) -> bool {
        self.checksum == compute_checksum(rom)
    }
}

/// Checksum of a ROM image: the wrapping sum of every big-endian 16-bit word
/// from 0x200 to the end of the ROM (plutiedev "ROM checksum"). Sega Retro's
/// "Checksum" notes that whether to check it at runtime, and what to do when it
/// fails, was up to the software - which is why this is a separate call rather
/// than a step of [`parse_header`].
///
/// A ROM with no bytes past 0x200 has nothing to sum, and returns 0.
pub fn compute_checksum(rom: &[u8]) -> u16 {
    let data = rom.get(CHECKSUM_DATA_START..).unwrap_or(&[]);
    let (words, tail) = data.as_chunks::<2>();
    let mut sum = 0u16;
    for word in words {
        sum = sum.wrapping_add(u16::from_be_bytes(*word));
    }
    // ROM images are word sized in practice, so the leftover byte this handles
    // is bad input rather than a format. It is the high half of its word,
    // because the run of words starts at the even address 0x200.
    if let Some(&tail_byte) = tail.first() {
        sum = sum.wrapping_add(u16::from_be_bytes([tail_byte, 0]));
    }
    sum
}

/// Decode one padded header text field.
///
/// Latin-1: every byte becomes the code point of the same value, so the ASCII
/// Sega filled in survives exactly and the Shift-JIS that plutiedev "Game
/// title" permits in domestic titles survives as mojibake instead of failing
/// the parse. Space is the padding Sega mandated; NUL turns up in homebrew.
fn text(field: &[u8]) -> String {
    field
        .iter()
        .map(|&byte| char::from(byte))
        .collect::<String>()
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_string()
}

// The three helpers below take a field slice that is already the length the
// header says it is: `parse_header` cut them out of a 0x100 byte header, so the
// indexing here cannot run off the end.

fn be_u16(field: &[u8]) -> u16 {
    u16::from_be_bytes([field[0], field[1]])
}

fn be_u32(field: &[u8]) -> u32 {
    u32::from_be_bytes([field[0], field[1], field[2], field[3]])
}

fn extra_memory(field: &[u8]) -> [u8; 12] {
    let mut raw = [0u8; 12];
    raw.copy_from_slice(field);
    raw
}

/// Region support, from the three bytes at 0x1F0.
///
/// Two formats were used over the life of the console (plutiedev "Region
/// support", segaretro "Regional compatiblity"): the old style, one letter per
/// region, and the new style, a single ASCII hex digit.
fn parse_regions(field: &[u8]) -> Regions {
    if let Some(bits) = new_style_bits(field) {
        return Regions {
            japan: bits & 0b0001 != 0,
            americas: bits & 0b0100 != 0,
            europe: bits & 0b1000 != 0,
        };
    }
    // Old style: one letter per region, in any order. plutiedev warns that the
    // padding is not always at the end, so all three bytes are scanned.
    let mut regions = Regions::default();
    for &byte in field {
        match byte {
            b'J' => regions.japan = true,
            b'U' => regions.americas = true,
            b'E' => regions.europe = true,
            _ => {}
        }
    }
    regions
}

/// The new-style region byte as its four bits, or `None` if this field is not
/// new style (plutiedev "New style", segaretro "1994 system").
///
/// Bit 0 is Japan NTSC, bit 1 the never-used Japan 50 Hz, bit 2 the Americas
/// NTSC, bit 3 Europe PAL.
///
/// `E` is deliberately not read as the hex digit 0xE: as the old-style letter
/// it means Europe only, and both references say to read it that way, since a
/// game claiming the 50 Hz-Japan bit is effectively nonexistent.
fn new_style_bits(field: &[u8]) -> Option<u8> {
    let (&first, rest) = field.split_first()?;
    if !rest.iter().all(|&byte| is_padding(byte)) {
        return None;
    }
    match first {
        b'0'..=b'9' => Some(first - b'0'),
        b'A'..=b'D' | b'F' => Some(first - b'A' + 10),
        _ => None,
    }
}

/// Padding in the region field: space is what Sega mandated, NUL is what
/// sloppy builders left.
fn is_padding(byte: u8) -> bool {
    byte == b' ' || byte == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::require_data;

    /// A ROM image `size` bytes long: everything zero except 0x100..0x200,
    /// which is pre-filled with the space padding real headers use and then
    /// written over with a valid header. Every field has a known value, so a
    /// test only has to override the one it is about.
    fn fake_rom(size: usize) -> Vec<u8> {
        assert!(size >= 0x200, "fake_rom needs room for a header");
        let mut rom = vec![0u8; size];
        rom[0x100..0x200].fill(b' ');
        put(&mut rom, 0x100, b"SEGA MEGA DRIVE ");
        put(&mut rom, 0x110, b"(C)TST 2024.JAN");
        put(&mut rom, 0x120, b"DOMESTIC TITLE");
        put(&mut rom, 0x150, b"OVERSEAS TITLE");
        put(&mut rom, 0x180, b"GM 00001009-00");
        put(&mut rom, 0x190, b"J6");
        put_u32(&mut rom, 0x1A0, 0x00_0000);
        put_u32(&mut rom, 0x1A4, 0x0F_FFFF);
        put_u32(&mut rom, 0x1A8, 0xFF_0000);
        put_u32(&mut rom, 0x1AC, 0xFF_FFFF);
        put(&mut rom, 0x1B0, b"RA\xF8\x20");
        put_u32(&mut rom, 0x1B4, 0x20_0001);
        put_u32(&mut rom, 0x1B8, 0x20_FFFF);
        rom
    }

    /// Write `bytes` at `at`, as the header stores its text fields.
    fn put(rom: &mut [u8], at: usize, bytes: &[u8]) {
        rom[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Write a big-endian u16 at `at`, as the header stores its checksum.
    fn put_u16(rom: &mut [u8], at: usize, value: u16) {
        rom[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian u32 at `at`, as the header stores its addresses.
    fn put_u32(rom: &mut [u8], at: usize, value: u32) {
        rom[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn parses_system_type_and_titles() {
        let mut rom = fake_rom(0x200);
        // Space padding is what the headers Sega shipped use; NUL padding is
        // what some homebrew and leaked builders use. Both are padding.
        put(&mut rom, 0x120, b"SONIC THE HEDGEHOG\0\0\0");
        let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
        assert_eq!(header.system_type, "SEGA MEGA DRIVE");
        assert_eq!(header.copyright, "(C)TST 2024.JAN");
        assert_eq!(header.domestic_title, "SONIC THE HEDGEHOG");
        assert_eq!(header.overseas_title, "OVERSEAS TITLE");
    }

    #[test]
    fn parses_serial_and_rom_range() {
        let mut rom = fake_rom(0x200);
        put_u32(&mut rom, 0x1A4, 0x07_FFFF);
        let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
        assert_eq!(header.serial, "GM 00001009-00");
        assert_eq!(header.rom_start, 0x00_0000);
        assert_eq!(header.rom_end, 0x07_FFFF);
        assert_eq!(header.ram_start, 0xFF_0000);
        assert_eq!(header.ram_end, 0xFF_FFFF);
    }

    #[test]
    fn parses_io_support_and_extra_memory() {
        let rom = fake_rom(0x200);
        let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
        assert_eq!(header.io_support, "J6");
        // The raw twelve bytes only: deciding what the extra memory *is* is
        // M3-11 (#71).
        assert_eq!(&header.extra_memory[0..4], b"RA\xF8\x20");
        assert_eq!(&header.extra_memory[4..8], &[0x00, 0x20, 0x00, 0x01]);
        assert_eq!(&header.extra_memory[8..12], &[0x00, 0x20, 0xFF, 0xFF]);
    }

    #[test]
    fn parses_old_style_region_letters() {
        let cases = [
            (
                "J",
                Regions {
                    japan: true,
                    americas: false,
                    europe: false,
                },
            ),
            (
                "U",
                Regions {
                    japan: false,
                    americas: true,
                    europe: false,
                },
            ),
            // "E" is read as the old-style letter, never as the new-style hex
            // digit 0xE: plutiedev "Old style" and segaretro "1994 system"
            // both say to assume Europe.
            (
                "E",
                Regions {
                    japan: false,
                    americas: false,
                    europe: true,
                },
            ),
            (
                "JUE",
                Regions {
                    japan: true,
                    americas: true,
                    europe: true,
                },
            ),
            (
                "EUJ",
                Regions {
                    japan: true,
                    americas: true,
                    europe: true,
                },
            ),
            // Padding can sit in the middle of the field, not just at the end,
            // so all three bytes are checked (plutiedev "Old style").
            (
                "J E",
                Regions {
                    japan: true,
                    americas: false,
                    europe: true,
                },
            ),
        ];
        for (field, want) in cases {
            let mut rom = fake_rom(0x200);
            put(&mut rom, 0x1F0, field.as_bytes());
            let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
            assert_eq!(header.regions, want, "region field {field:?}");
        }
    }

    #[test]
    fn parses_new_style_region_hex() {
        // One ASCII hex digit, bit 0 = Japan NTSC, bit 2 = Americas NTSC,
        // bit 3 = Europe PAL (plutiedev "New style", segaretro "1994 system").
        let cases = [
            (
                "1",
                Regions {
                    japan: true,
                    americas: false,
                    europe: false,
                },
            ),
            (
                "4",
                Regions {
                    japan: false,
                    americas: true,
                    europe: false,
                },
            ),
            (
                "8",
                Regions {
                    japan: false,
                    americas: false,
                    europe: true,
                },
            ),
            // Bit 1 is "Japan PAL", a region with no home here, so it claims
            // none of the three.
            (
                "2",
                Regions {
                    japan: false,
                    americas: false,
                    europe: false,
                },
            ),
            (
                "D",
                Regions {
                    japan: true,
                    americas: true,
                    europe: true,
                },
            ),
            (
                "F",
                Regions {
                    japan: true,
                    americas: true,
                    europe: true,
                },
            ),
        ];
        for (digit, want) in cases {
            let mut rom = fake_rom(0x200);
            put(&mut rom, 0x1F0, digit.as_bytes());
            let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
            assert_eq!(header.regions, want, "region digit {digit}");
        }
    }

    #[test]
    fn empty_region_means_unknown() {
        for field in ["   ", "\0\0\0", " \0 "] {
            let mut rom = fake_rom(0x200);
            put(&mut rom, 0x1F0, field.as_bytes());
            let header = parse_header(&rom).expect("a 0x200 byte ROM parses");
            assert_eq!(header.regions, Regions::default(), "region field {field:?}");
        }
    }

    #[test]
    fn short_rom_errors() {
        assert_eq!(parse_header(&[]).unwrap_err(), LoadError::TooSmall);
        assert_eq!(
            parse_header(&vec![0u8; 0x1FF]).unwrap_err(),
            LoadError::TooSmall
        );
        // Exactly 0x200 is enough: the header ends at 0x200.
        assert!(parse_header(&vec![0u8; 0x200]).is_ok());
    }

    /// The acceptance box: 0x200 bytes of 0xFF is a header no console would
    /// boot, and parsing one must still not panic.
    #[test]
    fn all_ff_header_parses() {
        let rom = vec![0xFFu8; 0x200];
        let header = parse_header(&rom).expect("garbage parses, it does not panic");
        assert_eq!(header.checksum, 0xFFFF);
        assert_eq!(header.rom_start, 0xFFFF_FFFF);
        assert_eq!(header.rom_end, 0xFFFF_FFFF);
        assert_eq!(header.ram_start, 0xFFFF_FFFF);
        assert_eq!(header.ram_end, 0xFFFF_FFFF);
        // 0xFF is neither a region letter nor a hex digit.
        assert_eq!(header.regions, Regions::default());
    }

    #[test]
    fn computes_checksum() {
        // Three big-endian words past the header: 0x1234 + 0x0001 + 0x0002.
        let mut rom = fake_rom(0x206);
        put_u16(&mut rom, 0x200, 0x1234);
        put_u16(&mut rom, 0x202, 0x0001);
        put_u16(&mut rom, 0x204, 0x0002);
        assert_eq!(compute_checksum(&rom), 0x1237);
    }

    /// The sum runs from 0x200 to the end of the ROM: neither the vectors nor
    /// the header itself are in it (plutiedev "ROM checksum").
    #[test]
    fn checksum_covers_only_the_bytes_after_the_header() {
        let mut rom = fake_rom(0x202);
        assert_eq!(compute_checksum(&rom), 0);
        rom[..0x200].fill(0xFF);
        assert_eq!(compute_checksum(&rom), 0);
    }

    #[test]
    fn checksum_wraps_at_sixteen_bits() {
        let mut rom = fake_rom(0x204);
        put_u16(&mut rom, 0x200, 0xFFFF);
        put_u16(&mut rom, 0x202, 0x0003);
        assert_eq!(compute_checksum(&rom), 0x0002);
    }

    /// A ROM image is an even number of bytes on real hardware, so an odd tail
    /// is bad input rather than a format. It must not panic, and the last byte
    /// holds the high half of its word because 0x200 is even.
    #[test]
    fn checksum_of_an_odd_length_rom() {
        let mut rom = fake_rom(0x203);
        rom[0x200..0x203].copy_from_slice(&[0x00, 0x01, 0xFF]);
        assert_eq!(compute_checksum(&rom), 0x0001 + 0xFF00);
    }

    #[test]
    fn checksum_of_a_rom_with_no_data_is_zero() {
        assert_eq!(compute_checksum(&[]), 0);
        assert_eq!(compute_checksum(&vec![0xFFu8; 0x1FF]), 0);
        assert_eq!(compute_checksum(&vec![0xFFu8; 0x200]), 0);
    }

    #[test]
    fn checksum_ok_flag() {
        let mut rom = fake_rom(0x204);
        put_u16(&mut rom, 0x200, 0xABCD);
        let computed = compute_checksum(&rom);
        put_u16(&mut rom, 0x18E, computed);
        let header = parse_header(&rom).expect("a 0x204 byte ROM parses");
        assert_eq!(header.checksum, computed);
        assert!(header.checksum_ok(&rom));

        // Lots of shipped games get the header checksum wrong and still have
        // to run, so a mismatch is a flag and never a load error.
        put_u16(&mut rom, 0x18E, computed.wrapping_add(1));
        let header = parse_header(&rom).expect("a bad checksum still parses");
        assert!(!header.checksum_ok(&rom));
    }

    /// The same decode end to end on a real ROM. Local only: CI has no
    /// commercial ROMs, so `require_data!` skips it there.
    ///
    /// The file name is pinned on purpose. `find_commercial_rom` takes the
    /// first match in sorted order, and `"Sonic The Hedgehog (USA, Europe)"`
    /// sorts to the `(Beta)` dump, whose title fields are blank.
    #[test]
    #[ignore = "needs a commercial ROM, which only exists locally"]
    fn parses_real_sonic_header() {
        let path = require_data!(testkit::commercial_rom(
            "Sonic The Hedgehog (USA, Europe).md"
        ));
        let rom = std::fs::read(path).expect("read the Sonic ROM");
        let header = parse_header(&rom).expect("Sonic parses");
        assert!(
            header.system_type.starts_with("SEGA"),
            "{}",
            header.system_type
        );
        assert!(
            header.overseas_title.contains("SONIC"),
            "{}",
            header.overseas_title
        );
        assert_eq!(header.serial, "GM 00001009-00");
        assert_eq!(header.rom_start, 0x00_0000);
        assert_eq!(header.rom_end, 0x07_FFFF);
        assert_eq!(header.ram_start, 0xFF_0000);
        assert_eq!(header.ram_end, 0xFF_FFFF);
        assert!(header.regions.japan);
        assert!(header.regions.americas);
        assert!(header.regions.europe);
        assert!(
            header.checksum_ok(&rom),
            "header says {:04X}, computed {:04X}",
            header.checksum,
            compute_checksum(&rom),
        );
    }
}
