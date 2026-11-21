# `scripts/ref/` - blastem as an external reference oracle (#525)

Three tickets ended with the same sentence: *our draw matches our own tables, so no
internal measurement can tell "our emulator is wrong" from "the ROM asked for
this"* - [#404](https://github.com/scottjab/rustsega/issues/404),
[#441](https://github.com/scottjab/rustsega/issues/441),
[#357](https://github.com/scottjab/rustsega/issues/357). The way out is a second
emulator. This directory is the harness for one: run a cartridge in
[blastem](https://www.retrobrute.com/) and in rustsega over the same number of
frames, dump 68000 work RAM from both, and diff.

The first finding it produced is
[#526](https://github.com/scottjab/rustsega/issues/526): on `Sonic & Knuckles
(World)`, with no input, work RAM agrees with blastem byte for byte at frame 2 and 25
addresses in `$FFF62A-$FFFFD7` never agree again. Follow-up measurement in the same
ticket found most of that 25 was the two dumpers sampling the same per-frame handshake
byte at two different places in the instruction stream, which is what
[`--stop-at-pc`](#the-comparison-point---stop-at-pc-526) is for; at one shared
instruction S&K is down to 12, of which one is scratch nothing reads.

## What is here

| File | What it does |
|---|---|
| `run-ref.sh` | Run a cart in blastem's debugger, save work RAM at named frames -> `bNNNNN.bin`. `--stop-at-pc` names an instruction instead of a frame; `--vdp` saves the VDP's VRAM, CRAM and register file instead -> `bvNNNNN.bin`, `bcNNNNN.bin`, `brNNNNN.bin` |
| `run-ours.sh` | Run the same cart in rustsega with no input, save work RAM at the same frames -> `rNNNNN.bin`. Takes the same `--stop-at-pc` and `--vdp` (`rvNNNNN.bin`, `rcNNNNN.bin`, `rrNNNNN.bin`) |
| `compare.py` | Compare the two: frame alignment sweep, per-frame diff, persistence test, `--vdp` for the VDP's two memories and register file (#534, #549), a `class` line separating a contiguous persistent divergence from frame phase (#539), and `--meter`, which reads a cart's own parked progress counters as a cycle meter (#530). `--selftest` is in `just check` |
| `sweep-vdp.sh` | The #539 sweep: `--vdp` every cart in `crates/genesis/tests/commercial_list.toml`, one resumable cart at a time, and collect one row per cart into `target/ref/vdp-sweep/results.tsv` |
| `terminal-standin.py` | The `x-terminal-emulator` blastem's `-d` wants and NixOS has not got; makes the debugger a pipe |
| `bin/x-terminal-emulator` | The shim `run-ref.sh` puts on `PATH` so blastem finds the stand-in |
| `common.sh` | The shared default output directory and frame-list handling |

Both dumpers write a raw 64 KiB image of `$FF0000-$FFFFFF` per frame - all of work
RAM, no header, no address, no length - which is what makes the comparison a byte
comparison. Frame `N` means **the state after N frames** on both sides. With
`--vdp` they write the VDP's memories in the same shape: `v` images are the 64 KiB
of VRAM and `c` images are the 64 colour words of CRAM as 128 raw bytes.

## The commands

```sh
nix develop -c bash                                  # blastem and python3 are in the shell
ROM="$RUSTSEGA_ROM_DIR/Sonic & Knuckles (World).md"

scripts/ref/run-ref.sh   "$ROM" 595-605               # ~12 s; blastem is the slow half
scripts/ref/run-ours.sh  "$ROM" 595-605               # ~1 s
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)" 595-605
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)/anchor" 47-51 --anchor 0x584 --meter

# the VDP's own memories instead of work RAM (#534): same frames, same directory
scripts/ref/run-ref.sh  --vdp "$ROM" 595-605
scripts/ref/run-ours.sh --vdp "$ROM" 595-605
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)" 595-605 --vdp
```

Frames are given the same way to all three: separate numbers or `A-B` ranges
(`2 3 600`, `595-605 1200`); `compare.py` takes them comma-separated too.

The two dumpers write to `target/ref/<cart>/` - the ROM's file name with spaces
replaced - unless `-o DIR` says otherwise, and the comparator is then pointed at that
directory. `target/` is gitignored, so nothing this tool produces (a ROM path, a
reference dump, a debugger transcript) can be committed by accident, and none of it
should be. `compare.py` exits 0 on agreement, 1 on disagreement, and 2 when the
harness itself could not run.

Blastem writes a `vdpregs` dump next to every frame in `blastem.log`, which is the
thing to read when the RAM diff says the picture is being programmed differently.

The frame numbers in the report are the **reference's**. If the two frame counters
disagree - and on S&K they do, by one - the report says so and compares our images
at that offset; see "Frame alignment" below.

## The comparison point: `--stop-at-pc` (#526)

By default both dumpers stop at the end of a frame. That is not one place in the
instruction stream: ours ends when the VDP raises `frame_done` and the line it was
running finishes, and blastem's ends wherever its own frame loop stops the 68000. The
gap is wide enough to matter - a byte the game sets and clears once a frame holds
different values at the two samples, and #526 counted 25 of them on S&K, every one of
them written by the same game routine on both sides at the same cadence.

`--stop-at-pc ADDRESS` replaces "frame" with "instruction". Frame `N` then means **the
Nth time the 68000 is about to execute `ADDRESS`**: the reference runs `breakpoint
ADDRESS` and `N` `continue`s, and our half runs
[`Genesis::run_until_pc`](../crates/genesis/src/lib.rs) to the Nth arrival. Both stop
at an instruction boundary, so the point is the same one on both machines and frame
phase cannot leak into it:

```sh
scripts/ref/run-ref.sh  --stop-at-pc 0x584 "$ROM" 595-605
scripts/ref/run-ours.sh --stop-at-pc 0x584 "$ROM" 595-605
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)/anchor" 595-605 --anchor 0x584
```

On S&K that moves the persistent list from 25 addresses to 12, the measured frame
offset from `+1` to `+0`, and the mean differing bytes from 25.6 to 13.1. What is left
is one table (`$FFFF13-$FFFF3D`) and the write-only scratch byte `$FFFFD7`; what went
away is the handshake set (`$FFF62A`, `$FFFDF0-$FFFDF7`, `$FFFB2D`, `$FFFB34`) that the
end-of-frame comparison could not see past.

Give it an address the cart reaches **once a frame**: the entry of the game's own
vertical-blank routine.

- Read it from the cartridge header. The VDP's vertical interrupt is IPL6, so the 68000
  takes it on the autovector `24 + 6` = vector 30 (genvdp.txt section 4 for the level,
  `crates/m68k/src/interrupts.rs` for the arithmetic): the long at `$78`.
  `xxd -s $((0x78)) -l 4 "$ROM"`.
- If that long is a ROM address, use it: `Golden Axe II (World)` -> `0xDD60`.
- If it points into work RAM, the game has built itself a jump stub there and blastem
  cannot stop on a work-RAM address at all (it accepts the `breakpoint` and never
  reports the hit). Read the stub out of one of our own work-RAM dumps and use its
  target: `Sonic & Knuckles (World)` has `$FFFFFFF0: jmp.l $584`, so `0x584`; `Ecco the
  Dolphin` has `$FFFFFF00: trap #0; jsr $179E`, so `0x179E`.
