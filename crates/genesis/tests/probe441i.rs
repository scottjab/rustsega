//! SCRATCH #441 probe i: trail tail at several frame counts.

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
    format!("0x{addr:06x} {text}")
}

#[test]
#[ignore = "scratch"]
fn hardball() {
    let name = "Al Michaels Announces HardBall III (USA, Europe).md";
    let Some(path) = testkit::commercial_rom(name) else {
        eprintln!("SKIPPED");
        return;
    };
    let rom = std::fs::read(&path).expect("rom");
    let mut machine = Genesis::new(&rom, Config::default()).expect("load");
    machine.arm_cpu_trail(Some(0x45312));
    let mut done = 0;
    for mark in [58u32, 59, 60, 61, 62, 70, 120] {
        while done < mark {
            machine.run_frame();
            done += 1;
        }
        let rom_image = &machine.bus().rom;
        let trail = machine.cpu().trail().expect("armed");
        let entries = trail.entries();
        println!("#### frame {mark}: pc={:08X} stopped={} cycles={} entries={}",
            machine.cpu().pc, machine.cpu().stopped, machine.cpu().cycles, entries.len());
        for entry in entries.iter().rev().take(24).rev() {
            println!("  {}", trail_line(rom_image, *entry));
        }
    }
}
