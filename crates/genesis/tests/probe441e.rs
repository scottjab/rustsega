//! SCRATCH #441 probe e: is the 68000 actually running?

use genesis::Config;
use genesis::Genesis;

fn run(name: &str, warmup: u32, frames: u32) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    for _ in 0..warmup {
        machine.run_frame();
    }
    println!("#### {name} after {warmup} frames");
    for i in 0..frames {
        let c = machine.cpu();
        println!(
            "f{} pc={:08X} sr={:04X} stopped={} halted={} d0={:08X} d1={:08X} d2={:08X} a0={:08X} a1={:08X} usp/a7={:08X}",
            warmup + i,
            c.pc,
            c.sr,
            c.stopped,
            c.halted,
            c.d[0],
            c.d[1],
            c.d[2],
            c.a[0],
            c.a[1],
            c.a[7],
        );
        machine.run_frame();
    }
    let c = machine.cpu();
    let pc = c.pc;
    // The instruction bytes at pc and the register state, straight out of the ROM.
    let base = (pc as usize) & 0x3F_FFFF;
    println!("rom bytes at {pc:08X}: {:02X?}", &rom[base..base + 24]);
    println!("d: {:08X?}", c.d);
    println!("a: {:08X?}", c.a);
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 62, 6);
}

#[test]
#[ignore = "scratch"]
fn janou() {
    run("Janou Touryuumon (Japan).md", 122, 6);
}
