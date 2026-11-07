//! What the pinned 680x0 suite says about exception entry (M1-08, #16).
//!
//! The unit tests in `src/exceptions.rs` check the implementation against the
//! rules; these check the rules against the data, so a constant that drifts away
//! from what the suite measures fails here and names the file it stopped
//! matching. They also enable `TRAP`, the one exception file the core can now
//! run end to end.
//!
//! Everything except [`trap_file_passes_with_a_state_check`] reads case data
//! without running the CPU: `CHK`, `TRAPV` and `DIVU` need instructions the core
//! does not have yet (they are deliberately out of #16's scope), but the slot
//! each of them raises and the frame it leaves behind are visible in the case
//! data on their own.

// Shared harness; it allows its own dead code since each binary uses a subset.
#[path = "harness/mod.rs"]
mod harness;

use m68k::{EXC_CHK, EXC_DIV_ZERO, EXC_ILLEGAL, EXC_PRIVILEGE, EXC_TRAPV, trap_vector};

/// The vector table the suite's cases put their handlers in: the first 256
/// bytes, one long per vector (M68000UM "Exception Vector Memory Map").
const VECTOR_TABLE_END: u32 = 0x100;
/// Vector longs are four bytes apart.
const VECTOR_STRIDE: u32 = 4;
/// A group 1/2 frame is an SR word and a PC long.
const FRAME_BYTES: u32 = 6;

/// The files whose cases take exceptions, and therefore say something about the
/// vector map. Found by reading the data, not by guessing: these are the files
/// with cases whose `final.pc` is a long in the vector table.
const EXCEPTION_FILES: [&str; 4] = ["TRAP", "CHK", "TRAPV", "DIVU"];

/// Bytes of a sparse `ram` array keyed by address, for reading longs and words
/// out of a case's RAM.
fn ram_map(ram: &[(u32, u8)]) -> Vec<(u32, u8)> {
    ram.to_vec()
}

fn byte_at(map: &[(u32, u8)], addr: u32) -> Option<u8> {
    map.iter().find(|&&(a, _)| a == addr).map(|&(_, v)| v)
}

