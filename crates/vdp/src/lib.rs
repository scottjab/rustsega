//! Sega Genesis VDP (video display processor): VRAM/CRAM/VSRAM, registers, DMA, rendering.
//!
//! What lives here today is the state layer: the three memories, the 24
//! registers, the register writes, and the mode getters every later VDP ticket
//! reads, the two ports the 68k reaches them with (`ports`: control port
//! writes and data port writes and reads), the status register the control port
//! reads answer (`status`), the beam position and the HV counter port (`hv`),
//! and the three DMA modes, the 68k transfer, the fill and the copy (`dma`),
//! and the beam timing that drives the status flags and raises the interrupts
//! (`timing`). Rendering is M4-10 (#88) onwards, so the line renderer that
//! `timing` calls is the backdrop stub of M4-17 (#95).
//!
//! Register bit documentation cites Charles MacDonald's `genvdp.txt` by section
//! number and the plutiedev "VDP register reference" pages by the port address
//! of the register (`$83xx` is R3), which is how each is usually quoted.

mod dma;
mod framebuffer;
mod hv;
mod ports;
mod regs;
mod status;
mod timing;

pub use dma::DmaBus;
pub use framebuffer::FrameBuffer;
pub use regs::{DmaMode, HScrollMode, InterlaceMode};

/// VRAM: 64 KiB of 8-bit RAM (genvdp.txt section 8).
pub const VRAM_SIZE: usize = 0x1_0000;
/// CRAM: 64 words of colour RAM (genvdp.txt section 9).
pub const CRAM_WORDS: usize = 64;
/// VSRAM: 40 words of vertical scroll RAM (genvdp.txt section 10).
pub const VSRAM_WORDS: usize = 40;

/// Active pixels per line in H40 (40 cells).
pub const WIDTH_H40: usize = 320;
/// Active pixels per line in H32 (32 cells).
pub const WIDTH_H32: usize = 256;
/// Active lines per frame in V28 mode (28 cells, genvdp.txt section 17 "$01").
pub const LINES_V28: usize = 224;
/// Active lines per frame in V30 mode (30 cells, genvdp.txt section 17 "$01").
pub const LINES_V30: usize = 240;

