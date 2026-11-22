//! The browser half of a movie session (M7-18, #365): the record/replay state
//! machine the frame loop consults, and the admission checks the shell makes
//! before a replay starts.
//!
//! The format itself is `genesis::movie`'s and is proven there - the round
//! trip, every parse edge case, and `replay_matches_record`. What is web's is
//! everything around it: which pads each frame's pads come from while a
//! recording or a replay is on, that a replay answers nothing to the page's
//! own input, that a recording's output is a `.rsm` a replay can take back,
//! that a movie for another cartridge or region is refused with words a
//! player can act on, and which region the running console resolves to.
//!
//! Everything here is pure or drives a real `Genesis` on the fake ROM the
//! genesis tests use, so it runs natively under `cargo test -p web`; the two
//! browser facts a download needs - the Blob and the anchor - are
//! `browser_movie.rs`.

use genesis::movie::Movie;
use genesis::{Buttons, Config, Genesis, Region};
use web::movie::{self, MovieSession, MovieState, ReplayError};

/// Where the fake ROM puts its program: past the 0x200 bytes a load needs, so
/// `genesis::parse_header` accepts the image.
const CODE: u32 = 0x1000;
/// The reset stack pointer the fake ROM asks for: the top of work RAM.
const STACK: u32 = 0x00FF_0000;
/// Port 1's data register (`IoData1`, plutiedev "I/O ports").
const IO_DATA_1: u32 = 0xA1_0003;
/// Where the pad program copies the `TH` high sample: the last mirror of the
/// 64 KiB of work RAM at `0xE00000` (plutiedev "Memory map").
const PAD_HIGH: u32 = 0xFF_0000;
/// Where it copies the `TH` low sample, the next byte of the same RAM.
const PAD_LOW: u32 = 0xFF_0001;

/// A cartridge image with the reset table for [`STACK`] / [`CODE`] and `program`
/// at [`CODE`] - the same fake ROM `crates/genesis/tests/movie.rs` drives,
/// because these tests borrow its machine.
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

/// `move.b (addr).L, (dest).L`: `13F9 src32 dst32`.
fn load_byte(addr: u32) -> Vec<u16> {
    let src = addr.to_be_bytes();
    vec![
        0x13F9,
        u16::from_be_bytes([src[0], src[1]]),
        u16::from_be_bytes([src[2], src[3]]),
    ]
}

/// `move.b #$value, (addr).L`: `13FC imm16 addr32`.
fn store_imm_byte(value: u16, addr: u32) -> Vec<u16> {
    let imm = value.to_be_bytes();
    let mut words = vec![0x13FC, u16::from_be_bytes([imm[0], imm[1]])];
    words.extend(store_addr(addr));
    words
}

/// The destination half of [`load_byte`].
fn store_addr(addr: u32) -> Vec<u16> {
    let dest = addr.to_be_bytes();
    vec![
        u16::from_be_bytes([dest[0], dest[1]]),
        u16::from_be_bytes([dest[2], dest[3]]),
    ]
}

/// `move.b (src).L, (dest).L`, whole.
fn copy_byte(src: u32, dest: u32) -> Vec<u16> {
    let mut words = load_byte(src);
    words.extend(store_addr(dest));
    words
}

/// The pad copier of the genesis movie tests: samples port 1 twice a pass,
/// both halves of the pad, and stores them side by side in work RAM forever.
///
/// ```text
/// loop: move.b ($A10003).L, ($FF0000).L   ; TH high: U D L R B C
///       move.b #$00,       ($A10003).L   ; TH low
///       move.b ($A10003).L, ($FF0001).L   ; TH low:  S A 0 0 D U
///       move.b #$40,       ($A10003).L   ; TH high again
///       bra    loop
/// ```
fn pad_copier() -> Vec<u8> {
    let mut program = copy_byte(IO_DATA_1, PAD_HIGH);
    program.extend(store_imm_byte(0x00, IO_DATA_1));
    program.extend(copy_byte(IO_DATA_1, PAD_LOW));
    program.extend(store_imm_byte(0x40, IO_DATA_1));
    // `BRA.B` back to the top: body plus branch word is 38 bytes, so the
    // displacement is -38 = 0xDA (M68000UM section 8 "BRA").
    program.push(0x60DA);
    fake_rom(&program)
}

