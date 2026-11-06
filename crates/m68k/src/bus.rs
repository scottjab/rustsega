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
/// big endian and, on real hardware, always even; the CPU guarantees even word
/// addresses so implementors can index directly.
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
    /// Reset every device behind the bus. Used by the 68000 `RESET`
    /// instruction, which restarts the whole console, not just the CPU.
    fn reset_devices(&mut self) {}
}
