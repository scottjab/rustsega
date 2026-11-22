//! Play TASVideos movies: BizHawk `.bk2` and Gens `.gmv` files, and the `.zip`
//! TASVideos serves each one in, turned into the emulator's own
//! [`genesis::movie::Movie`] (#673).
//!
//! Those are the two formats TASVideos accepts for the Genesis
//! (tasvideos.org/Platforms: BizHawk preferred, Gens). [`import`] takes whatever
//! file the user picked, works out which it is from its bytes rather than its
//! name, and returns a movie for the ROM that is loaded. Neither format carries
//! the FNV hash a `.rsm` names its cartridge by, so the movie is bound to the
//! ROM it is played on; the ROM the movie was made on is reported
//! ([`Imported::game_name`], [`Imported::rom_digest`]) for the front end to
//! show, not enforced.
//!
//! A TAS is a frame-exact script for the emulator it was made on. It plays
//! correctly for exactly as long as this emulator's timing matches that one's,
//! and where it stops matching it desyncs: that is a fact about the emulator,
//! not about the import.
//!
//! What is refused rather than played wrong: movies that start from a
//! savestate or from saved cartridge RAM, three-player Gens movies, movies for
//! another platform, and a Reset or power cycle in the middle of the movie,
//! which [`genesis::movie::Movie`] has no way to say yet.

pub mod bk2;
pub mod gmv;
pub mod zip;

use std::fmt;

use genesis::movie::{self, Movie};
use genesis::{Buttons, Region};

/// How deep a zip may nest inside a zip: TASVideos' wrapper around a `.bk2`,
/// which is itself a zip, is two.
const MAX_ZIP_DEPTH: usize = 2;

/// Why a file could not be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasError(String);

impl TasError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TasError {}

/// Which format a movie was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// BizHawk.
    Bk2,
    /// Gens.
    Gmv,
}

impl Format {
    /// The name a user knows the format by.
    pub fn name(self) -> &'static str {
        match self {
            Format::Bk2 => "BizHawk .bk2",
            Format::Gmv => "Gens .gmv",
        }
    }
}

/// What a format's parser hands back: the input, and what the file says about
/// the run it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// Both pads' buttons, frame by frame, from frame 0.
    pub frames: Vec<[Buttons; 2]>,
    /// Was each pad a 6-button pad.
    pub six_button: [bool; 2],
    /// Did the file say the console was PAL (Gens says 50 or 60 fps).
    pub pal: Option<bool>,
    /// The region the file names, if it names one.
    pub region: Option<Region>,
    /// The game the movie was made for, as the file names it.
    pub game_name: Option<String>,
    /// The ROM digest the file carries (BizHawk's `SHA1` header key).
    pub rom_digest: Option<String>,
    /// How many times the author re-recorded.
    pub rerecords: Option<u64>,
}

/// A movie ready to play, and what its file said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The input, for the ROM [`import`] was given.
    pub movie: Movie,
    /// The format it came from.
    pub format: Format,
    /// Its length in frames.
    pub frames: u32,
    /// The game it was made for, as the file names it.
    pub game_name: Option<String>,
    /// The digest of the ROM it was made on, if the file carries one.
    pub rom_digest: Option<String>,
    /// How many times its author re-recorded.
    pub rerecords: Option<u64>,
}

/// Is `bytes` something [`import`] might read: a Gens movie or a zip.
pub fn looks_like_tas(bytes: &[u8]) -> bool {
    gmv::is_gmv(bytes) || zip::is_zip(bytes)
}

