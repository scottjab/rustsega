# Run `just --list` to see recipes. Always run inside `nix develop`.

core := "-p m68k -p z80 -p vdp -p ym2612 -p sn76489 -p genesis"

# The VDP debug views are behind a cargo feature, off by default, so nothing
# else compiles them at all: `just check` has to say so twice (#104).
vdp-debug := "-p vdp --features debug-views"

default: check

# Everything a commit must pass.
check: fmt-check actionlint clippy test wasm-core wasm-web

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# The workflow files of .github (M8-07, #162). In `check` rather than only in the
# release workflow, because nothing else reads them until a tag is pushed and by
# then a typo is a broken release rather than a red build. It is a single static
# pass over two small YAML files, so it costs milliseconds.
actionlint:
    actionlint .github/workflows/*.yml

clippy:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy {{vdp-debug}} --all-targets -- -D warnings

test:
    cargo nextest run --workspace --no-tests=pass
    cargo nextest run {{vdp-debug}}

# Includes slow #[ignore] suites (SingleStepTests, long ROM runs).
test-all:
    cargo nextest run --workspace --no-tests=pass --run-ignored all

# Milliseconds per `Genesis::run_frame` (M6-11, #131): a synthetic full-screen
# scene, and Sonic 1 when that cartridge is on disk. `cargo bench` builds with
# the release profile. It is a measurement rather than a gate, so it is not part
# of `check`; see crates/genesis/benches/frame.rs for how to read the numbers.
bench:
    cargo bench -p genesis --bench frame

# Commercial ROM smoke tests (M6-04, #123): local only, they read $RUSTSEGA_ROM_DIR,
# which CI does not have. The name filter is what `--run-ignored only` needs: it
# picks the one #[ignore]d test of tests/commercial.rs out of the genesis crate.
test-commercial:
    cargo nextest run -p genesis --run-ignored only commercial

# Public CPU conformance suites -> test-roms/ (pinned, idempotent).
fetch:
    scripts/fetch-test-data.sh

# Browser tests of the web crate (M7-20, #153): `wasm-bindgen-test` in headless
# Firefox. Cargo is pointed at the dev shell's `wasm-bindgen-test-runner`, which
# finds the pinned geckodriver through the `GECKODRIVER` the flake exports and
# runs headless unless `NO_HEADLESS=1`. Deliberately not part of `check`: this is
# the one step that needs a browser, and a WebDriver problem should not gate a
# core-crate commit. Compiling the web package for wasm finds the native tests of
# `crates/web/tests` too, and they register no test there; they run on the host,
# through `check`.
test-web:
    CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test --target wasm32-unknown-unknown -p web

# The cartridge the in-browser frame budget is measured with (M7-21, #154):
# Overdrive 2, the one pinned public ROM that is a load rather than a judge, so
# the VDP and both sound chips are busy on every line of it (docs/test-roms.md).
perf-rom-dir := env_var_or_default('RUSTSEGA_TEST_ROM_DIR', 'test-roms')
perf-rom := absolute_path(perf-rom-dir + "/genesis/overdrive2/titan-overdrive2.bin")

# The in-browser frame budget (M7-21, #154): 120 warm-up frames, then 600 timed
# ones, in headless Firefox at the release profile, against an 8.0 ms average and
# a 14.0 ms p99. Manual or nightly on purpose - it is the one measurement that
# needs a cartridge baked into the test binary and an optimized wasm build, and CI
# has neither, so it is in neither `check` nor CI. `perf-rom` is the cargo feature
# that compiles `tests/browser_perf.rs` at all; `RUSTSEGA_PERF_ROM` is the path it
# embeds - absolute, because `include_bytes!` reads a relative path against the
# source file rather than against the directory cargo was run from.
perf-web:
    test -r "{{perf-rom}}" || { echo "no {{perf-rom}}: run 'just fetch'" >&2; exit 1; }
    RUSTSEGA_PERF_ROM="{{perf-rom}}" CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test --release --target wasm32-unknown-unknown -p web --features perf-rom --test browser_perf -- --nocapture

# Core crates must always build for the browser.
wasm-core:
    cargo build --target wasm32-unknown-unknown {{core}}

# The web crate too (M7-01, #134): `trunk build` does this itself, but this
# recipe keeps it in `just check` without needing trunk or a dist directory.
wasm-web:
    cargo build --target wasm32-unknown-unknown -p web

# Bundle the browser frontend to crates/web/dist with trunk (offline: the tools
# come from the dev shell, see crates/web/Trunk.toml).
web:
    cd crates/web && trunk build --release

# The single binary (M8-01, #155): `target/release/rustsega`, the frontend baked
# in. `embed-web` is off by default, which is what lets fmt, clippy, nextest and
# the wasm builds run with no `crates/web/dist` at all; this recipe is the one
# place that turns it on, and it runs `web` first so the folder is there.
build: web
    cargo build --release -p server --features embed-web

# The portable single binary (M8-05b, #159): `result/bin/rustsega`, a static
# x86_64-linux-musl build of `just build`'s binary. It carries its own C library,
# so it runs on a Linux box with no nix and no glibc match - copy that one file.
# nix does the build, so this one runs `nix` rather than `cargo`.
build-static:
    nix build .#static

# Serve the frontend with trunk's dev server (autoreload) on http://127.0.0.1:8081.
# This is the debug build: fast to compile, ~13x the wasm, not a thing to measure fps on.
web-serve:
    cd crates/web && trunk serve --open=false --address 127.0.0.1 --port 8081

# Same as web-serve but with --release (#402): autoreload plus the optimized wasm.
# Use this one to look at frame pacing, the renderer and audio timing.
web-serve-release:
    cd crates/web && trunk serve --release --open=false --address 127.0.0.1 --port 8081
