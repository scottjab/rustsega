//! The J-CART: the register Codemasters' board answers at $380000-$3FFFFF, and
//! the cart detection that says which carts carry it.
//!
//! The board (SRJCV1-1/SRJCV1-2 with, for the EEPROM carts, a 16R4 PAL that
//! Genesis Plus GX names "CODEMASTERS mapper (SRJCV2-1 & SRJCV2-2 boards)"
//! against its own comment) is two pieces of hardware on one quadrant:
//!
//! * **the register** - the cart's two extra controller ports. A write puts
//!   bit 0 of the byte on the TH line of *both* ports (Genesis Plus GX
//!   `gamepad.c` `jcart_write`, which drives `gamepad_write(5, ...)` and
//!   `gamepad_write(6, ...)` with the same bit; BlastEm `jcart.c` agrees). A
//!   read answers port 5 in the low byte and port 6 in the high byte with bit
//!   14 tied low, which is what unbreaks Micro Machines 2 (GPGX's
//!   `jcart_read`, whose comment says exactly that; BlastEm cites the same
//!   fact: "according to Eke, bit 14 is forced low, at least on the Micro
//!   Machines 2 cart").
//! * **the EEPROM data line**, on bit 7 of the odd byte. That is the register
//!   and [`crate::cart::eeprom`] sharing one byte, and it is why this module
//!   exists next to [`crate::cart::eeprom`] rather than inside it: the line is
//!   a wire on the board, the ports are the PAL's own answer, and GPGX's
//!   `mapper_i2c_jcart_read8` puts them together with
//!   `(eeprom_i2c_out() << 7) | (jcart_read(address) & 0x7f)` - the port's own
//!   bit 7 masked away *because* the wire is there instead of it, which is
//!   what makes the OR lossless. Bits 0-6 and 8-13 belong to the ports, bit 7
//!   to the chip, bit 14 to the PAL.
//!
//! There are no bank registers anywhere on this board, and nothing of it is in
//! the `$A130xx` block (plutiedev "$A130xx area usage" lists that block's
//! owners and has no J-CART in it; GPGX's `default_time_w` routes
//! `$A13000`-$A1305F to the multi-game mapper and above `$A130F1` to the Sega
//! mapper). The local set makes the question moot anyway: every J-CART-family
//! dump in it is 1 MiB, so the quadrant is past image end on all of them.
//!
//! The ports are modelled **unconnected**: the six input pins of an empty DB9
//! are pulled up, so a port reads `$3F`, with TH - the one line the register
//! writes, and the bit a game toggles to clock a pad - in bit 6 of port 5's
//! answer. Genesis Plus GX and BlastEm both model two *connected* pads on the
//! ports (that is what makes four-player Micro Machines work there); wiring
//! real pads to them is a front-end input job this crate has no ports for yet,
//! and until then pull-ups are what the wire does with nothing in it.
//!
//! What is deliberately not modelled: the Easter egg counter (the two-thumbs-
//! up screen a cart shows when a game writes to the quadrant some number of
//! times). No implementing reference models it - GPGX and BlastEm both skip it
//! - and no page documenting it survived to be cited.

use super::header::parse_header;
use serde::{Deserialize, Serialize};

/// The quadrant the register is answered in: `$380000-$3FFFFF`, the top half of
/// the 2 MiB the console's own mapper does not use. GPGX's
/// `mapper_i2c_jcart_init` installs its read/write handlers over exactly this
/// range ("serial EEPROM (read) & J-CART (read/write) mapped to $380000-$3fffff"),
/// and so does its EEPROM-only variant for the cricket carts.
pub const WINDOW: std::ops::Range<u32> = 0x38_0000..0x40_0000;

/// The first longword of the two zero-serial Micro Machines carts: GPGX's
/// `i2c_database` matches them on it ("additional check for Codemasters games",
/// `sp == READ_WORD_LONG(cart.rom, 0)`) because their serial field is
/// literally `00000000`. The bytes spell `DNLD`; what they are the *bytes* of
/// is the boot code's first word pair, and that is why they discriminate: the
/// original Micro Machines (Europe) revisions carry the same checksums with a
/// different boot longword and are not J-CART carts.
const BOOT_MICRO_MACHINES: u32 = 0x444E_4C44;
/// The checksum GPGX matches Micro Machines: Turbo Tournament 96 on
/// (`{"00000000", 0x444e4c44, 0x165E, mapper_i2c_jcart_init, EEPROM_24C16}`).
const CHECKSUM_TT96: u16 = 0x165E;
/// The checksum GPGX matches Micro Machines Military on
/// (`{"00000000", 0x444e4c44, 0x168B, mapper_i2c_jcart_init, EEPROM_24C08}`).
const CHECKSUM_MILITARY: u16 = 0x168B;

