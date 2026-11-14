//! Serialising [`Vdp::frame_buffer`] as the picture's size rather than its pixels.
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

use serde::{Deserialize, Serialize};
use serde::{Deserializer, Serializer};

use crate::framebuffer::FrameBuffer;

/// The frame buffer as the two numbers that say where its lines start.
pub fn serialize<S: Serializer>(frame: &FrameBuffer, ser: S) -> Result<S::Ok, S::Error> {
    // `u32` rather than `usize` so a state is the same width on every target.
    (frame.width as u32, frame.height as u32).serialize(ser)
}

/// A black buffer of the size `serialize` wrote.
pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<FrameBuffer, D::Error> {
    let (width, height) = <(u32, u32)>::deserialize(de)?;
    Ok(FrameBuffer {
        width: width as usize,
        height: height as usize,
        ..FrameBuffer::new()
    })
}