/// The VDP: its memories, its registers, and the frame it is drawing.
///
/// `Debug` is for the `genesis` bus, which owns a `Vdp` and derives its own
/// (docs/ARCHITECTURE.md: "`genesis` ... Depends on all chip crates").
#[derive(Debug)]
pub struct Vdp {
    /// VRAM, 64 KiB, holding the pattern, name and sprite tables
    /// (genvdp.txt section 8).
    ///
    /// A `Box<[u8]>` rather than a `[u8; VRAM_SIZE]` field so that building a
    /// `Vdp` never puts 64 KiB on the caller's stack; the length is fixed at
    /// `VRAM_SIZE` by [`Vdp::new`] and nothing resizes it.
    ///
    /// The data port writes it (M4-02, #80, in `ports`) and reads it (M4-03,
    /// #81), big endian: the high byte of a word is at the even address.
    pub vram: Box<[u8]>,
    /// CRAM: 64 words of 9-bit colour in `0000 BBB0 GGG0 RRR0` form
    /// (genvdp.txt section 9: `----bbb-ggg-rrr-`).
    ///
    /// TODO(M4-09 #87): conversion of these words to DAC levels, including the
    /// shadow/highlight halves of the range.
    pub cram: [u16; CRAM_WORDS],
    /// VSRAM: 40 words of vertical scroll values (genvdp.txt section 10).
    ///
    /// The scroll value is 10 bits (`------yyyyyyyyyy`, genvdp.txt section 10)
    /// but the word is 11 bits wide, which is what interlace mode 2 uses, so
    /// the data port masks a write to 11 bits rather than 10 (`ports`, and
    /// SpritesMind "Interlace mode (LSMx=3)": "VSRAM width is 11-bit but bit0
    /// is ignored in interlaced mode 2").
    pub vsram: [u16; VSRAM_WORDS],
    /// R0 to R23, in the order the control port numbers them
    /// (genvdp.txt section 17).
    regs: [u8; regs::NUM_REGISTERS],
    /// The VDP code register, CD5-CD0: which memory a data port access is for
    /// and whether it reads or writes (genvdp.txt section 7 "VDP ports").
    /// Written by [`Vdp::write_control`], read by [`Vdp::write_data`]; the
    /// per-memory rules are in `ports`.
    code: u8,
    /// The VDP address register, A15-A0: where the data port points
    /// (genvdp.txt section 7). R15 moves it along after every data port access,
    /// see `ports::increment`.
    addr: u16,
    /// Set between the two halves of a control port command word: the VDP
    /// "maintains an internal write-pending flag", which the first half sets
    /// and the second half, a data port access, and a control port read clear
    /// (genvdp.txt section 7).
    pending: bool,
    /// The last word written to the data port.
    ///
    /// The read side answers the bits the memory it read does not drive from
    /// this, and the whole word when the code register holds a write code
    /// ([`Vdp::read_data`], M4-03 #81).
    ///
    /// A stand-in for the VDP's four-entry FIFO, which is what supplies those
    /// bits on real hardware: unlike the FIFO this has no depth and no timing.
    /// TODO(M4-21a #99): the FIFO itself, with the read prefetch that feeds it.
    fifo_last: u16,
    /// Set by a command word with CD5 while R1 M1 allows DMA (genvdp.txt
    /// section 7: "Setting CD5 will trigger a DMA operation"). [`Vdp::run_dma`]
    /// consumes it, and each mode of genvdp.txt section 11 takes its turn there:
    /// the 68k transfer and the copy run, and the fill goes to
    /// [`Vdp::dma_fill_pending`] because it waits for a data port write.
    dma_requested: bool,
    /// A DMA fill that is waiting for the data port write which starts it.
    ///
    /// Set when a command word with CD5 asks for the fill mode (R23 bits 7-6
    /// `10`) and cleared by [`Vdp::run_fill`] (`dma`) when the next word
    /// reaches the data port: "Any write to the data port will then start a
    /// VRAM fill" (genvdp.txt section 11 "VRAM fill").
    dma_fill_pending: bool,
    /// Status bit 7 (F): a vertical interrupt is waiting for the 68k.
    ///
    /// Set at VBlank start and cleared by the interrupt acknowledge, never by
    /// the status read that shows it (genvdp.txt section 6: "Bit 7 is set when a
    /// vertical interrupt occurs ... Reading the control port does not clear
    /// this bit"). `timing` is what sets it and [`Vdp::ack_irq`] what clears it.
    vint_pending: bool,
    /// A line interrupt is waiting for the 68k: the HINT counter expired on a
    /// line and nobody has acknowledged level 4 since.
    ///
    /// Set whatever R0 bit 4 (IE1) says, because genvdp.txt section 4 gates the
    /// interrupt on the bit rather than the counter running ("When the counter
    /// has expired, and if bit 4 (IE1) of register #0 is set, then a level 4
    /// interrupt will occur"), so the enable is read at [`Vdp::pending_irq`].
    /// There is no status bit for it: the 68k just gets the interrupt.
    hint_pending: bool,
    /// The line interrupt counter: decremented on every line of the active
    /// display and reloaded from R10 when it expires (genvdp.txt section 4
    /// "Line interrupts"; [`Vdp::hint_reload`]).
    hint_counter: u8,
    /// A frame has finished drawing and the scheduler has not been told yet.
    ///
    /// Set at VBlank start by `timing`, taken by [`Vdp::frame_done`].
    frame_ready: bool,
    /// Status bit 6 (SOVR): too many sprites on the scan line just drawn.
    ///
    /// "Set when too many sprites are on the current scan line, meaning when the
    /// VDP parses the 21st sprite in 40-cell mode or the 17th sprite in 32-cell
    /// mode from the sprite list" (genvdp.txt section 6). Cleared by the status
    /// read that reports it, see [`Vdp::read_status`].
    ///
    /// TODO(M4-16 #93): the sprite limit that sets it.
    sprite_overflow: bool,
    /// Status bit 5 (C): two sprites with non-transparent pixels overlapped.
    ///
    /// "Set when any sprites have non-transparent pixels overlapping one
    /// another. This may hold true for sprites outside of the display area too"
    /// (genvdp.txt section 6). Cleared by the status read, as bit 6 is.
    ///
    /// TODO(M4-16 #93): the collision detection that sets it.
    sprite_collision: bool,
    /// Status bit 4 (ODD): this frame is the odd one of an interlace pair.
    ///
    /// "Set when the display is interlaced, and in the odd frame, otherwise it
    /// is cleared in the even frame. This applies to both interlace modes"
    /// (genvdp.txt section 6). Clear always while the frame counter does not
    /// exist: TODO(M4-05 #83).
    odd_frame: bool,
    /// Status bit 3 (VB): the vertical blanking signal is live.
    ///
    /// "Set on line E0h, at H counter cycle AAh, and cleared on line FFh, at H
    /// counter cycle AAh" (genvdp.txt section 6). A switched-off display reads
    /// as blanking whatever this field says; `status` says where that comes from.
    ///
    /// Driven by `timing`, line-based for now: the H counter cycle of that rule
    /// is M4-23's.
    in_vblank: bool,
    /// Status bit 2 (HB): the horizontal blanking signal is live.
    ///
    /// "Set at H counter cycle E4h and cleared at H counter cycle 08h"
    /// (genvdp.txt section 6). Driven by `timing` at the two points of the line
    /// it names.
    in_hblank: bool,
    /// Status bit 1: a DMA is running. "Set for the duration of a DMA
    /// operation ... only useful for fills and copies, since the 68000 is frozen
    /// during 68k -> VDP transfers" (genvdp.txt section 6).
    ///
    /// Only a fill is ever seen busy, and only between the command word that
    /// arms it and the data port write that runs it (`dma::Vdp::arm_fill`,
    /// M4-08 #86). TODO(M4-22 #101): DMA timing, which is what would set and
    /// clear it for the transfers of M4-07 (#85) and the copies of #86 too -
    /// both of which run in one call and so finish before anything can read it.
    dma_busy: bool,
    /// The scanline the beam is on: 0 is the first line of the frame, and the
    /// frame is 262 lines on NTSC and 313 on PAL. [`Vdp::advance`] walks it and
    /// [`Vdp::set_position`] jumps it (M4-05 #83, M4-06 #84).
    line: u16,
    /// Master clocks into [`Vdp::line`], `0..hv::MCLKS_PER_LINE`.
    line_mclk: u32,
    /// Whether the once-a-line work of `timing::start_line` has run for
    /// [`Vdp::line`]: false for the line 0 of a VDP that has never been
    /// advanced, and cleared each time the beam moves to a new line (M4-06 #84).
    line_started: bool,
    /// The HV counter frozen by R0 bit 1 (M3), `None` while the bit is clear.
    ///
    /// `Some` from the moment M3 goes up to the moment it comes down, which is
    /// what [`Vdp::hv_counter`] answers with while it is set (M4-05, #83).
    hv_latch: Option<u16>,
    /// The frame the renderer draws into. Read through [`Vdp::frame`].
    frame_buffer: FrameBuffer,
    /// Which crystal the chip is clocked by: `true` for PAL, which can drive a
    /// 240-line display, `false` for NTSC (the `PAL` pin of the VDP,
    /// genvdp.txt section 18 "VDP Pinout"). `genesis` knows it from the console
    /// configuration.
    pal: bool,
}

