# Compatibility sweep

`headless sweep` over the local ROM collection: every cartridge run for 600 frames in its own child process with a 30s wall-clock limit. `Boots` means the run finished and its last frame had more than 4 distinct colours, which is a claim about the machine, not about the game: no row here says the picture is right, only that something was drawn. The full per-ROM table is [`compat.csv`](compat.csv); this page lists the problems.

| Status | ROMs |
|---|---|
| Boots | 2111 |
| Blank | 567 |
| Panic | 97 |

The tables here are the sweep of #124 as reswept after #349. The sweep has been run
again since, at `1536bb5`, for the M6-13 triage (#133): **Boots 2112, Blank 567,
Panic 96**. Three rows moved, all of them from #78's mapper work (`87981e3`):
`Tom Clown (Taiwan) (En) (Unl).md` Panic -> Boots, and `Funny World & Balloon Boy
(USA) (Unl).md` and `Whac-a-Critter (USA) (Unl).md` gained colours (26 -> 31 and 7
-> 22). The tables below are not regenerated here, because #133 is a triage and not
authorised to rewrite this page's data.

## Known issues

One issue per cause, from the M6-13 triage (#133): `just test-all`,
`just test-commercial` and the 2775-ROM sweep. `just test-all` fails on exactly one
test by design and `just test-commercial` on the same one, both until #362 is fixed.

| Issue | Cause | ROMs |
|---|---|---|
| [#371](https://github.com/scottjab/rustsega/issues/371) | A mid-frame V28/V30 switch moves the VBlank line, so `run_frame`'s watchdog fires a frame early. 48 carts report `314 lines` (all PAL) and 35 report `263` (33 `usa`, 2 `japan`): the same event on the two rasters, since the guard is `lines_per_frame + 1`. [#354](https://github.com/scottjab/rustsega/issues/354) is the inventory of these 83 and should close into #371. | 83 |
| [#355](https://github.com/scottjab/rustsega/issues/355) | Unimplemented 68000 opcodes: `4741` x9, `00FF` x3, `00FA` x1. Seven of the nine `4741` hits are five betas of `NFL '95` and two Pro Action Replay carts, so it is narrower than nine separate instruction gaps. `0AC6` is gone since #78. | 13 |
| [#374](https://github.com/scottjab/rustsega/issues/374) | The SVP DSP in the `Virtua Racing` cartridge, which nothing in the tree models. `Virtua Racing (USA)` and `(Japan) (En)` stay `#000000` at every frame sampled. | 2 |
| [#375](https://github.com/scottjab/rustsega/issues/375) | 28 retail carts that draw pure black at all 21 sampled frame counts, with and without input - diagnosis ticket, including the `QuackShot` rev pair where `-00` boots and `-01` does not. | 28 |
| [#357](https://github.com/scottjab/rustsega/issues/357) | The 2 MB/4 MB Taiwan and China unlicensed carts, which are not a mapper problem. 26 of them are still in the never-drawing set; listed on the issue. | 26 |
| [#373](https://github.com/scottjab/rustsega/issues/373) | **This is why the `Blank` number above should not be read as 567 broken games.** The sweep samples one frame, at 600, with no pad connected. Re-running the 536 non-BIOS `Blank` ROMs at other frame counts and with input shows 426 of them do draw: 379 at another frame with no input, 27 more only once a button is held, and 20 more at frame counts outside that first set (12 with no input, 8 only with input). | 426 |
| [#362](https://github.com/scottjab/rustsega/issues/362) | The one failing test: `commercial_save_state_is_audible_after_a_load`. Loading a state leaves the mixer ahead of the machine, so the console goes silent for as many frames as it ran between the save and the load. | - |

### Deliberately out of scope

These are in `~/games/roms/genesis` and are counted as failures by the sweep, but
they are not Mega Drive games this emulator can run, so they are listed rather than
filed:

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

`headless sweep ~/games/roms/genesis --jobs 24 --frames 600` takes 103 s wall clock
on 28 cores. The wider per-ROM sampling behind #373 and #375 was
`headless run <rom> --colours` at the frame counts 60, 120, 180, 240, 300, 420, 540,
600, 700, 900, 1200, 1500, 2100, 2400, 2700 and 3600, with and without `--pad1`, and
`identify -format %k` plus `convert ... txt:-` on `--dump-png` output to read back
what the single colour actually was (`#000000`).

### Blank (567 - see the CSV for the full table)

| ROM | Title | Notes |
|---|---|---|
| `007 Shitou - The Duel (Japan) (En).md` | 007 (James Bond)The Duel. | 1 colours |
| `16t (Japan) (Game no Kanzume Vol. 2) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `6-Pak (USA).md` | MEGAGAMES 6in1 US | 2 colours |
| `ATP Tour (Europe).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-07-19).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-07-23).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-02).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-05).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-08-08).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA) (Beta) (1994-09-08).md` | ATP Tour Championship Tennis | 2 colours |
| `ATP Tour Championship Tennis (USA).md` | ATP Tour Championship Tennis | 2 colours |
| `Aaahh!!! Real Monsters (USA) (Beta) (1995-07-07).md` | KILLER TOMATOES | 1 colours |
| `Aaahh!!! Real Monsters (USA).md` | NICKELODEON AAAHH!!! REAL MONSTERS | 2 colours |
| `Addams Family, The (USA, Europe) (Beta).md` | ADDAMS FAMILY | 3 colours |
| `Addams Family, The (USA, Europe).md` | ADDAMS FAMILY | 3 colours |
| `Advanced Daisenryaku - Deutsch Dengeki Sakusen (Japan) (Beta) (1991-03-07).md` | SUPER DAISENRYAKU | 1 colours |
| `Advanced Daisenryaku - Deutsch Dengeki Sakusen (Japan) (Rev A).md` | ADVANCED DAISENRYAKU | 2 colours |
| `Adventures of Batman & Robin, The (USA) (Beta) (1995-04-21).md` | THE ADVENTURES OF BATMAN AND ROBIN | 1 colours |
| `Adventures of Rocky and Bullwinkle and Friends, The (USA).md` | ROCKY AND BULLWINKLE AND FRIENDS | 1 colours |
| `Air Diver (Japan) (En).md` | AIR DIVER | 4 colours |
| `Air Diver (USA).md` | AIR DIVER | 3 colours |
| `Air Management II - Koukuuou o Mezase (Japan).md` | ..-.......2 | 4 colours |
| `Akira (USA) (Proto).md` | OOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOO | 1 colours |
| `Al Michaels Announces HardBall III (USA, Europe).md` | HardBall III | 2 colours |
| `Altered Beast (Japan, USA) (En) (Sega 3D Classics Collection).md` | .b...L | 1 colours |
| `Arcade Legends Sega Mega Drive (World).md` | KID CHAMELEON | 1 colours |
| `Arcade Legends Street Fighter II' - Special Champion Edition (World).md` | STREET FIGHTER.U'SPECIAL CHAMPION EDITION | 1 colours |
| `Arch Rivals - The Arcade Game (USA, Europe).md` | ARCH RIVALS | 2 colours |
| `Art Alive (World) (1991-09-20) (Sega Channel).md` | Art Alive! | 3 colours |
| `Art Alive (World).md` | Art Alive! | 3 colours |
| `Art of Fighting (Europe).md` | ART OF FIGHTING | 1 colours |
| `Awesome Possum (Japan) (En,Ja).md` | Awesome Possum | 1 colours |
| `Awesome Possum Kicks Dr. Machino's Butt (USA).md` | Awesome Possum | 1 colours |
| `Awesome Possum Kicks Dr. Machino's Butt! (USA) (Beta 1).md` | Awesome Possum | 1 colours |
| `Awesome Possum Kicks Dr. Machino's Butt! (USA) (Beta 2) (1993-07-12).md` | Awesome Possum | 1 colours |
| `Awesome Possum Kicks Dr. Machino's Butt! (USA) (Beta 3).md` | Awesome Possum | 1 colours |
| `Aworg - Hero in the Sky (Japan) (Game no Kanzume Vol. 2) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `B.O.B. (USA, Europe).md` | B.O.B. | 1 colours |
| `Baby Boom (USA) (Proto) (1994-06-03).md` | BABY BOOM S | 4 colours |
| `Baby Boom (USA) (Proto) (1994-08-11).md` | BABY BOOM S | 4 colours |
| `Back to the Future Part III (Europe).md` | BTTF3 | 1 colours |
| `Back to the Future Part III (USA).md` | BTTF3 | 2 colours |
| `Bare Knuckle III (Japan) (Beta).md` | STREETS OF RAGE3 | 2 colours |
| `Bare Knuckle III (Japan).md` | STREETS OF RAGE3 | 2 colours |
| `Battle Golfer Yui (Japan).md` | BATTLE-GOLFER YUI | 1 colours |
| `Beavis and Butt-Head (USA) (Beta).md` | Beavis and Butthead | 1 colours |
| `Beethoven's 2nd - The Quest for Pups (USA) (Proto).md` | Mickey's Ultimate Challenge | 1 colours |
| `Beggar Prince (USA, Europe) (Rev 1) (Super Fighter Team) (Unl).md` | BEGGAR PRINCE | 2 colours |
| `Beggar Prince (USA, Europe) (Super Fighter Team) (Unl).md` | BEGGAR PRINCE | 2 colours |
| `Bill's Tomato Game (USA) (Proto).md` | BILL'S TOMATO GAME | 2 colours |
| `Blockbuster World Video Game Championship II (USA).md` | NBA JAM TOURNAMENT EDITION | 1 colours |
| `Bloodshot (Europe) (En,Fr,De,Es).md` | BLOODSHOT / BATTLE FRENZY (USA) | 1 colours |
| `Body Count (Brazil) (En) (Beta) (1994-01-27).md` | BODY COUNT | 2 colours |
| `Boogerman - A Pick and Flick Adventure (USA).md` | BOOGERMAN | 1 colours |
| `Bram Stoker's Dracula (Europe).md` | DRACULA | 2 colours |
| `Bram Stoker's Dracula (USA).md` | DRACULA | 2 colours |
| `Brave Battle Saga - Legend of the Magic Warrior (USA, Europe) (Unl).md` | Brave Battle Saga - Legend of the Magic Warrior | 2 colours |
| `Brett Hull Hockey 95 (USA).md` | Brett Hull Hockey '95 | 1 colours |
| `Bubble and Squeak (USA).md` | BUBBLE AND SQUEAK | 1 colours |
| `Buck Rogers - Countdown to Doomsday (USA, Europe).md` | BUCK RODGERS COUNTDOWN TO DOOMSD | 3 colours |
| `Bug's Life, A - Chong Chong Weiji (Taiwan) (En) (Unl).md` | ........g..,N....jE.... G...u.p.N....R..... .. | 1 colours |
| `Burning Force (World) (Evercade).md` | BURNING FORCE | 2 colours |
| `Caesar no Yabou II (Japan).md` | worrior of rome2 | 1 colours |
| `Caliber.50 (USA).md` | CALIBER FIFTY | 1 colours |
| `Castle of Illusion Starring Mickey Mouse (USA, Europe).md` | MICKEY MOUSE | 2 colours |
| `Chaoji Dafuweng (Taiwan) (Unl).md` | ................................................ | 1 colours |
| `Chaoji Poker (Taiwan) (En) (Unl).md` | ................................................ | 1 colours |
| `Chengjisihan II - Yuanchao Mishi Zhi Canglang Yu Bailu (China) (Pirate).md` | .....T...........E.......j | 1 colours |
| `Chester Cheetah - Wild Wild Quest (USA).md` | Chester Cheetah | 4 colours |
| `College Football's National Championship (USA) (Beta) (1994-04-13).md` | NFL SPORTSTALK FOOTBALL STARRINGJOE MONTANA | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-04-18).md` | NFL SPORTSTALK FOOTBALL STARRINGJOE MONTANA | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-04-19).md` | NFL SPORTSTALK FOOTBALL STARRINGJOE MONTANA | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-04-29).md` | COLLEGE FOOTBALL'S NATIONAL CHAMPIONSHIP | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-05-03).md` | COLLEGE FOOTBALL'S NATIONAL CHAMPIONSHIP | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-05-06).md` | COLLEGE FOOTBALL'S NATIONAL CHAMPIONSHIP | 1 colours |
| `College Football's National Championship (USA) (Beta) (1994-05-11).md` | COLLEGE FOOTBALL'S NATIONAL CHAMPIONSHIP | 1 colours |
| `Cyborg Justice (USA, Europe) (Beta).md` | CYBORG JUSTICE | 2 colours |
| `Da Hanghai Shidai (Taiwan) (Pirate).md` | UNCHARTED WATERS | 3 colours |
| `Da Hanghai Shidai II (Taiwan) (Simplified) (Pirate).md` | ...q.C.....U | 1 colours |
| `Da Hanghai Shidai II (Taiwan) (Traditional) (Pirate).md` | ...q.C.....U | 1 colours |
| `Da Zhanlue - Shandian Qixi (China) (Pirate).md` | ADVANCED DAISENRYAKU | 1 colours |
| `Daffy Duck in Hollywood (Europe) (Beta) (1994-08-xx).md` | DAFFY DUCK IN HOLLYWOOD | 4 colours |
| `Daffy Duck in Hollywood (Europe) (En,Fr,De,Es,It).md` | DAFFY DUCK IN HOLLYWOOD | 4 colours |
| `Daikoukai Jidai II (Japan).md` | ...q.C.....U | 4 colours |
| `Dangerous Seed (Japan) (En).md` | DANGEROUS SEED | 2 colours |
| `Darius II (Japan).md` | DARIUS 2 | 1 colours |
| `Darius II (World) (Ja) (Darius Cozmic Collection).md` | DARIUS 2 | 1 colours |
| `Demolition Man (USA, Europe) (Beta).md` | Demolition Man | 1 colours |
| `Demolition Man (USA, Europe).md` | Demolition Man | 1 colours |
| `Desert Demolition (USA, Europe) (Beta) (1994-12-06).md` | DESERT DEMOLITION | 1 colours |
| `Desert Demolition (USA, Europe) (Beta) (1994-12-12) (Alt).md` | DESERT DEMOLITION | 1 colours |
| `Devi to Pii (Japan) (Mega Drive Mini 2).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 2 colours |
| `Dick Tracy (World).md` | DICK TRACY | 1 colours |
| `Dino Dini's Soccer (Europe) (En,Fr,De).md` | DINO DINI'S SOCCER | 4 colours |
| `Dinosaur's Tale, A (USA).md` | A Dinosaur's Tale | 1 colours |
| `Dragon Slayer - Eiyuu Densetsu II (Japan).md` | ........... .p.Y.`...Q | 3 colours |
| `Dragon's Lair (USA) (Proto) (1994-06-xx).md` | Dragon's Lair The Adventure Continues | 1 colours |
| `Duli Zhanzheng (China) (Pirate).md` | Liberty or Death | 3 colours |
| `Dyna Brothers 2 (Japan).md` | .... ........2 | 1 colours |
| `Dyna Brothers 2 Special (Japan) (Virtual Console).md` | .... ........2 .............. | 1 colours |
| `ESPN Sunday Night NFL (USA) (Beta).md` | ESPN SUNDAY NIGHT FOOTBALL | 1 colours |
| `ESPN Sunday Night NFL (USA).md` | ESPN SUNDAY NIGHT FOOTBALL | 1 colours |
| `Earthworm Jim (USA).md` | EARTHWORM JIM | 2 colours |
| `Earthworm Jim (World) (Evercade).md` | EARTHWORM JIM | 2 colours |
| `El.Viento (Japan).md` | ELVIENTO | 1 colours |
| `El.Viento (USA).md` | ELVIENTO | 2 colours |
| `El.Viento (World) (Evercade).md` | ELVIENTO | 2 colours |
| `El.Viento (World) (Retro-Bit).md` | ELVIENTO | 2 colours |
| `Elitserien 95 (Sweden).md` | ELITSERIEN '95 | 1 colours |
| `Empire of Steel (Europe).md` | STEEL EMPIRE | 3 colours |
| `Escape from Mars Starring Taz (USA) (Beta) (1994-04-18).md` | TAZ-MANIA 2 | 1 colours |
| `Eternal Champions (Europe) (Beta).md` | ETERNAL CHAMPIONS | 3 colours |
| `Eternal Champions (Europe).md` | ETERNAL CHAMPIONS | 3 colours |
| `Eternal Champions (Japan, Korea) (En) (Beta) (1993-11-17).md` | ETERNAL CHAMPIONS | 3 colours |
| `Eternal Champions (Japan, Korea) (En).md` | ETERNAL CHAMPIONS | 3 colours |
| `European Club Soccer (Europe).md` | Club Soccer (Console Edition) | 1 colours |
| `Ex-Mutants (USA, Europe) (Beta).md` | XMUTANTS | 1 colours |
| `Ex-Mutants (USA, Europe).md` | EX-MUTANTS | 1 colours |
| `Exo Squad (USA) (Beta).md` | EXO-SQUAD | 1 colours |
| `Exo Squad (USA).md` | EXO-SQUAD | 1 colours |
| `Exodus - Journey to the Promised Land (USA) (Unl).md` | INC..09....09... 09... .9.... .. g.#.SEGA..@.09 | 2 colours |
| `F-15 Strike Eagle II (Europe).md` | F-15 STRIKE EAGLE II | 1 colours |
| `F-15 Strike Eagle II (USA) (Beta) (1992-07-13).md` | F15-II STRIKE EAGL . | 1 colours |
| `F-15 Strike Eagle II (USA) (Sample).md` | F-15 STRIKE EAGLE II | 1 colours |
| `F-15 Strike Eagle II (USA).md` | F-15 STRIKE EAGLE II | 1 colours |
| `F1 - World Championship Edition (Europe) (Beta).md` | F1 WORLD CHAMPIONSHIP EDITION. | 1 colours |
| `F1 Circus MD (Japan).md` | F1 CIRCUS MD | 1 colours |
| `FIFA International Soccer (Japan) (En,Ja).md` | FIFA International Soccer | 1 colours |
| `FIFA Soccer 95 (China) (En,Fr,De,Es) (Pirate).md` | FIFA Soccer 95 | 1 colours |
| `FIFA Soccer 95 (USA, Europe) (En,Fr,De,Es).md` | FIFA Soccer 95 | 1 colours |
| `Family Feud (USA).md` | FAMILY FEUD | 2 colours |
| `Fatal Fury (Europe, Korea) (En).md` | FATAL FURY | 2 colours |
| `Fatal Fury (USA).md` | FATAL FURY | 2 colours |
| `Fatal Fury 2 (USA, Korea) (En).md` | FATAL FURY 2 | 4 colours |
| `Fatal Rewind (USA, Europe) (Rev 1).md` | THE KILLING GAME SHOW | 1 colours |
| `Fatal Rewind (USA, Europe).md` | FATAL REWIND | 1 colours |
| `Fengkuang Taohuayuan (Taiwan) (Unl).md` | unknown | 1 colours |
| `Ferrari Grand Prix Challenge (Europe).md` | FERRARI GRANDPRIX CHALLENGE | 2 colours |
| `Ferrari Grand Prix Challenge (USA).md` | FERRARI GRANDPRIX CHALLENGE | 2 colours |
| `Fighting Masters (Japan, Korea) (En).md` | FIGHTING MASTERS | 2 colours |
| `Fighting Masters (USA).md` | FIGHTING MASTERS | 2 colours |
| `Flashback (Europe) (En,Fr) (Beta) (1992-09-18).md` | FLASHBACK Version on 18/09/1992 | 4 colours |
| `Flashback (Europe) (En,Fr).md` | FLASHBACK Version on 01/20/1993 | 4 colours |
| `Flashback - The Quest for Identity (USA) (En,Fr).md` | FLASHBACK Version on 01/20/1993 | 4 colours |
| `Fushigi no Umi no Nadia (Japan).md` | THE SECRET OF BLUE WATER | 3 colours |
| `Futari de Puyo Puyo Sun (Japan) (Mega Drive Mini 2).md` | PUYO PUYO SUN | 1 colours |
| `GEMS (USA) (v2.8) (Proto) (1994-06-04) (Program).md` | GEMS | 2 colours |
| `Galaxy Force II (USA, Europe) (SEGA Classic Collection).md` | GALAXY FORCE II | 1 colours |
| `Gambler Jiko Chuushinha - Katayama Masayuki no Mahjong Doujou (Japan).md` | ............. .......S.h.@...R.................. | 2 colours |
| `Garou Densetsu - Shukumei no Tatakai (Japan).md` | FATAL FURY | 2 colours |
| `Garou Densetsu 2 - Aratanaru Tatakai (Japan).md` | FATAL FURY 2 | 4 colours |
| `Gauntlet (Japan) (En,Ja).md` | GAUNTLET 4 (C)ATARI GAMES (C)TENGEN | 1 colours |
| `Gemfire (USA).md` | GEMFIRE | 4 colours |
| `Ghostbusters (Japan) (En,Ja).md` | GHOST BUSTERS | 3 colours |
| `Ghostbusters (USA, Europe) (En,Ja) (Beta) (1990-05-01).md` | GHOST BUSTERS | 3 colours |
| `Ghostbusters (USA, Europe) (En,Ja).md` | GHOST BUSTERS | 3 colours |
| `Ghouls'n Ghosts (World) (En,Ja) (Rev A) (Sega Channel).md` | GHOULS'N GHOSTS | 4 colours |
| `Gods (Europe).md` | GODS | 1 colours |
| `Gods (Japan).md` | GODS | 1 colours |
| `Gods (USA) (Beta).md` | GODS | 1 colours |
| `Gods (USA).md` | GODS | 1 colours |
| `Goofy's Hysterical History Tour (USA).md` | GOOFY | 4 colours |
| `Gouketsuji Ichizoku (Japan).md` | POWER INSTINCT | 1 colours |
| `Great Circus Mystery - Mickey to Minnie Magical Adventure 2 (Japan).md` | THE GREAT CIRCUS MYSTERY Starring Mickey&Minnie | 4 colours |
| `Great Circus Mystery Starring Mickey & Minnie, The (USA).md` | THE GREAT CIRCUS MYSTERY Starring Mickey&Minnie | 4 colours |
| `Greendog - The Beached Surfer Dude! (USA, Europe).md` | GREENDOG | 1 colours |
| `Guaishou Jingji Chang - Pokemon Stadium (Taiwan) (En) (Unl).md` | E.....G.....N.....E....`G.....N.....N.../.N....B | 1 colours |
| `Gunfight 3 in 1 (Taiwan) (En) (Unl).md` | .Enterprises Ltd. | 1 colours |
| `Gunstar Heroes (Japan) (Beta).md` | LUNATIC GUNSTAR | 1 colours |
| `Haidi Liang Wanli (Lanbaoshi Zhi Mi) (China) (Pirate).md` | THE SECRET OF BLUE WATER | 2 colours |
| `HardBall '95 (USA).md` | HardBall 95 | 1 colours |
| `Heavy Nova (USA).md` | HEAVY NOVA | 1 colours |
| `High Seas Havoc (USA) (En,Ja) (Beta).md` | unknown | 3 colours |
| `Hokuto no Ken - Shin Seikimatsu Kyuuseishu Densetsu (Japan).md` | HOKUTO NO KEN II | 2 colours |
| `Home Alone (USA, Europe) (Beta) (1992-07-xx).md` | HOME ALONE | 4 colours |
| `Home Alone (USA, Europe).md` | HOME ALONE | 4 colours |
| `Humans, The (USA).md` | INTRODUCING THE HUMANS | 2 colours |
| `Huoqiang Yingxiong (China) (Pirate).md` | GUNSTAR HEROES | 1 colours |
| `Hurricanes (Europe).md` | HURRICANES | 4 colours |
| `Hybrid Front, The (Japan) (Beta).md` | HYBRID FRONT | 1 colours |
| `Hybrid Front, The (Japan).md` | HYBRID FRONT | 2 colours |
| `Hyper Marbles (Japan) (Game no Kanzume Vol. 1) [b].md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `I Love Mickey Mouse - Fushigi no Oshiro Daibouken (Japan).md` | MICKEY MOUSE | 2 colours |
| `I Love Mickey and Donald - Fushigi na Magic Box (Japan) (Rev 1).md` | World of Illusion starringMickeyMouse&DonaldDuck | 4 colours |
| `I Love Mickey and Donald - Fushigi na Magic Box (Japan).md` | World of Illusion starringMickeyMouse&DonaldDuck | 4 colours |
| `It Came from the Desert (USA) (Proto).md` | IT CAME FROM THEDESERT | 1 colours |
| `J. League Champion Soccer (Japan).md` | J-League Champion Soccer | 1 colours |
| `J. League Pro Striker (Japan) (Rev A).md` | J.LEAGUE PRO STRIKER | 1 colours |
| `J. League Pro Striker (Japan).md` | J.LEAGUE PRO STRIKER | 1 colours |
| `James Bond 007 - The Duel (Europe).md` | James Bond The Duel. | 1 colours |
| `James Bond 007 - The Duel (USA).md` | James Bond The Duel. | 1 colours |
| `Janou Touryuumon (Japan).md` | ...c.o.... | 2 colours |
| `Jantei Monogatari (Japan).md` | ........ | 4 colours |
| `Jordan vs Bird (USA, Europe) (Rev 1).md` | Jordan vs. Bird | 1 colours |
| `Jordan vs Bird (USA, Europe).md` | Jordan vs. Bird | 1 colours |
| `Joshua & the Battle of Jericho (USA) (Unl).md` | INC..09....09... 09... .9.... .. g.#.SEGA..@.09 | 2 colours |
| `Judge Dredd (World).md` | JUDGE DREDD THE MOVIE | 3 colours |
| `Jungle Book, The (World) (Disney Classic Games).md` | THE JUNGLE BOOK | 1 colours |
| `Jurassic Park - Rampage Edition (USA, Europe) (Beta) (1994-06-20).md` | JAPAN GAME NAME | 1 colours |
| `Jurassic Park - Rampage Edition (USA, Europe) (Beta) (1994-06-22).md` | JAPAN GAME NAME | 1 colours |
| `Juuouki (Japan) (En).md` | .b...L | 1 colours |
| `Kidou Keisatsu Patlabor - 98-Shiki Kidou Seyo! (Japan).md` | PATLABOR .X.W...N.......I | 2 colours |
| `Killing Game Show, The (Japan) (En).md` | THE KILLING GAMESHOW - THE 1993 MEGADRIVE REMIX | 1 colours |
| `King of Fighters 98', The (Taiwan) (En) (Unl).md` | ...g.. N....*E.....N....$E.....N....$N.. R.L. | 1 colours |
| `King of the Monsters (Europe).md` | KING OF THE MONSTERS | 2 colours |
| `King of the Monsters (USA).md` | KING OF THE MONSTERS | 2 colours |
| `King of the Monsters 2 (USA).md` | KING OF THE MONSTERS 2 | 3 colours |
| `Krusty's Super Fun House (USA, Europe) (Rev A).md` | KRUSTYS SUPER FUNHOUSE | 1 colours |
| `Krusty's Super Fun House (USA, Europe).md` | KRUSTYS SUPER FUNHOUSE | 1 colours |
| `Last Battle (USA, Europe, Korea) (En).md` | HOKUTO NO KEN II | 2 colours |
| `Legend of Wukong (USA, Europe) (Super Fighter Team) (Unl).md` | LEGEND OF WUKONG | 1 colours |
| `Lemmings 2 - The Tribes (Europe).md` | LEMMINGS 2 THE TRIBES | 1 colours |
| `Lemmings 2 - The Tribes (USA).md` | LEMMINGS 2 THE TRIBES | 1 colours |
| `Light Crusader (Europe) (En,Fr,De,Es).md` | LIGHT CRUSADER | 4 colours |
| `Light Crusader (Japan) (En,Ja).md` | LIGHT CRUSADER | 4 colours |
| `Lightening Force - Quest for the Darkstar (USA) (Sega Ages).md` | LIGHTENING FORCE | 4 colours |
| `Lightening Force - Quest for the Darkstar (USA).md` | LIGHTENING FORCE | 1 colours |
| `Link Dragon (Taiwan) (En) (Unl).md` | unknown | 1 colours |
| `Lion King II, The - Simba's Mighty Adventure (Taiwan) (En) (Unl).md` | ................................................ | 1 colours |
| `Lion King III, The (Taiwan) (En) (Unl).md` | Of Sega Enterprises Ltd. | 1 colours |
| `Lotus Turbo Challenge (USA, Europe).md` | Lotus Turbo Challenge | 1 colours |
| `MD Soft Checker (USA) (v0.30) (Proto) (Unl).md` | MD Soft Checker Version 0.30**Prg. by papi** | 2 colours |
| `MLBPA Baseball (USA).md` | MLBPA BASEBALL | 1 colours |
| `Mahjong Emo Ta - Devilish Mahjong Tower (Taiwan) (Unl).md` | "\|.... <.........n..Q...N.....3..D.....9......g. | 1 colours |
| `Mamono Hunter Youko - Dai 7 no Keishou (Japan).md` | .....n...^.[.d.q | 1 colours |
| `Mario Andretti Racing (USA, Europe).md` | Mario Andretti Racing | 1 colours |
| `Mary Shelley's Frankenstein (USA).md` | FRANKENSTEIN | 3 colours |
| `Master of Monsters (Japan) (Beta) (1991-04-26).md` | .............. | 4 colours |
| `Master of Monsters (Japan).md` | .............. | 4 colours |
| `Master of Monsters (USA).md` | .............. | 2 colours |
| `Maximum Carnage (World).md` | MAXIMUM CARNAGE | 2 colours |
| `McDonald's Treasure Land Adventure (Japan) (En,Ja) (Beta).md` | MC DONALDS TREASURE LAND ADVENTURE | 4 colours |
| `Mega 6 Vol. 3 (Europe).md` | MEGAGAMES Vol.3 | 2 colours |
| `Mega Bomberman (Europe, Korea) (En).md` | MEGA BOMBER MAN | 3 colours |
| `Mega Bomberman (USA).md` | MEGA BOMBER MAN | 3 colours |
| `Mega Bomberman - Special 8-Player-Demo (Europe) (Proto).md` | unknown | 3 colours |
| `Mega Games 6 (Europe).md` | MEGAGAMES Vol.1 | 2 colours |
| `Mega Games 6 Vol. 2 (Europe).md` | MEGAGAMES Vol.2 | 2 colours |
| `Mega Man - The Wily Wars (USA) (Retro-Bit).md` | MEGAMAN THE WILY WARS | 2 colours |
| `Mega Net (Brazil) (Program).md` | MEGA NET | 1 colours |
| `Mega lo Mania (Japan).md` | MEGA lo MANIA | 1 colours |
| `MegaMind (Japan) (Sega Game Toshokan).md` | SEGANET GAME | 2 colours |
| `Menghuan Moni Zhan (China) (Pirate).md` | WARSONG | 1 colours |
| `Mercs (World) (En,Ja).md` | MERCS | 1 colours |
| `Mickey Mania (Europe).md` | MICKEY MANIA | 4 colours |
| `Mickey Mania - The Timeless Adventures of Mickey Mouse (Japan).md` | MICKEY MANIA | 1 colours |
| `Mickey Mania - The Timeless Adventures of Mickey Mouse (USA).md` | MICKEY MANIA | 1 colours |
| `Mickey's Ultimate Challenge (USA).md` | Mickey's Ultimate Challenge | 2 colours |
| `Micro Machines - Military (Europe) (Beta).md` | unknown | 1 colours |
| `Micro Machines - Military (Europe) (J-Cart).md` | unknown | 1 colours |
| `Micro Machines - Turbo Tournament 96 (Europe) (J-Cart).md` | unknown | 1 colours |
| `Micro Machines - Turbo Tournament 96 (Europe) (Rev 1) (J-Cart).md` | unknown | 1 colours |
| `Mighty Morphin Power Rangers - The Movie (USA) (Beta) (1995-07-13).md` | unknown | 2 colours |
| `Mighty Morphin Power Rangers - The Movie (USA) (Beta) (1995-07-17).md` | POWER RANGERS 2 | 3 colours |
| `Mighty Morphin Power Rangers - The Movie (USA) (Beta) (1995-07-22).md` | POWER RANGERS 2 | 3 colours |
| `Mighty Morphin Power Rangers - The Movie (USA) (Beta) (1995-07-24).md` | POWER RANGERS 2 | 3 colours |
| `Mighty Morphin Power Rangers - The Movie (USA).md` | POWER RANGERS 2 | 3 colours |
| `Mortal Kombat 3 (China) (En) (Pirate).md` | MORTAL KOMBAT 3 | 2 colours |
| `Mortal Kombat 3 (Europe).md` | MORTAL KOMBAT 3 | 4 colours |
| `Mortal Kombat 3 (USA).md` | MORTAL KOMBAT 3 | 2 colours |
| `Mr. Nutz - Hoppin' Mad (Europe) (Proto).md` | MR NUTZ HOPPIN MAD | 1 colours |
| `Musha Aleste (Japan).md` | M.U.S.H.A. | 1 colours |
| `Mutant League Football (USA, Europe).md` | MUTANT LEAGUE FOOTBALL | 2 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1994-12-15).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-08).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-12).md` | NBA Basketball '95 | 1 colours |
| `NBA Action '95 Starring David Robinson (USA, Europe) (Beta) (1995-01-15).md` | NBA Basketball '95 | 1 colours |
| `NBA Live 95 (USA, Europe).md` | NBA Live '95 | 3 colours |
| `NBA Live 98 (USA).md` | NBA Live '98 | 2 colours |
| `NBA Live Nong-gu 95 (Korea).md` | NBA Live '95 | 3 colours |
| `NFL '95 (USA) (Beta) (1994-09-05) (Alt).md` | NFL 95 | 1 colours |
| `NFL '95 (USA) (Beta) (1994-09-05).md` | NFL 95 | 1 colours |
| `NFL Quarterback Club (World).md` | NFL QUARTERBACK CLUB | 1 colours |
| `NFL Quarterback Club 96 (USA, Europe).md` | NFL QUARTERBACK CLUB 96 | 1 colours |
| `NHL 97 (USA, Europe).md` | NHL 97 | 1 colours |
| `NHLPA Hockey (USA) (Proto) (HeartBeat Catalyst).md` | NHLPA HOCKEY | 1 colours |
| `NHLPA Hockey 93 (USA, Europe) (Rev 1).md` | NHLPA HOCKEY '93 | 1 colours |
| `NHLPA Hockey 93 (USA, Europe) (Rev 2).md` | NHLPA Hockey '93 | 1 colours |
| `NHLPA Hockey 93 (USA, Europe).md` | NHLPA Hockey '93 | 1 colours |
| `Nakajima Satoru Kanshuu - F1 Grand Prix (Japan).md` | NAKAJIMA SATORU F1 GRAND PRIX | 1 colours |
| `Newman Haas IndyCar Featuring Nigel Mansell (World).md` | Newman-Haas IndyCar Featuring Nigel Mansell | 3 colours |
| `Nigel Mansell's World Championship Racing (Europe).md` | NIGEL MANSELLS WORLD CHAMPIONSHIP | 3 colours |
| `Ninja Gaiden (Japan) (En) (Proto).md` | ARM WRESTLING | 3 colours |
| `Onslaught (USA, Europe) (Unl).md` | ONSLAUGHT | 1 colours |
| `Out of the Vortex (USA) (Proto) (1995-09-13).md` | OUT OF THE VORTEX | 3 colours |
| `OutRunners (USA) (Sega Channel).md` | OUT_RUNNERS | 2 colours |
| `OutRunners (USA).md` | OUT_RUNNERS | 2 colours |
| `Outback Joey (USA).md` | OUTBACK JOEY | 1 colours |
| `Outworld 2375 A.D. (USA) (HeartBeat Catalyst).md` | OUTWORLD | 1 colours |
| `PGA Tour Golf II (USA) (HeartBeat Catalyst).md` | PGA Tour Golf | 1 colours |
| `Pac-Man 2 - The New Adventures (USA).md` | PAC-MAN2 | 1 colours |
| `Paddle Fighter (Japan) (Game no Kanzume Otokuyou).md` | SEGANET GAME | 1 colours |
| `Paddle Fighter (Japan) (Sega Game Toshokan).md` | SEGANET GAME | 1 colours |
| `Pagemaster, The (Europe).md` | PAGEMASTER | 1 colours |
| `Pagemaster, The (USA) (Beta).md` | PAGEMASTER | 2 colours |
| `Pagemaster, The (USA).md` | PAGEMASTER | 2 colours |
| `Pebble Beach Golf Links (Europe).md` | PEBBLE BEACH GOLF LINKS | 3 colours |
| `Pebble Beach Golf Links (USA) (Rev A) (1994-02-14) (Sega Channel).md` | PEBBLE BEACH GOLF LINKS | 2 colours |
| `Pebble Beach Golf Links (USA).md` | PEBBLE BEACH GOLF LINKS | 2 colours |
| `Pense Bem (Brazil) (Mega Drive 4).md` | Fica 1 | 3 colours |
| `Phantasy Star Fukkokuban (Japan).md` | ....:....!.:....!.. o..g~#fo..2...2...2......... | 1 colours |
| `Phantasy Star II (USA, Europe) (Rev A) (Virtual Console).md` | PHANTASY STAR 2 | 1 colours |
| `Phantasy Star II - Amia's Adventure (Japan) (Game no Kanzume Vol. 1).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Anne's Adventure (Japan) (Game no Kanzume Vol. 2).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Huey's Adventure (Japan) (Game no Kanzume Vol. 1).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star II - Kinds's Adventure (Japan) (Game no Kanzume Vol. 2).md` | SEGA NET CARTRIDGE "GAME LIBRARY" | 1 colours |
| `Phantasy Star III - Generations of Doom (USA, Europe, Korea) (En).md` | PHANTASY STAR 3 GENERATIONS OF DOOM | 4 colours |
| `Phantasy Star IV (USA) (Beta) (1994-11-07).md` | PHANTASY STAR The end of the millennium | 1 colours |
| `Phantom 2040 (Europe) (En,Fr,De).md` | PHANTOM 2040 | 1 colours |
| `Phantom 2040 (USA).md` | PHANTOM 2040 | 4 colours |
| `Pikachu - Pocket Monster (Taiwan) (En) (Unl) (Alt).md` | ........g..<N...T.E....@G.....N.....0<..E....`G. | 1 colours |
| `Pikachu - Pocket Monster (Taiwan) (En) (Unl).md` | ........g..<N...T.E....@G.....N.....0<..E....`G. | 1 colours |
| `Pink Goes to Hollywood (USA, Europe) (Beta).md` | PINK GOES TO HOLLYWOOD | 1 colours |
| `Pink Goes to Hollywood (USA, Europe).md` | PINK GOES TO HOLLYWOOD | 1 colours |
| `Pinocchio (USA).md` | PINOCCHIO | 4 colours |
| `Pirates of Dark Water, The (Europe).md` | THE PIRATES OF DARK WATER | 2 colours |
| `Pirates of Dark Water, The (USA).md` | THE PIRATES OF DARK WATER | 2 colours |
| `Pit-Fighter (China) (En) (Pirate).md` | Pit Fighter | 1 colours |
| `Pit-Fighter (World) (Beta).md` | Pit Fighter | 3 colours |
| `Pit-Fighter (World) (En,Ja).md` | Pit Fighter | 4 colours |
| `Poker Club 6 in 1 (Taiwan) (Unl).md` | NuK... .M...$8...MSG,\|. ....Q.......... .f.A.. | 4 colours |
| `Prince of Persia 2 (Europe) (Proto) (1995-02-20).md` | PRINCE OF PERSIA 2 | 3 colours |
| `Pro CDX (USA, Europe) (v1.70) (Program) (Unl) [b].md` | CDX BINARY IMAGE | 1 colours |
| `Pro CDX (USA, Europe) (v1.8I) (Program) (Unl).md` | .<....a.. A....Ha. .#.......@.F.%.......@~.9..@. | 2 colours |
| `Pro Moves Soccer (USA).md` | PRO MOVES SOCCER | 1 colours |
| `Pro Yakyuu Super League '91 (Japan).md` | SUPER LEAGUE 91 | 1 colours |
| `Psy-O-Blade (Japan).md` | PSY-O-BLADE | 2 colours |
| `Punisher, The (USA).md` | THE PUNISHER | 1 colours |
| `Puyo Puyo Tsuu (Japan) (Rev A) (Virtual Console).md` | P.U.Y.O.2 | 1 colours |
| `Puyo Puyo Tsuu (Japan) (Rev A).md` | P.U.Y.O.2 | 1 colours |
| `Puyo Puyo Tsuu (Japan).md` | P.U.Y.O.2 | 1 colours |
| `QuackShot Starring Donald Duck (World) (En,Ja) (Rev A).md` | QUACK SHOT STARRING DONALD DUCK | 1 colours |
| `Quad Challenge (World) (Evercade).md` | QUAD CHALLENGE | 2 colours |
| `Ransei no Hasha (Japan).md` | .V...... | 3 colours |
| `Rent a Hero (Japan).md` | RENT A HERO | 3 colours |
| `Renzhe Wu lei Chuanshuo (China) (Pirate).md` | .E.......`.. | 1 colours |
| `Resq (Europe) (Proto).md` | Rescue Game which is NOT a shoot-em-up | 2 colours |
| `Revenge of Shinobi, The (Japan, USA) (En).md` | THE REVENGE SHINOBI | 1 colours |
| `Revenge of Shinobi, The (USA, Europe) (Rev A) (Beta) (1990-02-01).md` | THE REVENGE SHINOBI | 3 colours |
| `Revenge of Shinobi, The (USA, Europe) (Rev A).md` | THE REVENGE SHINOBI | 3 colours |
| `Revenge of Shinobi, The (USA, Europe) (Rev B).md` | THE REVENGE SHINOBI | 3 colours |
| `Revenge of Shinobi, The (USA, Europe).md` | THE REVENGE SHINOBI | 1 colours |
| `Revenge of Shinobi, The (World) (Rev B) (Virtual Console).md` | THE REVENGE SHINOBI | 3 colours |
| `Richard Scarry's Busytown (USA) (Beta) (1994-08-26) (Alt).md` | BUSYTOWN | 1 colours |
| `RoboCop Versus The Terminator (USA) (Beta) (1993-04-30).md` | GLOBAL GLADIATOR | 4 colours |
| `Robot Wreckage (USA) (Beta) (1992-07-xx).md` | unknown | 2 colours |
| `Rockheaven (Asia) (Ja) (Pirate).md` | .A...N.X.L.b.h .V...... | 2 colours |
| `Rockman - Mega World (Japan).md` | MEGAMAN THE WILY WARS | 1 colours |
| `Rockman X3 (Taiwan) (En) (Unl).md` | unknown | 2 colours |
| `Rockworld (Asia) (Ja) (Pirate).md` | .... ..... | 1 colours |
| `Roger Clemens' MVP Baseball (USA).md` | CLEMENS BASEBALL | 1 colours |
| `Rolling Thunder 2 (Europe).md` | ROLLING THUNDER 2 | 3 colours |
| `Rolling Thunder 3 (USA).md` | ROLLING THUNDER 3 | 3 colours |
| `Romance of the Three Kingdoms II (USA).md` | Romance of The Three Kingdoms .U | 4 colours |
| `Sagaia (USA).md` | SAGAIA | 2 colours |
| `Samurai Shodown vs Soul Edge (Taiwan) (En) (Unl).md` | R....,........g...N...A..9.....(g..,...... .g | 1 colours |
| `Sanguo Yanyi - Huoshao Chibi (Taiwan) (Unl).md` | .Enterprises Ltd. | 1 colours |
| `Sega Art Tool (Japan) (En) (Program).md` | ................N...H...NqL...NsNV..N.....N...#. | 3 colours |
| `Sega Channel (Japan) (v2.11) (Earlier) (Program).md` | Not Support | 1 colours |
| `Sega Channel (Japan) (v2.11) (Program).md` | Not Support | 1 colours |
| `Sega Channel (USA) (General Instrument) (Program).md` | Sega Channel | 1 colours |
| `Sega Channel (USA) (Scientific Atlanta) (Program).md` | Sega Channel | 1 colours |
| `Sega Game Toshokan (Japan) (En) (Rev A) (Program).md` | .Z.K.Q.[...}.... | 3 colours |
| `Separation Anxiety (USA, Europe).md` | SEPARATION ANXIETY | 1 colours |
| `Sesame Street Counting Cafe (USA).md` | Sesame Street: Counting Cafe | 1 colours |
| `Shadow Dancer - The Secret of Shinobi (USA, Europe) (SEGA Classic Collection).md` | SHADOW DANCER .e.... | 1 colours |
| `Shadow Dancer - The Secret of Shinobi (World) (Beta) (1990-10-02).md` | ................................................ | 1 colours |
| `Shadow Dancer - The Secret of Shinobi (World).md` | SHADOW DANCER .e.... | 1 colours |
| `Shadowrun (USA) (Beta) (1993-12-28).md` | SHADOWRUN | 2 colours |
| `Shi no Meikyuu - Labyrinth of Death (Japan) (Sega Game Toshokan).md` | SEGANET GAME | 2 colours |
| `Shiliu Zhang Mahjong II (Taiwan) (Unl).md` | ................................................ | 1 colours |
| `Simpsons, The - Bart vs. the Space Mutants (USA, Europe) (Rev A).md` | SIMPSONS | 2 colours |
| `Sonic & Knuckles (World) (Lock-on).bin` | .D.@.>0;..N.. R.. L...Ns......... ... ..... .. . | 1 colours |
| `Sonic & Knuckles (World) (Sonic Classic Collection).md` | SONIC & KNUCKLES | 1 colours |
| `Sonic Compilation (Europe).md` | SONIC COMPILATION | 1 colours |
| `Sonic Compilation (USA, Europe, Korea) (En) (Rev A).md` | SONIC COMPILATION | 2 colours |
| `Sonic Crackers (USA) (Proto) (1994-07-xx).md` | SONIC STUDIUM | 3 colours |
| `Sonic Jam 6 (Taiwan) (En) (Pirate).md` | Of Sega Enterprises Ltd. | 1 colours |
| `Sonic The Hedgehog (USA, Europe) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG | 1 colours |
| `Sonic The Hedgehog 2 (World) (Beta) (1992-09-24).md` | SONIC THE HEDGEHOG 2 | 2 colours |
| `Sonic The Hedgehog 2 (World) (Rev A) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG 2 | 1 colours |
| `Sonic The Hedgehog 2 (World) (Rev A) (Sonic Mega Collection).md` | SONIC THE HEDGEHOG 2 | 2 colours |
| `Sonic The Hedgehog 2 (World) (Rev A).md` | SONIC THE HEDGEHOG 2 | 2 colours |
| `Sonic The Hedgehog 2 (World) (Rev B) (Sonic Compilation ~ Sonic Classics).md` | SONIC THE HEDGEHOG 2 | 2 colours |
| `Sonic The Hedgehog 2 (World).md` | SONIC THE HEDGEHOG 2 | 2 colours |
| `Sonic The Hedgehog 3 (USA) (Sonic Classic Collection).md` | SONIC THE HEDGEHOG 3 | 1 colours |
| `Sorcer Kingdom (Japan) (Beta) (1991-11-08).md` | SORCER KINGDOM | 1 colours |
| `Sorcer Kingdom (Japan).md` | SORCER KINGDOM | 1 colours |
| `Sorcerer's Kingdom (USA) (Beta).md` | SORCERER_S KINGDOM | 1 colours |
| `Sorcerer's Kingdom (USA).md` | SORCERER_S KINGDOM | 1 colours |
| `Speedball 2 (Japan) (En).md` | SPEEDBALL 2 - BRUTAL DELUXE | 2 colours |
| `Speedball 2 - Brutal Deluxe (USA) (Beta) (1991-06-07).md` | SPEEDBALL 2 - BRUTAL DELUXE | 1 colours |
| `Speedball 2 - Brutal Deluxe (USA).md` | SPEEDBALL 2 - BRUTAL DELUXE | 1 colours |
| `Speedball 2 - Brutal Deluxe (World) (Evercade).md` | SPEEDBALL 2 - BRUTAL DELUXE | 1 colours |
| `Splatterhouse 2 (Europe).md` | SPLATTER HOUSE 2 | 3 colours |
| `Splatterhouse 2 (USA) (1992-03-02) (Sega Channel).md` | SPLATTER HOUSE 2 | 3 colours |
| `Splatterhouse 2 (USA).md` | SPLATTER HOUSE 2 | 3 colours |
| `Splatterhouse 2 (World) (Evercade).md` | SPLATTER HOUSE 2 | 2 colours |
| `Splatterhouse 3 (World) (Evercade).md` | SPLATTER HOUSE 3 | 2 colours |
| `Sport Games (Brazil) (En).md` | SUPER Volley ball | 3 colours |
| `Sports Talk Baseball (USA).md` | MLBPA SPORTS TALK BASEBALL | 3 colours |
| `Star Cruiser (Japan).md` | STAR CRUISER/MD | 1 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1993-12-28).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1993-12-29).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1994-01-03).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1994-01-10).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1994-01-18).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation (USA) (Beta) (1994-01-25).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation - Echoes from the Past (USA) (Rev A).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Trek - The Next Generation - Echoes from the Past (USA).md` | STAR TREK: THE NEXT GENERATION | 4 colours |
| `Star Wars (USA) (Proto) (1993-01-25).md` | GREENDOG | 3 colours |
| `Steel Empire (USA) (1992-03-13) (Sega Channel).md` | STEEL EMPIRE | 3 colours |
| `Steel Empire (USA).md` | STEEL EMPIRE | 3 colours |
| `Street Racer (Europe) (Sample) (1995-03-21).md` | STREET RACER | 1 colours |
| `Street Racer (Europe).md` | STREET RACER | 1 colours |
| `Sub-Terrania (Asia) (En) (Beta) (1994-02-02).md` | SUB-TERRANIA | 1 colours |
| `Sub-Terrania (Asia) (En).md` | SUB-TERRANIA | 1 colours |
| `Sub-Terrania (Europe).md` | SUB-TERRANIA | 1 colours |
| `Sub-Terrania (USA) (Beta 2) (1993-10-xx).md` | SUBTERRANIA | 1 colours |
| `Sub-Terrania (USA) (Beta 3) (1993-10-xx).md` | SUB-TERRANIA S P | 1 colours |
| `Sub-Terrania (USA).md` | SUB-TERRANIA | 1 colours |
| `Super High Impact (USA).md` | SUPER HIGH IMPACT FOOTBALL | 1 colours |
| `Super King Kong 99 (Taiwan) (En) (Unl).md` | Of Sega Enterprises Ltd. | 1 colours |
| `Super Mario 2 (Taiwan) (En) (Pirate).md` | ................................................ | 1 colours |
| `Super Mario Bros. (Taiwan) (En) (Unl).md` | SUPER MARIO BROS | 2 colours |
| `Super Mario World 64 (Taiwan) (En) (Unl).md` | unknown | 1 colours |
| `Super Shinobi, The (Japan) (En) (Beta) (1989-08-21) (Sega Channel).md` | SUPER SHINOBI | 1 colours |
| `Super Shinobi, The (Japan) (En) (Beta) (1989-xx-xx) (Sega Smash Pack).md` | SUPER SHINOBI | 1 colours |
| `Super Volley Ball (Japan).md` | SUPER Volley ball | 3 colours |
| `Super Volleyball (USA) (Sega Channel).md` | SUPER Volley ball | 3 colours |
| `Super Volleyball (USA).md` | SUPER Volley ball | 3 colours |
| `Taikong Zhanshi - Mofa Zhanshi (Barver Battle Saga) (Taiwan) (Unl).md` | N.....N....ZN.....N....pF.L.\|.NsJ9..`.g...BA.9.. | 2 colours |
| `Taipingyang Zhengba Zhan (China) (Pirate).md` | P.T.O. | 1 colours |
| `Taiwan Daheng (Taiwan) (Unl).md` | "\|.... <.........n..Q...N.....3..D.....9......g. | 1 colours |
| `Tecmo Cup Football Game (Europe) (Proto).md` | TECMO CUP | 1 colours |
| `Tecmo Super Bowl (Japan) (En).md` | TECMO SUPER BOWL | 2 colours |
| `Tecmo Super Bowl (USA).md` | TECMO SUPER BOWL | 2 colours |
| `Tecmo Super Bowl II - Special Edition (Japan) (En).md` | TECMO SUPER BOWL2 SPECIAL EDITION | 4 colours |
| `Tecmo Super Bowl II - Special Edition (USA).md` | TECMO SUPER BOWL2 SPECIAL EDITION | 4 colours |
| `Teenage Mutant Ninja Turtles - The Hyperstone Heist (USA).md` | TEENAGE MUTANT NINJA TURTLES | 1 colours |
| `Terminator, The (Europe).md` | THE TERMINATOR | 1 colours |
| `Terminator, The (USA) (Beta) (1992-04-14).md` | THE TERMINATOR | 1 colours |
| `Terminator, The (USA).md` | THE TERMINATOR | 1 colours |
| `Thunder Force IV (Europe).md` | THUNDER FORCE 4 | 1 colours |
| `Thunder Force IV (Japan) (En) (Sega Ages).md` | THUNDER FORCE 4 | 4 colours |
| `Thunder Force IV (Japan) (En).md` | THUNDER FORCE 4 | 1 colours |
| `Thunder Fox (Japan).md` | THUNDER FOX | 1 colours |
| `Thunder Fox (USA).md` | THUNDER FOX | 1 colours |
| `Tick, The (USA).md` | The Tick | 1 colours |
| `Time Killers (Europe).md` | TIMEKILLERS | 1 colours |
| `Time Killers (USA).md` | TIME KILLERS | 1 colours |
| `Tinhead (USA) (Rev 1).md` | TINHEAD | 1 colours |
| `Tinhead (USA).md` | TINHEAD | 1 colours |
| `Tiny Toon Adventures - Buster's Hidden Treasure (Europe).md` | TINY TOON ADVENTURES | 3 colours |
| `Tiny Toon Adventures - Buster's Hidden Treasure (USA).md` | TINY TOON ADVENTURES | 3 colours |
| `Tiny Toon Adventures 3 (Taiwan) (En) (Unl).md` | GHTS RESERVED OF XXXXXX LTD. | 1 colours |
| `Tom Mason's Dinosaurs for Hire (USA) (Beta) (1993-03-xx).md` | DINO'S FOR HIRE | 3 colours |
| `Tom Mason's Dinosaurs for Hire (USA) (Beta) (1993-04-26).md` | DINOS FOR HIRE | 1 colours |
| `Tom Mason's Dinosaurs for Hire (USA) (Beta) (1993-04-27).md` | DINOS FOR HIRE | 1 colours |
| `Tom Mason's Dinosaurs for Hire (USA) (Beta) (1993-05-02).md` | DINOS FOR HIRE | 1 colours |
| `Tom Mason's Dinosaurs for Hire (USA).md` | DINOS FOR HIRE | 1 colours |
| `Triple Play - Gold Edition (USA).md` | Triple Play Gold | 1 colours |
| `Triple Play 96 (USA).md` | Triple Play Baseball | 1 colours |
| `Troy Aikman NFL Football (USA).md` | Troy Aikman NFL Football | 2 colours |
| `Turrican (USA, Europe) (Unl).md` | TURRICAN | 1 colours |
| `Twinkle Tale (Japan).md` | TWINKL TALE | 1 colours |
| `Two Tribes - Populous II (Europe).md` | POPULOUS II | 4 colours |
| `Tyrants - Fight Through Time (USA).md` | MEGA lo MANIA | 1 colours |
| `Ultimate Mortal Kombat 3 (USA).md` | ULTIMATE MORTAL KOMBAT 3 | 1 colours |
| `Ultracore (Japan) (En) (Columbus Circle).md` | ULTRACORE | 1 colours |
| `Ultraman (Japan).md` | .E...g...}.. | 2 colours |
| `Uncharted Waters (USA).md` | UNCHARTED WATERS | 4 colours |
| `Unnecessary Roughness '95 (USA).md` | UNNECESSARY ROUGHNESS '95 | 4 colours |
| `VS Puyo Puyo Sun (USA, Europe) (Mega Drive Mini 2, Genesis Mini 2).md` | PUYO PUYO SUN | 1 colours |
| `Virtua Racing (Europe) (Beta).md` | Virtua Racing | 1 colours |
| `Virtua Racing (Europe).md` | Virtua Racing | 1 colours |
| `Virtua Racing (Japan) (En).md` | Virtua Racing | 1 colours |
| `Virtua Racing (USA).md` | Virtua Racing | 1 colours |
| `WWF WrestleMania - The Arcade Game (USA, Europe).md` | WRESTLEMANIA | 1 colours |
| `Warrior of Rome II (USA).md` | worrior of rome2 | 1 colours |
| `Warsong (USA) (Beta) (1991-10-01).md` | WARSONG | 1 colours |
| `Warsong (USA).md` | WARSONG | 1 colours |
| `WeaponLord (USA).md` | WEAPONLORD | 2 colours |
| `Wheel of Fortune (USA).md` | WHEEL OF FORTUNE | 2 colours |
| `Where in Time Is Carmen Sandiego (Brazil) (Es,Pt).md` | WHERE IN TIME IS CARMEN SANDIEGO | 4 colours |
| `Where in Time Is Carmen Sandiego (USA, Europe) (En,Fr,De,Es,It).md` | WHERE IN TIME IS CARMEN SANDIEGO | 1 colours |
| `Where in the World Is Carmen Sandiego (Brazil) (Es,Pt).md` | Where in the World is Carmen Sandiego? | 1 colours |
| `Where in the World Is Carmen Sandiego (USA, Europe) (En,Fr,De,Es,It).md` | Where in the World is Carmen Sandiego? | 1 colours |
| `Winter Olympic Games (USA) (En,Fr,De,Es,It,Pt,Sv,No).md` | WINTER OLYMPICS | 3 colours |
| `Winter Olympics (Europe) (En,Fr,De,Es,It,Pt,Sv,No).md` | WINTER OLYMPICS | 3 colours |
| `Winter Olympics (Japan) (En,Fr,De,Es,It,Pt,Sv,No).md` | WINTER OLYMPICS | 1 colours |
| `Wisdom Tree Collection (USA) (Piko Interactive) (Unl).md` | INC..09....09... 09... .9.... .. g.#.SEGA..@.09 | 4 colours |
| `Wiz 'N' Liz (USA).md` | WIZ 'N' LIZ IN THE FRANTIC WABBIT WESCUE | 1 colours |
| `Wiz 'n' Liz - The Frantic Wabbit Wescue (Europe).md` | WIZ 'N' LIZ IN THE FRANTIC WABBIT WESCUE | 1 colours |
| `Wolfchild (USA).md` | WOLFCHILD | 1 colours |
| `Wolverine - Adamantium Rage (USA, Europe).md` | WOLVERINE ADAMANTIUM RAGE | 2 colours |
| `Wonder Library (Japan) (Program).md` | WONDER LIBRARY | 1 colours |
| `Wonder MIDI (Japan) (En) (Program).md` | MIDI World | 1 colours |
| `World Cup USA 94 (USA, Europe, Korea) (En,Fr,De,Es,It,Nl,Pt,Sv).md` | WORLD CUP USA'94 | 3 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-14).md` | JAPAN GAME NAME | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-18).md` | JAPAN GAME NAME | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-20).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-25).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-01-30).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-02).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-03).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-07).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-09) (Alt).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-09).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-11).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-12).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-13).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA) (Beta) (1995-02-14).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '95 (USA).md` | WORLD SERIES BASEBALL 95 | 1 colours |
| `World Series Baseball '96 (USA) (Demo) (Sega Channel) (Test Drive).md` | WORLD SERIES BASEBALL 96 | 1 colours |
| `World Series Baseball '96 (USA).md` | WORLD SERIES BASEBALL 96 | 1 colours |
| `World Series Baseball (USA) (Beta) (1993-12-22).md` | BASEBALL WORLD SERIES | 1 colours |
| `World Series Baseball (USA) (Beta) (1994-03-04).md` | BASEBALL WORLD SERIES | 1 colours |
| `World Series Baseball 98 (USA).md` | WORLD SERIES BASEBALL 98 | 1 colours |
| `World Trophy Soccer (USA).md` | Club Soccer (Console Edition) | 1 colours |
| `World of Illusion (China) (En) (Pirate).md` | World of Illusion starringMickeyMouse&DonaldDuck | 4 colours |
| `World of Illusion Starring Mickey Mouse and Donald Duck (Europe).md` | World of Illusion starringMickeyMouse&DonaldDuck | 3 colours |
| `World of Illusion Starring Mickey Mouse and Donald Duck (USA, Korea) (En).md` | World of Illusion starringMickeyMouse&DonaldDuck | 4 colours |
| `World of Illusion starring Mickey Mouse & Donald Duck (USA, Korea) (En) (Beta 1).md` | Mickey Mouse & Donald Duck WorldOfIllusion | 4 colours |
| `World of Illusion starring Mickey Mouse & Donald Duck (USA, Korea) (En) (Beta 2).md` | Mickey Mouse & Donald Duck WorldOfIllusion | 4 colours |
| `Worms (Europe) (Beta).md` | WORMS | 2 colours |
| `Wushu Shen Jie (China) (Pirate).md` | SORCERER_S KINGDOM | 2 colours |
| `X-Men 2 - Clone Wars (USA, Europe) (Beta) (1994-10-18).md` | X-MEN 2 | 1 colours |
| `XDR - X-Dazedly-Ray (Japan) (En).md` | XDR | 1 colours |
| `Zero the Kamikaze Squirrel (Europe).md` | ZERO THE KAMIKAZE SQUIRREL | 4 colours |
| `Zero the Kamikaze Squirrel (USA) (Limited Run Games).md` | ZERO THE KAMIKAZE SQUIRREL | 4 colours |
| `Zero the Kamikaze Squirrel (USA).md` | ZERO THE KAMIKAZE SQUIRREL | 4 colours |
| `Zool (Europe) (Beta) [b].md` | Zool | 1 colours |
| `Zoop (Europe).md` | ZOOP | 4 colours |
| `Zoop (USA) (Beta).md` | ZOOP | 4 colours |
| `Zoop (USA).md` | ZOOP | 4 colours |
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

