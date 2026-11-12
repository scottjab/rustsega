# Commercial game compatibility

Which real games boot, and what the suite says about them. The tests are
`crates/genesis/tests/commercial.rs` and the list they read is
`crates/genesis/tests/commercial_list.toml`; the ROMs are the owner's own set
under `$RUSTSEGA_ROM_DIR` (`docs/TESTING.md`, "External test data"), so nothing
here runs in CI and everything here runs with:

```sh
nix develop -c just test-commercial
```

One row of this table is one game. A game runs for its checkpoint with START
held on frames 300 and 301, and three things are checked: the machine did not
panic, the last frame has more than four distinct colours, and its hash is the
one in `crates/genesis/tests/golden/commercial.toml`.

## What a row does and does not prove

**It proves** that the cartridge loaded, that the 68000, the Z80, the VDP and
the sound chips survived to the checkpoint, and that the frame at the checkpoint
is byte-for-byte the frame the emulator drew when the row was blessed.

**It does not prove** the picture is correct. The goldens are regression
baselines: they were recorded from a frame and they catch the next change to it,
which is a much weaker claim than "this game is emulated correctly". The picture
behind each row is written to `target/test-output/commercial/<key>.png` on every
run - `golden key` below is the file name - so reviewing a row with eyes is one
run and one `xdg-open` away. Per-title review is still to do: no row here is
blessed as *right*, only as *unchanged*, and the weakest picture in the list is
`nbajam_600` at five distinct colours (an attract screen) against
`sonic3d_600` at ninety-six.

A mismatch is reported with the expected and actual hash and leaves the PNG
behind; bless it only the way `docs/TESTING.md` says
("Golden files"), never as a way to make the suite green.

## The list

| Game | ROM | Checkpoint | Golden key | Why this one |
|---|---|---|---|---|
| Sonic The Hedgehog | `Sonic The Hedgehog (USA, Europe).md` | 600 | `sonic1_600` | the smallest game that exercises the whole machine |
| Sonic The Hedgehog 2 | `Sonic The Hedgehog 2 (World).md` | 900 | `sonic2_900` | frame 600 is a fade to black on the way out of the title |
| Sonic 3D Blast | `Sonic 3D Blast (USA, Europe, Korea) (En).md` | 600 | `sonic3d_600` | banked/mega cartridge, and the richest picture of the set |
| Streets of Rage 2 | `Streets of Rage 2 (USA).md` | 600 | `sor2_600` | sprite-heavy, 68k-heavy |
| Gunstar Heroes | `Gunstar Heroes (Europe).md` | 600 | `gunstar_600` | the most scaled/rotated sprites the console has |
| Phantasy Star IV | `Phantasy Star IV (USA).md` | 600 | `psiv_600` | SRAM save |
| Phantasy Star II | `Phantasy Star II (USA, Europe).md` | 600 | `psii_600` | SRAM save |
| Contra - Hard Corps | `Contra - Hard Corps (USA, Korea) (En).md` | 600 | `contrahc_600` | sprite-per-frame pressure |
| Thunder Force IV | `Thunder Force IV (Europe).md` | 600 | `thunderforce4_600` | needs START at 300 *and* 301 to get past the menu into a picture |
| Castlevania - Bloodlines | `Castlevania - Bloodlines (USA).md` | 600 | `castlevania_600` | 16x16 tiles, DMA to the VDP |
| Shinobi III | `Shinobi III - Return of the Ninja Master (USA).md` | 600 | `shinobi3_600` | plane scrolling |
| Vectorman | `Vectorman (USA, Europe).md` | 900 | `vectorman_900` | chunky-tile plane A; frame 600 is mid-transition |
| Comix Zone | `Comix Zone (USA).md` | 600 | `comixzone_600` | window plane use |
| Mortal Kombat II | `Mortal Kombat II (World).md` | 600 | `mk2_600` | |
| NBA Jam | `NBA Jam (USA, Europe).md` | 600 | `nbajam_600` | EEPROM save |
| Super Street Fighter II | `Super Street Fighter II (USA).md` | 600 | `ssf2_600` | the SSF2 cart mapper |
| Road Rash II | `Road Rash II (USA, Europe).md` | 600 | `roadrash2_600` | |
| Ristar | `Ristar (USA, Europe).md` | 600 | `ristar_600` | |
| Toy Story | `Toy Story (USA).md` | 600 | `toystory_600` | |

No row in this table has a rendering defect open against it. Two checkpoints are
900 rather than 600, and say so in the table and in the list: the 600th frame of
Sonic 2 and of Vectorman is a black transition, and a golden of a black frame
would catch nothing.

## Held out of the list

Three more games were set up to run and panic before the checkpoint, so they are
not in `commercial_list.toml` yet:

| Game | What happens | |
|---|---|---|
| Aladdin (`Aladdin (USA).md`) | panics in `Lfo::tick` | [#349](https://github.com/scottjab/rustsega/issues/349) |
| Earthworm Jim (`Earthworm Jim (USA).md`) | panics in `Lfo::tick` | #349 |
| Ecco the Dolphin (`Ecco the Dolphin (USA, Europe, Korea) (En).md`) | panics in `Lfo::tick` | #349 |

The sound chip is what fails, not the video path, and each of the three goes
back into the list the moment #349 is fixed.

## Adding a game

Add a `[[game]]` block to `crates/genesis/tests/commercial_list.toml`, run
`just test-commercial` once with `RUSTSEGA_BLESS=1`, look at the PNG it writes,
and only then commit the golden. The key is `<key>_<frames>`, so changing how
long a game runs names itself in the golden and cannot silently compare against
a picture taken at another moment.
