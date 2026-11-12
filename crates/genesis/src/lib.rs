//! The console: what a front end builds a game on.
//!
//! [`Genesis`] is the whole machine - the 68000, the sound CPU, the VDP, the
//! cartridge and the I/O chip - wired together by [`Bus`] and interleaved by
//! [`Scheduler`] - and this module is the only part a front end needs:
//! `Genesis::new` with a ROM, [`Genesis::run_frame`] once per picture, and
//! [`Genesis::frame`] to look at what came back (docs/ARCHITECTURE.md,
//! "genesis").
//!
//! The pieces are all crate-visible already, and none of them is a lie about
//! the machine: the bus is the 68k's memory map (M3-04 #64 onwards), the
//! scheduler is the master clock (M3-14 #75), and the cartridge is the image
//! plus whatever its header says it has (M3-06 to M3-13). What this module adds
//! is the order they are started in - normalize, header, region, bus, reset -
//! which is the one thing no single one of them can do for itself.

pub mod audio;
mod big_array;
pub mod bus;
pub mod cart;
pub mod config;
pub mod io;
pub mod scheduler;
pub mod state;
pub mod z80_bus;

use m68k::Bus as _;
use m68k::M68k;

use crate::audio::Mixer;
use z80::Z80;

pub use bus::Bus;
pub use cart::{
    LoadError, Regions, RomHeader, compute_checksum, deinterleave, is_smd, normalize_rom,
    parse_header,
};
pub use scheduler::Scheduler;
pub use state::StateError;
pub use z80_bus::{Z80Side, Z80View};
// `Region` is the console this emulator presents, one of three; `Regions` above
// is what a cartridge header claims at 0x1F0, a set of three booleans. The names
// are close on purpose and `config::Config::resolve_region` is the mapping.
pub use config::{Config, LowPass, Region, RegionSetting};
// `Buttons` is what a front end feeds the emulated pads: docs/ARCHITECTURE.md,
// "Input: one `InputState` that merges keyboard, Gamepad API, and touch overlay
// into `Buttons` per port".
pub use io::{Buttons, Device, Io, Pad6State, Port};

/// The console.
///
/// Everything behind it is private on purpose: the machine only works if the
/// 68000, the sound CPU and the VDP are stepped by the same [`Scheduler`]
/// against the same [`Bus`], and handing either out on its own is how a front
/// end ends up running one without the other. What a caller needs instead is
/// here already: [`Genesis::frame`] for the picture, [`Genesis::sram`] for the
/// save, and [`Genesis::set_pad`] for the pads.
pub struct Genesis {
    /// The 68000, held the way the scheduler leaves it: [`Bus`] is its memory
    /// map, and it is never stepped outside [`Genesis::run_frame`].
    cpu: M68k,
    /// The sound CPU. Held in reset by [`Bus::new`] until the 68k lets it go,
    /// which is what the machine does at power on (see [`Bus::z80_can_run`]).
    z80: Z80,
    /// The memory map, the cartridge and every device behind it - including the
    /// VDP whose frame [`Genesis::frame`] hands out.
    bus: Bus,
    /// The master clock the two CPUs and the VDP share.
    scheduler: Scheduler,
    /// The mixer that turns the two chips' own rates into `config.sample_rate`
    /// for [`Genesis::drain_audio`] - the resampler, the levels, the DC blocker
    /// and the board's low-pass (M5-14, #119).
    ///
    /// It lives here rather than on [`Bus`] because its rate, its PSG level and
    /// its filter are the host's choice, out of [`Config`], and the bus is built
    /// from a version byte with no [`Config`] in sight. Its clock position is the
    /// mixer's own and is never wound back, so a reset of the chips does not make
    /// the mixer ask for samples it has already had.
    mixer: Mixer,
}

