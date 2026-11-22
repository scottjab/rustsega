//! Gens `.gmv` movies (#673).
//!
//! The layout is TASVideos' "GMV file format" page
//! (tasvideos.org/EmulatorResources/Gens/GMV), checked against a published
//! Gens 11b movie (TASVideos #3570, Sonic the Hedgehog):
//!
//! | offset | size | what |
//! |---|---|---|
//! | `0x00` | 15 | `Gens Movie TEST` |
//! | `0x0F` | 1 | the version, an ASCII character (`'A'` and up carry the flags) |
//! | `0x10` | 4 | rerecord count, little endian |
//! | `0x14` | 1 | player 1's pad, `'3'` or `'6'` |
//! | `0x15` | 1 | player 2's pad, `'3'` or `'6'` |
//! | `0x16` | 1 | flags: bit 7 50 fps, bit 6 starts from a savestate, bit 5 three players |
//! | `0x18` | 40 | the movie's name, zero terminated |
//! | `0x40` | 3 per frame | the input |
//!
//! Each frame is three bytes, every bit **set when the button is not pressed**:
//! player 1, player 2, then a byte of the 6-button buttons for both, X, Y, Z
//! and Mode of player 1 in bits 0-3 and of player 2 in bits 4-7. The 3-button
//! bytes are `0x01` Up, `0x02` Down, `0x04` Left, `0x08` Right, `0x10` A,
//! `0x20` B, `0x40` C, `0x80` Start. There is no frame count: the frames are
//! the rest of the file, three bytes at a time.

use genesis::Buttons;

use crate::{Inputs, TasError};

/// What every Gens movie starts with, before the version character.
pub const MAGIC: &[u8; 15] = b"Gens Movie TEST";
/// The header ahead of the frame data.
const HEADER_LEN: usize = 0x40;
/// Bytes of input per frame.
const FRAME_LEN: usize = 3;
/// Flag bit: the movie runs at 50 frames a second, a PAL console.
const FLAG_PAL: u8 = 0x80;
/// Flag bit: the movie starts from a Gens savestate rather than from power on.
const FLAG_SAVESTATE: u8 = 0x40;
/// Flag bit: the third byte is a third player rather than the 6-button byte.
const FLAG_THREE_PLAYERS: u8 = 0x20;
/// The first version whose header has the flags byte ("Version A and up only").
const FIRST_FLAGGED_VERSION: u8 = b'A';

/// The 3-button bytes, bit by bit.
const PAD_BITS: [(u8, Buttons); 8] = [
    (0x01, Buttons::UP),
    (0x02, Buttons::DOWN),
    (0x04, Buttons::LEFT),
    (0x08, Buttons::RIGHT),
    (0x10, Buttons::A),
    (0x20, Buttons::B),
    (0x40, Buttons::C),
    (0x80, Buttons::START),
];
/// One player's half of the 6-button byte, shifted down to bits 0-3.
const EXTRA_BITS: [(u8, Buttons); 4] = [
    (0x01, Buttons::X),
    (0x02, Buttons::Y),
    (0x04, Buttons::Z),
    (0x08, Buttons::MODE),
];

