# Save state format

M6-07 (#127). The binary format `Genesis::save_state` writes and
`Genesis::load_state` reads, implemented in
[`state.rs`](../crates/genesis/src/state.rs).

A save state is the whole machine except the cartridge: the 68000, the sound
CPU, the bus with the VDP, the I/O chip, both working RAMs and the cartridge's
save and bank state, and the scheduler's position in its own frame — wrapped in
a header that says which format version it is and which game it belongs to.

## Layout

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 4 | magic | `b"RSGS"` |
| 4 | 4 | format version | `u32`, little endian, currently `1` (`state::FORMAT_VERSION`) |
| 8 | 8 | ROM checksum | `u64`, little endian, [`state::rom_checksum`] of the loaded image |
| 16 | 4 | payload length | `u32`, little endian — the number of bytes that follow offset 20 |
| 20 | n | payload | the `postcard` encoding of the machine state |

Every integer is little endian, including the ones postcard would have written
big-endian-free anyway: postcard is varint-encoded, so the only fixed-size
numbers in the file are the header's.

The payload is not self-describing. It is the postcard encoding of the tuple

```rust
(&M68k, &Z80, &genesis::Bus, &Scheduler)
```

in that order, in the version-1 layout. A payload from a build with a different
field order is a different payload, which is what the version field is for.

## The ROM checksum

`state::rom_checksum` is FNV-1a 64 — Fowler-Noll-Vo, *The FNV non-cryptographic
hash algorithm*, the 32-bit offset basis and prime widened to 64:

```text
hash = 0xcbf29ce484222325
for byte in rom:
    hash ^= byte
    hash *= 0x00000100000001b3
```

It is the same hash every golden value in this repo is built from
(`testkit::fnv1a_64`), reimplemented in `genesis` because `testkit` is a
dev-dependency and a save state has to work in a release build.

**What it is taken over**: the image as the machine holds it, `Bus::rom` — the
output of `cart::normalize_rom`. An SMD image is therefore checksummed
deinterleaved, and a front end that wants the checksum of the file it read off
disk has to normalize it first. The state identifies the machine, not the
container.

**What it is for**: an identity check, so `load_state` can refuse a save written
for a different game with `StateError::WrongRom`. It is not a security check, and
a ROM deliberately built to collide with FNV-1a 64 would fool it.

**Why not the ROM bytes.** A state does not carry the cartridge image. Save
states are small on purpose — the payload of a running machine is a few hundred
kilobytes at most — and a front end that has a save state always has the ROM it
loaded next to it. Carrying a 2 MiB image in every save slot to prove which game
a save slot is for would be the expensive way to make a check the checksum makes
in eight bytes. The consequence is the rule the loader enforces: **the ROM has to
be loaded before `load_state` is called**, which is also the only way the machine
on the other side of the load is a machine you can run.

## What is in a state, and what is not

In a state, because it is the machine:

- the 68000 and the Z80, every register of both (M6-07a, #126);
- the VDP — VRAM, CRAM, VSRAM, the 24 registers, the code and address
  registers, the write-pending flag, the write FIFO, a DMA in flight including
  the words it has already read, the scroll latch of the line being drawn, the
  sprite cache and the sprite list of the line after it, the beam position, the
  status flags and the interrupt state;
- the 64 KiB of work RAM and the 8 KiB of sound RAM, the bank shift register;
- the I/O chip: three ports, their direction registers and whatever pad is
  attached with its `TH` counter where it was;
- the cartridge's save RAM or serial EEPROM, mid-transfer state included, and the
  bank registers of a mapped cart;
- the bus's TMSS state, its Z80 reset and bus-request lines, its pending stall
  counters;
- the scheduler's four clocks, so a restored frame continues where it stopped
  rather than starting again.

Not in a state, and why:

| Not in a state | Why | Where that is decided |
|---|---|---|
| The ROM | Named by checksum instead; see above | `Bus::rom`, `#[serde(skip)]` |
| The VDP colour LUT | A fixed 3 × 512-entry table derived from the CRAM encoding; rebuilt on load | `vdp::Vdp::color_lut`, `#[serde(skip)]` |
| The frame buffer's pixels | 320 × 480 `u32` of the picture the beam has already painted, which the next `run_frame` draws again from state that *is* saved. Its `width`/`height` are kept, because a line is written at `line * width` and the renderer only refreshes the geometry on line 0. | `vdp::frame_state` |
| The audio mixer | The host's resampler, levels and filter, built from `Config`; not part of the machine, and a state written at 44.1 kHz must not rewrite the settings of a machine now running at 48 kHz. Its *settings* are never touched by a load. Its *position* and its filter memory are not settings, and `Mixer::seek` re-derives both from the loaded machine rather than leaving them where the previous run had got to. | `Genesis::load_state` calls `audio::Mixer::seek`; `Mixer::state`, `out_clock`, `Stream::seek` |
| TMSS boot ROM, `Config`, front-end state | Not machine state; the front end's own business | — |

For scale: the machine of `tests/save_state.rs`'s fake ROM saves in 144 KiB, and
what it holds is 64 KiB of work RAM, 8 KiB of sound RAM, a 64 KiB VRAM, the
memories and registers of the rest. The frame buffer that is *not* in there is
320 × 480 × 4 = 600 KiB, which is why a state is not five times the size.

The practical consequences of those four rows are that after a load the first
`run_frame` redraws the picture and the first `drain_audio` goes on making sound.
Nothing else.

That second half is worth spelling out, because the mixer's exclusion from a state
can be read two ways (#362). A load *does* place the mixer: `Mixer::seek` moves its
output position onto the mixer's own sample grid at or after the master clock the
loaded machine has reached, empties the two chip streams, and resets the DC
blocker and low-pass poles. Those are the mixer's memory of what it had mixed, and
a machine put back in time has just un-mixed all of it, so all three are wrong in
the same direction; a mixer that has mixed nothing has default poles, which is what
`Mixer::new` gives. Leaving them alone does not fade out either - the DC blocker is
an IIR filter, so the residue of a rolled-back future is a permanent fraction of the
signal, and it showed up as last-bit differences dozens of frames after a load.

What a restore therefore guarantees about audio is: the machine's own sound
bit-exactly (both chips' samples and their clocks, which *are* in the state), and
the mixer's output bit-exactly from the frame its filters have settled onto live
signal, about an eighth of a second later - `MIXER_SETTLE_FRAMES` in
`tests/save_state.rs` measures it at bit-exact from frame +8 on all nineteen carts,
both restoring into the console that wrote the state and into one that never ran.
The mixer's settings - rate, gains, filter coefficients - are the host's and no
load can write them; its filter *memory* is not restored and is not asking to be.
Anyone tempted to close the last quarter-second by putting `FilterState` in the
format should read the row above first: those poles belong to this build's sample
rate, so a state carrying them would be a state that changes what a console sounds
like depending on where the slot came from, which is exactly what #127 ruled out.

## Errors

`Genesis::load_state` answers with `StateError`, and every check is answered
before any part of the machine is replaced: a refused load leaves the console
running where it was.

| Variant | Cause |
|---|---|
| `BadMagic` | Fewer than twenty bytes, or the first four are not `RSGS` |
| `UnsupportedVersion(n)` | The state is format `n` and this build knows `1` — newer or older, since there is no migration either way |
| `WrongRom` | The checksum is not `rom_checksum` of the loaded image |
| `Corrupt` | The length field disagrees with the bytes that follow it, or the postcard payload does not decode |

The order of the checks is magic, version, ROM, payload length. A truncated
header reads as `BadMagic` rather than as a length error, because nothing past
byte four means anything until the magic does.

## Versioning

`FORMAT_VERSION` is 1. Bumping it is a decision with three parts, and the format
has no migration yet — that is out of scope of #127 and does not exist:

1. A state written by a build with `FORMAT_VERSION = 1` will be *refused* by a
   build with `FORMAT_VERSION = 2` with `UnsupportedVersion(1)`, and the other
   way round with `UnsupportedVersion(2)`. That is the whole compatibility
   story: nothing is guessed and nothing is half-loaded.
2. The payload layout — the field order of the tuple above and of every `serde`
   type in it — is the format. Reordering a struct field, adding one or removing
   one changes the encoding, and postcard does not detect that; only the version
   field can.
3. Anything that changes what a byte of the payload means has to bump
   `FORMAT_VERSION` in the same commit, and update this page.

## Out of scope

- **Migration between versions.** A state of an older format is refused, not
  converted (#127 lists this as out of scope; the first format change that needs
  it will be its own ticket).
- **The front-end UI** for save slots, names, timestamps and screenshots is
  M7-15.
- **Rewind / ring buffers** of states, which is a caller of this format rather
  than a change to it.
