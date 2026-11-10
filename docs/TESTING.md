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

## Layers of tests

| Layer | Where | What |
|---|---|---|
| Unit | `#[cfg(test)] mod tests` next to the code | One behavior per test. Hand-assembled opcodes, register pokes. |
| CPU conformance | `crates/m68k/tests/`, `crates/z80/tests/` | SingleStepTests JSON suites, zexdoc/zexall. |
| Chip tests | `crates/vdp/tests/`, audio crates | Register writes in, pixels/samples out. Golden values. |
| Test ROMs | `crates/genesis/tests/` | Homebrew accuracy ROMs run headless; frame hashes and on-screen pass/fail checked. |
| Commercial smoke | `crates/genesis/tests/commercial.rs` | Boot real games N frames, assert no panic, non-blank output, golden frame hash. |
| Frontend | `crates/web` with `wasm-bindgen-test` | Input mapping, settings, pure logic. Keep logic out of components so it is testable natively. |

`wasm-bindgen-test` runs the wasm tests in a real browser: the dev shell pins
Firefox and geckodriver and exports `GECKODRIVER` for
`wasm-bindgen-test-runner`, which runs headless unless you set `NO_HEADLESS`.
There is nothing to install per machine: both are pinned in `flake.nix`.

CPU conformance runs against the fetched SingleStepTests suites and skips (does
not fail) when the data is absent, so `just check` is green with or without
`just fetch`. Coverage is per opcode file: implementing a group means adding a
test that names its files, e.g. `harness::run_files(&["40", "41"])` in
`crates/z80/tests/`. The whole-suite runs are `#[ignore]`d so `just check` stays
quick: `all_instructions` for the m68k, which gates all 124 files at state and
clock level and is recorded in [`m68k-conformance.md`](m68k-conformance.md), and
`all_opcodes` for the Z80, which still reports per file instead of asserting so
it is useful before the instructions exist. `just test-all`, which runs on
`main`, includes both.

## External test data

Nothing copyrighted gets committed. Test data is fetched or found at test time.

- `RUSTSEGA_TEST_ROM_DIR` (default `./test-roms`, gitignored): downloaded
  conformance suites and homebrew test ROMs. Fetched by
  `scripts/fetch-test-data.sh`, which pins every download by SHA-256.
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
on main without making every PR wait for them. The 68000 conformance gate is the
exception: at 3.7 s it runs as its own CI step on every push and pull request,
with `--no-capture` so its per-file table reaches the log ([#186](https://github.com/scottjab/rustsega/issues/186)
tracks making `just test-all` do the same for every passing test). See
[`m68k-conformance.md`](m68k-conformance.md).

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
