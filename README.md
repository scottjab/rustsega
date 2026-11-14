# rustsega

[![CI](https://github.com/scottjab/rustsega/actions/workflows/ci.yml/badge.svg)](https://github.com/scottjab/rustsega/actions/workflows/ci.yml)

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

### The single binary

```sh
nix build                                    # -> ./result/bin/rustsega
nix run . -- --rom-dir ~/games/roms/genesis  # build it, and serve the ROM library
```

One derivation, and no network: trunk builds the frontend, then the server is
compiled with the result embedded in it. What `./result` holds is therefore the
whole app, and it needs nothing else on disk to serve it. `nix flake check`
builds that binary and runs a smoke test against it; inside `nix develop`,
`just build` does the same two steps without Nix being in charge of them.

### Running it as a NixOS service

The flake carries a NixOS module, `nixosModules.default` (also under
`nixosModules.rustsega`):

```nix
services.rustsega = {
  enable = true;
  romDir = "/srv/roms/genesis";
  port = 8080;
};
```

The unit runs as a dynamic user and reads `romDir` read-only, so the collection
has to be readable by a user that does not exist until the service starts.
`/srv/roms` is the place that is normally set up for that; `/home/<user>` is the
place that is not, and needs the files group-readable or the directory bind
mounted somewhere the service may read. `bind` says which address to answer on
-- `127.0.0.1`, and so nobody but this machine, until told otherwise -- and
`openFirewall` is the other half of that decision. TLS is not in this binary: it
belongs to a reverse proxy in front of it.

`nix build .#checks.x86_64-linux.nixos-module` boots this configuration in a
virtual machine and asks it `/healthz`, `/api/info` and `/api/roms`.

### Releasing

A release is a tag. The version of the binary is the workspace version, which
`crates/server/Cargo.toml` inherits, and pushing `v<version>` builds
`nix build .#static` for that commit and puts the result on a GitHub release
next to its SHA-256:

```sh
$EDITOR Cargo.toml                              # [workspace.package] version = "0.2.0"
git commit -a -m "chore: release 0.2.0"
git tag v0.2.0
git push --follow-tags
```

The tag has to be exactly `v` and that version: the workflow reads it back out of
the manifest with `cargo metadata` and fails a run that does not agree, so a tag
that was cut before the bump is refused rather than releasing a binary that
prints a different number from the one on the tin. The two files it attaches are
`rustsega-<tag>-x86_64-linux` and `rustsega-<tag>-x86_64-linux.sha256`, and the
second one is checked in place with `sha256sum -c`.

To try all of that without publishing anything, `gh workflow run release.yml
--ref <ref>` runs the same build and checksum and leaves them on a **draft**
release, which is removed again with `gh release delete <tag> --yes`.

## Working with agents

Coding agents should read [AGENTS.md](AGENTS.md). Short version:

```sh
nix develop -c scripts/next-ticket.sh
```

prints the next ticket to work on.
