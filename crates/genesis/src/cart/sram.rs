//! Battery-backed save RAM (SRAM) as a cartridge declares it at header 0x1B0.
//!
//! A cart that can keep a game's progress describes it in the twelve bytes after
//! the RAM range of its header - `'R' 'A'`, a type byte, a blank, and the range
//! of the save (plutiedev "Saving progress with SRAM", "ROM header changes", and
//! segaretro "ROM header", "External RAM data"):
//!
//! ```text
//! 0x1B0  2  'R' 'A'        the signature; any other two letters, no save RAM
//! 0x1B2  1  type           bit 6 battery backed, bits 4-3 width and parity
//! 0x1B3  1  blank          0x20 in Sega's carts, and nothing reads it
//! 0x1B4  4  first address  usually 0x200001
//! 0x1B8  4  last address   usually 0x20FFFF
//! ```
//!
//! The save lives *inside* the cartridge area at 0x000000-0x3FFFFF, so a read of
//! it is a read of what the cartridge slot answers there: `crate::bus` decides
//! that order in `read_rom_byte`, and [`Sram`] is the 0xA130F1 switch that puts
//! the save in front of the ROM.

use super::header::parse_header;
use crate::bus::OPEN_BUS;

/// The bit of the type byte that says the extra RAM is battery backed.
///
/// segaretro "ROM header" spells the same three widths twice, once without it
/// ($A0 16-bit, $B0 8-bit even, $B8 8-bit odd) and once as "Backup" ($E0, $F0,
/// $F8), and a cart whose extra RAM loses its contents when the power is
/// switched off has no save to persist.
const BACKED_UP: u8 = 0x40;

/// The bits of the type byte that choose the width: segaretro's $00/$10/$18 of
/// `type & 0x18`, for 16-bit, 8-bit even, and 8-bit odd. The fourth choice,
/// 0x08 ($E8), is the cart's EEPROM, which is M3-13 (#73).
const WIDTH: u8 = 0x18;
const WIDTH_WORD: u8 = 0x00;
const WIDTH_EVEN: u8 = 0x10;
const WIDTH_ODD: u8 = 0x18;
/// The last address of the cartridge area ([`crate::bus`]'s `Region::Rom`), so
/// the last address a save window can be decoded at: Genesis Plus GX lists a few
/// carts that wire save RAM to 0x400001 and one bootleg to 0x7E0000, and nothing
/// on this bus decodes either, so a window out there is not a save this emulator
/// can offer.
const CARTRIDGE_END: u32 = 0x3F_FFFF;
/// The largest save RAM any Genesis cart shipped with, and the point of
/// refusing a wider declaration: without it, a header whose window is nonsense
/// is an enormous allocation rather than a cart nothing saves on. Genesis Plus
/// GX sizes its save the same way, "Assuming max. 64k backup RAM throughout".
const MAX_STORAGE: usize = 0x1_0000;
/// What save RAM reads before anything has written it. See [`Sram::new`].
const POWER_UP: u8 = 0xFF;

/// `0xA130F1` bit 0: the save RAM shows up in the cartridge area in place of the
/// ROM (plutiedev "Saving progress with SRAM", `SramLock: equ $A130F1`).
const MAP_BIT: u8 = 1;
/// `0xA130F1` bit 1: writes to the save RAM are refused (Genesis Plus GX's
/// `mapper_sega_w` takes both bits from the same write, which is how they are
/// read here too).
const WRITE_PROTECT_BIT: u8 = 2;

/// Which half of the 16-bit cartridge bus a cart's save RAM answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SramKind {
    /// 8-bit save RAM in the odd bytes: `start`, `start + 2`, `start + 4`, ...
    /// of the window is storage, and the even bytes in between are nothing.
    /// Type byte $F8, and by a wide margin the common case.
    Odd,
    /// 8-bit save RAM in the even bytes, the mirror image. Type byte $F0.
    Even,
    /// 16-bit save RAM: every byte of the window is storage. Type byte $E0.
    Word,
}

/// Where a cart's save RAM is and how wide it is: the twelve raw extra-memory
/// bytes of a header, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SramInfo {
    /// First address of the window, inclusive: 0x200001 for the usual cart.
    pub start: u32,
    /// Last address of the window, inclusive: 0x20FFFF for the usual cart.
    pub end: u32,
    /// Which half of the bus the save answers.
    pub kind: SramKind,
}

