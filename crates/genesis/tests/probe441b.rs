//! SCRATCH #441 probe b: where in VRAM did the data go, and what do the name tables say?

use genesis::Config;
use genesis::Genesis;
use vdp::Plane;
use vdp::WIDTH_H40;

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
    println!("==== {name} frame {frames} pc={:08X} ====", machine.cpu().pc);
    let regs: Vec<String> = (0..=0x17u8)
        .map(|r| format!("R{r:02X}={:02X}", v.reg(r)))
        .collect();
    println!(
        "{}",
        regs.chunks(8)
            .map(|c| c.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!(
        "planeA={:04X} planeB={:04X} window={:04X} sprite={:04X} hscroll_t={:04X}",
        v.plane_a_base(),
        v.plane_b_base(),
        v.window_base(),
        v.sprite_base(),
        v.hscroll_base(),
    );

    // VRAM occupancy: non-zero bytes per 1 KiB block, only the non-empty blocks.
    let mut occ = String::new();
    for blk in 0..64usize {
        let n = v.vram[blk * 1024..][..1024]
            .iter()
            .filter(|b| **b != 0)
            .count();
        if n > 0 {
            occ.push_str(&format!("{}K{n} ", blk * 1024, n = n * 100 / 1024));
        }
    }
    println!("vram occupancy (address, % nonzero): {occ}");

    // CRAM: 4 palette lines of 16 words.
    for p in 0..4usize {
        let words: Vec<String> = (0..16)
            .map(|i| format!("{:04X}", v.cram[p * 16 + i] & 0x1FF))
            .collect();
        println!("cram p{p}: {}", words.join(" "));
    }

    // Name table: first 8 cell rows, entry decodes (tile.pal) for 32 cells.
    for (base, tag) in [
        (v.plane_a_base() as usize, "A"),
        (v.plane_b_base() as usize, "B"),
    ] {
        println!("name table {tag} @ {base:#06X}");
        for row in 0..6usize {
            let mut line = String::new();
            for cell in 0..40usize {
                let a = (base + (row * 40 + cell) * 2) & 0xFFFE;
                let w = u16::from_be_bytes([v.vram[a], v.vram[a ^ 1]]);
                line.push_str(&format!("{:04X} ", w));
            }
            println!("  r{row}: {line}");
        }
    }

    // Plane renders as ASCII: one char per cell, '.' if all-transparent.
    for plane in [Plane::A, Plane::B] {
        println!("plane {plane:?} cells:");
        for row in 0..28usize {
            let mut out = [0u8; WIDTH_H40];
            v.render_plane_line(plane, (row * 8) as u16, &mut out);
            let mut line = String::new();
            for cell in 0..40 {
                let n = out[cell * 8..cell * 8 + 8].iter().filter(|p| **p != 0).count();
                line.push(if n == 0 { '.' } else { '#' });
            }
            println!("  {line}");
        }
    }
}

#[test]
#[ignore = "scratch"]
fn probe() {
    dump("Janou Touryuumon (Japan).md", 600);
    dump("Al Michaels Announces HardBall III (USA, Europe).md", 600);
}
