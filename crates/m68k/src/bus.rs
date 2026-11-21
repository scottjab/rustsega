//! The bus the 68000 talks to.
//!
//! Chip crates never depend on each other or on `genesis`
//! (`docs/ARCHITECTURE.md`), so the CPU reaches memory and I/O through this
//! trait. `genesis` implements it with its real memory map; the m68k unit
//! tests use a small local one.

/// The 68000 has a 24 bit address bus (M68000UM, "Address Definition": only
/// A23–A1 are decoded, A0 is used for upper/lower byte selection). The core
/// masks every address with this before calling [`Bus`], so implementors never
/// have to.
pub const ADDR_MASK: u32 = 0x00FF_FFFF;

/// Memory and I/O as seen by the 68000.
///
/// All addresses are physical, already masked to 24 bits by the CPU. Words are
/// big endian and, on real hardware, even: the checked accessors of
/// [`crate::cpu::M68k`] raise the address error instead of starting the cycle
/// (M68000UM section 6.3.5 "Address Error").
///
/// That is not something an implementor may index on, though. Byte accesses
/// arrive at either parity (A0 is the byte select), so a mirrored region has to
/// decode its bytes separately - which is what #488 had to teach the Genesis bus
/// to do when `Star Odyssey`'s odd stack pointer had exception entry walking its
/// frame past the end of a RAM mirror. Exception entry is the access that made
/// it matter: it writes its frame with the unchecked accessors, because the
/// stack pointer is what it points at (`crate::address_error`). #522 made the
/// CPU force bit 0 off on its own stack traffic, so those frames land even and
/// the core puts no odd word address on the bus any more; the byte-wise decode
/// stays, because bytes have always arrived at either parity.
pub trait Bus {
    /// Read the byte at `addr` (A0 = 1 on real hardware).
    fn read_byte(&mut self, addr: u32) -> u8;
    /// Read the big endian word at even `addr`.
    fn read_word(&mut self, addr: u32) -> u16;
    /// Write the byte at `addr`.
    fn write_byte(&mut self, addr: u32, value: u8);
    /// Write the big endian word at even `addr`.
    fn write_word(&mut self, addr: u32, value: u16);
    /// Interrupt acknowledge for interrupt level `level` (1..=7). Returning
    /// `None` means "autovector", which is what the Genesis always does: the
    /// CPU takes the vector from its internal vector table instead of asking
    /// the bus for a vector number.
    fn int_ack(&mut self, _level: u8) -> Option<u8> {
        None
    }
    /// The word just fetched into the back of the CPU's prefetch queue: the
    /// instruction-stream word it will consume next.
    ///
    /// This is what an unclaimed data lane answers on real hardware. A bus does
    /// not float to a fixed value; it retains the last word driven over it, and
    /// in a running instruction stream that is a word of the queue. Genesis
    /// Plus GX answers the unused bits of the Z80 control reads with exactly
    /// this - the word at the 68k's PC ("Unused bits return prefetched bus data
    /// (Time Killers)", `ctrl_io_read_word` case 0x11) - and BlastEm's
    /// open-bus value is the word at the end of the current instruction
    /// (`get_open_bus_value`, reading `last_prefetch_address`). Both are the
    /// word this publishes, one call per queue refill.
    ///
    /// The default records nothing. A bus that answers open-bus lanes from the
    /// instruction stream implements it; one that answers a fixed pattern for
    /// unclaimed bits ignores it.
    fn note_prefetched_word(&mut self, _word: u16) {}
    /// Reset every device behind the bus. Used by the 68000 `RESET`
    /// instruction, which restarts the whole console, not just the CPU.
    fn reset_devices(&mut self) {}
}
