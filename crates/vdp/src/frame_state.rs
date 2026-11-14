//! Serialising [`Vdp::frame_buffer`] as the picture's size plus the one field a
//! mode-2 frame cannot draw again.
//!
//! A frame buffer is 320 x 480 `u32` - 600 KiB of what the beam has already
//! painted, which is a pure function of the memories, the registers and the beam
//! position that the rest of a save state carries, and the first thing the
//! restored chip does is overwrite it. Putting it in a state file would make
//! every save six hundred kilobytes to keep a picture for one frame, and a save
//! state of a Genesis emulator is written to a browser's storage (M6-07, #127).
//!
//! The `width` and `height` are kept, and they are not decoration. A line is
//! written at `display_line * frame_buffer.width`, and `Vdp::size_frame_width`
//! refreshes that stride on the first line of a frame: a restored chip that
//! started at [`FrameBuffer::new`]'s widest size would spend the rest of the frame
//! writing rows with the stride of a mode it is not in. With the two numbers the
//! first line the restored chip draws lands where the saved one would have put
//! it, and only the rows nobody has redrawn yet read as black.
//!
//! Keeping the size rather than the pixels is only sound while a frame writes
//! every pixel of the picture it publishes, which is what tying the stride to the
//! frame's own mode is for (#450).
//!
//! Interlace mode 2 is where that stops holding, and it stops by construction
//! (#457). [`Vdp::display_line`] maps a beam line to `line * 2 + field` and
//! leaves the lines of the other field alone, while [`Vdp::frame_height`]
//! publishes `2 * active_height`. Half of the picture a mode-2 frame publishes is
//! therefore the frame before it: no frame that has yet to run will draw it, and
//! the rest of the state cannot be run forward into it, because the tiles and the
//! palette a frame is drawn from are rewritten every frame. So a third thing goes
//! with the two sizes - the lines of the field the frame being drawn does *not*
//! touch, as pixels after all - and only then.
//!
//! That third thing is `Option`al in the payload rather than conditional on the
//! interlace mode the reader has or has not reached. `postcard` is not
//! self-describing, so a field that is only *sometimes* there is decodable if and
//! only if the mode is known at that byte. It happens to be: the mode comes from
//! the register file, which precedes the frame buffer within [`Vdp`], and the VDP
//! precedes the scheduler in the state tuple of `genesis::state`. Relying on that
//! would let a future reordering change what a payload means without changing a
//! byte, so the presence tag says whether the field is there instead. It costs
//! the two modes that are not interlaced one byte, and
//! `a_progressive_frame_buffer_still_carries_no_pixels` is what holds them to
//! that.
//!
//! Mode 2 pays the rest: `width * active_height` `u32`s, so 224 KiB at H32 and
//! 300 KiB at H40 with PAL in V30, and no cart of `commercial_list.toml` reaches
//! the mode. Only the lines inside the published picture are carried, since
//! nothing outside it is published.

use serde::{Deserialize, Serialize};
use serde::{Deserializer, Serializer};

use crate::framebuffer::FrameBuffer;

/// Bytes of one pixel of a carried field: the `u32` of [`FrameBuffer::pixels`],
/// written little endian so a state is the same width on every target.
const PIXEL_BYTES: usize = 4;

/// The frame buffer as the two numbers that say where its lines start, plus the
/// field of a woven picture that the frame being drawn leaves alone.
pub fn serialize<S: Serializer>(frame: &FrameBuffer, ser: S) -> Result<S::Ok, S::Error> {
    // `u32` rather than `usize` so a state is the same width on every target.
    (frame.width as u32, frame.height as u32, carried(frame)).serialize(ser)
}

/// A black buffer of the size `serialize` wrote, with the lines it carried in
/// them and the field they belong to recorded against the frame that has to
/// share the buffer with them.
pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<FrameBuffer, D::Error> {
    let (width, height, woven): (u32, u32, Option<(bool, Vec<u8>)>) =
        <(u32, u32, Option<(bool, Vec<u8>)>)>::deserialize(de)?;
    let mut frame = FrameBuffer {
        width: width as usize,
        height: height as usize,
        ..FrameBuffer::new()
    };
    if let Some((field, bytes)) = woven {
        restore_carried(&mut frame, field, &bytes).map_err(serde::de::Error::custom)?;
    }
    Ok(frame)
}

/// The lines of the field the frame being drawn leaves alone, as
/// `(field, little-endian pixels)`, and `None` when the picture is not a woven
/// one so there is no such field.
///
/// Those are the lines of `frame` at `2 * line + field` for the `field` the chip
/// is *not* drawing (see [`FrameBuffer::odd_frame`]), as many of them as the
/// published picture holds: rows outside `height` are not published, so a state
/// has no reason to carry them.
fn carried(frame: &FrameBuffer) -> Option<(bool, Vec<u8>)> {
    let field = !frame.odd_frame?;
    let mut bytes = Vec::with_capacity(field_lines(frame) * frame.width * PIXEL_BYTES);
    for line in (usize::from(field)..frame.height).step_by(2) {
        for &pixel in &frame.pixels[line * frame.width..(line + 1) * frame.width] {
            bytes.extend_from_slice(&pixel.to_le_bytes());
        }
    }
    Some((field, bytes))
}

