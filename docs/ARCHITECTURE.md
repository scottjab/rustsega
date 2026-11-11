# Architecture

This is the contract every ticket builds on. If a ticket and this doc disagree,
the ticket wins for that ticket, and the ticket should also update this doc.

## Goals

1. Accurate. Games should behave like they do on a real Model 1 / Model 2
   Genesis (NTSC and PAL). Accuracy is proven with tests, not eyeballing.
2. Runs in the browser at full speed (60 fps NTSC, 50 fps PAL) via WASM.
3. All app code is Rust. The browser UI is Rust (Leptos) compiled to WASM.
   The only JavaScript is what `wasm-bindgen` generates.
4. Deploys as one binary: `server` embeds the built frontend and serves it.
5. Playable with keyboard, gamepad, or touch.

## Workspace layout

```
crates/
  m68k/      Motorola 68000 core. No deps on other crates.
  z80/       Z80 core. No deps on other crates.
  vdp/       VDP: VRAM/CRAM/VSRAM, registers, DMA, rendering. No CPU deps.
  ym2612/    FM synth chip.
  sn76489/   PSG chip.
  genesis/   The console: bus, memory map, cartridge + mappers, controllers,
             scheduler, audio mixing, save states. Depends on all chip crates.
  testkit/   Test-only helpers (ROM discovery, hashing, golden files).
  headless/  CLI that runs a ROM without a browser. Used by integration tests.
  web/       Leptos CSR app, built with `trunk` to wasm.
  server/    axum server that embeds web/dist into the binary.
```

Rules:

- Chip crates (`m68k`, `z80`, `vdp`, `ym2612`, `sn76489`) never depend on each
  other or on `genesis`. They talk to the world through traits or plain
  method calls.
- Every crate except `web` and `server` must build for both the host and
  `wasm32-unknown-unknown`. No threads, no `std::time`, no filesystem in core
  crates. Only `testkit`, `headless`, `server` and tests may touch the
  filesystem.
- No `unsafe` (workspace lint forbids it).
- Core crates must not allocate per frame in hot paths. Allocate buffers once.
- State that must survive save states derives `serde::Serialize` and
  `serde::Deserialize` (added in the save-state milestone; keep fields plain
  data so this is easy: no `Box<dyn Trait>`, no references).

## Clocks and timing (NTSC numbers, PAL in parentheses)

- Master clock: 53_693_175 Hz (53_203_424 Hz).
- 68000: master / 7 (~7.67 MHz). Z80: master / 15 (~3.58 MHz).
- YM2612: one output sample every 144 68k cycles (master / 7 / 144, ~53267 Hz).
- PSG: master / 15 / 16 (~223722 Hz internal tick).
- One scanline = 3420 master clocks. 262 lines per frame (313 PAL).
- The scheduler in `genesis` runs line by line. Within a line it interleaves
  the 68000 and Z80 in small cycle slices, and fires HINT/VINT at the right
  master-clock position. Sub-line accuracy (FIFO stalls, mid-line register
  writes) comes later as separate tickets.

## Core APIs (initial shape, tickets may refine)

### m68k

```rust
pub trait Bus {
    fn read_byte(&mut self, addr: u32) -> u8;
    fn read_word(&mut self, addr: u32) -> u16;   // addr is even
    fn write_byte(&mut self, addr: u32, value: u8);
    fn write_word(&mut self, addr: u32, value: u16);
    /// Interrupt acknowledge. Return None for autovector (Genesis always autovectors).
    fn int_ack(&mut self, _level: u8) -> Option<u8> { None }
}

pub struct M68k {
    pub d: [u32; 8],
    pub a: [u32; 8],      // a[7] is the active stack pointer
    pub usp: u32,         // inactive stack pointer storage
    pub ssp: u32,
    pub pc: u32,
    pub sr: u16,
    pub prefetch: [u16; 2],  // IRC/IRD model, needed to pass SingleStepTests
    pub ipl: u8,          // pending interrupt level from the bus, 0..=7
    pub stopped: bool,
    pub halted: bool,
    // ...
}

impl M68k {
    pub fn new() -> Self;
    pub fn reset(&mut self, bus: &mut impl Bus);
    /// Run one instruction (or exception). Returns cycles used (68k clocks).
    pub fn step(&mut self, bus: &mut impl Bus) -> u32;
    pub fn set_ipl(&mut self, level: u8);
}
```

Addresses are 24 bit; the core masks with `0x00FF_FFFF` before calling the bus.

### z80

```rust
pub trait Z80Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn io_read(&mut self, port: u16) -> u8;
    fn io_write(&mut self, port: u16, value: u8);
}

pub struct Z80 { /* af, bc, de, hl, shadows, ix, iy, sp, pc, i, r, iff1, iff2, im, halted, wz (memptr), q ... */ }

impl Z80 {
    pub fn new() -> Self;
    pub fn reset(&mut self);
    pub fn step(&mut self, bus: &mut impl Z80Bus) -> u32; // T-states
    pub fn set_int_line(&mut self, asserted: bool);
    pub fn nmi(&mut self);
}
```

### vdp

