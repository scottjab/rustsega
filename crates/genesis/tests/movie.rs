//! The input movie format of `docs/movie-format.md` (M6-09, #129).
//!
//! A movie is pad input over time: the frames on which the buttons change, in a
//! text file, replayable on the same cartridge. Everything here is the format and
//! the machine it drives, on a fake ROM - the file is written and parsed as text
//! by [`genesis::movie`] and only ever reaches the disk in `headless`.
//!
//! The format tests are the round trip plus one test per edge case the sample in
//! the ticket does not settle: what a comment line is, what a repeated frame
//! means, whether the frame lines have to be in order, what an unknown letter
//! does, how many pads the console has, and what happens before frame 0 is
//! listed. The machine test is the one the format exists for: recorded input,
//! replayed, byte for byte the same work RAM every frame.

use std::collections::BTreeMap;

use genesis::movie::{self, Movie, MovieError};
use genesis::{Buttons, Config, Genesis, Region};

/// Where the fake ROM puts its program: past the 0x200 bytes a load needs, so
/// [`genesis::parse_header`] accepts the image.
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROMs ask for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// Port 1's data register (`IoData1`, plutiedev "I/O ports").
const IO_DATA_1: u32 = 0xA1_0003;
/// Where the pad program copies the `TH` high sample: the last mirror of the
/// 64 KiB of work RAM at `0xE00000` (plutiedev "Memory map"), as `boot.rs` uses.
const PAD_HIGH: u32 = 0xFF_0000;
/// Where it copies the `TH` low sample, the next byte of the same RAM.
const PAD_LOW: u32 = 0xFF_0001;
/// The movie's pad count: the fake program watches port 1 only.
const PADS: usize = 1;
/// Frames of [`replay_matches_record`]'s run.
const RUN_FRAMES: usize = 100;