impl SramInfo {
    /// Decode the raw [`RomHeader::extra_memory`](super::RomHeader::extra_memory)
    /// of a header, or `None` when it declares no save RAM this emulator can
    /// offer: no `RA` signature, a cart that says its extra RAM is not battery
    /// backed, an EEPROM (M3-13, #73), or a window the bus cannot serve
    /// ([`CARTRIDGE_END`], [`MAX_STORAGE`]).
    ///
    /// The blank at `raw[3]` is not checked: segaretro calls it "(Blank)" and
    /// plutiedev's example fills it with the 0x20 of a space, but nothing that
    /// reads a cart header looks at it.
    pub fn from_extra_memory(raw: &[u8; 12]) -> Option<Self> {
        if &raw[0..2] != b"RA" {
            return None;
        }
        if raw[2] & BACKED_UP == 0 {
            return None;
        }
        let kind = match raw[2] & WIDTH {
            WIDTH_WORD => SramKind::Word,
            WIDTH_EVEN => SramKind::Even,
            WIDTH_ODD => SramKind::Odd,
            // 0x08: EEPROM, of segaretro's table.
            _ => return None,
        };
        let info = Self {
            start: u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]),
            end: u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]),
            kind,
        };
        info.is_served().then_some(info)
    }

    /// Bytes of storage this window needs. Half-width save RAM holds half of its
    /// addresses, and the arithmetic is over `end - start` rather than
    /// `end - start + 1` because `start` is storage byte 0: `start + 2n` is
    /// storage byte `n`.
    pub fn storage_len(&self) -> usize {
        let span = (self.end - self.start) as usize;
        match self.kind {
            SramKind::Word => span + 1,
            SramKind::Odd | SramKind::Even => span / 2 + 1,
        }
    }

    /// Can this window be decoded to a real save: forwards, inside the cartridge
    /// area, and no larger than a cart save is.
    fn is_served(&self) -> bool {
        self.start <= self.end && self.end <= CARTRIDGE_END && self.storage_len() <= MAX_STORAGE
    }

    /// Is the window reachable with nothing written to `0xA130F1`?
    ///
    /// Yes when the loaded image stops short of it, which is what
    /// `image_len <= start` says: a cart whose ROM ends at 0x1FFFFF has nothing
    /// at 0x200001 to switch out of the way, so the save answers on its own.
    /// Genesis Plus GX puts the same condition in `md_cart.c`, "SRAM is mapped by
    /// default unless it overlaps with ROM area", naming the carts that do
    /// overlap - Phantasy Star 4, Beyond Oasis, World Series Baseball 9x, Duke
    /// Nukem 3D - as the ones that have to use the register.
    ///
    /// The folklore version of this rule adds "and the ROM is 2 MB or smaller".
    /// That number comes from a different thing: with a header that says nothing
    /// at all, Genesis Plus GX *invents* a 64 KiB save at 0x200000-0x20FFFF for
    /// any image of 2 MB or less. This does not, because inventing a save for
    /// every small homebrew would make [`crate::Bus::sram`] report one for a cart
    /// that cannot have one; a cart with save RAM its own header forgot is what
    /// [`SRAM_OVERRIDES`] is for.
    pub fn mapped_by_default(&self, image_len: usize) -> bool {
        image_len <= self.start as usize
    }

    /// Which byte of the storage the 68k address `addr` names, or `None` when
    /// `addr` is not this window's at all.
    ///
    /// The parity check is the offset from `start`, not the parity of `addr`, so
    /// a cart that declares its odd window as 0x200000-0x20FFFF rather than
    /// 0x200001-0x20FFFF still gets `start + 2n` as storage byte `n`.
    fn storage_index(&self, addr: u32) -> Option<usize> {
        if addr < self.start || addr > self.end {
            return None;
        }
        let offset = (addr - self.start) as usize;
        match self.kind {
            SramKind::Word => Some(offset),
            SramKind::Odd | SramKind::Even => offset.is_multiple_of(2).then_some(offset / 2),
        }
    }
}

