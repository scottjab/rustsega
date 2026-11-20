//! The SVP: the 68k-visible half of the Sega Virtual Processor, the board the
//! *Virtua Racing* cartridge carries.
//!
//! The SVP is a Fujitsu SSP1601 DSP sitting beside the game ROM on one cart,
//! with its own program ROM (a slice of the game image), 2 KiB of IRAM, 128 KiB
//! of DRAM shared with the 68000, and a handful of status/control registers
//! (notaz *SVP doc*, section 1 "Overview" and section 3 "Memory map"). The 68k
//! sees three windows of it:
//!
//! | 68k byte address | what it is | who answers it |
//! |---|---|---|
//! | `0x000000`-`0x0007FF` | SVP program IRAM, aliasing the game image | [`crate::bus`]'s `Region::Rom` (the cart area, already right) |
//! | `0x300000`-`0x31FFFF` | SVP DRAM, 16-bit, shared with the 68k | [`Svp::read_dram_byte`] and friends |
//! | `0xA15000`-`0xA15009` | SVP status/control registers | [`Svp::read_register_byte`] and friends |
//!
//! The IRAM window needs no board at all - it is the image read at its own
//! offsets - which is why this module is only the two windows the image cannot
//! answer. The DSP itself is not modelled yet (#433 stage 2): the registers
//! answer with the DSP-side latches of [`Svp`] and nothing ever drives them
//! from the other side, which is the "do-nothing DSP" of #433 stage 1.
//!
//! ## Which carts carry the board
//!
//! Nothing in a cart header says "I have an SVP", and the SVP shipped in
//! exactly one game: notaz's overview opens with "The only game released with
//! SVP chip was Virtua Racing". The detection is Picodrive's (notaz's own
//! emulator, `pico/carthw.cfg`, `[Virtua Racing - SVP]` sections), which keys
//! the board on the domestic title at 0x150 containing "Virtua Racing" and on
//! the bytes `OHMP` at 0x810, a string sitting inside the DSP program blob the
//! cart carries at 0x800. Both halves are image content, not a table of image
//! hashes: [`docs/mappers.md`](../../../../docs/mappers.md)'s principle that
//! detection keys on what the image itself cannot lie about, with the probe
//! doing the work a hash table would do, once, for all four regional revisions
//! (all four local images carry the probe; ordinary carts do not).

use super::header::parse_header;
use serde::{Deserialize, Serialize};

mod ssp16;

/// The DRAM window, in 68k byte addresses: `0x300000`-`0x31FFFF`, 128 KiB read
/// as 16-bit words (notaz *SVP doc*, section 1: "128KB of DRAM"; section 3's
/// memory-map table; Genesis Plus GX's `svp.h` carries it as `dram[0x20000]`).
///
/// The window is inside the cartridge area, so [`crate::bus`] decodes it as
/// `Region::Rom` and the image would answer it; [`Svp::covers_dram`] is the
/// overlay test that runs first.
pub const DRAM_WINDOW: std::ops::Range<u32> = 0x30_0000..0x32_0000;

/// The status/control register page, in 68k byte addresses.
///
/// notaz *SVP doc* section 3 lists the registers at `0xA15000`-`0xA15009`;
/// Genesis Plus GX routes the whole 64 KiB page `0xA15000`-`0xA15FFF` to its
/// SVP handler (`case 0x50` of `mem68k.c`, on `(address >> 8) & 0xFF`) and
/// answers only the lanes documented below, so this window is the page and the
/// per-lane decode is [`Svp`]'s to do.
pub const REGISTER_WINDOW: std::ops::RangeInclusive<u32> = 0xA1_5000..=0xA1_5FFF;

/// Words of DRAM: 128 KiB of 16-bit memory.
const DRAM_WORDS: usize = 0x1_0000;
/// Mask that names the word of [`DRAM_WINDOW`] an address lands on: the window
/// is 64K words and the low address bits above the byte lane select it.
const DRAM_WORD_MASK: u32 = DRAM_WORDS as u32 - 1;

/// The bytes Picodrive probes at [`PROGRAM_MAGIC_ADDR`] to know the DSP
/// program blob is there.
const PROGRAM_MAGIC: &[u8; 4] = b"OHMP";
/// Where Picodrive probes it: 16 bytes into the DSP program ROM, which notaz
/// section 3 maps from byte 0x800 of the game image.
const PROGRAM_MAGIC_ADDR: usize = 0x810;
/// The overseas title of the one SVP game, matched the way Picodrive's
/// `check_str` does - as a substring, so the padded title field of any of the
/// four regional revisions matches. 0x150, where Picodrive probes it, is the
/// overseas field: [`crate::cart::RomHeader::overseas_title`].
const TITLE: &str = "Virtua Racing";