fn long_at(map: &[(u32, u8)], addr: u32) -> Option<u32> {
    let bytes: Option<Vec<u8>> = (0..4).map(|i| byte_at(map, addr + i)).collect();
    bytes.map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn word_at(map: &[(u32, u8)], addr: u32) -> Option<u16> {
    let hi = byte_at(map, addr)?;
    let lo = byte_at(map, addr + 1)?;
    Some(u16::from_be_bytes([hi, lo]))
}

/// Every vector slot in `case`'s initial RAM whose long is where the case ends
/// up — i.e. the handler the exception fetched. Empty when the case did not take
/// an exception.
///
/// A `Vec` rather than an `Option` so a caller can see ambiguity: a slot list of
/// exactly one entry means the case really says which vector it took, and the
/// tests below insist on that rather than quietly accepting a tie.
fn fetched_vector_slots(case: &harness::TestCase) -> Vec<u32> {
    let map = ram_map(&case.initial.ram);
    (0..VECTOR_TABLE_END)
        .step_by(VECTOR_STRIDE as usize)
        .filter(|&slot| long_at(&map, slot) == Some(case.final_state.pc))
        .collect()
}

/// The frame the case pushed, as `(saved_sr, pushed_pc)`, read from the final
/// RAM at the final stack pointer. `None` when the case's RAM does not list all
/// six frame bytes.
fn pushed_frame(case: &harness::TestCase) -> Option<(u16, u32)> {
    let map = ram_map(&case.final_state.ram);
    let sp = case.final_state.active_sp();
    let sr = word_at(&map, sp)?;
    let pc = long_at(&map, sp + 2)?;
    Some((sr, pc))
}

/// The suite file `TRAP` runs end to end under the state-only check: the 8065
/// cases that take a trap must land on the trap vector with the trap frame, and
/// the 65 that end in an address error are skipped by the harness (they are
/// M1-29, #37). This is the file #16 exists to turn green.
#[test]
fn trap_file_passes_with_a_state_check() {
    harness::assert_file_passes("TRAP", harness::Check::State);
}

/// TRAP #n fetches the long at `$80 + 4 * n`, which is what makes the vector
/// number `32 + n` ([`trap_vector`]). Measured over every trap case in the file,
/// with the trap number read back out of the case's own opcode.
#[test]
fn the_suite_puts_traps_at_vector_32_plus_n() {
    let cases = harness::load_file("TRAP").expect("TRAP file should load");
    let mut measured = 0;
    for case in &cases {
        let opcode = case.initial.prefetch[0];
        if !(0x4E40..=0x4E4F).contains(&opcode) {
            continue;
        }
        let number = (opcode & 0x000F) as u32;
        let want = VECTOR_TABLE_END / 2 + VECTOR_STRIDE * number; // $80 + 4n
        let slots = fetched_vector_slots(case);
        assert_eq!(
            slots,
            vec![want],
            "case {:?}: TRAP #{number} must fetch exactly the long at ${want:08X}",
            case.name
        );
        assert_eq!(
            want,
            u32::from(trap_vector(number as u8)) * VECTOR_STRIDE,
            "trap_vector({number}) must name the slot the data uses"
        );
        measured += 1;
    }
    assert_eq!(
        measured, 8065,
        "every trap case in the file must be measured, not just some"
    );
}

/// The frame every trap case pushes: SR as it was, then the address of the
/// instruction after the TRAP, six bytes below the stack pointer. This is the
/// `pushed_pc` rule for the exceptions that resume after themselves, read off the
/// data alone (M68000UM "Stack Frame Formats", group 1/2 vector frame).
#[test]
fn trap_frames_hold_the_saved_sr_and_the_next_instruction_address() {
    let cases = harness::load_file("TRAP").expect("TRAP file should load");
    let mut measured = 0;
    for case in &cases {
        let Some((sr, pc)) = pushed_frame(case) else {
            continue;
        };
        let sp = case.final_state.active_sp();
        assert_eq!(
            case.initial.ssp - sp,
            FRAME_BYTES,
            "case {:?}: a group 1/2 frame is six bytes",
            case.name
        );
        assert_eq!(
            sr, case.initial.sr,
            "case {:?}: the frame saves SR as it was, before S and T are changed",
            case.name
        );
        assert_eq!(
            pc,
            case.initial.pc + 2,
            "case {:?}: TRAP is one word, so the frame points past it",
            case.name
        );
        measured += 1;
    }
    assert_eq!(
        measured, 8065,
        "every trap case must be measured, not just some"
    );
}

/// CHK's own exception is vector [`EXC_CHK`] (long at `$000018`) in 4272 cases,
/// and its frame points past the instruction — at `pc + 2`, `+4` or `+6`, the
/// length of the CHK form, never a bare `+2`. 3117 more fetch [`EXC_ILLEGAL`]
/// because their effective address is illegal, which is a different route.
///
/// The remaining 676 fetch no handler at all, because CHK is also the one
/// instruction here that usually *succeeds*: its range check passes and the
/// instruction just updates the condition codes. Those must show the negative
/// side of the same rule — no frame pushed, and `pc` landed right after the
/// instruction — so "fetched no handler" can only ever mean "took no exception",
/// never "took one whose slot we did not look at". The three counts together are
/// the whole file, so a route that moved a case between them fails too.
#[test]
fn chk_exceptions_use_vector_six_and_point_past_the_instruction() {
    let cases = harness::load_file("CHK").expect("CHK file should load");
    let chk_slot = u32::from(EXC_CHK) * VECTOR_STRIDE;
    let illegal_slot = u32::from(EXC_ILLEGAL) * VECTOR_STRIDE;
    let mut measured = 0;
    let mut illegal_route = 0;
    let mut no_exception = 0;
    for case in &cases {
        let slots = fetched_vector_slots(case);
        if slots.is_empty() {
            assert!(
                pushed_frame(case).is_none(),
                "case {:?}: fetched no handler vector, so it took no exception \
                 and must not have pushed a group 1/2 frame either",
                case.name
            );
            let offset = case.final_state.pc - case.initial.pc;
            assert!(
                matches!(offset, 2 | 4 | 6),
                "case {:?}: a CHK that does not raise lands past its own \
                 instruction, got +{offset}",
                case.name
            );
            no_exception += 1;
            continue;
        }
        match slots.as_slice() {
            [slot] if *slot == illegal_slot => {
                illegal_route += 1;
                continue;
            }
            [slot] if *slot == chk_slot => {}
            other => panic!(
                "case {:?}: CHK case fetched its handler from {other:?}, \
                 which is neither CHK (${chk_slot:08X}) nor illegal (${illegal_slot:08X})",
                case.name
            ),
        }
        let Some((_, pc)) = pushed_frame(case) else {
            continue;
        };
        let offset = pc - case.initial.pc;
        assert!(
            matches!(offset, 2 | 4 | 6),
            "case {:?}: CHK pushes the address after the instruction, got +{offset}",
            case.name
        );
        measured += 1;
    }
    assert_eq!(
        measured, 4272,
        "every CHK-exception case in the file must be measured"
    );
    assert_eq!(
        illegal_route, 3117,
        "every illegal-effective-address CHK case must be counted"
    );
    assert_eq!(
        no_exception, 676,
        "every CHK case whose range check passes must be counted"
    );
}

/// TRAPV's exception is vector [`EXC_TRAPV`] (long at `$00001C`) and its frame
/// points at `pc + 2`, like TRAP: one word, resume after it.
#[test]
fn trapv_exceptions_use_vector_seven() {
    let cases = harness::load_file("TRAPV").expect("TRAPV file should load");
    let want = u32::from(EXC_TRAPV) * VECTOR_STRIDE;
    let mut measured = 0;
    for case in &cases {
        let slots = fetched_vector_slots(case);
        if slots.is_empty() {
            continue;
        }
        assert_eq!(
            slots,
            vec![want],
            "case {:?}: TRAPV must fetch exactly the long at ${want:08X}",
            case.name
        );
        if let Some((_, pc)) = pushed_frame(case) {
            assert_eq!(
                pc,
                case.initial.pc + 2,
                "case {:?}: TRAPV is one word, so the frame points past it",
                case.name
            );
            measured += 1;
        }
    }
    assert_eq!(
        measured, 4095,
        "every TRAPV-exception case in the file must be measured"
    );
}

/// Divide by zero is vector [`EXC_DIV_ZERO`] (long at `$000014`), and — unlike
/// TRAP, TRAPV and CHK — its frame points at the faulting instruction itself, so
/// a handler can fix the operands and re-execute it. The suite has exactly one
/// case that divides by zero, and it is `80ef [DIVU (d16, A7), D0] 5745`, whose
/// divisor word at `$0000F0` is zero.
#[test]
fn divide_by_zero_uses_vector_five_and_pushes_the_faulting_address() {
    let cases = harness::load_file("DIVU").expect("DIVU file should load");
    let want = u32::from(EXC_DIV_ZERO) * VECTOR_STRIDE;
    let mut measured = 0;
    for case in &cases {
        let slots = fetched_vector_slots(case);
        if !slots.contains(&want) {
            continue;
        }
        assert_eq!(
            slots,
            vec![want],
            "case {:?}: divide by zero must fetch exactly the long at ${want:08X}",
            case.name
        );
        let (sr, pc) = pushed_frame(case).unwrap_or_else(|| {
            panic!(
                "case {:?}: the divide-by-zero frame must be in the data",
                case.name
            )
        });
        assert_eq!(
            pc, case.initial.pc,
            "case {:?}: divide by zero pushes the faulting instruction's own address",
            case.name
        );
        assert_eq!(
            case.initial.ssp - case.final_state.active_sp(),
            FRAME_BYTES,
            "case {:?}: group 1 frame",
            case.name
        );
        assert_eq!(sr, 0x2710, "the CCR bits the divide cleared before raising");
        measured += 1;
    }
    assert_eq!(
        measured, 1,
        "the suite has exactly one divide-by-zero case, and it must be the one measured"
    );
}

/// There is no line-A or line-F vector on the 68000, and this crate has no
/// constant for one: the slots the 68010 would use are never fetched by any case
/// in any exception file, and the suite has no A-line or F-line file to pin a
/// number from in the first place. The anti-test for the `line_a_vector_10` /
/// `line_f_vector_11` the ticket asks for.
#[test]
fn the_suite_has_nothing_to_pin_a_line_a_or_line_f_vector_to() {
    // `$24`..`$30`: where the ticket's numbers 9, 10 and 11 would land under the
    // "slot address = number x 4" convention. Trace (#38) owns `$24`; `$28` and
    // `$2C` are the 68010's line-A and line-F slots in the ticket's labelling.
    let forbidden: Vec<u32> = (0x24..=0x30).step_by(VECTOR_STRIDE as usize).collect();
    for name in EXCEPTION_FILES {
        let cases = harness::load_file(name).unwrap_or_else(|| panic!("{name} should load"));
        for case in &cases {
            for slot in fetched_vector_slots(case) {
                assert!(
                    !forbidden.contains(&slot),
                    "case {:?} in {name} fetched a handler from ${slot:08X}, \
                     a slot the 68000 has no exception for",
                    case.name
                );
            }
        }
    }

    // And the suite cannot pin one from the other end either: no file covers the
    // A-line or F-line groups at all.
    let files = harness::all_files().expect("the suite should be present");
    for file in &files {
        let upper = file.to_uppercase();
        assert!(
            !(upper.contains("LINEA") || upper.contains("LINEF") || upper.contains("ILLEGAL")),
            "{file}: the suite would have to be re-read before #20 (M1-12) claims \
             to have measured a line-A or line-F vector"
        );
    }
}

/// The map, in one place, with the slot each constant names. `#16` measured the
/// first three and the traps; [`EXC_PRIVILEGE`] is the slot after
/// [`EXC_TRAPV`], which no suite case can confirm because every case in every
/// file runs in supervisor mode.
#[test]
fn the_vector_map_is_the_slot_address_over_four() {
    assert_eq!(u32::from(EXC_ILLEGAL) * VECTOR_STRIDE, 0x0000_000C);
    assert_eq!(u32::from(EXC_DIV_ZERO) * VECTOR_STRIDE, 0x0000_0014);
    assert_eq!(u32::from(EXC_CHK) * VECTOR_STRIDE, 0x0000_0018);
    assert_eq!(u32::from(EXC_TRAPV) * VECTOR_STRIDE, 0x0000_001C);
    assert_eq!(u32::from(EXC_PRIVILEGE) * VECTOR_STRIDE, 0x0000_0020);
    assert_eq!(u32::from(trap_vector(0)) * VECTOR_STRIDE, 0x0000_0080);
    assert_eq!(u32::from(trap_vector(15)) * VECTOR_STRIDE, 0x0000_00BC);
    // The numbering the tickets quote from Motorola's figure is one higher: it
    // labels this table's slot 3 as "vector 4". The slots are what matter.
    assert_eq!(EXC_ILLEGAL, 3);
}
