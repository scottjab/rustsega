//! Hardware-captured conformance for the undocumented `IXH`/`IXL`/`IYH`/`IYL`
//! registers (M2-15, #58).
//!
//! These are the files that prove the two rules the index halves live or die on:
//!
//! * with `DD`/`FD` in front, the `H` and `L` slots of an 8-bit instruction are
//!   the halves of the index pair, and the real `H`/`L` are left alone;
//! * the exception is an instruction whose other operand is `(IX+d)`, where the
//!   `H`/`L` slot stays the real `H`/`L` (`dd 66` is `LD H,(IX + d)`, and it is
//!   covered by the loads' own sweep, not this one).
//!
//! The opcode lists are generated from the x/y/z fields rather than typed out,
//! because the point of the group is that it falls out of the encoding: `y` and
//! `z` are the register slots in `LD r,r'` and `INC`/`DEC r`/`LD r,n`, and `z`
//! alone is the operand in the `ALU A,r` block.

mod harness;

/// Which of the 3-bit register slots a prefix can rename: `H` and `L`.
fn is_half(slot: u8) -> bool {
    matches!(slot, 4 | 5)
}

/// The `LD r,r'` opcodes (`40`-`7F`) where the prefix has something to rename.
///
/// Both `(IX+d)` cases are excluded: `z = 6` is `LD r,(IX + d)` and `y = 6` is
/// `LD (IX + d),r`, and in those the plain register operand is the real `H`/`L`.
fn ld_rr_half() -> Vec<u8> {
    (0x40..=0x7F)
        .filter(|op| {
            let (y, z) = ((op >> 3) & 7, op & 7);
            y != 6 && z != 6 && (is_half(y) || is_half(z))
        })
        .collect()
}

/// `INC`/`DEC` and `LD r,n` for the halves: `24`/`2C` and `25`/`2D` are
/// `INC`/`DEC`, `26`/`2E` are `LD r,n`, all with `y` naming `H` or `L`.
const LD_INC_DEC_HALF: [u8; 6] = [0x24, 0x25, 0x26, 0x2c, 0x2d, 0x2e];

/// `ALU A,IXH` / `ALU A,IXL`: `y` is the operation here, so the whole block is
/// the eight operations crossed with `z = 4` and `z = 5`.
fn alu_half() -> Vec<u8> {
    (0x80..=0xBF).filter(|op| is_half(op & 7)).collect()
}

/// Every opcode file this ticket owns, for both prefixes.
fn opcode_files() -> Vec<String> {
    let mut names = Vec::new();
    for prefix in ["dd", "fd"] {
        for op in ld_rr_half() {
            names.push(format!("{prefix} {op:02x}"));
        }
        for op in LD_INC_DEC_HALF {
            names.push(format!("{prefix} {op:02x}"));
        }
        for op in alu_half() {
            names.push(format!("{prefix} {op:02x}"));
        }
    }
    names
}

/// The whole undocumented-half sweep: `LD r,r'`, `LD r,n`, `INC`/`DEC r` and
/// `ALU A,r` in both index spaces.
#[test]
fn sst_index_halves() {
    let names = opcode_files();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    harness::run_files(&names);
}

/// The two rules of the group, spelled out as counts so a change to
/// [`ld_rr_half`] that quietly starts or stops covering a file shows up here.
#[test]
fn opcode_lists_cover_the_half_register_space() {
    // 24 per prefix: the 16 that name a half on both sides plus the 8 in the
    // `LD H,*` / `LD L,*` rows, with `76` (`HALT` in this space) and both
    // `(IX+d)` directions excluded.
    assert_eq!(ld_rr_half().len(), 24);
    assert!(!ld_rr_half().contains(&0x76), "DD 76 is HALT, not LD H,H");
    assert!(
        !ld_rr_half().contains(&0x66),
        "LD H,(IX + d) keeps the real H"
    );
    assert!(
        !ld_rr_half().contains(&0x74),
        "LD (IX + d),H keeps the real H"
    );
    assert_eq!(alu_half().len(), 16);
    assert_eq!(opcode_files().len(), 2 * (24 + 6 + 16));
}