/// The two bytes the pad copier has written: `(TH high, TH low)` of port 1.
fn pad_bytes(machine: &Genesis) -> (u8, u8) {
    let ram = &machine.bus().ram[..];
    let at = PAD_HIGH as usize & 0xFFFF;
    (ram[at], ram[at + 1])
}

/// An empty pads row: nobody touching either pad.
fn released() -> [Buttons; 2] {
    [Buttons::empty(), Buttons::empty()]
}

/// A movie of one pad holding `buttons` on `port` from frame 0, for `rom`.
fn holding(rom: &[u8], port: usize, buttons: Buttons) -> Movie {
    let mut movie = Movie::new(movie::rom_hash(rom), Region::Usa, 2);
    let mut row = [Buttons::empty(), Buttons::empty()];
    row[port] = buttons;
    movie.set(0, &row);
    movie
}

#[test]
fn a_recording_covers_both_pads_of_the_cartridge_it_names() {
    let rom = pad_copier();
    let mut session = MovieSession::default();
    assert_eq!(session.state(), MovieState::Idle);
    assert!(!session.drives_pads());

    session.start_recording(movie::rom_hash(&rom), Region::Europe);
    assert_eq!(session.state(), MovieState::Recording);
    // The pads stay the page's own while a recording is on.
    assert!(!session.drives_pads());

    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    let mut live = released();
    live[0] = Buttons::B;
    session.sync_pads(&mut machine, 0, &live);
    session.sync_pads(&mut machine, 1, &live);
    let mut later = released();
    later[1] = Buttons::START;
    session.sync_pads(&mut machine, 2, &later);

    // The recording is the page's two ports, on the cartridge and region it
    // was started with, and it holds what was held.
    let recorded = session.stop().expect("a recording was rolling");
    assert_eq!(recorded.pads(), 2);
    assert_eq!(recorded.region(), Region::Europe);
    assert_eq!(recorded.rom_hash(), movie::rom_hash(&rom));
    assert_eq!(recorded.buttons_at(1, 0), Buttons::B);
    assert_eq!(recorded.buttons_at(2, 1), Buttons::START);
    // `record` lists a change once: frame 0 (the first frame, B already
    // held), frame 1 held on, frame 2 is the only other line.
    let frames: Vec<u32> = recorded.changes().keys().copied().collect();
    assert_eq!(frames, vec![0, 2]);
}

#[test]
fn a_recording_leaves_the_pads_working_and_stopping_clears_the_session() {
    let rom = pad_copier();
    let mut session = MovieSession::default();
    session.start_recording(movie::rom_hash(&rom), Region::Usa);

    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    let mut live = released();
    live[0] = Buttons::B;
    session.sync_pads(&mut machine, 0, &live);
    session.sync_pads(&mut machine, 1, &live);
    machine.run_frame();

    // The live input reached the machine while the recorder was watching: a
    // recording is of a game the player is actually playing. A held button
    // grounds its pin, so B held reads 0x6F (bit 4 grounded, bit 6 the TH
    // line, the rest idling high).
    assert_eq!(pad_bytes(&machine).0, 0x6F, "TH high: B held");

    assert!(session.stop().is_some());
    assert_eq!(session.state(), MovieState::Idle);
    // Nothing is rolling now, and there is no second download hiding here.
    assert!(session.stop().is_none());
}

/// The replay decision of the ticket: while a movie runs it owns the pads, and
/// the page's live input - the keys a player presses mid-replay - is not read
/// at all until the session is stopped.
#[test]
fn a_replay_drives_the_pads_and_ignores_live_input() {
    let rom = pad_copier();
    let movie = holding(&rom, 0, Buttons::B);

    let mut session = MovieSession::default();
    session.start_replay(movie);
    assert_eq!(session.state(), MovieState::Replaying);
    assert!(session.drives_pads());

    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    // The player leans on A (and START on port 2) while the movie, which holds
    // only B, is on. The machine must see the movie's pads, not the player's.
    let mut live = released();
    live[0] = Buttons::A;
    live[1] = Buttons::START;
    session.sync_pads(&mut machine, 0, &live);
    session.sync_pads(&mut machine, 1, &live);
    machine.run_frame();

    let (high, low) = pad_bytes(&machine);
    // TH high: B held (bit 4 grounded), TH low: the pad idles at 0x33 - the
    // two multiplexer pins grounded, and NOT the 0x23 the live A would read,
    // because the page's input never reached the machine.
    assert_eq!(high, 0x6F, "the movie's B, TH high - not the live A");
    assert_eq!(low, 0x33, "TH low idles; the live A is not on the machine");

    session.stop();
    assert_eq!(session.state(), MovieState::Idle);
    assert!(!session.drives_pads());
}

