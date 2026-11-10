//! The Sega cartridge mapper: pages of an image larger than the 4 MiB cartridge
//! area, switched in by the seven registers at 0xA130F3-0xA130FF.
//!
//! The console sees 4 MiB of cartridge ([`crate::bus`]'s `Region::Rom`), so a
//! bigger image is only readable a piece at a time (plutiedev "Going beyond
//! 4MB", "How it works"). The vocabulary there is worth keeping straight
//! because the two words are easy to swap: a **page** is a 512 KiB chunk of the
//! image, a **slot** is a 512 KiB chunk of what the console sees, and a mapper
//! is the lookup that says which page answers which slot.
//!
//! ```text
//! slot 0   0x000000-0x07FFFF  no register: pinned to page 0
//! slot 1   0x080000-0x0FFFFF  0xA130F3
//! slot 2   0x100000-0x17FFFF  0xA130F5
//! slot 3   0x180000-0x1FFFFF  0xA130F7
//! slot 4   0x200000-0x27FFFF  0xA130F9
//! slot 5   0x280000-0x2FFFFF  0xA130FB
//! slot 6   0x300000-0x37FFFF  0xA130FD
//! slot 7   0x380000-0x3FFFFF  0xA130FF
//! ```
//!
//! The seven registers and their slots are plutiedev's "List of bank switching
//! registers"; the pinning of slot 0 is that page's "The first bank is always
//! pointed to the first page (so the game can boot safely on reset), the other
//! banks can be pointed to any page we want at any moment".
//!
//! This is the mapper of Super Street Fighter II, the only official cart to use
//! it (plutiedev, "The Sega mapper has only been used by one official game").
//! Two neighbours are deliberately left out: the Mega Everdrive's *extended*
//! variant, which has a register for slot 0 as well and which Genesis Plus GX
//! gives any cart whose system type says `SEGA SSF` (its `mapper_512k_w`, of
//! which `mapper_ssf2_w` is the slot-0-pinned form), and the unlicensed mappers
//! of M3-17.
//!
//! ## Nothing unlocks these registers
//!
//! A write to a bank register takes effect on its own, and so does a write to
//! [`crate::bus`]'s save RAM control at `0xA130F0`: there is no unlock byte for
//! the block. Every reference that decodes it agrees -
//!
//! - plutiedev "Going beyond 4MB", "Putting it into use": the documented
//!   sequence is `move.b #9, (MapperBank7)`, with no setup write before it, and
//!   the "$A130xx area usage" page lists `$A130F0 to $A130FF` as "Sega mapper"
//!   without a gate either;
//! - plutiedev "Saving progress with SRAM" has the save RAM control answered by
//!   the first byte written to it (`move.b #1, (SramLock)`), which no unlock
//!   could survive: nothing has been written by the time it is asked to work;
//! - Genesis Plus GX's `md_cart.c`: `mapper_sega_w` takes the map and write
//!   protect bits from the byte it is handed, and `default_time_w` routes
//!   anything above `0xA130F1` to `mapper_512k_w`, both unconditionally;
//! - BlastEm's `sega_mapper.c`: `write_bank_reg_b` and `write_bank_reg_w`
//!   move a bank on the write that arrives, with no state read first.
//!
//! The locks that do exist around this address range belong to other cart
//! hardware, and none of them is a key value to write: SF-001's lock bit and the
//! T-5740's speculative "enable bankswitch registers access" bit, both in
//! Genesis Plus GX, and the Mega SD's own protect bit - which holds writes to
//! *its* bank #0 register until bit 7 of the byte written to `0xA130F0` says so
//! (`megasd_enhanced_ssf2_mapper_w`, and one more reason not to read a meaning
//! into what a cart writes to that lane). That is where the idea that a mapper
//! register needs unlocking comes from; for the Sega mapper on an ordinary cart,
//! no reference asks for one and the save RAM's documented use would break if
//! there were.

