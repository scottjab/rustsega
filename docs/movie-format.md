# Movie format

M6-09 (#129). The text format `genesis::movie` writes and parses, `.rsm` files:
one run of one cartridge's controller input, small enough to read, exact enough
to replay.

A movie is not a video and not a save state. It is the list of frames on which
the pads changed, so a two-minute section of a game is usually a dozen lines, and
playing it back on the same ROM reproduces the same frames — which is what makes
it usable as a regression test that plays a real game section rather than a fake
ROM.

The code is [`movie.rs`](../crates/genesis/src/movie.rs), which never touches a
file: `headless run --movie PATH` and `--record PATH` are the file side
(`crates/headless/src/run.rs`).

## Layout

```text
rustsega-movie 1
rom-hash 0x0123456789abcdef
region usa
pads 2
# frame: pad1 pad2  (buttons as letters UDLRABCSXYZM, '.' for none)
0 . .
120 S .
121 . .
```

| Line | Field | Meaning |
|---|---|---|
| 1 | marker | `rustsega-movie` and the format version, `1` (`movie::FORMAT_VERSION`) |
| 2 | `rom-hash` | `0x` and 1 to 16 hex digits: [`movie::rom_hash`] of the cartridge |
| 3 | `region` | `usa`, `japan` or `europe` — the region the run was made on |
| 4 | `pads` | how many pads the movie covers: `1` or `2` |
| 5+ | frames | `<frame> <pad1> [pad2]`, one change of input per line |

The three header lines may come in any order and each appears exactly once; the
frame lines follow them. Everything below the marker is a comment or a frame line.

## The two rules

1. **Only the frames on which the input changes are listed, and the input holds
   until the next listed frame.** A movie that says `120 S .` means START held on
   frame 120, and still held on frame 121 unless 121 is listed too.
2. **The header names the cartridge**, so a movie cannot be played on the wrong
   game: `Movie::check_rom` compares `rom-hash` with the ROM the caller is about
   to run, and `headless` refuses the mismatch.

### Frame numbers

Frames are **0-based**: frame 0 is the first `Genesis::run_frame` after the ROM is
loaded, and frame 0 may be left out. Before the first listed frame every pad is
released, which is why a movie that begins with nothing held lists nothing.

`headless --pad1` counts from 1 — `UP@1` is the first frame — so a `--pad1`
script recorded with `--record` lands one lower: `UP@3` becomes the line `2 U`.
`--pad1` holds a button for exactly the frames it lists and releases the pad in
between, and the recorder records what the machine actually held, so the release
appears in the file as its own line:

```text
$ headless run game.md --frames 6 --pad1 UP@3,DOWN@6 --record run.rsm
$ cat run.rsm
rustsega-movie 1
rom-hash 0x…
region usa
pads 1
# frame: pad1  (buttons as letters UDLRABCSXYZM, '.' for none)
2 U
3 .
5 D
```

### Buttons

A buttons field is `.` for nothing held, or one letter per held button:

| Letter | Button | Letter | Button |
|---|---|---|---|
| `U` | Up | `S` | Start |
| `D` | Down | `X` | X (6-button pad) |
| `L` | Left | `Y` | Y (6-button pad) |
| `R` | Right | `Z` | Z (6-button pad) |
| `A` | A | `M` | Mode (6-button pad) |
| `B` | B | | |
| `C` | C | | |

A field may hold several letters, in any order and repeating: `UR`, `RU`, `UUR`
are the same input. `format` writes one field per pad in `UDLRABCSXYZM` order.

## Edge cases, and what they mean

The sample above does not settle these, so they are decided here and tested in
[`tests/movie.rs`](../crates/genesis/tests/movie.rs).

| Case | Decision |
|---|---|
| Trailing newline on the last line | `format` always writes one; `parse` does not require one. `\r\n` line endings are accepted, `\r` being trailing whitespace. |
| Comment lines | A line whose first non-blank character is `#` is ignored **anywhere**, including before the marker. `#` does not start a comment at the end of a line: a `#` in a buttons field is an unknown button, so a mistyped line is an error rather than a change of input. |
| Blank lines | Ignored anywhere, including indentation and tabs where there is no field. |
| Duplicate frames | **Refused** (`DuplicateFrame`). Two lines for frame 120 make its buttons a guess, and silently keeping one of them changes what the movie plays. |
| Out-of-order frames | **Accepted.** The frames become a map, so `20 .` before `10 A` means the same thing; `format` writes them ascending. Re-sorting a hand-edited file must not change its meaning. |
| Unknown letters | **Refused** (`UnknownButton`), naming the line and the letter. A typo is not "nothing pressed". Lower-case letters are accepted; `format` writes upper case. |
| More pads than the console has | **Refused** (`BadPadCount`). `pads` is 1 or 2: the I/O chip's third port is the expansion port and is never a pad (`genesis::io`'s module docs). `pads 0` is refused too — a movie records somebody. |
| Wrong number of pad columns on a frame line | **Refused** (`ColumnCount`), saying which line, how many the header declared and how many it saw. |
| Frame 0 omitted | **Allowed.** The pads are released from the start of the run until the first listed frame, and a movie with no frame lines at all is a run with nobody touching a pad. |
| A frame line that repeats the input before it | **Allowed and kept.** It means the same as leaving it out, and nothing that a file spells out is dropped on the way in. `Movie::record` is what never writes one. |
| Input held past the last listed frame | Holds for the rest of the run, however long it is. |
| `rom-hash` with the leading zeros dropped | Accepted: `0x1` is a hash. The `0x` prefix is required, because it is what says this is a hash and not a count. |
| Region name | Matched case-insensitively against `usa`, `japan`, `europe` — the same three names `headless --region` takes and its summary line prints (`Region::name`). |