/// One cart's save RAM: the storage, the two `0xA130F1` bits, and whether the
/// front end still has to persist it.
#[derive(Debug)]
pub struct Sram {
    /// What the header declared - the window and its width, which is what anything
    /// reading [`Self::contents`] needs to make sense of it.
    pub info: SramInfo,
    /// The save itself, packed: 32 KiB for the usual odd-only window, addressed
    /// by [`SramInfo::storage_index`].
    ///
    /// Packed rather than a 64 KiB array read at `addr & 0xFFFF` the way Genesis
    /// Plus GX does it, so that what [`crate::Bus::sram`] hands the front end is
    /// the 32 KiB the cart can actually keep.
    data: Vec<u8>,
    /// `0xA130F1` bit 0.
    enabled: bool,
    /// `0xA130F1` bit 1.
    write_protect: bool,
    /// Has the save changed since [`Sram::take_dirty`] last cleared it?
    dirty: bool,
    /// Does the image stop short of the window, so that the save answers without
    /// `0xA130F1` bit 0? See [`SramInfo::mapped_by_default`].
    mapped_by_default: bool,
}

impl Sram {
    /// A save for `info` over a ROM image `image_len` bytes long.
    ///
    /// The storage is filled with [`POWER_UP`] (0xFF), which is a decision and
    /// not a fact about hardware: real SRAM powers up undefined, and plutiedev
    /// "Saving progress with SRAM", "Precautions", tells authors to expect
    /// garbage and to notice it with a magic value and a checksum of their own.
    /// 0xFF is what other emulators fill it with - Genesis Plus GX's `sram_init`
    /// does it "by default ... (Micro Machines 2, Dino Dini Soccer)" - because
    /// it is what nothing driving the data bus reads as ([`OPEN_BUS`]) and
    /// because a game checking for a save finds none, which is the safe answer
    /// for a cart whose battery has run down.
    pub fn new(info: SramInfo, image_len: usize) -> Self {
        Self {
            info,
            data: vec![POWER_UP; info.storage_len()],
            enabled: false,
            write_protect: false,
            dirty: false,
            mapped_by_default: info.mapped_by_default(image_len),
        }
    }

    /// The save itself, for [`crate::Bus::sram`].
    pub fn contents(&self) -> &[u8] {
        &self.data
    }

    /// Is the save mapped over the cartridge area right now?
    pub fn mapped(&self) -> bool {
        self.enabled || self.mapped_by_default
    }

    /// The byte the save answers for the 68k address `addr`:
    ///
    /// - `Some(value)` when `addr` is inside a mapped window - the storage on
    ///   the half the window wires up, and [`OPEN_BUS`] on the half a half-width
    ///   window does not. A mapped window owns all of itself: the image stopped
    ///   answering there the moment the save was switched in, so nothing drives
    ///   the dead half either;
    /// - `None` when the window is not mapped or `addr` is outside it, which is
    ///   the cartridge image's turn.
    pub fn read(&self, addr: u32) -> Option<u8> {
        if !self.mapped() {
            return None;
        }
        match self.info.storage_index(addr) {
            Some(index) => Some(self.data[index]),
            None if self.covers(addr) => Some(OPEN_BUS),
            None => None,
        }
    }

    /// A write from the 68k to `addr`, answered with whether the window owns the
    /// address: `false` means it is outside a mapped window and so the cartridge
    /// image's turn, which takes no writes either.
    ///
    /// `true` means the window answered, not that the byte was stored: a
    /// write-protected save, and the dead half of a half-width window, both
    /// refuse silently. [`Sram::dirty`] is only set when a byte really is stored.
    pub fn write(&mut self, addr: u32, value: u8) -> bool {
        if !self.mapped() {
            return false;
        }
        // `covers` first: outside the window the save has no opinion, and the
        // caller's fall-through is the image, which is also a no-op.
        if !self.covers(addr) {
            return false;
        }
        if self.write_protect {
            return true;
        }
        if let Some(index) = self.info.storage_index(addr) {
            self.data[index] = value;
            self.dirty = true;
        }
        true
    }