impl Vdp {
    /// A VDP with zeroed memories and zeroed registers, clocked for `pal`.
    ///
    /// Zeroed registers mean H32, display off, no interrupts, 224 lines: the
    /// state of a VDP that has never been talked to. Real boot code writes a
    /// full register set before enabling the display.
    ///
    /// The 64 KiB of VRAM is the only allocation, and it is on the heap.
    pub fn new(pal: bool) -> Self {
        Self {
            vram: vec![0; VRAM_SIZE].into_boxed_slice(),
            cram: [0; CRAM_WORDS],
            vsram: [0; VSRAM_WORDS],
            regs: [0; regs::NUM_REGISTERS],
            code: 0,
            addr: 0,
            pending: false,
            fifo_last: 0,
            dma_requested: false,
            dma_fill_pending: false,
            vint_pending: false,
            sprite_overflow: false,
            sprite_collision: false,
            odd_frame: false,
            in_vblank: false,
            in_hblank: false,
            dma_busy: false,
            hint_pending: false,
            hint_counter: 0,
            frame_ready: false,
            line: 0,
            line_mclk: 0,
            line_started: false,
            hv_latch: None,
            frame_buffer: FrameBuffer::new(),
            pal,
        }
    }

    /// The frame the VDP has produced, as the frontend and the golden tests see
    /// it.
    ///
    /// TODO(M4-10 #88 onwards): the renderer needs to write it, which is when a
    /// mutable accessor appears here.
    #[inline]
    pub fn frame(&self) -> &FrameBuffer {
        &self.frame_buffer
    }

    /// Write `value` into register `reg`.
    ///
    /// Registers 24 to 31 do not exist and writes to them are ignored rather
    /// than panicking: the control port decode of `ports` hands this function
    /// the 5-bit field of a `100r rrrr` command word, which can name them
    ///
    /// R0 is the one register whose write does something outside the register
    /// file: bit 1 (M3) latching the HV counter as it goes up and releasing it
    /// as it comes down (M4-05, #83, and [`Vdp::hv_counter`]).
    pub fn write_register(&mut self, reg: u8, value: u8) {
        let index = usize::from(reg);
        let Some(previous) = self.regs.get(index).copied() else {
            return;
        };
        if index == regs::R0 {
            self.m3_written(previous, value);
        }
        self.regs[index] = value;
    }

    /// Raw read of register `reg`, for the code that wants a register rather
    /// than a decoded mode bit. Numbers past 23 read as 0, since there is
    /// nothing there to read (genvdp.txt section 17).
    #[inline]
    pub fn reg(&self, reg: u8) -> u8 {
        self.regs.get(usize::from(reg)).copied().unwrap_or(0)
    }

    /// R0 bit 5 (LCB): blank the leftmost 8 pixels of the display, filling them
    /// with the backdrop colour (genvdp.txt section 17 "$00"; plutiedev
    /// "$80xx").
    #[inline]
    pub fn left_column_blank(&self) -> bool {
        self.regs[regs::R0] & (1 << 5) != 0
    }

    /// R0 bit 4 (IE1): enable the HINT horizontal-blank interrupt
    /// (genvdp.txt section 4 "Interrupts"; plutiedev "$80xx").
    ///
    /// Read by [`Vdp::pending_irq`], which is the side that decides whether a
    /// pending HINT reaches the 68k.
    #[inline]
    pub fn hint_enabled(&self) -> bool {
        self.regs[regs::R0] & (1 << 4) != 0
    }

