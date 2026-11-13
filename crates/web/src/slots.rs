//! Save state slots: the record a slot holds, the picture it shows, and the
//! slot numbers a player moves between (M7-15, #148).
//!
//! Everything here is the decision half - how a slot's bytes are laid out, what a
//! thumbnail is, which slot F2 answers with - and none of it is the browser half.
//! The browser half is [`crate::emulator`], which owns the running console and the
//! [`crate::storage`] round trip, and the panel in [`crate::ui::slot_panel`],
//! which draws the cards.
//!
//! The record is one binary blob per slot, put in the `states` store at
//! [`crate::storage::state_key`] - the ticket's `slot_key`, which #147 (#147)
//! called `state_key` and this reuses rather than spelling a second way. It is
//! hand-rolled for the same reason [`genesis`]'s own container is: the payload is
//! already a compact binary blob, and a `Vec<u8>` through `serde_json` becomes a
//! JSON array of decimal numbers, which costs several times the state itself to
//! store the same bytes.

/// The number of slots a game gets, so the panel is a 2x2 grid and F2/F3 wrap
/// over `0..SLOT_COUNT`.
pub const SLOT_COUNT: u32 = 4;

/// The record format this build writes, and the only one it reads.
pub const RECORD_VERSION: u8 = 1;

/// Bytes of [`RECORD_VERSION`] header: magic, version, clock, thumbnail size, and
/// the two lengths.
const HEADER_LEN: usize = 4 + 1 + 8 + 2 + 2 + 4 + 4;

/// Magic bytes at the front of every slot record.
const MAGIC: &[u8; 4] = b"RSTS";

/// What one slot holds: a save state, and enough about it to show a card without
/// opening the state.
///
/// `state` is the container [`genesis::Genesis::save_state`] wrote, byte for
/// byte - a slot adds nothing to it, so a state that this build would refuse is
/// refused here for its own reasons and says so with its own [`genesis::StateError`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    /// When the slot was written, milliseconds since the epoch, from
    /// `Date::now()`. `u64` rather than the `f64` that call gives: the record has
    /// `Eq` and a millisecond needs no fraction.
    pub created_ms: u64,
    /// The card's picture: RGBA8888, [`SlotRecord::thumb_w`] by
    /// [`SlotRecord::thumb_h`], as [`make_thumbnail`] made it.
    pub thumb_rgba: Vec<u8>,
    /// Thumbnail width in pixels.
    pub thumb_w: u32,
    /// Thumbnail height in pixels.
    pub thumb_h: u32,
    /// The save state itself.
    pub state: Vec<u8>,
}

/// Why a record is not a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotError {
    /// Fewer than [`HEADER_LEN`] bytes, so there is no header to read.
    Truncated,
    /// The bytes do not start with [`MAGIC`]: not a slot record at all.
    BadMagic,
    /// The record is format version `n` and this build knows
    /// [`RECORD_VERSION`] only.
    UnsupportedVersion(u8),
    /// The header's lengths disagree with the bytes that follow it.
    BadLength,
}

impl core::fmt::Display for SlotError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnsupportedVersion(_) => write!(
                f,
                "This save state is from an older version and can't be loaded"
            ),
            Self::Truncated | Self::BadMagic | Self::BadLength => {
                write!(f, "This save state is damaged and can't be loaded")
            }
        }
    }
}

