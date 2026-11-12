//! What console the emulator presents to the cartridge: region, TMSS, audio rate.
//!
//! Everything here is the host's choice, as opposed to [`crate::cart`], which is
//! the cartridge's claim about itself. [`Config::resolve_region`] is where the
//! two meet, and it is the only thing that turns a header into a region.
//!
//! The region reaches the 68k through one byte: the hardware version register at
//! 0xA10001, `HwVersion: equ $A10001` in plutiedev "Using the Sub-CPU in mode 1".
//! [`Region::version_register`] documents the bits.

use crate::cart::{Regions, RomHeader};

/// Default host sample rate for [`Config::sample_rate`], the 48 kHz of
/// docs/ARCHITECTURE.md ("Audio chips").
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// Master clock of the NTSC machine, Hz: 53_693_175, the number the whole timing
/// model divides (docs/ARCHITECTURE.md, "Clocks and timing").
const MASTER_CLOCK_NTSC_HZ: u64 = 53_693_175;
/// The same for PAL, a little slower: 53_203_424 (docs/ARCHITECTURE.md, "Clocks
/// and timing").
const MASTER_CLOCK_PAL_HZ: u64 = 53_203_424;

/// The default for [`Config::psg_level`]: the PSG at its reference level, which
/// is the balance documented on `crate::audio::PSG_LEVEL_REFERENCE`.
pub const DEFAULT_PSG_LEVEL: u32 = 100;

/// Version register bit 7, MODE: 1 = overseas (Americas or Europe), 0 = the
/// domestic Japanese machine (plutiedev "Using the Sub-CPU in mode 1" calls the
/// register `HwVersion`; the bit names and meanings are Charles MacDonald's
/// Mega Drive notes as reproduced in MAME's `src/mame/sega/megadriv.cpp`).
const MODE_OVERSEAS: u8 = 0x80;
/// Version register bit 6, VMOD: 1 = PAL, 0 = NTSC.
const VMOD_PAL: u8 = 0x40;
/// Version register bit 5, DISK: 1 = no expansion unit attached. A Mega CD in
/// the expansion slot reads 0 here (plutiedev "Using the Sub-CPU in mode 1":
/// "Check if bit 5 of HwVersion is 0"), and this emulator has no expansion, so
/// it is constant 1.
const DISK_NONE: u8 = 0x20;
/// Version register bit 0: the only hardware version number this emulator
/// reports, 1 = TMSS. Bits 3-0 read 0 without TMSS, and TMSS itself is M3-10
/// (#70), which keeps the number at 0 or 1.
const VERSION_TMSS: u8 = 1;

/// Does the version register byte `version` report the PAL machine?
///
/// VMOD is bit 6 of the register [`Region::version_register`] builds, so a
/// [`crate::Bus`] built from `region.version_register(config.tmss)` can tell its
/// VDP which crystal it is clocked by ([`Vdp::new`](vdp::Vdp::new), the `PAL` pin
/// of the chip) from the one byte the bus is handed. It is a question about the
/// register rather than about a [`Region`], because the register is all the bus
/// is given - and the bit lives here, with the rest of the layout.
pub(crate) const fn version_is_pal(version: u8) -> bool {
    version & VMOD_PAL != 0
}

/// A console region: one of the three the Mega Drive / Genesis shipped in.
///
/// This is the single region the emulator runs as, which is why it is its type
/// rather than another use of [`crate::cart::Regions`]: a header can claim three
/// regions at once and a console is only ever one. The two stay separate and
/// [`Config::resolve_region`] is the explicit mapping between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Mega Drive, NTSC, domestic market: header letter `J`.
    Japan,
    /// Genesis, NTSC, Americas: header letter `U`.
    Usa,
    /// Mega Drive, PAL, Europe: header letter `E`.
    Europe,
}

