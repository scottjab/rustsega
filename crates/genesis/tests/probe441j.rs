//! SCRATCH #441 probe j: who is supposed to make $FFFF1B78 non-zero?
//!
//! Not for merge. Logs 68000 accesses to work RAM $FFFF1B70-$FFFF1B7F and
//! $FFFF19D0-$FFFF19DF with the pc, frames 0..N, and prints one row per
//! (op, pc, address) with the frame range and whether any value was non-zero.

use genesis::Genesis;
use genesis::Config;
use genesis::bus::scratch441;

fn run(name: &str, frames: u32) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    scratch441::reset();
    scratch441::set_on(true);
    for f in 0..frames {
        scratch441::set_frame(f);
        machine.run_frame();
    }
    scratch441::set_on(false);
    println!("#### {name} after {frames} frames");
    scratch441::log_dump();
    scratch441::vhist_dump();
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 61);
}

#[test]
#[ignore = "scratch"]
fn janou() {
    run("Janou Touryuumon (Japan).md", 121);
}
