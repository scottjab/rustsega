# rustsega

A Sega Genesis / Mega Drive emulator written in Rust. It runs in the browser
through WebAssembly and deploys as one binary: the server embeds the whole web
app, so you copy one file and run it.

Status: planning, no code yet (the first ticket creates the Cargo workspace). Work is tracked in GitHub issues; the issue labeled
`tracking` lists every ticket in order.

## Goals

- Accuracy first. Games should run the way they do on real hardware, and the
  test suite proves it (CPU conformance suites, hardware test ROMs, golden
  frames from real games).
- Full speed in the browser (60 fps NTSC, 50 fps PAL).
- Rust everywhere. The UI is Leptos compiled to WASM, no hand written JS.
- Play with keyboard, gamepad, or touch.
- Battery saves, save states, rebindable controls, 3 and 6 button pads.

## Planned layout

```
crates/m68k      68000 CPU
crates/z80       Z80 CPU
crates/vdp       video chip
crates/ym2612    FM sound
crates/sn76489   PSG sound
crates/genesis   the console: bus, cartridge, I/O, scheduler, save states
crates/testkit   test helpers
crates/headless  CLI runner used by tests
crates/web       browser UI (Leptos + trunk)
crates/server    single-binary server that embeds the UI
```

More in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/TESTING.md](docs/TESTING.md).

## Development

You need Nix with flakes enabled. Nothing else.

```sh
nix develop            # or `direnv allow`
just check             # fmt, clippy, tests, wasm build of the core
just test-all          # also runs the slow conformance suites
```

Commercial ROMs for local tests are read from `$RUSTSEGA_ROM_DIR`
(default `~/games/roms/genesis`). Public test suites go in `./test-roms`
via `scripts/fetch-test-data.sh`. Neither is ever committed.

## Working with agents

Coding agents should read [AGENTS.md](AGENTS.md). Short version:

```sh
nix develop -c scripts/next-ticket.sh
```

prints the next ticket to work on.
