//! The input movie format of `docs/movie-format.md` (M6-09, #129).
//!
//! A movie is the pad input of one run of one cartridge: the frames on which the
//! buttons change, in a text file, small enough to read and to hand-edit and
//! exact enough to replay a section of a game as a regression test.
//!
//! ```text
//! rustsega-movie 1
//! rom-hash 0x0123456789abcdef
//! region usa
//! pads 2
//! # frame: pad1 pad2  (buttons as letters UDLRABCSXYZM, '.' for none)
//! 0 . .
//! 120 S .
//! 121 . .
//! ```
//!
//! This module does the text and nothing else: [`parse`] and
//! [`Movie::format`] never touch a file, because the core crates have no
//! filesystem (docs/ARCHITECTURE.md, "Workspace layout"), and it is
//! `crates/headless` that reads and writes `.rsm` files.
//!
//! Two rules carry the whole format:
//!
//! * **Only the frames on which the input changes are listed**, and the input
//!   holds until the next listed frame. A two-minute movie of a title screen is
//!   therefore a handful of lines.
//! * **The header names the cartridge** by [`rom_hash`], which is the same
//!   FNV-1a 64 a save state carries ([`crate::state::rom_checksum`],
//!   `docs/save-state-format.md`), so a movie and a save state cannot disagree
//!   about which ROM they belong to and [`Movie::check_rom`] refuses one built
//!   for another game instead of replaying into it.
//!
//! Frame numbers are 0-based: frame 0 is the first [`crate::Genesis::run_frame`]
//! after the ROM is loaded. Before the first listed frame every pad is released,
//! which is why a movie that begins with nothing held does not have to list
//! frame 0 at all.

use std::collections::BTreeMap;
use std::fmt;

use crate::{Buttons, Genesis, Region};

/// The word that starts a movie file, before the format version.
const MARKER: &str = "rustsega-movie";
/// The format version this build writes, and the only one it reads.
///
/// A movie carries no payload that could drift - the frame lines are the
/// format - so this only moves if the text itself changes meaning.
pub const FORMAT_VERSION: u32 = 1;
/// The most pads a movie can record: the console has two pad ports, and the
/// third port of the I/O chip is the expansion port and never a pad
/// ([`crate::io`]'s module docs).
pub const MAX_PADS: usize = 2;
/// The smallest number of pads a movie can claim: a movie records somebody.
const MIN_PADS: usize = 1;
/// A buttons field with nothing held down.
const NO_BUTTON: char = '.';
/// The twelve buttons of a pad, as the one letter each is written with, in the
/// order [`Movie::format`] writes them.
///
/// The eight a 3-button pad has come first, in the order of [`Buttons`]' own
/// constants, and `X`/`Y`/`Z`/`Mode` last, as the 6-button pad adds them
/// (plutiedev "Controllers", "Button layout").
const LETTERS: [(char, Buttons); 12] = [
    ('U', Buttons::UP),
    ('D', Buttons::DOWN),
    ('L', Buttons::LEFT),
    ('R', Buttons::RIGHT),
    ('A', Buttons::A),
    ('B', Buttons::B),
    ('C', Buttons::C),
    ('S', Buttons::START),
    ('X', Buttons::X),
    ('Y', Buttons::Y),
    ('Z', Buttons::Z),
    ('M', Buttons::MODE),
];

