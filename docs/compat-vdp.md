# VRAM and CRAM against blastem

> The work RAM map is [`compat-commercial.md`](compat-commercial.md) and the whole-collection
> crash sweep is [`compat.md`](compat.md). This page is the VDP's own memories, compared
> against a second emulator: what #536 could see and nothing else could.

VRAM and CRAM are dumpable from both machines: rustsega writes `Bus::vdp.vram` and
`Bus::vdp.cram` and blastem's own debugger answers `save "v.bin" vdp:vram` and
`save/w "c.bin" vdp:cram`, so the comparison needs no patch and derives nothing from the
reference (#534). `scripts/ref/sweep-vdp.sh` runs it over every cart in
`crates/genesis/tests/commercial_list.toml` and keeps the rows.

Run it with:

```sh
DISPLAY=:0 nix develop -c bash scripts/ref/sweep-vdp.sh          # resumable, skips what it has
```

Blastem costs minutes a cart, so a cart with a row in `target/ref/vdp-sweep/results.tsv` is
skipped and a cart whose dumps are on disk skips to the comparison. Nothing here runs in CI:
it reads `$RUSTSEGA_ROM_DIR`.

## What a row says

Window is the last 15 frames of the cart's own checkpoint run from the list, **with no
input on either side** - neither dumper presses START, so this is the attract/title state,
and a divergence that only appears once you are in a level is invisible to it. `shift` is
the frame alignment `compare.py` measured for that cart, and the phase-safe re-run at one
shared instruction is `--stop-at-pc <anchor>` with the anchor from the cart's own V-Int
vector (long at `$78`).

`real` is a divergence contiguous in address, or large enough that no frame phase explains
it, holding in **every** frame compared. `phase` is the scattered stuff that moves when the
alignment shifts: S&K's `$F8xx` family differs in a mean of 32 VRAM bytes a frame with
nothing wrong (#534). The rules and the two thresholds are in `scripts/ref/README.md`.

## The map

Measured 2026-11-18 at `c89b5a2` (the #537 DMA freeze fix), window `586-600` unless the
cart's checkpoint is 900 frames. The `psii`, `psiv` and `nbajam` rows were re-measured
2026-11-20 on current main after the #544 harness fix (see the section below), and that fix
also closed a hole the 2026-11-18 rows carry unknowingly: until it, blastem ran on the real
`HOME` and an SRAM or EEPROM cart booted on whatever save the last session had left in
`~/.local/share/blastem`. `runs/largest` is the run count and widest run, in bytes,
of what differs in every frame of the window.

| cart | VRAM mean/frame | first difference | runs/largest | CRAM | verdict |
|---|---:|---|---|---|---|
| `sonic1` Sonic The Hedgehog | 1.7 | `$00FF7B` ref=76 ours=74 | 1/1 | agrees | phase |
| `sonic2` Sonic The Hedgehog 2 | 12.7 | `$00FF07` ref=55 ours=54 | 0/0 | agrees | phase |
| `sonic3d` Sonic 3D Blast | 0.0 | - | 0/0 | agrees | agree |
| `sor2` Streets of Rage 2 | 0.0 | - | 0/0 | 2.2/frame, none persistent | phase |
| `gunstar` Gunstar Heroes | 0.0 | - | 0/0 | agrees | agree |
| `contrahc` Contra: Hard Corps | 0.0 | - | 0/0 | agrees | agree |
| `thunderforce4` Thunder Force IV | 0.4 | `$00F9B3` ref=37 ours=00 | 0/0 | agrees | phase |
| `castlevania` Castlevania: Bloodlines | 266.7 | `$00C61B` ref=00 ours=01 | 0/0 | agrees | phase |
| `shinobi3` Shinobi III | 38.4 | `$00D184` ref=00 ours=80 | 0/0 | agrees | phase |
| `vectorman` Vectorman | 239.6 | `$002008` ref=A2 ours=C2 | 12/1 | 6.0/frame, none persistent | phase |
| `comixzone` Comix Zone | 0.0 | - | 0/0 | agrees | agree |
| `mk2` Mortal Kombat II | 0.0 | - | 0/0 | agrees | agree |
| `nbajam` NBA Jam | 0.0 | - | 0/0 | 2.9/frame, none persistent | phase |
| `psiv` Phantasy Star IV | 0.0 | - | 0/0 | 0.5/frame, none persistent | phase |
| `psii` Phantasy Star II | 0.0 | - | 0/0 | agrees | agree |
| `ssf2` Super Street Fighter II | 0.0 | - | 0/0 | agrees | agree |
| `roadrash2` Road Rash II | 3.7 | `$007231` ref=2F ours=23 | 0/0 | 14.4/frame, none persistent | phase |
| `ristar` Ristar | **982.6** | `$007E01` ref=78 ours=00 | 86/5 | agrees | **real** |
| `toystory` Toy Story | 0.0 | - | 0/0 | agrees | agree |

Nothing about CRAM has been confirmed yet: no cart has a colour differing in every frame of
its window, at end of frame or at an anchor. The three carts with moving colours are fades.

## The three confirmed ones

Each was re-run at its own V-Int entry so the two machines are stopped at one instruction.
Ranges and first bytes are from that phase-safe run, which is the number to work from; the
end-of-frame mean in the table above is the same divergence plus phase.

| cart | anchor | VRAM mean/frame | range | runs/largest | first difference | CRAM |
|---|---|---:|---|---|---|---|
| `shinobi3` | `$522` | 0.0 | - | 0/0 | - | agrees |
| `roadrash2` | `$11b8` | 0.0 | - | 0/0 | - | agrees |
| `ristar` | `$4284` | 107.0 | `$00A897-$00A987` | 61/5 | `$00A897` ref=28 ours=16 | agrees |

Filed as [#540](https://github.com/scottjab/rustsega/issues/540) (Shinobi III),
[#541](https://github.com/scottjab/rustsega/issues/541) (Road Rash II) and
[#542](https://github.com/scottjab/rustsega/issues/542) (Ristar).

Two things worth carrying into those tickets. Shinobi III and Road Rash II are the same
signature - thousands of two-byte holes, mostly aligned to 4, ours reading 0 where the
reference holds data - which is not the shape of one dropped transfer (#536's was one
contiguous 1536-byte block) but of the same small loss repeated. Ristar is the odd one:
most of its differing bytes have data on *both* sides, so something was written there and
written differently, which is closer to what #404 is after than to either of the other two.

### Shinobi III and Road Rash II: the second word of a long to the data port

That repeated small loss was the write FIFO, and it is #540. The FIFO kept four entries and a
single latch for a fifth, and a write arriving when the latch was taken overwrote it. The 68k
makes two of them without a slot in between whenever it does a long to `$C00000`, because the
port is mirrored at `$C00002` (genvdp.txt section 3) and a long is two word cycles, so every
long written across a full FIFO lost its first word: two bytes of VRAM, gone. Shinobi III
loses 6271 of them in the 600 frames the anchor window is taken over, and every one of the
3209 VRAM bytes that differed at frame 600 lies inside a word lost that way.

Queueing the write instead of latching it takes both carts to nothing at their anchors, and to
38.4 and 3.7 bytes a frame at end of frame, which is `phase`: no run survives, the way
`castlevania` and `vectorman` differ and nothing is wrong. The signature held as predicted,
and #541 was the same bug. Ristar's 107.0 remains, and its shape says it is a different one.

The three rows above and the three cart rows of the map were re-measured at this commit; the
rest of the map has not been re-run, and the queueing moves writes for every cart that makes
more than four of them across a full FIFO, so the next full sweep may move others.

## Ristar: the divergence is the game, not the VDP (#542)

Re-measured 2026-11-20 at `368d9ce`: 107.0 B/frame at `$4284`, unchanged, first difference
still `$00A897` ref=28 ours=16. Two measurements then took the finding apart.

**The anchor is not one moment in the game.** Both dumpers stop before the same instruction,
but they do not get there after the same amount of game. At arrival 54 blastem holds the
V-Int handler's own frame byte (`$FFAE10`) at 53 - one vertical interrupt per arrival, demo
not started - while ours holds the demo's object-walker counter (`$FF9FE2`) at 9 and has
already been through the transition that clears `$FFAE10`: our anchored legs span four to
six frames of game time each, blastem's span one. Nothing in the comparison can see that, so
the anchored table above compares VRAM from different attract-cycle phases. The whole
persistent window `$00A897-$00A987` is the sprite attribute table (R5=`$54`, SAT at
`$A800`, entries 18-31), which the game freezes once at the end of its demo - frozen at
different times with different content, it differs in every frame afterwards, which is
exactly the `real` shape.

**End of frame it is a 4-5 frame offset in that one event.** EOF work RAM agrees completely
through frames 1-14 and again through 100-180; the attract demo starts at frame 217 on ours
and 221-222 on blastem, and then advances at the same rate (ours at frame N is ref at
frame N+4: the demo counter reads 342 at ours 235 and ref 240). When the demo ends the game
freezes the SAT into VRAM - ours by EOF 385 (`16 82 32 83`), blastem still animating at
400 - and a one-shot freeze a few frames apart leaves bytes that differ in every later
frame. EOF VRAM today is 323.9 B/frame at measured shift -2, and the shift moves it:
phase-family numbers wearing a one-shot costume, not a write error.

**Why the gate opens early is the open question**, and it is a timing question, not a VDP
one. The registers at frame 214 are identical on both sides (`R1=$64`, `R10=$6F`,
`R0=$14`), so the difference lives in the ~200-frame title-screen phase before the demo -
the #530 systemic-lead family, where ~2% of a frame over 200 frames is four frames. The
V-Int handler even carries cycle-burning delays (`move.l #$B3,d7 / dbf`), the kind of loop
that turns a per-frame cycle lead into a frame count. The demo's object walker itself is at
PC `$054E84` (jump-table dispatch, chain next pointer at +72, one counter bump per active
node) and runs from the main loop - the HInt handler of this phase is a bare `rte` at
`$44B0`.

Also ruled out here, for #491's record: Ristar's R0 is `$14` in every frame of 580-600 (bit
4 set, bit 6 clear, the enable main decodes), and the #490 bit-6 flip moves the anchored
divergence 107.0 -> 115.2, away from zero. HInt-enable is not this ticket's cause.

## What phase looks like, so it does not get filed

`castlevania` differs by 266 VRAM bytes a frame and **nothing differs in every frame**. Same
for the 239.6 of `vectorman` (12 bytes do persist, none of them in a run) and for the 32.0 on
S&K that #534 measured. Sprites, scroll tables and attract animation: the two machines are
not on the same scanline of the same frame and the picture is not in question. A `phase`
row is not a clean row, it is a row that this comparison cannot judge, and the answer for it
is `--stop-at-pc`.

## Not covered

- **Anything behind a button press.** Neither dumper drives input, so this sweep compares
  attract screens. #525 lists the missing piece.
- **The Z80 and the cartridge.** Still not dumpable from the reference
  (`scripts/ref/README.md`, "Work RAM is what it reached first").

## The three carts the reference used to die on (#544)

Re-measured 2026-11-20 on current main. The deaths did not survive the diagnosis, and none
of them was inside blastem's emulation of these carts:

- **The walk never died on current main.** One serial run per cart on an idle box, window
  `586-600 --vdp`: all fifteen markers, all fifteen VRAM and CRAM image pairs, about 11 s
  each. Plain `blastem <rom>` under a bound timeout also runs `psii` fine.
- **Every run still exited 141.** Blastem answers `quit` by SIGKILLing the terminal process
  it exec'd (the harness's driver) and only then writes its exit-time output to the same
  FIFO: "Saved SRAM to ...", the last prompt. With the reader dead that write is a broken
  pipe, and blastem does not ignore SIGPIPE, so it died at the finish line having already
  written every dump. `run-ref.sh` then aborted before its size checks and printed nothing,
  which the sweep reads as a reference death. The fix is a FIFO reader blastem cannot kill:
  `run-ref.sh` now holds `/tmp/blastem_output` open itself for the whole run.
- **The original "3 of 15 dumps land" signature is the driver's own timeout, not a cart
  property.** `terminal-standin.py` has one deadline (`REF_DRIVE_TIMEOUT`, 120 s) for the
  whole session; when the box was loaded on sweep day, the 585-frame lead-in to the first
  dump outran it, the driver vanished mid-walk and blastem's next FIFO write died the same
  death. Today the walk is 11 s so it cannot fire here; it is still the shape to expect
  when a reference run dies partway through on a busy machine.
- **The private `HOME` was built but never exported.** The comment and the README promised
  `ram_init zero` and a clean boot; blastem actually ran on the real `HOME`, read the real
  `~/.gen`, and loaded save SRAM and EEPROM from `~/.local/share/blastem` - all three of
  these carts have one. `run-ref.sh` now exports it (plus the two XDG overrides), so the
  reference boots clean the way the docs always said it did.

With all that fixed, `sweep-vdp.sh --only psii,psiv,nbajam` walks all three and writes
rows: VRAM 0.0 bytes per frame on every one; `psii` agrees outright; `psiv` (0.5) and
`nbajam` (2.9) differ only in CRAM colours that never persist, the fade signature the
"What phase looks like" section above describes. The phase-safe pass at each cart's own
V-Int vector (`psii` `$5A90`, `psiv` `$524`, `nbajam` `$142094`) agrees: `nbajam` outright,
and the other two keep moving-only state (40.7 and 0.1 bytes a frame, no run of four in
any frame). Nothing here is a divergence to file; these three are now ordinary measured
carts.

One correction to the ticket's own title while the keys are being read anyway: the carts
are **Phantasy Star II, Phantasy Star IV and NBA Jam** (`psii`, `psiv`, `nbajam`), not
Psycho Pinball, Police Panic and NBA Jam Live.

## #530: is the ~1.5% cycle lead systemic?

**Mostly answered by this sweep, one cart short.** The meter needs a cart that parks a
progress counter in work RAM at V-Int, and the two best candidates (`psii`, `psiv`) now run
clean through the reference (#544), so what they park is reachable - but finding their
cursor means taking a work-RAM pair and reading a growing long out of the dumps by hand,
which was not in this ticket's budget. Golden Axe II anchors at `$DD60` and is meterable
with `scripts/ref/run-ref.sh --stop-at-pc 0xdd60` plus a work-RAM dump, with the same
caveat. Until a second cart is metered, the ~1.0-2.0% lead on #530 stays a single-cart
measurement.