/// The serials of the carts GPGX routes through `mapper_i2c_jcart_init` with
/// the register installed, `i2c_database` order. The two cricket carts -
/// `T-120106` and `T-120146` - are deliberately absent: GPGX installs them the
/// EEPROM read window instead, under the comment "check if J-CART is used
/// (support for Brian Lara Cricket games without J-CART)". The two Pete
/// Sampras Tennis serials are on the list with `NO_EEPROM`: a J-CART whose
/// board carries no chip, so bit 7 of the answer is the line released (high).
const SERIAL_PORTS: &[&str] = &["T-120096", "T-120066", "T-123456"];

/// The register's state: the TH line the writes move. It comes up high -
/// Genesis Plus GX's `gamepad_reset` starts every port at `0x40`, "default
/// state", and a reset goes back there ([`Jcart::reset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jcart {
    th: bool,
}

impl Jcart {
    /// The register as a reset leaves it: TH high.
    pub fn new() -> Self {
        Self { th: true }
    }

    /// Is `addr` inside the register's quadrant ([`WINDOW`])?
    pub fn covers(addr: u32) -> bool {
        WINDOW.contains(&addr)
    }

    /// A write of one byte of the quadrant: bit 0 is the TH line of both ports
    /// (Genesis Plus GX's `jcart_write`, BlastEm's `jcart_write_w`). GPGX
    /// accepts the bit from either byte lane - its handler is installed as
    /// `write8` and `write16` alike, "/LWR & /UWR are unused" - so the bus
    /// hands every byte here; BlastEm answers only the odd lane, and a word
    /// write settles the same TH either way because its odd byte is written
    /// last.
    pub fn write(&mut self, value: u8) {
        self.th = value & 1 != 0;
    }

    /// The byte the register answers at `addr`, with `sda` as the cart's EEPROM
    /// data line. The odd lane is port 5 with TH in bit 6 and the line in bit
    /// 7; the even lane is port 6 with bit 14 tied low, so `$3F` whatever TH
    /// does.
    pub fn read_byte(&self, addr: u32, sda: bool) -> u8 {
        let port = 0x3F | (u8::from(self.th) << 6);
        if addr & 1 == 1 {
            port | (u8::from(sda) << 7)
        } else {
            0x3F
        }
    }

    /// Restart the register: TH back high with everything else
    /// ([`gamepad_reset` in GPGX], "default state ... `0x40`"). The quadrant's
    /// answers and the EEPROM's lines are each other's business and neither is
    /// touched here.
    pub fn reset(&mut self) {
        self.th = true;
    }
}

impl Default for Jcart {
    fn default() -> Self {
        Self::new()
    }
}

