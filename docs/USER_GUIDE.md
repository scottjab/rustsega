# User guide

How to get rustsega running, how to play it, and what to do when it does not
behave. For how the thing is built see [ARCHITECTURE.md](ARCHITECTURE.md); for
which games work see [compat.md](compat.md).

## Quick start

rustsega is one executable with the whole web app inside it. You point it at a
directory of ROMs, it starts a web server, you play in a browser tab.

1. Get the binary. [Releases](https://github.com/scottjab/rustsega/releases)
   carries `rustsega-v<version>-x86_64-linux`, a static Linux binary that needs
   nothing else on the machine, and its `.sha256` next to it:

   ```sh
   sha256sum -c rustsega-v*-x86_64-linux.sha256
   chmod +x rustsega-v*-x86_64-linux
   mv rustsega-v*-x86_64-linux ~/.local/bin/rustsega
   ```

   On Nix there is nothing to download, because the binary is one build:

   ```sh
   nix run github:scottjab/rustsega -- --rom-dir ~/roms
   ```

2. Point it at your collection:

   ```sh
   rustsega --rom-dir ~/roms
   ```

   It answers with `rustsega listening on http://127.0.0.1:8080`. The directory is
   scanned for `.md`, `.bin`, `.gen` and `.smd` files, up to 10 MiB each.

3. Open <http://127.0.0.1:8080> and click a game.

No library, or a game that is not in it? Drag the file onto the page, or press
`Open ROM`. Either way the file goes from your disk to the emulator: it is not
uploaded to the server.

By default the server answers loopback only, because it has no accounts and no
password: whoever reaches the port can play every ROM in the directory and read
nothing else of your machine. `--bind 0.0.0.0` opens it to your network. TLS is
not in this binary either, so put a reverse proxy in front for anything that is
not your own laptop.

## Command line

Every flag has an environment variable, because a container often cannot pass a
command line. A flag beats the variable, which beats the default.

| Flag | Environment | Default | Meaning |
|---|---|---|---|
| `--bind <addr>` | `RUSTSEGA_BIND` | `127.0.0.1` | Address to answer on. `0.0.0.0` for the whole network. |
| `--port <port>` | `RUSTSEGA_PORT` | `8080` | Port. `0` asks the OS for a free one and prints what it got. |
| `--rom-dir <dir>` | `RUSTSEGA_ROM_DIR` | unset | Directory of ROMs to list. Unset runs with no library. |
| `--log <level>` | `RUSTSEGA_LOG` | `info` | One of `trace`, `debug`, `info`, `warn`, `error`, `off`. |

`--rom-dir` is checked before the port opens, so a typo is a usage error naming
the directory and a status of 2, not a silently empty list.

There are three endpoints worth knowing about besides the app itself:
`/healthz` for a liveness probe, `/api/info` for the version, and
`POST /api/roms/rescan` for picking up files you copied in after the server
started.

## NixOS

The flake carries a NixOS module, `nixosModules.default` (also
`nixosModules.rustsega`):

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rustsega.url = "github:scottjab/rustsega";
  };

  outputs = inputs @ { nixpkgs, ... }: {
    nixosConfigurations.playbox = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        inputs.rustsega.nixosModules.default
        {
          services.rustsega = {
            enable = true;
            romDir = "/srv/roms/genesis";
            bind = "127.0.0.1";
            port = 8080;
            logLevel = "info";
          };
        }
      ];
    };
  };
}
```

Every option is one flag of the binary, so `systemctl cat rustsega` shows the
whole configuration:

| Option | Default | Flag it sets |
|---|---|---|
| `enable` | `false` | |
| `package` | this flake's `packages.default` | |
| `bind` | `"127.0.0.1"` | `--bind` |
| `port` | `8080` | `--port` |
| `romDir` | `null` | `--rom-dir` |
| `logLevel` | `"info"` | `--log` |
| `openFirewall` | `false` | opens `port`, nothing else |

Three things to know before you start it:

- The unit runs as a dynamic user, so `romDir` has to be readable by a user that
  does not exist until the service starts. `/srv/roms` is the place where that is
  normal. `/home/you/roms` is the place that fails, and `romDir` under `/home` is
  read-only either way.
- The dynamic user cannot bind a port below 1024. Serve on 8080 and reverse proxy
  it, or use `systemd.socket` if you really want 443 handled here.
- Nothing authenticates, so `bind` and `openFirewall` are the only lock on it.
  `bind = "0.0.0.0"` with `openFirewall = true` publishes the collection to
  whoever is on that network.

`nix build .#checks.x86_64-linux.nixos-module` boots this configuration in a
virtual machine and asks it `/healthz`, `/api/info` and `/api/roms`.

## Behind a reverse proxy

The server sets `Content-Type: application/wasm` itself and compresses what it
serves, so a proxy only has to pass bytes through and keep the `Host` header.
Terminate TLS at the proxy and point it at the loopback port.

nginx:

```nginx
server {
  listen 443 ssl http2;
  server_name emu.example.com;

  # Your certificate, however you get one (certbot, acme, a mount).

  location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
  }
}
```

Caddy does the whole thing, certificate included:

```caddyfile
emu.example.com {
  reverse_proxy 127.0.0.1:8080
}
```

Keep the app on its own origin. Saves live in the browser storage of whatever
origin served them, so `http://emu.example.com` and
`https://emu.example.com` are two separate collections of games and saves.

