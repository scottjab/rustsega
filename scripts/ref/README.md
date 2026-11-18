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
| `run-ref.sh` | Run a cart in blastem's debugger, save work RAM at named frames -> `bNNNNN.bin`. `--stop-at-pc` names an instruction instead of a frame |
| `run-ours.sh` | Run the same cart in rustsega with no input, save work RAM at the same frames -> `rNNNNN.bin`. Takes the same `--stop-at-pc` |
| `compare.py` | Compare the two: frame alignment sweep, per-frame diff, persistence test, and `--meter`, which reads a cart's own parked progress counters as a cycle meter (#530). `--selftest` is in `just check` |
| `terminal-standin.py` | The `x-terminal-emulator` blastem's `-d` wants and NixOS has not got; makes the debugger a pipe |
| `bin/x-terminal-emulator` | The shim `run-ref.sh` puts on `PATH` so blastem finds the stand-in |
| `common.sh` | The shared default output directory and frame-list handling |

Both dumpers write a raw 64 KiB image of `$FF0000-$FFFFFF` per frame - all of work
RAM, no header, no address, no length - which is what makes the comparison a byte
comparison. Frame `N` means **the state after N frames** on both sides.

## The commands

```sh
nix develop -c bash                                  # blastem and python3 are in the shell
ROM="$RUSTSEGA_ROM_DIR/Sonic & Knuckles (World).md"

scripts/ref/run-ref.sh   "$ROM" 595-605               # ~12 s; blastem is the slow half
scripts/ref/run-ours.sh  "$ROM" 595-605               # ~1 s
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)" 595-605
python3 scripts/ref/compare.py "target/ref/Sonic_&_Knuckles_(World)/anchor" 47-51 --anchor 0x584 --meter
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
- **Region has to match, and it is not checked for you.** `run-ref.sh ROM 600 -- -r E`
  forces PAL on the reference; `run-ours.sh --region europe` forces it on ours. Do
  it to both or neither. On the S&K header (`JUE`) blastem with its defaults runs USA
  and that was confirmed by comparing `-r U` (byte-identical to its default) against
  `-r E` (347 bytes different); our `--region auto` prints `region=usa`.
- **The FIFOs are `/tmp/blastem_input` and `/tmp/blastem_output`, fixed.** One
  debugger run at a time, machine-wide, and stale ones from an interrupted run are
  deleted before starting.
- **`save` quoting:** the file name must be quoted (`save "b00600.bin"`) or it is
  parsed as an expression, and the address must be the memory form `[0xFF0000]`, not
  `0xFF0000`. It takes at most three arguments. Filenames are relative to the
  working directory, which `run-ref.sh` sets to the output directory.
- **Work RAM only, today.** VRAM, CRAM and the cartridge are not reachable as
  debugger expressions (`vram`, `vram_get(0)`, `cram_get(0)`, `debug_vram_get(0)`,
  `[0x100000]` all fail; `[0x100000]` reads zeros). Exposing blastem's own
  `debug_vram_get`/`debug_cram_get` as expressions is on the order of 20-40 lines of
  C in one of its files and is the patch worth doing if #404 needs it - a separate
  change from this harness, and this directory stays unpatched until then.
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

## No ROM here

The ROM path always comes from `$RUSTSEGA_ROM_DIR` (default `~/games/roms/genesis`)
and is passed by the caller. Nothing in this directory downloads or stores a ROM,
and the output directory is gitignored so a dump cannot be committed either.

## Running the harness's own check

```sh
nix develop -c just ref-selftest      # also part of `just check`
```

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
