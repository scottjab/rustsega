# Compatibility sweep

`headless sweep` over the local ROM collection: every cartridge run for 600 frames of checkpoint and on to 1800 frames in its own child process with a 30s wall-clock limit. `Boots` means the run finished and some frame of it held more than 4 distinct colours - any frame, not the one at 600 - which is a claim about the machine, not about the game: no row here says the picture is right, only that something was drawn. A cart that drew nothing across the whole run is run again with a START press, and the gate that worked is named in the CSV's `input_gate` column. The full per-ROM table is [`compat.csv`](compat.csv); this page lists the problems.

| Status | ROMs |
|---|---|
| Boots | 2517 |
| Blank | 148 |
| Panic | 110 |

These tables are the #373 resweep: every cart runs for the 600-frame checkpoint *and on to 1800 frames*, every frame of that run watched for a picture, and a cart that never drew is run a second time holding START. One sampled frame was the wrong measurement - a title screen fades in, holds and fades out - so `Blank` used to mean "frame 600 held four colours or fewer" and now means "no frame of the run drew, with or without a button". Against the table below that is **Boots 2517 / Blank 148 / Panic 110** where it read Boots 2111 / Blank 567 / Panic 97: 416 carts move to `Boots`, 13 of them only once START is held (each named in the CSV's `input_gate` column and listed under "Boots with input" below), and the rest of the `Blank` set is the carts that genuinely never draw - 31 of them BIOS images, so 117 Mega Drive carts. `Panic` is up from 97 for the same reason: three times the frames find three times as many crashes, which is a better picture of what is broken, not a new fault.

## Known issues

