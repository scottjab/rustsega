//! The GLSL ES 3.00 sources the WebGL path compiles (M7-13, #146).
//!
//! Three of them, in Rust `const`s and no external files: one vertex shader for
//! the fullscreen quad and two fragment shaders - [`SHARP`], the default, and
//! [`CRT`]. Keeping them here rather than in `webgl.rs` is what lets a native
//! test read them, which is worth something because a shader that will not
//! compile is a black screen rather than an exception: it is reported by
//! `get_shader_info_log` from inside the animation frame, where a panic would
//! leave the console borrowed for good (#387).

/// Every source, with the name a compile failure is reported under.
pub const SOURCES: [(&str, &str); 3] = [("vertex", VERTEX), ("sharp", SHARP), ("crt", CRT)];

/// The fullscreen quad both fragment shaders are drawn on.
///
/// `u_uv` is the rectangle the current frame fills inside the frame texture, as
/// [`crate::render::uv_rect`] reports it - `[left, top, right, bottom]`. The
/// quad's own y runs bottom to top while an uploaded image runs top to bottom,
/// so the y mix is the other way round to the x one: the top of the screen has
/// to sample the first row the VDP wrote.
pub const VERTEX: &str = r#"#version 300 es
precision highp float;

uniform vec4 u_uv;
in vec2 a_position;
out vec2 v_uv;

void main() {
  vec2 corner = (a_position + 1.0) * 0.5;
  v_uv = vec2(mix(u_uv.x, u_uv.z, corner.x), mix(u_uv.w, u_uv.y, corner.y));
  gl_Position = vec4(a_position, 0.0, 1.0);
}
"#;

/// The default filter: the frame as the VDP made it, one texel per output pixel.
///
/// The sampling itself is the texture's - point sampling, or linear while the
/// `Smoothing` setting of #145 is on - so this shader has nothing to do but ask
/// for the texel, which is what makes WebGL sharp mode the same picture the 2D
/// path draws.
pub const SHARP: &str = r#"#version 300 es
precision highp float;

uniform sampler2D u_image;
in vec2 v_uv;
out vec4 out_colour;

void main() {
  out_colour = texture(u_image, v_uv);
}
"#;

/// A gentle CRT: scanlines at the source line rate, a little bloom around bright
/// edges, and a faint aperture mask.
///
/// Three deliberate choices beyond the ticket's list:
///
/// * The scanline carrier is tied to `u_source`, the frame's own line count, not
///   to the output's. A screen-space carrier would give a 1080p television one
///   band per band of the panel and a 4K one four times as many - the same game
///   would look different at two sizes of the same window.
/// * Bloom only ever brightens, so a dark picture stays dark instead of turning
///   to grey.
/// * No curvature. The shell has already scaled the picture to fit the screen
///   area (M7-12, #145), and a curve would move pixels a player is aiming at.
pub const CRT: &str = r#"#version 300 es
precision highp float;

const float TAU = 6.283185307179586;
const float SCANLINE = 0.15; // how far the join between two source lines darkens
const float BLOOM = 0.18;    // how much a bright edge lights its neighbours
const float MASK = 0.05;     // aperture mask depth, 0 is off

uniform sampler2D u_image;
uniform vec4 u_uv;
uniform vec2 u_source;

in vec2 v_uv;
out vec4 out_colour;

// Sampled inside the frame's own rectangle: a tap that stepped outside it would
// read texture the VDP has never written, which is the previous frame's junk.
vec3 frame_at(vec2 uv) {
  return texture(u_image, clamp(uv, u_uv.xy, u_uv.zw)).rgb;
}

void main() {
  vec3 base = frame_at(v_uv);
  vec2 tap = 1.0 / max(u_source, vec2(1.0));
  vec3 around = (frame_at(v_uv + vec2(tap.x, 0.0))
               + frame_at(v_uv - vec2(tap.x, 0.0))
               + frame_at(v_uv + vec2(0.0, tap.y))
               + frame_at(v_uv - vec2(0.0, tap.y))) * 0.25;

  vec3 colour = base + BLOOM * max(around - base, vec3(0.0));

  // One beam per source line: full in the middle of a line, darkest at the join.
  float line = v_uv.y * u_source.y;
  float beam = 1.0 - abs(fract(line) - 0.5) * 2.0;
  colour *= mix(1.0 - SCANLINE, 1.0, beam);

  // Three device pixels to a phosphor triad, at a twentieth of full swing:
  // visible across a flat field, not enough to read as a grid over a game.
  float phase = TAU * floor(gl_FragCoord.x) / 3.0;
  vec3 phosphor = vec3(cos(phase), cos(phase + TAU / 3.0), cos(phase + 2.0 * TAU / 3.0));
  colour *= 1.0 - MASK * (0.5 + 0.5 * phosphor);

  out_colour = vec4(colour, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// GLSL ES 3.00 is what a WebGL2 context compiles, and it is picky about the
    /// two things this test checks: the `#version` line must be the first line of
    /// the source, and a fragment shader has no default float precision, so it
    /// has to declare one. A source that fails either is a shader that fails to
    /// compile at run time - a black screen, and no native test of its own.
    #[test]
    fn every_shader_is_glsl_es_300_with_a_declared_precision() {
        for (name, source) in SOURCES {
            assert!(
                source.starts_with("#version 300 es\n"),
                "{name} does not start with the GLSL ES 3.00 version line: {source}"
            );
            assert!(
                source.contains("precision "),
                "{name} declares no float precision"
            );
        }
    }

    /// The seam between the vertex shader and both fragment shaders: one varying
    /// spelled the same way on each side. A mismatch is a program that links and
    /// draws nothing.
    #[test]
    fn the_vertex_shader_hands_the_fragment_shaders_the_same_varying() {
        assert!(
            VERTEX.contains("out vec2 v_uv"),
            "VERTEX declares no varying"
        );
        for (name, source) in SOURCES {
            if name == "vertex" {
                continue;
            }
            assert!(source.contains("in vec2 v_uv"), "{name} takes no varying");
        }
    }

    /// Both fragment shaders sample the one frame texture, and the CRT one needs
    /// the frame's own size as well - that is what ties its scanlines to source
    /// lines rather than to screen rows.
    #[test]
    fn both_fragment_shaders_sample_the_frame_texture() {
        for (name, source) in SOURCES {
            if name == "vertex" {
                continue;
            }
            assert!(
                source.contains("uniform sampler2D u_image"),
                "{name} samples no texture"
            );
        }
        let (_, crt) = SOURCES[2];
        assert!(
            crt.contains("uniform vec2 u_source"),
            "CRT scanlines float free"
        );
    }
}