/// Why a movie could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovieError {
    /// The first line of the file is not `rustsega-movie <n>`, so it is not a
    /// movie at all.
    NotAMovie {
        /// The line that should have been the marker.
        line: usize,
        /// What it said instead.
        text: String,
    },
    /// The marker names format `n`, and this build writes and reads
    /// [`FORMAT_VERSION`] only.
    UnsupportedVersion(u32),
    /// A header line the format requires is missing: `rom-hash`, `region` or
    /// `pads`. A movie without a `rom-hash` would be a script for no cartridge in
    /// particular, so none of the three is optional.
    MissingHeader(&'static str),
    /// The header line `key` appears twice, and which one to believe is a guess.
    DuplicateHeader(&'static str),
    /// The header line `key` does not hold exactly one value: it is empty, or it
    /// has something after the value it names.
    BadHeaderValue {
        /// The line the bad header value is on, 1-based.
        line: usize,
        /// Which header key the line named.
        key: String,
        /// The whole line, so a reader can see what to fix.
        text: String,
    },
    /// `rom-hash` is not `0x` followed by 1 to 16 hex digits.
    BadRomHash {
        /// The line the bad value is on, 1-based.
        line: usize,
        /// The value that did not parse.
        text: String,
    },
    /// `region` is not one of `usa`, `japan`, `europe`.
    BadRegion {
        /// The line the bad value is on, 1-based.
        line: usize,
        /// The value that did not parse.
        text: String,
    },
    /// `pads` is not a count the console has, i.e. an integer from 1 to
    /// [`MAX_PADS`].
    BadPadCount {
        /// The line the bad value is on, 1-based.
        line: usize,
        /// The value that did not parse.
        text: String,
    },
    /// A frame line does not start with a frame number.
    BadFrame {
        /// The line the bad frame number is on, 1-based.
        line: usize,
        /// The value that did not parse.
        text: String,
    },
    /// Frame `frame` is listed by two lines, so what its buttons are is a guess.
    ///
    /// Lines out of order are fine - see [`parse`] - because re-sorting a frame
    /// line is what a `BTreeMap` does. A frame listed twice is not: dropping one
    /// of them silently changes what the movie plays.
    DuplicateFrame(u32),
    /// A buttons field holds `ch`, which is neither one of `UDLRABCSXYZM` nor the
    /// `.` that stands for no button.
    UnknownButton {
        /// The line the bad letter is on, 1-based.
        line: usize,
        /// The letter.
        ch: char,
    },
    /// A frame line holds `got` pad columns where the header's `pads` says
    /// `expected`.
    ColumnCount {
        /// The line with the wrong number of columns, 1-based.
        line: usize,
        /// The pad count the header declared.
        expected: usize,
        /// The number of columns the line actually held.
        got: usize,
    },
    /// The movie is for a different cartridge: its `rom-hash` is not
    /// [`rom_hash`] of the image the machine has loaded.
    WrongRom,
}

impl fmt::Display for MovieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAMovie { line, text } => {
                write!(
                    f,
                    "not a movie: line {line} is {text:?}, not \"{MARKER} 1\""
                )
            }
            Self::UnsupportedVersion(got) => {
                write!(f, "movie format {got}, this build knows {FORMAT_VERSION}")
            }
            Self::MissingHeader(key) => write!(f, "not a movie: no \"{key}\" header line"),
            Self::DuplicateHeader(key) => write!(f, "not a movie: two \"{key}\" header lines"),
            Self::BadHeaderValue { line, key, text } => write!(
                f,
                "not a movie: line {line} {text:?} is not one \"{key}\" value"
            ),
            Self::BadRomHash { line, text } => write!(
                f,
                "movie line {line}: rom-hash {text:?} is not 0x followed by 1 to 16 hex digits"
            ),
            Self::BadRegion { line, text } => write!(
                f,
                "movie line {line}: region {text:?} is not one of usa, japan, europe"
            ),
            Self::BadPadCount { line, text } => write!(
                f,
                "movie line {line}: pads {text:?} is not a count of pads this console has ({} to {MAX_PADS})",
                MIN_PADS
            ),
            Self::BadFrame { line, text } => {
                write!(f, "movie line {line}: {text:?} is not a frame number")
            }
            Self::DuplicateFrame(frame) => {
                write!(f, "movie: frame {frame} is listed twice")
            }
            Self::UnknownButton { line, ch } => write!(
                f,
                "movie line {line}: {ch:?} is not a button; use UDLRABCSXYZM, or '.' for no button"
            ),
            Self::ColumnCount {
                line,
                expected,
                got,
            } => write!(
                f,
                "movie line {line}: {got} pad columns where the header says {expected}"
            ),
            Self::WrongRom => write!(f, "movie is for a different cartridge"),
        }
    }
}

