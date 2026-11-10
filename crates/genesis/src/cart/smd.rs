//! SMD: the interleaved dump format of the Super Magic Drive copier.
//!
//! An SMD file is a 512-byte copier header, then the ROM in 16 KiB blocks stored
//! as its two byte lanes. Nothing can be done with such a file - not even
//! [`parse_header`](super::parse_header) - until it is back in address order, so
//! this conversion runs before anything else looks at the ROM.
//!
//! Reference: `smdtech.txt` ("Super Magic Drive hardware information", Charles
//! MacDonald, section "File formats"), which segaretro.org's "SMD" page quotes;
//! it is the closest thing the format has to a specification. It documents the
//! 512-byte header, the file size in units of 16 KiB, and the interleave - but
//! see [`EvenHalf`] for why the last of those is read from the data rather than
//! assumed.
//!
//! Header bytes 8 and 9 are often `0xAA 0xBB`, and it is deliberately not a
//! check: `smdtech.txt` records it as "for identification purposes only, the BIOS
//! does not seem to check these values". It is an identifier the dumping tool
//! writes, not part of the layout, so it can neither prove SMD nor rule it out.

/// Bytes of copier header in front of the ROM. Dropped.
const SMD_HEADER: usize = 0x200;
/// One interleaved block: the even and odd bytes of 16 KiB of ROM.
const SMD_BLOCK: usize = 0x4000;
/// One byte lane of a block, so either the evens or the odds.
const SMD_HALF: usize = SMD_BLOCK / 2;
/// Where the system type sits, which is all detection has to look at.
const SYSTEM_TYPE_AT: usize = 0x100;
/// The system type every licensed ROM starts with.
const SYSTEM_TYPE: &[u8] = b"SEGA";
/// The same field with its one-byte leading space, which a few dumps carry - 8 of
/// the local 2775 - and which must read as a header rather than as no header.
const PADDED_SYSTEM_TYPE: &[u8] = b" SEGA";

/// Which half of a block holds the address-even bytes.
///
/// Two conventions are in circulation, and they disagree about this:
///
/// - `Lower`: `smdtech.txt` ("the first 8K being the even bytes and the
///   remaining 8K being the odd bytes of a 16K block"), repeated by segaretro.org
///   "SMD", and coded that way by PicoDrive (`DecodeSmd`).
/// - `Upper`: coded that way by BizHawk (`RomGame.DeInterleaveSMD`), zrax and
///   EutherDrive, and by the ticket this module came from.
///
/// Nothing downstream can tell which was used: a reversed deinterleave yields an
/// image of exactly the right length, with every word byte-swapped, so the ROM
/// simply fails to boot. So the layout is read from the file rather than
/// assumed - see [`convention_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvenHalf {
    /// Even bytes in the lower half: the layout `smdtech.txt` documents.
    Lower,
    /// Even bytes in the upper half.
    Upper,
}

/// Every convention, in the order [`is_smd`] should try them.
const CONVENTIONS: [EvenHalf; 2] = [EvenHalf::Lower, EvenHalf::Upper];

/// Is `data` an SMD image rather than a plain binary?
///
/// Content only, never the file name or extension: the owner's whole collection
/// is `.md`, plain and interleaved alike. The length rule - a 512-byte header
/// then whole 16 KiB blocks - is the cheap filter, and the system type decides,
/// because a plain ROM can satisfy the length by accident.
pub fn is_smd(data: &[u8]) -> bool {
    convention_of(data).is_some()
}

/// The ROM inside an SMD image, in address order.
///
/// Assumes [`is_smd`] held, and uses the convention it would have picked;
/// [`normalize_rom`] is the entry point that pairs the two. A trailing partial
/// block cannot be deinterleaved - the layout says nothing about it - so it is
/// copied through rather than dropped.
pub fn deinterleave(data: &[u8]) -> Vec<u8> {
    let Some(body) = data.get(SMD_HEADER..) else {
        return Vec::new();
    };
    let convention = convention_of(data).unwrap_or(EvenHalf::Lower);
    let (blocks, tail) = body.as_chunks::<SMD_BLOCK>();
    let mut rom = Vec::with_capacity(body.len());
    for block in blocks {
        rom.extend(block_in_address_order(block, convention));
    }
    rom.extend(tail);
    rom
}

/// The ROM image to work with: `data` unchanged if it is already plain, and the
/// SMD contents deinterleaved if it is not.
///
/// This is the whole of what a loader has to do before [`parse_header`](super::parse_header),
/// and it strips nothing else: `.mgd` and the other copier formats are separate
/// questions.
pub fn normalize_rom(data: &[u8]) -> Vec<u8> {
    if is_smd(data) {
        deinterleave(data)
    } else {
        data.to_vec()
    }
}

