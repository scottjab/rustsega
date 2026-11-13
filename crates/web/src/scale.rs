//! The box the picture is drawn in (M7-12, #145).
//!
//! The canvas keeps the VDP's own resolution - 320x224, or 256 wide in H32, or
//! 448 lines in interlace mode 2 - and the shell scales it with CSS. Which box it
//! scales to is arithmetic rather than browser behaviour, so it is worked out
//! here and tested natively: [`display_rect`] takes the room the screen area has
//! and the frame that is being drawn, and says what the canvas's CSS box should
//! be. The browser half is [`crate::screen`].

use serde::Deserialize;
use serde::Serialize;

/// How the frame is scaled into the screen area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleMode {
    /// The largest whole multiple of the frame that fits.
    ///
    /// Every Genesis pixel becomes the same whole number of screen pixels, which
    /// is what `image-rendering: pixelated` is asking for; the price is a
    /// letterbox up to one frame wide that [`ScaleMode::Fit`] would have spent.
    #[default]
    Integer,
    /// The largest box of the right aspect that fits, whole pixels or not.
    Fit,
}

impl ScaleMode {
    /// Every mode, in the order the settings select lists them.
    pub const ALL: [Self; 2] = [Self::Integer, Self::Fit];

    /// The label for this mode in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Integer => "Integer (whole multiple)",
            Self::Fit => "Fit (fill the screen area)",
        }
    }
}

/// What shape the frame's pixels are stretched to.
///
/// The Genesis produces square pixels in a 4:3 field, so the two settings are the
/// two ways a real television and a bitmap viewer differ: [`Aspect::Square`]
/// shows the frame's own pixel grid, and [`Aspect::FourThree`] shows what the
/// console was pointing at a television with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    /// 4:3 whatever the frame's width, so H32 and H40 do not change the box.
    #[default]
    FourThree,
    /// The frame's own pixel ratio, so 256x224 is narrower than 320x224.
    Square,
}

impl Aspect {
    /// Every aspect, in the order the settings select lists them.
    pub const ALL: [Self; 2] = [Self::FourThree, Self::Square];

    /// The label for this aspect in the settings drawer.
    pub fn label(self) -> &'static str {
        match self {
            Self::FourThree => "4:3",
            Self::Square => "Square pixels",
        }
    }
}

/// The highest line count a progressive frame reaches (docs/ARCHITECTURE.md,
/// "vdp": `height` is "224 or 240 (448/480 in interlace mode 2)"). Anything
/// above it is one of the interlace modes, whose two fields share the picture
/// area of the progressive mode they are doubled from.
const PROGRESSIVE_MAX: u32 = 240;

/// The display box, in CSS pixels, for a `frame_w` x `frame_h` frame in a
/// `container_w` x `container_h` box.
///
/// The container is the room the picture has - the screen area's content box, so
/// its padding is already spent - and the answer is the canvas's CSS width and
/// height, which the shell writes as inline styles and `style.css` does not
/// touch. The picture itself is centred by the container, so this only decides
/// the size.
///
/// [`ScaleMode::Integer`] is the largest whole multiple that fits and is never
/// smaller than 1x, so on a container too small for one frame the picture
/// overflows rather than vanishing. A container that has not been measured at
/// all answers `(0.0, 0.0)`, which is the caller's cue to write nothing.
pub fn display_rect(
    container_w: f64,
    container_h: f64,
    frame_w: u32,
    frame_h: u32,
    mode: ScaleMode,
    aspect: Aspect,
) -> (f64, f64) {
    if container_w <= 0.0 || container_h <= 0.0 || frame_w == 0 || frame_h == 0 {
        return (0.0, 0.0);
    }
    let (base_w, base_h) = base_box(frame_w, frame_h, aspect);
    let scale = (container_w / base_w).min(container_h / base_h);
    match mode {
        ScaleMode::Fit => (base_w * scale, base_h * scale),
        // `max(1.0)` is the never-zero rule: a frame that does not fit at all is
        // still drawn once, and clipped by the screen area, rather than at 0x0.
        ScaleMode::Integer => {
            let whole = scale.floor().max(1.0);
            (base_w * whole, base_h * whole)
        }
    }
}