/// The card's picture: `rgba`, `w` by `h`, downscaled by a 2x box filter.
///
/// Four source pixels per output pixel, each channel averaged with the rounding of
/// `(a + b + c + d + 2) / 4` so that the mean of two 0s and two 255s is 128 rather
/// than 127. Alpha is averaged with them, which for a Genesis frame - always
/// opaque, see [`crate::emulator::frame_to_rgba_bytes`] - keeps it opaque.
///
/// Odd sizes floor: an odd edge has no partner to average with, and half a pixel
/// of it is worth less than a whole row of output that is wrong at one edge. So
/// 320x224 gives 160x112 and 321x225 gives 160x112 too.
///
/// `Err`-shaped failures are `(Vec::new(), 0, 0)`: an input shorter than `w * h *
/// 4` is a frame that was never completed, and 0x0 is what the panel already
/// treats as "no picture". Nothing here can panic on a short slice, because the
/// caller hands it bytes that came out of IndexedDB next to a state.
pub fn make_thumbnail(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (out_w, out_h) = (w / 2, h / 2);
    // The source size in bytes, with the multiply that a 65536-wide frame would
    // not fit in a 32-bit `usize` answered as "no picture" rather than wrapped.
    let needed = w
        .checked_mul(h)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok());
    if out_w == 0 || out_h == 0 || needed.is_none_or(|bytes| rgba.len() < bytes) {
        return (Vec::new(), 0, 0);
    }
    let width = w as usize;
    let mut out = vec![0u8; out_w as usize * 4 * out_h as usize];
    for out_y in 0..out_h as usize {
        for out_x in 0..out_w as usize {
            // The four source pixels of one output pixel: `(x, y)`, `(x+1, y)`,
            // `(x, y+1)`, `(x+1, y+1)` of the doubled grid.
            let left = out_x * 4;
            let right = left + 4;
            let top = out_y * 2 * width * 4;
            let bottom = top + width * 4;
            let pixel = out_y * out_w as usize * 4 + out_x * 4;
            for channel in 0..4 {
                let sum = u32::from(rgba[top + left + channel])
                    + u32::from(rgba[top + right + channel])
                    + u32::from(rgba[bottom + left + channel])
                    + u32::from(rgba[bottom + right + channel]);
                out[pixel + channel] = ((sum + 2) / 4) as u8;
            }
        }
    }
    (out, out_w, out_h)
}

/// The bytes one slot stores: [`SlotRecord`] as the header of [`RECORD_VERSION`].
///
/// Fixed little-endian header, then the thumbnail, then the state:
///
/// ```text
/// 0  magic          b"RSTS"
/// 4  record version u8
/// 5  created_ms     u64, little endian
/// 13 thumb width    u16
/// 15 thumb height   u16
/// 17 thumb length   u32
/// 21 state length   u32
/// 25 thumb RGBA, then the save state
/// ```
///
/// Twenty-five bytes either side of two blobs that are 71,680 and a save state,
/// which is the whole point: nothing here inflates what it stores.
pub fn encode(record: &SlotRecord) -> Vec<u8> {
    let thumb_w = u16::try_from(record.thumb_w).unwrap_or(u16::MAX);
    let thumb_h = u16::try_from(record.thumb_h).unwrap_or(u16::MAX);
    let thumb_len = record.thumb_rgba.len() as u32;
    let state_len = record.state.len() as u32;
    let mut out = Vec::with_capacity(HEADER_LEN + record.thumb_rgba.len() + record.state.len());
    out.extend_from_slice(MAGIC);
    out.push(RECORD_VERSION);
    out.extend_from_slice(&record.created_ms.to_le_bytes());
    out.extend_from_slice(&thumb_w.to_le_bytes());
    out.extend_from_slice(&thumb_h.to_le_bytes());
    out.extend_from_slice(&thumb_len.to_le_bytes());
    out.extend_from_slice(&state_len.to_le_bytes());
    out.extend_from_slice(&record.thumb_rgba);
    out.extend_from_slice(&record.state);
    out
}

/// The [`SlotRecord`] `bytes` holds, or why it does not.
///
/// Every check is answered before anything is copied out of `bytes`, so that no
/// input reaches a slice index that is not there: a truncation of a real record,
/// a byte flipped in one, or a record some other build wrote. That matters more
/// here than it does for most decoders, because this runs on bytes read straight
/// back out of IndexedDB, where a half-written transaction and a devtools
/// hand-edit both look like data.
pub fn decode(bytes: &[u8]) -> Result<SlotRecord, SlotError> {
    if bytes.len() < HEADER_LEN {
        return Err(SlotError::Truncated);
    }
    if &bytes[0..4] != MAGIC {
        return Err(SlotError::BadMagic);
    }
    if bytes[4] != RECORD_VERSION {
        return Err(SlotError::UnsupportedVersion(bytes[4]));
    }
    let thumb_w = u32::from(u16::from_le_bytes([bytes[13], bytes[14]]));
    let thumb_h = u32::from(u16::from_le_bytes([bytes[15], bytes[16]]));
    let thumb_len = u32::from_le_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]) as u64;
    let state_len = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]) as u64;
    // The two lengths are the whole of the body, in this order and with nothing
    // between: a record that claims more than it holds, or less, is not read
    // further. `u64` because both are read from `u32`s and the sum of two of them
    // does not fit in the `usize` of a 32-bit wasm build.
    let body = bytes.len() - HEADER_LEN;
    if thumb_len.checked_add(state_len) != Some(body as u64) {
        return Err(SlotError::BadLength);
    }
    let thumb_end = HEADER_LEN + thumb_len as usize;
    Ok(SlotRecord {
        created_ms: u64::from_le_bytes(bytes[5..13].try_into().unwrap_or([0; 8])),
        thumb_rgba: bytes[HEADER_LEN..thumb_end].to_vec(),
        thumb_w,
        thumb_h,
        state: bytes[thumb_end..].to_vec(),
    })
}