/// Is `bytes` a Gens movie?
pub fn is_gmv(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// The input of the Gens movie `bytes`.
pub fn parse(bytes: &[u8]) -> Result<Inputs, TasError> {
    if !is_gmv(bytes) {
        return Err(TasError::new(
            "not a Gens movie: no \"Gens Movie TEST\" signature",
        ));
    }
    if bytes.len() < HEADER_LEN {
        return Err(TasError::new(
            "Gens movie is shorter than its 64-byte header",
        ));
    }
    let version = bytes[0x0F];
    let flags = if version >= FIRST_FLAGGED_VERSION {
        bytes[0x16]
    } else {
        0
    };
    if flags & FLAG_SAVESTATE != 0 {
        return Err(TasError::new(
            "this Gens movie starts from a savestate, and only movies that start from power on can be played",
        ));
    }
    if flags & FLAG_THREE_PLAYERS != 0 {
        return Err(TasError::new("three-player Gens movies are not supported"));
    }
    let data = &bytes[HEADER_LEN..];
    if !data.len().is_multiple_of(FRAME_LEN) {
        return Err(TasError::new(format!(
            "Gens movie input is {} bytes, not a whole number of 3-byte frames",
            data.len()
        )));
    }

    let frames = data
        .as_chunks::<FRAME_LEN>()
        .0
        .iter()
        .map(|frame| {
            [
                pressed(frame[0], &PAD_BITS) | pressed(frame[2] & 0x0F, &EXTRA_BITS),
                pressed(frame[1], &PAD_BITS) | pressed(frame[2] >> 4, &EXTRA_BITS),
            ]
        })
        .collect();
    let name_bytes = &bytes[0x18..HEADER_LEN];
    let name_end = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(name_bytes.len());
    let name = String::from_utf8_lossy(&name_bytes[..name_end])
        .trim()
        .to_string();

    Ok(Inputs {
        frames,
        six_button: [bytes[0x14] == b'6', bytes[0x15] == b'6'],
        pal: Some(flags & FLAG_PAL != 0),
        region: None,
        game_name: (!name.is_empty()).then_some(name),
        rom_digest: None,
        rerecords: Some(u64::from(u32::from_le_bytes([
            bytes[0x10],
            bytes[0x11],
            bytes[0x12],
            bytes[0x13],
        ]))),
    })
}

/// The buttons whose bit in `byte` is clear: the format is active low.
fn pressed(byte: u8, bits: &[(u8, Buttons)]) -> Buttons {
    bits.iter()
        .filter(|(bit, _)| byte & bit == 0)
        .fold(Buttons::empty(), |held, (_, button)| held | *button)
}

/// A Gens movie for the tests: `frames` of `[p1, p2, extra]`, raw bytes.
#[cfg(test)]
pub(crate) fn build(version: u8, pads: [u8; 2], flags: u8, frames: &[[u8; 3]]) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[..15].copy_from_slice(MAGIC);
    out[0x0F] = version;
    out[0x10..0x14].copy_from_slice(&1234u32.to_le_bytes());
    out[0x14] = pads[0];
    out[0x15] = pads[1];
    out[0x16] = flags;
    out[0x18..0x18 + 5].copy_from_slice(b"sonic");
    for frame in frames {
        out.extend_from_slice(frame);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [u8; 3] = [0xFF, 0xFF, 0xFF];

    #[test]
    fn the_header_is_read() {
        let movie = build(b'A', *b"63", 0, &[NONE, NONE]);
        let inputs = parse(&movie).unwrap();
        assert_eq!(inputs.frames.len(), 2);
        assert_eq!(inputs.six_button, [true, false]);
        assert_eq!(inputs.pal, Some(false));
        assert_eq!(inputs.rerecords, Some(1234));
        assert_eq!(inputs.game_name.as_deref(), Some("sonic"));
    }

    /// Bits are active low: a clear bit is a pressed button. #3570's first
    /// press is frame 293's `0x7F`, Start.
    #[test]
    fn a_clear_bit_is_a_pressed_button() {
        let movie = build(
            b'A',
            *b"33",
            0,
            &[NONE, [0x7F, 0xFF, 0xFF], [0xFE, 0xEF, 0xFF]],
        );
        let frames = parse(&movie).unwrap().frames;
        assert_eq!(frames[0], [Buttons::empty(), Buttons::empty()]);
        assert_eq!(frames[1], [Buttons::START, Buttons::empty()]);
        assert_eq!(frames[2], [Buttons::UP, Buttons::A]);
    }

    /// The third byte holds X, Y, Z and Mode: player 1 in the low nibble and
    /// player 2 in the high one.
    #[test]
    fn the_third_byte_is_both_players_six_button_buttons() {
        let movie = build(b'A', *b"66", 0, &[[0xFF, 0xFF, 0xF7], [0xFF, 0xFF, 0xBF]]);
        let frames = parse(&movie).unwrap().frames;
        assert_eq!(frames[0], [Buttons::MODE, Buttons::empty()]);
        assert_eq!(frames[1], [Buttons::empty(), Buttons::Z]);
    }

    #[test]
    fn the_fps_flag_says_pal() {
        let movie = build(b'A', *b"33", 0x80, &[NONE]);
        assert_eq!(parse(&movie).unwrap().pal, Some(true));
    }

    /// Before version A the flags byte was not defined, so it is not read.
    #[test]
    fn an_old_version_has_no_flags() {
        let movie = build(b'9', *b"33", 0xFF, &[NONE]);
        let inputs = parse(&movie).expect("old movies play");
        assert_eq!(inputs.pal, Some(false));
    }

    #[test]
    fn savestate_and_three_player_movies_are_refused() {
        assert!(parse(&build(b'A', *b"33", 0x40, &[NONE])).is_err());
        assert!(parse(&build(b'A', *b"33", 0x20, &[NONE])).is_err());
    }

    #[test]
    fn a_short_or_ragged_file_is_refused() {
        assert!(parse(&MAGIC[..]).is_err());
        let mut ragged = build(b'A', *b"33", 0, &[NONE]);
        ragged.push(0xFF);
        assert!(parse(&ragged).is_err());
    }
}