use super::header::parse_header;

/// Bytes in one page of the image, and in one slot of the cartridge area: the
/// 512 KiB that plutiedev "Going beyond 4MB" splits both of them into.
const SLOT_SIZE: usize = 1 << SLOT_SHIFT;
/// The shift that turns a 68k address in the cartridge area into its slot
/// number: `SLOT_SIZE` is 2^19.
const SLOT_SHIFT: u32 = 19;
/// The eight slots the mapper divides [`crate::bus`]'s cartridge area into, so
/// 4 MiB: the most the console can see at once.
const CARTRIDGE_SIZE: usize = 8 * SLOT_SIZE;
/// The system type of a cart asking for the mapper by name, at 0x100: "replace
/// the system field of the ROM header ($0000100) with `"SEGA SSF"` (pad with
/// spaces), instead of the usual `"SEGA MEGA DRIVE"` ... This will enable the
/// mapper on Mega Everdrive, Mega SD, BlastEm, and possibly more" (plutiedev
/// "Going beyond 4MB", "ROM header").
const SYSTEM_TYPE: &str = "SEGA SSF";
/// The address of the save RAM control register, which [`crate::bus`] calls
/// `SRAM_CONTROL`: the first pair of the block this mapper's registers live in.
/// The bank registers are the seven pairs above it, so the slot of a lane is
/// its distance from here in pairs - and slot 0's own lane is the save RAM
/// control, which is a second reason the pinned slot of
/// [`Mapper::set_bank`] cannot be written by accident.
const REGISTER_BLOCK: u32 = 0xA130F0;
/// The last pair of bank registers, `0xA130FE`/`0xA130FF`, the top of the block.
const LAST_BANK_LANE: u32 = REGISTER_BLOCK + 0x0E;

/// What turns a cartridge address into an offset in the image: nothing, or the
/// Sega mapper's seven bank registers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapper {
    /// A cart small enough for the cartridge area, or one that says nothing
    /// about being bigger: the image answers in address order, which is what
    /// [`Mapper::map`] reporting `None` tells [`crate::Bus`] to do.
    None,
    /// The Sega mapper, with one page of the image behind each of the eight
    /// 512 KiB slots.
    SegaSsf2 {
        /// Page of the image behind each slot, indexed by slot.
        ///
        /// `banks[0]` is always 0 and is kept in the array so that a read is one
        /// index rather than an index with a special case: slot 0 is pinned to
        /// page 0 by the hardware, so that a cart boots, and the lane its
        /// register would be at is the save RAM control instead
        /// ([`REGISTER_BLOCK`]).
        banks: [u8; 8],
    },
}

impl Mapper {
    /// The bank numbers a cart comes up with: slot `n` on page `n`, which is the
    /// first 4 MiB of the image read in address order.
    ///
    /// BlastEm's `alloc_config_genesis` does exactly this on "initialize bank
    /// registers", giving each of the seven registers its own slot number, and it
    /// is the mapping that makes an oversized cart boot before its own init code
    /// has run: Genesis Plus GX asks for the same thing by reinstalling the
    /// default mapping on `/VRES` for the carts with `cart.hw.bankshift`.
    fn identity() -> [u8; 8] {
        std::array::from_fn(|slot| slot as u8)
    }