/// What a recording hands back is the same text a replay takes in: the download
/// of one session is the file of another.
#[test]
fn a_recordings_output_is_a_movie_a_replay_accepts() {
    let rom = pad_copier();
    let mut session = MovieSession::default();
    session.start_recording(movie::rom_hash(&rom), Region::Usa);
    let mut machine = Genesis::new(&rom, Config::default()).expect("fake ROM loads");
    let mut live = released();
    live[0] = Buttons::UP;
    session.sync_pads(&mut machine, 0, &live);
    session.sync_pads(&mut machine, 1, &live);
    let text = session.stop().expect("a recording was rolling").format();

    let replayed = movie::prepare_replay(&text, movie::rom_hash(&rom), Region::Usa)
        .expect("what the recorder wrote, the replayer takes back");
    assert_eq!(replayed.buttons_at(1, 0), Buttons::UP);
}

/// Starting one mode replaces the other: there is one session, and a replay
/// begun over a recording discards it (stopping then finds nothing to save).
#[test]
fn starting_one_mode_replaces_the_other() {
    let rom = pad_copier();
    let mut session = MovieSession::default();
    session.start_recording(movie::rom_hash(&rom), Region::Usa);
    session.start_replay(holding(&rom, 0, Buttons::B));
    assert_eq!(session.state(), MovieState::Replaying);
    assert!(
        session.stop().is_none(),
        "the discarded recording is not saved"
    );

    session.start_recording(movie::rom_hash(&rom), Region::Usa);
    assert_eq!(session.state(), MovieState::Recording);
    assert!(!session.drives_pads(), "a recording is not a replay");
    assert!(session.stop().is_some());
}

/// A movie for another cartridge is refused before it touches the machine, in
/// words that say which side is wrong.
#[test]
fn a_movie_for_another_cartridge_is_refused() {
    let rom = pad_copier();
    let text = holding(&rom, 0, Buttons::B).format();
    let mut other = pad_copier();
    // One byte of the image moved: another cartridge.
    other[0x300] ^= 0xFF;
    let error = movie::prepare_replay(&text, movie::rom_hash(&other), Region::Usa)
        .expect_err("a movie is for its own cartridge");
    assert_eq!(error, ReplayError::WrongRom);
    assert!(
        error.to_string().contains("different cartridge"),
        "the refusal names the cartridge: {}",
        error
    );
}

/// A movie of the right cartridge for the wrong region is a different run
/// (a PAL replay of an NTSC recording is a different run rather than a broken
/// file, as headless puts it), so it is refused with both regions named.
#[test]
fn a_movie_for_another_region_is_refused_with_both_names() {
    let rom = pad_copier();
    let text = holding(&rom, 0, Buttons::B).format();
    let error = movie::prepare_replay(&text, movie::rom_hash(&rom), Region::Europe)
        .expect_err("the movie says usa, the run is europe");
    assert_eq!(
        error,
        ReplayError::RegionMismatch {
            movie: Region::Usa,
            run: Region::Europe,
        }
    );
    let said = error.to_string();
    assert!(
        said.contains("usa") && said.contains("europe"),
        "the refusal names both regions, and said {said:?}"
    );
}

/// A file that is not a movie at all names the line it broke on, because the
/// point of a text format is that a person fixes it by hand.
#[test]
fn a_broken_file_names_the_line() {
    let rom = pad_copier();
    let hash = movie::rom_hash(&rom);
    let text = format!(
        "rustsega-movie 1\nrom-hash 0x{hash:016x}\nregion usa\npads 2\n\
         # frame: pad1 pad2\n5 B Q\n"
    );
    let error = movie::prepare_replay(&text, hash, Region::Usa).expect_err("Q is not a button");
    let ReplayError::Parse(inner) = &error else {
        panic!("a bad letter is a parse error, not {error:?}");
    };
    let said = error.to_string();
    assert!(said.contains("line 6"), "the bad line is named: {said:?}");
    assert!(said.contains('Q'), "the bad letter is named: {said:?}");
    assert!(!inner.to_string().is_empty());

    let error = movie::prepare_replay("", hash, Region::Usa).expect_err("an empty paste");
    assert!(matches!(error, ReplayError::Parse(_)));
}

