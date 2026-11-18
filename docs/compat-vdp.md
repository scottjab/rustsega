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
cart's checkpoint is 900 frames. `runs/largest` is the run count and widest run, in bytes,
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
| `shinobi3` Shinobi III | **3235.2** | `$000029` ref=01 ours=00 | 1657/2 | agrees | **real** |
| `vectorman` Vectorman | 239.6 | `$002008` ref=A2 ours=C2 | 12/1 | 6.0/frame, none persistent | phase |
| `comixzone` Comix Zone | 0.0 | - | 0/0 | agrees | agree |
| `mk2` Mortal Kombat II | 0.0 | - | 0/0 | agrees | agree |
| `nbajam` NBA Jam | - | - | - | - | reference run failed |
| `psiv` Phantasy Star IV | - | - | - | - | reference run failed |
| `psii` Phantasy Star II | - | - | - | - | reference run failed |
| `ssf2` Super Street Fighter II | 0.0 | - | 0/0 | agrees | agree |
| `roadrash2` Road Rash II | **4653.0** | `$00000C` ref=55 ours=00 | 2450/2 | 3.9/frame, none persistent | **real** |
| `ristar` Ristar | **1232.7** | `$00811C` ref=08 ours=FF | 358/5 | agrees | **real** |
| `toystory` Toy Story | 0.0 | - | 0/0 | agrees | agree |

Nothing about CRAM has been confirmed yet: no cart has a colour differing in every frame of
its window, at end of frame or at an anchor. The three carts with moving colours are fades.

## The three confirmed ones

Each was re-run at its own V-Int entry so the two machines are stopped at one instruction,
and each still diverges there. Ranges and first bytes are from that phase-safe run, which is
the number to work from; the end-of-frame mean in the table above is the same divergence
plus phase.

| cart | anchor | VRAM mean/frame | range | runs/largest | first difference | CRAM |
|---|---|---:|---|---|---|---|
| `shinobi3` | `$522` | 3209.0 | `$000029-$00FBAD` | 1657/2 | `$000029` ref=01 ours=00 | agrees |
| `roadrash2` | `$11b8` | 4653.0 | `$00000C-$00F6D9` | 2450/2 | `$00000C` ref=55 ours=00 | agrees |
| `ristar` | `$4284` | 393.4 | `$00A895-$00EFF1` | 200/5 | `$00A895` ref=87 ours=83 | agrees |

Filed as [#540](https://github.com/scottjab/rustsega/issues/540) (Shinobi III),
[#541](https://github.com/scottjab/rustsega/issues/541) (Road Rash II) and
[#542](https://github.com/scottjab/rustsega/issues/542) (Ristar).

Two things worth carrying into those tickets. Shinobi III and Road Rash II are the same
signature - thousands of two-byte holes, mostly aligned to 4, ours reading 0 where the
reference holds data - which is not the shape of one dropped transfer (#536's was one
contiguous 1536-byte block) but of the same small loss repeated. Ristar is the odd one:
most of its differing bytes have data on *both* sides, so something was written there and
written differently, which is closer to what #404 is after than to either of the other two.

## What phase looks like, so it does not get filed

`castlevania` differs by 266 VRAM bytes a frame and **nothing differs in every frame**. Same
for the 239.6 of `vectorman` (12 bytes do persist, none of them in a run) and for the 32.0 on
S&K that #534 measured. Sprites, scroll tables and attract animation: the two machines are
not on the same scanline of the same frame and the picture is not in question. A `phase`
row is not a clean row, it is a row that this comparison cannot judge, and the answer for it
is `--stop-at-pc`.

## Not covered

- **Three carts.** `psii`, `psiv` and `nbajam` die in blastem's debugger with the driver
  pipe breaking before the dumps land (SIGPIPE, no output). It is the same failure mode for
  all three and it is not the emulator under test: our half dumps them fine. Someone with a
  second machine's worth of patience should look at whether `-g -d` needs a different
  invocation for those.
- **Anything behind a button press.** Neither dumper drives input, so this sweep compares
  attract screens. #525 lists the missing piece.
- **The Z80 and the cartridge.** Still not dumpable from the reference
  (`scripts/ref/README.md`, "Work RAM is what it reached first").

## #530: is the ~1.5% cycle lead systemic?

**Not answered by this sweep.** The meter needs a cart that parks a progress counter in work
RAM at V-Int, and the carts that would have said something (`psii`, `psiv`) are three of the
ones the reference run died on. Golden Axe II anchors at `$DD60` and is meterable with
`scripts/ref/run-ref.sh --stop-at-pc 0xdd60` plus a work-RAM dump, but finding its cursor
means reading a growing long out of those dumps by hand, which was not in this ticket's
budget. Until a second cart is metered, the ~1.0-2.0% lead on #530 stays a single-cart
measurement.