    /// `0xA130F1`: bit 0 maps the save over the cartridge area, bit 1 write
    /// protects it. Both are levels, not edges, and both are taken from the one
    /// byte written - which is how Genesis Plus GX's `mapper_sega_w` reads the
    /// same register, and why [`crate::Bus`] hands it the whole byte.
    pub fn write_control(&mut self, value: u8) {
        self.enabled = value & MAP_BIT != 0;
        self.write_protect = value & WRITE_PROTECT_BIT != 0;
    }

    /// Has the save changed since this was last asked, clearing it? The front end
    /// persists on this edge rather than on a timer, and only ever once for each
    /// change.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    /// Put a save back, as [`crate::Bus::load_sram`] does.
    ///
    /// A blob of any other length is another cart's save, so it is refused whole
    /// and the contents stay exactly as they were - `load_sram` answers nothing,
    /// so a half-applied restore could not be noticed. Restoring is also not a
    /// change: `take_dirty` stays false, because the front end has just written
    /// what it is already holding.
    pub fn load(&mut self, data: &[u8]) {
        if data.len() != self.data.len() {
            return;
        }
        self.data.copy_from_slice(data);
    }

    /// Restart the cart slot's state, which is what a reset does: the two
    /// `0xA130F1` bits go back to 0, because that is what they are before the 68k
    /// has written them. The save itself does not: `RESET` is not a power cycle,
    /// the battery is still holding the bytes, and so is [`Sram::dirty`].
    pub fn reset(&mut self) {
        self.enabled = false;
        self.write_protect = false;
    }

    /// Is `addr` inside the declared window, mapped or not, on a live half or
    /// not?
    fn covers(&self, addr: u32) -> bool {
        (self.info.start..=self.info.end).contains(&addr)
    }
}

/// Serials whose carts have save RAM their own header does not declare.
///
/// Empty for now, and M6-05 (#124) - the compatibility sweep - is the ticket that
/// fills it: it is the one that runs the ROM set and finds the games that save
/// with no `RA` in their header, which Genesis Plus GX handles with a list of
/// serials and checksums in exactly this position ([`detect`]). A table rather
/// than a guess because a save RAM inferred from a heuristic is a save RAM a
/// front end persists for a cart that may not have one.
pub const SRAM_OVERRIDES: &[(&str, SramInfo)] = &[];

/// The declared save RAM of the cart `serial`, from [`SRAM_OVERRIDES`].
pub fn sram_override(serial: &str) -> Option<SramInfo> {
    SRAM_OVERRIDES
        .iter()
        .find(|(known, _)| *known == serial)
        .map(|(_, info)| *info)
}

