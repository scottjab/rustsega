//! SCRATCH #441 probe f: is the VINT being offered to a stopped 68000?

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
        let bus = machine.bus();
        println!(
            "f{} pc={:08X} sr={:04X} stopped={} ipl={} locked_up={} z80_can_run={} pending={:?} status={:04X}",
            warmup + i,
            c.pc,
            c.sr,
            c.stopped,
            c.ipl,
            bus.locked_up(),
            bus.z80_can_run(),
            bus.vdp.pending_irq(),
            // read_status has the side effect of clearing bit 0, so don't call it.
            0,
        );
        machine.run_frame();
    }
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 60, 8);
}

#[test]
#[ignore = "scratch"]
fn hardball_early() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 20, 40);
}
