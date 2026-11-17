//! SCRATCH #441 probe k: disassemble a range of the loaded cart ROM.
use m68k::disassemble;

struct Rom(Vec<u8>);

impl Rom {
    fn dump(&self, start: u32, count: usize) {
        println!("---- ${start:06X} ----");
        let start = (start & !1) as usize;
        let end = self.0.len().min(start + 128);
        let words: Vec<u16> = self.0[start..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect();
        let mut a = start as u32;
        for _ in 0..count {
            let i = ((a as usize) - start) / 2;
            let (text, size) = disassemble(&words[i..], a);
            println!("  {a:06X} {text}");
            a += size.max(2) as u32;
        }
    }
}

fn rom(name: &str) -> Option<Rom> {
    let path = testkit::commercial_rom(name)?;
    let bytes = std::fs::read(&path).expect("rom");
    let machine = genesis::Genesis::new(&bytes, genesis::Config::default()).expect("load");
    Some(Rom(machine.bus().rom.clone()))
}

const HB: &str = "Al Michaels Announces HardBall III (USA, Europe).md";

#[test]
#[ignore = "scratch"]
fn anchors() {
    let Some(r) = rom(HB) else { return };
    for a in [
        0x40E0D_u32,
        0x40EE8,
        0x40EF8,
        0x40F00,
        0x40F94,
        0x40FB8,
        0x40FC6,
        0x476C0,
        0x4771C,
    ] {
        r.dump(a, 12);
    }
}