impl Region {
    /// The lower-case name of this region, as `headless` prints it in its summary
    /// line and as `region` is spelled in a movie
    /// (`docs/movie-format.md`).
    ///
    /// One spelling for the three names on purpose: `--region` takes them, the
    /// summary prints them, and a movie records one, and all three have to agree
    /// for `headless run --movie x.rsm` to mean what it says.
    pub fn name(self) -> &'static str {
        match self {
            Self::Japan => "japan",
            Self::Usa => "usa",
            Self::Europe => "europe",
        }
    }

    /// Is this the PAL machine? Only Europe is: the header's Europe bit is the
    /// PAL one (plutiedev "Region support", segaretro "Regional compatiblity"),
    /// and docs/ARCHITECTURE.md "Clocks and timing" gives 313 lines for PAL
    /// against the 262 of NTSC.
    ///
    /// What it decides is everything the two consoles do differently, in three
    /// places: the scanline count of a frame ([`crate::Scheduler::new`], 313 for
    /// PAL and 262 for NTSC), the VMOD bit 6 of the hardware version register
    /// below - which is the chip's PAL pin as far as a program is concerned,
    /// through [`crate::Bus`], and so what allows [`vdp::Vdp::v30_mode`] to have
    /// its 240 lines - and the crystal, in [`Self::master_clock_hz`], which is
    /// only ever used to turn master clocks into seconds
    /// ([`crate::audio::Mixer`]). `tests/pal_timing.rs` checks the three of
    /// them together (M6-12, #132).
    pub fn is_pal(self) -> bool {
        matches!(self, Self::Europe)
    }

    /// The master clock this console runs on, in Hz: 53_693_175 for NTSC and
    /// 53_203_424 for PAL (docs/ARCHITECTURE.md, "Clocks and timing").
    ///
    /// Nothing in the timing model needs this - a scanline is 3420 master clocks
    /// in either region, and the region only decides how many of them a frame has
    /// ([`crate::Scheduler::new`]) - but the mixer has to turn master clocks into
    /// seconds to produce `config.sample_rate` of them, and that conversion is
    /// the one place the two crystals are different ([`crate::audio::Mixer`]).
    pub fn master_clock_hz(self) -> u64 {
        if self.is_pal() {
            MASTER_CLOCK_PAL_HZ
        } else {
            MASTER_CLOCK_NTSC_HZ
        }
    }

    /// The byte that 0xA10001 reports for this region.
    ///
    /// The register layout (plutiedev "Using the Sub-CPU in mode 1" names the
    /// register and its bit 5; Charles MacDonald's Mega Drive notes, as
    /// reproduced in MAME's `src/mame/sega/megadriv.cpp`, the rest):
    ///
    /// ```text
    /// bit 7 MODE  1 = overseas (Americas, Europe), 0 = domestic (Japan)
    /// bit 6 VMOD  1 = PAL, 0 = NTSC
    /// bit 5 DISK  1 = no expansion unit attached
    /// bit 4       unused, always 0
    /// bit 3-0     hardware version: 0 = no TMSS, 1 = TMSS
    /// ```
    ///
    /// So 0x20 for Japan NTSC, 0xA0 for a Genesis and 0xE0 for a PAL Mega Drive,
    /// each plus 1 once TMSS is on.
    pub fn version_register(self, tmss: bool) -> u8 {
        let mut value = DISK_NONE;
        if !matches!(self, Self::Japan) {
            value |= MODE_OVERSEAS;
        }
        if self.is_pal() {
            value |= VMOD_PAL;
        }
        if tmss {
            value |= VERSION_TMSS;
        }
        value
    }
}

/// The console's own output low-pass, which the mixer applies at the host rate
/// after the two chips have been summed (docs/ARCHITECTURE.md, "Audio chips").
///
/// This is the board's filter rather than the chips': the summed audio leaves the
/// motherboard through a resistor and a capacitor, and a Model 1's is low enough
/// that a 10 kHz tone is a third of what it was. The shape is the one Genesis
/// Plus GX has always used - `audio_update` of `core/system.c` runs
/// `l = l * factora + out * factorb` over the finished buffer, which is exactly
/// `y += a * (x - y)` (read 2026-07-31), and its own default coefficient is
/// `0x9999` (`libretro/libretro.c`, `config.lp_range = 0x9999`, 0.6 in 16.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowPass {
    /// No filtering: the mixed chips straight to the host.
    Off,
    /// The first-order low-pass of a Model 1 board, about 3.39 kHz.
    ///
    /// The corner is the ticket's figure for a VA3-ish board. It is not
    /// independent of Genesis Plus GX's default coefficient: `a = 0.6` at 48 kHz
    /// is a corner of about 3.9 kHz (`-ln(1 - a) * fs / 2 PI`), so the two agree
    /// to within half a kilohertz on where the board rolls off and on it being
    /// one pole.
    Model1,
}

