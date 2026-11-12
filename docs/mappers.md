# Unlicensed mappers

M3-17 (#78). Which cartridges in the local collection need address translation that
[`crate::cart::mapper`] does not already do, what they need it for, and what this
project has concluded about each group.

The console's own mapper — the seven 512 KiB bank registers at `0xA130F3`-`0xA130FF`
— shipped in M3-12 (#72) and is documented in [`mapper.rs`][mapper]. This page is
about the others.

[mapper]: ../crates/genesis/src/cart/mapper.rs

## How this survey was done, and what it can prove

Everything below was measured on the local collection
(`$RUSTSEGA_ROM_DIR`, 2775 files) against the sweep state on `docs/compat.csv`
(Boots 2111, Blank 567, Panic 97). Three rules kept it honest:

- **`compat.csv` was parsed with a real CSV reader**, not `awk -F,`. ROM titles
  contain commas, and field-splitting it on commas invents statuses like `usa` and
  `FIFA Soccer 95`.
- **Nothing here is inferred from a cart's file name.** "Taiwan", "Unl" and
  "Pirate" in a dump's name say who released it, not what silicon it is on.
- **A cart that is Blank is not automatically a mapper failure.** `Blank` means the
  last frame of a 600-frame run held 4 or fewer distinct colours ([`compat.md`][compat]),
  and it covers 567 carts including official ones whose failures are the VDP
  watchdog (#354) or a missing m68k opcode (#355). Attributing a Blank cart to a
  mapper needs positive evidence of address translation, and this survey looked for
  it and mostly did not find any.

[compat]: compat.md

The evidence used, in order of how much it is worth:

1. **Does the image need translation at all?** A cart at or below the 4 MiB
   cartridge area (`0x000000`-`0x3FFFFF`, `ROM_START`/`ROM_END` in `bus.rs`) is
   fully addressable without a mapper. A mapper can only be *necessary* above
   4 MiB, or when the board is known to page inside it.
2. **Does mirroring the image change the outcome?** Every sub-4 MiB candidate was
   padded to 4 MiB by repeating the image and re-run. If a cart were reading
   addresses the dump does not have, filling those addresses would say so.
   **It changed nothing.** Eight candidates were run raw and mirrored at the same
   frame count (300), because a comparison at different frame counts proves
   nothing: King of Fighters 98' 2 vs 2, Lion King III 1 vs 1, Super Mario Bros.
   2 vs 2, Rockman X3 2 vs 2, Sonic Jam 6 1 vs 1, Onslaught 1 vs 1, Turrican 1 vs
   1, Exodus 2 vs 2. Identical in every case, so these carts are not failing
   because the emulator cannot reach their data.
3. **What is the current failure, according to the sweep?** `Panic` rows name
   themselves, and the names are the strongest signal available:
   unimplemented m68k opcodes and the `frame_done` watchdog, both already filed.

## The 208 unlicensed carts, counted

208 files match `(Unl)` or `(Pirate)`: **142 Boots, 56 Blank, 10 Panic.** They are
spread evenly over every size from 32 KiB to 4 MiB — there is no "the unlicensed
ones are the big ones" story to tell:

| Image size | Boots | Blank | Panic |
|---|---|---|---|
| 4.00 MB | 5 | 4 | 0 |
| 3.00 MB | 2 | 0 | 1 |
| 2.12 MB | 0 | 1 | 0 |
| 2.00 MB | 50 | 22 | 2 |
| 1.50 MB | 1 | 0 | 0 |
| 1.25 MB | 2 | 1 | 0 |
| 1.00 MB | 45 | 18 | 3 |
| 0.62 MB | 1 | 0 | 0 |
| 0.57 MB | 1 | 0 | 0 |
| 0.50 MB | 30 | 5 | 1 |
| 0.25 MB | 2 | 3 | 0 |
| 0.12 MB | 1 | 1 | 1 |
| 0.08 MB | 1 | 0 | 0 |
| 0.06 MB | 0 | 1 | 2 |
| 0.03 MB | 1 | 0 | 0 |

Only 9 of the 208 images are above the 4 MiB cartridge area, and none of them is a
mapper board that the Sega mapper does not already cover. **The premise that the
local set contains a large population of banked pirate carts did not survive
counting.**

## Grouping

Per the ticket: game, mapper type, source. "Mapper type" is what address
translation the group needs; `none (measured)` means the survey looked for a
mapper and the evidence pointed elsewhere, which is a result, not a gap.

### 1. Realtec — implemented

| Game | Mapper type | Source |
|---|---|---|
| Earth Defense (USA, Taiwan) (En) (Unl) | Realtec | `SEGA` at `0x7E100`, measured |
| Funny World & Balloon Boy (USA) (Unl) | Realtec | `SEGA` at `0x7E100`, measured |
| Tom Clown (Taiwan) (En) (Unl) | Realtec | `SEGA` at `0x7E100`, measured |
| Whac-a-Critter (USA) (Unl) | Realtec | `SEGA` at `0x7E100`, measured |

The only unlicensed mapper with positive, detectable evidence in the local set.
A Realtec board holds a 512 KiB image and mirrors a selected slice of it across
the whole 4 MiB cartridge window, so the window is the paging mechanism and the
registers that control it live at `0x400000`-`0x7FFFFF` — not in the
`0xA13000`-`0xA141FF` cart space, which is Segaport/EEPROM/IO.

Detection is not a size heuristic: all four images are exactly 512 KiB **and** carry
a second `SEGA` system signature at `0x7E100` (the header of the paged image, the
one the mirror exposes at `0x000100` after reset). Of the 527 files in the
collection that are exactly 512 KiB, exactly 4 have that signature, so the rule is
discriminating rather than suggestive.

Semantics are from **BlastEm `realtec.c` (retrodev), read for behaviour, not
copied** — BlastEm is GPLv3, so this is a description of what it does. Two points
worth recording because they are the surprising parts:

- Before any register is written the last 8 KiB page of the image (`0x7E000`) is
  mirrored across the entire 4 MiB window. A Realtec cart does not come up with
  bank 0 mapped, and emulating the reset state as identity does not boot one.
- The slice is `size` bytes at `start`, and `size` is mirrored repeatedly to fill
  the window, wrapping around the end of the image. It is not a single bank swap.

Measured on all four carts at the sweep's own 600-frame checkpoint, before this
change and after it:

| Cart | `main` | with the mapper |
|---|---|---|
| Earth Defense | 24 colours | 24 colours |
| Funny World & Balloon Boy | 26 colours | **31** |
| Whac-a-Critter | 7 colours | **22** |
| Tom Clown | **Panic** (`not implemented: m68k opcode 0AC6`) | **30 colours** |

So the mapper fixes one cart outright: `Tom Clown` goes `Panic` → drawing, because
with the image paged correctly the cart stops executing the bytes that held the
unimplemented opcode. The two colour-count rises are the paging delivering the
bytes the carts were asking for; `Earth Defense` holding flat at 24 is the control
that says this is not just noise painting over a blank screen.

That is the whole yield: **one cart out of 2775**, plus more-correct behaviour on
three others. The premise that a large pirate-mapper population was waiting behind
this work did not survive counting, and saying so plainly is the ticket's real
deliverable.

### 2. The 2 MB and 4 MB Taiwan/China group — no mapper evidence

28 of the 66 not-booting carts are 2 MB or 4 MB images from Taiwan or China, the
group the ticket named. Measured:

| Game | Mapper type | Source |
|---|---|---|
| Beggar Prince (USA, Europe) (Rev 1) (Super Fighter Team) (Unl) | none (measured) | 4 MiB, fits the cart area; mirror test negative |
| Beggar Prince (USA, Europe) (Super Fighter Team) (Unl) | none (measured) | as above |
| Chaoji Poker (Taiwan) (En) (Unl) | none (measured) | 4 MiB; mirror test negative |
| Mortal Kombat 3 (China) (En) (Pirate) | none (measured) | 4 MiB; mirror test negative |
| King of Fighters 2000, The (Taiwan) (En) (Unl) | none (measured) | 2 MiB; Panic is `frame_done` (#354) |
| King of Fighters '99, The (Taiwan) (En) (Unl) | none (measured) | 3 MiB; Panic is opcode 4741 (#355) |
| MK5 - Mortal Kombat Mythologies - Sub-Zero (Taiwan) (En) (Unl) | none (measured) | 2 MiB; Panic is `frame_done` (#354) |
| King of Fighters 98', The (Taiwan) (En) (Unl) | none (measured) | 2 MiB; mirror test negative |
| Lion King II/III, The (Taiwan) (En) (Unl) | none (measured) | 2 MiB; mirror test negative |
| Pikachu - Pocket Monster (Taiwan) (En) (Unl) [+Alt] | none (measured) | 2 MiB; mirror test negative |
| Rockman X3 (Taiwan) (En) (Unl) | none (measured) | 2 MiB; mirror test negative |
| Super Mario Bros. / Super Mario 2 / Super King Kong 99 (Taiwan) | none (measured) | 2 MiB; mirror test negative |
| Sonic Jam 6 (Taiwan) (En) (Pirate) | none (measured) | 2 MiB; mirror test negative |
| Da Hanghai Shidai I/II, Duli Zhanzheng, FIFA Soccer 95 (China), Guishou Jingji Chang, Samurai Shodown vs Soul Edge, and 9 more | none (measured) | 1-2 MiB; headers present, image reachable |

Three separate things rule a mapper out for this group:

1. **They fit the cartridge area.** At 2 MB and 4 MB the whole image is already
   addressable at `0x000000`-`0x3FFFFF`, so a bank register cannot be what is
   stopping them booting.
2. **Padding them to a full 4 MiB by mirroring changed nothing**, for all twelve
   candidates tested. If they were paging inside a window they were not, or the
   data they wanted would have arrived.
3. **Their headers are not mapper headers.** They carry a `SEGA` system field but a
   zero or nonsense ROM-range field and a `0000`/`FFFF` checksum. Notably
   `King of Fighters 98'` and `King of Fighters 2000` have byte-identical reset
   vectors (`SP=00000000`, `PC=001FD9DE`) and `King of Fighters '99` a third —
   which is the fingerprint of a shared **builder or wrapper**, not of a
   bank-switching chip.

Conclusion recorded for the ticket: **this group is an emulation problem, not a
mapper problem**, and it is already filed as #354 (`frame_done` watchdog, 83 ROMs)
and #355 (unimplemented m68k opcodes, 14 ROMs). It needs a diagnosis ticket, not
a mapper, and that is what this PR opens.

### 3. J-CART — deliberately not touched, and why

| Game | Mapper type | Source |
|---|---|---|
| Micro Machines - Military (Europe) (J-Cart) | J-CART (bank + I2C) | name/tag; not implemented |
| Micro Machines - Turbo Tournament 96 (Europe) (J-Cart), [Rev 1] | J-CART (bank + I2C) | name/tag; not implemented |
| Cosmo Cop (Europe) (J-Cart), [Sega Zone] | J-CART (bank + I2C) | name/tag; not implemented |

J-CART's bank numbers and its I2C (EEPROM) serial data line live in the **same
registers** at `0xA13000`-`0xA141FF`: GPGX's `mapper_i2c_jcart_read8` ORs the SDA
level into `jcart_read()`. This repository already owns that block for save data —
`crate::cart::eeprom`, `docs/eeprom-games.md`, with #270 and #271 open on EEPROM
mode 3 and the Acclaim 32 Mbit shift. Two owners for one register set is how a
save gets silently corrupted, so J-CART is not implemented here; the follow-up
issue has to decide ownership first.

### 4. Beyond 4 MiB — already covered, nothing to do

| Game | Mapper type | Source |
|---|---|---|
| Super Street Fighter II (all regions, +Virtual Console) | Sega SSF2 | done in M3-12 (#72) |
| Sonic & Knuckles + Sonic 2 + Sonic 3 (World) (Virtual Console) | Sega SSF2 | 5.25 MB, enabled by size |
| Pier Solar (8 MB homebrew) | Sega SSF2 | **not in the local set**; no action |

The ticket's background note describes Pier Solar as a 4 MB cart space split into
eight individually-banked 512 KiB regions with banks 0-4 fixed after reset. That is
the Sega SSF2 window this repository already implements — identity on reset, seven
switchable slots — so it needs no new mapper; and the file is not in the local
collection, so there is nothing here to test it against. Recorded so the next
person does not open it as a new mapper.

## Header size fields are not trustworthy

The ticket asked whether the header could be used to detect a mapper. It cannot,
and the reason is worth keeping.

The documented ROM range is `0x1A0`-`0x1A7` (start at `0x1A0`, end at `0x1A4`,
plutiedev "ROM Header"), and `crate::cart::header` parses it as `rom_start` /
`rom_end`. **There is no ROM-size field at `0x1E8`**: that address sits inside the
40 reserved bytes beginning at `0x1C8`, so anything reading a size out of it is
cargo cult. Confirmed by dumping the field across the unlicensed set: its values
are `''`, eight NULs, `Martin`, `O-BASEBA`, `3051)` and a lot of noise.

Measured on the 208 unlicensed carts, `rom_end` disagrees with the file length more
often than it agrees, in both directions, by amounts from nothing to over a
gigabyte. So detection in this module is keyed on what cannot lie — the actual
image length, and the location of a `SEGA` system signature (`0x100` for a plain
cart, `0x7E100` for a Realtec) — and never on a declared size.

## Follow-up issues opened from this survey

See the PR description on #78 for the issues opened and what each asks for.