### Panic (97 - see the CSV for the full table)

| ROM | Title | Notes |
|---|---|---|
| `Boxing Legends of the Ring (USA).md` | BOXING LEGENDS OF THE RING | 263 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket (Australia).md` | BRIAN LARA CRICKET | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket (Europe).md` | BRIAN LARA CRICKET | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket 96 (Europe) (Rev 1).md` | BRIAN LARA 96 | 314 lines and the VDP has not raised `frame_done` |
| `Brian Lara Cricket 96 (Europe).md` | BRIAN LARA 96 | 314 lines and the VDP has not raised `frame_done` |
| `Chaoji Mahjong Club (Taiwan) (Unl).md` | Super Majon ClubVersion 1.00 Bill,Will & Andy | not implemented: m68k opcode 4741 |
| `Chavez II (Mexico).md` | CHAVEZ 2 | 263 lines and the VDP has not raised `frame_done` |
| `Comix Zone (Europe) (Beta) (1995-06-28).md` | COMIX ZONE | 314 lines and the VDP has not raised `frame_done` |
| `Comix Zone (Europe) (Beta) (1995-07-12).md` | COMIX ZONE | 314 lines and the VDP has not raised `frame_done` |
| `Cyber Police - ESWAT (Japan) (En).md` | ESWAT | 263 lines and the VDP has not raised `frame_done` |
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
| `Legende de Thor, La (France) (Mega Drive Mini).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `Legende de Thor, La (France).md` | THE STORY OF THOR | 314 lines and the VDP has not raised `frame_done` |
| `MK5 - Mortal Kombat Mythologies - Sub-Zero (Taiwan) (En) (Unl).md` | g&N.....E.....N.....Jy...lg E.....N.....N.. _^L. | 263 lines and the VDP has not raised `frame_done` |
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
| `Ottifants, The (Europe) (De) (Beta).md` | OTTIFANT | 314 lines and the VDP has not raised `frame_done` |
| `Ottifants, The (Europe) (En,Fr,De,Es,It).md` | THE OTTIFANTS | 314 lines and the VDP has not raised `frame_done` |
| `Paperboy (USA, Europe) (Beta) (1991-10-28).md` | PaperBoy | 263 lines and the VDP has not raised `frame_done` |
| `Paperboy 2 (USA, Europe).md` | PAPERBOY 2 | 263 lines and the VDP has not raised `frame_done` |
| `Pro Action Replay (Europe) (Program) (Unl).md` | ACTION REPLAY 2 | not implemented: m68k opcode 4741 |
| `Pro Action Replay 2 (Europe) (v2.1) (Program) (Unl) (Alt).md` | ACTION REPLAY 2 | not implemented: m68k opcode 4741 |
| `Pro Action Replay 2 (Europe) (v2.1) (Program) (Unl).md` | unknown | not implemented: m68k opcode 00FF |
| `Ren & Stimpy Show Presents, The - Stimpy's Invention (Europe) (1993-12-09) (Sega Channel).md` | STIMPY'S INVENTION | 314 lines and the VDP has not raised `frame_done` |
| `Ren & Stimpy Show Presents, The - Stimpy's Invention (Europe).md` | STIMPY'S INVENTION | 314 lines and the VDP has not raised `frame_done` |
| `Risk (USA).md` | RISK | 263 lines and the VDP has not raised `frame_done` |
| `Samurai Shodown (Europe).md` | SAMURAI SHODOWN | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (Europe) (En,Fr,De,It) (Beta).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (Europe) (En,Fr,De,It).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Sensible Soccer (World) (En,Fr,De,It) (Evercade).md` | SENSIBLE SOCCER | 314 lines and the VDP has not raised `frame_done` |
| `Shane Warne Cricket (Australia).md` | SHANE WARNE CRICKET | 314 lines and the VDP has not raised `frame_done` |
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
| `Tom Clown (Taiwan) (En) (Unl).md` | 1993. JUN. GAME:TOM CLOWN PROGRAMER:JAMES YO | not implemented: m68k opcode 0AC6 |
| `Ultracore (Europe) (Strictly Limited Games).md` | ULTRACORE | 314 lines and the VDP has not raised `frame_done` |
| `Ultracore (USA, Europe) (Switch, PS Vita).md` | ULTRACORE | 314 lines and the VDP has not raised `frame_done` |
| `WWF Super WrestleMania (USA, Europe).md` | WWF | 263 lines and the VDP has not raised `frame_done` |
| `WWF WrestleMania - The Arcade Game (USA, Europe) (Beta).md` | unknown | 263 lines and the VDP has not raised `frame_done` |
| `WildSnake (USA) (Proto).md` | WILDSNAKE | 263 lines and the VDP has not raised `frame_done` |
| `World Heroes (Asia) (En) (Beta) (1994-04-20) (Alt) [b].md` | O.L. .E.O.S | not implemented: m68k opcode 00FF |