/// The host's region setting: follow the cartridge, or ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionSetting {
    /// Decide from the header, in [`Config::prefer`] order: see
    /// [`Config::resolve_region`] for the exact rule.
    Auto,
    /// Run as this region whatever the header claims.
    Force(Region),
}

/// The console a front end asks for, before any ROM is loaded.
///
/// Host choices only: what the cartridge says about itself belongs to
/// [`crate::cart`], and [`Config::resolve_region`] is what combines the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Region, or [`RegionSetting::Auto`] to read it off the cartridge.
    pub region: RegionSetting,
    /// Start with the TMSS security check enabled, which is what a VA6 or later
    /// console does. M3-10 (#70) implements the check; this only decides whether
    /// the version register claims TMSS.
    pub tmss: bool,
    /// Audio output rate in Hz for the mixing docs/ARCHITECTURE.md describes
    /// ("Audio chips", "48 kHz default"). [`crate::audio::Mixer`] resamples to
    /// it, and the output really is this rate: the master clock is what decides
    /// how many samples a second of machine time is worth.
    pub sample_rate: u32,
    /// The console's own output low-pass, [`LowPass::Model1`], the Model 1 board
    /// filter most hosts want; [`LowPass::Off`] for a clean capture of the chips.
    pub lowpass: LowPass,
    /// The PSG's volume relative to the FM's, in percent: 100 is the balance of a
    /// VA4 Model 1 that `crate::audio::PSG_LEVEL_REFERENCE` is drawn from, 0 is
    /// the FM alone and 200 is the PSG twice as loud as that balance.
    ///
    /// A host turns this up when a game's sound effects sit under its music and
    /// down when the PSG chokes the FM; it is a mix control rather than a
    /// correction, which is why the default is the hardware's own balance and not
    /// something louder.
    pub psg_level: u32,
    /// Auto-detect order for a header that claims more than one region, most
    /// preferred first: [`Config::default`] is the ticket's order, USA, then
    /// Europe, then Japan. A host that prefers a European machine puts Europe
    /// first and gets PAL out of an all-region header.
    pub prefer: [Region; 3],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            region: RegionSetting::Auto,
            tmss: false,
            sample_rate: DEFAULT_SAMPLE_RATE,
            lowpass: LowPass::Model1,
            psg_level: DEFAULT_PSG_LEVEL,
            prefer: [Region::Usa, Region::Europe, Region::Japan],
        }
    }
}

impl Config {
    /// Which region to run `header` as.
    ///
    /// An explicit [`RegionSetting::Force`] wins over the cartridge whatever it
    /// claims, including a header too broken to read. Otherwise take the first
    /// entry of [`Config::prefer`] the header claims at 0x1F0, and a header that
    /// claims nothing usable - the common case for homebrew, see
    /// [`crate::cart::Regions`] - becomes [`Region::Usa`].
    ///
    /// That default is fixed rather than `prefer[0]`: it is what a header that
    /// says nothing gets, not part of the order the host chooses, and running an
    /// unknown ROM as an NTSC export machine is the choice that most of them
    /// want.
    pub fn resolve_region(&self, header: &RomHeader) -> Region {
        if let RegionSetting::Force(region) = self.region {
            return region;
        }
        self.prefer
            .iter()
            .copied()
            .find(|&region| claimed(header.regions, region))
            .unwrap_or(Region::Usa)
    }
}