    /// R0 bit 1 (M3): latch and freeze the HV counter instead of letting it
    /// count (genvdp.txt section 4 "Interrupts" and section 17 "$00"; plutiedev
    /// "$80xx"). [`Vdp::hv_counter`] is the side that holds the value.
    ///
    /// Unlike the other bits of R0 this one is read by [`Vdp::hv_counter`] and
    /// by nothing else: it is not a display mode.
    #[inline]
    pub fn hv_counter_latch(&self) -> bool {
        self.regs[regs::R0] & (1 << 1) != 0
    }

    /// R0 bit 2: palette select, which of the two CRAM halves a shadow or
    /// highlight pixel uses. Software is told to leave it set (plutiedev
    /// "$80xx"; genvdp.txt section 16 "Shadow / Hilight mode").
    #[inline]
    pub fn palette_select(&self) -> bool {
        self.regs[regs::R0] & (1 << 2) != 0
    }

    /// R1 bit 7: the 128 KiB VRAM select.
    ///
    /// There is no 128 KiB mode here. Retail hardware does not have the second
    /// 64 KiB, and this emulator always has [`VRAM_SIZE`] bytes; the bit is
    /// exposed so the register state a game set up is still visible
    /// (genvdp.txt section 17 "$01"; plutiedev lists the bit as always 0).
    #[inline]
    pub fn vram_128k_enabled(&self) -> bool {
        self.regs[regs::R1] & (1 << 7) != 0
    }

    /// R1 bit 6 (DISP): the display is drawn. When clear every line shows the
    /// backdrop colour and the memories can be written freely (genvdp.txt
    /// section 17 "$01"; plutiedev "$81xx").
    #[inline]
    pub fn display_enabled(&self) -> bool {
        self.regs[regs::R1] & (1 << 6) != 0
    }

    /// R1 bit 5 (IE0): enable the VINT vertical-blank interrupt (genvdp.txt
    /// section 4 "Interrupts"; plutiedev "$81xx").
    ///
    /// Read by [`Vdp::pending_irq`], so a VINT that was masked when it happened
    /// is delivered as soon as this goes up.
    #[inline]
    pub fn vint_enabled(&self) -> bool {
        self.regs[regs::R1] & (1 << 5) != 0
    }

    /// R1 bit 4 (M1): DMA is enabled. A DMA command word does nothing while
    /// this is clear (genvdp.txt section 11 "DMA"; plutiedev "$81xx").
    #[inline]
    pub fn dma_enabled(&self) -> bool {
        self.regs[regs::R1] & (1 << 4) != 0
    }

    /// R1 bit 3 (M2): V30 mode, a 30-cell (240-line) vertical resolution.
    /// Clear is V28, 28 cells (genvdp.txt section 17 "$01": "Bit 3 will select
    /// between a PAL (240) and NTSC (224 lines) display"; plutiedev "$81xx":
    /// "0: 28 tiles, 1: 30 tiles. Note that V30 mode only works properly on PAL
    /// systems").
    #[inline]
    pub fn v30_mode(&self) -> bool {
        self.regs[regs::R1] & (1 << 3) != 0
    }

    /// R1 bit 2 (M5): set for the Genesis's own mode 5, clear for the Master
    /// System's mode 4 (genvdp.txt section 17 "$01"; plutiedev "$81xx").
    ///
    /// Mode 4 is out of scope for this emulator, so nothing branches on this
    /// yet; it is exposed so a ROM that clears it can be recognised.
    #[inline]
    pub fn mode5_selected(&self) -> bool {
        self.regs[regs::R1] & (1 << 2) != 0
    }

    /// Plane A name table base, from R2 bits 5-3 (16 KiB steps; genvdp.txt
    /// section 17 "$02", plutiedev "$82xx").
    #[inline]
    pub fn plane_a_base(&self) -> u16 {
        regs::table_base(self.regs[regs::R2], 0x38, 10)
    }

    /// Window name table base, from R3 bits 5-1 (2 KiB steps; genvdp.txt
    /// section 17 "$03", plutiedev "$83xx").
    ///
    /// In H40 the hardware forces address bit A11 to zero, so R3 bit 1 is not
    /// part of the base there (genvdp.txt: "In 40-cell mode, A11 is always
    /// forced to zero"). That is why this reads R12 through [`Vdp::h40`] rather
    /// than being a pure function of R3: the mask has to be whatever H40 is
    /// *now*, and a mode change mid-frame is legal.
    #[inline]
    pub fn window_base(&self) -> u16 {
        let mask = if self.h40() { 0x3C } else { 0x3E };
        regs::table_base(self.regs[regs::R3], mask, 10)
    }

    /// Plane B name table base, from R4 bits 2-0 (16 KiB steps; genvdp.txt
    /// section 17 "$04", plutiedev "$84xx").
    #[inline]
    pub fn plane_b_base(&self) -> u16 {
        regs::table_base(self.regs[regs::R4], 0x07, 13)
    }

    /// Sprite attribute table base, from R5 bits 6-0 (512 byte steps; genvdp.txt
    /// section 17 "$05", plutiedev "$85xx").
    ///
    /// In H40 address bit A9 is forced to zero, so R5 bit 0 is not part of the
    /// base there. As with [`Vdp::window_base`], that makes R12 part of the
    /// answer and rules out caching it.
    #[inline]
    pub fn sprite_base(&self) -> u16 {
        let mask = if self.h40() { 0x7E } else { 0x7F };
        regs::table_base(self.regs[regs::R5], mask, 9)
    }

