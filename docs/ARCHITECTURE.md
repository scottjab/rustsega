# Architecture

This is the contract every ticket builds on. If a ticket and this doc disagree,
the ticket wins for that ticket, and the ticket should also update this doc.

## Goals

1. Accurate. Games should behave like they do on a real Model 1 / Model 2
   Genesis (NTSC and PAL). Accuracy is proven with tests, not eyeballing.
2. Runs in the browser at full speed (60 fps NTSC, 50 fps PAL) via WASM.
3. All app code is Rust. The browser UI is Rust (Leptos) compiled to WASM.
   The only JavaScript is what `wasm-bindgen` generates. One exception, kept
   as small as it can be: the AudioWorklet processor of the Safari audio ring
   (#629), about 60 lines of `inline_js` in `crates/web/src/audio_ring.rs`.
   An audio-thread processor cannot be written in wasm-bindgen Rust, and
   wasm-bindgen ships it as one of its own snippets, so it is still served
   from the app's origin under `script-src 'self'`.
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
- YM2612 timers tick in the same samples: timer A once per output sample (18.77
  us NTSC) and timer B once per 16 samples; BUSY holds 32 internal cycles (192
  chip clocks, 25 us) after each data write. Measured against the die shot in
  Nuked-OPN2 and the YM2608 manual's 1:16 ratio (#325); the crates/ym2612 module
  note carries the citations.
- PSG: master / 15 / 16 (~223722 Hz internal tick).
- One scanline = 3420 master clocks. 262 lines per frame (313 PAL).
- The scheduler in `genesis` runs line by line. Within a line it interleaves
  the 68000 and Z80 in small cycle slices, and fires HINT/VINT at the right
  master-clock position. Sub-line accuracy (the FIFO stall of `Vdp`, mid-line
  register writes) comes later as separate tickets: the VDP has reported its
  side of the write FIFO since M4-21a (#99), and `genesis` has to act on it.

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

#### m68k prefetch queue: two slots and the in-flight tail

`prefetch` is a two-word window, `[word(pc), word(pc + 2)]`, with `pc` the
address of the front (the instruction being executed). `pop_word` is the one
way a word leaves it: the back word comes out, `pc` slides +2, and a refill
fetch at the old `pc + 4` charges 4 clocks. Every extension word and the
end-of-instruction slide (`advance_opcode`) are pops, so an instruction's
cycle count is its pop count; `jump_to` throws the queue away and refills.

The recorded 68000 (the SingleStepTests recorder) adds one asymmetry the
queue holds in flight: a **read** leaves the operand's last extension pop
fully paid, but a **store** takes the bus slot first, so the last extension
word's slide and refill wait behind the store cycle. A completing store lets
them land before the advance (non-faulted totals unchanged); a faulting store
cancels both, which is why a faulted `(xxx).l` store stacks a `pc` one word
short of its instruction and charges 4 fewer clocks (#616; suite cases `33ea
[MOVE.w (d16, A2), (xxx).l] 18` and `33d1 [MOVE.w (A1), (xxx).l] 1826` pin
it). The move class resolves an `(xxx).l` destination through the deferred
read (`resolve_store_destination`) and lands it with `land_deferred_ext_word`
after the store; it only defers when the source was read off the bus - a
register or immediate source charges the tail in full (`33c1 [MOVE.w D1,
(xxx).l] 3196`). Read-modify-write instructions and MOVEM never defer: their
last pop is followed by a read or nothing, and the suite pins the full charge
there.

#### m68k exceptions: the vector map

Exception entry copies SR, sets S, pushes a 6-byte frame (SR word at SP, PC
long at SP+2), and fetches the handler long from `4 * vector`. The map, with
what pins each row (M68000UM Table 6-2, "Exception Vector Assignment"; the
six #436 black carts ship this exact table, with real handlers in the slots
they use and halt stubs in the ones they don't):

| vector | long at | raises |
|---|---|---|
| 0, 1 | `$00`, `$04` | reset SSP / PC |
| 2 | `$08` | bus error (not implemented) |
| 3 | `$0C` | address error (`address_error` module; the suite's recording core also sent illegal here — its bug, not the 68000's) |
| 4 | `$10` | illegal instruction, undefined encodings |
| 5 | `$14` | divide by zero |
| 6 | `$18` | CHK out of range |
| 7 | `$1C` | TRAPV taken |
| 8 | `$20` | privilege violation |
| 9 | `$24` | trace |
| 10 | `$28` | line-1010 emulator (A-line) |
| 11 | `$2C` | line-1111 emulator (F-line) |
| 24, 25–31 | `$60`, `$64`–`$7C` | spurious interrupt, level 1–7 autovectors |
| 32+n | `$80+4n` | TRAP #n |

The line-A/F rows are 68000 rows, not a 68010 addition — only Format Error
(14) is footnoted 68010-only in the UM's table, and Genesis carts count on
the rows existing: six carts execute line opcodes deliberately and put
compact VDP-write handlers in `$28`/`$2C` (#436, #482). What the 68010 adds
and this crate does not implement is mid-instruction restart and the
RTE-based emulation mode. The frame PC for illegal/line-A/F and privilege
violation is the faulting opcode's own address; the handler steps over it
(e.g. `ADDQ.L #2,$A(A7)` in the carts' handlers) before `RTE`.

Maskability is a property of the interrupt exception alone (M68000UM 6.2.5,
"Exception Processing Sequence": "For the reset and interrupt exceptions, the
interrupt priority mask is also updated appropriately" — the only mention of
the mask in the entry sequence). Illegal, line-A/F, TRAP, TRAPV, CHK and
divide-by-zero are taken whatever the SR interrupt mask holds, and leave it as
they found it; the UM's own grouping (6.2.3) puts trace, interrupt, privilege
and illegal in group 1 and the instruction traps in group 2, so "unmaskable"
spans both groups and it is wrong to gate any of them on `SR >> 8 & 7`. This
is load-bearing for the carts: Dyna Brothers 2's main loop does `ORI.W
#$700,SR` and then calls a 1111-format service routine expecting its `$2C`
handler to be entered anyway (#494), and it boots because it is.

The two interrupt lines the VDP holds are level signals with latched pending
flags, not pulses. Eke's hardware measurements (SpritesMind t=787, "VDP
Interrupts & HVC tests", Aug 2010): each flag is set at its trigger point -
the HINT flag at the R10 counter's overflow (H counter $A6 in H40), the VINT
flag at line $E0, H counter $02 - "as long as HINT pending flag is set and VDP
register $00 bit 4 is set, interrupt level 4 will be generated", and "the only
way for these two flags to be cleared is when the /INTACK signal is asserted",
not by register writes, status reads or a new frame. So a level that fires
while the 68000's mask is at or above it stays on the pins and is taken the
instant the mask drops; the practical shape for a handler is that an
`ORI #$400,(A7)`-style raise of the stacked mask suppresses the level until
something lowers the mask (a wrapper's `ANDI #$F8FF,(A7)`, or `STOP #$2000`'s
SR load), at which point the still-pending flag re-interrupts immediately.
`vdp::timing` implements this: `hint_step`/`take_vint` set the flags,
`pending_irq` holds the offer while flag && enable, and `ack_irq` is the only
clear. blastem's `vdp_int_ack` is the reference's version of the same rule.
HardBall III's loader (#557, #646) is the cart that lives on the held flag.

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
    /// Queue a data port write: it reaches VRAM/CRAM/VSRAM at the next external
    /// access slot, not in this call. See "VDP write FIFO" below.
    pub fn write_data(&mut self, value: u16);
    pub fn read_data(&mut self) -> u16;
    pub fn write_control(&mut self, value: u16);
    pub fn read_status(&mut self) -> u16;
    /// Master clocks the 68k is held for by a data port write, `0` when the
    /// write is taken straight away. The bus glue (M3-16) adds it to the cycle
    /// count of the write, the same way it adds `dma_68k_stall_mclk`.
    pub fn data_port_stall_mclk(&self) -> u32;
    pub fn hv_counter(&self) -> u16;
    /// After every control port write the bus glue calls this to start any DMA
    /// the write requested (write_control only flags it). Starting is not
    /// finishing: the transfer takes access slots, and `advance` spends them.
    pub fn run_dma(&mut self, bus: &mut impl DmaBus);
    /// Advance the VDP by master clocks. Renders lines and raises IRQs as it goes.
    pub fn advance(&mut self, mclk: u32);
    pub fn pending_irq(&self) -> Option<u8>; // 6 = VINT, 4 = HINT
    pub fn ack_irq(&mut self, level: u8);
    pub fn z80_int_line(&self) -> bool;
    pub fn frame_done(&mut self) -> bool;    // true once per frame, at VBlank start
    /// Master clocks the 68k is still frozen for by a 68k -> VDP DMA, `0` for a
    /// fill, a copy, and no DMA at all. The bus glue (M3-16) adds it to the
    /// cycle count of the command word that started the transfer.
    pub fn dma_68k_stall_mclk(&self) -> u32;
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

The VDP write FIFO (`crates/vdp/src/fifo.rs`, M4-21a #99) is what makes
`write_data` a queue rather than a write:

* A data port write goes onto a four-entry FIFO carrying the code register, the
  address register and the word. Address auto-increment happens at enqueue time,
  because the access the 68k made is the access that counted.
* An entry reaches its memory at an external access slot: two slots for a VRAM
  word, which is byte wide, one for a CRAM or VSRAM word.
* Status bit 9 is FIFO empty and bit 8 is FIFO full; both read 0 in the neutral
  state of one to three entries.
* A write that finds the FIFO full is not lost: it joins the queue behind the
  entries that filled it, and `data_port_stall_mclk` is how long the 68k is held
  for. That is the design chosen over `write_data` returning an indication,
  because the bus has to be able to act on it without the write being issued
  twice, and it matches the `dma_68k_stall_mclk` the same glue already reads.
  Keeping only the one write that a real chip could hold was #540: the second of
  the two writes a long makes at `$C00000` overwrote it, and the first went
  missing. Bit 8 reads against four entries, and `data_port_stall_mclk` is the
  wait until the queue is short enough for the write to fit among them - the
  charge for everything standing in front of it, not for the entry at the head
  alone, which is what let a cart run the queue 28 deep (#545). A queue deeper
  than four is still the bus running ahead of the transfer, not a deeper FIFO.
* A data port read drains the FIFO first, so a read never sees a location as it
  was before the write the 68k queued for it.
* The slots are the ones `crates/vdp/src/slots.rs` tabulates per mode and per
  part of the line (M4-21b #100): 18 of an active H40 line and 16 of an active
  H32 one, 205 and 167 of a blank line, and no wait at all while the display is
  off - which is why the pre-FIFO data port tests of M4-02 and M4-03 need no
  `advance` in them.
* A DMA write goes straight to memory rather than onto the FIFO, because a
  68k-to-VDP transfer is thousands of writes and the FIFO is four deep. It
  still costs slots, and they are the same slots: see "VDP DMA timing".
  In *order*, though, a transfer is a FIFO citizen: the DMA feeds the same
  write FIFO the data port does ("it will read a value from external memory
  using the DMA source address register and add it to the FIFO using the
  current command code and incremented command address registers", Nemesis on
  SpritesMind t=1291), so a transfer started while the port still has queued
  writes lets them land first, and its first source word then lands at the
  transfer's own start address with nothing lost - that is Nemesis's
  `TestFIFOFullDMATransfer` expected result, pinned by the dma.rs tests of
  #306, and `Vdp::start_running` keeps the order by draining the queue before
  the first transfer write. Their drain is part of the transfer's window
  (#563): "pending FIFO writes take priority" (same post), so on hardware the
  pushes cannot start behind a full queue and Nemesis's own hardware note is
  that "when a DMA transfer operation terminates, the FIFO is full of pending
  writes" - his freeze ends at the last push, with the last four words still
  in flight. This window is landing-based like the fill's and the copy's, so
  it charges the drain in full and stays a couple of slots long against that
  push reading by at most the last four words, uniform in the queue; that
  push-vs-landing corner is fenced with the status-bits-mid-transfer residue
  of #563. A fill waits the same way - its steps "will only ever run at an
  access slot if the FIFO is empty" (same post), and the write that triggers
  it is one of the entries it waits behind - while a copy has no such wait:
  Nemesis advances it with no FIFO condition at all.

### VDP DMA timing

`crates/vdp/src/dma.rs` (M4-07 #85, M4-08 #86) runs the three DMA modes over
the access slots of `slots.rs` rather than inside the call that started them
(M4-22, #101):

* `run_dma` and the data port write that arms a fill *start* a transfer. It is
  spent one unit per access slot by `advance`, so VRAM is partly written while a
  transfer runs, which games rely on.
* A unit is a word of a 68k transfer and a byte of a fill or a copy. It costs
  two slots for a VRAM word and a copied byte - VRAM is byte wide, and a copy
  reads as well as writes - and one for a CRAM or VSRAM word and a fill step.
  That is the Sega DMA bandwidth table out of the slot schedule: 18 bytes an
  active H40 line for a 68k transfer, 17 for a fill and 9 for a copy; 205, 204
  and 102 in blanking; and the H32 column of 16 / 15 / 8 and 167 / 166 / 83.
  A fill pays one slot of every line and moves nothing.
* A 68k transfer freezes the 68k for its whole length, and
  `dma_68k_stall_mclk` is how long: the time to the last of the transfer's
  slots. A transfer started behind the port's queued writes also pays their
  drain first (#563), two slots a queued VRAM word and one a CRAM or VSRAM
  word, on the same slot schedule. A fill and a copy hold the 68k off the
  data port instead, which is what status bit 1 says, and report no stall.
* A transfer's source words are read when it starts, because `advance` has no
  bus to read them with and the frozen 68k cannot change them meanwhile. That is
  a promise `DmaBus` carries: a read may not do anything the first of them would
  not have done.
* The length registers are spent, and a transfer's source registers moved on,
  when the last slot arrives rather than when the command word is written, and
  status bit 1 comes down with them.

### VDP per-line latching

`crates/vdp/src/latch.rs` (M4-23, #102) is what makes a raster effect written in
a HINT handler look right. A line is drawn from a snapshot of the state it needs,
taken once at the start of its active display - the H-blank release point
`timing` already stops the beam at, 81 master clocks in H40 and 100 in H32 - and
not from the chip as it stands when the line is drawn:

* The snapshot is the two horizontal scroll values of the line (read out of the
  scroll table at that moment, which is where hardware reads it), the whole of
  VSRAM and the whole register file. `advance` takes it; nothing else does.
* A register, VSRAM or scroll-table write that lands mid line therefore shows on
  the next line, and the frame the mode is asked for in is the frame before the
  one that shows it. `frame.width` is the mode of the frame's first line, so the
  latch of that one line fixes the geometry of the whole frame.
* The latch is a read-side view only: writes still reach the live registers and
  the live VSRAM, and `line_reg` / `latched_vsram` / `latched_hscroll` are the
  three places the renderer reads them. They answer live state outside
  `render_line`, so `render_plane_line` and a `render_line` call for a line the
  beam never latched work as they always did.
* The exact clock of the latch is INFERRED, with a TODO on `latch.rs` for the
  scroll ROM of M6-06. The sprite list rides the same point one line early: the
  search of line N's list runs at the latch point of line N - 1, and it keeps
  what it read (below), so the sprite work of a line is decided by the one
  moment, not spread across the line's draw.

### VDP sprite attribute cache

`crates/vdp/src/sprites.rs` (M4-23b, #103) keeps the VDP's own copy of the sprite
attribute table, because a sprite fetch cannot wait on the bus for all eight bytes
of every entry:

* 80 lines of four bytes, the `y` word and the `size`/`link` word of one entry
  each (the window is Nemesis's, SpritesMind t=851 note 7: "The first 4 bytes,
  containing the vertical position, size, and link data, are cached internally
  in the VDP"). The pattern and the `x` are not cached and are read from VRAM
  once a line: Nemesis's timing notes (t=851 note 5) read "all the mapping data
  for every sprite" of a scanline "during the previous line", and Mask of
  Destiny's VRAM-bus captures (t=1291) watched one second-half entry read per
  sprite slot of that line and none in the line that draws them. The sprite
  search of `sprites.rs` does that read at the latch point (above) and keeps the
  whole entry, so a pattern or an `x` written halfway across a line lands on the
  line after the next, exactly as a whole entry does.
* Every VRAM write of the chip goes through `Vdp::write_vram_byte`, and that is
  what feeds the cache: a write inside `[sat_base, sat_base + 640)` that lands on
  one of the first four bytes of an entry updates that entry's line. The three DMA
  modes of `dma.rs` reach it the way the data port of `ports.rs` does. The feed
  is Nemesis's snoop (SpritesMind t=1291, 2012, answering Mask of Destiny's
  VRAM-bus captures: the VDP "only ever updates the contents of the cache if it
  detects a VRAM write being made within the SAT cache 'window' based on the
  current location of the SAT as set by register 5"), and the updates are byte
  sized, the width of the VRAM writes it watches.
* Moving the SAT with R5 refills nothing. A table moved to under one that was
  never written is therefore drawn with the y, the size and the link of the old
  one and the pattern and x of the new one, until the game rewrites the entries
  ("you can change register 5, and the cache will continue to use the cached
  data from the old memory location. CastleVania Bloodlines relies on this",
  the same thread).
* It is chip state, not part of the line snapshot above: the chip takes no VRAM
  write while a line is being drawn.
* Two corners are fenced, written up on #313 (#313's tests fix them in so a
  change fails loudly): no source gives the cache a size of its own, so the model
  holds all 80 entries in both widths - the shrink to 64 in 32-cell mode is only
  TascoDLX's unconfirmed guess of t=851 - and no source says what a link of
  80..=127 reads, so the model falls back to VRAM there (#94).
* Two more were fenced by #318, with the citations on `Vdp::search_sprites`. The
  clock of the search inside its line is still unmeasured - Mask of Destiny, whose
  captures pin the fetch slots of the line before, says of the scan itself that
  it leaves him "without any solid reads as to when the VDP scans the cached copy
  of the SAT" - so the model shares the latch point with the scroll tables and
  the mode registers, and a SAT write in the first blanking window of a line is
  in time for the next line's list until a sprite ROM says otherwise.
* The display-off half of that fence shipped as #566's slot budget. Eke (t=851,
  April 2011, verified on hardware) watched the first line after a display-off
  stretch draw no sprites - "in some games that reduce the active screen height
  by expanding vertical borders using the display OFF feature, you can see that
  the first line of 'active' display does not display sprites as well" (the
  Power Instinct border, Sonic 2 VS's bottom screen) - and Genesis Plus GX
  models the partial case in vdp_ctrl.c as a budget: the dot count of the line
  and the pre-processed sprite count of the next line are both "reduced
  depending on the amount of cycles spent with display disabled". Their code
  approximates that proportion with one fixed cut (128 dots on a re-enable
  inside HBLANK); this scales it. `advance` gathers the master clocks every
  chunk spends with R1 bit 6 off - a register write lands between `advance`
  calls, so a mid-line toggle (Overdrive 2) is exact at that granularity - and
  the two sprite moments spend the one accumulator: the search visits at most
  80/64 entries times the enabled fraction of the window from the previous
  latch point to its own, which is the sprite processing that had elapsed when
  it ran (the search is one instant early, above), and `render_sprite_line`
  draws at most 320/256 dots times the enabled fraction of the line's own
  latch-point-to-line-end window. The per-line sprite count of 20/16 is not
  scaled: Eke's note names the other two counts, and GPGX keeps its per-line
  limit at the display width over sixteen. The documented choices beyond the
  sources: no floor above zero (Eke's fully-off window is a zero budget, which
  any floor would break), off clocks clamped to the window so a beam that
  jumped past a latch point cannot push the fraction out of range, and integer
  math that floors, so any disabled clock costs budget. Both live on
  `search_sprites` and `sprite_dot_budget`.

The layer buffer those entries are drawn into is filled a clipped cell run at a
time, not a tested pixel at a time (#368): the pixels of one cell are a
contiguous run, so the two edges of the line buffer are decided once per cell
and the pixel loop walks two slices of that one length. The shape is a
constraint, not a style: the per-pixel bounds test it replaced spilled the
whole loop to the stack (the line pointer and the pattern index were reloaded
a pixel at a time), and three other shapes the ticket measured - a deferred
masking-mode-2 scan, a byte-at-a-time `tile_row`, forced `#[inline]` on the
SAT fetch - each lost to the simpler code it replaced. What the same ticket
learned about measuring: `perf` shares per symbol move when inlining moves code
between symbols (one profile had `fill_cell_run` jump from 1.3% to 10.4% with
its code untouched), so a candidate is believed only when the two bench
binaries, run interleaved, agree on it and the digests never move.

### VDP debug views

`crates/vdp/src/debug.rs` (M4-24, #104) renders VDP internals into RGBA buffers for
`headless`, the web UI and agents to look at. It is behind the `debug-views` cargo
feature, default off, so no release build of the emulator or the WASM core carries
it, and `just check` runs `cargo nextest run -p vdp --features debug-views` and the
same for clippy - without the feature the module is not compiled at all:

* `tiles(palette)`, `plane(PlaneView::A | B | Window)`, `palette()` return a
  `DebugImage { width, height, pixels }` of the same `0xAABBGGRR` pixels
  `FrameBuffer` holds, and `sprite_list()` returns the SAT walk as a
  `Vec<SpriteInfo>` for text. These are slow-path: they allocate, and none of
  them runs on the beam.
* A plane view ignores the scroll: it is the name table as written, of the R16
  size the chip is in now (the window's size is fixed by the display width), with
  a 1 px white rectangle over the visible area of the two scroll planes. Cells
  come from `Vdp::fill_cell_run`, so the entry decode is the renderer's and not a
  second reading of it. `PlaneView` is a debug-local three-way choice rather than
  a third `planes::Plane` variant, because the renderer must not be handed the
  window - it has no scroll and no R16 size.
* `sprite_list()` walks the live SAT chain, the way `sprites::Vdp::sprite_list`
  does, not the list of the line being drawn, which is a line old (#311) and is
  not reachable from outside the beam.

### Audio chips

```rust
impl Sn76489 { pub fn new() -> Self; pub fn write(&mut self, value: u8); pub fn clock(&mut self) -> i16; }
impl Ym2612  { pub fn new() -> Self; pub fn write(&mut self, port: u8, value: u8); pub fn read_status(&self) -> u8;
               pub fn clock(&mut self) -> (i16, i16); }
```

`genesis` owns mixing: it clocks each chip at its native rate, then
resamples to the host rate (48 kHz default) with a band-limited resampler,
and applies the board's output filter, which follows the console's chip
generation (`Config::chip_model`, one setting for both halves, #342): a
discrete-YM2612 Model 1 gets the board's first-order low-pass, about 3.39
kHz (Genesis Plus GX's default coefficient agrees on the corner and on it
being one pole), while a YM3438 Model 2/3 gets nothing modelled - its real
Sallen-Key stage has no published pole or Q (ConsoleArtisan's Genesis repair
reference), so modelling it would be invention, and Genesis Plus GX likewise
treats its low-pass as a Model 1 flavour only.

One YM2612 register detail the write path has to model exactly: the `&A4`
frequency latch is the chip's, not the channel's. `&A4`-`&A6` only latch the
block and the top three F-Num bits, and the next `&A0`-`&A2` write of any
channel - part I or part II - is what applies them, so a driver that writes
`&A4` once and then steps `&A0`-`&A2` leaves every channel it touches on the
same latched half (YM2608 application manual 2-4-1 "F-Number/Block": the
setting procedure is "(1) Data write of Block/F-Num2: $A4-$A6 (2) Data write
of F-Num1: $A0-$A2"; Nuked-OPN2's `ym3438.c` keeps the same one `reg_a4`
scalar). Real drivers write `&A4` before every `&A0`, which is why the
every-channel effect of the rule is rarely heard. The `&A8`-`&AE` group of
channel 3 special mode (#114) is modelled the same way: one `&AC` latch for
the whole group.

The chip's idle fast path (#369) rests on three constraints any later change
has to keep. The gate is `busy == 0` (a data write outlives one sample: 32
internal cycles against 24), neither timer running, and all 24 envelopes at
rest - no key, attenuation at the silence a release clamps at - which is an
absorbing state, and the path skips only what that makes no-ops: the
envelope updates and the per-operator sum. The phase counters are not
skipped even though an operator at silence outputs zero whatever its phase:
the model pins them counting every sample, keyed or not. And the gate is
recomputed per sample rather than cached in a field, because the `serde`
derive carries the whole chip into the save-state payload and any added
field moves it, which is a version bump, not a perf ticket's side effect.

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
    pub fn set_pad6(&mut self, port: usize, buttons: Buttons); // the 6-button pad: X Y Z MODE reach the game (plutiedev "Controllers")
    pub fn save_state(&self) -> Vec<u8>;   // the container of docs/save-state-format.md
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

`save_state` cannot fail and so returns the bytes: the machine is made of buffers
and integers, and encoding one is total. `load_state` is the half that can be
handed a stranger's bytes, and `StateError` is what it answers them with -
wrong magic, wrong format version, wrong cartridge, or a payload that does not
decode. The format itself, including what a state does not carry (the ROM, the
frame buffer) and what that means after a load, is
[`docs/save-state-format.md`](save-state-format.md) (M6-07, #127).

The bus keeps two open-bus models where the sources give them (#508). An
undriven read on the 68k side answers the 68000's prefetched instruction word -
the last word it drove onto the bus, published on every queue refill - with
`$FFFF` from the data-line pull-ups before the first fetch, byte reads taking
the lane their address sits on; that is the answer both surviving references
implement (Genesis Plus GX's `m68k_read_bus_16`, BlastEm's
`get_open_bus_value`) and the one hardware observation on record (Kabuto's
hardware notes v1.5, on the VDP debug ports). No decay is modelled: no source
gives one. The `$C00020`-`$C0003F` corner of the VDP area is not a return value
at all: plutiedev "Memory map" says it "hangs up", so an access there sets the
same freeze the TMSS lock-up uses, cleared only by `reset_devices`. The Z80's
own undriven areas stay at the fixed `0xFF`: no source describes the Z80 side's
float, and where the Z80 crosses onto the 68k's bus (the window, the VDP
ports) it inherits the 68k's answer through the same dispatch.

### web

- Leptos (CSR) app built with `trunk build --release` from `crates/web`.
- Emulation runs on the main thread, driven by `requestAnimationFrame`, paced
  by audio time (the audio clock is the master; video drops or repeats a
  frame if needed).
- Video: canvas, `putImageData` of the frame buffer first, WebGL later for
  scaling/CRT filter.
- Audio: Web Audio API through `web-sys`, scheduling `AudioBufferSourceNode`s
  ahead of the play head. No hand written JS files.
- Audio on Safari/WebKit (#629): Safari on macOS and iOS was reported choppy -
  audio and rendering together - with Chrome fine on the same laptop. A 60 s
  headless measurement of all three engines (Playwright Chromium 156, Firefox
  157 and WebKit 27.2, each against a live audio clock) showed the per-frame
  scheme itself healthy on WebKit: 0 underruns, `start_with_when` honoured,
  `currentTime` advancing smoothly. What the engine gets instead is a coarser
  schedule: on a WebKit user agent one scheduled block carries 4 console frames
  (`crates/web/src/audio_pacing.rs::WEBKIT_FRAMES_PER_BLOCK`, 15 nodes a second
  instead of 60, a quarter of the per-second `AudioBuffer` + channel-copy churn
  on the engine whose JIT and GC are the suspected frame-budget culprit), and
  the cursor target deepens to one batch plus three frames of pacer room
  (`target_ahead_secs`, 117 ms on NTSC) - a batched cursor only advances once
  per batch, and the first measurement with batching alone ran the lead's floor
  onto the too-late line (28 restarts in the 60 s window; the deeper target
  brought the same window to 0). The lead the pacer reads counts the frames
  the batcher is still holding as well as the scheduled ones
  (`audio_pacing::lead_secs`). The first batched build counted only scheduled
  blocks, so the pacer ran frames in bursts (`0 0 1 2 0 0 1 2 3` frames per
  display tick) and Safari showed dropped frames, since a tick paints only its
  last frame; the native loop model in `audio_pacing`'s tests pins one frame
  per tick and the 50 ms floor a block is placed at. Chrome and Firefox keep one frame per block at
  the plain 60 ms target, which the measurement shows working;
  `framesPerBlock` on `window.rustsegaAudio` says which shape a given browser
  got. What the headless rig cannot see, and what stays browser-specific:
  CoreAudio's real output latency on macOS, iOS's gesture and sample-rate
  conversion rules, and Safari's low-power rAF throttling - a real-device pass
  is still the acceptance test.

  The real-device pass came back still stuttering, sound and picture, after the
  batching fix. The cause is the audio-clock pacing itself: when `currentTime`
  moves in coarse steps, a loop that asks it every tick how many frames to run
  gets 0 or 2 frames on the ticks a step lands on. Headless Chromium has a coarse
  clock too and shows it. On the scheduled output, Sonic 1 over 20 s ran 0
  frames on 730 ticks, 2-3 on 466, and exactly 1 on only 5. So WebKit now gets an
  **AudioWorklet ring** (`crates/web/src/audio_ring.rs`). The console's sound is
  posted to a processor that drains it on the audio thread as one continuous
  stream, at the device's own sample rate. The console is paced on the display
  (`FramePacer`), and the ring's lead only adds or drops a frame when it leaves
  a +-2-frame band around 60 ms (`crates/web/src/ring_pacing.rs`, with a native
  loop model that drains the ring in coarse 1024-frame callbacks). The same
  20 s run on the ring: 1 frame on 1199 of 1200 ticks and 0 ring underruns;
  headless WebKit with real GStreamer audio: 0 ring underruns, and the only
  0-frame ticks are its 62 Hz rAF being held to the console's 60. The
  scheduled output stays as the fallback when the worklet does not load, and
  as what Chrome and Firefox use. `?audio=ring` or `?audio=scheduled` forces
  either path on any browser; `output`, `ringLead` and `ringUnderruns` on
  `window.rustsegaAudio` show which one is running and how it is doing.
- Input: one `InputState` that merges keyboard, Gamepad API, and touch
  overlay into `Buttons` per port. All bindings are rebindable and stored in
  `localStorage`.
- Input movies: the drawer's Input movie section records the two pads to a
  `.rsm` and stops into a download, and replays a picked or pasted one. A
  replay is admitted only when it parses, names this cartridge (`rom-hash`)
  and is of the region this run resolved to; either session restarts the run
  from the console's reset with the frame count at 0, because a movie's
  frame 0 is the first frame of a run. While a replay is on, `Movie::set_pads`
  feeds the loop and the page's own input is unread until it is stopped; a
  recording notes the pads the page reports (`crates/web/src/movie.rs`, #365).
- Persistence: SRAM and save states in IndexedDB (via the `rexie` or
  `idb` crate, or raw `web-sys`), keyed by ROM SHA-1.
- ROMs are loaded by file picker / drag and drop, or from the server's
  optional ROM library. ROMs never get bundled into the binary.

### server

- `axum` + `tokio`. Embeds `crates/web/dist` at compile time (`rust-embed`
  or `include_dir`). Serves it with correct MIME types (`application/wasm`)
  and compression.
- Configured by flags, each with an environment variable, and a flag beats the
  variable (M8-02, #156): `--bind` / `RUSTSEGA_BIND` (default `127.0.0.1`),
  `--port` / `RUSTSEGA_PORT` (default `8080`, and `0` means any free one),
  `--rom-dir` / `RUSTSEGA_ROM_DIR`, `--log` / `RUSTSEGA_LOG` (a `tracing` level,
  default `info`). An unusable `--rom-dir` is a startup error with status 2
  rather than a server with an empty library.
- `RUSTSEGA_ROM_DIR` is the same variable `testkit::rom_dir` and the test suite
  use for the owner's collection, so inside `nix develop` the server starts with
  that library already configured. It still binds loopback unless told otherwise,
  and TLS is the reverse proxy's job, not this binary's.
- `GET /healthz` answers `ok`, and `GET /api/info`
  `{ "version": "<crate version>", "library": <bool> }` (#156). Both are answered
  from the shared `AppState` and neither touches the asset tree.
- Optional `--rom-dir <path>`: exposes `GET /api/roms` (list, with `?q=` to
  filter by name), `GET /api/roms/{id}` (bytes) and `POST /api/roms/rescan` so the
  UI can show a library (M8-03, #157). The directory is scanned once at startup -
  `.md`, `.bin`, `.gen`, `.smd`, up to 10 MiB, recursively - and an id is the
  SHA-1 of the path relative to it. That id is the only way a request can name a
  file: nothing a request says is ever joined to the rom dir, and a symlink that
  resolves outside it is not listed at all.
- Everything else is the frontend: a built file is served as itself, and a path
  whose last component has no extension is served `index.html`, so a client-side
  route like `/library/sonic` survives a refresh (M8-01, #155). `/api/` is the
  exception: an unknown path under it is an endpoint that does not exist, so it
  gets the same plain-text `not found` 404 the library gives an unknown id rather
  than the app shell at 200 (#418). A real endpoint with the wrong method is a
  405 from axum, settled before the fallback is reached.
- One `CompressionLayer` covers the whole router, following tower-http's default
  rule (not gRPC, not an image, not SSE, not under 32 bytes) plus one exception:
  `application/octet-stream` is never compressed, because that is a cartridge, and
  a Genesis ROM is dense binary that barely shrinks (#417). The download is cached
  `private`, so compressing it was work repeated per client rather than once. What
  is left still compresses, the wasm and the ROM listing included.
- `nix build` produces the single binary. The frontend dist is its own
  derivation, built once by trunk and embedded by both it and the static build
  (#424): the wasm runs in the browser, so nothing in it depends on the
  server's C library or target. `nix build .#static`, which `just build-static`
  runs, produces the same binary for `x86_64-unknown-linux-musl`, fully static
  (M8-05b, #159), so it runs on a Linux box with no nix on it.
- `nixosModules.default`, and the same module as `nixosModules.rustsega`, is the
  NixOS module of #161: `services.rustsega` with `enable`, `package`, `bind`,
  `port`, `romDir`, `openFirewall` and `logLevel`, each one handed to the server
  as the flag of the same name by a unit that runs as a dynamic user with the ROM
  directory read-only. `nix/module.nix` is the module and `nix/module-test.nix`
  the VM test that boots it, `nix build .#checks.x86_64-linux.nixos-module`.
  A reverse proxy and TLS are M8-08 (#163).

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