/// The 68k-visible state of the board: the shared DRAM and the two latches the
/// status/control registers answer.
///
/// `xst` and `pm0` are the halves of the 68k/SSP handshake that live on this
/// side of the bus (notaz *SVP doc* 2.2.4 "XST" and 2.2.1 "PM0"): the 68k
/// writes its command into XST and polls PM0 for the answer bit. Both come up
/// zeroed with the board (Genesis Plus GX's `svp_init` memsets `svp`, and
/// neither its `svp_reset` nor `ssp1601_reset` touches them again).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Svp {
    /// The shared DRAM, 64K words. It is RAM, not chip state: a soft reset
    /// leaves it alone the way [`crate::cart::sram`] is left alone, and the
    /// reference's `svp_reset` clears only the DSP's program mapping and core
    /// state, which is #433 stage 2's business.
    #[serde(with = "crate::big_array")]
    dram: Box<[u16; DRAM_WORDS]>,
    /// XST: the command/result register the 68k sees at `0xA15000` and its
    /// `0xA15002` mirror (notaz *SVP doc* section 3, register list: "a15000 -
    /// w/r command/result register", "a15002 - mirror of the above").
    xst: u16,
    /// PM0: the handshake status (notaz *SVP doc* 2.2.1). Bit 0 is set when
    /// the SSP has written to XST, and clears when the 68k reads it; bit 1 is
    /// set when the 68k writes XST, and clears when the SSP reads PM0. With
    /// no DSP on the other side, bit 1 never clears - and bit 0 is set by
    /// [`Svp::write_register_word`]'s fake answer.
    pm0: u16,
}

/// What the game's driver writes into XST as a query, and what the board
/// answers: the driver's `move.l #$00004F48, $A15000` puts the ASCII pair
/// `OH` into the command register, and the DSP firmware - which spells
/// `OHMP` at byte 0x810 of itself, the same bytes Picodrive's detection
/// probes - answers the other half of the word. The reply is a measured
/// inference, not a documented fact: see [`Svp::write_register_word`] for
/// what it buys and what it was measured against.
const HANDSHAKE_QUERY: u16 = 0x4F48; // "OH"
const HANDSHAKE_REPLY: u16 = 0x4D50; // "MP"

impl Svp {
    /// The board of the cart `rom`, if the image names itself an SVP cart.
    ///
    /// Both halves of the probe are needed, and for the same reason
    /// [`crate::cart::Mapper::is_realtec`] wants both of its halves: `OHMP` is
    /// four bytes at one address and "Virtua Racing" is a title field, and
    /// either alone could be satisfied by an image that is not this board.
    /// Together they say the cart carries a DSP program blob under a header
    /// naming the one SVP game.
    pub fn detect(rom: &[u8]) -> Option<Self> {
        let title_matches =
            parse_header(rom).is_ok_and(|header| header.overseas_title.contains(TITLE));
        let program_matches = rom
            .get(PROGRAM_MAGIC_ADDR..)
            .is_some_and(|tail| tail.starts_with(PROGRAM_MAGIC));
        (title_matches && program_matches).then(Self::new)
    }

    /// The board as it powers up: empty DRAM, both handshake latches clear.
    pub fn new() -> Self {
        Self {
            dram: Box::new([0; DRAM_WORDS]),
            xst: 0,
            pm0: 0,
        }
    }

    /// Is `addr` inside the DRAM window ([`DRAM_WINDOW`])?
    pub fn covers_dram(addr: u32) -> bool {
        DRAM_WINDOW.contains(&addr)
    }

    /// Is `addr` inside the register page ([`REGISTER_WINDOW`])?
    pub fn covers_registers(addr: u32) -> bool {
        REGISTER_WINDOW.contains(&addr)
    }

    /// The index of the DRAM word `addr` names. The window is 64K words and
    /// the low bit is the byte lane, so `addr >> 1` masked to the window is
    /// the word - the same decode Genesis Plus GX's `svp_write_dram` makes
    /// with `address & 0x1fffe`.
    fn dram_word(addr: u32) -> usize {
        ((addr >> 1) & DRAM_WORD_MASK) as usize
    }

    /// DRAM byte read at `addr`: the high lane of the word on even addresses,
    /// the low lane on odd ones. The memory is 16-bit and the 68k's lanes are
    /// big-endian, as everywhere else on this bus.
    pub fn read_dram_byte(&self, addr: u32) -> u8 {
        let word = self.dram[Self::dram_word(addr)];
        if addr & 1 == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }

