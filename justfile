# Run `just --list` to see recipes. Always run inside `nix develop`.

core := "-p m68k -p z80 -p vdp -p ym2612 -p sn76489 -p genesis"

# The VDP debug views are behind a cargo feature, off by default, so nothing
# else compiles them at all: `just check` has to say so twice (#104).
vdp-debug := "-p vdp --features debug-views"

default: check

# Everything a commit must pass.
check: fmt-check clippy test wasm-core

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

# Commercial ROM smoke tests (M6-04, #123): local only, they read $RUSTSEGA_ROM_DIR,
# which CI does not have. The name filter is what `--run-ignored only` needs: it
# picks the one #[ignore]d test of tests/commercial.rs out of the genesis crate.
test-commercial:
    cargo nextest run -p genesis --run-ignored only commercial

# Public CPU conformance suites -> test-roms/ (pinned, idempotent).
fetch:
    scripts/fetch-test-data.sh

# Core crates must always build for the browser.
wasm-core:
    cargo build --target wasm32-unknown-unknown {{core}}
