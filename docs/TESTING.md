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

## External test data

Nothing copyrighted gets committed. Test data is fetched or found at test time.

- `RUSTSEGA_TEST_ROM_DIR` (default `./test-roms`, gitignored): downloaded
  conformance suites and homebrew test ROMs. Fetched by
  `scripts/fetch-test-data.sh`, which pins every download by SHA-256.
- `RUSTSEGA_ROM_DIR` (default `~/games/roms/genesis`): the owner's commercial
  ROM set. Note: files there use the `.md` extension (Mega Drive), they are
  raw binary ROMs, not markdown. A few are `.bin`. Some may be interleaved
  `.smd` format, detect that by content, not by name.

### Pinned by `scripts/fetch-test-data.sh`

`just fetch` (or `scripts/fetch-test-data.sh`, optionally with suite names to
fetch only some, e.g. `scripts/fetch-test-data.sh z80`) downloads these into
`test-roms/`. Each entry is pinned by commit SHA in the URL and by the SHA-256
of the downloaded tarball, so CI and local runs always see identical bytes.
The tarball is kept in `test-roms/.cache/` and the verified hash in
`test-roms/.stamp-<name>`; a matching stamp means no network access at all.

| Suite | Repo | Pinned commit | Tarball SHA-256 | Lands in | Files |
|---|---|---|---|---|---|
| `680x0` | [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0) | `e0d5ece9670205cc84a0101081837deb446f86a3` | `f90dab5ba7519c6d9e20143be12e433403b6db6586f6bae3e5d3bf0123f01b50` | `test-roms/680x0/68000/v1/` | 125 |
| `z80` | [SingleStepTests/z80](https://github.com/SingleStepTests/z80) | `ebe1875d48f374bcfd4b505d8eb8ee751568b5f7` | `f44683f0edf1c1c37f98861ccd1bfda3b9bce65b5b258d4817a0f1d2012076a7` | `test-roms/z80/v1/` | 1604 |

To bump a pin: get the new commit SHA, download
`https://github.com/<org>/<repo>/archive/<sha>.tar.gz` once and `sha256sum` it,
then update both the URL and the hash in the `MANIFEST` at the top of the
script. Delete `test-roms/.stamp-<name>` (or the whole `test-roms/` directory)
and rerun `just fetch`.

Tests that need external data must not fail when the data is missing. Use the
`testkit` helpers (`testkit::test_rom("...")` / `testkit::commercial_rom("...")`)
which return `None` when the file is absent, and print a `SKIPPED:` line.
CI fetches the public test data; commercial ROM tests only run locally.

Heavy suites (full SingleStepTests, long ROM runs) are `#[ignore]` by default
and run with `cargo nextest run --run-ignored all` or `just test-all`.

## Known public test data (verify each before pinning)

- SingleStepTests 68000: https://github.com/SingleStepTests/680x0 (68000 JSON,
  one file per instruction, with cycle counts and bus transactions).
- SingleStepTests Z80: https://github.com/SingleStepTests/z80
- zexdoc / zexall (Z80 instruction exerciser, CP/M .com files).
- 240p Test Suite (Genesis build) by Artemio Urbina.
- Nemesis's VDP test ROMs (VDPFIFOTesting and friends, SpritesMind).
- TiTAN "Overdrive" and "Overdrive 2" demos (stress tests, very timing sensitive).
- Search for more (e.g. "genesis test rom", "mega drive hardware test",
  "ym2612 test rom", "md vdp test rom") and add anything useful to
  `docs/test-roms.md` with URL, SHA-256, license, and what it tests.

## Golden files

- Frame hashes: `testkit::frame_hash(&FrameBuffer) -> u64` (xxhash or FNV of
  the pixel bytes). Goldens live in `crates/*/tests/golden/*.toml`.
- Updating a golden needs a human-visible reason in the commit message. Dump a
  PNG with `headless --dump-png` and look at it before accepting a new hash.
- Never "fix" a failing golden by regenerating it unless the ticket says the
  old one was wrong.
