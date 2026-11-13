# Testing

This project is test driven. Every ticket follows red, green, refactor:

1. Write the failing test(s) from the ticket's "Tests to write" section first.
   Run them and see them fail for the right reason.
2. Write the smallest code that makes them pass.
3. Clean up. Run the full check before committing.

The full check (must pass before every commit):

```sh
nix develop -c just check
```

which runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo nextest run --workspace` (or `cargo test --workspace`), and the wasm build
of the core crates.

Feature-gated code is checked with its feature on, because a gated module is not
compiled at all by the workspace run: `just check` adds the clippy and nextest of
`-p vdp --features debug-views` (the VDP debug views, #104) to its own.

## Layers of tests

| Layer | Where | What |
|---|---|---|
| Unit | `#[cfg(test)] mod tests` next to the code | One behavior per test. Hand-assembled opcodes, register pokes. |
| CPU conformance | `crates/m68k/tests/`, `crates/z80/tests/` | SingleStepTests JSON suites, zexdoc/zexall. |
| Chip tests | `crates/vdp/tests/`, audio crates | Register writes in, pixels/samples out. Golden values. |
| Test ROMs | `crates/genesis/tests/` | Homebrew accuracy ROMs run headless; frame hashes and on-screen pass/fail checked. |
| Commercial smoke | `crates/genesis/tests/commercial.rs` | Boot real games N frames, assert no panic, non-blank output, golden frame hash. The games and what they say are in [`compat-commercial.md`](compat-commercial.md). |
| Commercial gameplay | `crates/genesis/tests/gameplay.rs` | Replay a committed input movie (`tests/movies`, [`movie-format.md`](movie-format.md)) into the first level of five games and golden three frames of it plus the audio of the run. `cargo nextest run -p genesis --run-ignored only gameplay`. |
| Collection sweep | `headless sweep <ROM DIR>` | Run every ROM in the local collection in a child process and report what happened. `cargo` runs none of it; the report is a committed artifact, [`compat.md`](compat.md) over [`compat.csv`](compat.csv). |
| Frontend | `crates/web` with `wasm-bindgen-test` | Input mapping, settings, pure logic. Keep logic out of components so it is testable natively. What needs a browser runs under `just test-web`, and what a browser frame costs is measured under `just perf-web`. |

`wasm-bindgen-test` runs the wasm tests in a real browser: the dev shell pins
Firefox and geckodriver and exports `GECKODRIVER` for
`wasm-bindgen-test-runner`, which runs headless unless you set `NO_HEADLESS`.
There is nothing to install per machine: both are pinned in `flake.nix`.