## Controls

### Keyboard

Player 1 by default. The d-pad is on the arrow keys and the buttons are on the
left hand, on the home row and the row above it, so two hands on the keyboard sit
like two hands on a pad.

| Genesis button | Key |
|---|---|
| Up | ↑ |
| Down | ↓ |
| Left | ← |
| Right | → |
| A | A |
| B | S |
| C | D |
| X | Q |
| Y | W |
| Z | E |
| Start | Enter |
| Mode | Right Shift |

Player 2 starts unbound, because one player is the common case. Bind a second
player in the settings drawer under Controls.

Rebinding: open the drawer (`Esc`), go to Controls, click the key you want to
change and press the new one. `Esc` while it is waiting cancels. The hotkeys
below are fixed and the page tells you so if you try to take one.

### Controller

Any pad the browser reports through the Gamepad API works, and pad number one is
player one, pad two is player two. The default map is the standard mapping, which
means the button names below are the ones on an Xbox-style pad:

| Genesis button | Controller button |
|---|---|
| Up, Down, Left, Right | d-pad, or the left stick |
| A | X |
| B | A |
| C | B |
| X | LB |
| Y | Y |
| Z | RB |
| Start | Start |
| Mode | Select |

The stick reads eight directions and ignores the middle quarter of its travel, so
a slightly-drifty stick does not walk you into a wall. While the settings drawer
is open the same pad drives the menu: the d-pad or stick moves, `A` confirms, `B`
goes back, and `Home`, or `Select` and `Start` together, closes it. Hold `RT` for
fast forward.

A pad that has never been pressed is invisible to every browser, so press a
button on it once the page is open.

### Touch

On a phone or tablet the page draws an overlay: a d-pad disc in the lower left,
the six face buttons in the lower right with `X`, `Y`, `Z` in a row above them,
`Start` and `Mode` as pills along the bottom, and a menu button in the top right
corner. A finger can slide off a button and keep holding it, and the d-pad reads
diagonals.

It appears by itself when the browser says the pointer is a finger. Under
`Controls` in the drawer, `Show touch controls` has `Automatic`, `Always` and
`Never` for when that guess is wrong, and `Hide touch controls when a controller
is connected` takes the overlay out of the way once a real pad is plugged in.

### Hotkeys

| Key | Does |
|---|---|
| Esc | Open or close the settings drawer |
| F1 | Save state to the current slot |
| F4 | Load state from the current slot |
| F2 | Previous save slot, of four |
| F3 | Next save slot, of four |
| Tab | Held: fast forward |
| P | Hold the console (pause) |
| F11 | Fullscreen |
| `` ` `` | FPS readout on and off |

## Saves

Both kinds of save live in the browser, in an IndexedDB database named `rustsega`
under the site's own origin:

- **Battery saves** (the SRAM a real cart would keep alive with a battery) are
  written back automatically while you play, and again when you hide or close the
  tab.
- **Save states** are the emulator's own snapshot of everything: four slots per
  game, with a thumbnail and a time, reached by F1, F4, F2 and F3, or by the
  `Save states` button.
- ROM images you have opened recently are kept alongside them so the same game
  reopens without the server that served it.

What that means in practice:

- Saves belong to one browser on one machine, per origin. A different browser, a
  different machine, or `http` versus `https` all start empty.
- **There is no export or import yet.** Clearing site data, clearing browsing
  history, or using a private window really does delete them. If a save matters
  to you, do not clear this site's data.
- A save is keyed by the contents of the ROM, so two dumps of one game (a release
  and its `Rev A`) get two separate saves. That is deliberate: the two are not
  guaranteed to mean the same thing to each other.

## Troubleshooting

**No sound.** Browsers will not let a page make noise until you have interacted
with it. Click once on the page or press a key. If it is still quiet, check the
volume and mute control in the drawer, then the system mixer.

**The controller does nothing.** Press a button on it. A pad that has not been
pressed since the page opened is not reported to the page at all, by design, and
there is nothing the emulator can do about that. To see whether the browser has
the pad at all, open `Controls` and click a binding: it waits for a pad button
exactly the way it waits for a key. Then check that the browser supports the
Gamepad API.

**Stutter.** Press `` ` `` (the key next to `1`) and watch the number. Then in
the drawer, under `Picture`, set `Renderer` to `Canvas 2D`: the WebGL path is
faster on most machines, but there are drivers it disagrees with, and the 2D path
is the fallback. Close other tabs, since the console shares its process with
everything else in the browser. Held `Tab` (fast forward) and a held console both
move the number too, so check neither is stuck on.

**A game will not start, or shows black.** Check
[compat.md](compat.md) first: most causes are already filed there with an issue
number. If your game is not on the list, open an issue naming the ROM. Name it,
do not attach it: ROMs are not committed to this repository and cannot be
debugged from a copy that arrived through an issue.

**The game list is empty.** Check that `--rom-dir` was pointed at a directory that
exists and holds `.md`, `.bin`, `.gen` or `.smd` files. The directory is scanned
when the server starts; for files you added afterwards:

```sh
curl -X POST http://127.0.0.1:8080/api/roms/rescan
```

**The page says the frontend is not embedded.** That build has no web app in it.
`just build` (or `nix build`) makes the real thing.

**Nothing at all happens and the console is full.** Open devtools and read the
error. If it is a panic from the wasm, that is a bug worth an issue with the
message and the browser version pasted in.
