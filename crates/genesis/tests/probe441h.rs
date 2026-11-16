//! SCRATCH #441 probe h: what is the 68000 actually executing?

use genesis::Config;
use genesis::Genesis;

fn trail_line(rom: &[u8], addr: u32) -> String {
    let start = addr as usize;
    if start + 2 > rom.len() {
        return format!("0x{addr:06x}");
    }
    let end = rom.len().min(start + 8);
    let words: Vec<u16> = rom[start..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    let (text, _len) = m68k::disassemble(&words, addr);
    format!("0x{addr:06x}  {text}")
}

fn run(name: &str, frames: u32, tail: usize) {
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED {name}");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    machine.arm_cpu_trail(None);
    for _ in 0..frames {
        machine.run_frame();
    }
    let rom_image = &machine.bus().rom;
    let trail = machine.cpu().trail().expect("armed");
    let entries = trail.entries();
    println!("#### {name} after {frames} frames: {} entries", entries.len());
    for entry in entries.iter().rev().take(tail).rev() {
        println!("{}", trail_line(rom_image, *entry));
    }
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    run("Al Michaels Announces HardBall III (USA, Europe).md", 200, 120);
}