    /// DRAM byte write at `addr`: the named lane of the word, leaving the
    /// other lane alone.
    pub fn write_dram_byte(&mut self, addr: u32, value: u8) {
        let index = Self::dram_word(addr);
        let word = self.dram[index];
        self.dram[index] = if addr & 1 == 0 {
            (u16::from(value) << 8) | (word & 0xFF)
        } else {
            (word & 0xFF00) | u16::from(value)
        };
    }

    /// DRAM word read at `addr`.
    pub fn read_dram_word(&self, addr: u32) -> u16 {
        self.dram[Self::dram_word(addr)]
    }

    /// DRAM word write at `addr`.
    pub fn write_dram_word(&mut self, addr: u32, value: u16) {
        self.dram[Self::dram_word(addr)] = value;
    }

    /// The lanes of the register page that answer XST, the command/result
    /// register and its mirror: `0xA15000`-`0xA15003` (notaz *SVP doc* section
    /// 3, register list). Genesis Plus GX decodes the same registers with
    /// `(address & 0xFC) == 0`, which accepts the first pair of lanes and not
    /// the mirror; the document's "a15002 - mirror of the above" is the
    /// behaviour we follow where the two disagree.
    fn is_xst(addr: u32) -> bool {
        addr & 0xFC == 0
    }

    /// The lanes of the register page that answer PM0, the handshake status:
    /// `0xA15004`-`0xA15005`, Genesis Plus GX's `(address & 0xFE) == 4`. The
    /// document's next register, `0xA15006` ("possibly halts the SVP"), is not
    /// decoded by the reference either - its reads fall through to open bus
    /// and its writes to `m68k_unused_16_w` - and no working reference needs
    /// it, so it stays undecoded here like `0xA15008` ("possibly causes an
    /// interrupt").
    fn is_pm0(addr: u32) -> bool {
        addr & 0xFE == 4
    }

    /// Register page byte read at `addr`. The one side effect on the bus side
    /// of the handshake: reading PM0 clears its "SSP has answered" bit (notaz
    /// *SVP doc* 2.2.1: "cleared when 0xA15004 is read by 68k"). The value a
    /// poll sees is [`Svp::write_register_word`]'s fake answer; what the game
    /// makes of the answer itself is the stage-1 measurement, and the real
    /// DSP of stage 2 is what replaces the fake.
    pub fn read_register_byte(&mut self, addr: u32) -> u8 {
        if Self::is_xst(addr) {
            return Self::lane(addr, self.xst);
        }
        if Self::is_pm0(addr) {
            let pm0 = self.pm0;
            self.pm0 &= !1;
            return Self::lane(addr, pm0);
        }
        crate::bus::OPEN_BUS
    }

    /// Register page word read at `addr`, with [`Svp::read_register_byte`]'s
    /// PM0 side effect.
    pub fn read_register_word(&mut self, addr: u32) -> u16 {
        if Self::is_xst(addr) {
            return self.xst;
        }
        if Self::is_pm0(addr) {
            let pm0 = self.pm0;
            self.pm0 &= !1;
            return pm0;
        }
        crate::bus::OPEN_BUS_WORD
    }

    /// Register page byte write at `addr`. Nothing on the board answers a
    /// byte: the registers are 16-bit and the game drives them with word
    /// writes, and Genesis Plus GX drops every byte write to the page
    /// (`m68k_unused_8_w` on its `case 0x50` write path).
    pub fn write_register_byte(&mut self, _addr: u32, _value: u8) {}

    /// Register page word write at `addr`. An XST lane takes the command.
    /// Writes set PM0 bit 1, the documented "68k has written" (notaz *SVP
    /// doc* 2.2.1); bit 0 is the answer bit, set here by the same stroke -
    /// and the answer itself is faked, because there is no DSP here yet
    /// (#433 stage 2). Two fakes, both behind measurements on
    /// `Virtua Racing (USA)`:
    ///
    /// * every command is answered the moment it is written, and `OH` is
    ///   answered with `MP` ([`HANDSHAKE_QUERY`]/[`HANDSHAKE_REPLY`]). With
    ///   neither, the game's boot self-test - send `SV`, poll PM0 bit 0,
    ///   expect `OK` (the routine at 0x1C072 of the image) - reboots the
    ///   machine into a black screen, and with the echo alone the attract
    ///   loop parks waiting for XST to move off `OH`, which the open bus of
    ///   `main` (a `FFFF` read) never blocked. Genesis Plus GX shipped the
    ///   same idea as its 2008 "SVP faking: display 2D graphics in Virtua
    ///   Racing" release; the full DSP of stage 2 is what replaces both.
    /// * a write of `OH` answers `MP` rather than echoing, for the same
    ///   reason at the next handshake out.
    ///
    /// Everything else in the page is dropped, like the reference's
    /// `m68k_unused_16_w`.
    pub fn write_register_word(&mut self, addr: u32, value: u16) {
        if Self::is_xst(addr) {
            self.xst = if value == HANDSHAKE_QUERY {
                HANDSHAKE_REPLY
            } else {
                value
            };
            self.pm0 |= 3;
        }
    }