    /// The mapper the cart `rom` carries, in its power-on state.
    ///
    /// Two things ask for one, and either is enough:
    ///
    /// - **more image than cartridge area.** Past 4 MiB the mapper is the only
    ///   way the extra bytes can be read at all, so a cart that big is asking
    ///   whether or not its header says so;
    /// - **the system type** [`SYSTEM_TYPE`], which is how a cart of 4 MiB or
    ///   less requests the mapper - the way of plutiedev "Going beyond 4MB",
    ///   "ROM header" quoted on [`SYSTEM_TYPE`], which BlastEm reads with a
    ///   comparison of the first eight bytes of the field (`romdb.c`) and
    ///   Genesis Plus GX as a substring of the console type (`md_cart.c`).
    ///
    /// Neither reference needs the first condition: BlastEm enables the mapper on
    /// the system type alone, with its cart database covering the carts whose
    /// header forgot to ask, and Genesis Plus GX works off the console type, the
    /// title and its own table. The size is here anyway because it is the one of
    /// the two that cannot be wrong about a cart that big, and because enabling a
    /// mapper a cart does not use costs nothing: [`Mapper::identity`] is the
    /// plain image, so a cart that never writes the registers never notices one
    /// is there.
    ///
    /// Nothing here comes from the cart's serial, although both references do keep
    /// one: BlastEm lists the Super Street Fighter II serials (`MK-12056`,
    /// `T-12043`, `T-12056`) in its cart database as carrying the Sega mapper, and
    /// Genesis Plus GX finds that game by name. It is not needed for #72 because
    /// Super Street Fighter II is 5 MB and so is enabled by its size alone; a
    /// table like [`super::sram::SRAM_OVERRIDES`] is the compatibility sweep
    /// (M6-05, #124) finding the carts that need one, and it will want the mapper
    /// and the save RAM entries in the same pass.
    pub fn detect(rom: &[u8]) -> Self {
        if rom.len() > CARTRIDGE_SIZE || asks_for_the_mapper(rom) {
            Self::SegaSsf2 {
                banks: Self::identity(),
            }
        } else {
            Self::None
        }
    }

    /// The offset in the image that a read of the cartridge address `addr`
    /// answers from, or `None` when this cart has no mapper and `addr` is
    /// therefore its own offset.
    ///
    /// `addr` is a cartridge address - masked to 24 bits and decoded as
    /// [`crate::bus`]'s `Region::Rom` by the caller - so `addr >> SLOT_SHIFT` is
    /// its slot and the low 19 bits are the byte within it.
    ///
    /// Whether the offset exists is not this function's question, and that is
    /// deliberate: the caller's lookup in the image is what turns a page number
    /// past the end of the cart - and a cart shorter than the area it fills -
    /// into open bus. Keeping the image length out of here means the mapper
    /// cannot disagree with the image about how long it is, and it is why a bank
    /// register can hold any `u8` without a check. Genesis Plus GX masks the
    /// page instead (`mapper_512k_w`'s `(data << 19) & cart.mask`, which mirrors
    /// a too-large page back into the image); this reads nothing, which is what
    /// a page the cart does not have is.
    pub fn map(&self, addr: u32) -> Option<usize> {
        match self {
            Self::None => None,
            Self::SegaSsf2 { banks } => {
                let page = usize::from(banks[(addr >> SLOT_SHIFT) as usize]);
                Some(page * SLOT_SIZE + (addr as usize & (SLOT_SIZE - 1)))
            }
        }
    }

    /// A write of `page` to the register of slot `slot`, as [`bank_slot`] names
    /// one.
    ///
    /// Slot 0 refuses: it is pinned to page 0 so that the cart boots, which is
    /// why it has no register at all ([`REGISTER_BLOCK`]). A cart with no mapper
    /// ignores the write entirely - [`Mapper::detect`] decided whether this cart
    /// has bank registers, and a cart that fills the cartridge area and says
    /// nothing about the mapper does not.
    pub fn set_bank(&mut self, slot: usize, page: u8) {
        let Self::SegaSsf2 { banks } = self else {
            return;
        };
        if slot != 0 && slot < banks.len() {
            banks[slot] = page;
        }
    }

    /// Restart the registers, which is what a reset does: every slot back on the
    /// page it came up with ([`Mapper::identity`]). BlastEm does the same in
    /// `alloc_config_genesis`, and Genesis Plus GX asks for it with
    /// `cart.hw.bankshift = 1` on the carts that have the mapper - "cartridge ROM
    /// mapping is reinitialized on /VRES".
    pub fn reset(&mut self) {
        if let Self::SegaSsf2 { banks } = self {
            *banks = Self::identity();
        }
    }
}