    /// R7 bits 5-4: palette the backdrop colour is read from (genvdp.txt
    /// section 17 "$07"; plutiedev "$87xx", "CPT1-0").
    #[inline]
    pub fn backdrop_palette(&self) -> u8 {
        (self.regs[regs::R7] >> 4) & 0x03
    }

    /// R7 bits 3-0 combined with the palette bits: the CRAM index of the
    /// backdrop colour, which is also the index used for every pixel drawn over
    /// nothing (genvdp.txt section 17 "$07": "Bits 5-0 of this register select a
    /// palette entry to be used as the backdrop color").
    #[inline]
    pub fn backdrop_index(&self) -> u8 {
        self.regs[regs::R7] & 0x3F
    }

    /// R10: value reloaded into the HINT counter after every HINT
    /// (genvdp.txt section 4 "Interrupts", section 17 "$0A"; plutiedev
    /// "$8Axx"). The counter it is loaded into is `timing`'s.
    #[inline]
    pub fn hint_reload(&self) -> u8 {
        self.regs[regs::R10]
    }

    /// R11 bit 3 (IE2): enable the external interrupt driven by the TH pin.
    /// That pin belongs to the console I/O registers, not the VDP, so nothing
    /// here uses it yet (genvdp.txt section 17 "$0B"; plutiedev "$8Bxx").
    #[inline]
    pub fn ext_int_enabled(&self) -> bool {
        self.regs[regs::R11] & (1 << 3) != 0
    }

    /// R11 bit 2 (VSCR): vertical scrolling is per 2-cell column instead of
    /// whole-screen, reading VSRAM (genvdp.txt section 17 "$0B"; plutiedev
    /// "$8Bxx").
    #[inline]
    pub fn vscroll_per_cell_column(&self) -> bool {
        self.regs[regs::R11] & (1 << 2) != 0
    }

    /// R11 bits 1-0 (HSCR/LSCR): how the horizontal scroll table is walked
    /// (plutiedev "$8Bxx"; genvdp.txt section 17 "$0B" lists the same four
    /// settings under the two bit names).
    #[inline]
    pub fn hscroll_mode(&self) -> HScrollMode {
        regs::decode_hscroll_mode(self.regs[regs::R11])
    }

    /// H40 mode: the display is 40 cells wide.
    ///
    /// R12 bits 7 (RS0) and 0 (RS1) pick the horizontal resolution: `00` is the
    /// 32-cell display and `11` the 40-cell one. `01` is a real if ugly 40-cell
    /// setting (genvdp.txt section 17 "$0C" names the unlicensed Populous as
    /// using it) and `10` is invalid, so any set bit counts as H40 (plutiedev
    /// "$8Cxx").
    #[inline]
    pub fn h40(&self) -> bool {
        self.regs[regs::R12] & regs::R12_RES_MASK != 0
    }

    /// R12 bit 3 (S/TE): shadow/highlight mode enabled, which is what makes the
    /// half-brightness and double-brightness CRAM ranges live (genvdp.txt
    /// section 16 "Shadow / Hilight mode"; plutiedev "$8Cxx").
    #[inline]
    pub fn shadow_highlight_enabled(&self) -> bool {
        self.regs[regs::R12] & (1 << 3) != 0
    }

    /// R12 bits 2-1 (LSM): the interlace mode (genvdp.txt section 17 "$0C";
    /// plutiedev "$8Cxx"). `10` is documented as "no interlace" and decodes to
    /// [`InterlaceMode::Off`].
    ///
    /// TODO(M4-20 #98): what the modes do to the display, including the 480
    /// lines of [`InterlaceMode::Mode2`].
    #[inline]
    pub fn interlace_mode(&self) -> InterlaceMode {
        regs::decode_interlace_mode(self.regs[regs::R12])
    }

    /// Horizontal scroll table base, from R13 bits 5-0 (1 KiB steps; genvdp.txt
    /// section 17 "$0D", plutiedev "$8Dxx").
    #[inline]
    pub fn hscroll_base(&self) -> u16 {
        regs::table_base(self.regs[regs::R13], 0x3F, 10)
    }

    /// R15: value added to the VDP address register after every data port
    /// access (genvdp.txt section 17 "$0F", section 7 "VDP ports"; plutiedev
    /// "$8Fxx").
    ///
    /// Applied by `ports::increment`, which is where the width of the field
    /// and the wrap of the address register are settled.
    #[inline]
    pub fn auto_increment(&self) -> u8 {
        self.regs[regs::R15]
    }

    /// True when a control port command word asked for a DMA and nobody has
    /// run it yet (genvdp.txt section 7: "Setting CD5 will trigger a DMA
    /// operation"). [`Vdp::run_dma`] (`dma`) is what runs it and clears this.
    #[inline]
    pub fn dma_requested(&self) -> bool {
        self.dma_requested
    }