/// Put the lines `bytes` holds back into `frame` as field `field`, and record on
/// the buffer that its own frame is the other one.
fn restore_carried(frame: &mut FrameBuffer, field: bool, bytes: &[u8]) -> Result<(), &'static str> {
    let lines = field_lines(frame);
    if bytes.len() != lines * frame.width * PIXEL_BYTES {
        return Err("a carried field is width * height / 2 pixels of four bytes");
    }
    if 2 * lines * frame.width > frame.pixels.len() {
        return Err("the size of the picture reaches past the frame buffer");
    }
    // The check above is that `bytes` is a whole number of pixels, so the
    // remainder of `as_chunks` is empty and every byte of it lands.
    let (pixels, _) = bytes.as_chunks::<PIXEL_BYTES>();
    for (index, pixel) in pixels.iter().enumerate() {
        // The `index`th pixel of the field is `index / width` field lines down,
        // and a field line is two lines of the published picture.
        let line = index / frame.width;
        let column = index % frame.width;
        frame.pixels[(2 * line + usize::from(field)) * frame.width + column] =
            u32::from_le_bytes(*pixel);
    }
    frame.odd_frame = Some(!field);
    Ok(())
}

/// How many lines one field of a published picture holds: the two fields of a
/// woven picture share its `height` two lines at a time.
fn field_lines(frame: &FrameBuffer) -> usize {
    frame.height / 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::{MAX_HEIGHT, MAX_WIDTH};

    /// The `with` module of `Vdp::frame_buffer` is a module rather than a type, so
    /// the tests put it on a field of their own to get a `Serialize`/`Deserialize`
    /// pair out of it. A newtype struct is transparent to postcard, so this is the
    /// payload of `Vdp`'s frame buffer field and nothing else.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Saved(#[serde(with = "crate::frame_state")] FrameBuffer);

    /// A frame buffer of `width` x `height` whose every pixel says where it is,
    /// woven - or not - as `odd_frame` says.
    fn buffer(width: usize, height: usize, odd_frame: Option<bool>) -> FrameBuffer {
        let mut frame = FrameBuffer::new();
        frame.width = width;
        frame.height = height;
        frame.odd_frame = odd_frame;
        for (index, pixel) in frame.pixels.iter_mut().enumerate() {
            *pixel = index as u32;
        }
        frame
    }

    fn encode(frame: FrameBuffer) -> Vec<u8> {
        postcard::to_allocvec(&Saved(frame)).expect("serialise a frame buffer")
    }

    fn decode(bytes: &[u8]) -> FrameBuffer {
        postcard::from_bytes::<Saved>(bytes)
            .expect("deserialise a frame buffer")
            .0
    }

    #[test]
    fn a_progressive_frame_buffer_still_carries_no_pixels() {
        let plain = buffer(256, 224, None);
        let mut other = buffer(256, 224, None);
        other.pixels[1000] = 0x00FF_00FF;

        let bytes = encode(plain);
        assert_eq!(
            bytes,
            encode(other),
            "a progressive frame buffer is still its size and none of its pixels"
        );
        // Exactly one byte more than the two sizes on their own: the presence tag
        // is what makes the field decodable without knowing the interlace mode, and
        // one byte is what a progressive save pays for it.
        let sizes_only = postcard::to_allocvec(&(256_u32, 224_u32)).expect("serialise sizes");
        assert_eq!(
            bytes.len(),
            sizes_only.len() + 1,
            "a progressive state grows by the presence tag alone"
        );
    }

    #[test]
    fn a_woven_frame_buffer_carries_one_field() {
        // The frame is on the odd field, so it is the even lines of the picture
        // that this frame leaves alone and that have to come along as pixels.
        let field = 256 * 224;
        let bytes = encode(buffer(256, 448, Some(true)));
        assert!(
            bytes.len() > field * 4 && bytes.len() <= field * 4 + 32,
            "a woven state is one field of `u32` plus the sizes, the tag and the parity, \
             and is {} bytes",
            bytes.len()
        );

        let back = decode(&bytes);
        assert_eq!(back.odd_frame, Some(true), "the field it came back on");
        for line in 0..448 {
            for column in (0..256).step_by(29) {
                let at = line * 256 + column;
                let want = if line % 2 == 0 { at as u32 } else { 0 };
                assert_eq!(
                    back.pixels[at], want,
                    "line {line} column {column}: the even lines are the field that was \
                     carried and the odd ones the field being drawn"
                );
            }
        }
    }

    /// The `Option` is what makes the field decodable without knowing the mode, and
    /// a state that lies about how much it carries is an error rather than a panic:
    /// the check on the size is what makes the write in `restore_carried` safe.
    #[test]
    fn a_carried_field_outside_the_buffer_is_refused() {
        let lying = postcard::to_allocvec(&(
            MAX_WIDTH as u32,
            MAX_HEIGHT as u32,
            Some((true, vec![0_u8; 4 * 1024 * 1024])),
        ))
        .expect("serialise a lie");
        assert!(postcard::from_bytes::<Saved>(&lying).is_err());
    }
}
