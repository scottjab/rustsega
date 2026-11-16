//! SCRATCH #441 probe d: how does VRAM/frame evolve, and what do the raw bytes look like?

use genesis::Config;
use genesis::Genesis;
use vdp::Plane;
use vdp::WIDTH_H40;

fn hexdump(v: &vdp::Vdp, addr: usize, len: usize) -> String {
    let mut s = String::new();
    for (i, b) in v.vram[addr..addr + len].iter().enumerate() {
        s.push_str(&format!("{b:02X}"));
        if i % 2 == 1 {
            s.push('_');
        }
        if i % 32 == 31 {
            s.push('\n');
        } else {
            s.push(' ');
        }
    }
    s
}

fn occupancy(v: &vdp::Vdp) -> String {
    let mut occ = String::new();
    for blk in 0..64usize {
        let n = v.vram[blk * 1024..][..1024]
            .iter()
            .filter(|b| **b != 0)
            .count();
        if n > 0 {
            occ.push_str(&format!("{:04X}:{:03} ", blk * 1024, n));
        }
    }
    occ
}

fn run(name: &str, marks: &[u32]) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    let mut done = 0u32;
    println!("#### {name}");
    for &mark in marks {
        while done < mark {
            machine.run_frame();
            done += 1;
        }
        let v = &machine.bus().vdp;
        let total: usize = v.vram.iter().filter(|b| **b != 0).count();
        let cram: usize = v.cram.iter().filter(|w| **w != 0).count();
        let pic = machine.frame();
        let mut colours: Vec<u32> = pic.pixels[..pic.width * pic.height].to_vec();
        colours.sort_unstable();
        colours.dedup();
        let colours = colours.len();
        let mut rows = 0usize;
        for row in 0..28 {
            let mut out = [0u8; WIDTH_H40];
            v.render_plane_line(Plane::A, (row * 8) as u16, &mut out);
            if out.iter().any(|p| *p != 0) {
                rows += 1;
            }
        }
        let pc = machine.cpu().pc;
        let r1 = v.reg(1);
        let r2 = v.reg(2);
        let r16 = v.reg(16);
        println!(
            "frame {mark:>5} vram_nonzero={total:<6} cram_nonzero={cram:<3} colours={colours:<3} planeA_rows={rows:<3} pc={pc:08X} R01={r1:02X} R02={r2:02X} R16={r16:02X}"
        );
        println!("  occ: {}", occupancy(v));
    }
}

#[test]
#[ignore = "scratch"]
fn growth() {
    run(
        "Al Michaels Announces HardBall III (USA, Europe).md",
        &[1, 30, 60, 120, 300, 600, 900, 1200, 1800, 2400],
    );
    run(
        "Janou Touryuumon (Japan).md",
        &[1, 30, 60, 120, 300, 600, 900, 1200, 1800, 2400],
    );
}

#[test]
#[ignore = "scratch"]
fn bytes() {
    let Some(path) = testkit::commercial_rom("Al Michaels Announces HardBall III (USA, Europe).md")
    else {
        eprintln!("SKIPPED");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    for _ in 0..600 {
        machine.run_frame();
    }
    let v = &machine.bus().vdp;
    println!("vsram: {:?}", &v.vsram[..]);
    for addr in [0x0000usize, 0x1800, 0x1880, 0x3A00, 0x3A80, 0xA080, 0xA100, 0xA180] {
        println!("--- bytes @ {addr:#06X}\n{}", hexdump(v, addr, 64));
    }
}