/// The slot one step from `slot` in `delta` direction, wrapping over
/// `0..SLOT_COUNT`: F3 takes 0 to 1 and 3 to 0.
///
/// Wrapping rather than stopping is what makes two keys enough to reach any of
/// four slots without the player counting presses to the end of the row.
pub fn next_slot(slot: u32, delta: i32) -> u32 {
    let count = SLOT_COUNT as i64;
    let steps = i64::from(slot) + i64::from(delta);
    ((steps % count + count) % count) as u32
}

/// Whether `slot` is one of this build's slots.
pub fn is_slot(slot: u32) -> bool {
    slot < SLOT_COUNT
}

/// How long ago `created_ms` was, for a card: "just now", "3 min ago", "2 hr ago",
/// "yesterday", "5 days ago".
///
/// Relative rather than a date because the thing a player is deciding is "is this
/// the one from before the boss", and a clock time says that only if they remember
/// when they played. Coarse to one significant unit for the same reason: a card
/// that reads "3 min 12 s ago" is a card nobody reads.
///
/// "Yesterday" is elapsed time - the 24 h to 48 h of it - and not a calendar day,
/// because this is given two numbers on a monotonic-ish clock and has no date,
/// timezone or midnight to work out (`#149` asked for the word; a calendar day
/// would have needed a calendar). So a game opened at 11 pm reads "yesterday" at
/// 1 am the next morning, which is what a card two hours old should say.
///
/// Given the clock rather than reading one, like [`crate::storage::SramWatcher`],
/// so the whole of it runs natively.
pub fn relative_time(now_ms: f64, created_ms: f64) -> String {
    let elapsed_secs = (now_ms - created_ms) / 1_000.0;
    // `partial_cmp` because a clock that went backwards and a `NaN` from a time
    // that was never read both land on "just now", and neither is a number that
    // answers `<=`.
    if elapsed_secs
        .partial_cmp(&1.0)
        .is_none_or(std::cmp::Ordering::is_le)
    {
        return "just now".to_string();
    }
    let secs = elapsed_secs as u64;
    if secs < 60 {
        format!("{secs} sec ago")
    } else if secs < 3_600 {
        unit(secs / 60, "min", "min")
    } else if secs < 86_400 {
        unit(secs / 3_600, "hr", "hr")
    } else if secs < 2 * 86_400 {
        "yesterday".to_string()
    } else {
        unit(secs / 86_400, "day", "days")
    }
}

/// One unit of relative time: the abbreviated units of a card stay short in the
/// plural - "3 min ago", not "3 mins ago" - and "day" is the one word long
/// enough to be worth its plural.
fn unit(count: u64, name: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {name} ago")
    } else {
        format!("{count} {plural} ago")
    }
}

/// The clock a slot is stamped with: milliseconds since the epoch.
///
/// A separate function so the one place that reads a clock in this module is
/// named; native there is no `Date`, and nothing natively writes a slot.
pub fn clock_ms() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

