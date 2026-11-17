# `scripts/ref/` - blastem as an external reference oracle (#525)

Three tickets ended with the same sentence: *our draw matches our own tables, so no
internal measurement can tell "our emulator is wrong" from "the ROM asked for
this"* - [#404](https://github.com/scottjab/rustsega/issues/404),
[#441](https://github.com/scottjab/rustsega/issues/441),
[#357](https://github.com/scottjab/rustsega/issues/357). The way out is a second
emulator. This directory is the harness for one: run a cartridge in
[blastem](https://www.retrobrute.com/) and in rustsega over the same number of
frames, dump 68000 work RAM from both, and diff.

The first finding it produced is [#526](https://github.com/scottjab/rustsega/issues/526):
on `Sonic & Knuckles (World)`, with no input, work RAM agrees with blastem byte for
byte at frame 2 and 25 addresses in `$FFF62A-$FFFFD7` never agree again.

## What is here

| File | What it does |
|---|---|
| `run-ref.sh` | Run a cart in blastem's debugger, save work RAM at named frames -> `bNNNNN.bin` |
| `run-ours.sh` | Run the same cart in rustsega with no input, save work RAM at the same frames -> `rNNNNN.bin` |
| `compare.py` | Compare the two: frame alignment sweep, per-frame diff, persistence test. `--selftest` is in `just check` |
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

- **blastem ignores `SIGTERM`.** `run-ref.sh` runs it under `timeout -k 5 <n>` so a
  wedged run dies. `REF_TIMEOUT` raises the wall limit (default 300 s); 1200 frames
  with dumps is about 10 s.
- **Never send `cont` to a piped debugger.** It never returns and the driver hangs
  until its timeout. Advance with `frames N`, which returns a prompt after N frames,
  and end the command file with `quit`.
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
offset, a truncated dump and a half-present frame, and asserts the report
distinguishes all seven. It is what stops a refactor of `compare.py` turning the
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