- Check it. The reference transcript `<dir>/anchor/blastem.log` prints `== arrival N
  pc=... ==` beside every dump and that PC has to be the address you gave it. Neither
  dumper checks anything else: any address is a legal comparison point, and a poor one
  simply agrees badly.

Anchored dumps go to `target/ref/<cart>/anchor/`, apart from the end-of-frame ones,
because `b00600.bin` names a different moment in each and the comparator is handed only
a directory and a frame list. `compare.py --anchor ADDRESS` changes no arithmetic: it
names the comparison point in the report, and at an anchor a non-zero measured shift
means something it did not mean before - the two machines have taken that interrupt a
different number of times by there, which is a finding rather than a nuisance.

## `--vdp`: the VDP's own memories (#534)

Work RAM cannot finish [#404](https://github.com/scottjab/rustsega/issues/404). Its
surviving hypotheses are a wrong-but-present tile index, a scroll mis-coupling and a
wrong palette line, and all three produce VDP state that is self-consistent, so
nothing internal can separate them from correct emulation. What answers them is the
VDP's VRAM and CRAM from both machines at one named frame.

It needs no patch to the reference, and nothing from the reference's source. Blastem
1.0.0 already registers the VDP's memories as **arrays in a debugger namespace**,
and `save` takes arrays:

```text
> print size(vdp:vram)         65536
> print size(vdp:cram)         64
> save "v00600.bin" vdp:vram          65536 bytes, byte addressed like the chip
> save/w "c00600.bin" vdp:cram        128 bytes: 64 colour words, low byte first
```

The separator is `:`. The VDP is a second debug context hanging off the 68000 one as
`other_roots["vdp"]`, so `vram` on its own is not defined, and `.` is the element
size suffix (`.b`, `.w`, `.l`) rather than a qualifier, which is why `vdp.vram` reads
back as `Failed to eval vdp.v`.

### Route 1, the whole table

#525 wrote that `vram`, `vram_get` and `cram_get` "are not registered debugger
expressions, so `save` cannot reach them", and #534 asked for that to be checked
rather than repeated. This is what a stock `pkgs.blastem` 1.0.0 answers, so nobody
runs the dead ends again.

| tried | answered |
|---|---|
| `help` | the full command list; nothing in it dumps a memory that is not in the 68000 address space |
| `symbols` | nothing: no symbol file is loaded, and symbols are 68000 addresses anyway |
| `print vram`, `cram`, `vsram`, `vdp`, `reg` | `Failed to eval ...` |
| `print vram_get(0)`, `cram_get(0)`, `vsram_get(0)`, `debug_vram_get(0)`, `debug_cram_get(0)`, `vdp_get_vram(0)`, `get_vram(0)`, `read_vram(0)` | `Failed to eval ...` - so #525's claim is right about the *names* |
| `print vram[0]`, `cram[0]`, `vsram[0]`, `reg[0]` | `Failed to eval ...`: the arrays exist, but not in the 68000 context |
| `print vdp.vram` | `Failed to eval vdp.v` - `.` is a size suffix, and `.vram` never reaches the parser as a name |
| `print vdp:vram[0]`, `vdp:cram[0]`, `vdp:reg[0]` | **works.** `0`, `0`, `4`: VRAM and CRAM at zero, register 0 |
| `print size(vdp:vram)` / `vdp:cram` / `vdp:vsram` / `vdp:reg` | **works.** 65536, 64, 40, 24 |
| `save "v.bin" vdp:vram` | **works. 65536 bytes of VRAM**, byte addressed, no header |
| `save "c.bin" vdp:cram` | 64 bytes: every colour narrowed to its low byte. `save/w` gives the 128 |
| `print [0x100000]`, `[0xC0000000]`, `[0xC0000004]`, `[0xC0000006]` | the **68000** address space, masked to 24 bits: `[0xC0000006] == [0x6]` is S&K's reset PC and `[0xC0000000] == [0x0] == [0x100000]` is its reset SP. The VDP ports sit above the mask, so neither its memories nor its ports are reachable this way |
| `set [0xC0000006] 0` | accepted silently, and it is a write to the ROM mirror. Not a VDP control-port write |
| `set [0x3000].w 0xBEEF` | not written: the cart image is read-only, so a 68000 routine cannot be injected to copy VRAM out through the data port |
| `print PC`, `set PC 0xFF8200` | PC reads; `set PC` does nothing. There is no way to redirect the 68000 into such a routine either |
| `vdpregs`, `vdpsprites` | work, and are the only VDP state the debugger will print: the registers (with DMA, counters, status) and the 80-entry sprite table, as text |
| `vdpsregs`, `dump`, `palette`, `mem`, `regions`, `call`, `ext`, `gdb`, `genesis`, `sregs`, `vdp` | `... is not a recognized command` (`vdp` only as an ambiguous prefix of `vdpregbreak`/`vdpregs`/`vdpsprites`) |
| `binddown "ui.vram_debug"`, `ui.cram_debug` | those UI actions exist in the binary; a VRAM *window* is not a dump, and #525 already found the screenshot path unreachable from `binddown` |
| `while (i < 3)`, `if`, `command BREAKPOINT` | they exist, but `{` and `}` on their own lines come back as `... is not a recognized command`, and a block holds only the commands above, none of which reach VDP memory |
| `save "x.bin" 0xFF0000 65536` | `Argument 0xFF0000 did not evaluate to an array`: the address has to be `[0xFF0000]` |
| `array a 1 2 3 4` then `save "x.bin" a 4` | writes `01 02 03 04`. `save` takes debugger arrays, which is the half of the trick `vdp:vram` needs |
| `save/2 ...` | `Invalid size 2`; the size letters are `b`, `w`, `l` |

The two things that made it work were `:` for the namespace and `save` taking an
array. The first was found by reading blastem's error text (`%s is not a valid
namespace`) and the second by reading its `save` help (`from the array or memory
region specified by ARRAY`), which is the kind of thing this table is here to
remember.

### What the two sides write

`run-ours.sh --vdp` writes the same three images out of our own chip: `Bus::vdp.vram`
is already the 64 KiB byte-addressed image, `Bus::vdp.cram` is the 64 words, and
`--regs` writes `Vdp::reg(0..24)` - so the example narrows nothing and reconstructs
nothing - it is the state the VDP was asked to hold, not a picture of it.

`bcNNNNN.bin`/`rcNNNNN.bin` are therefore 128 bytes, and `compare.py` reads them as
64 words and calls a difference a *colour* rather than two bytes:

```sh
scripts/ref/run-ref.sh  --vdp "$ROM" 595-605
scripts/ref/run-ours.sh --vdp "$ROM" 595-605
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)" 595-605 --vdp
```

The alignment sweep runs on VRAM and all three memories are compared at the shift it
found: CRAM is 128 B of mostly-still colour, which is nowhere near enough for a
minimum of its own to mean anything, and a CRAM read at a different frame from the
VRAM beside it would be a different moment.

All three images have to be there. A directory with VRAM but no CRAM - or with the
memories but no `brNNNNN.bin`/`rrNNNNN.bin` register dumps (#549) - exits 2 saying so
rather than reporting agreement about the half it had, and so does a CRAM image of
64 bytes or a register image of 24, which is what `save` without `/w` writes.
Directories dumped before #549's registers half need one re-run of both halves
before they compare again.

### What the registers half is for (#549)

VRAM and CRAM cannot see the VDP's register file, and the #549 family is exactly
where that hides a divergence: several of its carts hold byte-identical VRAM and
CRAM on both machines with R1 `74` on the reference and `34` on ours - the
difference is a screen the reference turns on and we do not, and no memory dump
can say so. The register image is 24 little endian words, reference numbering
(genvdp.txt section 17), reported as `R1 ref=0074 ours=0034` rather than as a
two-byte offset.

### What it said first

`Sonic & Knuckles (World)`, no input, frames 595-604 reference numbering, end of
frame, measured 2026-11-17:

- **CRAM agrees.** 0 of 64 colours differ in any of the 10 frames compared.
- **VRAM differs by 1536 bytes in every one of them**, one contiguous block from
  `$001640` to `$001C3F`, where the reference holds tile data and ours reads 0. Mean
  1568.0 of 65536 bytes per frame, shift +1 measured, and another 32-52 addresses
  around `$00F800` differing at frame phase rather than persistently.
- First difference: VRAM `$001640`, `ref=11 ours=00`.

That is the number #534 was for. What it means is #404's business, not this file's.

## Sweeping it over the commercial list: `sweep-vdp.sh` (#539)

`--vdp` on one cart is a measurement. #539 wants the map: run it over every cart in
`crates/genesis/tests/commercial_list.toml` and file one issue per cart with a confirmed
divergence. `sweep-vdp.sh` is that loop, and it is resumable because blastem costs minutes
per cart.

```sh
DISPLAY=:0 nix develop -c bash scripts/ref/sweep-vdp.sh            # every cart without a row yet
DISPLAY=:0 nix develop -c bash scripts/ref/sweep-vdp.sh --limit 5  # a handful, then stop
                 nix develop -c bash scripts/ref/sweep-vdp.sh --only psiv,shinobi3 --force