impl std::error::Error for MovieError {}

/// The 64-bit identity of the cartridge `rom` runs on, as a movie's `rom-hash`.
///
/// [`crate::state::rom_checksum`] of [`crate::normalize_rom`]'s output: the same
/// value a save state's header carries, so a movie says "this cartridge" the same
/// way and an SMD image is identified deinterleaved
/// (`docs/save-state-format.md`, "The ROM checksum"). A front end with a file on
/// disk has to normalise it before hashing, exactly as it does for a state.
pub fn rom_hash(rom: &[u8]) -> u64 {
    crate::state::rom_checksum(&crate::cart::normalize_rom(rom))
}

/// One recorded run of one cartridge: the frames on which the pads change.
///
/// The frame data is a map, so a movie's meaning does not depend on the order its
/// lines were written in, and reading a frame is [`Movie::buttons_at`] - the
/// buttons of the latest change at or before that frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    /// The cartridge this is a movie of: [`rom_hash`] of its image.
    rom_hash: u64,
    /// The region the run was made on, recorded because a PAL and an NTSC run of
    /// the same ROM are different runs at a different frame rate.
    region: Region,
    /// How many pads the movie covers: 1 or 2, never more (see [`MAX_PADS`]).
    pads: usize,
    /// The frames on which the input changes, in order, with the buttons each pad
    /// holds from that frame on.
    changes: BTreeMap<u32, Vec<Buttons>>,
}

impl Movie {
    /// An empty movie of `pads` pads on the cartridge `rom_hash` names, with
    /// nothing held from frame 0.
    ///
    /// # Panics
    ///
    /// If `pads` is not between 1 and [`MAX_PADS`]: a movie for no
    /// pads, or for a third pad, has nothing to replay.
    pub fn new(rom_hash: u64, region: Region, pads: usize) -> Self {
        assert!(
            (MIN_PADS..=MAX_PADS).contains(&pads),
            "a movie records {MIN_PADS} to {MAX_PADS} pads, not {pads}"
        );
        Self {
            rom_hash,
            region,
            pads,
            changes: BTreeMap::new(),
        }
    }

    /// The cartridge this movie is a movie of.
    pub fn rom_hash(&self) -> u64 {
        self.rom_hash
    }

    /// The region the run was made on.
    pub fn region(&self) -> Region {
        self.region
    }

    /// How many pads the movie covers.
    pub fn pads(&self) -> usize {
        self.pads
    }

    /// The change list: frame number to the buttons each pad holds from that
    /// frame. Frames with no entry hold what the entry before them left.
    pub fn changes(&self) -> &BTreeMap<u32, Vec<Buttons>> {
        &self.changes
    }

    /// Say that from frame `frame` the pads hold `row`.
    ///
    /// This is the unfiltered listing: unlike [`Movie::record`] it lists `frame`
    /// whether or not anything changed, which is what a hand-built movie wants.
    ///
    /// # Panics
    ///
    /// If `row` is not [`Movie::pads`] long.
    pub fn set(&mut self, frame: u32, row: &[Buttons]) {
        assert_eq!(
            row.len(),
            self.pads,
            "a movie of {} pad(s) takes {} button(s) for frame {frame}",
            self.pads,
            row.len()
        );
        self.changes.insert(frame, row.to_vec());
    }

    /// Note what the pads hold on frame `frame` while recording, listing the frame
    /// only if it differs from what was held before it.
    ///
    /// This is what keeps the format's one rule true without the caller having to
    /// remember the previous frame: a run of a hundred frames with the pad
    /// released records nothing at all.
    pub fn record(&mut self, frame: u32, row: &[Buttons]) {
        if row != self.row_at(frame) {
            self.set(frame, row);
        }
    }

    /// What every pad holds on `frame`: the row of the latest listed change at or
    /// before it, or nothing held at all if there is none.
    pub fn row_at(&self, frame: u32) -> Vec<Buttons> {
        self.changes
            .range(..=frame)
            .next_back()
            .map(|(_, row)| row.clone())
            .unwrap_or_else(|| vec![Buttons::empty(); self.pads])
    }