/// The box one frame occupies at 1x: the frame's width, or the 4:3 width its
/// height implies, by [`Aspect`]; and its height, halved for an interlaced frame
/// because the two fields of a 448- or 480-line frame share the picture area of
/// the 224- or 240-line frame they are doubled from (docs/ARCHITECTURE.md,
/// "vdp").
///
/// The 4:3 box is built from the height and not from the width, which is what
/// makes the box independent of the horizontal resolution: an H32 game switching
/// between 256 and 320 keeps the same picture box, and the whole multiple of
/// [`ScaleMode::Integer`] stays a whole number of doubled lines.
fn base_box(frame_w: u32, frame_h: u32, aspect: Aspect) -> (f64, f64) {
    let height = if frame_h > PROGRESSIVE_MAX {
        f64::from(frame_h) / 2.0
    } else {
        f64::from(frame_h)
    };
    match aspect {
        Aspect::Square => (f64::from(frame_w), height),
        Aspect::FourThree => (height * 4.0 / 3.0, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four numbers the ticket gives are the spec, so they are asserted
    /// exactly rather than to within a pixel.
    #[test]
    fn the_tickets_four_numbers() {
        assert_eq!(
            display_rect(1920.0, 1080.0, 320, 224, ScaleMode::Integer, Aspect::Square),
            (1280.0, 896.0)
        );
        assert_eq!(
            display_rect(1920.0, 1080.0, 320, 224, ScaleMode::Fit, Aspect::FourThree),
            (1440.0, 1080.0)
        );
        assert_eq!(
            display_rect(200.0, 150.0, 320, 224, ScaleMode::Integer, Aspect::Square),
            (320.0, 224.0)
        );
        assert_eq!(
            display_rect(1024.0, 768.0, 256, 224, ScaleMode::Fit, Aspect::FourThree),
            (1024.0, 768.0)
        );
    }

    #[test]
    fn integer_mode_falls_back_to_one_and_never_to_zero() {
        // Too small for a single frame at all: still one frame, never nothing.
        for container in [(200.0, 150.0), (1.0, 1.0), (319.0, 223.0)] {
            let (w, h) = display_rect(
                container.0,
                container.1,
                320,
                224,
                ScaleMode::Integer,
                Aspect::Square,
            );
            assert_eq!(
                (w, h),
                (320.0, 224.0),
                "integer mode went below 1x in {container:?}"
            );
        }
    }

    #[test]
    fn an_interlaced_frame_is_scaled_as_half_its_lines() {
        // 448 lines is two 224-line fields, so it takes the same box as 320x224.
        assert_eq!(
            display_rect(1920.0, 1080.0, 320, 448, ScaleMode::Integer, Aspect::Square),
            display_rect(1920.0, 1080.0, 320, 224, ScaleMode::Integer, Aspect::Square)
        );
        assert_eq!(
            display_rect(1024.0, 768.0, 320, 480, ScaleMode::Fit, Aspect::FourThree),
            (1024.0, 768.0)
        );
    }

    #[test]
    fn four_three_does_not_care_about_the_frame_width() {
        for mode in [ScaleMode::Integer, ScaleMode::Fit] {
            assert_eq!(
                display_rect(1920.0, 1080.0, 256, 224, mode, Aspect::FourThree),
                display_rect(1920.0, 1080.0, 320, 224, mode, Aspect::FourThree),
                "4:3 moved the picture between H32 and H40 in {mode:?}"
            );
        }
    }

    #[test]
    fn square_pixels_do_care_about_the_frame_width() {
        // The other half of the rule above: the point of `Square` is that the
        // frame's own ratio is what is shown.
        assert_ne!(
            display_rect(1920.0, 1080.0, 256, 224, ScaleMode::Integer, Aspect::Square),
            display_rect(1920.0, 1080.0, 320, 224, ScaleMode::Integer, Aspect::Square)
        );
    }

    #[test]
    fn integer_mode_is_a_whole_multiple_of_the_frame() {
        for container_w in [640.0, 900.0, 1024.0, 1280.0, 1920.0, 2560.0] {
            for container_h in [360.0, 720.0, 768.0, 1080.0] {
                let (w, _) = display_rect(
                    container_w,
                    container_h,
                    320,
                    224,
                    ScaleMode::Integer,
                    Aspect::Square,
                );
                let multiple = w / 320.0;
                assert_eq!(
                    multiple.fract(),
                    0.0,
                    "{container_w}x{container_h} gave {w}px, not a whole multiple"
                );
            }
        }
    }

    #[test]
    fn fit_never_larger_than_the_container() {
        for mode in [ScaleMode::Integer, ScaleMode::Fit] {
            for aspect in [Aspect::FourThree, Aspect::Square] {
                let (w, h) = display_rect(1200.0, 800.0, 320, 224, mode, aspect);
                assert!(
                    w <= 1200.0 && h <= 800.0,
                    "{mode:?} {aspect:?} overflowed: {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn a_container_with_no_room_yet_has_no_box() {
        // The first `ResizeObserver` report can be a zero box; writing a zero
        // canvas size is worse than writing nothing.
        assert_eq!(
            display_rect(0.0, 0.0, 320, 224, ScaleMode::Fit, Aspect::FourThree),
            (0.0, 0.0)
        );
        assert_eq!(
            display_rect(1000.0, 0.0, 320, 224, ScaleMode::Integer, Aspect::Square),
            (0.0, 0.0)
        );
    }
}
