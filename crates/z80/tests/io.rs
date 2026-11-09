//! Hardware-captured conformance for the I/O instructions (M2-12, #55).
//!
//! `IN`/`OUT` are the only places the core meets the outside world, and the
//! suite records every port access: `OUT (C),0` really does push a zero byte out
//! of `ED 71` on the NMOS part, `IN (C)` really does throw the value away and
//! keep the flags, and the `wz` of a simple `IN A,(n)` is the port plus one
//! rather than the address of the operand byte.
//!
//! The block group is the interesting half. Its flags come from a 9-bit sum that
//! is not stored anywhere (`k = value + delta`), `B` steps before `OUT` picks its
//! port and after `IN` reads one, and a repeating pass that continues rewrites
//! `H` and `P/V` a second time from the low bits of `B` after taking `X`/`Y` from
//! the instruction's own address.

mod harness;

#[test]
fn sst_io() {
    // `IN A,(n)` and `OUT (n),A`.
    harness::run_files(&["db", "d3"]);

    // `IN r,(C)` and `OUT (C),r`, including the two slot-6 forms.
    harness::run_files(&[
        "ed 40", "ed 48", "ed 50", "ed 58", "ed 60", "ed 68", "ed 70", "ed 78", "ed 41", "ed 49",
        "ed 51", "ed 59", "ed 61", "ed 69", "ed 71", "ed 79",
    ]);

    // Block I/O: INI/IND and OUTI/OUTD, then the four repeating forms.
    harness::run_files(&[
        "ed a2", "ed aa", "ed b2", "ed ba", "ed a3", "ed ab", "ed b3", "ed bb",
    ]);
}