    /// What pad `pad` holds on `frame`.
    ///
    /// # Panics
    ///
    /// If `pad` is not one of this movie's pads.
    pub fn buttons_at(&self, frame: u32, pad: usize) -> Buttons {
        assert!(
            pad < self.pads,
            "a movie of {} pad(s) has no pad {pad}",
            self.pads
        );
        self.row_at(frame)[pad]
    }

    /// Put the pads of frame `frame` onto `machine` - the whole of what a replay
    /// has to do, once per frame before [`Genesis::run_frame`].
    ///
    /// The pads go on through [`Genesis::set_pad`], which attaches a 3-button
    /// pad, so the `X`, `Y`, `Z` and `Mode` letters a movie can hold are recorded
    /// and replayed but are not visible to the game: delivering them needs a
    /// 6-button pad on the API, which is not this format's business (see
    /// `docs/movie-format.md`, "What a movie cannot do yet").
    pub fn set_pads(&self, machine: &mut Genesis, frame: u32) {
        for pad in 0..self.pads {
            machine.set_pad(pad, self.buttons_at(frame, pad));
        }
    }

    /// Refuse this movie unless it was written for `rom`: the movie's
    /// [`Movie::rom_hash`] has to be [`rom_hash`] of the image.
    ///
    /// `rom` is the image as it was read off disk - the normalization is this
    /// function's, so a caller cannot get it wrong in the way that matters.
    pub fn check_rom(&self, rom: &[u8]) -> Result<(), MovieError> {
        if self.rom_hash == rom_hash(rom) {
            Ok(())
        } else {
            Err(MovieError::WrongRom)
        }
    }

    /// The whole movie as the text of `docs/movie-format.md`: the header, the
    /// comment line that names the columns, and the change list in frame order,
    /// every line ended by `\n`.
    pub fn format(&self) -> String {
        let mut text = format!(
            "{MARKER} {FORMAT_VERSION}\nrom-hash 0x{:016x}\nregion {}\npads {}\n",
            self.rom_hash,
            self.region.name(),
            self.pads
        );
        text.push_str(&self.columns_comment());
        for (frame, row) in &self.changes {
            let fields: Vec<String> = row.iter().map(|buttons| field(*buttons)).collect();
            text.push_str(&format!("{frame} {}\n", fields.join(" ")));
        }
        text
    }

    /// The `# frame: pad1 pad2 ...` line above the change list, which is what
    /// tells someone with a text editor what the columns mean.
    fn columns_comment(&self) -> String {
        let columns: Vec<String> = (1..=self.pads).map(|pad| format!("pad{pad}")).collect();
        format!(
            "# frame: {}  (buttons as letters {}, '{NO_BUTTON}' for none)\n",
            columns.join(" "),
            LETTERS
                .iter()
                .map(|(letter, _)| letter.to_string())
                .collect::<String>()
        )
    }
}