    /// Plane A and B name table width in cells, from R16 bits 1-0 (HSZ;
    /// genvdp.txt section 17 "$10", plutiedev "$90xx"). See
    /// [`regs::plane_extent`] for the invalid `10`.
    #[inline]
    pub fn plane_width_cells(&self) -> u8 {
        regs::plane_extent(self.regs[regs::R16] & 0x03)
    }

    /// Plane A and B name table height in cells, from R16 bits 5-4 (VSZ;
    /// genvdp.txt section 17 "$10", plutiedev "$90xx").
    #[inline]
    pub fn plane_height_cells(&self) -> u8 {
        regs::plane_extent(self.regs[regs::R16] >> 4)
    }

    /// R17 bit 7 (RIGT): the window is drawn to the right of the split when set,
    /// to the left when clear (genvdp.txt section 17 "$11"; plutiedev "$91xx").
    #[inline]
    pub fn window_right(&self) -> bool {
        self.regs[regs::R17] & (1 << 7) != 0
    }

    /// R17 bits 4-0 (WHP): how wide the window is, in 2-cell (16 pixel) units;
    /// zero means no window (genvdp.txt section 17 "$11"; plutiedev "$91xx").
    #[inline]
    pub fn window_h_position(&self) -> u8 {
        self.regs[regs::R17] & 0x1F
    }

    /// R18 bit 7 (DOWN): the window is drawn below the split when set, above it
    /// when clear (genvdp.txt section 17 "$12"; plutiedev "$92xx").
    #[inline]
    pub fn window_down(&self) -> bool {
        self.regs[regs::R18] & (1 << 7) != 0
    }

    /// R18 bits 4-0 (WVP): how high the window is, in cells (genvdp.txt
    /// section 17 "$12"; plutiedev "$92xx").
    #[inline]
    pub fn window_v_position(&self) -> u8 {
        self.regs[regs::R18] & 0x1F
    }

    /// DMA length, R19 low and R20 high (genvdp.txt section 11
    /// "DMA"; plutiedev "$93xx/$94xx").
    ///
    /// The raw field, which is either a count of words or a count of bytes: a
    /// 68k transfer counts 16-bit words (genvdp.txt "Registers 19, 20, specify
    /// how many 16-bit words to transfer") and a fill or a copy counts bytes
    /// ("... how many 8-bit bytes to fill", "... to copy"). `dma::words_to_transfer`
    /// and `dma::bytes_to_move` are where those two readings, and the rule that
    /// a field of 0 means 0x10000 of them, live.
    #[inline]
    pub fn dma_length(&self) -> u16 {
        u16::from(self.regs[regs::R19]) | (u16::from(self.regs[regs::R20]) << 8)
    }

    /// DMA source address, from R21 (source bits 8-1), R22 (bits 16-9) and R23
    /// bits 6-0 (bits 23-17) (plutiedev "$95xx/$96xx/$97xx"; genvdp.txt
    /// section 11).
    ///
    /// The top bit comes from R23 bit 6, which is also `DMD0`. That is not a
    /// mistake: in [`DmaMode::Mc68kTransfer`] the mode field is `0x`, so the
    /// bit is free and the hardware uses it as address bit 23 (plutiedev:
    /// "DMD0 becomes SA23").
    #[inline]
    pub fn dma_source(&self) -> u32 {
        (u32::from(self.regs[regs::R21]) << 1)
            | (u32::from(self.regs[regs::R22]) << 9)
            | ((u32::from(self.regs[regs::R23]) & 0x7F) << 17)
    }

    /// The VRAM source address of a copy: R21 bits 7-0 and R22 bits 15-8
    /// (genvdp.txt section 11 "VRAM copy": "#21: S07 S06 ... S00, #22: S15 S14
    /// ... S08", and "the address bits in register 23 are ignored").
    ///
    /// All 16 bits of the two registers are address here, unlike
    /// [`Vdp::dma_source`]: a 68k transfer reads words, so its R21 holds bits
    /// 8-1, while a copy reads bytes.
    #[inline]
    pub fn dma_copy_source(&self) -> u16 {
        u16::from(self.regs[regs::R21]) | (u16::from(self.regs[regs::R22]) << 8)
    }

    /// R23 bits 7-6 (DMD1-0): which DMA the command asked for (genvdp.txt
    /// section 11; plutiedev "$97xx").
    ///
    /// All three modes are in `dma`: the 68000 transfer runs from
    /// [`Vdp::run_dma`], the copy runs from there too, and the fill waits for
    /// the next data port write.
    #[inline]
    pub fn dma_mode(&self) -> DmaMode {
        regs::decode_dma_mode(self.regs[regs::R23])
    }

    /// Width of the active display in pixels: 320 in H40, 256 in H32
    /// (R12 RS0/RS1, [`Vdp::h40`]; genvdp.txt section 2 "Display Modes").
    #[inline]
    pub fn active_width(&self) -> usize {
        if self.h40() { WIDTH_H40 } else { WIDTH_H32 }
    }