    /// The byte of `word` the lane of `addr` answers: high lane on even
    /// addresses, low lane on odd ones.
    fn lane(addr: u32, word: u16) -> u8 {
        if addr & 1 == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }
}

impl Default for Svp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic image with the domestic title and the DSP program probe,
    /// shaped like the real carts: a 2 MiB image whose header names Virtua
    /// Racing and whose 0x810 says `OHMP`.
    fn svp_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x200_000];
        // 0x150 is the overseas title field, the one Picodrive's check_str
        // probes and [`Svp::detect`] matches through.
        rom[0x150..0x150 + TITLE.len()].copy_from_slice(TITLE.as_bytes());
        rom[PROGRAM_MAGIC_ADDR..PROGRAM_MAGIC_ADDR + PROGRAM_MAGIC.len()]
            .copy_from_slice(PROGRAM_MAGIC);
        rom
    }

    /// A cart carrying neither half of the probe is not an SVP cart: the
    /// title without the program blob, the blob without the title, a cart
    /// whose title merely contains the words, and no header at all.
    #[test]
    fn detect_needs_the_title_and_the_program_blob() {
        assert_eq!(Svp::detect(&svp_rom()), Some(Svp::new()), "both halves");

        let mut title_only = svp_rom();
        title_only[PROGRAM_MAGIC_ADDR..PROGRAM_MAGIC_ADDR + 4].copy_from_slice(b"XXXX");
        assert_eq!(Svp::detect(&title_only), None, "title, no blob");

        let mut blob_only = svp_rom();
        blob_only[0x150..0x150 + TITLE.len()].fill(0);
        assert_eq!(Svp::detect(&blob_only), None, "blob, no title");

        let mut short = svp_rom();
        short.truncate(0x1FF);
        assert_eq!(Svp::detect(&short), None, "no header to read at 0x100");

        // But a truncated dump whose probes both survive still carries the
        // board, and still gets it: the detection asks what the image has,
        // not how long it is.
        let mut cut = svp_rom();
        cut.truncate(0x100_000);
        assert_eq!(
            Svp::detect(&cut),
            Some(Svp::new()),
            "both probes, short image"
        );
    }

    /// The detection does not fire on ordinary carts: a Sonic-2-shaped image
    /// (different title, program bytes at 0x810) and a mapper-shaped one
    /// (`SEGA SSF`, the way a cart asks for #72's bank registers) stay
    /// board-less.
    #[test]
    fn detect_stays_off_ordinary_carts() {
        let mut sonic = vec![0u8; 0x100_000];
        sonic[0x150..0x162].copy_from_slice(b"SONIC THE HEDGEHOG");
        sonic[0x810..0x814].copy_from_slice(&[0x11, 0x00, 0x66, 0xF6]);
        assert_eq!(Svp::detect(&sonic), None);

        let mut ssf2 = vec![0u8; 0x50_0000];
        ssf2[0x150..0x15D].copy_from_slice(b"Virtua Racing");
        ssf2[0x100..0x108].copy_from_slice(b"SEGA SSF");
        assert_eq!(Svp::detect(&ssf2), None, "a mapper cart is not this board");
    }

    /// DRAM holds what the 68k puts in it, at either width, across the whole
    /// window: a word write reads back as a word and as its two byte lanes,
    /// and a byte write changes only its own lane. 0x31FFFF is the last byte
    /// of the window and decodes onto the last word of the array.
    #[test]
    fn dram_write_then_read_back() {
        let mut svp = Svp::new();

        svp.write_dram_word(0x30_0000, 0x1234);
        assert_eq!(svp.read_dram_word(0x30_0000), 0x1234);
        assert_eq!(svp.read_dram_byte(0x30_0000), 0x12, "high lane");
        assert_eq!(svp.read_dram_byte(0x30_0001), 0x34, "low lane");

        svp.write_dram_byte(0x30_0001, 0xAB);
        assert_eq!(svp.read_dram_word(0x30_0000), 0x12AB, "low lane only");

        svp.write_dram_word(0x31_FFFE, 0xCAFE);
        assert_eq!(svp.read_dram_word(0x31_FFFE), 0xCAFE, "the last word");
        assert_eq!(svp.read_dram_byte(0x31_FFFF), 0xFE, "the last byte");

        svp.write_dram_word(0x30_0FF0, 0x0001);
        assert_eq!(svp.read_dram_word(0x30_0FF0), 0x0001, "mid-window");
    }

    /// XST reads answer at `0xA15000` and its `0xA15002` mirror, both widths,
    /// big-endian lanes at byte width; before anything is written the command
    /// register reads clear.
    #[test]
    fn xst_reads_answer_at_the_register_and_its_mirror() {
        let mut svp = Svp::new();
        assert_eq!(svp.read_register_word(0xA1_5000), 0x0000, "clear at reset");

        svp.write_register_word(0xA1_5000, 0xBEEF);
        assert_eq!(svp.read_register_word(0xA1_5000), 0xBEEF);
        assert_eq!(svp.read_register_word(0xA1_5002), 0xBEEF, "the mirror");
        assert_eq!(svp.read_register_byte(0xA1_5000), 0xBE);
        assert_eq!(svp.read_register_byte(0xA1_5001), 0xEF);
        assert_eq!(svp.read_register_byte(0xA1_5002), 0xBE, "mirror lanes");
        assert_eq!(svp.read_register_byte(0xA1_5003), 0xEF);

        svp.write_register_word(0xA1_5002, 0x0042);
        assert_eq!(svp.read_register_word(0xA1_5000), 0x0042, "mirror write");
    }

    /// PM0: a write to XST sets both handshake bits - bit 1 is the documented
    /// "68k has written", bit 0 is the fake DSP answer of the stage-1 board
    /// (see [`Svp::write_register_word`] for why it is faked). The read hands
    /// both back and clears the answer bit on the way out, the one side
    /// effect the 68k side has.
    #[test]
    fn pm0_reads_the_handshake_and_clears_the_answer_bit() {
        let mut svp = Svp::new();
        assert_eq!(svp.read_register_word(0xA1_5004), 0x0000);

        svp.write_register_word(0xA1_5000, 0x0001);
        assert_eq!(svp.read_register_word(0xA1_5004), 0x0003);
        // Every read of PM0 clears the answer bit, at either width; bit 1
        // stays until the DSP side reads it, which never happens here.
        assert_eq!(svp.read_register_byte(0xA1_5004), 0x00, "high lane");
        assert_eq!(svp.read_register_byte(0xA1_5005), 0x02, "low lane, bit 1");

        // The next command sets it again: the handshake cycles.
        svp.write_register_word(0xA1_5002, 0xFFFF);
        assert_eq!(svp.read_register_word(0xA1_5004), 0x0003);
    }

    /// The undecoded lanes of the page: `0xA15006` and `0xA15008` (the two
    /// registers the sources name but no working reference decodes) and the
    /// rest of the page read open bus and drop writes, at both widths.
    #[test]
    fn undecoded_lanes_read_open_bus_and_drop_writes() {
        let mut svp = Svp::new();
        for addr in [0xA1_5006, 0xA1_5008, 0xA1_5009, 0xA1_500A, 0xA1_5FFE] {
            assert_eq!(
                svp.read_register_byte(addr),
                crate::bus::OPEN_BUS,
                "byte at {addr:#08x}"
            );
            assert_eq!(
                svp.read_register_word(addr & !1),
                crate::bus::OPEN_BUS_WORD,
                "word at {addr:#08x}"
            );
            svp.write_register_word(addr & !1, 0x2222);
            svp.write_register_byte(addr, 0x11);
        }
        // None of it reached the latches.
        assert_eq!(svp.read_register_word(0xA1_5000), 0x0000);
        assert_eq!(svp.read_register_word(0xA1_5004), 0x0000);
    }

    /// Byte writes to the page are dropped whole: the registers are 16-bit
    /// and the reference drops every byte write to the page.
    #[test]
    fn byte_writes_are_dropped() {
        let mut svp = Svp::new();
        svp.write_register_byte(0xA1_5000, 0xFF);
        svp.write_register_byte(0xA1_5001, 0xFF);
        svp.write_register_byte(0xA1_5002, 0xFF);
        svp.write_register_byte(0xA1_5003, 0xFF);
        assert_eq!(svp.read_register_word(0xA1_5000), 0x0000);
        assert_eq!(svp.read_register_word(0xA1_5004), 0x0000, "no bit 1 either");
    }
}