One issue per cause, from the M6-13 triage (#133): `just test-all`,
`just test-commercial` and the 2775-ROM sweep. `just test-all` fails on five tests by
design: `virtua_racing_boots_to_non_blank_screen` until
[#433](https://github.com/scottjab/rustsega/issues/433) gives `Virtua Racing` its SVP,
and - all four of them, and only where the commercial collection is on disk - the
#375 carts `quackshot_rev_a_boots_to_non_blank_screen`,
`krustys_super_fun_house_boots_to_non_blank_screen`, `time_killers_boots_to_non_blank_screen`
and `lemmings_2_tribes_boots_to_non_blank_screen`. `just test-commercial` filters on
`commercial` in the test name, so it runs those and none of the save-state round trips
unless they are named too.

The count was six before [#362](https://github.com/scottjab/rustsega/issues/362) and is
five now, and it is worth saying why it did not simply go down by one. Before #362,
two of the three save-state tests were passing on a cancelled error: a state loaded
into a console whose mixer had never mixed a sample is behind its own mixer, so both
sides of `commercial_save_state_transplants_to_a_fresh_console` were silent for the
frames in question and silence matched silence. Fixing the mixer made both sides
produce audio, which is when the one cart that has a real save-state-adjacent bug -
`sonic3d`, on picture only - started to be able to fail it. So all three save-state
tests now run, all nineteen carts pass audio across a restore in both directions
(same console and fresh console), and one cart is held out by name for
[#450](https://github.com/scottjab/rustsega/issues/450), a VDP field the state does not
carry, which reproduces on main without #362 and is nothing to do with sound. Held out
means still run and still compared: divergence is reported and going clean is a
failure, so the exclusion cannot outlive the bug.

| Issue | Cause | ROMs |
|---|---|---|
| [#371](https://github.com/scottjab/rustsega/issues/371) | A mid-frame V28/V30 switch moves the VBlank line, so `run_frame`'s watchdog fires a frame early. 48 carts report `314 lines` (all PAL) and 35 report `263` (33 `usa`, 2 `japan`): the same event on the two rasters, since the guard is `lines_per_frame + 1`. [#354](https://github.com/scottjab/rustsega/issues/354) is the inventory of these 83 and should close into #371. | 83 |
| [#355](https://github.com/scottjab/rustsega/issues/355) | Unimplemented 68000 opcodes: `4741` x9, `00FF` x3, `00FA` x1. Seven of the nine `4741` hits are five betas of `NFL '95` and two Pro Action Replay carts, so it is narrower than nine separate instruction gaps. `0AC6` is gone since #78. | 13 |
| [#374](https://github.com/scottjab/rustsega/issues/374) | **A named gap, not an unexplained one:** the SVP - the Fujitsu SSP1601 DSP, its program ROM and its 2 KB of DRAM that the `Virtua Racing` cartridge carries and the 68k addresses at `0xA15000` and `0x300000` - is unemulated, so `Virtua Racing (USA)` and `(Japan) (En)` are expected to be blank. Measured on this tree: `headless run "Virtua Racing (USA).md" --frames 1800 --max-colours` is `max_colours=1` with no input, and the `17 colours` in the START-gated table below is the same cart poked, not its 3D layer. `virtua_racing_boots_to_non_blank_screen` in `crates/genesis/tests/boot.rs` is the measurement, and [#433](https://github.com/scottjab/rustsega/issues/433) is what is left. | 2 |
| [#375](https://github.com/scottjab/rustsega/issues/375) | **Diagnosed; the causes are [#435](https://github.com/scottjab/rustsega/issues/435)-[#441](https://github.com/scottjab/rustsega/issues/441) and they are not one bug.** 28 retail carts that draw pure black at all 21 sampled frame counts, with and without input. Where it started and what it turned out to be is the section below. | 28 |
| [#357](https://github.com/scottjab/rustsega/issues/357) | The 2 MB/4 MB Taiwan and China unlicensed carts, which are not a mapper problem. 26 of them are still in the never-drawing set; listed on the issue. | 26 |
| [#373](https://github.com/scottjab/rustsega/issues/373) | **Fixed by the resweep below.** The sweep sampled one frame, at 600, with no pad connected; watching the whole run and holding START moves 416 of these carts to `Boots`, 13 of them only with the START press. Nothing here is a fix to the emulator: the measurement was wrong. | 416 |
| [#362](https://github.com/scottjab/rustsega/issues/362) | **Fixed.** Loading a state left the mixer ahead of the machine, so the console went silent for as many frames as it had run since the save, and `load_state` now re-anchors it (`audio::Mixer::seek`). Audio across a restore is bit-exact on all nineteen carts from the frame the mixer's output filters settle on, about an eighth of a second later; see `docs/save-state-format.md`. | - |
| [#450](https://github.com/scottjab/rustsega/issues/450) | A state restored mid-game draws one wrong picture in `Sonic 3D` (frame 381 of the round trip) - something the state does not carry, picture only, present on main independently of #362. It is the only cart of the nineteen that does not round-trip, and it is named in `HELD_OUT` in `crates/genesis/tests/save_state.rs` rather than the sweep being weakened. | 1 |

### The 28 black carts of #375, and what they turned out to be

[#375](https://github.com/scottjab/rustsega/issues/375) was the diagnosis ticket for the
largest block of never-drawing retail carts: 28 of them, black at every one of the sweep's
21 sampled frame counts, with and without a START press. It asked one question - **did the
68000 ever get as far as initialising the VDP** - and the answer is that 26 of the 28 never
write a single word of CRAM, so for those the VDP is not involved at all: it is sitting in
its reset state, displaying the black backdrop of a disabled display, because the program
that was going to set it up stopped first.

**The single most useful measurement is not a colour count, it is "how many of the 64 CRAM
words are non-zero"**, taken from the public `Genesis::bus().vdp` fields at every frame
boundary alongside the 68000's `pc`. 26 of the 28 have zero CRAM words, and for most of
them the `pc` histogram over 1800 frames is one or two addresses wide - a stopped program,
not a broken pipeline. Only two of the 28 ever load a palette, and those two are the render
problem the ticket expected rather than the boot problem it feared.

The causes, one issue each, and the cart that measures them. The `Carts` column counts per
issue, so the three carts that sit under two causes - `QuackShot (Rev A)`, which halts and
is also truncated, and `Caesar no Yabou II` and `Back to the Future Part III (Europe)`,
which are named on the inventory of [#440](https://github.com/scottjab/rustsega/issues/440)
as well - are counted twice; all 28 are covered, and no cart is left unassigned.

| Cause | Issue | Carts |
|---|---|---|
| **The image is not the cart.** The header declares a ROM size past the end of the file, the cart's own boot self-test reads the mirrored space up there, gets the `0xFF` filler of `Bus::read_rom_byte`, and fails. `QuackShot Starring Donald Duck (World) (En,Ja) (Rev A)` is the proof rather than the suspicion: it is `0x80000` and declares `0x13FFFF`, while `(Rev A) (Alt)` is `0x140000`, declares the same `0x13FFFF`, verifies, and draws 33 colours. Same revision, same code, complete dump. | [#435](https://github.com/scottjab/rustsega/issues/435) | 4 |
| **The 68000 stops itself.** Within 30 frames the program reaches a `NOP; NOP; BRA.S $self`, a `BRA.S $self`, or a `STOP #$2700` in its own start-up code and never leaves it - so no R1 display enable, no palette, nothing. The halt sits in the same block of start-up code as the security write (`MOVE.L #$53454741` - `SEGA` - to `$A14000`), so it is a boot-check failure exit; which check fails is what is left to establish. Forcing the region does not move any of them. | [#436](https://github.com/scottjab/rustsega/issues/436) | 7 |
| **The Z80 bus-request read never says what the cart is waiting for.** `Time Killers` spins for 100% of its frames in `MOVE.W $A11100,D0` / `BTST #0,D0` / `BNE -6`: the documented poll between asking the sound CPU for its bus and touching the sound side. A *word* read of that register answers the flag in bit 8 and leaves bit 0 a fixed `1` (`busreq_word` in `crates/genesis/src/bus.rs`), so bit 0 of the word can never be the `0` the cart is polling for. | [#437](https://github.com/scottjab/rustsega/issues/437) | 2 |
| **It only boots in a region it does not ask for.** `usa` and `japan` give 1 colour, `europe` gives 12 and 30. Region forcing is a diagnostic, not a fix - it is the cart's own branch off a read of `0xA10001` that is the question. | [#438](https://github.com/scottjab/rustsega/issues/438) | 3 |
| **A busy-wait on a VDP read that never changes.** `Mega lo Mania` and `Tyrants` (one engine, `MEGA lo MANIA`) sit on `MOVE.W $C00004,D0` / `BTST #7,D0`; `Bubble and Squeak` sits on `CMPI.B #$E0,$C00008`, waiting for the beam to reach line 224. This is the "waiting on a status bit that never clears" branch of the triage, and for these three the wait is disassembled rather than inferred - which of #302/#303 or #77/#308 is responsible is not yet established. | [#439](https://github.com/scottjab/rustsega/issues/439) | 3 |
| **Measured, and no cause yet.** Ten carts run at 1800 frames with a `pc` histogram, a CRAM/VRAM count and a three-region sweep and still do not sort into a cause. Filed as an inventory rather than filed under a guess. | [#440](https://github.com/scottjab/rustsega/issues/440) | 10 |
| **The VDP is set up and the picture is still black.** `Al Michaels Announces HardBall III` and `Janou Touryuumon` are the only two of the 28 whose display is enabled *and* whose palette is loaded - 60 and 28 non-zero CRAM words, 2558 and 17552 non-zero VRAM bytes - and whose frame is 2 colours after 1800 of them. That is the branch the ticket called much easier, and it is a render/enable question, not a boot one. | [#441](https://github.com/scottjab/rustsega/issues/441) | 2 |

Three of the carts of the first row are worth naming because they change what to look for
next, and one of them is a false lead that cost time:

- `Triple Play - Gold Edition` and `Triple Play 96` are `0x300000` images declaring
  `0x3FFFFF`, and they are the `00FF` opcode of the #355 row. Padding one of them to the
  `0x400000` its header declares **removes the panic** - so `00FF` is being executed past
  the end of the data the dump holds, and #355 is being triggered by a short image rather
  than by a real instruction gap on those carts. It still draws one colour, because
  padding is not the missing data.
- `Phantasy Star Fukkokuban (Japan)` has a size field of `0x2120C211`, which is not a size,
  and its `pc` runs away to `$D3BFC9FF` - an odd, unmapped address - rather than halting.
- Rewriting a header checksum to the value this tree computes for it does **not** unstick
  `Warrior of Rome II` or `Caesar no Yabou II`, both of which fail their own checksum
  check, so "the checksum is what is wrong" is not a general answer here either. It is
  recorded on [#436](https://github.com/scottjab/rustsega/issues/436) so nobody retries it.

The method, so it can be repeated: `headless run <rom> --frames 1800 --colours
--max-colours` for the colour count, the same under `--region usa|japan|europe` for the
region column, and a throwaway probe over the public `Genesis::cpu()` /
`Genesis::bus().vdp` fields for the `pc`-at-frame-boundary histogram and the CRAM and VRAM
counts. The probe was deleted rather than committed: it is a diagnostic, and the four
`#[ignore]`d boot tests in `crates/genesis/tests/boot.rs` are what replaces it.

### Deliberately out of scope

These are in `~/games/roms/genesis` and are counted as failures by the sweep, but
they are not Mega Drive games this emulator can run, so they are listed rather than
filed. The set they are counted over is the 148 `Blank` rows below (31 of them `[BIOS] *` images); the per-cause counts are the #133 triage's and are
not recomputed here:

- **31 BIOS images** (`[BIOS] *.md`): Sega CD 1/2, CDX, WonderMega, X'Eye, Teradrive
  and the Mega Drive/Genesis boot ROMs. Sega CD and 32X are out of scope for the
  project, and the rest need a lock-on or TMSS path rather than a bug fix.
- **20 development, download and program carts**: `Sega Channel` x4,
  `Sega Game Toshokan`, `Wonder Library`, `Wonder MIDI`, `Sega Art Tool`,
  `Pro CDX`, `MD Soft Checker`, the seven `Sega Net Cartridge` / `Game no Kanzume`
  net-cart images (`16t`, `Aworg`, `Hyper Marbles` and the four
  `Phantasy Star II` scenario carts), and three HeartBeat Catalyst prototypes
  (`NHLPA Hockey`, `Outworld 2375 A.D.`, `PGA Tour Golf II`). They need a modem, a
  Mega CD, or dev hardware.
- **8 re-release container images**: the four `Sonic Classic Collection` files, two
  `Mega Drive Mini 2` files (`Futari de Puyo Puyo Sun`, `VS Puyo Puyo Sun`) and two
  Virtual Console dumps (`Dyna Brothers 2 Special`, `Phantasy Star II (Rev A)`).
  They do carry Mega Drive headers - the sweep reads titles and serials off them - so
  unlike the entries above these may be real Mega Drive images. They are listed here
  rather than filed only because a container-specific boot path is the obvious first
  thing to rule out before anyone spends a session on them.
- **1 lock-on-only image**: `Sonic & Knuckles (World) (Lock-on).bin`, which is the
  lock-on child, not a standalone cart.
- Betas and protos are not separately filed: the 25 in the never-drawing set are
  builds of titles that draw fine in their retail form, and should fall with
  whatever fixes their parent (#375, #357).

### Method, so the numbers can be repeated

`headless sweep ~/games/roms/genesis --jobs 24` takes 422 s (7 min) wall clock on 28 cores: a 600-frame checkpoint (`--frames`), a 1800-frame watched run (`--run-frames`, three times the checkpoint) and a second pass holding `START@300,START@301` for any cart that drew nothing (`--input-gate none` switches that pass off). `distinct_colors` and `frame_hash` are still the checkpoint frame's, so the CSV diffs against the old table; `max_colours` and `input_gate` are #373's columns. The wider per-ROM sampling behind #373 and #375 was `headless run <rom> --colours` at the frame counts 60, 120, 180, 240, 300, 420, 540, 600, 700, 900, 1200, 1500, 2100, 2400, 2700 and 3600, with and without `--pad1`, and `identify -format %k` plus `convert ... txt:-` on `--dump-png` output to read back what the single colour actually was (`#000000`).
on 28 cores. The wider per-ROM sampling behind #373 and #375 was
`headless run <rom> --colours` at the frame counts 60, 120, 180, 240, 300, 420, 540,
600, 700, 900, 1200, 1500, 2100, 2400, 2700 and 3600, with and without `--pad1`, and
`identify -format %k` plus `convert ... txt:-` on `--dump-png` output to read back
what the single colour actually was (`#000000`).

### Boots with input (13 - drew nothing unperturbed, drew once START@300,START@301 was held)

| ROM | Title | Notes |
|---|---|---|
| `Baby Boom (USA) (Proto) (1994-08-11).md` | BABY BOOM S | 29 colours with START@300,START@301 |
| `Brave Battle Saga - Legend of the Magic Warrior (USA, Europe) (Unl).md` | Brave Battle Saga - Legend of the Magic Warrior | 23 colours with START@300,START@301 |
| `Chaoji Dafuweng (Taiwan) (Unl).md` | ................................................ | 37 colours with START@300,START@301 |
| `Mega Bomberman - Special 8-Player-Demo (Europe) (Proto).md` | unknown | 33 colours with START@300,START@301 |
| `RoboCop Versus The Terminator (USA) (Beta) (1993-04-30).md` | GLOBAL GLADIATOR | 47 colours with START@300,START@301 |
| `Robot Wreckage (USA) (Beta) (1992-07-xx).md` | unknown | 30 colours with START@300,START@301 |
| `Star Wars (USA) (Proto) (1993-01-25).md` | GREENDOG | 39 colours with START@300,START@301 |
| `Taikong Zhanshi - Mofa Zhanshi (Barver Battle Saga) (Taiwan) (Unl).md` | N.....N....ZN.....N....pF.L.\|.NsJ9..`.g...BA.9.. | 23 colours with START@300,START@301 |
| `Tom Mason's Dinosaurs for Hire (USA) (Beta) (1993-03-xx).md` | DINO'S FOR HIRE | 39 colours with START@300,START@301 |
| `Virtua Racing (Europe) (Beta).md` | Virtua Racing | 17 colours with START@300,START@301 |
| `Virtua Racing (Europe).md` | Virtua Racing | 17 colours with START@300,START@301 |
| `Virtua Racing (Japan) (En).md` | Virtua Racing | 17 colours with START@300,START@301 |
| `Virtua Racing (USA).md` | Virtua Racing | 17 colours with START@300,START@301 |

### Blank (148 - see the CSV for the full table)

| ROM | Title | Notes |
|---|---|---|
| `16t (Japan) (Game no Kanzume Vol. 2) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `ATP Tour (Europe).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-07-19).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-07-23).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-02).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-05).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-08).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-09-08).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA).md` | ATP Tour Championship Tennis | 2 colours |
| `Al Michaels Announces HardBall III (USA, Europe).md` | HardBall III | 2 colours |
| `Arcade Legends Sega Mega Drive (World).md` | KID CHAMELEON | 1 colours |
| `Arcade Legends Street Fighter II' - Special Champion Edition (World).md` | STREET FIGHTER.U'SPECIAL CHAMPION EDITION | 1 colours |
| `Aworg - Hero in the Sky (Japan) (Game no Kanzume Vol. 2) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Baby Boom (USA) (Proto) (1994-06-03).md` | BABY BOOM S | 4 colours |
| `Back to the Future Part III (Europe).md` | BTTF3 | 1 colours |
| `Beavis and Butt-Head (USA) (Beta).md` | Beavis and Butthead | 1 colours |
| `Body Count (Brazil) (En) (Beta) (1994-01-27).md` | BODY COUNT | 2 colours |
| `Bubble and Squeak (USA).md` | BUBBLE AND SQUEAK | 1 colours |
| `Bug's Life, A - Chong Chong Weiji (Taiwan) (En) (Unl).md` | ........g..,N....jE.... G...u.p.N....R..... .. | 1 colours |
| `Burning Force (World) (Evercade).md` | BURNING FORCE | 2 colours |
| `Caesar no Yabou II (Japan).md` | worrior of rome2 | 1 colours |
| `Chaoji Poker (Taiwan) (En) (Unl).md` | ................................................ | 1 colours |
| `Demolition Man (USA, Europe) (Beta).md` | Demolition Man | 2 colours |
| `Desert Demolition (USA, Europe) (Beta) (1994-12-06).md` | DESERT DEMOLITION | 1 colours |
| `Desert Demolition (USA, Europe) (Beta) (1994-12-12) (Alt).md` | DESERT DEMOLITION | 1 colours |
| `Dyna Brothers 2 (Japan).md` | .... ........2 | 1 colours |
| `Dyna Brothers 2 Special (Japan) (Virtual Console).md` | .... ........2 .............. | 1 colours |
| `Fengkuang Taohuayuan (Taiwan) (Unl).md` | unknown | 1 colours |
| `Futari de Puyo Puyo Sun (Japan) (Mega Drive Mini 2).md` | PUYO PUYO SUN | 1 colours |
| `GEMS (USA) (v2.8) (Proto) (1994-06-04) (Program).md` | GEMS | 2 colours |
| `Gouketsuji Ichizoku (Japan).md` | POWER INSTINCT | 1 colours |
| `Guaishou Jingji Chang - Pokemon Stadium (Taiwan) (En) (Unl).md` | E.....G.....N.....E....`G.....N.....N.../.N....B | 1 colours |
| `Gunfight 3 in 1 (Taiwan) (En) (Unl).md` | .Enterprises Ltd. | 1 colours |
| `Heavy Nova (USA).md` | HEAVY NOVA | 1 colours |
| `Hyper Marbles (Japan) (Game no Kanzume Vol. 1) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Janou Touryuumon (Japan).md` | ...c.o.... | 2 colours |
| `Jurassic Park - Rampage Edition (USA, Europe) (Beta) (1994-06-20).md` | JAPAN GAME NAME | 1 colours |
| `Jurassic Park - Rampage Edition (USA, Europe) (Beta) (1994-06-22).md` | JAPAN GAME NAME | 1 colours |
| `Krusty's Super Fun House (USA, Europe) (Rev A).md` | KRUSTYS SUPER FUNHOUSE | 1 colours |
| `Krusty's Super Fun House (USA, Europe).md` | KRUSTYS SUPER FUNHOUSE | 1 colours |
| `Lemmings 2 - The Tribes (Europe).md` | LEMMINGS 2 THE TRIBES | 1 colours |
| `Lemmings 2 - The Tribes (USA).md` | LEMMINGS 2 THE TRIBES | 1 colours |
| `Lion King II, The - Simba's Mighty Adventure (Taiwan) (En) (Unl).md` | ................................................ | 1 colours |
| `Lion King III, The (Taiwan) (En) (Unl).md` | Of Sega Enterprises Ltd. | 1 colours |
| `MD Soft Checker (USA) (v0.30) (Proto) (Unl).md` | MD Soft Checker Version 0.30**Prg. by papi** | 2 colours |
| `Mahjong Emo Ta - Devilish Mahjong Tower (Taiwan) (Unl).md` | "\|.... <.........n..Q...N.....3..D.....9......g. | 1 colours |
| `Mamono Hunter Youko - Dai 7 no Keishou (Japan).md` | .....n...^.[.d.q | 1 colours |
| `Mega lo Mania (Japan).md` | MEGA lo MANIA | 1 colours |
| `Mr. Nutz - Hoppin' Mad (Europe) (Proto).md` | MR NUTZ HOPPIN MAD | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1994-12-15).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-08).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-12).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-15).md` | NBA Basketball '95 | 1 colours |
| `NFL '95 (USA) (Beta) (1994-09-05) (Alt).md` | NFL 95 | 1 colours |
| `NFL '95 (USA) (Beta) (1994-09-05).md` | NFL 95 | 1 colours |
| `NHLPA Hockey (USA) (Proto) (HeartBeat Catalyst).md` | NHLPA HOCKEY | 1 colours |
| `Onslaught (USA, Europe) (Unl).md` | ONSLAUGHT | 1 colours |
| `Outback Joey (USA).md` | OUTBACK JOEY | 1 colours |
| `Outworld 2375 A.D. (USA) (HeartBeat Catalyst).md` | OUTWORLD | 1 colours |
| `PGA Tour Golf II (USA) (HeartBeat Catalyst).md` | PGA Tour Golf | 1 colours |
| `Phantasy Star Fukkokuban (Japan).md` | ....:....!.:....!.. o..g~#fo..2...2...2......... | 1 colours |
| `Phantasy Star II (USA, Europe) (Rev A) (Virtual Console).md` | PHANTASY STAR 2 | 1 colours |
| `Phantasy Star II - Amia's Adventure (Japan) (Game no Kanzume Vol. 1).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Anne's Adventure (Japan) (Game no Kanzume Vol. 2).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Huey's Adventure (Japan) (Game no Kanzume Vol. 1).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Kinds's Adventure (Japan) (Game no Kanzume Vol. 2).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star IV (USA) (Beta) (1994-11-07).md` | PHANTASY STAR The end of the millennium | 1 colours |
| `Pikachu - Pocket Monster (Taiwan) (En) (Unl) (Alt).md` | ........g..<N...T.E....@G.....N.....0<..E....`G. | 1 colours |
| `Pikachu - Pocket Monster (Taiwan) (En) (Unl).md` | ........g..<N...T.E....@G.....N.....0<..E....`G. | 1 colours |
| `Pro CDX (USA, Europe) (v1.70) (Program) (Unl) [b].md` | CDX BINARY IMAGE | 2 colours |
| `QuackShot Starring Donald Duck (World) (En,Ja) (Rev A).md` | QUACK SHOT STARRING DONALD DUCK | 1 colours |
| `Quad Challenge (World) (Evercade).md` | QUAD CHALLENGE | 2 colours |
| `Richard Scarry's Busytown (USA) (Beta) (1994-08-26) (Alt).md` | BUSYTOWN | 2 colours |
| `Rockheaven (Asia) (Ja) (Pirate).md` | .A...N.X.L.b.h .V...... | 3 colours |
| `Rockman X3 (Taiwan) (En) (Unl).md` | unknown | 3 colours |
| `Rockworld (Asia) (Ja) (Pirate).md` | .... ..... | 1 colours |
| `Samurai Shodown vs Soul Edge (Taiwan) (En) (Unl).md` | R....,........g...N...A..9.....(g..,...... .g | 1 colours |
| `Sega Channel (Japan) (v2.11) (Earlier) (Program).md` | Not Support | 1 colours |
| `Sega Channel (Japan) (v2.11) (Program).md` | Not Support | 1 colours |
| `Sega Channel (USA) (General Instrument) (Program).md` | Sega Channel | 1 colours |
| `Sega Channel (USA) (Scientific Atlanta) (Program).md` | Sega Channel | 1 colours |
| `Sega Game Toshokan (Japan) (En) (Rev A) (Program).md` | .Z.K.Q.[...}.... | 3 colours |
| `Sesame Street Counting Cafe (USA).md` | Sesame Street: Counting Cafe | 1 colours |
| `Shiliu Zhang Mahjong II (Taiwan) (Unl).md` | ................................................ | 1 colours |
| `Sonic & Knuckles (World) (Lock-on).bin` | .D.@.>0;..N.. R.. L...Ns......... ... ..... .. . | 1 colours |
| `Sonic & Knuckles (World) (Sonic Classic Collection).md` | SONIC & KNUCKLES | 1 colours |
| `Sonic Jam 6 (Taiwan) (En) (Pirate).md` | Of Sega Enterprises Ltd. | 1 colours |
| `Sonic The Hedgehog (USA, Europe) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG | 1 colours |
| `Sonic The Hedgehog 2 (World) (Rev A) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG 2 | 1 colours |
| `Sonic The Hedgehog 3 (USA) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG 3 | 1 colours |
| `Splatterhouse 2 (World) (Evercade).md` | SPLATTER HOUSE 2 | 2 colours |
| `Splatterhouse 3 (World) (Evercade).md` | SPLATTER HOUSE 3 | 2 colours |
| `Star Cruiser (Japan).md` | STAR CRUISER/MD | 1 colours |
| `Super King Kong 99 (Taiwan) (En) (Unl).md` | Of Sega Enterprises Ltd. | 1 colours |
| `Super Mario Bros. (Taiwan) (En) (Unl).md` | SUPER MARIO BROS | 2 colours |
| `Super Mario World 64 (Taiwan) (En) (Unl).md` | unknown | 2 colours |
| `Taipingyang Zhengba Zhan (China) (Pirate).md` | P.T.O. | 1 colours |
| `Taiwan Daheng (Taiwan) (Unl).md` | "\|.... <.........n..Q...N.....3..D.....9......g. | 1 colours |
| `Tecmo Cup Football Game (Europe) (Proto).md` | TECMO CUP | 1 colours |
| `Time Killers (Europe).md` | TIMEKILLERS | 1 colours |
| `Time Killers (USA).md` | TIME KILLERS | 1 colours |
| `Tiny Toon Adventures 3 (Taiwan) (En) (Unl).md` | GHTS RESERVED OF XXXXXX LTD. | 1 colours |
| `Turrican (USA, Europe) (Unl).md` | TURRICAN | 1 colours |
| `Twinkle Tale (Japan).md` | TWINKL TALE | 1 colours |
| `Tyrants - Fight Through Time (USA).md` | MEGA lo MANIA | 1 colours |
| `VS Puyo Puyo Sun (USA, Europe) (Mega Drive Mini 2, Genesis Mini 2).md` | PUYO PUYO SUN | 1 colours |
| `Warrior of Rome II (USA).md` | worrior of rome2 | 1 colours |
| `Wisdom Tree Collection (USA) (Piko Interactive) (Unl).md` | INC..09....09... 09... .9.... .. g.#.SEGA..@.09 | 4 colours |
| `Wonder Library (Japan) (Program).md` | WONDER LIBRARY | 1 colours |
| `Wonder MIDI (Japan) (En) (Program).md` | MIDI World | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-14).md` | JAPAN GAME NAME | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-18).md` | JAPAN GAME NAME | 1 colours |
| `World Series Baseball (USA) (Beta) (1993-12-22).md` | BASEBALL WORLD SERIES | 1 colours |
| `World Series Baseball (USA) (Beta) (1994-03-04).md` | BASEBALL WORLD SERIES | 1 colours |
| `Worms (Europe) (Beta).md` | WORMS | 2 colours |
| `X-Men 2 - Clone Wars (USA, Europe) (Beta) (1994-10-18).md` | X-MEN 2 | 1 colours |
| `Zool (Europe) (Beta) [b].md` | Zool | 1 colours |
| `[BIOS] Aiwa CSD-GM1 (Japan).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] LaserActive (Japan) (v1.02).md` | MEGA-LD BOOT ROM | 1 colours |
| `[BIOS] LaserActive (Japan) (v1.05).md` | MEGA-LD BOOT ROM | 1 colours |
| `[BIOS] LaserActive (USA) (v1.02).md` | MEGA-LD BOOT ROM | 2 colours |
| `[BIOS] LaserActive (USA) (v1.04).md` | MEGA-LD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Asia) (Ja) (Rev H).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Europe).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Beta).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev A).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev B).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev C).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev D).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev E).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (Rev H).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD (Japan) (v1.11B).md` | MEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Mega-CD 2 (Europe) (Rev A).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Mega-CD 2 (Europe) (v2.11).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Mega-CD 2 (Europe).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Mega-CD 2 (Japan).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Multi-Mega (Europe).md` | CDX BOOT ROM | 1 colours |
| `[BIOS] Sega CD (USA) (Rev B).md` | SEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Sega CD (USA).md` | SEGA-CD BOOT ROM | 1 colours |
| `[BIOS] Sega CD 2 (USA) (Rev A).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Sega CD 2 (USA).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Sega CD 2A (USA).md` | CD2 BOOT ROM | 1 colours |
| `[BIOS] Sega CDX (USA).md` | CDX BOOT ROM | 1 colours |
| `[BIOS] Sega Mega Drive - Genesis - Boot ROM (World).md` | GENESIS OS | 2 colours |
| `[BIOS] Sega Teradrive - TMSS (Japan) (En).md` | TERA286 INITIALIZE | 1 colours |
| `[BIOS] WonderMega (Japan) (En).md` | WONDER-MEGA BOOT | 1 colours |
| `[BIOS] WonderMega M2 (Japan) (En).md` | WONDERMEGA2 BOOT | 1 colours |
| `[BIOS] X'Eye (USA).md` | X EYE BOOT for(MP) | 1 colours |

### Panic (110 - see the CSV for the full table)

| ROM | Title | Notes |
|---|---|---|
| `Boxing Legends of the Ring (USA).md` | BOXING LEGENDS OF THE RING | 263 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket (Australia).md` | BRIAN LARA CRICKET | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket (Europe).md` | BRIAN LARA CRICKET | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket 96 (Europe) (Rev 1).md` | BRIAN LARA 96 | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket 96 (Europe).md` | BRIAN LARA 96 | 314 lines and the VDP has not raised `frame_done` |
| `Bubsy in - Claws Encounters of the Furred Kind (USA, Europe).md` | Bubsy | 263 lines and the VDP has not raised `frame_done` |
| `Chaoji Mahjong Club (Taiwan) (Unl).md` | Super Majon ClubVersion 1.00 Bill,Will & Andy | not implemented: m68k opcode 4741 |
| `Chavez II (Mexico).md` | CHAVEZ 2 | 263 lines and the VDP has not raised `frame_done` |
| `Comix Zone (Europe) (Beta) (1995-06-28).md` | COMIX ZONE | 314 lines and the VDP has not raised `frame_done` |
| `Comix Zone (Europe) (Beta) (1995-07-12).md` | COMIX ZONE | 314 lines and the VDP has not raised `frame_done` |
| `Cyber Police - ESWAT (Japan) (En).md` | ESWAT | 263 lines and the VDP has not raised `frame_done` |
| `Darius (World) (Genesis Mini, Mega Drive Mini).md` | DARIUS MD | 263 lines and the VDP has not raised `frame_done` |
| `Dragon - The Bruce Lee Story (Europe).md` | DRAGON | 314 lines and the VDP has not raised `frame_done` |
| `Dragon's Revenge (Japan) (En).md` | DRAGON'S FURY2 | 263 lines and the VDP has not raised `frame_done` |
| `Dragon's Revenge (USA, Europe).md` | DRAGON'S REVENGE | 263 lines and the VDP has not raised `frame_done` |
| `ESWAT - City under Siege (USA, Europe) (Rev A).md` | ESWAT | 263 lines and the VDP has not raised `frame_done` |
| `Ecco - The Tides of Time (Europe) (En,Fr,De,Es).md` | ECCO THE TIDES OF TIME | 314 lines and the VDP has not raised `frame_done` |
| `Exo Squad (Europe).md` | EXO-SQUAD | 314 lines and the VDP has not raised `frame_done` |
| `International Sensible Soccer (Europe) (En,Fr,De,It).md` | SENSIBLE SOCCER-INTERNATIONAL EDITION | 314 lines and the VDP has not raised `frame_done` |
| `John Madden Football '93 (USA, Europe).md` | John Madden Football '93 | 263 lines and the VDP has not raised `frame_done` |
| `John Madden Football (USA, Europe) (1990-11-07) (Sega Channel).md` | DO NOT USE | 263 lines and the VDP has not raised `frame_done` |
| `John Madden Football (USA, Europe).md` | DO NOT USE | 263 lines and the VDP has not raised `frame_done` |
| `John Madden Football - Championship Edition (USA).md` | John Madden Football '93 Championship Edition | 263 lines and the VDP has not raised `frame_done` |
| `Jurassic Park (Europe) (1993-06-14) (Sega Channel).md` | JURASSIC PARK | 314 lines and the VDP has not raised `frame_done` |
| `King of Fighters '99, The (Taiwan) (En) (Unl).md` | g.. N....LE.....N.....E.....N.....N....:L...Ns | not implemented: m68k opcode 4741 |
| `King of Fighters 2000, The (Taiwan) (En) (Unl).md` | ...g.. N....*E.....N....$E.....N....$N.. R.L. | 263 lines and the VDP has not raised `frame_done` |
| `King of Fighters 98', The (Taiwan) (En) (Unl).md` | ...g.. N....*E.....N....$E.....N....$N.. R.L. | 263 lines and the VDP has not raised `frame_done` |
| `Legende de Thor, La (France) (Mega Drive Mini).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Legende de Thor, La (France).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `MK5 - Mortal Kombat Mythologies - Sub-Zero (Taiwan) (En) (Unl).md` | g&N.....E.....N.....Jy...lg E.....N.....N.. _^L. | 263 lines and the VDP has not raised `frame_done` |
| `Madden NFL 98 (USA).md` | MADDEN NFL 98 | 263 lines and the VDP has not raised `frame_done` |
| `Mega Turrican (Europe).md` | MEGA TURRICAN | 314 lines and the VDP has not raised `frame_done` |
| `Mega Turrican (USA).md` | MEGA TURRICAN | 263 lines and the VDP has not raised `frame_done` |
| `Mega-lo-Mania (Europe) (Beta).md` | MEGA lo MANIA | 314 lines and the VDP has not raised `frame_done` |
| `Mega-lo-Mania (Europe).md` | MEGA lo MANIA | 314 lines and the VDP has not raised `frame_done` |
| `Mega-lo-Mania (France).md` | MEGA lo MANIA | 314 lines and the VDP has not raised `frame_done` |
| `Mega-lo-Mania (World) (Evercade).md` | MEGA lo MANIA | 314 lines and the VDP has not raised `frame_done` |
| `Mike Ditka Power Football (USA, Europe) (Unl) (Alt).md` | Mike Ditka Power Football | 263 lines and the VDP has not raised `frame_done` |
| `Mike Ditka Power Football (USA, Europe) (Unl).md` | Mike Ditka Power Football | 263 lines and the VDP has not raised `frame_done` |
| `Mortal Kombat 3 (USA) (Beta).md` | unknown | 263 lines and the VDP has not raised `frame_done` |
| `NBA Action '94 (USA) (Beta) (1994-01-04).md` | NBA Action | 263 lines and the VDP has not raised `frame_done` |
| `NBA Action '94 (USA) (Beta) (1994-01-16).md` | NBA Action | 263 lines and the VDP has not raised `frame_done` |
| `NFL '95 (USA) (Beta) (1994-08-17) (Alt).md` | NFL 95 | not implemented: m68k opcode 4741 |
| `NFL '95 (USA) (Beta) (1994-09-04).md` | NFL 95 | not implemented: m68k opcode 4741 |
| `NFL '95 (USA) (Beta) (1994-09-06).md` | NFL 95 | not implemented: m68k opcode 00FA |
| `NFL '95 (USA) (Beta) (1994-09-07).md` | NFL 95 | not implemented: m68k opcode 4741 |
| `NFL '95 (USA) (Beta) (1994-09-08).md` | NFL 95 | not implemented: m68k opcode 4741 |
| `NFL '95 (USA) (Beta) (1994-09-09).md` | NFL 95 | not implemented: m68k opcode 4741 |
| `Nigel Mansell's World Championship Racing (USA).md` | NIGEL MANSELLS WORLD CHAMPIONSHIP | 263 lines and the VDP has not raised `frame_done` |
| `Ottifants, The (Europe) (De) (Beta).md` | OTTIFANT | 314 lines and the VDP has not raised `frame_done` |
| `Ottifants, The (Europe) (En,Fr,De,Es,It).md` | THE OTTIFANTS | 314 lines and the VDP has not raised `frame_done` |
| `Paperboy (USA, Europe) (Beta) (1991-10-28).md` | PaperBoy | 263 lines and the VDP has not raised `frame_done` |
| `Paperboy 2 (USA, Europe).md` | PAPERBOY 2 | 263 lines and the VDP has not raised `frame_done` |
| `Power Football (World) (Evercade) (Unl).md` | Piko Super Power Football | 263 lines and the VDP has not raised `frame_done` |
| `Pro Action Replay (Europe) (Program) (Unl).md` | ACTION REPLAY 2 | not implemented: m68k opcode 4741 |
| `Pro Action Replay 2 (Europe) (v2.1) (Program) (Unl) (Alt).md` | ACTION REPLAY 2 | not implemented: m68k opcode 4741 |
| `Pro Action Replay 2 (Europe) (v2.1) (Program) (Unl).md` | unknown | not implemented: m68k opcode 00FF |
| `Ren & Stimpy Show Presents, The - Stimpy's Invention (Europe) (1993-12-09) (Sega Channel).md` | STIMPY'S INVENTION | 314 lines and the VDP has not raised `frame_done` |
| `Ren & Stimpy Show Presents, The - Stimpy's Invention (Europe).md` | STIMPY'S INVENTION | 314 lines and the VDP has not raised `frame_done` |
| `Risk (USA).md` | RISK | 263 lines and the VDP has not raised `frame_done` |
| `RoboCop Versus The Terminator (Europe).md` | ROBOCOP VERSUS THE TERMINATOR | 314 lines and the VDP has not raised `frame_done` |
| `Samurai Shodown (Europe).md` | SAMURAI SHODOWN | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (Europe) (En,Fr,De,It) (Beta).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (Europe) (En,Fr,De,It).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (World) (En,Fr,De,It) (Evercade).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Shane Warne Cricket (Australia).md` | SHANE WARNE CRICKET | 314 lines and the VDP has not raised `frame_done` |
| `Show do Milhao - Volume 2 (Brazil).md` | SHOW DO MILHAO VOL 2 | 263 lines and the VDP has not raised `frame_done` |
| `Side Pocket (Europe).md` | SIDE POCKET | 314 lines and the VDP has not raised `frame_done` |
| `Sonic & Knuckles (World) (Beta) (1994-05-25).md` | SONIC & KNUCKLES | 263 lines and the VDP has not raised `frame_done` |
| `Sonic & Knuckles + Sonic The Hedgehog 2 + Sonic The Hedgehog 3 (World) (Virtual Console).md` | SONIC & KNUCKLES | 263 lines and the VDP has not raised `frame_done` |
| `Sonic & Knuckles + Sonic The Hedgehog 3 (Europe) (Lock-on Combination).md` | SONIC & KNUCKLES | 263 lines and the VDP has not raised `frame_done` |
| `Sonic & Knuckles + Sonic The Hedgehog 3 (Japan) (En) (Lock-on Combination).md` | SONIC & KNUCKLES | 263 lines and the VDP has not raised `frame_done` |
| `Sonic & Knuckles + Sonic The Hedgehog 3 (USA) (Lock-on Combination).md` | SONIC & KNUCKLES | 263 lines and the VDP has not raised `frame_done` |
| `Sonic 3 (Europe).md` | SONIC THE HEDGEHOG 3 | 314 lines and the VDP has not raised `frame_done` |
| `Sonic The Hedgehog 3 (Japan, Korea) (En).md` | SONIC THE HEDGEHOG 3 | 263 lines and the VDP has not raised `frame_done` |
| `Sonic The Hedgehog 3 (USA) (1993-11-20) (Sega Channel).md` | SONIC THE HEDGEHOG 3 | 263 lines and the VDP has not raised `frame_done` |
| `Sonic The Hedgehog 3 (USA).md` | SONIC THE HEDGEHOG 3 | 263 lines and the VDP has not raised `frame_done` |
| `Sonic The Hedgehog 3 Limited Edition (Japan) (En) (Proto) (1994-04-08).md` | SONIC THE HEDGEHOG 3 | 263 lines and the VDP has not raised `frame_done` |
| `Sonic The Hedgehog 3 Limited Edition (Japan) (En) (Proto) (1994-05-17).md` | SONIC THE HEDGEHOG 3 | 263 lines and the VDP has not raised `frame_done` |
| `Star Odyssey (USA) (Super Fighter Team).md` | STAR ODYSSEY | not implemented: m68k opcode 00FF |
| `Story of Thor, The (Europe) (Beta) (1994-04-xx).md` | The Story of Thor | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Europe) (Mega Drive Mini).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Europe).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Germany) (Mega Drive Mini).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Germany).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Spain) (Mega Drive Mini).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Story of Thor, The (Spain).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 2 (Japan) (En,Ja) (Mega Drive Mini).md` | STREETS OF RAGE 2 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe) (Beta) (1994-04-12).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe) (Beta) (1994-04-13).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe) (Beta) (1994-04-15).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe) (Beta) (1994-04-20).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe) (Beta) (1994-04-25).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage 3 (Europe).md` | STREETS OF RAGE3 | 314 lines and the VDP has not raised `frame_done` |
| `Streets of Rage II (Japan, Europe) (En,Ja).md` | STREETS OF RAGE 2 | 314 lines and the VDP has not raised `frame_done` |
| `Super Skidmarks (Europe) (Beta) (1995-09-22) (J-Cart).md` | unknown | 314 lines and the VDP has not raised `frame_done` |
| `Super Skidmarks (Europe) (J-Cart).md` | unknown | 314 lines and the VDP has not raised `frame_done` |
| `Tecmo Super NBA Basketball (USA).md` | Tecmo Super NBA BAsketball | 263 lines and the VDP has not raised `frame_done` |
| `Test Drive II - The Duel (USA, Europe) (Unl).md` | The Duel TDII | 263 lines and the VDP has not raised `frame_done` |
| `Triple Play - Gold Edition (USA).md` | Triple Play Gold | not implemented: m68k opcode 00FF |
| `Triple Play 96 (USA).md` | Triple Play Baseball | not implemented: m68k opcode 00FF |
| `Ultracore (Europe) (Strictly Limited Games).md` | ULTRACORE | 314 lines and the VDP has not raised `frame_done` |
| `Ultracore (USA, Europe) (Switch, PS Vita).md` | ULTRACORE | 314 lines and the VDP has not raised `frame_done` |
| `Vectorman 2 (USA) (Beta) (1995-08-xx).md` | VECTORMAN | 263 lines and the VDP has not raised `frame_done` |
| `WWF Super WrestleMania (USA, Europe).md` | WWF | 263 lines and the VDP has not raised `frame_done` |
| `WWF WrestleMania - The Arcade Game (USA, Europe) (Beta).md` | unknown | 263 lines and the VDP has not raised `frame_done` |
| `WildSnake (USA) (Proto).md` | WILDSNAKE | 263 lines and the VDP has not raised `frame_done` |
| `World Heroes (Asia) (En) (Beta) (1994-04-20) (Alt) [b].md` | O.L. .E.O.S | not implemented: m68k opcode 00FF |
| `Zero Wing (Japan).md` | ZERO WING | 263 lines and the VDP has not raised `frame_done` |
| `Zero Wing (USA, Europe) (Retro-Bit).md` | ZERO WING | 263 lines and the VDP has not raised `frame_done` |

