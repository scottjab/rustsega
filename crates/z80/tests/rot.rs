//! The rotate and shift slice of the Z80 SingleStepTests suite (M2-08, #51).
//!
//! Same shape as the load and ALU tickets: 1000 hardware-captured cases per
//! opcode, checking every visible and hidden register plus the RAM the
//! instruction wrote. For this group the hidden state is the interesting part,
//! because the two halves of it disagree: the accumulator rotates leave `S`, `Z`
//! and `P/V` alone while `CB 00`-`3F` takes all of them from the result, and both
//! have to end with `q` holding the flag byte they wrote.
//!
//! The `cb 36` and `cb 3e` files are the undocumented `SLL`, and the `cb __`
//! files with `z = 6` are the read-modify-write on the memory at `HL`.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

/// `RLCA RRCA RLA RRA` (`07 0F 17 1F`) and the `CB` rotate/shift group
/// (`cb 00` through `cb 3f`: RLC, RRC, RL, RR, SLA, SRA, SLL, SRL over the eight
/// operand slots, so each operation appears eight times with `z = 6` being the
/// memory at `HL`).
#[test]
fn sst_rot() {
    let mut names: Vec<String> = ["07", "0f", "17", "1f"]
        .into_iter()
        .map(String::from)
        .collect();
    names.extend((0x00u8..=0x3F).map(|op| format!("cb {op:02x}")));

    let files: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(files.len(), 68, "4 accumulator rotates + 64 CB shifts");
    harness::run_files(&files);
}