```

For each cart it reads the exact ROM name and the frame count out of the cart list - the
same file `tests/commercial.rs` reads, so the sweep cannot drift from the set the tests
cover - dumps both halves at `--vdp` over the last 16 frames of that cart's own checkpoint
run, runs `compare.py --vdp --tsv`, and appends one row to `target/ref/vdp-sweep/results.tsv`.
A cart that has a row is skipped, and a cart whose dumps are on disk for the requested window
skips straight to the comparison, so an interrupted sweep resumes. `--force` redoes one.

| `results.tsv` column | what it says |
|---|---|
| `key`, `name`, `frames` | the cart, and the window dumped |
| `shift` | the frame alignment the comparator measured for it |
| `vram_mean` | mean VRAM bytes differing per frame at that shift |
| `vram_first` | the first differing byte: address, then `ref=` and `ours=`, then the frame |
| `vram_span`, `vram_runs`, `vram_largest` | the extent, run count and widest run of what differs in *every* frame: this is how contiguous the divergence is |
| `cram_mean`, `cram_persistent` | CRAM, in colours |
| `verdict` | `agree`, `phase`, or `real` - see the next section |
| `phase_safe` | `-` until a `--phase-safe` re-run, then the anchor it was re-run at |

Blastem cannot be run in parallel with itself - its debugger is driven over fixed FIFOs in
`/tmp` (caveats below) - so carts go one at a time and `--limit` is there to bound a session,
not to widen one. The rows and the dumps live under `target/`.

## Telling a divergence from frame phase: `class`, and `--min-run` (#539)

An end-of-frame comparison carries frame phase, and a byte count cannot see the difference.
S&K at frames 595-604 reports a mean of 32.0 of 65536 VRAM bytes differing with nothing
wrong in it, all of it scattered single bytes on a stride around `$F800`; the real bug
beside it, #536, was 1536 bytes of one contiguous block. So every `--vdp` and work-RAM
report ends with a `class` line that says which one it is looking at:

```text
class    real - 1536 bytes in all 10 frame(s), 1 contiguous run(s) of >= 4, spanning 0x001640-0x001C3F
class    phase - no run of >= 4 addresses differs in all 10 frame(s); 0 bytes do, widest run 1, in 0 run(s)
```

`real` means a run of at least `--min-run` elements (default 4) differs in **every** frame
compared: contiguous in address and not moving, which is what a dropped transfer looks
like. `phase` is everything else. The threshold is 4 elements because one is a stray and
every frame-phase family this harness has met arrives one element every four or eight,
while a nametable cell, a tile row, and any burst the VDP would have taken in one go are
all runs of four or more. `--min-run 1` lowers the bar rather than changing the rule, and
the numbers the verdict was made from are printed next to it so it can be argued with.

`--tsv` prints the same finding as one tab-separated line per compared memory, in the
order `dir  memory  frames  compared  shift  mean  persistent  runs  largest  span  first
verdict`, which is what `sweep-vdp.sh` builds its rows out of. Exit status is unchanged:
0 agree, 1 disagree, 2 the harness could not run.

What `real` does **not** mean is "confirmed". The confirmation is the `--stop-at-pc` rerun:

```sh
DISPLAY=:0 nix develop -c bash scripts/ref/sweep-vdp.sh --phase-safe --only psiv --anchor psiv=0xDD60
```

That re-dumps the cart at the anchor (its own V-Int entry, per "The comparison point" above)
and rewrites the row with the numbers from there. A divergence that survives it is the kind
an issue gets filed for; one that does not was frame phase wearing a block costume.

## Reading a cycle meter: `--meter` (#530)

Some carts park a 68000 register set in work RAM from inside their own vertical
interrupt, and two of the parked registers are pointers that only move forwards
while the cart is working. `Sonic & Knuckles (World)` decompresses art in the
background and saves the decoder's registers at `$FFFF10-$FFFF37` every time the
V-Int preempts it (#529). `$FFFF2C` is its ROM input cursor and `$FFFF30` its
work-RAM output cursor, so those two longs say exactly how far the 68000 got
between two vertical interrupts, on a machine you are not instrumenting:

```sh
scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)/anchor" 47-51 --anchor 0x584 --meter
```

```
meter 0xFFFF30    lead = ours - ref, signed
   frame         ref        ours        lead   per frame
      47   FFFF4E19   FFFF4E47         +46          -
      48   FFFF55EB   FFFF5624         +57        +11
      49   FFFF5CCD   FFFF5D31        +100        +43
      50   FFFF645F   FFFF6503        +164        +64
      51   FFFF6BF5   FFFF6CA8        +179        +15
  run 47..51: lead +133 over 4 frame(s) = +33.2 per frame
  ours ahead in 4 measured step(s) of 4, behind in 0, level in 0; per-frame lead +33.2

