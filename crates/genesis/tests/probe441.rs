//! SCRATCH #441 probe: dump the VDP register state of the two carts that load CRAM,
//! enable the display and still draw black.

use genesis::Config;
use genesis::Genesis;
use std::collections::HashMap;
use vdp::Plane;
use vdp::WIDTH_H40;

fn hist(px: &[u32]) -> Vec<(u32, usize)> {
    let mut h: HashMap<u32, usize> = HashMap::new();
    for p in px {
        *h.entry(*p).or_insert(0) += 1;
    }
    let mut h: Vec<(u32, usize)> = h.into_iter().collect();
    h.sort_by_key(|(c, n)| (std::cmp::Reverse(*n), *c));
    h
}

fn indices(out: &[u8; WIDTH_H40]) -> Vec<(u8, usize)> {
    let mut h: HashMap<u8, usize> = HashMap::new();
    for p in out {
        if *p != 0 {
            *h.entry(*p & 0x3F).or_insert(0) += 1;
        }
    }
    let mut h: Vec<(u8, usize)> = h.into_iter().collect();
    h.sort();
    h
}

fn dump(name: &str, frames: u32) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    for _ in 0..frames {
        machine.run_frame();
    }
    let v = &machine.bus().vdp;
    println!("==== {name} frame {frames} ====");
    let regs: Vec<String> = (0..=0x17u8)
        .map(|r| format!("R{r:02}={:02X}", v.reg(r)))
        .collect();
    println!(
        "{}",
        regs.chunks(8)
            .map(|c| c.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!(
        "display={} h40={} v30={} mode5={} shadow={} pal_select={} lcb={:?} hscroll={:?}",
        v.display_enabled(),
        v.h40(),
        v.v30_mode(),
        v.mode5_selected(),
        v.shadow_highlight_enabled(),
        v.palette_select(),
        v.left_column_blank(),
        v.hscroll_mode(),
    );
    println!(
        "planeA={:06X} planeB={:06X} window={:06X} sprite={:05X} hscroll_t={:06X} wcells={} wrows={} awidth={} aheight={} interlace={:?}",
        v.plane_a_base(),
        v.plane_b_base(),
        v.window_base(),
        v.sprite_base(),
        v.hscroll_base(),
        v.plane_width_cells(),
        v.plane_height_cells(),
        v.active_width(),
        v.active_height(),
        v.interlace_mode(),
    );
    println!(
        "window right={} whp={} down={} wvp={} backdrop p={} i={}",
        v.window_right(),
        v.window_h_position(),
        v.window_down(),
        v.window_v_position(),
        v.backdrop_palette(),
        v.backdrop_index()
    );
    println!("pc={:08X}", machine.cpu().pc);
    let pic = machine.frame();
    let px = &pic.pixels[..pic.width * pic.height].to_vec();
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-output/probe441");
    let _ = std::fs::create_dir_all(&out);
    let slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let short = if slug.starts_with("Janou") {
        "janou"
    } else if slug.starts_with("Al_") {
        "hardball"
    } else {
        "quackshot"
    };
    testkit::dump_png(
        &out.join(format!("{short}_frame.png")),
        px,
        pic.width,
        pic.height,
    )
    .unwrap();
    let cram = vdp::debug::palette(v);
    testkit::dump_png(
        &out.join(format!("{short}_cram.png")),
        &cram.pixels,
        cram.width,
        cram.height,
    )
    .unwrap();
    for p in 0..4u8 {
        let img = vdp::debug::tiles(v, p);
        testkit::dump_png(
            &out.join(format!("{short}_tiles{p}.png")),
            &img.pixels,
            img.width,
            img.height,
        )
        .unwrap();
    }
    for (view, tag) in [
        (vdp::debug::PlaneView::A, "pa"),
        (vdp::debug::PlaneView::B, "pb"),
        (vdp::debug::PlaneView::Window, "pw"),
    ] {
        let img = vdp::debug::plane(v, view);
        testkit::dump_png(
            &out.join(format!("{short}_plane{tag}.png")),
            &img.pixels,
            img.width,
            img.height,
        )
        .unwrap();
    }

    let fh = hist(px);
    println!("frame {}x{} colours={:X?}", pic.width, pic.height, fh);

    // Per-layer view of a few lines: which layer carries the visible pixels?
    let back = fh[0].0;
    for line in [0u16, 22, 40, 60, 82, 100, 130, 160, 200] {
        let mut a = [0u8; WIDTH_H40];
        let mut b = [0u8; WIDTH_H40];
        v.render_plane_line(Plane::A, line, &mut a);
        v.render_plane_line(Plane::B, line, &mut b);
        let frame_row: Vec<u32> = px[line as usize * pic.width..][..pic.width].to_vec();
        let visible = frame_row.iter().filter(|p| **p != back).count();
        println!(
            "line {line:>3}: A={:?} B={:?} frame-visible={visible}",
            indices(&a),
            indices(&b),
        );
    }
}

#[test]
#[ignore = "scratch"]
fn probe() {
    dump("Janou Touryuumon (Japan).md", 600);
    dump("Al Michaels Announces HardBall III (USA, Europe).md", 600);
    dump("QuackShot Starring Donald Duck (World) (En,Ja).md", 600);
}