    /// Height of the active display in lines.
    ///
    /// 240 lines only when the chip is clocked for PAL *and* R1 bit 3 puts it
    /// in V30 mode: on NTSC the frame has no room for the extra 16 lines, and
    /// V28 is 224 everywhere (genvdp.txt section 17 "$01"; plutiedev "$81xx":
    /// "V30 mode only works properly on PAL systems").
    ///
    /// TODO(M4-20 #98): interlace mode 2 draws 480 lines, which this does not
    /// report yet.
    #[inline]
    pub fn active_height(&self) -> usize {
        if self.pal && self.v30_mode() {
            LINES_V30
        } else {
            LINES_V28
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn new_zeroes_memories_and_registers() {
        let vdp = Vdp::new(false);
        assert_eq!(vdp.vram.len(), VRAM_SIZE);
        assert!(vdp.vram.iter().all(|&b| b == 0));
        assert!(vdp.cram.iter().all(|&w| w == 0));
        assert!(vdp.vsram.iter().all(|&w| w == 0));
        for reg in 0..=31 {
            assert_eq!(vdp.reg(reg), 0, "R{reg:02x} is not zeroed");
        }
    }

    #[test]
    fn vdp_values_stay_off_the_stack() {
        // The 64 KiB of VRAM is boxed, so a `Vdp` value is the boxed pointer
        // plus the fixed arrays. A `[u8; VRAM_SIZE]` field here would put 64 KiB
        // on the stack of every test that makes one, which is the thing
        // `Vdp::new` has to keep doing on the heap.
        assert!(
            size_of::<Vdp>() < 512,
            "Vdp is {} bytes, so VRAM is no longer boxed",
            size_of::<Vdp>()
        );
    }

    #[test]
    fn new_frame_buffer_is_320x224_with_room_for_320x480() {
        let vdp = Vdp::new(false);
        let frame = vdp.frame();
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 224);
        assert_eq!(frame.pixels.len(), 320 * 480);
        assert!(frame.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn write_register_one_sets_display_and_interrupt_bits() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0x64);
        assert!(vdp.display_enabled(), "R1 bit 6");
        assert!(vdp.vint_enabled(), "R1 bit 5");
        assert!(!vdp.dma_enabled(), "R1 bit 4 is clear in 0x64");
    }

    #[test]
    fn write_register_ignores_numbers_24_to_31() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(0, 0x02);
        vdp.write_register(23, 0x07);
        for reg in 24..=31 {
            vdp.write_register(reg, 0xFF);
        }
        assert_eq!(vdp.reg(0), 0x02);
        assert_eq!(vdp.reg(23), 0x07);
        for reg in 0..regs::NUM_REGISTERS as u8 {
            assert_ne!(
                vdp.reg(reg),
                0xFF,
                "R{reg:02x} was written by a write that should have been ignored"
            );
        }
    }

