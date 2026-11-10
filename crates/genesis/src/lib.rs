//! Genesis system: bus, memory map, cartridge + mappers, controllers, scheduler, audio mixing, save states.

pub mod bus;
pub mod cart;
pub mod config;
pub mod io;
pub mod z80_bus;

pub use bus::Bus;
pub use cart::{
    LoadError, Regions, RomHeader, compute_checksum, deinterleave, is_smd, normalize_rom,
    parse_header,
};
pub use z80_bus::{Z80Side, Z80View};
// `Region` is the console this emulator presents, one of three; `Regions` above
// is what a cartridge header claims at 0x1F0, a set of three booleans. The names
// are close on purpose and `config::Config::resolve_region` is the mapping.
pub use config::{Config, Region, RegionSetting};
// `Buttons` is what a front end feeds the emulated pads: docs/ARCHITECTURE.md,
// "Input: one `InputState` that merges keyboard, Gamepad API, and touch overlay
// into `Buttons` per port".
pub use io::{Buttons, Device, Io, Pad6State, Port};