RESULT: disagree - 0xFFFF30 leads +46..+179, +33.2 per frame (ours ahead)
```

That is one of the measurements behind #530: our 68000 got through 33 more bytes of
the stream per frame than the reference's in that leg, and the same sign comes back
from every window where the decoder is running (arrivals 46-51, 226-258 and 440-474,
20-odd measured steps in all). The three things the tool does that reading the two
longs out of the dumps by hand does not:

- **The sign is the finding.** A count of differing addresses cannot say which
  machine did more work. `lead` is `ours - ref`, so positive is us ahead and
  negative is the reference ahead, and a stretch that is level in every step is two
  machines that are not running at different speeds.
- **A frame that is not comparable is labelled, not averaged.** Every load of the
  stream starts from the same pointers on both machines and ends when the stream is
  out, so a step is only a rate while both machines are inside the same load. Each
  step gets one of four labels and only `measured` ones carry the slope:

  | label | what happened | why it is not a rate |
  |---|---|---|
  | `restart` | a cursor went backwards, or was 0 | the lead is now a different stretch of art: thousands of bytes, all of it spurious |
  | `stopped` | one machine's cursor moved and the other's did not | one of them finished the load; the lead changes by the whole advance of the one still running |
  | `idle` | neither cursor moved | no decode is running, so the parked table is dead state |
  | `measured` | both cursors moved | this is the frame-per-frame comparison |

  All three exclusions are load-bearing. On arrivals 440-480 one `stopped` step is
  -694 bytes on its own, and counting it as a frame turned a window in which we are
  ahead into a report that said the reference was.
- **It divides by frames, not samples.** `per frame` is the change in lead over the
  arrivals a run spans, so a window sampled every tenth arrival reports the same
  slope as the same work dumped every frame. Frame numbers are arrivals at a
  `--stop-at-pc` anchor, which is what makes their difference a frame count; the
  anchor is what makes them comparable at all.

Because of that last point `--meter` aligns with the same sweep the byte comparison
uses and reads our images at the shift it found. Our cursor read one arrival late is
a frame's worth of work, and a frame's worth of work looks exactly like the cycle
gap this tool is for. `--sweep 0` skips the sweep and reads at `+0`.

`--meter` takes the addresses to read (`--meter 0xFFFF2C,0xFFFF30`); a bare
`--meter` is those two, which are S&K's. It reads big-endian longs, the size the
`MOVEM.L` that parks them writes. It does not diff the rest of the image: pass
`--meter` for the cursors and leave it off for the byte comparison.

### What a byte of lead is worth in cycles

Counting the decoder's opcode mix to get cycles per output byte is easy to get wrong
in exactly the direction you are trying to measure. Perturbing the emulator is not:
change the 68000's clock ratio by a known amount, re-dump our side, and read how far
the cursor moves. The ratio is a hardware constant (`MASTER_CLOCKS_PER_68K` in
`crates/genesis/src/scheduler.rs`, 7 on real hardware), so the size of the step is
known even when the decoder's cost is not:

```text
scratch: MASTER_CLOCKS_PER_68K 7 -> 8, which is 12.5% less 68000 time in a frame