/// The convention `data` is interleaved with, or `None` if it is not SMD.
///
/// The system type decides: it is the one field a Genesis ROM is obliged to
/// carry, and it is inside the first block, so the wrong convention turns it into
/// `ESAG` rather than into something that still looks valid. That makes it a
/// reliable discriminator between the two entries of [`EvenHalf`], which is what
/// lets one loader read dumps from either family of tools.
fn convention_of(data: &[u8]) -> Option<EvenHalf> {
    // Header size plus whole blocks, which is the only shape an SMD can have.
    if data.len() % SMD_BLOCK != SMD_HEADER {
        return None;
    }
    // A plain ROM that lands on that length by accident still says `SEGA` where
    // it should, and that is what rules it out.
    if has_sega_at_100(data) {
        return None;
    }
    let block = data.get(SMD_HEADER..SMD_HEADER + SMD_BLOCK)?;
    CONVENTIONS
        .into_iter()
        .find(|&convention| has_sega_at_100(&block_in_address_order(block, convention)))
}

/// The block's bytes as a slice in address order, which is what the 68000 sees.
fn block_in_address_order(block: &[u8], convention: EvenHalf) -> Vec<u8> {
    let (even, odd) = halves(block, convention);
    let mut rom = Vec::with_capacity(block.len());
    for (&even, &odd) in even.iter().zip(odd) {
        rom.push(even);
        rom.push(odd);
    }
    rom
}

/// The two byte lanes of a block, as `(even, odd)`.
///
/// The one place the two conventions differ, which is why nothing else is
/// allowed to match on [`EvenHalf`] directly.
fn halves(block: &[u8], convention: EvenHalf) -> (&[u8], &[u8]) {
    let (lower, upper) = block.split_at(SMD_HALF);
    match convention {
        EvenHalf::Lower => (lower, upper),
        EvenHalf::Upper => (upper, lower),
    }
}