/// The movie `text` holds: see the module docs and `docs/movie-format.md`.
///
/// What the reader accepts, and what it refuses:
///
/// * Blank lines, and lines whose first non-blank character is `#`, are ignored
///   wherever they appear, including before the `rustsega-movie` line. `#` does
///   not start a comment at the end of a line: a `#` in a buttons field is an
///   unknown letter, so that a mistyped line is an error rather than a silent
///   change of input.
/// * A line ends at `\n`, and a trailing `\r`, tabs and runs of spaces are all
///   field separators. The last line of the file does not have to end with a
///   newline.
/// * The header is the marker and then `rom-hash`, `region` and `pads`, in any
///   order, each exactly once. Every one of them is required: a movie with no
///   `rom-hash` would be a script for no cartridge in particular.
/// * Frame lines may be in any order - the frames become a `BTreeMap` - but a
///   frame may be listed only once ([`MovieError::DuplicateFrame`]).
/// * A frame line holds `pads` buttons fields, each a set of the letters
///   `UDLRABCSXYZM` in any order and without repeats mattering, or `.` for none.
///   Letters are case-insensitive; anything else is
///   [`MovieError::UnknownButton`] rather than nothing pressed.
/// * A line that repeats the input before it is allowed: it is redundant, not
///   wrong, and it is kept exactly as written - nothing that a file says is
///   dropped on the way in. [`Movie::record`] is what never produces one.
pub fn parse(text: &str) -> Result<Movie, MovieError> {
    let mut marker = false;
    let mut rom_hash = None;
    let mut region = None;
    let mut pads = None;
    let mut changes: BTreeMap<u32, Vec<Buttons>> = BTreeMap::new();

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let line_text = raw.trim();
        if line_text.is_empty() || line_text.starts_with('#') {
            continue;
        }
        if !marker {
            read_marker(line_text, line)?;
            marker = true;
            continue;
        }
        let mut fields = line_text.split_whitespace();
        let head = fields
            .next()
            .expect("a trimmed, non-empty line has a field");
        if is_header_key(head) {
            let value = header_value(head, fields, line, line_text)?;
            match head {
                "rom-hash" => {
                    if rom_hash.is_some() {
                        return Err(MovieError::DuplicateHeader("rom-hash"));
                    }
                    rom_hash = Some(read_rom_hash(value, line)?);
                }
                "region" => {
                    if region.is_some() {
                        return Err(MovieError::DuplicateHeader("region"));
                    }
                    region = Some(read_region(value, line)?);
                }
                _ => {
                    if pads.is_some() {
                        return Err(MovieError::DuplicateHeader("pads"));
                    }
                    pads = Some(read_pads(value, line)?);
                }
            }
            continue;
        }

        // A frame line. The header has to be there first: the pad count is what
        // says how many columns this line should have, and the ROM is what says
        // what the movie is for.
        let pads = header_complete(rom_hash, region, pads)?;
        read_frame_line(head, fields, line, pads, &mut changes)?;
    }

    if !marker {
        return Err(MovieError::NotAMovie {
            line: 1,
            text: String::new(),
        });
    }
    header_complete(rom_hash, region, pads)?;
    Ok(Movie {
        rom_hash: rom_hash.expect("checked by header_complete"),
        region: region.expect("checked by header_complete"),
        pads: pads.expect("checked by header_complete"),
        changes,
    })
}

/// Add one frame line to `changes`: `head` is its frame number, `fields` the
/// buttons of each of `pads` pads, and `line` the line number, for the errors.
fn read_frame_line<'a>(
    head: &str,
    fields: impl Iterator<Item = &'a str>,
    line: usize,
    pads: usize,
    changes: &mut BTreeMap<u32, Vec<Buttons>>,
) -> Result<(), MovieError> {
    let frame = head.parse::<u32>().map_err(|_| MovieError::BadFrame {
        line,
        text: head.to_string(),
    })?;
    let columns: Vec<&str> = fields.collect();
    if columns.len() != pads {
        return Err(MovieError::ColumnCount {
            line,
            expected: pads,
            got: columns.len(),
        });
    }
    let row = columns
        .iter()
        .map(|field| read_buttons(field, line))
        .collect::<Result<Vec<_>, _>>()?;
    if changes.insert(frame, row).is_some() {
        return Err(MovieError::DuplicateFrame(frame));
    }
    Ok(())
}

/// The marker line: `rustsega-movie 1` and nothing else on it.
fn read_marker(line_text: &str, line: usize) -> Result<(), MovieError> {
    let not_a_movie = || MovieError::NotAMovie {
        line,
        text: line_text.to_string(),
    };
    let mut fields = line_text.split_whitespace();
    let (Some(word), Some(version)) = (fields.next(), fields.next()) else {
        return Err(not_a_movie());
    };
    if word != MARKER || fields.next().is_some() {
        return Err(not_a_movie());
    }
    let version = version.parse::<u32>().map_err(|_| not_a_movie())?;
    if version != FORMAT_VERSION {
        return Err(MovieError::UnsupportedVersion(version));
    }
    Ok(())
}