/// What save RAM the ROM image `rom` declares: the header's own answer at 0x1B0
/// if it has one, and otherwise the cart's serial in [`SRAM_OVERRIDES`].
///
/// This is a scan of the image, not a load: turning a file into a machine is
/// M3-15's loader (#76), which will have parsed the header already and can pass
/// what it found along.
pub fn detect(rom: &[u8]) -> Option<SramInfo> {
    let header = parse_header(rom).ok()?;
    SramInfo::from_extra_memory(&header.extra_memory).or_else(|| sram_override(&header.serial))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The twelve raw extra-memory bytes of a header (`RomHeader::extra_memory`)
    /// declaring save RAM of type `kind` over `start ..= end`: `RA`, the type,
    /// the blank byte, and the two big-endian addresses (plutiedev "Saving
    /// progress with SRAM", "ROM header changes").
    fn extra_memory(kind: u8, start: u32, end: u32) -> [u8; 12] {
        let mut raw = [0u8; 12];
        raw[0..2].copy_from_slice(b"RA");
        raw[2] = kind;
        raw[3] = 0x20;
        raw[4..8].copy_from_slice(&start.to_be_bytes());
        raw[8..12].copy_from_slice(&end.to_be_bytes());
        raw
    }

    /// The range plutiedev tells homebrew authors to use: 32 KiB of odd bytes.
    const ODD: [u8; 12] = [
        b'R', b'A', 0xF8, 0x20, 0x00, 0x20, 0x00, 0x01, 0x00, 0x20, 0xFF, 0xFF,
    ];

    #[test]
    fn parses_sram_header_odd() {
        // "RA", $F8: segaretro "ROM header" lists $F8 as "Backup, 8-bit
        // addresses (odd)", and this exact range as the example in plutiedev
        // "Saving progress with SRAM".
        let info = SramInfo::from_extra_memory(&ODD).expect("the documented SRAM header");
        assert_eq!(info.start, 0x20_0001);
        assert_eq!(info.end, 0x20_FFFF);
        assert_eq!(info.kind, SramKind::Odd);
        assert_eq!(info.storage_len(), 32 * 1024);
    }

    #[test]
    fn parses_sram_header_even() {
        let info =
            SramInfo::from_extra_memory(&extra_memory(0xF0, 0x20_0000, 0x20_FFFF)).expect("$F0");
        assert_eq!(info.kind, SramKind::Even);
        assert_eq!(info.storage_len(), 32 * 1024);
    }

    #[test]
    fn parses_sram_header_word() {
        let info =
            SramInfo::from_extra_memory(&extra_memory(0xE0, 0x20_0000, 0x20_1FFF)).expect("$E0");
        assert_eq!(info.kind, SramKind::Word);
        // 16-bit SRAM answers every byte of its range, so the storage is the
        // whole span rather than every other byte of it.
        assert_eq!(info.storage_len(), 8 * 1024);
    }

    /// The two ways a cart says "no save RAM": no `RA` signature at all, and the
    /// `RA` of a cart whose extra RAM is not battery backed. segaretro "ROM
    /// header" lists $A0/$B0/$B8 as the same three widths without "Backup", and
    /// a save nobody can keep is not worth a byte of persistence.
    #[test]
    fn absent_or_unbacked_extra_memory_is_no_sram() {
        assert_eq!(SramInfo::from_extra_memory(&[0u8; 12]), None, "all zeros");
        for raw in [
            // Spaces: what a header with nothing to say is padded with.
            *b"    \x20\x20\x20\x20\x20\x20\x20\x20",
            // Right signature, wrong first letter.
            *b"RX\xF8\x20\x00\x20\x00\x01\x00\x20\xFF\xFF",
            // The three unbacked widths of segaretro's table.
            extra_memory(0xA0, 0x20_0000, 0x20_1FFF),
            extra_memory(0xB0, 0x20_0000, 0x20_1FFF),
            extra_memory(0xB8, 0x20_0001, 0x20_FFFF),
            // A header of 0xFF bytes, which `header::tests` proves parses.
            [0xFFu8; 12],
        ] {
            assert_eq!(SramInfo::from_extra_memory(&raw), None, "{raw:02X?}");
        }
    }

    /// $E8 is EEPROM in segaretro's table: real hardware on the cart bus, but
    /// M3-13 (#73), not SRAM. Its bit 3 is the one bit that separates it from
    /// the odd widths, so the width decode has to leave it alone rather than
    /// read it as a fourth kind of SRAM.
    #[test]
    fn eeprom_is_left_to_m3_13() {
        assert_eq!(
            SramInfo::from_extra_memory(&extra_memory(0xE8, 0x20_0001, 0x20_00FF)),
            None
        );
    }

    /// Storage size is the span of the window and nothing else, and a half-width
    /// window holds half of it: `start + 2n` is storage byte `n`, so 32 KiB of
    /// addresses is 16 KiB of bytes, and the arithmetic is over `end - start`
    /// rather than `end - start + 1` because `start` itself is storage.
    #[test]
    fn storage_is_packed_by_span_and_parity() {
        let cases = [
            // Sonic 3: `52 41 F8 20 00 20 00 01 00 20 03 FF` (segaretro "ROM
            // header"): 1 KiB of odd addresses, so 512 bytes of storage.
            (0xF8u8, 0x20_0001, 0x20_03FF, 512),
            // The 8 KiB odd window Genesis Plus GX gives PGA Tour Golf to.
            (0xF8, 0x20_0001, 0x20_3FFF, 8 * 1024),
            (0xF8, 0x20_0001, 0x20_FFFF, 32 * 1024),
            (0xF0, 0x20_0000, 0x20_FFFF, 32 * 1024),
            (0xE0, 0x20_0000, 0x20_FFFF, 64 * 1024),
            // One byte is a legal, if useless, window.
            (0xF8, 0x20_0001, 0x20_0001, 1),
            (0xE0, 0x20_0001, 0x20_0001, 1),
        ];
        for (kind, start, end, want) in cases {
            let info = SramInfo::from_extra_memory(&extra_memory(kind, start, end))
                .expect("a well-formed window");
            assert_eq!(
                info.storage_len(),
                want,
                "{kind:#04X} {start:#08X}..={end:#08X}"
            );
        }
    }

    /// Windows this bus cannot serve, or would not survive: backwards, outside
    /// the cartridge area at 0x000000-0x3FFFFF (Genesis Plus GX lists a handful
    /// of carts wiring save RAM to 0x400001 and one bootleg to 0x7E0000, which
    /// nothing here decodes), and wider than the 64 KiB the biggest cart save
    /// ships - the cap being what stops a nonsense range becoming an enormous
    /// allocation.
    #[test]
    fn nonsense_ranges_are_refused() {
        for (kind, start, end) in [
            (0xF8, 0x20_0010, 0x20_000F),
            (0xE0, 0x40_0001, 0x40_FFFF),
            (0xF8, 0x7E_0000, 0x7F_FFFF),
            (0xF8, 0x00_0001, 0x3F_FFFF),
            (0xE0, 0x20_0000, 0x3F_FFFF),
            (0xF8, 0x00_0000, 0xFF_FFFF),
        ] {
            assert_eq!(
                SramInfo::from_extra_memory(&extra_memory(kind, start, end)),
                None,
                "{kind:#04X} {start:#08X}..={end:#08X}",
            );
        }
    }

    /// Odd-only SRAM is a half-width window, not a byte array with a stride:
    /// `start + 2n` is storage byte `n`, and a window starts its storage whether
    /// or not the declared start has the parity its name suggests.
    #[test]
    fn odd_window_addresses_are_every_other_byte() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x20_0000);
        assert_eq!(sram.read(0x20_0001), Some(0xFF));
        assert_eq!(sram.read(0x20_0003), Some(0xFF));
        assert_eq!(sram.read(0x20_0005), Some(0xFF));
        // The last address of the window is its last storage byte.
        assert_eq!(sram.read(0x20_FFFF), Some(0xFF));
        // Below the window, above it, and on its dead half, are not the
        // window's: `None` is the image's turn.
        assert_eq!(sram.read(0x20_0000), None);
        assert_eq!(sram.read(0x00_0001), None);
        assert_eq!(sram.read(0x21_0001), None);
        assert_eq!(sram.read(0x20_0002), Some(0xFF), "dead half reads open bus");
        // The dead half is the window's own business, so nothing is stored there
        // and nothing else is asked either.
        assert!(sram.write(0x20_0002, 0x00), "the window owns the dead half");
        assert!(sram.contents().iter().all(|&byte| byte == 0xFF));
    }

    /// A window that is not mapped is not the window's at all: every address in
    /// it belongs to the image, which is what a 4 MB cart does until the program
    /// asks for its save RAM (plutiedev "Games larger than 2MB").
    #[test]
    fn unmapped_window_leaves_the_image_alone() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x40_0000);
        assert!(!sram.mapped());
        for addr in [0x20_0000, 0x20_0001, 0x20_0002, 0x20_FFFF] {
            assert_eq!(sram.read(addr), None, "{addr:#08x}");
            assert!(!sram.write(addr, 0x00), "{addr:#08x}");
        }
    }

    /// A mapped window answers all of itself, including the half a half-width
    /// window does not wire up: with the save RAM mapped over the cartridge area
    /// the image has stopped answering there, so the dead half is nothing
    /// driving the bus rather than the ROM behind it.
    #[test]
    fn mapped_window_answers_both_halves() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x20_0000);
        assert!(sram.mapped(), "the image ends at 0x1FFFFF");
        assert!(sram.write(0x20_0001, 0x42));
        assert_eq!(sram.read(0x20_0001), Some(0x42));
        assert_eq!(sram.read(0x20_0002), Some(0xFF), "dead half");
        assert!(sram.write(0x20_0003, 0x24));
        assert_eq!(sram.contents()[0..2], [0x42, 0x24]);
    }

    /// The 0xA130F1 register: bit 0 maps the window over the cartridge area,
    /// bit 1 write protects it (plutiedev "Saving progress with SRAM" for the
    /// mapping, and Genesis Plus GX's `mapper_sega_w` takes the pair from the
    /// same byte).
    #[test]
    fn control_bit_zero_maps_bit_one_protects() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");

        // A 4 MB image reaches the window, so nothing is mapped until bit 0.
        let mut sram = Sram::new(info, 0x40_0000);
        sram.write_control(0x00);
        assert!(!sram.mapped());
        sram.write_control(0x01);
        assert!(sram.mapped());
        sram.write_control(0x00);
        assert!(!sram.mapped(), "and back again");

        // Bit 1 alone leaves the mapping alone: a small-image cart that only
        // means to lock the save keeps answering reads.
        let mut sram = Sram::new(info, 0x20_0000);
        assert!(
            sram.write(0x20_0001, 0x11),
            "unprotected before the register"
        );
        assert!(sram.take_dirty(), "and that was a change");
        sram.write_control(0x02);
        assert!(sram.mapped(), "and still mapped, the image stops short");
        assert_eq!(sram.read(0x20_0001), Some(0x11), "reads keep working");
        sram.write_control(0x03);
        assert!(
            sram.write(0x20_0001, 0x22),
            "the window still takes the write"
        );
        assert_eq!(sram.read(0x20_0001), Some(0x11), "but nothing is stored");
        assert!(
            !sram.take_dirty(),
            "a refused write did not change the save"
        );
        sram.write_control(0x01);
        assert!(sram.write(0x20_0001, 0x22));
        assert_eq!(sram.read(0x20_0001), Some(0x22));
    }

    /// What SRAM holds before anything writes it is a decision, not a fact: real
    /// SRAM powers up undefined, and plutiedev "Saving progress with SRAM" tells
    /// authors to expect garbage and to keep a magic value and a checksum in the
    /// save to notice. 0xFF is what other emulators fill it with - Genesis Plus
    /// GX's `sram_init` does it "by default ... (Micro Machines 2, Dino Dini
    /// Soccer)" - and it is what nothing driving the data bus reads as here.
    #[test]
    fn sram_powers_up_as_ff() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x20_0000);
        assert_eq!(sram.contents().len(), 32 * 1024);
        assert!(sram.contents().iter().all(|&byte| byte == 0xFF));
        assert!(!sram.take_dirty(), "power-up is not a change to persist");
    }

    /// A restore is the front end handing the save back, so it is not dirty; and
    /// a blob of the wrong size is for another cartridge, so it is refused
    /// entirely rather than half applied. `load_sram` returns nothing, so
    /// refusing is only honest if it leaves the contents exactly as they were.
    #[test]
    fn load_replaces_contents_and_wrong_length_is_ignored() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x20_0000);
        let saved = vec![0x5Au8; sram.contents().len()];
        sram.load(&saved);
        assert!(sram.contents().iter().all(|&byte| byte == 0x5A));
        assert!(!sram.take_dirty(), "a restore is not a change");

        for wrong in [sram.contents().len() - 1, sram.contents().len() + 1, 0, 1] {
            sram.load(&vec![0x00u8; wrong]);
            assert_eq!(sram.contents().len(), 32 * 1024);
            assert!(
                sram.contents().iter().all(|&byte| byte == 0x5A),
                "a {wrong} byte blob changed nothing",
            );
        }
    }

    /// `dirty` is the front end's reason to write anything: set by every write
    /// the save RAM accepts, and by nothing that does not.
    #[test]
    fn dirty_is_set_by_an_accepted_write_only() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x40_0000);
        assert!(!sram.take_dirty());

        // Nothing at all while the window is not mapped.
        sram.write(0x20_0001, 0x11);
        sram.read(0x20_0001);
        sram.write_control(0x01);
        assert!(!sram.take_dirty(), "mapping is not a change to the save");

        // Nothing for the dead half, or for an address outside the window.
        sram.write(0x20_0002, 0x11);
        sram.write(0x10_0001, 0x11);
        assert!(!sram.take_dirty());

        // Yes for a stored byte, even the byte that was already there.
        assert!(sram.write(0x20_0001, 0x11));
        assert!(sram.take_dirty());
        assert!(
            !sram.take_dirty(),
            "and reading it back is not another change"
        );
        assert!(sram.write(0x20_0001, 0x11));
        assert!(sram.take_dirty());

        // No for a write the protect bit refused.
        sram.write_control(0x03);
        sram.write(0x20_0003, 0x22);
        assert!(!sram.take_dirty());
    }

    /// A reset restarts the console, so the cart slot's register goes back to
    /// what it was before the 68k wrote it. The save itself does not: `RESET` is
    /// not a power cycle, and the battery is still holding the bytes.
    #[test]
    fn reset_clears_the_control_register_only() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        let mut sram = Sram::new(info, 0x40_0000);
        sram.write_control(0x01);
        assert!(sram.write(0x20_0001, 0x11));
        sram.write_control(0x03);

        sram.reset();
        assert!(!sram.mapped(), "the map bit is back to 0");
        assert_eq!(
            sram.read(0x20_0001),
            None,
            "so the window is the image's again"
        );
        sram.write_control(0x01);
        assert_eq!(sram.read(0x20_0001), Some(0x11), "the contents survived");
        assert!(sram.take_dirty(), "and so did the fact that they changed");
    }

    /// The window only needs `0xA130F1` when the loaded image reaches it: on a
    /// cart that stops short of the save RAM there is nothing to switch out of
    /// the way. Genesis Plus GX puts it the same way in `md_cart.c`: "SRAM is
    /// mapped by default unless it overlaps with ROM area".
    #[test]
    fn a_window_the_image_reaches_needs_the_control_bit() {
        let info = SramInfo::from_extra_memory(&ODD).expect("odd SRAM");
        assert!(info.mapped_by_default(0x00_0000));
        assert!(
            info.mapped_by_default(0x20_0001),
            "up to and including start"
        );
        assert!(!info.mapped_by_default(0x20_0002));
        assert!(!info.mapped_by_default(0x40_0000));
    }

    /// A ROM image is needed for the whole of detection: too short for a header
    /// at 0x100, or a header that says nothing, is a cart with no save RAM.
    #[test]
    fn detect_needs_a_header_that_says_sram() {
        assert_eq!(detect(&[]), None);
        assert_eq!(detect(&vec![0u8; 0x1FF]), None);

        // Long enough for a header, and the header does not mention save RAM.
        let mut rom = vec![b' '; 0x400];
        rom[0x1B0..0x1BC].copy_from_slice(&[b' '; 12]);
        assert_eq!(detect(&rom), None);
        // Nor does 0xFF, which is what a header of nothing is.
        rom[0x1B0..0x1BC].copy_from_slice(&[0xFF; 12]);
        assert_eq!(detect(&rom), None);
        assert_eq!(detect(&vec![0u8; 0x400]), None);
    }

    /// Detection on a real image: the bytes at 0x1B0, decoded from a full ROM
    /// rather than from the twelve bytes on their own.
    #[test]
    fn detect_reads_the_extra_memory_of_a_rom_image() {
        let mut rom = vec![b' '; 0x400];
        rom[0x1B0..0x1BC].copy_from_slice(&extra_memory(0xF8, 0x20_0001, 0x20_03FF));
        let info = detect(&rom).expect("the image declares SRAM");
        assert_eq!(info.kind, SramKind::Odd);
        assert_eq!(info.start, 0x20_0001);
        assert_eq!(info.storage_len(), 512);
    }

    /// Carts with save RAM the header forgot are found by serial, which is what
    /// [`SRAM_OVERRIDES`] is for; a header that does speak still wins.
    #[test]
    fn detect_falls_back_to_the_serial_table() {
        assert_eq!(sram_override("GM 00000000-00"), None);

        let mut rom = vec![b' '; 0x400];
        rom[0x180..0x18E].copy_from_slice(b"GM 00000000-00");
        assert_eq!(detect(&rom), None, "no header SRAM and no override");
        rom[0x1B0..0x1BC].copy_from_slice(&extra_memory(0xE0, 0x20_0000, 0x20_1FFF));
        assert_eq!(detect(&rom).map(|info| info.kind), Some(SramKind::Word));
    }
}