/// The movie in `bytes` - a `.bk2`, a `.gmv`, or a `.zip` holding one - as a
/// movie of the cartridge whose [`movie::rom_hash`] is `rom_hash`: the ROM it
/// is about to be played on.
///
/// `region` is the region the run would use without the movie (the
/// cartridge's own, after the front end's preference): it is what an
/// autodetecting BizHawk movie runs as, and it picks between the two NTSC
/// regions for a Gens movie, which only says 50 or 60 fps.
pub fn import(bytes: &[u8], rom_hash: u64, region: Region) -> Result<Imported, TasError> {
    let (format, inputs) = read(bytes, 0)?;
    let region = match (inputs.region, inputs.pal) {
        (Some(named), _) => named,
        (None, Some(true)) => Region::Europe,
        (None, Some(false)) if region == Region::Europe => Region::Usa,
        _ => region,
    };
    let mut movie = Movie::new(rom_hash, region, movie::MAX_PADS);
    for (pad, six) in inputs.six_button.iter().enumerate() {
        if *six {
            movie.set_six_button(pad);
        }
    }
    for (frame, row) in inputs.frames.iter().enumerate() {
        let frame = u32::try_from(frame).map_err(|_| TasError::new("movie is too long"))?;
        movie.record(frame, row);
    }
    Ok(Imported {
        movie,
        format,
        frames: u32::try_from(inputs.frames.len()).unwrap_or(u32::MAX),
        game_name: inputs.game_name,
        rom_digest: inputs.rom_digest,
        rerecords: inputs.rerecords,
    })
}