## The ROM hash

`movie::rom_hash` is `state::rom_checksum` of `cart::normalize_rom`'s output:
FNV-1a 64 over the image as the machine holds it, the **same** 8-byte identity a
save state's header carries (`docs/save-state-format.md`, "The ROM checksum"). A
movie and a save state therefore identify a ROM identically, an SMD image is
identified deinterleaved, and the front end has to normalize a file before
hashing it for either format.

It is an identity check and not a security check, for the same reasons the save
state gives: it says "the same cartridge", and nothing weaker that says that is
available without carrying the ROM.

## Replaying

`Genesis::set_pad` per frame is all a replay is, so `Movie::set_pads` does that
loop and `headless` calls it before every `Genesis::run_frame`:

```rust
let movie = genesis::movie::parse(&text)?;   // or Movie::new(...) while recording
movie.check_rom(&rom)?;                      // WrongRom if it is for another game
let mut machine = Genesis::new(&rom, config)?;
for frame in 0..frames {
    movie.set_pads(&mut machine, frame);
    machine.run_frame();
}
```

`headless` also refuses a movie whose `region` is not the region the run resolved
to, because a PAL replay of an NTSC recording is a different run rather than a
broken file.

## Errors

`movie::parse` and `Movie::check_rom` answer with `MovieError`:

| Variant | Cause |
|---|---|
| `NotAMovie { line, text }` | The first meaningful line is not `rustsega-movie 1` |
| `UnsupportedVersion(n)` | The marker names format `n` and this build knows `1` |
| `MissingHeader(key)` | One of `rom-hash`, `region`, `pads` is absent — none of them is optional |
| `DuplicateHeader(key)` | A header line appears twice |
| `BadHeaderValue { line, key, text }` | A header line holds something other than its one value |
| `BadRomHash { line, text }` | `rom-hash` is not `0x` and 1 to 16 hex digits |
| `BadRegion { line, text }` | `region` is not `usa`, `japan` or `europe` |
| `BadPadCount { line, text }` | `pads` is not 1 or 2 |
| `BadFrame { line, text }` | A frame line does not start with a frame number |
| `DuplicateFrame(frame)` | Frame `frame` is listed twice |
| `UnknownButton { line, ch }` | A buttons field holds a letter that is not a button |
| `ColumnCount { line, expected, got }` | A frame line has the wrong number of pad columns |
| `WrongRom` | `rom-hash` is not the hash of the ROM the caller is loading |

Every line-numbered error names its line, because the point of a text format is
that a person fixes it by hand.

## What a movie cannot do yet

- **`X`, `Y`, `Z` and `Mode` reach the file but not the game.** A movie records
  all twelve buttons, but replay goes through `Genesis::set_pad`, which attaches a
  3-button pad, and a 3-button pad cannot report those four
  (plutiedev "Controllers", "Button layout"). Delivering them needs a 6-button
  pad on the console's API (`Io::set_buttons_6` exists; `Genesis` does not expose
  it), which is #364 rather than part of this format.
- **Recording from the browser.** `#129` keeps the front end out of scope; the
  format is the same text, so a front end that keeps one in memory can offer the
  same record/replay once input arrives there (#365).
- **Audio, saves and the memory watch.** A movie is input only. What the machine
  did with it is what `--hash` and `--dump-png` report, and SRAM is
  `Genesis::sram`.

## Versioning

`FORMAT_VERSION` is 1 and `parse` refuses anything else with
`UnsupportedVersion`. A movie is only its frame lines, so the version moves when
the text changes meaning — a new field, a different frame numbering, a button
letter that means something else — and not when the parser learns to accept
something a previous version also meant.