/// A cartridge image with the reset table for [`STACK`] / [`CODE`] and `program`
/// at [`CODE`].
fn fake_rom(program: &[u16]) -> Vec<u8> {
    let need = CODE as usize + 2 * program.len();
    let mut rom = vec![0_u8; need.max(0x200)];
    rom[0..4].copy_from_slice(&STACK.to_be_bytes());
    rom[4..8].copy_from_slice(&CODE.to_be_bytes());
    for (word, at) in program.iter().zip(0..) {
        let at = CODE as usize + 2 * at;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
    rom
}

/// `move.b (addr).L, (dest).L`. `MOVE.B (xxx).L, (yyy).L` is `13F9 src32 dst32`,
/// the encoding `boot.rs`'s `sample_forever` uses and `m68k::disassemble` checked.
fn load_byte(addr: u32) -> Vec<u16> {
    let src = addr.to_be_bytes();
    vec![
        0x13F9,
        u16::from_be_bytes([src[0], src[1]]),
        u16::from_be_bytes([src[2], src[3]]),
    ]
}

/// `move.b #$value, (addr).L`: `MOVE.B #imm, (xxx).L` is `13FC imm16 addr32`,
/// with the destination's mode-select in bits 11-9 and its mode in bits 8-6 as
/// `crates/m68k/src/ops/move_.rs` decodes it.
fn store_imm_byte(value: u16, addr: u32) -> Vec<u16> {
    let imm = value.to_be_bytes();
    let addr = addr.to_be_bytes();
    let mut words = vec![0x13FC, u16::from_be_bytes([imm[0], imm[1]])];
    words.extend(store_addr(addr));
    words
}

/// `move.b (src).L, (dest).L`: the destination half of [`load_byte`], the two
/// address words of an absolute long.
fn store_addr(addr: [u8; 4]) -> Vec<u16> {
    vec![
        u16::from_be_bytes([addr[0], addr[1]]),
        u16::from_be_bytes([addr[2], addr[3]]),
    ]
}

/// `move.b (src).L, (dest).L`, whole.
fn copy_byte(src: u32, dest: u32) -> Vec<u16> {
    let mut words = load_byte(src);
    words.extend(store_addr(dest.to_be_bytes()));
    words
}

/// The pad copier: a program that samples port 1 twice a pass, both halves of the
/// pad, and stores them side by side in work RAM forever.
///
/// A 3-button pad answers six buttons per `TH` level (segaretro "Control Pad
/// (Mega Drive)", "Technical information"), so one read is not enough to see the
/// eight a movie can hold: with `TH` high the byte is `?1CBRLDU` and with `TH` low
/// it is `?0SA00DU` (plutiedev "Controllers", "Reading the buttons"). The program
/// therefore reads with the power-on `TH` (high), drops `TH` by writing the data
/// register, reads again, and puts `TH` back for the next pass.
///
/// ```text
/// loop: move.b ($A10003).L, ($FF0000).L   ; TH high: U D L R B C
///       move.b #$00,       ($A10003).L   ; TH low
///       move.b ($A10003).L, ($FF0001).L   ; TH low:  U D A START
///       move.b #$40,       ($A10003).L   ; TH high again
///       bra    loop
/// ```
fn pad_copier() -> Vec<u8> {
    let mut program = copy_byte(IO_DATA_1, PAD_HIGH);
    program.extend(store_imm_byte(0x00, IO_DATA_1));
    program.extend(copy_byte(IO_DATA_1, PAD_LOW));
    program.extend(store_imm_byte(0x40, IO_DATA_1));
    // `BRA.B` back to the top of the loop. `BRA`'s displacement is added to the
    // address of the instruction *after* the branch (M68000UM section 8 "BRA":
    // "destination = (PC)+2 + displacement"), and the loop body plus the branch
    // word is 38 bytes, so the displacement is -38 = 0xDA.
    program.push(0x60DA);
    fake_rom(&program)
}

/// A second ROM, same shape, one byte of code different: the movie written for
/// the first one has to notice.
fn other_rom() -> Vec<u8> {
    let mut rom = pad_copier();
    // `move.b #$01, (IO_DATA_1).L` instead of `#$00` - a different image, a
    // different machine, a movie that is not for it.
    rom[CODE as usize + 12] ^= 0x01;
    rom
}

/// What the pad program has left in work RAM: both samples of the pad, the two
/// bytes of [`PAD_HIGH`] and [`PAD_LOW`], then the rest of the first page, which
/// nothing else writes and which therefore says "the same machine state" rather
/// than "the same pad".
fn pad_ram(machine: &Genesis) -> Vec<u8> {
    let ram = &machine.bus().ram[..];
    let at = PAD_HIGH as usize & 0xFFFF;
    ram[at..at + 0x40].to_vec()
}

/// The whole of a movie's text with `rom_hash` in its header and `frames` as its
/// change list.
fn movie_text(rom_hash: u64, frames: &[(u32, &str)]) -> String {
    let mut text = format!("rustsega-movie 1\nrom-hash 0x{rom_hash:016x}\nregion usa\npads 1\n");
    for (frame, buttons) in frames {
        text.push_str(&format!("{frame} {buttons}\n"));
    }
    text
}

/// The `Buttons` a [`pad_copier`] run can be seen to hold: the six of the
/// `TH` high byte and the two more of the `TH` low one, so a bit the movie drops
/// or shifts is a bit this test fails on.
fn held_buttons() -> Vec<Buttons> {
    [
        Buttons::UP,
        Buttons::DOWN,
        Buttons::LEFT,
        Buttons::RIGHT,
        Buttons::B,
        Buttons::C,
        Buttons::A,
        Buttons::START,
    ]
    .into_iter()
    .collect()
}

/// A deterministic pseudo-random button for frame `frame`: an 8-bit
/// xorshift/multiply hash of the frame number used to pick which of the eight
/// 3-button bits are held.
///
/// `Buttons`' own bits are private and their order is not this test's business, so
/// the choice is by index into [`held_buttons`].
fn pseudo_random(frame: usize) -> Buttons {
    let mut x = (frame as u32).wrapping_add(0x9E37_79B9);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    let buttons = held_buttons();
    let mut held = Buttons::empty();
    for (bit, button) in buttons.iter().enumerate() {
        if x & (1 << bit) != 0 {
            held |= *button;
        }
    }
    held
}

/// The 100 frames of input [`replay_matches_record`] records, as the pad buttons
/// of [`held_buttons`] - deliberately not all eight at once, since the pad
/// program can only show the eight a 3-button pad has.
fn pseudo_random_run() -> Vec<Buttons> {
    (0..RUN_FRAMES).map(pseudo_random).collect()
}

/// `Buttons` as the byte of a `TH` high pad read: every line high, each held
/// button pulling its own low (segaretro "Control Pad (Mega Drive)").
fn pins_th_high(buttons: Buttons) -> u8 {
    let mut pins = 0x7F;
    for (bit, button) in [
        (0, Buttons::UP),
        (1, Buttons::DOWN),
        (2, Buttons::LEFT),
        (3, Buttons::RIGHT),
        (4, Buttons::B),
        (5, Buttons::C),
    ] {
        if buttons.contains(button) {
            pins &= !(1 << bit);
        }
    }
    pins
}

/// `Buttons` as the byte of a `TH` low pad read, `?0SA00DU`: Up and Down again,
/// `A` on bit 4 and `Start` on bit 5, bits 3 and 2 wired to ground inside the pad
/// so they read 0 whatever the buttons do, and bit 6 the `TH` line itself, which
/// the console is holding low (plutiedev "Controllers", "Reading the buttons").
fn pins_th_low(buttons: Buttons) -> u8 {
    let mut pins = 0x3F;
    for (bit, button) in [
        (0, Buttons::UP),
        (1, Buttons::DOWN),
        (4, Buttons::A),
        (5, Buttons::START),
    ] {
        if buttons.contains(button) {
            pins &= !(1 << bit);
        }
    }
    pins & !((1 << 2) | (1 << 3))
}

/// Build a movie in memory, write it, parse it back, and require the two to be
/// equal - and require the text to be the text of `docs/movie-format.md`, so a
/// change to either is a deliberate one.
#[test]
fn movie_roundtrip_text() {
    let mut movie = Movie::new(0x0123_4567_89ab_cdef, Region::Usa, 2);
    movie.set(0, &[Buttons::empty(), Buttons::empty()]);
    movie.set(120, &[Buttons::START, Buttons::empty()]);
    movie.set(121, &[Buttons::empty(), Buttons::empty()]);
    movie.set(
        200,
        &[Buttons::UP | Buttons::RIGHT, Buttons::A | Buttons::X],
    );

    let text = movie.format();
    assert_eq!(
        text,
        "rustsega-movie 1\n\
         rom-hash 0x0123456789abcdef\n\
         region usa\n\
         pads 2\n\
         # frame: pad1 pad2  (buttons as letters UDLRABCSXYZM, '.' for none)\n\
         0 . .\n\
         120 S .\n\
         121 . .\n\
         200 UR AX\n"
    );

    let back = movie::parse(&text).expect("the text this build wrote parses back");
    assert_eq!(back, movie);
    assert_eq!(back.format(), text, "and writing it again is identical");
}

/// The movie identifies its cartridge by the same 64-bit value a save state
/// does, so one ROM check cannot be satisfied by a different hash in one of the
/// two formats (`docs/save-state-format.md`, "The ROM checksum").
#[test]
fn movie_and_save_state_hash_a_rom_the_same_way() {
    let rom = pad_copier();
    assert_eq!(
        movie::rom_hash(&rom),
        genesis::state::rom_checksum(&genesis::cart::normalize_rom(&rom))
    );
}

/// A movie replayed on the cartridge it was not written for is refused: its
/// `rom-hash` is not the hash of the loaded image.
#[test]
fn movie_rejects_wrong_rom() {
    let rom = pad_copier();
    let other = other_rom();
    let text = movie_text(movie::rom_hash(&rom), &[(0, ".")]);
    let movie = movie::parse(&text).expect("the movie parses");

    movie.check_rom(&rom).expect("its own ROM");
    assert_eq!(
        movie.check_rom(&other).expect_err("another cartridge"),
        MovieError::WrongRom
    );
    // And an image with no header at all is just another wrong ROM, not a panic.
    assert_eq!(
        movie.check_rom(&[0_u8; 4]).expect_err("not a ROM"),
        MovieError::WrongRom
    );
}

/// The reason the format exists: input recorded while playing is replayed exactly,
/// so a movie is a regression test.
///
/// A fake ROM copies port 1 into work RAM continuously, so work RAM is the
/// observable: every frame of the replay has to hold the pad the recording had,
/// and the whole watched page of RAM has to match frame for frame.
#[test]
fn replay_matches_record() {
    let rom = pad_copier();
    let inputs = pseudo_random_run();

    let mut recorded_ram = Vec::new();
    {
        let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
        let mut movie = Movie::new(movie::rom_hash(&rom), Region::Usa, PADS);
        for (frame, &buttons) in inputs.iter().enumerate() {
            movie.record(frame as u32, &[buttons]);
            machine.set_pad(0, buttons);
            machine.run_frame();
            recorded_ram.push(pad_ram(&machine));
        }
        // The run has to be a run: the program reached the pad, and the pad
        // changed. Otherwise this test would pass on a machine that did nothing.
        let movie = movie::parse(&movie.format()).expect("what was recorded is a movie");
        assert!(
            movie.changes().len() > 10,
            "a pseudo-random run changes the pads more often than that: {:?}",
            movie.changes().keys().collect::<Vec<_>>()
        );
    }
    assert!(
        recorded_ram.windows(2).any(|pair| pair[0] != pair[1]),
        "the recording shows the pad changing RAM at all"
    );

    // Replay: the same ROM, the movie as its text, and no `set_pad` of its own.
    let text = {
        let mut movie = Movie::new(movie::rom_hash(&rom), Region::Usa, PADS);
        for (frame, &buttons) in inputs.iter().enumerate() {
            movie.record(frame as u32, &[buttons]);
        }
        movie.format()
    };
    let movie = movie::parse(&text).expect("the recorded movie parses");
    movie.check_rom(&rom).expect("the movie is for this ROM");

    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    for (frame, expected) in recorded_ram.iter().enumerate() {
        movie.set_pads(&mut machine, frame as u32);
        machine.run_frame();
        assert_eq!(&pad_ram(&machine), expected, "frame {frame} of the replay");
    }
}

/// The sample of the ticket parses to what it says it means, including the
/// comment line between the header and the frames.
#[test]
fn ticket_sample_parses() {
    let text = "rustsega-movie 1\n\
                rom-hash 0x0123456789abcdef\n\
                region usa\n\
                pads 2\n\
                # frame: pad1 pad2  (buttons as letters UDLRABCSXYZM, '.' for none)\n\
                0 . .\n\
                120 S .\n\
                121 . .\n";
    let movie = movie::parse(text).expect("the ticket's own sample");
    assert_eq!(movie.rom_hash(), 0x0123_4567_89ab_cdef);
    assert_eq!(movie.region(), Region::Usa);
    assert_eq!(movie.pads(), 2);
    assert_eq!(
        movie.changes().keys().copied().collect::<Vec<_>>(),
        vec![0, 120, 121]
    );
    // Input holds until the next listed frame: frame 119 is still the frame 0 row
    // and frame 120 is the one that pressed START.
    assert_eq!(movie.buttons_at(119, 0), Buttons::empty());
    assert_eq!(movie.buttons_at(120, 0), Buttons::START);
    assert_eq!(movie.buttons_at(121, 0), Buttons::empty());
    // The second pad is never touched by anything but the frame 0 row.
    assert_eq!(movie.buttons_at(120, 1), Buttons::empty());
}

/// Every letter of `UDLRABCSXYZM` means its own button, and a field of several
/// letters means all of them.
#[test]
fn every_button_letter_round_trips() {
    let letters = "UDLRABCSXYZM";
    let expected = [
        Buttons::UP,
        Buttons::DOWN,
        Buttons::LEFT,
        Buttons::RIGHT,
        Buttons::A,
        Buttons::B,
        Buttons::C,
        Buttons::START,
        Buttons::X,
        Buttons::Y,
        Buttons::Z,
        Buttons::MODE,
    ];
    for (letter, want) in letters.chars().zip(expected) {
        let text = format!("rustsega-movie 1\nrom-hash 0x0\nregion usa\npads 1\n0 {letter}\n",);
        let movie =
            movie::parse(&text).unwrap_or_else(|e| panic!("{letter} must be a button: {e}"));
        assert_eq!(movie.buttons_at(0, 0), want, "{letter}");
    }

    let both = movie_text(0, &[(5, "UA")]);
    let movie = movie::parse(&both).expect("two letters on one frame");
    assert_eq!(movie.buttons_at(5, 0), Buttons::UP | Buttons::A);
    // Order and repeats are not meaning: `AU`, `AAU` and `UAU` are the same input.
    let repeated = movie_text(0, &[(5, "AAUU")]);
    assert_eq!(movie::parse(&repeated).expect("repeats"), movie);
    // And letters are case-insensitive on the way in, though `format` always
    // writes the upper-case ones.
    let lower = movie_text(0, &[(5, "ua")]);
    assert_eq!(movie::parse(&lower).expect("lower case"), movie);
}

/// What `docs/movie-format.md` promises about the text: comments, blank lines, a
/// missing final newline and out-of-order frames all parse to the same movie, and
/// `format` writes one canonical shape.
#[test]
fn text_awkwardness_that_keeps_the_meaning_is_let_through() {
    let want =
        movie::parse(&movie_text(7, &[(0, "."), (10, "A"), (20, ".")])).expect("the plain movie");

    let awkward = "  \n\
                   # a header comment, indented\n\
                   rustsega-movie 1\r\n\
                   rom-hash 0x0000000000000007\n\
                   \n\
                   pads 1\n\
                   region usa\n\
                   20 .\n\
                   # out of order, and with a tab and two spaces before the buttons\n\
                   10\tA \n\
                   0   .";
    assert_eq!(
        movie::parse(awkward).expect("the same movie, written badly"),
        want
    );
}

/// A trailing newline is written; a file without one is still a movie. Both are
/// the same file to `parse`, and `format` always ends a line with `\n`.
#[test]
fn trailing_newline_is_written_and_not_required() {
    let text = movie_text(3, &[(0, ".")]);
    assert!(text.ends_with('\n'), "{text}");
    let without = text.trim_end_matches('\n');
    assert!(
        !without.ends_with('\n'),
        "the point of the test is a file with no final newline"
    );
    assert_eq!(
        movie::parse(without).expect("no final newline"),
        movie::parse(&text).expect("final newline")
    );
}

/// A frame listed twice is not a movie: the second line's meaning is a guess.
#[test]
fn a_duplicate_frame_is_refused() {
    let text = movie_text(1, &[(0, "."), (10, "A"), (10, "B")]);
    assert_eq!(
        movie::parse(&text).expect_err("frame 10 twice"),
        MovieError::DuplicateFrame(10)
    );
}

/// A letter outside `UDLRABCSXYZM.` is refused rather than ignored: a typo in a
/// hand-edited movie must not quietly become "no button".
#[test]
fn an_unknown_button_letter_is_refused() {
    for letter in ["Q", "!", "1", "-", "ABZ!", "N", "E"] {
        let text = movie_text(1, &[(0, letter)]);
        let err = movie::parse(&text).expect_err("{letter} is not a button");
        assert!(
            matches!(err, MovieError::UnknownButton { .. }),
            "{letter}: {err:?}"
        );
    }
}

/// The console has two pad ports, so a movie for three is not a movie: port 3 is
/// the expansion port and is never a pad (`genesis::io`'s module docs).
#[test]
fn more_pads_than_the_console_has_are_refused() {
    for pads in ["0", "3", "two", "2 "] {
        let text =
            format!("rustsega-movie 1\nrom-hash 0x0000000000000001\nregion usa\npads {pads}\n");
        let parsed = movie::parse(&text);
        if pads == "2 " {
            // Trailing space on a header value is whitespace, not part of it.
            assert!(parsed.is_ok(), "{pads:?} is a pads count: {parsed:?}");
            continue;
        }
        assert!(
            matches!(parsed, Err(MovieError::BadPadCount { .. })),
            "{pads}: {parsed:?}"
        );
    }
}

/// A frame line with the wrong number of pad columns is refused, and the error
/// says which line and how many it saw.
#[test]
fn a_frame_line_must_have_exactly_pads_columns() {
    let too_many = "rustsega-movie 1\nrom-hash 0x1\nregion usa\npads 1\n0 . .\n";
    let err = movie::parse(too_many).expect_err("two columns for one pad");
    assert_eq!(
        err,
        MovieError::ColumnCount {
            line: 5,
            expected: 1,
            got: 2
        },
        "{err:?}"
    );

    let too_few = "rustsega-movie 1\nrom-hash 0x1\nregion usa\npads 2\n0 .\n";
    assert_eq!(
        movie::parse(too_few).expect_err("one column for two pads"),
        MovieError::ColumnCount {
            line: 5,
            expected: 2,
            got: 1
        }
    );
}

/// Frame 0 may be left out: the pads are released from the start of the run until
/// the first listed frame.
#[test]
fn frame_0_may_be_omitted() {
    let movie = movie::parse(&movie_text(1, &[(50, "A")])).expect("no frame 0 line");
    assert!(!movie.changes().contains_key(&0));
    assert_eq!(movie.buttons_at(0, 0), Buttons::empty());
    assert_eq!(movie.buttons_at(49, 0), Buttons::empty());
    assert_eq!(movie.buttons_at(50, 0), Buttons::A);
    // And a movie with no frame lines at all is a released pad forever.
    let bare = movie::parse(&movie_text(1, &[])).expect("no frames at all");
    assert_eq!(bare.buttons_at(1000, 0), Buttons::empty());
    assert_eq!(bare.changes().len(), 0);
}

/// Input holds until the next listed frame, including past the last one and
/// before frame 0 - which is what makes a movie a short file.
#[test]
fn input_holds_until_the_next_listed_frame() {
    let movie = movie::parse(&movie_text(1, &[(10, "UD"), (20, ".")])).expect("two changes");
    for frame in 10..20 {
        assert_eq!(
            movie.buttons_at(frame, 0),
            Buttons::UP | Buttons::DOWN,
            "{frame}"
        );
    }
    assert_eq!(movie.buttons_at(20, 0), Buttons::empty());
    assert_eq!(movie.buttons_at(10_000, 0), Buttons::empty());
}

/// `record` is how a caller builds a movie frame by frame, and only the frames on
/// which the input changes are listed.
#[test]
fn record_lists_only_the_frames_that_change() {
    let mut movie = Movie::new(0, Region::Usa, 2);
    movie.record(0, &[Buttons::empty(), Buttons::empty()]);
    movie.record(1, &[Buttons::empty(), Buttons::empty()]);
    movie.record(2, &[Buttons::A, Buttons::empty()]);
    movie.record(3, &[Buttons::A, Buttons::empty()]);
    movie.record(4, &[Buttons::A, Buttons::B]);
    assert_eq!(
        movie.changes().keys().copied().collect::<Vec<_>>(),
        vec![2, 4],
        "unchanged frames are not listed, not even frame 0 over the released pad a movie starts with"
    );
    assert_eq!(movie.buttons_at(3, 1), Buttons::empty());
    assert_eq!(movie.buttons_at(4, 1), Buttons::B);
}

/// A frame line that repeats the state before it is allowed: it means the same
/// thing, so it is kept as written rather than quietly dropped, and it survives a
/// round trip. `record` is what never writes one.
#[test]
fn a_redundant_frame_line_is_allowed_and_kept() {
    let text = movie_text(1, &[(0, "."), (10, "A"), (11, "A"), (20, ".")]);
    let movie = movie::parse(&text).expect("a redundant line is not an error");
    assert_eq!(movie.buttons_at(11, 0), Buttons::A);
    let written = movie.format();
    assert!(
        written.contains("\n11 A\n"),
        "a line the file spells is not quietly dropped: {written}"
    );
    assert_eq!(
        movie::parse(&written).expect("what was written still parses"),
        movie
    );
}

/// Anything else in the header position is refused rather than half-read: a
/// missing `rom-hash` would silently mean "any cartridge".
#[test]
fn a_header_must_be_complete_and_come_from_this_format() {
    let no_hash = "rustsega-movie 1\nregion usa\npads 1\n";
    assert_eq!(
        movie::parse(no_hash).expect_err("no rom-hash"),
        MovieError::MissingHeader("rom-hash")
    );
    let no_region = "rustsega-movie 1\nrom-hash 0x1\npads 1\n";
    assert_eq!(
        movie::parse(no_region).expect_err("no region"),
        MovieError::MissingHeader("region")
    );
    let no_pads = "rustsega-movie 1\nrom-hash 0x1\nregion usa\n";
    assert_eq!(
        movie::parse(no_pads).expect_err("no pads"),
        MovieError::MissingHeader("pads")
    );
    let no_marker = "rom-hash 0x1\nregion usa\npads 1\n";
    assert!(
        matches!(movie::parse(no_marker), Err(MovieError::NotAMovie { .. })),
        "{no_marker:?}"
    );
    let other = "rustsega-movie 2\nrom-hash 0x1\nregion usa\npads 1\n";
    assert_eq!(
        movie::parse(other).expect_err("a format this build does not read"),
        MovieError::UnsupportedVersion(2)
    );
    let twice = "rustsega-movie 1\nrom-hash 0x1\nrom-hash 0x2\nregion usa\npads 1\n";
    assert_eq!(
        movie::parse(twice).expect_err("two rom-hash lines"),
        MovieError::DuplicateHeader("rom-hash")
    );
    // A frame line before the header is complete is a bad header, not a frame.
    let early = "rustsega-movie 1\n0 .\n";
    assert_eq!(
        movie::parse(early).expect_err("frames before the pads count"),
        MovieError::MissingHeader("rom-hash")
    );
}

/// A value the header cannot hold is refused with the line that holds it, so a
/// hand editor knows where to look.
#[test]
fn bad_header_values_are_named_with_their_line() {
    let hash = "rustsega-movie 1\nrom-hash 1234\nregion usa\npads 1\n";
    assert!(matches!(
        movie::parse(hash).expect_err("no 0x prefix"),
        MovieError::BadRomHash { line: 2, .. }
    ));
    let empty_hash = "rustsega-movie 1\nrom-hash 0x\nregion usa\npads 1\n";
    assert!(matches!(
        movie::parse(empty_hash).expect_err("no digits at all"),
        MovieError::BadRomHash { line: 2, .. }
    ));
    let long_hash = "rustsega-movie 1\nrom-hash 0x1_2345_6789_abcd_ef01\nregion usa\npads 1\n";
    assert!(matches!(
        movie::parse(long_hash).expect_err("more than 64 bits"),
        MovieError::BadRomHash { line: 2, .. }
    ));
    let region = "rustsega-movie 1\nrom-hash 0x1\nregion europa\npads 1\n";
    assert!(matches!(
        movie::parse(region).expect_err("not a region name"),
        MovieError::BadRegion { line: 3, .. }
    ));
    let frame = "rustsega-movie 1\nrom-hash 0x1\nregion usa\npads 1\nframe .\n";
    assert!(matches!(
        movie::parse(frame).expect_err("not a frame number"),
        MovieError::BadFrame { line: 5, .. }
    ));
    let huge = format!(
        "rustsega-movie 1\nrom-hash 0x1\nregion usa\npads 1\n{} .\n",
        u64::from(u32::MAX) + 1
    );
    assert!(matches!(
        movie::parse(&huge).expect_err("no such frame"),
        MovieError::BadFrame { line: 5, .. }
    ));
}

/// `set_pads` drives the machine, which is all of a replay a caller has to
/// remember: no `set_pad`, no bookkeeping, just the frame number.
#[test]
fn set_pads_drives_both_ports() {
    let mut movie = Movie::new(0, Region::Usa, 2);
    movie.set(0, &[Buttons::UP, Buttons::DOWN]);
    let rom = pad_copier();
    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    movie.set_pads(&mut machine, 0);
    machine.run_frame();
    let ram = pad_ram(&machine);
    assert_eq!(
        ram[0],
        pins_th_high(Buttons::UP),
        "port 1 holds Up: {}",
        ram[0]
    );
}

/// The pad copier really is a copier: what it puts in RAM is what a 3-button pad
/// with those buttons held reads as, on both `TH` phases.
#[test]
fn the_pad_copier_reads_both_halves_of_the_pad() {
    let rom = pad_copier();
    for buttons in held_buttons() {
        let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
        machine.set_pad(0, buttons);
        machine.run_frame();
        let ram = pad_ram(&machine);
        assert_eq!(ram[0], pins_th_high(buttons), "{buttons:?} TH high");
        assert_eq!(ram[1], pins_th_low(buttons), "{buttons:?} TH low");
    }
}

/// A movie whose change list is a `BTreeMap` frame by frame: `changes` is the
/// whole of the frame data, and nothing else is.
#[test]
fn changes_are_keyed_by_frame() {
    let mut movie = Movie::new(0, Region::Europe, 1);
    movie.set(3, &[Buttons::Z]);
    movie.set(1, &[Buttons::Y]);
    let back = movie::parse(&movie.format()).expect("round trip");
    let expected: BTreeMap<u32, Vec<Buttons>> =
        [(1, vec![Buttons::Y]), (3, vec![Buttons::Z])].into();
    assert_eq!(*back.changes(), expected);
    assert_eq!(back.region(), Region::Europe);
}