```rust
pub struct Vdp { /* vram: [u8; 0x10000], cram: [u16; 64], vsram: [u16; 40], regs: [u8; 24], ... */ }

pub trait DmaBus { fn read_word(&mut self, addr: u32) -> u16; } // 68k-side memory for DMA

impl Vdp {
    pub fn new(pal: bool) -> Self;
    pub fn write_data(&mut self, value: u16);
    pub fn read_data(&mut self) -> u16;
    pub fn write_control(&mut self, value: u16);
    pub fn read_status(&mut self) -> u16;
    pub fn hv_counter(&self) -> u16;
    /// After every control port write the bus glue calls this to run any DMA
    /// the write requested (write_control only flags it).
    pub fn run_dma(&mut self, bus: &mut impl DmaBus);
    /// Advance the VDP by master clocks. Renders lines and raises IRQs as it goes.
    pub fn advance(&mut self, mclk: u32);
    pub fn pending_irq(&self) -> Option<u8>; // 6 = VINT, 4 = HINT
    pub fn ack_irq(&mut self, level: u8);
    pub fn z80_int_line(&self) -> bool;
    pub fn frame_done(&mut self) -> bool;    // true once per frame, at VBlank start
    pub fn dma_68k_stall_mclk(&mut self) -> u32;
    /// Render one line directly. Public for tests; the scheduler uses advance().
    pub fn render_line(&mut self, line: u16);
    pub fn frame(&self) -> &FrameBuffer;
}

pub struct FrameBuffer {
    pub width: usize,   // 256 or 320 (active area for this frame)
    pub height: usize,  // 224 or 240 (448/480 in interlace mode 2)
    pub pixels: Vec<u32>, // fixed capacity 320*480, RGBA8888 little endian (0xAABBGGRR as u32)
}
```

### Audio chips

```rust
impl Sn76489 { pub fn new() -> Self; pub fn write(&mut self, value: u8); pub fn clock(&mut self) -> i16; }
impl Ym2612  { pub fn new() -> Self; pub fn write(&mut self, port: u8, value: u8); pub fn read_status(&self) -> u8;
               pub fn clock(&mut self) -> (i16, i16); }
```

`genesis` owns mixing: it clocks each chip at its native rate, then
resamples to the host rate (48 kHz default) with a band-limited resampler,
and applies the Model 1 low-pass filter.

### genesis

```rust
pub struct Genesis { /* cpu, z80, vdp, ym, psg, cart, ram, io ... */ }

impl Genesis {
    pub fn new(rom: &[u8], config: Config) -> Result<Self, LoadError>;
    pub fn reset(&mut self);                 // soft reset (reset button)
    pub fn run_frame(&mut self);             // runs one whole frame: until the beam is back at the top of the screen
    pub fn frame(&self) -> &FrameBuffer;
    pub fn drain_audio(&mut self, out: &mut Vec<i16>); // interleaved stereo at config.sample_rate
    pub fn set_pad(&mut self, port: usize, buttons: Buttons); // bitflags: UP DOWN LEFT RIGHT A B C START X Y Z MODE
    pub fn save_state(&self) -> Result<Vec<u8>, StateError>;
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), StateError>;
    pub fn sram(&self) -> Option<&[u8]>;
    pub fn load_sram(&mut self, data: &[u8]);
}
```

`run_frame` used to be described as running "until the next VBlank start", and the
VDP does raise its frame flag there - line 224 of a 262 line NTSC frame. It runs
the rest of the raster as well, so that one call is one frame and takes exactly
one of those flags with it; ending at VBlank start would make the first call 224
lines and every later one 262. See `Scheduler::run_frame`.

`save_state` returns a `Result` because saying "no save-state format in this
build" is not something a `Vec<u8>` can say (M6-07 #127 fills it in).

### web

- Leptos (CSR) app built with `trunk build --release` from `crates/web`.
- Emulation runs on the main thread, driven by `requestAnimationFrame`, paced
  by audio time (the audio clock is the master; video drops or repeats a
  frame if needed).
- Video: canvas, `putImageData` of the frame buffer first, WebGL later for
  scaling/CRT filter.
- Audio: Web Audio API through `web-sys`, scheduling `AudioBufferSourceNode`s
  ahead of the play head. No hand written JS files.
- Input: one `InputState` that merges keyboard, Gamepad API, and touch
  overlay into `Buttons` per port. All bindings are rebindable and stored in
  `localStorage`.
- Persistence: SRAM and save states in IndexedDB (via the `rexie` or
  `idb` crate, or raw `web-sys`), keyed by ROM SHA-1.
- ROMs are loaded by file picker / drag and drop, or from the server's
  optional ROM library. ROMs never get bundled into the binary.

### server

- `axum` + `tokio`. Embeds `crates/web/dist` at compile time (`rust-embed`
  or `include_dir`). Serves it with correct MIME types (`application/wasm`)
  and compression.
- Optional `--rom-dir <path>`: exposes `GET /api/roms` (list) and
  `GET /api/roms/{id}` (bytes) so the UI can show a library.
- `nix build` produces the single binary (frontend built in the same
  derivation).

## Reference material

Use these. Do not copy code from GPL emulators (Genesis Plus GX, BlastEm,
MAME parts) verbatim; reading them to understand behavior is fine.

- 68000: Motorola "M68000 Family Programmer's Reference Manual" (M68000PM/AD),
  "M68000 8-/16-/32-Bit Microprocessors User's Manual" (M68000UM/AD, has
  cycle timing tables), yacht.txt (68000 bus cycle timing per instruction).
- Z80: Zilog "Z80 CPU User Manual" (UM0080), "The Undocumented Z80
  Documented" by Sean Young.
- Genesis: https://plutiedev.com, https://segaretro.org (hardware pages),
  Charles MacDonald's `genvdp.txt` (VDP), Sega "Genesis Software Manual",
  SpritesMind forum (Nemesis's VDP and YM2612 research threads).
- YM2612: Nemesis's YM2612 research on SpritesMind, the YM2608 (OPNA)
  application manual for the shared OPN core.
