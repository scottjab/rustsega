//! Cartridge: ROM header, ROM loading, and later the mappers and the save RAM
//! that hang off them.

pub mod header;

pub use header::{Regions, RomHeader, parse_header};

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
