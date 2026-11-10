//! Cartridge: the ROM image and the header that describes it. The mappers and
//! the save RAM it declares are decoded in their own tickets.

pub mod header;

pub use header::{Regions, RomHeader, compute_checksum, parse_header};

/// Why a ROM image could not be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// Under 0x200 bytes, so there is no header to read at 0x100.
    TooSmall,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSmall => write!(f, "ROM is too small to hold a header at 0x100"),
        }
    }
}

impl std::error::Error for LoadError {}