/// The one sentence a load failure shows, one per [`genesis::StateError`] variant.
///
/// The version case is the ticket's sentence. [`genesis::StateError::WrongRom`] is
/// worth its own rather than being folded into "older version", because it is the
/// reachable one: slots are keyed by the ROM's SHA-1 ([`crate::rom::LoadedRom::id`]),
/// so a `Rev A` dump and the dump it was revised from are two games to the key and
/// two different machines to [`genesis::Genesis::load_state`]. A player who copied
/// a folder of slots between two dumps is being told the truth, not that they have
/// an old browser.
pub fn state_error_toast(error: genesis::StateError) -> &'static str {
    match error {
        genesis::StateError::UnsupportedVersion(_) => {
            "This save state is from an older version and can't be loaded"
        }
        genesis::StateError::WrongRom => "That save state is for a different game",
        genesis::StateError::BadMagic | genesis::StateError::Corrupt => {
            "This save state is damaged and can't be loaded"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one colour, `w` by `h`, for the thumbnail tests.
    fn flat(w: u32, h: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h) as usize * 4);
        for _ in 0..w * h {
            out.extend_from_slice(&pixel);
        }
        out
    }

    /// The ticket's case: a 320x224 frame gives a 160x112 card picture, which is
    /// a Genesis frame at half size in both directions.
    #[test]
    fn a_frame_thumbnails_to_half_its_size() {
        let (thumb, w, h) = make_thumbnail(&flat(320, 224, [10, 20, 30, 255]), 320, 224);
        assert_eq!((w, h), (160, 112));
        assert_eq!(thumb.len(), 160 * 112 * 4);
    }

    /// The filter is a *box* filter, not a point sample: four different pixels
    /// average channel by channel, with the third pixel of the block - the one a
    /// point sample would have taken - deliberately unlike the answer.
    #[test]
    fn a_two_by_two_block_of_known_colours_averages() {
        // 2x2, RGBA: (255,0,0,255) (0,255,0,255) / (0,0,255,255) (0,0,0,143)
        let source: Vec<u8> = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 143,
        ]
        .to_vec();
        let (thumb, w, h) = make_thumbnail(&source, 2, 2);
        assert_eq!((w, h), (1, 1));
        // R: (255 + 0 + 0 + 0 + 2) / 4 = 64, G: (0 + 255 + 0 + 0 + 2) / 4 = 64,
        // B: (0 + 0 + 255 + 0 + 2) / 4 = 64, A: (255 + 255 + 255 + 143 + 2) / 4 = 227.
        assert_eq!(thumb, vec![64, 64, 64, 227]);
    }

    /// A 2x2 block whose mean is exactly halfway between two 8-bit values rounds
    /// up rather than down, and does it the same way for every channel: `(sum + 2)
    /// / 4` is the rounding, so 127.5 reads as 128.
    #[test]
    fn an_odd_average_rounds_to_the_nearest_value() {
        // Every channel sums to 510: mean 127.5.
        let source: Vec<u8> = [
            255u8, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0,
        ]
        .to_vec();
        let (thumb, _, _) = make_thumbnail(&source, 2, 2);
        assert_eq!(thumb, vec![128, 128, 128, 128]);
    }

    /// An odd width or height has a column and a row with nothing to average with.
    /// Flooring is the rule, so the output is the floor of half in both
    /// directions - 321x225 and 320x224 cannot both be 161x113.
    #[test]
    fn an_odd_frame_floors_rather_than_inventing_an_edge() {
        let (thumb, w, h) = make_thumbnail(&flat(321, 225, [1, 2, 3, 255]), 321, 225);
        assert_eq!((w, h), (160, 112));
        assert_eq!(thumb.len(), 160 * 112 * 4);
        // And a 3x3 frame of one colour is one pixel of it: the last column and
        // row are the ones dropped, and the one output pixel is the average of a
        // whole 2x2 block.
        let (small, small_w, small_h) = make_thumbnail(&flat(3, 3, [9, 9, 9, 255]), 3, 3);
        assert_eq!((small_w, small_h), (1, 1));
        assert_eq!(small, vec![9, 9, 9, 255]);
    }

    /// A frame that is not two pixels across, and a slice that is shorter than the
    /// frame it claims, are both "no picture" rather than a panic or a thumbnail
    /// read off the end of a buffer.
    #[test]
    fn a_frame_that_cannot_be_halved_thumbnails_to_nothing() {
        assert_eq!(
            make_thumbnail(&flat(1, 1, [0; 4]), 1, 1),
            (Vec::new(), 0, 0)
        );
        assert_eq!(
            make_thumbnail(&flat(0, 0, [0; 4]), 0, 0),
            (Vec::new(), 0, 0)
        );
        // 320x224 needs 286,720 bytes; hand it one fewer.
        let short = flat(320, 224, [0; 4]);
        assert_eq!(
            make_thumbnail(&short[..short.len() - 1], 320, 224),
            (Vec::new(), 0, 0)
        );
        assert_eq!(make_thumbnail(&[], 320, 224), (Vec::new(), 0, 0));
    }

    /// A record is the state and the card's picture, and both come back
    /// untouched - byte for byte, including a state that is not a state at all,
    /// because whether the state is one is [`genesis`]'s question, not the
    /// container's.
    #[test]
    fn a_record_survives_the_round_trip() {
        let record = SlotRecord {
            created_ms: 1_700_000_000_123,
            thumb_rgba: vec![7u8; 16],
            thumb_w: 2,
            thumb_h: 2,
            state: (0u16..256).map(|n| n as u8).collect(),
        };
        let bytes = encode(&record);
        assert_eq!(decode(&bytes).expect("decodes"), record);
    }

    /// A record with no picture and no state is a record: an empty state is what
    /// a slot never holds, but the container has no reason to invent a rule about
    /// it that the console has not already decided.
    #[test]
    fn an_empty_record_round_trips() {
        let record = SlotRecord {
            created_ms: 0,
            thumb_rgba: Vec::new(),
            thumb_w: 0,
            thumb_h: 0,
            state: Vec::new(),
        };
        assert_eq!(decode(&encode(&record)).expect("decodes"), record);
    }

    /// The container says what version it is, so a record from a build that wrote
    /// a different one is refused with the one sentence the ticket asks for
    /// rather than decoded wrong.
    #[test]
    fn a_record_of_another_version_is_refused() {
        let record = SlotRecord {
            created_ms: 1,
            thumb_rgba: vec![1, 2, 3, 4],
            thumb_w: 1,
            thumb_h: 1,
            state: vec![9; 8],
        };
        let mut bytes = encode(&record);
        bytes[4] = RECORD_VERSION + 1;
        assert_eq!(
            decode(&bytes),
            Err(SlotError::UnsupportedVersion(RECORD_VERSION + 1))
        );
        assert_eq!(
            decode(&bytes).unwrap_err().to_string(),
            "This save state is from an older version and can't be loaded"
        );
    }

    /// Garbage is an error, not a panic: every truncation of a real record, every
    /// single-byte flip of the header and of the lengths, and a few megabytes of
    /// a counter, all have to answer and none of them may reach a slice index
    /// outside the buffer (#387 - a panic on the frame loop kills the page).
    #[test]
    fn garbage_is_refused_and_never_panics() {
        let record = SlotRecord {
            created_ms: 12_345,
            thumb_rgba: vec![5u8; 64],
            thumb_w: 4,
            thumb_h: 4,
            state: vec![0xA5u8; 300],
        };
        let good = encode(&record);

        // Every truncation, and nothing at all.
        for cut in 0..good.len() {
            let outcome = std::panic::catch_unwind(|| decode(&good[..cut]));
            assert!(outcome.is_ok(), "decode of a {cut}-byte prefix panicked");
            assert_ne!(
                outcome.unwrap().map(|decoded| decoded.state),
                Ok(record.state.clone())
            );
        }
        assert!(decode(&[]).is_err());

        // Every single-byte change: magic, version, clock, sizes, lengths, body.
        for index in 0..good.len() {
            for bit in [1u8, 2, 4, 8, 16, 32, 64, 128] {
                let mut broken = good.clone();
                broken[index] ^= bit;
                let _ = std::panic::catch_unwind(move || decode(&broken))
                    .expect("decode of a flipped byte must not panic");
            }
        }

        // Foreign bytes of every length up to a kilobyte.
        let foreign: Vec<u8> = (0..1024u32).map(|n| (n * 37) as u8).collect();
        for cut in (0..foreign.len()).step_by(7) {
            assert!(std::panic::catch_unwind(|| decode(&foreign[..cut])).is_ok());
        }
        assert_eq!(decode(&foreign).unwrap_err(), SlotError::BadMagic);
    }

    /// A record whose lengths do not agree with the bytes present - the shape of a
    /// write IndexedDB took halfway - is refused rather than read past.
    #[test]
    fn a_lying_header_is_refused() {
        let good = encode(&SlotRecord {
            created_ms: 7,
            thumb_rgba: vec![3u8; 8],
            thumb_w: 2,
            thumb_h: 2,
            state: vec![4u8; 40],
        });
        // Claim a thumbnail a kilobyte longer than the record.
        let mut lying = good.clone();
        lying[17..21].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(decode(&lying), Err(SlotError::BadLength));
        // And one that leaves bytes over.
        let mut short = good.clone();
        short[17..21].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(decode(&short), Err(SlotError::BadLength));
    }

    /// Four slots, numbered from 0, because the panel is a 2x2 grid and the key
    /// is `"{rom_id}:{slot}"` - the key of [`crate::storage::state_key`], which
    /// this is the slot number half of.
    #[test]
    fn there_are_four_slots_and_they_are_numbered_from_zero() {
        assert_eq!(SLOT_COUNT, 4);
        for slot in 0..SLOT_COUNT {
            assert!(is_slot(slot), "slot {slot} is a slot");
            assert_eq!(
                crate::storage::state_key("abcd", slot),
                format!("abcd:{slot}")
            );
        }
        assert!(!is_slot(SLOT_COUNT));
        assert!(!is_slot(SLOT_COUNT + 1));
    }

    /// F2 and F3 move one slot and wrap, so four slots are reachable with two keys
    /// whoever is holding them is on their way to any of the others.
    #[test]
    fn the_slot_keys_step_one_slot_and_wrap() {
        assert_eq!(next_slot(0, 1), 1);
        assert_eq!(next_slot(2, 1), 3);
        assert_eq!(next_slot(3, 1), 0);
        assert_eq!(next_slot(0, -1), 3);
        assert_eq!(next_slot(0, -4), 0);
        assert_eq!(next_slot(1, -3), 2);
        // Eight steps of either key come back where they started.
        let mut slot = 0;
        for _ in 0..8 {
            slot = next_slot(slot, 1);
        }
        assert_eq!(slot, 0);
    }

    /// The card's one line of time, in the unit a player thinks in.
    #[test]
    fn the_time_on_a_card_is_relative_and_coarse() {
        let minute = 60_000.0;
        let hour = 60.0 * minute;
        let day = 24.0 * hour;
        assert_eq!(relative_time(1_000.0, 1_000.0), "just now");
        assert_eq!(relative_time(59_000.0, 0.0), "59 sec ago");
        assert_eq!(relative_time(minute, 0.0), "1 min ago");
        assert_eq!(relative_time(3.4 * minute, 0.0), "3 min ago");
        assert_eq!(relative_time(59.5 * minute, 0.0), "59 min ago");
        assert_eq!(relative_time(hour, 0.0), "1 hr ago");
        assert_eq!(relative_time(26.0 * hour, 0.0), "yesterday");
        assert_eq!(relative_time(5.0 * day, 0.0), "5 days ago");
        // A clock that went backwards, and a save from the future: both "just
        // now", because a card that says "-3 min ago" is a card that is wrong.
        assert_eq!(relative_time(0.0, minute), "just now");
        assert_eq!(relative_time(0.0, 0.0), "just now");
    }

    /// The two edges of "yesterday", pinned on both sides (#149 changed this
    /// caption from "1 day ago"): the day before that is still counted in hours,
    /// and the second day after is counted in days again.
    #[test]
    fn yesterday_is_two_days_of_elapsed_time_and_not_a_calendar_day() {
        let minute = 60_000.0;
        let hour = 60.0 * minute;
        assert_eq!(relative_time(23.0 * hour + 59.0 * minute, 0.0), "23 hr ago");
        assert_eq!(relative_time(24.0 * hour, 0.0), "yesterday");
        assert_eq!(relative_time(47.0 * hour + 59.0 * minute, 0.0), "yesterday");
        assert_eq!(relative_time(48.0 * hour + 1_000.0, 0.0), "2 days ago");
        // Exactly two days, the same answer: the word "yesterday" is the day
        // between one day and two, and it does not stretch to cover either edge.
        assert_eq!(relative_time(48.0 * hour, 0.0), "2 days ago");
    }

    /// Every way a state can fail to load has its own sentence, and there is no
    /// variant of [`genesis::StateError`] without one: a load that fails is the
    /// one moment the player needs to be told what happened rather than see
    /// nothing, and the game has to keep running either way.
    #[test]
    fn every_state_error_has_its_one_sentence() {
        use genesis::StateError;

        assert_eq!(
            state_error_toast(StateError::UnsupportedVersion(0)),
            "This save state is from an older version and can't be loaded"
        );
        assert_eq!(
            state_error_toast(StateError::WrongRom),
            "That save state is for a different game"
        );
        assert_eq!(
            state_error_toast(StateError::BadMagic),
            "This save state is damaged and can't be loaded"
        );
        assert_eq!(
            state_error_toast(StateError::Corrupt),
            "This save state is damaged and can't be loaded"
        );
    }

    /// The machine a slot restores includes the sound it was making.
    ///
    /// Two identical machines run the same fake ROM for ten frames; one saves
    /// there, both run ten more, and the two chips' samples and the master clock
    /// each one starts at are one hash per frame. Then the first machine goes back
    /// to its own state and runs those same ten frames again, and every hash comes
    /// out the same as the machine that never saved at all - which is the state
    /// carrying the FM and PSG state, their sample buffers, and both chips'
    /// positions in the master clock (`crate::audio::Audio::frame`). None of that
    /// shows in a picture, which is why a slot that saved only the picture would
    /// pass every other test here and be out of step for ever after.
    ///
    /// This is the machine, not the speaker: what the host's mixer does with a
    /// clock that just went backwards is
    /// [#362](https://github.com/scottjab/rustsega/issues/362), and what the
    /// browser half does about it is measured in the PR.
    #[test]
    fn a_restored_machine_goes_on_making_the_same_sound() {
        let rom = fake_rom(&[0x60FE]);
        let mut saved = genesis::Genesis::new(&rom, genesis::Config::default()).expect("console");
        let mut plain = genesis::Genesis::new(&rom, genesis::Config::default()).expect("console");

        let mut expected = Vec::new();
        for _ in 0..10 {
            run_one_frame(&mut plain);
            run_one_frame(&mut saved);
        }
        let state = saved.save_state();
        for _ in 0..10 {
            run_one_frame(&mut plain);
            run_one_frame(&mut saved);
            let (here, there) = (chip_hash(&saved), chip_hash(&plain));
            assert_eq!(here, there, "the chips drifted before the save");
            expected.push(there);
        }

        saved.load_state(&state).expect("the state loads back");
        for (frame, want) in expected.iter().enumerate() {
            run_one_frame(&mut saved);
            assert_eq!(
                chip_hash(&saved),
                *want,
                "frame {frame} after the load sounds different from the same frame never saved"
            );
        }
    }

    /// The reset table for a program of `program`, in the shape
    /// `crates/genesis/tests/save_state.rs` uses for the same reason: a machine
    /// with a running 68000 is what has a state worth saving.
    fn fake_rom(program: &[u16]) -> Vec<u8> {
        let code = 0x1000usize;
        let mut rom = vec![0_u8; (code + 2 * program.len()).max(0x200)];
        rom[0..4].copy_from_slice(&0x00FF_0000u32.to_be_bytes());
        rom[4..8].copy_from_slice(&(code as u32).to_be_bytes());
        for (word, at) in program.iter().zip(0..) {
            let at = code + 2 * at;
            rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
        }
        rom
    }

    /// One frame of the machine, and the audio drained with it: the mixer is only
    /// advanced when its caller asks, and a test that never drains is a test that
    /// never touches the half of the machine this is about.
    fn run_one_frame(machine: &mut genesis::Genesis) {
        machine.run_frame();
        let mut samples = Vec::new();
        machine.drain_audio(&mut samples);
    }

    /// The frame's sound as the machine made it: both chips' samples and the
    /// master clock each one starts at, hashed by the same FNV-1a the rest of the
    /// repo hashes bytes with ([`crate::storage::fnv1a_64`]).
    fn chip_hash(machine: &genesis::Genesis) -> u64 {
        let frame = machine.bus().audio.frame();
        let mut bytes = Vec::new();
        for (left, right) in frame.ym {
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        for sample in frame.psg {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes.extend_from_slice(&frame.ym_first.to_le_bytes());
        bytes.extend_from_slice(&frame.psg_first.to_le_bytes());
        crate::storage::fnv1a_64(&bytes)
    }
}