/// Does the header's [`Regions`] claim `region`?
///
/// The one mapping between the header's three booleans and this crate's
/// [`Region`], spelled out so the two types cannot be confused for each other.
/// The Japan-PAL bit of a new style header (`cart::header::new_style_bits` bit 1)
/// is not a `Region` and is dropped by the header decode, so it claims nothing
/// here.
fn claimed(claims: Regions, region: Region) -> bool {
    match region {
        Region::Japan => claims.japan,
        Region::Usa => claims.americas,
        Region::Europe => claims.europe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cart::parse_header;

    /// The header of a ROM whose region field at 0x1F0 is `field`, read back by
    /// the real decoder rather than built by hand, so both header formats work
    /// through here (`"JUE"` is old style, `"8"` new style).
    fn header(field: &str) -> RomHeader {
        assert_eq!(field.len(), 3, "the region field is three bytes wide");
        let mut rom = vec![0u8; 0x200];
        rom[0x100..0x200].fill(b' ');
        rom[0x1F0..0x1F3].copy_from_slice(field.as_bytes());
        parse_header(&rom).expect("a 0x200 byte ROM has a header")
    }

    #[test]
    fn version_register_usa() {
        assert_eq!(Region::Usa.version_register(false), 0xA0);
        assert!(!Region::Usa.is_pal());
    }

    #[test]
    fn version_register_japan() {
        assert_eq!(Region::Japan.version_register(false), 0x20);
        assert!(!Region::Japan.is_pal());
    }

    #[test]
    fn version_register_europe_pal() {
        assert_eq!(Region::Europe.version_register(false), 0xE0);
        assert!(Region::Europe.is_pal());
    }

    #[test]
    fn version_register_with_tmss() {
        // TMSS is bit 0 of the low nibble and nothing else changes.
        assert_eq!(Region::Japan.version_register(true), 0x21);
        assert_eq!(Region::Usa.version_register(true), 0xA1);
        assert_eq!(Region::Europe.version_register(true), 0xE1);
    }

    #[test]
    fn auto_detect_prefers_usa_when_jue() {
        let config = Config::default();
        assert_eq!(config.resolve_region(&header("JUE")), Region::Usa);
    }

    #[test]
    fn auto_detect_europe_only_is_pal() {
        let config = Config::default();
        // Old style letters and the new style hex digit must agree.
        for field in ["E  ", "8  "] {
            let region = config.resolve_region(&header(field));
            assert_eq!(region, Region::Europe, "region field {field:?}");
            assert!(region.is_pal(), "region field {field:?}");
        }
    }

    #[test]
    fn auto_detect_unknown_region_defaults_usa() {
        let config = Config::default();
        for field in ["   ", "\0\0\0"] {
            assert_eq!(
                config.resolve_region(&header(field)),
                Region::Usa,
                "region field {field:?}"
            );
        }
    }

    #[test]
    fn explicit_region_overrides_header() {
        let config = Config {
            region: RegionSetting::Force(Region::Japan),
            ..Config::default()
        };
        // A header claiming every region, and one claiming none of them, both
        // lose to an explicit setting.
        assert_eq!(config.resolve_region(&header("JUE")), Region::Japan);
        assert_eq!(config.resolve_region(&header("   ")), Region::Japan);
    }

    #[test]
    fn auto_detect_follows_prefer_order() {
        // The order is a Config field, so a European host gets PAL out of the
        // same all-region header an American one gets NTSC from.
        let config = Config {
            prefer: [Region::Europe, Region::Usa, Region::Japan],
            ..Config::default()
        };
        assert_eq!(config.resolve_region(&header("JUE")), Region::Europe);
        assert_eq!(config.resolve_region(&header("JU ")), Region::Usa);
    }

    #[test]
    fn config_defaults() {
        let config = Config::default();
        assert_eq!(config.region, RegionSetting::Auto);
        assert!(!config.tmss);
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.lowpass, LowPass::Model1);
        assert_eq!(config.psg_level, DEFAULT_PSG_LEVEL);
        assert_eq!(config.prefer, [Region::Usa, Region::Europe, Region::Japan]);
    }

    /// The two crystals, and only the two of them: NTSC for the two NTSC regions
    /// and PAL for Europe (docs/ARCHITECTURE.md, "Clocks and timing").
    #[test]
    fn master_clock_per_region() {
        assert_eq!(Region::Usa.master_clock_hz(), 53_693_175);
        assert_eq!(Region::Japan.master_clock_hz(), 53_693_175);
        assert_eq!(Region::Europe.master_clock_hz(), 53_203_424);
    }
}