/// The region a replay is checked against is the one the console resolves the
/// cartridge to: the same header read `Genesis::new` does, under the page's
/// default configuration. A header that claims nothing reads as Usa, and an
/// image with no header at all has no region to offer.
#[test]
fn the_run_region_is_what_the_header_resolves_to() {
    let header_rom = |region: &str| {
        let mut rom = vec![0u8; 0x200];
        rom[0x100..0x200].fill(b' ');
        let at = 0x1F0;
        rom[at..at + region.len()].copy_from_slice(region.as_bytes());
        rom
    };
    assert_eq!(movie::run_region(&header_rom("UFE")), Some(Region::Usa));
    assert_eq!(movie::run_region(&header_rom(" E ")), Some(Region::Europe));
    assert_eq!(
        movie::run_region(&header_rom("   ")),
        Some(Region::Usa),
        "no claim is Usa, the same default Genesis::new runs"
    );
    assert_eq!(
        movie::run_region(&[0_u8; 16]),
        None,
        "no header, no region to check a movie against"
    );
}

/// A download is named for the cartridge's id, as a `.srm` is, so what lands
/// in the player's downloads folder says which game it belongs to.
#[test]
fn a_download_is_named_for_the_rom_id() {
    assert_eq!(movie::file_name("2b8fdb67"), "2b8fdb67.rsm");
    assert!(movie::file_name("abc").ends_with(movie::MOVIE_SUFFIX));
}

/// The words the page shows while a session is on. The chip and the button
/// labels read these, so a change here is a UI change and should be made
/// deliberately.
#[test]
fn the_session_states_have_ui_words() {
    assert_eq!(MovieState::Idle.label(), "");
    assert_eq!(MovieState::Recording.label(), "Recording input");
    assert_eq!(MovieState::Replaying.label(), "Replaying movie");
}

/// A Gens movie of `frames` (P1, P2, 6-button byte; active low), for #673.
fn gens_movie(frames: &[[u8; 3]]) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x40];
    bytes[..15].copy_from_slice(b"Gens Movie TEST");
    bytes[0x0F] = b'A';
    bytes[0x14] = b'3';
    bytes[0x15] = b'3';
    for frame in frames {
        bytes.extend_from_slice(frame);
    }
    bytes
}

/// A TASVideos movie becomes a movie of the running cartridge (#673): it names
/// no rustsega hash, so the hash it replays under is the console's.
#[test]
fn a_tas_movie_is_admitted_for_the_running_cartridge() {
    let hash = 0x0123_4567_89ab_cdef;
    let file = gens_movie(&[[0xFF; 3], [0x7F, 0xFF, 0xFF]]);
    let (replay, about) =
        movie::prepare_tas(&file, hash, Region::Usa).expect("a Gens movie is admitted");
    assert_eq!(replay.rom_hash(), hash);
    assert_eq!(replay.buttons_at(1, 0), genesis::Buttons::START);
    assert!(about.contains("Gens .gmv of 2 frames"), "{about}");
}

/// A Gens movie made at 50 fps is a PAL run, and an NTSC console refuses it
/// with both regions named, as it would a `.rsm`.
#[test]
fn a_pal_tas_movie_on_an_ntsc_run_is_refused() {
    let mut file = gens_movie(&[[0xFF; 3]]);
    file[0x16] = 0x80;
    let error = movie::prepare_tas(&file, 1, Region::Usa).expect_err("PAL on NTSC");
    assert!(error.to_string().contains("europe"), "{error}");
}

/// What the importer refuses, the toast says in its words.
#[test]
fn a_savestate_tas_movie_is_refused_in_the_importers_words() {
    let mut file = gens_movie(&[[0xFF; 3]]);
    file[0x16] = 0x40;
    let error = movie::prepare_tas(&file, 1, Region::Usa).expect_err("starts from a savestate");
    assert!(error.to_string().contains("savestate"), "{error}");
}