/// Which bank register's slot `addr` names, or `None` when `addr` is not one of
/// the seven.
///
/// The registers are byte registers at the odd addresses 0xA130F3, F5, F7, F9,
/// FB, FD and FF, and the pair is the register: 0xA130F2 answers as 0xA130F3
/// does, with the value of a write coming from the byte at the odd lane.
///
/// Genesis Plus GX and BlastEm both decode the pair rather than the lane -
/// `mapper_512k_w` picks its slot with `(address << 2) & 0x38`, which drops the
/// low bits of the address, and `write_bank_reg_w` with `address &= 0xE;
/// address >>= 1` - and both take the page from the low half of a 16 bit write.
/// BlastEm's 8 bit handler ignores the even lane of a pair instead of answering
/// for it, which only a program byte-writing 0xA130F2 could tell the
/// difference. The pair is the rule here anyway: it is how every other register
/// pair on this bus decodes (`EVEN_LANE` in [`crate::bus`], and the save RAM
/// control's own `0xA130F0`/`0xA130F1` from #71), and a byte write and a word
/// write to one pair cannot be allowed to disagree about what is there.
///
/// Only the seven pairs above the save RAM control at [`REGISTER_BLOCK`] are
/// bank registers: `0xA130F0`/`0xA130F1` is the save RAM control that
/// `crate::bus` calls `SRAM_CONTROL`, and 0xA130EC-0xA130EF is the 32X signature
/// of plutiedev "$A130xx area usage", so the decode is the exact range rather
/// than three bits of the address.
pub fn bank_slot(addr: u32) -> Option<usize> {
    let lane = addr & !1;
    if lane <= REGISTER_BLOCK || lane > LAST_BANK_LANE {
        return None;
    }
    Some(((lane - REGISTER_BLOCK) / 2) as usize)
}

