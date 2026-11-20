//! Cartridge: the ROM image and the header that describes it. What a cart's
//! hardware adds to the slot - the save RAM of [`sram`], the serial EEPROM of
//! [`eeprom`], the bank registers of [`mapper`], and the J-CART register of
//! [`jcart`] - each decoded from the image and each living behind
//! [`crate::Bus`]'s cartridge area - has its own module.
//!
//! [`mapper`] holds the console's own cart mapper and the one unlicensed mapper
//! with evidence in the local collection - the survey of the rest is
//! [`docs/mappers.md`](../../../../docs/mappers.md).

pub mod eeprom;
pub mod header;
pub mod jcart;
pub mod mapper;
pub mod smd;
pub mod sram;
pub mod svp;

pub use eeprom::{Board, Chip, Eeprom, EepromInfo};
pub use header::{Regions, RomHeader, compute_checksum, parse_header};
pub use jcart::Jcart;
pub use mapper::Mapper;
pub use smd::{deinterleave, is_smd, normalize_rom};
pub use sram::{Sram, SramInfo, SramKind};
pub use svp::Svp;

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
