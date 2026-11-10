//! Genesis system: bus, memory map, cartridge + mappers, controllers, scheduler, audio mixing, save states.

pub mod cart;

pub use cart::{LoadError, Regions, RomHeader, compute_checksum, parse_header};