/// The format and input of `bytes`, `depth` zips deep.
fn read(bytes: &[u8], depth: usize) -> Result<(Format, Inputs), TasError> {
    if gmv::is_gmv(bytes) {
        return Ok((Format::Gmv, gmv::parse(bytes)?));
    }
    if !zip::is_zip(bytes) {
        return Err(TasError::new(
            "not a movie this player knows: expected a BizHawk .bk2, a Gens .gmv, or a .zip holding one",
        ));
    }
    let entries = zip::entries(bytes).map_err(TasError::new)?;
    if bk2::is_bk2(&entries) {
        return Ok((Format::Bk2, bk2::parse(&entries)?));
    }
    if depth >= MAX_ZIP_DEPTH {
        return Err(TasError::new("the zip holds no BizHawk or Gens movie"));
    }
    let movies: Vec<&zip::Entry> = entries
        .iter()
        .filter(|e| gmv::is_gmv(&e.data) || zip::is_zip(&e.data))
        .collect();
    match movies.as_slice() {
        [one] => read(&one.data, depth + 1),
        [] => Err(TasError::new("the zip holds no BizHawk or Gens movie")),
        many => Err(TasError::new(format!(
            "the zip holds {} movies ({}); unzip it and pick one",
            many.len(),
            many.iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROM: &[u8] = &[0u8; 0x200];

    fn hash() -> u64 {
        movie::rom_hash(ROM)
    }
    const KEY3: &str = "LogKey:#Power|Reset|#P1 Up|P1 Down|P1 Left|P1 Right|P1 A|P1 B|P1 C|P1 Start|#P2 Up|P2 Down|P2 Left|P2 Right|P2 A|P2 B|P2 C|P2 Start|";

    fn bk2(sync: &str, rows: &[&str]) -> Vec<u8> {
        let log = format!("[Input]\n{KEY3}\n{}\n[/Input]\n", rows.join("\n"));
        zip::build(&[
            (
                "Header.txt",
                b"Platform GEN\nCore Genplus-gx\nGameName Test\nrerecordCount 5\n",
                true,
            ),
            ("SyncSettings.json", sync.as_bytes(), true),
            ("Input Log.txt", log.as_bytes(), true),
        ])
    }

    #[test]
    fn a_gens_movie_becomes_a_movie_of_the_loaded_rom() {
        let gmv = gmv::build(b'A', *b"63", 0, &[[0xFF; 3], [0x7F, 0xFF, 0xFF], [0xFF; 3]]);
        let imported = import(&gmv, hash(), Region::Usa).unwrap();
        assert_eq!(imported.format, Format::Gmv);
        assert_eq!(imported.frames, 3);
        assert_eq!(imported.movie.rom_hash(), movie::rom_hash(ROM));
        assert!(imported.movie.check_rom(ROM).is_ok());
        assert_eq!(imported.movie.buttons_at(1, 0), Buttons::START);
        assert_eq!(imported.movie.buttons_at(2, 0), Buttons::empty());
        assert!(imported.movie.is_six_button(0), "pad 1 was a 6-button pad");
        assert!(!imported.movie.is_six_button(1));
    }

    /// TASVideos serves a .bk2 inside a .zip, and a .bk2 is a zip itself.
    #[test]
    fn a_bk2_inside_the_tasvideos_zip_is_found() {
        let inner = bk2(
            r#"{"UseSixButton":false,"Region":0}"#,
            &["|..|........|........|", "|..|.......S|........|"],
        );
        let outer = zip::build(&[("author-sonic.bk2", &inner, true)]);
        let imported = import(&outer, hash(), Region::Japan).unwrap();
        assert_eq!(imported.format, Format::Bk2);
        assert_eq!(imported.frames, 2);
        assert_eq!(
            imported.movie.region(),
            Region::Japan,
            "autodetect keeps the cartridge's region"
        );
        assert_eq!(imported.movie.buttons_at(1, 0), Buttons::START);
        assert_eq!(imported.game_name.as_deref(), Some("Test"));
        assert_eq!(imported.rerecords, Some(5));
    }

    #[test]
    fn a_gmv_inside_a_zip_is_found() {
        let gmv = gmv::build(b'A', *b"33", 0, &[[0xFF; 3]]);
        let outer = zip::build(&[
            ("readme.txt", b"hi", false),
            ("aglar-sonic.gmv", &gmv, true),
        ]);
        assert_eq!(
            import(&outer, hash(), Region::Usa).unwrap().format,
            Format::Gmv
        );
    }

    /// Gens only says 50 or 60 fps: PAL is Europe, and NTSC keeps the
    /// cartridge's NTSC region but is never Europe.
    #[test]
    fn a_gens_movie_picks_its_region_from_its_frame_rate() {
        let ntsc = gmv::build(b'A', *b"33", 0, &[[0xFF; 3]]);
        let pal = gmv::build(b'A', *b"33", 0x80, &[[0xFF; 3]]);
        assert_eq!(
            import(&ntsc, hash(), Region::Japan).unwrap().movie.region(),
            Region::Japan
        );
        assert_eq!(
            import(&ntsc, hash(), Region::Europe)
                .unwrap()
                .movie
                .region(),
            Region::Usa
        );
        assert_eq!(
            import(&pal, hash(), Region::Usa).unwrap().movie.region(),
            Region::Europe
        );
    }

    #[test]
    fn a_bk2_names_its_region() {
        let file = bk2(
            r#"{"UseSixButton":true,"Region":2}"#,
            &["|..|........|........|"],
        );
        let imported = import(&file, hash(), Region::Usa).unwrap();
        assert_eq!(imported.movie.region(), Region::Europe);
        assert!(imported.movie.is_six_button(0) && imported.movie.is_six_button(1));
    }

    #[test]
    fn things_that_are_not_movies_are_refused() {
        assert!(import(b"rustsega-movie 1\n", hash(), Region::Usa).is_err());
        let empty = zip::build(&[("readme.txt", b"hi", false)]);
        assert!(
            import(&empty, hash(), Region::Usa)
                .unwrap_err()
                .to_string()
                .contains("no BizHawk or Gens")
        );
        let gmv = gmv::build(b'A', *b"33", 0, &[[0xFF; 3]]);
        let two = zip::build(&[("a.gmv", &gmv, true), ("b.gmv", &gmv, true)]);
        assert!(
            import(&two, hash(), Region::Usa)
                .unwrap_err()
                .to_string()
                .contains("2 movies")
        );
    }

    /// The movie written back out is an ordinary `.rsm` that reads back the same.
    #[test]
    fn an_imported_movie_saves_as_rsm() {
        let gmv = gmv::build(b'A', *b"66", 0, &[[0xFF; 3], [0xFE, 0xFF, 0xFF]]);
        let imported = import(&gmv, hash(), Region::Usa).unwrap();
        let text = imported.movie.format();
        assert!(text.contains("six-button 1 2"));
        assert_eq!(movie::parse(&text).unwrap(), imported.movie);
    }
}
