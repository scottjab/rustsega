//! SCRATCH #441 probe m: is the Z80 running cart code, or a loop over un-loaded
//! sound RAM?
//!
//! Not for merge. The Z80 map has no ROM at all below 0x4000 - 0x0000-0x3FFF is
//! the 8 KiB of sound RAM mirrored - so "the Z80 sits at $0003" can only mean
//! "it is executing sound RAM". This probe says what is in that RAM, when it got
//! written, by whom, and whether the chip is being stepped at all.
//!
//! Per frame: the Z80 pc/sp, the number of Z80 slices the scheduler actually
//! stepped, `z80_can_run()`, and the 16 bytes at Z80 $0000 (with a `*` on the
//! frames where they changed). Then the low-page access histogram, the low-page
//! write log (both CPUs), and the 68000 writes to the Z80 area that were dropped
//! for want of a bus grant.

use genesis::bus::scratch441;
use genesis::Config;
use genesis::Genesis;

fn run(name: &str, frames: u32) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    scratch441::reset();
    scratch441::set_on(true);
    println!("#### {name}: {frames} frames");
    let mut last: Vec<u8> = Vec::new();
    for f in 0..frames {
        scratch441::set_frame(f);
        machine.run_frame();
        let steps = scratch441::z80_steps_take();
        let (nw, lo, hi) = scratch441::z80_writes_take();
        let z = machine.z80();
        let ram: Vec<u8> = machine.bus().z80.ram[0..16].to_vec();
        let mark = if ram == last { " " } else { "*" };
        let quiet = f % 10 != 0 && f > 80;
        let hex: Vec<String> = ram.iter().map(|b| format!("{b:02X}")).collect();
        if quiet {
            continue;
        }
        let cramnz = machine
            .bus()
            .vdp
            .cram
            .iter()
            .filter(|c| **c != 0)
            .count();
        println!(
            "f{f:3} cramnz={cramnz:2} z80pc={:04X} sp={:04X} steps={steps:6} can={} iff1={} im={:?} halt={} 68kwr={nw:5} [{lo:04X}..{hi:04X}] {mark} {}",
            z.pc(),
            z.sp(),
            machine.bus().z80_can_run(),
            z.iff1(),
            z.im(),
            z.halted(),
            hex.join(" ")
        );
        last = ram;
    }
    scratch441::set_on(false);
    scratch441::z80_dump();
}

#[test]
#[ignore = "scratch"]
fn zlowhardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 200);
}

#[test]
#[ignore = "scratch"]
fn zlowjanou() {
    run("Janou Touryuumon (Japan).md", 200);
}
