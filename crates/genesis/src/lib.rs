//! Genesis system: bus, memory map, cartridge + mappers, controllers, scheduler, audio mixing, save states.

pub mod bus;
pub mod cart;

pub use bus::Bus;
pub use cart::{
    LoadError, Regions, RomHeader, compute_checksum, deinterleave, is_smd, normalize_rom,
    parse_header,
};