/// Does `rom` carry the register? The serials first, then the two zero-serial
/// carts by the pair that discriminates them - and by nothing else, because
/// the original Micro Machines (Europe) revisions share their checksums with a
/// different boot longword and are ordinary carts (GPGX's own table matches
/// the `0x165E`/`0xfffffffc` pair as "Pete Sampras Tennis (Prototype ?)",
/// which on this machine's dumps is the original Micro Machines; the row is
/// not copied for exactly that reason).
pub fn detect(rom: &[u8]) -> Option<Jcart> {
    let header = parse_header(rom).ok()?;
    if SERIAL_PORTS
        .iter()
        .any(|serial| header.serial.contains(serial))
    {
        return Some(Jcart::new());
    }
    let boot = u32::from_be_bytes(*rom.first_chunk::<4>()?);
    (boot == BOOT_MICRO_MACHINES
        && (header.checksum == CHECKSUM_TT96 || header.checksum == CHECKSUM_MILITARY))
        .then_some(Jcart::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image with a header that parses: `serial` at 0x180, `checksum` at
    /// 0x18E, and `boot` as the first longword. Nothing else is set, which is
    /// what the real carts look like too - none of the family declares an
    /// extra-memory record.
    fn rom(serial: &str, checksum: u16, boot: u32) -> Vec<u8> {
        let mut rom = vec![0u8; 0x200];
        rom[0x180..0x180 + serial.len()].copy_from_slice(serial.as_bytes());
        rom[0x18E..0x190].copy_from_slice(&checksum.to_be_bytes());
        rom[0..4].copy_from_slice(&boot.to_be_bytes());
        rom
    }

    /// The register's ports are the carts GPGX gives them, by serial; the two
    /// cricket carts are EEPROM-only and get none.
    #[test]
    fn detect_names_the_ports_by_serial() {
        let mm2 = rom("GM T-120096-50", 0x7E50, 0xFFFF_FFFC);
        assert_eq!(detect(&mm2), Some(Jcart::new()), "Micro Machines 2");

        let pete = rom("GM T-120066-00", 0x0000, 0x0104_0000);
        assert_eq!(detect(&pete), Some(Jcart::new()), "Pete Sampras Tennis");

        let pete96 = rom("GM T-123456-00", 0x0000, 0x0104_0000);
        assert_eq!(detect(&pete96), Some(Jcart::new()));

        let lara = rom("GM T-120106-00", 0xA2B8, 0x00FF_FFFC);
        assert_eq!(detect(&lara), None, "Brian Lara Cricket has no ports");

        let lara96 = rom("GM T-120146-50", 0x9A5C, 0x00FF_FFFC);
        assert_eq!(detect(&lara96), None, "nor has Brian Lara Cricket 96");

        let sonic = rom("GM MK-1215-00", 0x0000, 0x0100_0000);
        assert_eq!(detect(&sonic), None, "and no other cart either");
    }

    /// The two zero-serial carts, on the checksum and the boot longword
    /// together - and the original Micro Machines revisions, which carry the
    /// same checksums, stay ordinary carts because their boot longword is
    /// different.
    #[test]
    fn detect_names_the_zero_serial_machines_by_checksum_and_boot() {
        let tt96 = rom("GM 00000000-00", CHECKSUM_TT96, BOOT_MICRO_MACHINES);
        assert_eq!(detect(&tt96), Some(Jcart::new()));
        let military = rom("GM 00000000-00", CHECKSUM_MILITARY, BOOT_MICRO_MACHINES);
        assert_eq!(detect(&military), Some(Jcart::new()));

        // The collision, exactly as the local set carries it: same serial and
        // checksum as Turbo Tournament 96, boot longword of an ordinary cart.
        let mm1 = rom("GM 00000000-00", CHECKSUM_TT96, 0xFFFF_FFFC);
        assert_eq!(detect(&mm1), None);
        let mm1b = rom("GM 00000000-00", 0x31FC, 0xFFFF_FFFC);
        assert_eq!(detect(&mm1b), None, "Micro Machines (USA)");
    }

    /// An unconnected port reads its pull-ups with TH in bit 6 of port 5's
    /// answer and bit 14 of port 6's tied low: the register as it comes up
    /// (TH high), and with the line released or pulled, and as a word.
    #[test]
    fn the_register_reads_pull_ups_th_and_the_line() {
        let jcart = Jcart::new();

        // TH high, line high: port 5's byte is $FF, port 6's $3F, word $3FFF.
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xFF);
        assert_eq!(jcart.read_byte(0x38_0000, true), 0x3F);
        assert_eq!(jcart.read_byte(0x3F_FFFF, true), 0xFF, "the whole quadrant");
        let word = (u16::from(jcart.read_byte(0x38_0000, true)) << 8)
            | u16::from(jcart.read_byte(0x38_0001, true));
        assert_eq!(word, 0x3FFF);

        // The line, pulled low by the chip, is bit 7 and nothing else.
        assert_eq!(jcart.read_byte(0x38_0001, false), 0x7F);
        assert_eq!(jcart.read_byte(0x38_0000, false), 0x3F, "bit 14 stays low");
    }

    /// A register write moves TH and nothing else: bit 6 follows bit 0 of the
    /// byte written, the even lane keeps its `$3F`, and the line bit is
    /// untouched (it is not this register's).
    #[test]
    fn a_register_write_moves_th_and_nothing_else() {
        let mut jcart = Jcart::new();
        jcart.write(0x00);
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xBF, "TH down, line up");
        assert_eq!(jcart.read_byte(0x38_0000, true), 0x3F);

        // Bit 0 of the byte, either lane's, is the line: BlastEm answers the
        // odd lane only, GPGX either; both land here.
        jcart.write(0x01);
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xFF);
        jcart.write(0xFE);
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xBF, "bit 1 is not TH");

        // The line bit rides through every write unchanged: it is `sda`'s
        // argument, and the register has no say in it.
        assert_eq!(jcart.read_byte(0x38_0001, false), 0x3F, "TH down, line low");
    }

    /// A reset puts TH back high, however the writes left it.
    #[test]
    fn a_reset_pulls_th_high_again() {
        let mut jcart = Jcart::new();
        jcart.write(0x00);
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xBF);
        jcart.reset();
        assert_eq!(jcart.read_byte(0x38_0001, true), 0xFF);
    }
}
