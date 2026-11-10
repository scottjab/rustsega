//! Genesis system: bus, memory map, cartridge + mappers, controllers, scheduler, audio mixing, save states.

pub mod bus;
pub mod cart;
pub mod config;

pub use bus::Bus;
pub use cart::{
    LoadError, Regions, RomHeader, compute_checksum, deinterleave, is_smd, normalize_rom,
    parse_header,
};
// `Region` is the console this emulator presents, one of three; `Regions` above
// is what a cartridge header claims at 0x1F0, a set of three booleans. The names
// are close on purpose and `config::Config::resolve_region` is the mapping.
pub use config::{Config, Region, RegionSetting};
