//! SCRATCH #441 probe c: raw VRAM layout, name tables with the R16 stride, tile bytes.

use genesis::Config;
use genesis::Genesis;
use vdp::Plane;
use vdp::WIDTH_H40;

fn hexdump(v: &vdp::Vdp, addr: usize, len: usize) -> String {
    let mut s = String::new();
    for (i, b) in v.vram[addr..addr + len].iter().enumerate() {
        s.push_str(&format!("{b:02X}"));
        if i % 2 == 1 {
            s.push(' ');
        }
        if i % 32 == 15 {
            s.push('\n');
        }
    }
    s
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
    println!("==== {name} frame {frames} pc={:08X} ====", machine.cpu().pc);
    let regs: Vec<String> = (0..=0x17u8)
        .map(|r| format!("R{r:02X}={:02X}", v.reg(r)))
        .collect();
    println!("{}", regs.chunks(8).map(|c| c.join(" ")).collect::<Vec<_>>().join("\n"));
    let wcells = usize::from(v.plane_width_cells());
    println!(
        "planeA={:04X} planeB={:04X} window={:04X} sprite={:04X} hscroll_t={:04X} wcells={} hcells={}",
        v.plane_a_base(),
        v.plane_b_base(),
        v.window_base(),
        v.sprite_base(),
        v.hscroll_base(),
        wcells,
        usize::from(v.plane_height_cells()),
    );

    // Where is there data?
    let mut occ = String::new();
    for blk in 0..64usize {
        let n = v.vram[blk * 1024..][..1024]
            .iter()
            .filter(|b| **b != 0)
            .count();
        if n > 0 {
            occ.push_str(&format!("{:04X}:{}/1024 ", blk * 1024, n));
        }
    }
    println!("vram occupancy: {occ}");

    for (base, tag) in [
        (usize::from(v.plane_a_base()), 'A'),
        (usize::from(v.plane_b_base()), 'B'),
    ] {
        println!("--- name table {tag} @ {base:#06X}, stride {wcells} cells ---");
        for row in 0..8usize {
            let mut line = String::new();
            for cell in 0..wcells.min(64) {
                let idx = row * wcells + cell;
                let a = (base + idx * 2) & 0xFFFE;
                let w = ((v.vram[a] as usize) << 8) | v.vram[a ^ 1] as usize;
                line.push_str(&format!("{w:04X} "));
            }
            println!("r{row}: {line}");
        }
    }

    // A sample of raw bytes at each table base and at pattern base 0.
    for addr in [0usize, 0x1000, 0x2000, 0x4000, 0x8000, 0xA000, 0xB000] {
        println!("--- bytes @ {addr:#06X}\n{}", hexdump(v, addr, 64));
    }

    for plane in [Plane::A, Plane::B] {
        println!("--- plane {plane:?} render: tile-ish map (nonzero pixels per cell) ---");
        for row in 0..28usize {
            let mut out = [0u8; WIDTH_H40];
            v.render_plane_line(plane, (row * 8) as u16, &mut out);
            let mut line = String::new();
            for cell in 0..40 {
                let n = out[cell * 8..cell * 8 + 8].iter().filter(|p| **p != 0).count();
                line.push_str(&format!("{:02X} ", n));
            }
            println!("r{row:02}: {line}");
        }
    }
}

#[test]
#[ignore = "scratch"]
fn probe() {
    dump("Al Michaels Announces HardBall III (USA, Europe).md", 600);
    dump("Janou Touryuumon (Japan).md", 600);
}