frame window        lead as shipped    lead at master/8
S&K 47-51                 +33.2/f            -241.0/f
S&K 226-262               +24.4/f            -268.8/f
S&K 440-480               +41.1/f            -211.7/f
```

`master / 8` is 1/8 of a frame of 68000 time: 112,005 clocks in a frame instead of
128,006. The difference between the two columns is therefore the work that 12.5% of a
frame buys, and dividing by 12.5 gives the scale. It comes out the same way from all
three windows:

```text
window         bytes per 1% of a frame        shipped lead, in cycles    scanlines
S&K 47-51                    21.9                     1940                   4.0
S&K 226-262                  23.5                     1330                   2.7
S&K 440-480                  20.2                     2600                   5.3
```

So the gap this cart reports is **1.0% to 2.0% of a frame of 68000 time, 1300 to 2600
cycles, 2.7 to 5.3 scanlines**, and a byte of output on this loader is worth 54 to 63
cycles. Two things about the method are worth noticing. The swing reverses sign
cleanly, which is the independent check that the meter is reading 68000 time and not
something else about the decoder; and the calibration run is a scratch change to the
emulator, made and thrown away, never committed. It is written up here because the
number it produces is the only honest way to read the tool's units: it comes from
moving the machine by a known amount, not from a model of the decoder that could be
wrong in the same direction as the thing being measured.

Three things it is not:

- **Not a cycle counter.** The units are cursor bytes; see the next section for the
  one way to turn them into cycles that does not depend on modelling the decoder.
- **Not a measurement of a cart that parks nothing.** If every address reads 0 in
  every frame the tool says so and exits 2: two machines agreeing about an address
  no program has written is not agreement, it is a probe in mid-air. Find another
  cart's cursor by reading the parked register set out of one work-RAM dump and
  looking for a long that only ever grows.
- **Not a second reference.** Same rule as the rest of this directory: it says the
  two machines disagree about 68000 time per frame, and about which of them is
  wrong it is silent (#530's own caveat).

## What it says

```
dir      target/ref/Sonic_&_Knuckles_(World)
region   0xFF0000-0xFFFFFF, 65536 B per frame
frames   10: 595..604 (reference numbering)
shift    +1 (measured: reference frame N read against our frame N+1)

