# Run `just --list` to see recipes. Always run inside `nix develop`.

core := "-p m68k -p z80 -p vdp -p ym2612 -p sn76489 -p genesis"

# The VDP debug views are behind a cargo feature, off by default, so nothing
# else compiles them at all: `just check` has to say so twice (#104).
vdp-debug := "-p vdp --features debug-views"

default: check

# Everything a commit must pass.
check: fmt-check clippy test wasm-core wasm-web

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

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

# Serve the frontend with trunk's dev server (autoreload) on http://127.0.0.1:8081.
# This is the debug build: fast to compile, ~13x the wasm, not a thing to measure fps on.
web-serve:
    cd crates/web && trunk serve --open=false --address 127.0.0.1 --port 8081

# Same as web-serve but with --release (#402): autoreload plus the optimized wasm.
# Use this one to look at frame pacing, the renderer and audio timing.
web-serve-release:
    cd crates/web && trunk serve --release --open=false --address 127.0.0.1 --port 8081
