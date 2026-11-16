//! SCRATCH #441 probe g: is `M68k::step` being called at all?

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
            "f{} pc={:08X} cycles={} stopped={} ipl={}",
            warmup + i,
            c.pc,
            c.cycles,
            c.stopped,
            c.ipl,
        );
        machine.run_frame();
    }
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 60, 4);
}

#[test]
#[ignore = "scratch"]
fn janou() {
    run("Janou Touryuumon (Japan).md", 130, 4);
}