/// Does the header of `rom` ask for the mapper with [`SYSTEM_TYPE`]?
///
/// `starts_with` is the comparison the references make rather than equality:
/// BlastEm compares the first eight bytes of the 16 byte field (`romdb.c`'s
/// `memcmp("SEGA SSF", rom + 0x100, 8)`) and Genesis Plus GX looks for the
/// string inside the console type (`md_cart.c`'s `strstr`), which for a field
/// that starts with `SEGA` as every Genesis cart header's does amount to the
/// same thing.
///
/// An image with no readable header asks for nothing, which is the answer a
/// header saying `SEGA MEGA DRIVE` gets too.
fn asks_for_the_mapper(rom: &[u8]) -> bool {
    parse_header(rom).is_ok_and(|header| header.system_type.starts_with(SYSTEM_TYPE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::require_data;

    /// The 5 MB cart of the ticket: every byte of each of its ten 512 KiB pages
    /// is that page's number, so `image[page * 0x80000] == page` and a read of
    /// any byte says which page it came from.
    fn banked_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x50_0000];
        for page in 0..10u8 {
            let start = usize::from(page) * SLOT_SIZE;
            rom[start..start + SLOT_SIZE].fill(page);
        }
        rom
    }

    /// The byte that answers the start of slot `slot` under `mapper`: `map` is a
    /// translation, so the byte is whatever the image holds at the offset it
    /// gives back.
    fn page_at(image: &[u8], mapper: &Mapper, slot: usize) -> Option<u8> {
        let offset = mapper
            .map((slot * SLOT_SIZE) as u32)
            .unwrap_or(slot * SLOT_SIZE);
        image.get(offset).copied()
    }

    /// [`Mapper::detect`] on the two conditions, each alone: an image over the
    /// 4 MiB cartridge area, and a 4 MiB-or-smaller image whose system type is
    /// `SEGA SSF`.
    #[test]
    fn detect_enables_on_size_or_on_the_system_type() {
        assert_eq!(Mapper::detect(&[]), Mapper::None, "nothing to map");
        assert_eq!(
            Mapper::detect(&vec![0u8; CARTRIDGE_SIZE]),
            Mapper::None,
            "4 MiB is what the cartridge area holds, so it needs no mapper"
        );

        // Size alone: no header at all, and no `SEGA SSF` anywhere in it.
        assert_eq!(
            Mapper::detect(&banked_rom()),
            Mapper::SegaSsf2 {
                banks: Mapper::identity()
            }
        );

        // System type alone, on an image the mapper has no need to be there for.
        let mut rom = vec![b' '; 0x400];
        rom[0x100..0x110].copy_from_slice(b"SEGA SSF        ");
        assert_ne!(Mapper::detect(&rom), Mapper::None, "as homebrew asks");

        // And the ordinary cart, same size, saying nothing of the kind.
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        assert_eq!(Mapper::detect(&rom), Mapper::None);
    }

    /// Slot `n` maps page `n`, so a 5 MB cart reads as its first 4 MiB until the
    /// program says otherwise, and the pinned slot 0 is page 0.
    #[test]
    fn identity_maps_the_first_four_megabytes_in_order() {
        let rom = banked_rom();
        let mapper = Mapper::detect(&rom);
        for slot in 0..8usize {
            assert_eq!(page_at(&rom, &mapper, slot), Some(slot as u8));
        }
    }

    /// The seven bank registers, at the odd addresses of plutiedev's table, and
    /// nothing else of the 0xA13000-0xA130FF block: the save RAM control's pair
    /// below them, and the corners of the block that other hardware owns.
    #[test]
    fn bank_registers_are_the_seven_pairs_above_the_sram_control() {
        for slot in 1..8usize {
            let addr = 0xA130F0 + 2 * slot as u32;
            assert_eq!(bank_slot(addr), Some(slot), "{addr:#010x}");
            assert_eq!(bank_slot(addr + 1), Some(slot), "{:#010x}", addr + 1);
        }

        for addr in [
            0xA13000, 0xA13001, // bottom of the block
            0xA130EC, 0xA130ED, // 32X signature
            0xA130EE, 0xA130EF, 0xA130F0, 0xA130F1, // the save RAM control
        ] {
            assert_eq!(bank_slot(addr), None, "{addr:#010x} is not a bank");
        }
    }

    /// A bank register write moves its slot and nothing else, and slot 0 - which
    /// has no register - cannot be moved at all.
    #[test]
    fn writing_a_bank_moves_its_slot_only() {
        let rom = banked_rom();
        let mut mapper = Mapper::detect(&rom);

        mapper.set_bank(7, 9);
        assert_eq!(page_at(&rom, &mapper, 7), Some(9));
        for slot in 0..7 {
            assert_eq!(page_at(&rom, &mapper, slot), Some(slot as u8));
        }

        // Slot 0 is pinned, and `bank_slot` never names it either: the lane of
        // its nonexistent register is the save RAM control's.
        mapper.set_bank(0, 9);
        assert_eq!(page_at(&rom, &mapper, 0), Some(0));
        assert_eq!(bank_slot(0xA130F0), None);
        assert_eq!(bank_slot(0xA130F1), None);
    }

    /// A reset puts every slot back on the page it came up with, however the
    /// registers were left.
    #[test]
    fn reset_restores_the_identity() {
        let rom = banked_rom();
        let mut mapper = Mapper::detect(&rom);
        for slot in 1..8 {
            mapper.set_bank(slot, 9);
        }
        assert_eq!(page_at(&rom, &mapper, 3), Some(9));

        mapper.reset();
        for slot in 0..8usize {
            assert_eq!(page_at(&rom, &mapper, slot), Some(slot as u8));
        }
    }

    /// A page number past the end of the image is a translation to an offset the
    /// image does not have, and no panic: [`Mapper::map`] reports the offset and
    /// the caller's lookup is what has nothing there. `bus`'s own test reads such
    /// a bank back as open bus.
    #[test]
    fn a_page_past_the_end_of_the_image_translates_and_misses() {
        let rom = banked_rom();
        let mut mapper = Mapper::detect(&rom);
        mapper.set_bank(7, 200);

        let offset = mapper.map(0x38_0000).expect("a translation");
        assert!(
            offset >= rom.len(),
            "{offset:#010x} is past the end of a 5 MB image"
        );
        assert!(
            page_at(&rom, &mapper, 7).is_none(),
            "and so it reads nothing"
        );
    }

    /// The real Super Street Fighter II, the one official cart with this mapper,
    /// if this machine has it: its header parses, it declares no save RAM for
    /// [`super::sram`] to build an overlay out of, and [`Mapper::detect`] gives
    /// it the mapper.
    ///
    /// Skips by design. `require_data!` returns early wherever
    /// `$RUSTSEGA_ROM_DIR` has no such file, which is every CI run and this
    /// machine - the local collection has the Street Fighter 1 and 2 variants
    /// and no Super Street Fighter II - and the test is `#[ignore]`d on top of
    /// that so it does not even show as skipped. What it stands in for is
    /// covered against a synthetic 5 MB image by the tests above and by the
    /// banked-cart tests in [`crate::bus`].
    #[test]
    #[ignore = "needs a commercial ROM, which only exists locally"]
    fn ssf2_boots_header() {
        // Prefix rather than a pinned file name: the region and revision
        // suffixes of this one differ between dumps, and BlastEm's cart
        // database has three serials for it (MK-12056, T-12043, T-12056).
        let path = require_data!(testkit::find_commercial_rom("Super Street Fighter II"));
        let rom = std::fs::read(path).expect("read Super Street Fighter II");

        let header = parse_header(&rom).expect("its header parses");
        assert!(
            header.system_type.starts_with("SEGA"),
            "{}",
            header.system_type
        );
        assert!(
            header.domestic_title.contains("STREET FIGHTER")
                || header.overseas_title.contains("STREET FIGHTER"),
            "{}/{}",
            header.domestic_title,
            header.overseas_title,
        );

        // 5 MB, which is what enables the mapper whatever the system type says:
        // the carts that need it are the ones that are too big for the cartridge
        // area, and only the smaller ones have to ask by name.
        assert!(
            rom.len() > CARTRIDGE_SIZE,
            "{} bytes, which is the whole point",
            rom.len()
        );
        let mut mapper = Mapper::detect(&rom);
        assert_ne!(mapper, Mapper::None, "a 5 MB cart is the mapper's");
        assert_eq!(mapper.map(0x38_0000), Some(0x38_0000), "identity");

        mapper.set_bank(7, 0);
        assert_eq!(mapper.map(0x38_0000), Some(0), "and switchable");

        // The assumption #71 and this ticket make together: a cart this big is a
        // mapper cart, and it has no save RAM for the overlay to sit on.
        assert_eq!(
            crate::cart::sram::detect(&rom),
            None,
            "{} declares no save RAM",
            header.serial
        );
    }

    /// A cart with no mapper has no bank registers to write: [`Mapper::set_bank`]
    /// is where that is decided, so the bus can call it for every cart.
    #[test]
    fn a_mapper_disabled_cart_ignores_bank_writes() {
        let image = vec![0u8; 0x10_0000];
        let mut mapper = Mapper::detect(&image);
        assert_eq!(mapper, Mapper::None);

        for slot in 1..8 {
            mapper.set_bank(slot, 5);
        }
        mapper.reset();
        // `None` is not a translation, so the address stays the offset - which is
        // the plain image, exactly as a 1 MB cart reads.
        assert_eq!(mapper.map(0x08_0000), None);
        assert_eq!(mapper.map(0x38_0000), None);
        assert_eq!(image[0x08_0000], 0);
    }
}
