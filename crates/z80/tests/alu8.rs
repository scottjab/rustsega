//! The 8-bit ALU and INC/DEC slice of the Z80 SingleStepTests suite (M2-06, #49).
//!
//! Same deal as the load tickets: 1000 hardware-captured cases per opcode, with
//! random register and memory state, and the suite checks every visible and
//! hidden register (including `wz` and the undocumented `q`), the RAM bytes the
//! instruction wrote, and the T-state count. For this group that is the whole
//! point: `q` must equal the flag byte the instruction computed, and `CP` must
//! take X/Y from its operand rather than from the result it throws away.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `ADD`/`ADC`/`SUB`/`SBC`/`AND`/`XOR`/`OR`/`CP` against the accumulator:
/// `80`-`BF` for the register and `(HL)` operands, `C6 CE D6 DE E6 EE F6 FE`
/// for immediates, and the `(IX+d)` / `(IY+d)` operands for both prefixes. Plus
/// 8-bit `INC`/`DEC` in the same three operand shapes: `04`-`3C`, `34`/`35` and
/// the `DD`/`FD` `34`/`35`.
///
/// The indexed list is the six opcodes whose `z` field is `6`, which is the only
/// `(IX+d)` ALU encoding that exists; the other `DD 80`-`DD BF` opcodes are the
/// undocumented `IXH`/`IXL` forms and belong to M2-15 (#58).
#[test]
fn sst_alu8() {
    let mut names: Vec<String> = (0x80u16..=0xBF).map(|op| format!("{op:02x}")).collect();

    names.extend(
        [
            "c6", "ce", "d6", "de", "e6", "ee", "f6", "fe", // ALU A,n
            "04", "0c", "14", "1c", "24", "2c", "3c", // INC r
            "05", "0d", "15", "1d", "25", "2d", "3d", // DEC r
            "34", "35", // INC/DEC (HL)
        ]
        .into_iter()
        .map(String::from),
    );

    for prefix in ["dd", "fd"] {
        for op in ["34", "35", "86", "8e", "96", "9e", "a6", "ae", "b6", "be"] {
            names.push(format!("{prefix} {op}"));
        }
    }

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 108, "64 + 8 + 16 + 20");
    harness::run_files(&files);
}