/// Does a system type start at [`SYSTEM_TYPE_AT`]?
fn has_sega_at_100(rom: &[u8]) -> bool {
    rom.get(SYSTEM_TYPE_AT..)
        .is_some_and(|at| at.starts_with(SYSTEM_TYPE) || at.starts_with(PADDED_SYSTEM_TYPE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain ROM of `size` bytes with a valid header: a header in address
    /// order at 0x100, and a byte pattern that changes every byte so a
    /// deinterleave that permutes anything is caught.
    fn fake_rom(size: usize) -> Vec<u8> {
        assert!(size >= 0x200, "a fake ROM needs room for its header");
        let mut rom = vec![0u8; size];
        for (offset, byte) in rom.iter_mut().enumerate() {
            *byte = ((offset * 7 + 11) % 251) as u8;
        }
        rom[SYSTEM_TYPE_AT..SYSTEM_TYPE_AT + 5].copy_from_slice(b"1991.");
        rom[SYSTEM_TYPE_AT..SYSTEM_TYPE_AT + SYSTEM_TYPE.len()].copy_from_slice(SYSTEM_TYPE);
        rom
    }

    /// A plain ROM of exactly one SMD block: what an SMD file holds, so
    /// [`interleave`] can round trip it without dropping a tail.
    fn fake_plain() -> Vec<u8> {
        fake_rom(SMD_BLOCK)
    }

    /// A plain ROM whose length satisfies `len % 16384 == 512` by accident: the
    /// shape the length rule alone cannot tell apart from SMD.
    fn fake_plain_at_smd_length() -> Vec<u8> {
        fake_rom(SMD_HEADER + SMD_BLOCK)
    }

    /// The SMD file for `plain`: the 512-byte header, then each block of `plain`
    /// written as its two byte lanes, in the order the convention stores them.
    /// The exact inverse of [`deinterleave`], written independently of it.
    fn interleave(plain: &[u8], convention: EvenHalf) -> Vec<u8> {
        let mut smd = vec![0u8; SMD_HEADER];
        let (blocks, tail) = plain.as_chunks::<SMD_BLOCK>();
        assert!(tail.is_empty(), "interleave drops a partial block");
        for block in blocks {
            let (even, odd) = byte_lanes(block);
            let (lower, upper) = match convention {
                EvenHalf::Lower => (even, odd),
                EvenHalf::Upper => (odd, even),
            };
            smd.extend(lower);
            smd.extend(upper);
        }
        smd
    }

    /// The two byte lanes of a block in address order: `even[i]` is
    /// `block[i * 2]`, which is the byte a 16-bit read puts on the upper half of
    /// the bus.
    fn byte_lanes(block: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut even = Vec::with_capacity(block.len() / 2);
        let mut odd = Vec::with_capacity(block.len() / 2);
        let (pairs, _) = block.as_chunks::<2>();
        for &[even_byte, odd_byte] in pairs {
            even.push(even_byte);
            odd.push(odd_byte);
        }
        (even, odd)
    }

    #[test]
    fn interleave_then_deinterleave_roundtrip() {
        let plain = fake_rom(2 * SMD_BLOCK);
        let interleaved = interleave(&plain, EvenHalf::Lower);
        assert_eq!(interleaved.len(), SMD_HEADER + plain.len());
        assert_eq!(deinterleave(&interleaved), plain);
    }

    #[test]
    fn interleave_then_deinterleave_roundtrip_upper_half_even() {
        let plain = fake_rom(2 * SMD_BLOCK);
        let interleaved = interleave(&plain, EvenHalf::Upper);
        assert_eq!(deinterleave(&interleaved), plain);
    }

    #[test]
    fn detects_smd() {
        let plain = fake_plain();
        assert!(is_smd(&interleave(&plain, EvenHalf::Lower)));
        assert!(!is_smd(&plain), "a plain ROM is not SMD");
    }

    #[test]
    fn detects_smd_upper_half_even() {
        let plain = fake_plain();
        assert!(is_smd(&interleave(&plain, EvenHalf::Upper)));
    }

    #[test]
    fn plain_rom_with_512_extra_bytes_not_smd() {
        // The length rule alone is not enough: this is a plain ROM whose size
        // happens to satisfy `len % 16384 == 512`, and it says so at 0x100.
        let plain = fake_plain_at_smd_length();
        assert_eq!(plain.len() % SMD_BLOCK, SMD_HEADER);
        assert!(has_sega_at_100(&plain));
        assert!(!is_smd(&plain));
    }

    #[test]
    fn normalize_rom_returns_plain() {
        let plain = fake_rom(2 * SMD_BLOCK);
        assert_eq!(normalize_rom(&plain), plain);
        assert_eq!(
            normalize_rom(&interleave(&plain, EvenHalf::Lower)),
            plain,
            "SMD input comes back as the ROM inside it"
        );
        assert_eq!(normalize_rom(&interleave(&plain, EvenHalf::Upper)), plain);
    }

    #[test]
    fn normalize_rom_leaves_a_plain_rom_byte_identical() {
        // Not just equal after a parse: the plain path is a no-op on the bytes.
        let plain = fake_rom(3 * SMD_BLOCK + 1);
        let normalized = normalize_rom(&plain);
        assert_eq!(normalized.len(), plain.len());
        assert_eq!(normalized, plain);
    }

    #[test]
    fn wrong_convention_reads_the_system_type_byte_swapped() {
        // Why detection can trust the system type to pick the convention: the
        // wrong one is a pairwise swap, so `SEGA` lands as `ESAG`.
        let plain = fake_plain();
        let interleaved = interleave(&plain, EvenHalf::Lower);
        let block = &interleaved[SMD_HEADER..SMD_HEADER + SMD_BLOCK];
        let swapped = block_in_address_order(block, EvenHalf::Upper);
        assert_eq!(&swapped[SYSTEM_TYPE_AT..SYSTEM_TYPE_AT + 4], b"ESAG");
        assert!(!has_sega_at_100(&swapped));
        assert!(is_smd(&interleaved), "and the right one is still found");
    }

    #[test]
    fn lower_half_holds_the_even_bytes() {
        // The direction the whole module turns on, pinned against smdtech.txt
        // rather than only against the round trip: a reversed deinterleave is
        // invisible to a round trip, since it round trips too.
        let mut block = vec![0u8; SMD_BLOCK];
        block[0] = 1;
        block[1] = 2;
        block[SMD_HALF] = 3;
        block[SMD_HALF + 1] = 4;
        let rom = block_in_address_order(&block, EvenHalf::Lower);
        assert_eq!(&rom[..4], &[1, 3, 2, 4]);
    }

    #[test]
    fn upper_half_holds_the_even_bytes_for_the_other_convention() {
        let mut block = vec![0u8; SMD_BLOCK];
        block[0] = 1;
        block[1] = 2;
        block[SMD_HALF] = 3;
        block[SMD_HALF + 1] = 4;
        let rom = block_in_address_order(&block, EvenHalf::Upper);
        assert_eq!(&rom[..4], &[3, 1, 4, 2]);
    }

    #[test]
    fn detects_a_space_padded_system_type() {
        // `" SEGA"` is a real header, so it must not be mistaken for no header.
        let mut plain = fake_plain();
        plain[SYSTEM_TYPE_AT..SYSTEM_TYPE_AT + 16].copy_from_slice(b" SEGA MEGA DRIVE");
        assert!(!is_smd(&plain), "still a plain ROM");
        assert!(is_smd(&interleave(&plain, EvenHalf::Lower)));
        assert!(is_smd(&interleave(&plain, EvenHalf::Upper)));
    }

    #[test]
    fn ignores_a_file_of_smd_length_with_no_system_type() {
        let data = vec![0u8; SMD_HEADER + SMD_BLOCK];
        assert!(!is_smd(&data));
        assert_eq!(normalize_rom(&data), data);
    }

    #[test]
    fn rejects_inputs_too_small_to_hold_a_block() {
        assert!(!is_smd(&[]));
        assert!(!is_smd(&[0u8; SMD_HEADER]));
        assert!(!is_smd(
            &fake_plain_at_smd_length()[..SMD_HEADER + SMD_HALF]
        ));
    }
}
