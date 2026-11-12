# Genesis / Mega Drive test ROMs

Every hardware test ROM this project uses or plans to use, with where it came
from, what it actually checks, how it reports, and its licence. Two rules:

1. **No ROMs in git.** Everything lands in `test-roms/`, which is gitignored.
   Anything the fetch script can pull is pinned by URL *and* SHA-256.
2. **Record before you rely on it.** If you download a ROM by hand, add a row
   here with the URL, SHA-256, size, and licence before a test reads it. A test
   that depends on an undocumented ROM is a test nobody can reproduce.

See `docs/TESTING.md` section 7 for the directory layout and `just fetch`.

## Automated status

What M6-06 (#125) automated, and what is deliberately not automated. "pass" is
the state of the ROM under this emulator as of the PR that added the row; a
failing ROM gets an issue number instead of "pass" and an `#[ignore]` naming it,
and there are none: every ROM that could be automated passes.

| ROM / suite | Test | Status |
|---|---|---|
| `md-test-pack` (15 ROMs) | `crates/genesis/tests/hw_test_roms.rs`, goldens in `tests/golden/hw-test-roms.toml` | pass, 15 of 15. Per-file rows in the table below |
| `overdrive2` | `hw_test_roms.rs`: `overdrive2_runs_without_crashing_and_is_deterministic` | pass. No golden: it is a stress case, and its picture legitimately differs between VDP revisions |
| `zexall-cpm` | the Z80 conformance gate (`crates/z80/tests/`, M2-16/M2-17) | pass. It runs as a loaded `.com` image against a BDOS stub, not on the Genesis Z80 |
| 240p Test Suite, Genesis build | - | not automated: no fetchable file. itch.io has no stable download URL and the source repo ships no Genesis ROM; the licence is GPL-with-no-root-`LICENSE` (`NOASSERTION`). Pinning it means adding a release artifact upstream first |
| Nemesis's VDP FIFO, sprite masking and YM2612 ROMs; r57shell's VDP and pad tests; MoD's `gen_test_z80_bus` | - | not automated: SpritesMind attachments need a forum login, so there is nothing for `fetch-test-data.sh` to pin. See the manual table below |
| Sega's MD Soft Checker | - | not automated: a proprietary Sega cart dump, not ours to download |
| `pcm.bin`, `out.bin` + `z80driver.bin`, `gentest` | - | not automated: no licence, or source-only with no prebuilt ROM |
| `ZEXALL-SMS-0.21.zip` | - | not automated by design: an SMS cartridge image, which cannot boot on a Mega Drive bus. Reference only |
| MCD-Verificator | - | not automated by design: Sega CD hardware, out of scope |

## Fetched by `scripts/fetch-test-data.sh`

Run `nix develop -c scripts/fetch-test-data.sh` (or `just fetch`). A second run
prints `up to date` for every entry and touches no network.

| Name | Lands in | Download | SHA-256 (archive) | Licence |
|---|---|---:|---|---|
| `md-test-pack` | `test-roms/genesis/md-test-pack/` | 22,807 B | `1b37dd77e81f5bfd8e3dc09adc69609f07a30135ea611782a850e8c7336a33ba` | none stated |
| `overdrive2` | `test-roms/genesis/overdrive2/` | 4,010,970 B | `877d3244102531052d2ffc39e0e7390c7b1ca65a7323b49cf28d49ffb3ef6110` | scene freeware, no licence text |
| `zexall-cpm` | `test-roms/z80/zexall-cpm/` | 122,572 B | `e0f4e6af6c88764a615e0d9fa27b9bf9a2162bd77ac7492fd1f2d4fa9cdfb3bb` | GPL-2.0 (`Copying`) |

Every archive above was downloaded and hashed on 2026-11-06; that is also the
date the URLs were last checked. When you bump a pin, update that date.

### `md-test-pack` (Eke's Gens hardware test pack)

- **Source:** `http://genplus-gx.googlecode.com/files/md_test.zip`, the download
  page of Eke's Genesis Plus GX, assembled to check the emulator against real
  hardware. Google Code is gone, so the manifest pins a Wayback capture of the
  original file:
  `https://web.archive.org/web/20150610093059if_/http://genplus-gx.googlecode.com/files/md_test.zip`
- **Contents:** 15 files, 407,498 B unpacked. No readme and no licence text
  came with the pack, so the "what it tests" column was originally the usual
  reading of these names plus what the code sizes and dates suggest. M6-06
  (#125) ran all fifteen and replaced that guess with what each one actually
  puts on screen; two of the old guesses were wrong, and the table says so.
- **Every one of them boots as it is.** They all carry a header - eight say
  `SEGA GENESIS    (C)SEGA 2000.DEC` with a blank or `GM MK-0000` license code,
  so `genesis::parse_header` reports the title as unknown - and none of them
  needed a header stubbed on to run (M6-06 ran all fifteen).
- **Tickets:** M4-04 (status register), M4-05 (HV counter/latch), M4-06
  (interrupts), M4-07/M4-08 (DMA), M4-16 (sprite limits), M4-18
  (shadow/highlight), M4-19 (H32/H40, V28/V30), M6-03, M6-06.

| File | Size | SHA-256 | What it does, read off the screen it draws | Status |
|---|---:|---|---|---|
| `512PAL.BIN` | 1,024 | `21e44f11a072bdbd07e956892124ce65697b5a55de24da2a4dcfb58dd02cf2a4` | TmEE's 512-colour test, "50Hz only": walks the whole 9-bit palette with mid-line CRAM writes. 449 colours in a settled frame, H40 320x224 | pass (`md_test_pack_512pal`) |
| `PAL512.bin` | 4,642 | `b004a288e5dc5770e59d109e07f04265a9f61b0983aaf1acb9997b07e1237105` | a second 512-colour test, in H32: 256x240, 244 colours in repeating palette blocks | pass (`md_test_pack_pal512`) |
| `960PAL.bin` | 1,024 | `b0363e3f52fa8eb2f4bf786c5f55b38bfeeb6fda606b3a28799c9e675e5b7f99` | "960 color test - Is it possible to get more colors ???": 960 colours on screen, which needs three palette pages per frame. 872 distinct pixels in one frame, 256x240 | pass (`md_test_pack_960pal`) |
| `MD1536.bin` | 16,384 | `4bffb2e5ec2661bcb618e71a17358f530dc218a35a3397a9a558041334d15d0d` | walks all 1536 CRAM colours (512 x 3): a colour sweep over a plane/sprite field, animating on a 240-frame cycle, 1407 distinct colours in one frame | pass (`md_test_pack_md1536_frame_60`) |
| `mdtrans.bin` | 16,384 | `8bb1e85ec1cd45f2e9819f73c1b17f2f138ababbc40b51272ce89594a0c0a580` | transparency and plane priority over a scrolling field, 19 colours, animating | pass (`md_test_pack_mdtrans_frame_60`) |
| `memtest_68k.bin` | 16,384 | `6d58e6768b2d02e432101844e88787922cfffb923ea83cb3b229d82b4dfcb20a` | the 68000 memory test. Prints "68000 memory test" and a table of the ranges where a write is *not* expected to land - cartridge, I/O, the write-only VDP ports - as "range : value written (value read back)", ending "RAM, ROM, mirrored VDP addresses, and locations that lock up the machine are not tested." Work RAM is absent, i.e. it passed | pass (`md_test_pack_memtest_68k`) |
| `SHLTEST.BIN` | 3,796 | `3686e021f6f1199a0865ad25f947573659fbe1e72357873804d3d63f7cf058c9` | TmEE's shadow/highlight mode test, 2010: 16-colour ramps drawn in the four shadow/highlight modes, 17 colours | pass (`md_test_pack_shltest`) |
| `SHLTEST2.BIN` | 3,796 | `8c9a7b0980b616488235408f44784264e0b82b8f670651c0b9e8e52332167cd3` | same test, modes in another order | pass (`md_test_pack_shltest2`) |
| `stetest.bin` | 131,072 | `0c7729d63bc193ce96851c3e3fad78dceab495b4fe6a04444180346cdd3088d4` | Paul Lee sprite-table test ("DEMO PROGRAM 6", 1996): sprite shapes over solid plane bands, 256x224, settles in a few frames | pass (`md_test_pack_stetest`) |
| `STETEST2.bin` | 131,072 | `811b762c9f45c783f474f6fcc1183c8c8c94cc2a885441bfd5bbf970b41ff252` | the other one ("DEMO PROGRAM 1"), animated on a 240-frame cycle | pass (`md_test_pack_stetest2_frame_60`) |
| `vctest.bin` | 16,384 | `085115ec3e82ed4cbf4a2db5eac8bf35f7381da01a7619a88cab92388d9f0b12` | **not a colour test**: Charles MacDonald's "V counter test program" ("by Charles MacDonald, E-mail: cgfm2@hotmail.com"). A menu of the nine display modes, then the V counter values it measured per mode, driven by the pad (START runs a test, A toggles 262/312 lines, B/C page) | pass (`md_test_pack_vctest_menu`, `md_test_pack_vctest_after_start`) |
| `wtest.bin` | 16,384 | `160927b5a9f23efcea9bb72702932aea48a7e07e9c2d365927ef62906bee7419` | VDP write-timing test: a colour-block field plus the position it is writing at ("X:04 Y:08") | pass (`md_test_pack_wtest`) |
| `wtest_i1.bin` | 16,384 | `e0d1369ed5157dbb6b9599895340edbe5337ef2a50120e6d4863ab996f6aefa8` | `wtest` with interrupts enabled, variant 1 | pass (`md_test_pack_wtest_i1`) |
| `wtest_i2.bin` | 16,384 | `489e27f09f7d876064a1b5e2f1cedb510ed93232fd82e596ea44391adb843d5a` | `wtest` with interrupts enabled, variant 2, in 320x448 interlace | pass (`md_test_pack_wtest_i2`) |
| `wbug.bin` | 16,384 | `b5a984ba0aa3dd7a2a58a1576c0bed7047018dd9eb018528965d51c24e46145c` | write-timing bug repro: a block of single-pixel marks near the top-left, one per attempted VDP write, walking sideways on a 240-frame cycle | pass (`md_test_pack_wbug_frame_60`) |

**Reporting:** these are eye tests. They draw patterns or print text on screen;
there is no machine-readable pass/fail. M6-06 pinned them anyway, at 60 frames,
in `crates/genesis/tests/hw_test_roms.rs` over
`crates/genesis/tests/golden/hw-test-roms.toml`: every ROM whose output settles
has settled by then, the four that animate have a numbered frame pinned, and
every golden was checked against the screen the ROM itself prints - the pack's
text is the ROMs' own font, so the dumps were read back tile by tile against
the tile data in the ROM image rather than eyeballed.

### `overdrive2` (TiTAN, Overdrive 2, 2017)

- **Source:** `https://archive.org/download/pouet_69648/titan-overdrive2.zip`
  (a mirror of the pouet.net product page 69648). Unpacks to
  `titan-overdrive2.bin` (8,386,056 B,
  `f722fc342bbd7008a743b9970e9ccaa7f2891c4b841126cb98d5dad93da45611`) and the
  release `.nfo` (3,708 B).
- **What it tests:** everything at once. It is a 68000 demo that only runs
  right on accurate hardware: mid-line register writes, the VDP write FIFO and
  the 68k stalls it causes, DMA during the raster, the debug register, the
  128 KB address space with the SSF2 mapper, H32/H40 switching, and sprite
  limits. It is the demo that has broken the most emulators.
- **Reporting:** none. It is a *stress case, not a pass/fail judge*: its output
  legitimately differs between VDP revisions, and it needs the mapper, so
  "wrong picture" does not mean "wrong emulator". Use it to catch crashes and
  hangs, and as a long-run determinism check (same input, same frame hashes).
- **Licence:** no licence text ships with it; it is a freely distributed scene
  release. We only ever fetch it into the gitignored `test-roms/`.
- **Status:** pass, as a crash/determinism case only
  (`overdrive2_runs_without_crashing_and_is_deterministic`: 60 frames finish,
  the frame is non-blank, and two runs of it leave the same frame hash).
- **Tickets:** M6-03, M6-06 (as a crash/determinism case, not a golden), and as
  a stress test for M4-21a (FIFO), M4-22 (DMA timing), M3-16 (bus timing),
  M6-07 (save states).

### `zexall-cpm` (ZEXDOC / ZEXALL, Frank D. Cringle)

- **Source:** `https://mdfs.net/Software/Z80/Exerciser/CPM.zip` (J.G.Harston's
  site, which has hosted the CP/M and Spectrum ports since 2003). The
  canonical `zexdoc.com` is gone: the domain no longer resolves.
- **Contents:** `zexall.com` (8,587 B,
  `d2242b91ee8845102cab989163cafd24baa64290b3d16485a90caab435763a67`),
  `zexdoc.com` (8,587 B,
  `ae5caf315612ec8814272583f5ca3fa64ddbb10340c0c53899ac334bcb330e10`), both
  `.src` files (z80 assembly, ~42.8 kB each, which is the best documentation of
  the undocumented flags there is), `Copying` (GPL-2.0), and `CPMRUN` (a
  Z80Tube BASIC loader, not for us, but its 30-line BDOS stub is a nice sketch
  of what M2-16 needs).
- **What it tests:** the whole Z80 instruction set, flag by flag, including the
  undocumented X/Y flags and MEMPTR. `zexdoc` checks the documented flags only;
  `zexall` also asserts the undocumented ones, so it is the stricter gate.
- **Reporting:** plain text over the CP/M console (`.` per passing test, `Z`
  and a message per failure, ending in a summary). We capture the `putchar`
  hook in the harness: no screen emulation needed.
- **Tickets:** M2-16 (harness plus minimal BDOS) and M2-17 (conformance gate).
  Note the Genesis Z80 has no CP/M disk, so this runs as a loaded `.com` image
  with our own stub, which is what M2-16 is scoped for.

## Manual: place these by hand

Not auto-fetched, with the reason next to each. Put them under
`test-roms/genesis/<name>/` (the harness in M6-03 looks there) and add the
SHA-256 you got to this file.

| ROM | Where | Why not fetched | What it is | Tickets |
|---|---|---|---|---|
| **Nemesis, VDP FIFO / port tests** (`VDPFIFOTesting.zip`) | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=1291> ("VDP Internals", 2013) | attachments need a forum login; no stable anonymous URL found | the standard: ~122 self-checking tests of the 68k-to-VDP ports: FIFO depth and wait states, address/code registers, every DMA kind into VRAM/CRAM/VSRAM, the busy flag, wrapping, bus lock, HV latch, byte swapping, the VSRAM read cache, the Mode 4 register mask | M4-04, M4-05, M4-07, M4-08, M4-21a, M4-22 |
| **Nemesis, sprite masking and overflow test** | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=541> (2009) | same | nine tests of the 80-sprite line limit, dot overflow, and sprite/plane masking, in H32 and H40, with reference photos from a real MD1600 | M4-15, M4-16 |
| **r57shell, VDP tests** (`vdp_tests.bin`) | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=1585> ("VDP Tests", 2013) | same | mode changes, sprite link traversal, shadow/highlight priority, colour and layer interactions; judged by eye, and one of its tests is monitor-dependent | M4-15, M4-17, M4-18, M4-19 |
| **r57shell, `joy_speed_v1.02.bin`** | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=2308> ("Joypads 3/6 Button Response Timings") | same | measures how fast the console re-reads a pad, i.e. the multi-Tap / 6-button timing window | M6-09, input tickets |
| **MoD, `gen_test_z80_bus`** | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=3021> ("Z80 bus + I/O mapping redux", 2019) and <https://gendev.spritesmind.net/forum/viewtopic.php?t=1661> ("Wait States on Z80 Bank Access") | same | Z80 access to the 68k side: writes land, reads do not; the `$A0xxxx`/`$A1xxxx` mirrors, the I/O mirrors, the `/TIME` range. Comes with logic-analyser captures, which is why it is the reference for bus behaviour | M3-16, M2-17 area |
| **Nemesis, YM2612 test ROMs** (`ym2612detune.bin`, `ym2612csmmode.bin`, `ym2612multiplexing.bin`, `ym2612ssg-eg.bin`, `ym2612attackandrelease.bin`, `ym2612FrequencyWriteOrder.bin`) | SpritesMind <https://gendev.spritesmind.net/forum/viewtopic.php?t=386> (the authoritative YM2612 thread) | same | one ROM per FM subtlety: detune table, CSM, DAC multiplexing, SSG-EG, attack/release rates, register write order. Not self-checking: compare the WAV we render against the author's recordings | M5-03..M5-11 |
| **240p Test Suite, Genesis build** | binaries: <https://artemiourbina.itch.io/240p-test-suite>; source: <https://github.com/ArtemioUrbina/240pTestSuite> | itch.io has no stable file URL; the GitHub repo has no prebuilt Genesis ROM (the source tarball is 68 MB of other consoles) | video-pattern suite (240p timing, geometry, colour, audio levels) plus the MDFourier signal generator. Its Genesis README says "free software, with full source code available under the GPL"; GitHub reports `NOASSERTION` because there is no root `LICENSE` | M6-06 (the goldens ticket names this file), M5-14 |
| **Sega's MD Soft Checker (v0.30, USA proto)** | No-Intro / your own dump | proprietary Sega diagnostic ROM, not ours to download for you | the factory test cart: 68000 registers and the whole instruction set, work RAM, the Z80 bus request and its RAM, VRAM/CRAM/VSRAM, the I/O registers, every DMA type, VRAM fill and copy, plus pad detection. Prints its own verdict per check, so it is the best single self-checking ROM that exists | M1-14 area, M2-17, M3-16, M4-07/M4-08 |
| **`pcm.bin`** (WinFutureX, `pcmtest`) | <https://github.com/WinFutureX/pcmtest> (ROM in tree) | no licence file, so we should not redistribute it | plays PCM samples through the 68k DMA sound DAC: exercises the `$M0` PCM channel and DMA-over-bus behaviour | M5-09 area, M3-16 |
| **`out.bin` + `z80driver.bin`** (forrestengland, `md-seq-pcm-psg-ym`) | <https://github.com/forrestengland/md-seq-pcm-psg-ym> | same, no licence | a PCM/PSG sequencer: Z80 driving both sound chips, so it stresses the Z80 bank switch and the sound ports together | M3-16, M5 area |
| **`gentest`** (Charles MacDonald, `clbr/gentest`) | <https://github.com/clbr/gentest> | source only, no prebuilt ROM, no licence | a small hand-written test for one specific Genesis bug, plus its capture audio; needs our own assembler | open a ticket if a specific bug needs it |
| **`ZEXALL-SMS-0.21.zip`** (maxim-zhao, `zexall-sms`) | <https://github.com/maxim-zhao/zexall-sms/releases/download/v0.21/ZEXALL-SMS-0.21.zip> (52,991 B, `704e39402516fba832b923395c7579b617896e1f00d2f036696342ab4b5fde7c`, GPL-2.0) | it is an SMS cartridge image: the Master System cart port is not a valid Mega Drive cart, so it cannot boot on our bus | a port of ZEXALL to SMS hardware, useful for reading how the exerciser behaves on real Z80 silicon, and the fix list in its README documents real-hardware gotchas | reference only |
| **MCD-Verificator** (krikzz, `MEGA-PRO/mcd-verificator`) | <https://github.com/krikzz/MEGA-PRO/tree/master/mcd-verificator> | Sega CD hardware, and we do not emulate the CD bus | CD core accuracy checks | out of scope |

## Where the knowledge lives

- SpritesMind's <https://gendev.spritesmind.net/forum/viewtopic.php?t=2565>
  ("Genesis/MD test ROMs") is the closest thing to an index of this scene, and
  the thread where most of the ROMs above were posted. Read the first post of
  any thread before trusting a ROM: authors list which hardware they verified
  on, and several tests are known to fail on some Genesis 3 boards.
- `alewman/megadrive-python` (MIT,
  <https://github.com/alewman/megadrive-python>) has a `docs/validation.md`
  that maps each of these ROMs to what it can and cannot be used to judge,
  including which ones are self-checking and which are eye tests. It is the
  best map of this territory that exists; worth a read before writing a golden.
- `And-0/awesome-megadrive` is the general index of MD dev resources.

## Adding a ROM

1. Download it and `sha256sum` it. Never trust a mirrored file you did not hash.
2. Boot it in a reference emulator (or on hardware if you have it) and note
   what it does, how it reports, and whether its output is stable across runs.
3. Add a row above: author, year, URL, size, SHA-256, licence, what it tests,
   how it reports, which ticket wants it.
4. If the URL is stable and the licence allows redistribution, add a manifest
   row to `scripts/fetch-test-data.sh` (`name|url|sha256|in-archive path|dest|zip`
   for a zip) and run the script twice: the second run must print `up to date`.
   If it needs a login, a click-through, or it is commercial/proprietary, leave
   it in the manual table.
5. Say in the row whether it is **self-checking** (prints a verdict, good for a
   pass/fail test), **visual** (needs a golden frame hash plus a human), or a
   **stress case** (only proves it did not crash). Never pin a golden to a ROM
   you have not watched run.