/// The pad count of a header that has all three of its lines, checked in the
/// order `rom-hash`, `region`, `pads` so the error a caller gets is the first
/// thing wrong with the header.
fn header_complete(
    rom_hash: Option<u64>,
    region: Option<Region>,
    pads: Option<usize>,
) -> Result<usize, MovieError> {
    if rom_hash.is_none() {
        return Err(MovieError::MissingHeader("rom-hash"));
    }
    if region.is_none() {
        return Err(MovieError::MissingHeader("region"));
    }
    match pads {
        Some(pads) => Ok(pads),
        None => Err(MovieError::MissingHeader("pads")),
    }
}

/// Is `field` one of the three header keys, as opposed to a frame line?
fn is_header_key(field: &str) -> bool {
    matches!(field, "rom-hash" | "region" | "pads")
}

/// The single value of the header line `key`, named `line_text` on `line`.
fn header_value<'a>(
    key: &str,
    fields: impl Iterator<Item = &'a str>,
    line: usize,
    line_text: &'a str,
) -> Result<&'a str, MovieError> {
    let bad = || MovieError::BadHeaderValue {
        line,
        key: key.to_string(),
        text: line_text.to_string(),
    };
    let rest: Vec<&'a str> = fields.collect();
    if rest.len() != 1 {
        return Err(bad());
    }
    Ok(rest[0])
}

/// `rom-hash`: `0x` and 1 to 16 hex digits.
///
/// Leading zeros may be left out - a hand-written movie for a ROM whose hash
/// begins with zeros should not have to spell them - but the prefix may not,
/// because the `0x` is what tells a reader this is a hash and not a count.
fn read_rom_hash(text: &str, line: usize) -> Result<u64, MovieError> {
    let bad = || MovieError::BadRomHash {
        line,
        text: text.to_string(),
    };
    let digits = text.strip_prefix("0x").ok_or_else(bad)?;
    if digits.is_empty() || digits.len() > 16 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    u64::from_str_radix(digits, 16).map_err(|_| bad())
}

/// `region`: one of the three names `headless` prints for a region.
fn read_region(text: &str, line: usize) -> Result<Region, MovieError> {
    let bad = || MovieError::BadRegion {
        line,
        text: text.to_string(),
    };
    match text.to_ascii_lowercase().as_str() {
        "japan" => Ok(Region::Japan),
        "usa" => Ok(Region::Usa),
        "europe" => Ok(Region::Europe),
        _ => Err(bad()),
    }
}

/// `pads`: a count of pads this console has, so 1 or 2.
fn read_pads(text: &str, line: usize) -> Result<usize, MovieError> {
    let bad = || MovieError::BadPadCount {
        line,
        text: text.to_string(),
    };
    let pads = text.parse::<usize>().map_err(|_| bad())?;
    if (MIN_PADS..=MAX_PADS).contains(&pads) {
        Ok(pads)
    } else {
        Err(bad())
    }
}

/// A buttons field: `.` for none, or a set of the letters of [`LETTERS`], in any
/// order and case and repeating as much as it likes.
fn read_buttons(field: &str, line: usize) -> Result<Buttons, MovieError> {
    if field == NO_BUTTON.to_string() {
        return Ok(Buttons::empty());
    }
    let mut held = Buttons::empty();
    for ch in field.chars() {
        let wanted = ch.to_ascii_uppercase();
        let Some((_, button)) = LETTERS.iter().find(|(letter, _)| *letter == wanted) else {
            return Err(MovieError::UnknownButton { line, ch });
        };
        held |= *button;
    }
    Ok(held)
}

/// The buttons field for one pad: the letters of every held button, in
/// [`LETTERS`] order, or `.` for none.
fn field(buttons: Buttons) -> String {
    let letters: String = LETTERS
        .iter()
        .filter(|(_, button)| buttons.contains(*button))
        .map(|(letter, _)| *letter)
        .collect();
    if letters.is_empty() {
        NO_BUTTON.to_string()
    } else {
        letters
    }
}