impl Genesis {
    /// Build the console `rom` runs on and reset it, so the next call is
    /// [`Genesis::run_frame`].
    ///
    /// The order is the one every part of `cart` and [`Bus`] assumes and is the
    /// reason this function exists:
    ///
    /// 1. [`normalize_rom`] - an SMD image is deinterleaved before anything
    ///    reads a byte of it, and a plain one is untouched (M3-06 #62).
    /// 2. [`parse_header`] - and its [`LoadError`] is this function's: a ROM
    ///    too small to hold the header at 0x100 is refused here rather than
    ///    discovered halfway through a load or, worse, panicked on.
    /// 3. [`Config::resolve_region`] then [`Region::version_register`] - the
    ///    one place a header becomes a console, and the byte the cartridge slot
    ///    reports at 0xA10001 (M3-04 #64).
    /// 4. [`Bus::new`] - which reads the rest of the cartridge itself: the save
    ///    RAM, the serial EEPROM and the bank registers all come out of the
    ///    image, so none of them can disagree with it.
    /// 5. [`vdp::Vdp::start_in_vblank`] - the beam goes to the first line of
    ///    VBlank, so that the 68000's first instruction, and every register the
    ///    ROM writes from it, is written during blanking and is in time for line 0
    ///    of the first frame (#310).
    /// 6. The 68000's own reset, which is what turns those vectors into a
    ///    running program: `SSP` from `$000000` and `PC` from `$000004`
    ///    (M68000UM section 5 "Reset").
    ///
    /// The sound CPU is deliberately not started here. A real console holds it
    /// in reset until the 68k releases the line, [`Bus::new`] leaves that line
    /// asserted, and the scheduler takes the release edge as the Z80's own
    /// reset (plutiedev, "Using the Z80").
    pub fn new(rom: &[u8], config: Config) -> Result<Self, LoadError> {
        let rom = cart::normalize_rom(rom);
        let header = cart::parse_header(&rom)?;
        let region = config.resolve_region(&header);
        let mut bus = Bus::new(rom, region.version_register(config.tmss));
        // A VDP that has never been talked to sits at line 0, which is a line of
        // active display: leave it there and the program below starts 40 master
        // clocks into line 0, past the point that line is drawn from, and its whole
        // start-up register set is a frame late (#310). Blank first, then run.
        bus.vdp.start_in_vblank();
        let mut cpu = M68k::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu,
            z80: Z80::new(),
            bus,
            scheduler: Scheduler::new(region),
            mixer: Mixer::new(region.master_clock_hz(), &config),
        })
    }

    /// The console's reset button.
    ///
    /// **What it clears, and what it does not** - which is what the ticket asks
    /// to verify, recorded here because the answer is not "everything":
    ///
    /// * The 68000 **is** reset: [`m68k::M68k::reset`] re-reads `SSP` from
    ///   `$000000` and `PC` from `$000004` and re-enters supervisor mode with
    ///   the interrupt mask at 7 (M68000UM section 5 "Reset"). This is why a
    ///   game comes back to its own start-up code rather than to where it was.
    /// * The sound CPU **is** reset, by the same line that resets it in play:
    ///   [`Bus::reset_devices`] asserts its reset and drops its bus grant, so
    ///   it stops where it is and the 68k's own release edge - which the
    ///   scheduler answers with [`z80::Z80::reset`] - starts it again from
    ///   `$0000` (plutiedev, "Using the Z80"). Nothing here resets the chip
    ///   directly, because on this machine the line is the only thing that ever
    ///   does and taking it twice would be a second, silent reset.
    /// * The VDP **is** reset, with everything else behind the bus, and its
    ///   memories go with it: [`Bus::reset_devices`] rebuilds it, which M3-09's
    ///   ticket settled for the console's own reset and which is documented
    ///   there with the references (Genesis Plus GX's `gen_reset` reaching
    ///   `VDP_reset`, MAME's `machine_reset`).
    /// * The two working RAMs and the save are **not** cleared. Work RAM and
    ///   sound RAM are held on to by [`Bus::reset_devices`] - a reset is not a
    ///   power cycle, and a game that leaves variables in RAM across one is
    ///   relying on it - and neither the cartridge's save RAM nor the serial
    ///   EEPROM is touched, because the battery is still holding them.
    ///
    /// `TMSS` re-locks, the I/O chip goes back to its power-on registers with
    /// any pad still attached, and the mapper's bank registers return to the
    /// pages the image starts on. All of that is [`Bus::reset_devices`]'
    /// business and documented there.
    ///
    /// The VDP that reset rebuilds comes back where [`Genesis::new`] leaves a fresh
    /// one, on the first line of VBlank - [`Bus::reset_devices`] parks it there - so
    /// the register set the restarted program writes from its first instruction is in
    /// time for line 0 of the first frame after the reset (#310).
    pub fn reset(&mut self) {
        self.bus.reset_devices();
        self.scheduler.restart_frame();
        self.cpu.reset(&mut self.bus);
    }

    /// Run one frame: the two CPUs and the VDP, for one whole raster of the
    /// console's region, until the beam comes back to the blanking it started in.
    ///
    /// The VDP says when a frame is drawn at VBlank start, 224 lines into a 262
    /// line NTSC frame, and [`Scheduler::run_frame`] takes that as the signal to
    /// stop waiting and pays for the rest of the raster as well so that one call is
    /// exactly one frame and one `frame_done` however far the beam had to go to
    /// raise it. On this machine the two are the same moment: the console starts its
    /// rasters in blanking (#310), so the wait is the whole 262 lines, the drain has
    /// nothing left to pay for, and docs/ARCHITECTURE.md's "runs until the next
    /// VBlank start" is the end of the call as well as the wait. The two are held
    /// together by [`Scheduler::run_frame`] and [`Scheduler::restart_frame`].
    pub fn run_frame(&mut self) {
        self.scheduler
            .run_frame(&mut self.cpu, &mut self.z80, &mut self.bus);
    }

    /// The picture the VDP has produced: 256x224 or 320x224 for the display the
    /// game asked for, RGBA8888 as `0xAABBGGRR`.
    pub fn frame(&self) -> &vdp::FrameBuffer {
        self.bus.vdp.frame()
    }

    /// Append the console's audio since the last call to `out`, interleaved
    /// stereo `i16` at `config.sample_rate` (docs/ARCHITECTURE.md, "genesis").
    ///
    /// What it mixes is everything the chips have made that has not been handed
    /// out yet - what [`crate::Scheduler::run_frame`] ran them to - and so the
    /// number of output frames is whatever that much master clock is worth at
    /// `sample_rate`: about 801 for a 60 Hz NTSC raster and about 767 for a PAL
    /// one at 48 kHz, rather than a number the caller can predict from the rate
    /// alone. That is how `sample_rate` stays exactly `sample_rate` over an
    /// afternoon instead of drifting by the fraction a per-frame rounding throws
    /// away ([`crate::audio::Mixer`] is where that is done, and
    /// `audio::tests::output_count_exact_over_one_second` holds it).
    ///
    /// A frame's sound is handed out once: calling this again after one
    /// [`Genesis::run_frame`] appends the next frame's worth, and calling it
    /// again without one appends nothing. `out` is only ever appended to, so a
    /// caller that wants a fresh buffer passes an empty one, and one that wants
    /// to keep its own capacity does what any `Vec` user does and reserves.
    pub fn drain_audio(&mut self, out: &mut Vec<i16>) {
        let frame = self.bus.audio.frame();
        self.mixer.mix(&frame, out);
    }

    /// Say what the pad on port `port` is reporting: [`Buttons`], one bit per
    /// button (docs/ARCHITECTURE.md, "Input").
    ///
    /// Attaches a 3-button pad, which is what the I/O chip's own front-end
    /// entry point attaches; see [`Io::set_buttons`] for the pad, and
    /// [`Io::set_buttons_6`] for the 6-button one a front end that has a
    /// setting for it uses instead.
    ///
    /// # Panics
    ///
    /// If `port` is not one of the console's three. [`Io::set_buttons`] is the
    /// one that says so, and a caller with no pad to attach has no port either.
    pub fn set_pad(&mut self, port: usize, buttons: Buttons) {
        self.bus.io.set_buttons(port, buttons);
    }

    /// The cart's battery-backed save, or `None` for a cart that has none.
    ///
    /// The bytes a front end persists; [`Genesis::load_sram`] puts them back.
    /// A save is not a save state - it is the cartridge's own RAM, and it
    /// survives a reset, a reload and a different emulator.
    pub fn sram(&self) -> Option<&[u8]> {
        self.bus.sram()
    }

    /// Put `data` back as the cart's save, where `data` is what [`Genesis::sram`]
    /// gave for this cart.
    ///
    /// Anything else - another cart's save, a truncated one - is refused whole,
    /// and a cart with no save has nowhere for it to go; see [`Bus::load_sram`].
    pub fn load_sram(&mut self, data: &[u8]) {
        self.bus.load_sram(data);
    }

    /// The console as a save state: the versioned container of
    /// `docs/save-state-format.md`, ready to keep or to hand to
    /// [`Genesis::load_state`].
    ///
    /// The machine is everything the running game is - the 68000, the sound CPU,
    /// the bus with the VDP, the I/O chip, both working RAMs and the cartridge's
    /// save and bank state, and the scheduler's clock position - and it is in
    /// there even in the middle of a line, a FIFO or a DMA. Two things are not:
    ///
    /// * the **ROM**, which the header names by checksum instead of duplicating.
    ///   A state is therefore only ever loadable on the cartridge it was written
    ///   for, and [`Genesis::load_state`] answers anything else with
    ///   [`StateError::WrongRom`] rather than loading a save into the wrong game.
    /// * the **picture**, which the next [`Genesis::run_frame`] draws again; see
    ///   `vdp::frame_state` for why a save state does not carry 600 KiB of it.
    ///
    /// The audio mixer is the host's output path rather than the machine, so a
    /// state neither saves nor restores it: a state written at 44.1 kHz does not
    /// change the settings of a machine now running at 48.
    pub fn save_state(&self) -> Vec<u8> {
        state::encode(self)
    }

    /// Put the console back from a save state written by
    /// [`Genesis::save_state`] on this cartridge.
    ///
    /// The ROM has to be loaded already - a state carries no ROM bytes, only the
    /// checksum it was written against - and the version and the payload have to
    /// be this build's, or the state is refused with a [`StateError`] and
    /// `self` is left exactly as it was: every check is answered before any of
    /// the machine is replaced.
    ///
    /// The cartridge's battery-backed save is part of the state, because the
    /// game's idea of where it is is machine state like any other; a front end
    /// that also keeps [`Genesis::sram`] separately will find the two agree.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        state::decode(data, self)
    }

    /// The 68000, as the machine left it after the last [`Genesis::run_frame`].
    ///
    /// Read-only on purpose: the CPU only moves when the scheduler steps it, and
    /// [`Genesis::run_frame`] is the only thing that does.
    pub fn cpu(&self) -> &M68k {
        &self.cpu
    }

    /// The memory map, for the tests that have to look at work RAM to see what a
    /// program did, and for a front end that wants [`Bus::take_dirty`] to know
    /// when the cart's save is worth writing out.
    pub fn bus(&self) -> &Bus {
        &self.bus
    }
}