    #[test]
    fn h40_selects_the_active_width() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x81);
        assert!(vdp.h40());
        assert_eq!(vdp.active_width(), 320);

        vdp.write_register(12, 0);
        assert!(!vdp.h40());
        assert_eq!(vdp.active_width(), 256);
    }

    #[test]
    fn either_resolution_bit_means_h40() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(12, 0x01);
        assert!(vdp.h40(), "RS1 alone is a 40-cell display");
        vdp.write_register(12, 0x80);
        assert!(vdp.h40(), "RS0 alone is read as 40 cells too");
        vdp.write_register(12, 0x02);
        assert!(!vdp.h40(), "bit 1 is LSM0, not a resolution bit");
    }

    #[test]
    fn name_table_bases() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(2, 0x30);
        assert_eq!(vdp.plane_a_base(), 0xC000);
        vdp.write_register(4, 0x07);
        assert_eq!(vdp.plane_b_base(), 0xE000);
        vdp.write_register(3, 0x2C);
        assert_eq!(vdp.window_base(), 0xB000);
        vdp.write_register(5, 0x78);
        assert_eq!(vdp.sprite_base(), 0xF000);
        vdp.write_register(13, 0x3F);
        assert_eq!(vdp.hscroll_base(), 0xFC00);
    }

    #[test]
    fn h40_drops_a_base_bit_of_the_window_and_sprite_tables() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(3, 0x02);
        vdp.write_register(5, 0x79);

        assert_eq!(vdp.window_base(), 0x0800, "H32 uses R3 bit 1");
        assert_eq!(vdp.sprite_base(), 0xF200, "H32 uses R5 bit 0");

        vdp.write_register(12, 0x81);
        assert_eq!(vdp.window_base(), 0x0000, "H40 forces window A11 low");
        assert_eq!(vdp.sprite_base(), 0xF000, "H40 forces sprite A9 low");
    }

    #[test]
    fn plane_sizes_come_from_r16() {
        let mut vdp = Vdp::new(false);
        let cases: [(u8, u8, u8); 4] = [
            (0x01, 64, 32),
            (0x11, 64, 64),
            (0x03, 128, 32),
            (0x30, 32, 128),
        ];
        for (value, width, height) in cases {
            vdp.write_register(16, value);
            assert_eq!(
                (vdp.plane_width_cells(), vdp.plane_height_cells()),
                (width, height),
                "R16 = {value:#04x}"
            );
        }
    }

    #[test]
    fn backdrop_color_comes_from_r7() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(7, 0x2F);
        assert_eq!(vdp.backdrop_index(), 0x2F);
        assert_eq!(vdp.backdrop_palette(), 0b10);
    }

    #[test]
    fn active_height_is_240_only_on_pal_in_v30_mode() {
        assert_eq!(
            Vdp::new(false).active_height(),
            224,
            "NTSC has no 240-line display"
        );

        let mut pal = Vdp::new(true);
        assert_eq!(pal.active_height(), 224, "V28 until R1 bit 3 is set");
        pal.write_register(1, 0x08);
        assert!(pal.v30_mode());
        assert_eq!(pal.active_height(), 240);

        let mut ntsc = Vdp::new(false);
        ntsc.write_register(1, 0x08);
        assert_eq!(
            ntsc.active_height(),
            224,
            "a NTSC VDP cannot drive 240 lines"
        );
    }

    #[test]
    fn r0_mode_bits() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(0, 0b0011_0110);
        assert!(vdp.left_column_blank(), "bit 5");
        assert!(vdp.hint_enabled(), "bit 4");
        assert!(vdp.palette_select(), "bit 2");
        assert!(vdp.hv_counter_latch(), "bit 1");

        vdp.write_register(0, 0b1100_1001);
        assert!(!vdp.left_column_blank());
        assert!(!vdp.hint_enabled());
        assert!(!vdp.palette_select());
        assert!(!vdp.hv_counter_latch());
    }

    #[test]
    fn r1_mode_bits() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(1, 0b1111_1100);
        assert!(vdp.vram_128k_enabled(), "bit 7");
        assert!(vdp.display_enabled(), "bit 6");
        assert!(vdp.vint_enabled(), "bit 5");
        assert!(vdp.dma_enabled(), "bit 4");
        assert!(vdp.v30_mode(), "bit 3");
        assert!(vdp.mode5_selected(), "bit 2");

        vdp.write_register(1, 0b0000_0011);
        assert!(!vdp.vram_128k_enabled());
        assert!(!vdp.display_enabled());
        assert!(!vdp.vint_enabled());
        assert!(!vdp.dma_enabled());
        assert!(!vdp.v30_mode());
        assert!(!vdp.mode5_selected());
    }

    #[test]
    fn hint_reload_is_r10() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(10, 0x37);
        assert_eq!(vdp.hint_reload(), 0x37);
    }

    #[test]
    fn r11_scroll_and_external_interrupt_bits() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(11, 0b0000_1111);
        assert!(vdp.ext_int_enabled(), "bit 3");
        assert!(vdp.vscroll_per_cell_column(), "bit 2");
        assert_eq!(vdp.hscroll_mode(), HScrollMode::PerLine);

        vdp.write_register(11, 0b0000_0010);
        assert!(!vdp.ext_int_enabled());
        assert!(!vdp.vscroll_per_cell_column());
        assert_eq!(vdp.hscroll_mode(), HScrollMode::PerCell);
    }

    #[test]
    fn r12_shadow_and_interlace_bits() {
        let mut vdp = Vdp::new(false);
        assert_eq!(vdp.interlace_mode(), InterlaceMode::Off);
        vdp.write_register(12, 0x08);
        assert!(vdp.shadow_highlight_enabled(), "bit 3");
        vdp.write_register(12, 0x02);
        assert!(!vdp.shadow_highlight_enabled());
        assert_eq!(vdp.interlace_mode(), InterlaceMode::Mode1);
        vdp.write_register(12, 0x06);
        assert_eq!(vdp.interlace_mode(), InterlaceMode::Mode2);
    }

    #[test]
    fn auto_increment_is_r15() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(15, 0x80);
        assert_eq!(vdp.auto_increment(), 0x80);
    }

    #[test]
    fn window_position_registers() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(17, 0b1001_0011);
        assert!(vdp.window_right(), "R17 bit 7");
        assert_eq!(
            vdp.window_h_position(),
            0b001_0011,
            "R17 bits 4-0, in 2-cell units"
        );

        vdp.write_register(18, 0b1000_1111);
        assert!(vdp.window_down(), "R18 bit 7");
        assert_eq!(vdp.window_v_position(), 0x0F, "R18 bits 4-0, in cells");
    }

    #[test]
    fn dma_registers() {
        let mut vdp = Vdp::new(false);
        vdp.write_register(19, 0x34);
        vdp.write_register(20, 0x12);
        assert_eq!(vdp.dma_length(), 0x1234);

        vdp.write_register(21, 0x01);
        vdp.write_register(22, 0x02);
        vdp.write_register(23, 0b0011_1111);
        assert_eq!(vdp.dma_mode(), DmaMode::Mc68kTransfer);
        assert_eq!(vdp.dma_source(), 0x007E_0402);

        vdp.write_register(23, 0b0111_1111);
        assert_eq!(
            vdp.dma_source(),
            0x00FE_0402,
            "R23 bit 6 is the top source bit in 68k transfer mode"
        );

        vdp.write_register(23, 0b1000_0000);
        assert_eq!(vdp.dma_mode(), DmaMode::Fill);
        vdp.write_register(23, 0b1100_0000);
        assert_eq!(vdp.dma_mode(), DmaMode::Copy);
    }
}
