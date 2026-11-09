//! Hardware-captured conformance for jumps, calls, returns and restarts
//! (M2-10, #53).
//!
//! Every file here is one opcode, stepped once from a thousand random starting
//! states, against hardware truth that also recorded the undocumented `wz` and
//! the T-state count. The same files are the reference for the stack traffic of
//! `CALL`/`RET`/`RST`, for the two not-taken timings (`JP cc` 10, `CALL cc` 10,
//! `RET cc` 5, `JR cc` 7, `DJNZ` 8) and for `RETI`/`RETN` restoring `IFF1`
//! from `IFF2`.

mod harness;

#[test]
fn sst_flow() {
    // JP nn, JP (HL) and the eight conditional JP forms; JR e, DJNZ e and the
    // four conditional JR forms; CALL nn and its eight conditional forms; RET
    // and its eight; RST, one file per restart vector; the two indexed `JP`;
    // and RETI, RETN and the six RETN mirrors.
    let files: Vec<&str> = vec![
        "c3", "e9", "c2", "ca", "d2", "da", "e2", "ea", "f2", "fa", "18", "10", "20", "28", "30",
        "38", "cd", "c4", "cc", "d4", "dc", "e4", "ec", "f4", "fc", "c9", "c0", "c8", "d0", "d8",
        "e0", "e8", "f0", "f8", "c7", "cf", "d7", "df", "e7", "ef", "f7", "ff", "dd e9", "fd e9",
        "ed 4d", "ed 45", "ed 55", "ed 5d", "ed 65", "ed 6d", "ed 75", "ed 7d",
    ];
    harness::run_files(&files);
}
