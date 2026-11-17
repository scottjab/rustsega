//! SCRATCH #441 probe l: is the Z80 running, and is it being reset?
use genesis::Genesis;
use genesis::Config;

fn run(name: &str, frames: u32) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    println!("#### {name}");
    let mut last = 0xFFFF_u16;
    for f in 0..frames {
        machine.run_frame();
        let z = machine.z80();
        let mark = if z.pc() == last { " " } else { "*" };
        println!(
            "f{f:3} z80pc={:04X} sp={:04X} af={:04X} iff1={} im={:?} halted={} busycan={} {}",
            z.pc(),
            z.sp(),
            z.af(),
            z.iff1(),
            z.im(),
            z.halted(),
            machine.bus().z80_can_run(),
            mark
        );
        last = z.pc();
    }
}

#[test]
#[ignore = "scratch"]
fn z80hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 70);
}

#[test]
#[ignore = "scratch"]
fn z80janou() {
    run("Janou Touryuumon (Japan).md", 70);
}