Run the browser tests with `just test-web` (M7-20, #153). It sets
`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner` for one
`cargo test --target wasm32-unknown-unknown -p web`, so no `.cargo/config.toml`
is needed and the runner is only in play when the recipe asks for it. A test file
that needs a browser says so itself, with `#![cfg(target_arch = "wasm32")]` and
`wasm_bindgen_test_configure!(run_in_browser)`, and lives beside the native ones
in `crates/web/tests`; `browser_smoke.rs` is that file and the harness check. The
native tests of that directory compile for wasm and register nothing there, so
they stay with `check` on the host. `test-web` is not part of `check` -- `check`
gates core-crate commits too, and a WebDriver problem is not a reason for those
to go red -- but CI runs it as its own step, so it is checked on every pull
request. About 4 s warm, 13 s with the wasm test build.

### Browser frame budget: `just perf-web`

`just perf-web` (M7-21, #154) measures `Genesis::run_frame` in the same headless
Firefox and fails if it is over budget: 120 warm-up frames, then 600 timed ones,
average under 8.0 ms and the 99th percentile under 14.0 ms, printed either way.
It is a manual or nightly run, and deliberately in neither `check` nor CI: it
needs a cartridge and a release wasm build, CI has neither, and `test-web`
already owns the browser lane there.

The browser cannot read `$RUSTSEGA_TEST_ROM_DIR`, so the cartridge travels inside
the test binary: `crates/web/tests/browser_perf.rs` is
`include_bytes!(env!("RUSTSEGA_PERF_ROM"))` behind a `perf-rom` cargo feature of
the `web` crate that only that recipe turns on. The feature is load-bearing and
not decoration. `include_bytes!` is a compile-time read, so an unset variable is
a build failure, not a skipped test -- and `cargo test --target
wasm32-unknown-unknown -p web`, which is `just test-web`, which CI runs, compiles
every test file of the crate. A `SKIPPED` print would have needed the file to
compile first. Prove the gate to yourself by dropping `feature = "perf-rom"`
from the file's `#![cfg]` and running `cargo check --target
wasm32-unknown-unknown -p web --all-targets` with the variable unset: `error:
environment variable RUSTSEGA_PERF_ROM not defined at compile time`.

The ROM is Overdrive 2, 8,386,056 B, at
`$RUSTSEGA_TEST_ROM_DIR/genesis/overdrive2/titan-overdrive2.bin`. It is the one
pinned public ROM that is a *load* rather than a *judge*: mid-line register
writes, the VDP write FIFO, DMA during the raster, H32/H40 switching and sprite
limits, with both sound chips going on every line ([`test-roms.md`](test-roms.md)). The `md-test-pack` alternatives are 1 KB to
128 KB of pattern-and-register tests that draw a picture, print a verdict, and go
silent; they would measure the frame loop of an idle machine. What embedding it
costs, measured by building the same test binary against a 1 KB ROM instead:

| | 1 KB ROM | Overdrive 2 | delta |
|---|---:|---:|---:|
| release test wasm | 2,541,459 B | 10,926,490 B | +8,385,031 B |
| `cargo test --release --no-run`, warm | 1.15 s | 1.23 s | +0.08 s |

so the 8 MB is the ROM and nothing else - one raw data segment, and no extra work
for the optimiser - in a test binary that nothing ships. For scale, the frontend's
own `trunk build --release` wasm is 1,383,661 B.

#### The baseline

`nix develop -c just perf-web` on the dev machine: Intel Core i7-14700KF, 64 GB,
x86_64 Linux, in the Firefox and geckodriver `flake.nix` pins (Firefox 159.0.1,
geckodriver 0.37.1), headless, 2026-11-13:

| Run | avg | p99 | max |
|---|---:|---:|---:|
| 1 | 1.368 ms | 1.640 ms | 1.860 ms |
| 2 | 1.360 ms | 1.660 ms | 1.940 ms |
| 3 | 1.358 ms | 1.620 ms | 1.860 ms |
| 4 | 1.364 ms | 1.640 ms | 1.860 ms |

Ten thousandths of a millisecond of spread on the average between runs. The test
prints the colour count of its last frame with the timings - 10, for Overdrive 2
at frame 720 - and asserts on it, because a budget met by a machine that stopped
drawing is a budget met by an idle machine.

A sixth of the average budget and an eighth of the tail: this machine is not near
the gate, so the gate is the thing worth watching. Numbers from another machine,
browser, or browser version are a second baseline rather than a regression - what
`run_frame` costs is a property of the whole box, which is why the budget is what
is asserted and this table is only where one box sits under it.

If it ever fails the budget, do not loosen the budget and do not ignore the test.
M6-11 (#131) and its follow-ups are what fixes performance, and a quiet test is
how a regression becomes permanent.

CPU conformance runs against the fetched SingleStepTests suites and skips (does
not fail) when the data is absent, so `just check` is green with or without
`just fetch`. Coverage is per opcode file: implementing a group means adding a
test that names its files, e.g. `harness::run_files(&["40", "41"])` in
`crates/z80/tests/`. The whole-suite runs are `#[ignore]`d so `just check` stays
quick: `all_instructions` for the m68k, which gates all 124 files at state and
clock level and is recorded in [`m68k-conformance.md`](m68k-conformance.md), and
`all_opcodes` for the Z80, which has every one of its 1604 files passing and is
recorded in [`z80-conformance.md`](z80-conformance.md). `just check` cannot
afford either sweep, so the Z80's `sst_sample` runs in its place: the first 20
cases of all 1604 files. `just test-all`, which runs on `main`, includes both
full sweeps.

## External test data

Nothing copyrighted gets committed. Test data is fetched or found at test time.

- `RUSTSEGA_TEST_ROM_DIR` (default: discovered, gitignored): downloaded
  conformance suites and homebrew test ROMs. Fetched by
  `scripts/fetch-test-data.sh`, which pins every download by SHA-256.
  With the variable unset, `testkit::test_rom_dir` takes the nearest
  `test-roms/` at or above the current directory and then the one in the checkout
  that owns a linked worktree, so a worktree uses the main checkout's copy
  instead of failing for want of its own (#236). An explicit value always wins,
  and discovery never invents a directory that is not there: data that cannot be
  found is still reported missing, never skipped as if it had run.
  `docs/test-roms.md` documents every hardware test ROM: what it checks, how it
  reports, its licence, and the ones that have to be placed by hand.
- `RUSTSEGA_ROM_DIR` (default `~/games/roms/genesis`): the owner's commercial
  ROM set. Note: files there use the `.md` extension (Mega Drive), they are
  raw binary ROMs, not markdown. A few are `.bin`. Some may be interleaved
  `.smd` format, detect that by content, not by name.

### Pinned by `scripts/fetch-test-data.sh`

`just fetch` (or `scripts/fetch-test-data.sh`, optionally with suite names to
fetch only some, e.g. `scripts/fetch-test-data.sh z80`) downloads these into
`test-roms/`. Each entry is pinned by commit SHA (or a fixed archive URL) in
the URL and by the SHA-256 of the downloaded archive, so CI and local runs
always see identical bytes. The archive is kept in `test-roms/.cache/` and the
verified hash in `test-roms/.stamp-<name>`; a matching stamp means no network
access at all.

| Suite | Repo | Pinned commit | Tarball SHA-256 | Lands in | Files |
|---|---|---|---|---|---|
| `680x0` | [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0) | `e0d5ece9670205cc84a0101081837deb446f86a3` | `f90dab5ba7519c6d9e20143be12e433403b6db6586f6bae3e5d3bf0123f01b50` | `test-roms/680x0/68000/v1/` | 125 |
| `z80` | [SingleStepTests/z80](https://github.com/SingleStepTests/z80) | `ebe1875d48f374bcfd4b505d8eb8ee751568b5f7` | `f44683f0edf1c1c37f98861ccd1bfda3b9bce65b5b258d4817a0f1d2012076a7` | `test-roms/z80/v1/` | 1604 |
| `md-test-pack` | Eke's Gens hardware test pack, via [Wayback](https://web.archive.org/web/20150610093059if_/http://genplus-gx.googlecode.com/files/md_test.zip) | 2015 capture | `1b37dd77e81f5bfd8e3dc09adc69609f07a30135ea611782a850e8c7336a33ba` | `test-roms/genesis/md-test-pack/` | 15 |
| `overdrive2` | TiTAN, Overdrive 2, [archive.org mirror](https://archive.org/details/pouet_69648) | 2017 release | `877d3244102531052d2ffc39e0e7390c7b1ca65a7323b49cf28d49ffb3ef6110` | `test-roms/genesis/overdrive2/` | 2 |
| `zexall-cpm` | [mdfs.net](https://mdfs.net/Software/Z80/Exerciser/), Frank D. Cringle | 2003 build | `e0f4e6af6c88764a615e0d9fa27b9bf9a2162bd77ac7492fd1f2d4fa9cdfb3bb` | `test-roms/z80/zexall-cpm/` | 6 |

What each ROM is good for, and the ROMs that have to be downloaded by hand
(240p Test Suite, Nemesis's VDP FIFO tests, the YM2612 tests, Sega's own
factory diagnostic), are in [`test-roms.md`](test-roms.md).

To bump a pin: get the new commit SHA, download
`https://github.com/<org>/<repo>/archive/<sha>.tar.gz` once and `sha256sum` it,
then update both the URL and the hash in the `MANIFEST` at the top of the
script. Delete `test-roms/.stamp-<name>` (or the whole `test-roms/` directory)
and rerun `just fetch`.

Tests that need external data must not fail when the data is missing. Use the
`testkit` helpers (`testkit::test_rom("...")` / `testkit::commercial_rom("...")`)
which return `None` when the file is absent, and print a `SKIPPED:` line.
Commercial ROM tests only ever run locally.

CI (`.github/workflows/ci.yml`) runs `scripts/fetch-test-data.sh` before
`just check`, and caches `test-roms/` under a key derived from
`hashFiles('scripts/fetch-test-data.sh')`. So the first run after a pin bump
downloads and every later run prints `up to date` and touches no network. A
fetch failure fails the job on purpose: a dead pinned URL should be loud, not a
wall of `SKIPPED`.

Heavy suites (full SingleStepTests, long ROM runs) are `#[ignore]` by default
and run with `cargo nextest run --run-ignored all` or `just test-all`. CI runs
`just test-all` as an extra step on pushes to `main`, so the slow suites report
on main without making every PR wait for them. The two conformance gates are the
exception: at 3.7 s (68000) and 6.8 s (Z80) each runs as its own CI step on
every push and pull request, with `--no-capture` so its per-file table reaches
the log ([#186](https://github.com/scottjab/rustsega/issues/186) tracks making
`just test-all` do the same for every passing test). See
[`m68k-conformance.md`](m68k-conformance.md) and
[`z80-conformance.md`](z80-conformance.md).

The commercial smoke tests of `crates/genesis/tests/commercial.rs` are ignored
for a second reason, which no CI flag undoes: CI has no cartridges. They skip
there and run locally with `just test-commercial`, which is
`cargo nextest run -p genesis --run-ignored only commercial` - the name filter is
what picks the ignored test out of the crate.

## Known public test data

Pinned and fetched: SingleStepTests 680x0 and z80, Eke's Mega Drive hardware
test pack, Overdrive 2, and ZEXDOC/ZEXALL. Documented but fetched by hand:
the 240p Test Suite, Nemesis's VDP FIFO / sprite masking / YM2612 ROMs,
r57shell's VDP and pad-timing tests, MoD's Z80 bus tests, and Sega's MD Soft
Checker. All of it, with URLs, hashes, licences and what each one is fit to
judge, is in [`test-roms.md`](test-roms.md). Add a row there before a test
starts reading a new ROM.

## Golden files

- Frame hashes: `testkit::frame_hash(pixels, width, height) -> u64`, FNV-1a 64
  over the size and then the pixel bytes, so a wrong geometry can't hash the
  same as a right one. Pixels are `&[u32]` `0xAABBGGRR`; testkit takes raw
  slices so it never has to depend on a chip crate. Goldens live in
  `crates/*/tests/golden/*.toml`.
- Golden files are a flat, key-sorted TOML table of
  `key = "0x0123456789abcdef"`, so a diff shows only the value that moved.
  Read and write them with `testkit::Golden`, compare with
  `testkit::assert_golden(path, key, actual)`.
- A mismatch panics with the key, the expected value, the actual value, and a
  pointer to bless. To record or update a value:

  ```sh
  RUSTSEGA_BLESS=1 nix develop -c cargo nextest run -p <crate> <test_name>
  ```

  Blessing is per run, so bless one test by name, never the whole workspace at
  once, or every golden gets rewritten by whatever the code currently does.
- Updating a golden needs a human-visible reason in the commit message. Dump a
  PNG with `testkit::dump_png` and look at it before accepting a new hash. A
  golden that matches garbage is worse than none. (`headless --dump-png` is the
  same thing from the CLI, once the headless ticket adds the flag.)
- Never "fix" a failing golden by regenerating it unless the ticket says the
  old one was wrong.