frame        differ  addresses
    595        25  0xFFF62A, 0xFFFB2D, ...
...
alignment (mean differing bytes of 65536, our frame number shifted):
  shift +0      74.6  over 11 frame pair(s)
  shift +1      25.6  over 10 frame pair(s) <- best
  shift +2      75.6  over 9 frame pair(s)

differs in all 10 of 10 frames - persistent divergence:
  0xFFF62A  ref=00  ours=1A  (10 of 10 frames)
  ...
RESULT: disagree at shift +1, 25.6 of 65536 bytes per frame on average, 25 addresses in every frame compared
```

## Frame alignment, and why the tool measures it instead of assuming it

Two cores that are not cycle-identical can be a frame apart at the point of
comparison, which turns one frame's worth of ordinary churn into a large,
informative-looking diff. So `compare.py` sweeps our frame number against the
reference's over `--sweep N` offsets, finds the minimum, and **then** compares -
not afterwards. This is not hypothetical: the first version of this file compared at
shift 0 and reported 35 persistent addresses on S&K where the truth is 25.

Blastem's frame counter is one behind ours on that cart: its state after 595 frames
is our state after 596, measured directly (70 bytes differ at our frame 595, 25 at
596) and not inferred. Do not bake that into a dumper. Measure it per cart, because
it is a property of the channel - how far blastem runs before it enters the
debugger - and not of the game.

At a `--stop-at-pc` anchor the sweep usually finds `+0` and means something different
when it does not; that case is written up in "The comparison point" above rather than
here, because there the counter being swept is the cart's own interrupt rather than
either emulator's frame loop.

Two caveats about the sweep, both paid for:

- **It is only as good as the churn in the window you give it.** Over frames 2-17 of
  S&K almost nothing in work RAM is moving, both candidate shifts score within a few
  bytes of each other and the sweep picks the wrong one. Over 595-605 the minimum is
  25.6 against 74.6 next best and unambiguous. Give it a window where the game is
  working, and look at the table before believing the `best`.
- **A byte count on one frame means nothing.** Work RAM churns by 11-74 bytes per
  frame on this cart, so the report's headline is *differs in k of m frames*: an
  address differing in every frame sampled is a divergence, one differing in a
  single frame is frame phase. The persistence test is the result; the byte count is
  decoration.

## Caveats, all of them paid for

- **blastem needs a display.** It is a GUI emulator: with no `DISPLAY` it dies with
  `Unable to init SDL: x11 not available`, and under `xvfb-run` it opens a zenity
  dialog for `Failed to set vsync to off` and waits for somebody to press it. Point
  `DISPLAY` at the session's real display, which is how every number in this file and
  in #526 was measured. Nothing in here needs X for anything else.
- **blastem ignores `SIGTERM`.** `run-ref.sh` runs it under `timeout -k 5 <n>` so a
  wedged run dies. `REF_TIMEOUT` raises the wall limit (default 300 s); 1200 frames
  with dumps is about 10 s.
- **blastem kills its terminal, then writes to it one last time.** Answering `quit`,
  it SIGKILLs the process it exec'd as `x-terminal-emulator` (the driver) and only
  then writes its exit-time output - "Saved SRAM to ...", the final prompt - to the
  same FIFO. It does not ignore SIGPIPE, so with the driver dead that write breaks
  the pipe and blastem dies with signal 13 after doing all of its work (#544). Two
  things stand between that and the harness: `run-ref.sh` holds `/tmp/blastem_output`
  open itself for the whole run (a reader blastem cannot kill), and the driver, when
  it is not killed first, holds its end until the reference exits or a short grace
  passes. The exit-time signature to recognise from the outside: every dump on disk,
  every frame in the transcript, exit 141, nothing on stdout.
- **A reference run that dies partway through the walk is the driver's deadline, not
  a cart.** `terminal-standin.py` has one deadline (`REF_DRIVE_TIMEOUT`, default 120 s)
  for the whole session, boot included. When the box is loaded and the window starts
  deep into the cart's run, the lead-in outruns it, the driver vanishes mid-walk, and
  blastem's next FIFO write dies the death above with only the first few dumps on
  disk (#544's "3 of 15"). An idle box walks the same window in ~11 s; before blaming
  a cart, check the load and the wall time.
- **Never send `cont` to a piped debugger with nothing armed.** It never returns and
  the driver hangs until its timeout. Advance with `frames N`, which returns a prompt
  after N frames, and end the command file with `quit`. `--stop-at-pc` is the
  exception, measured: with a breakpoint armed, `continue` *does* return a prompt on
  the hit, which is what makes an anchored dump possible. It is also why an anchor that
  is never reached hangs the reference instead of failing it; our half carries a scanline
  budget and says `arrival N of 0x... never happened inside M scanlines`.
- **`HOME` is isolated, deliberately.** blastem writes `~/.gen` (its config) on first
  run, and the config decides `ram_init`, `default_region` and `force_region`.
  `run-ref.sh` gives it a throwaway `HOME` so every run uses the shipped defaults -
  in particular `ram_init zero`, which matters more than it looks: work RAM
  randomised at power-on on one side and zeroed on the other makes a first-frame
  comparison meaningless. Nothing in this repo depends on what your `~/.gen` says.
  The same isolation covers save data: blastem 1.0.0 also loads and writes save
  SRAM/EEPROM under `~/.local/share/blastem/<rom>/`, so an SRAM or EEPROM cart booted
  on whatever the last session left behind until #544 caught the throwaway `HOME`
  being built and never exported. The XDG overrides go into the throwaway too, so a
  machine that sets `XDG_DATA_HOME` does not leak the operator's saves in either.
- **Region has to match, and it is not checked for you.** `run-ref.sh ROM 600 -- -r E`
  forces PAL on the reference; `run-ours.sh --region europe` forces it on ours. Do
  it to both or neither. On the S&K header (`JUE`) blastem with its defaults runs USA
  and that was confirmed by comparing `-r U` (byte-identical to its default) against
  `-r E` (347 bytes different); our `--region auto` prints `region=usa`.
- **The FIFOs are `/tmp/blastem_input` and `/tmp/blastem_output`, fixed.** One
  debugger run at a time, machine-wide, and stale ones from an interrupted run are
  deleted before starting. `run-ref.sh` also parks a silent holder on the output FIFO
  for the length of the run (see the SIGPIPE caveat above); it goes away when the run
  does, and until then it is one more reason not to start a second reference run.
- **`save` quoting:** the file name must be quoted (`save "b00600.bin"`) or it is
  parsed as an expression, and the address must be the memory form `[0xFF0000]`, not
  `0xFF0000`. It takes at most three arguments. Filenames are relative to the
  working directory, which `run-ref.sh` sets to the output directory.
- **Work RAM is what it reached first.** VRAM and CRAM are reachable now, but not
  by the route #525 tried: see
  [`--vdp`: the VDP's own memories](#vdp-the-vdps-own-memories-534) for the one
  spelling that works and the handful that look like they should. The cartridge and
  the Z80's own memory are still not dumpable.
- **Neither side drives input yet.** Both dumps are no-input runs. blastem's
  debugger can press buttons (`binddown "gamepads.0.start"`), our side has
  `--pad1`; making a comparison drive both is the missing piece for anything that
  depends on input, and is deliberately not done here.
- **A reference is not truth.** Disagreement is evidence, not proof: change
  behaviour to match blastem only with a second reference in agreement, and record
  in the issue which reference said what (#525's own constraint).

## Licensing - the line this directory walks

blastem is GPLv3. It is used as **a separate process, invoked by path**
(`REF_BLASTEM`, default `blastem` from the nix dev shell), and:

- no blastem source, data, configuration or generated dump is in this repository;
- nothing in these files is derived from blastem's code. `terminal-standin.py` uses
  blastem's *documented* terminal protocol - the two FIFOs it creates and the
  `x-terminal-emulator -title ... -e .../termhelper` it execs - the way any driver
  uses a documented interface;
- the dumps it writes land in `target/`, are never a build input to anything here,
  and are not committed as goldens.

Compare a cart and get numbers. Do not copy anything out of blastem into here.

#534 is the case where that line mattered rather than annoyed. Getting VRAM and CRAM
out of the reference looked like a 20-40 line patch to blastem's debugger, which is a
derivative work of a GPLv3 program and so could not live here however useful it was.
Reading the stock debugger instead - `--help`, then the namespace and array
behaviour it documents - turned it into `save "v.bin" vdp:vram`, which needs no
patch, derives from nothing, and keeps the packaged unmodified 1.0.0 in the dev
shell. If a future ticket does need a patch, the question of where the diff lives is
the maintainer's, not the harness's.

## No ROM here

The ROM path always comes from `$RUSTSEGA_ROM_DIR` (default `~/games/roms/genesis`)
and is passed by the caller. Nothing in this directory downloads or stores a ROM,
and the output directory is gitignored so a dump cannot be committed either.

## Running the harness's own check

```sh
nix develop -c just ref-selftest      # also part of `just check`
```

Two selftests, both in `just check`. `compare.py --selftest` is the one below;
`terminal-standin.py --selftest` drives a fake blastem over the same FIFO protocol and
pins the teardown: the fake writes one more prompt after `quit`, late, and the driver
must hold the FIFO open until it lands (#544), so a driver that lets go early makes the
fake die of a broken pipe and the selftest fails.

It builds a canned pair of images that agree, a one-byte mutation of one frame, a
mutation of the same address in every frame, a pair aligned only at a one-frame
offset, an anchored pair with a byte planted in every frame, an anchored pair that is
one arrival out of step, a truncated dump and a half-present frame, and asserts the
report distinguishes all of them. Then the meter's own set: canned cursors that
match, one planted in our favour growing at a known rate (it must report that rate,
signed), the same planted the other way (it must go negative), one where the load
restarts halfway through the window (it must split the window rather than average
across the restart, which would report the same number with the wrong sign), one
sampled every tenth arrival (it must divide by the frames it spanned), an unwritten
cursor (exit 2, not `agree`), and a cursor one arrival behind (alignment, not a
lead). The two anchored ones are the pair that keeps
`--stop-at-pc` honest: an anchor that quietly stopped reporting differences would be
worse than the frame loop it replaced, and an anchor that reported a one-arrival
offset as a divergence would be re-manufacturing #526. It is what stops a refactor of `compare.py` turning the
oracle into something that prints `agree` for everything.

Then the VDP set of #534, which is the same discipline over two more memories:
canned VRAM and CRAM that match, a byte planted in every frame of VRAM (found, and
named by a VRAM offset rather than a work-RAM one), a colour planted in CRAM (found
as one colour, and printed as one colour), a CRAM image saved at half width (exit 2,
not `agree`), a run with VRAM but no CRAM (exit 2), a directory of work-RAM dumps
asked to be a VDP comparison (exit 2), and a VDP pair aligned only at a one-frame
offset (alignment, and not agreement when read at 0). The missing-memory checks are
the point of the set: a comparison of a memory nobody dumped is the one case where
an oracle can print `agree` about nothing.

## Where the other channels went

- **`-e` / `BLSTEL`, blastem's binary event log.** Real, and 4.4 KB for 60 frames of
  S&K, but after a zlib header the record encoding exists only in blastem's own
  encoder. Reading it means writing a decoder against its source, which is exactly
  the derivation the license line above rules out. Parked.
- **Frame images.** The PNG/APNG writers are all behind UI actions and the
  debugger's `binddown` cannot reach them, so a picture needs a patch to blastem.
  Parked; work RAM answers the questions these tickets have without one.
- **The right home for our half** is `headless run --dump-ram PATH`, not the cargo
  example it currently shells into
  (`crates/genesis/examples/ram_dump.rs`). That is the spike's own recommendation
  and needs its own tests and docs; when it lands, `run-ours.sh` becomes one line.
