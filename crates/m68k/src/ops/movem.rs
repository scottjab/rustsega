//! `MOVEM` — move multiple registers to or from memory (M1-24, #32).
//!
//! Encoding `0100 1d00 1s eeeeee` plus a 16-bit register-list extension word
//! (M68000UM "MOVEM — Move Multiple Registers"): `d` picks the direction — clear
//! is registers to memory (`$4880`), set is memory to registers (`$4C80`) — and
//! `s` the size, clear word and set long. Nothing about MOVEM touches a condition
//! code, so no flag is written here; all 16 130 cases of `MOVEM.w` and `MOVEM.l`
//! leave `SR` alone.
//!
//! # The register list, and why a store through `-(An)` reads it backwards
//!
//! Mask bit 0 is `D0` … bit 7 `D7`, bit 8 `A0` … bit 15 `A7`, and the registers
//! move in that order. For a store whose operand is `-(An)` the 68000 reads the
//! word backwards — bit 0 is `A7`, bit 7 `A0`, bit 8 `D7`, bit 15 `D0` — and stores
//! from `A7` down to `D0` at decreasing addresses, so the first register it writes
//! is the one furthest up the stack. That is why `MOVEM.L D0,-(A7)` and
//! `MOVEM.L D0,(A0)` put `D0` at opposite ends of the transfer, and it is
//! measured: `MOVEM.l.json.gz` runs 935 cases of `$48Ex` — a predecrement store —
//! and none of them matches the normal reading.
//!
//! # The operand is one address, not `n` of them
//!
//! `MOVEM` cannot use [`M68k::resolve_ea`], which applies the `(An)+` / `-(An)`
//! register update as it resolves: MOVEM moves its address register once, after all
//! `n` transfers, by the whole `n × size` (M68000UM MOVEM: `(An)` is "updated to
//! point past the last register read", `-(An)` "to the last register written"). So
//! the base comes from [`M68k::ea_address`] for the control modes and from `An`
//! itself for the two stack modes, the transfers walk away from it by hand, and
//! `An` is written at the end — which is what makes the two `An` quirks below fall
//! out of the order of operations instead of needing cases of their own.
//!
//! * A store through `-(An)` whose list names `An` stores the *initial* value of
//!   `An`, before any of the decrements. 921 cases across the two files include
//!   their own mode register, and every one reports the pointer as it started.
//! * A load through `(An)+` whose list names `An` ends with `An` past the last
//!   read, not with the value it just loaded, because the update is last.
//!
//! # The extra read
//!
//! A load issues one more word read after the last register — the bus is left
//! running one cycle, which is also why an odd address there can raise an address
//! error on real silicon (M68000UM MOVEM, "Note"). It is modelled, at the address
//! the transfers left off, for the same reason [`M68k::read_ea`] does the access
//! rather than pretending: a device bus is allowed to see it, and `#[cfg(test)]
//! movem_load_reads_one_more_word_than_it_writes` shows it in an access log. It
//! cannot change any architectural state — it is a read — and the 4 clock cycles it
//! costs belong to M1-31 (#39/#40), which is why this handler returns 0 like every
//! other.
//!
//! # An empty list is legal
//!
//! `MOVEM.L #$0,(A0)` does nothing at all, and it is in the data:
//! `MOVEM.l.json.gz` case `48d0 [MOVEM.l #, (A0)] 6460` starts and ends with the
//! same machine. `decode` therefore accepts every operand MOVEM can address and
//! this handler never rejects one for an empty list.
//!
//! # Cycles
//!
//! Nothing extra, including the extra read above: `read_w` and `write_w` charge
//! the word cycles of each register in the list, and the total they reach is
//! M68000UM section 8's `8/12 + n`-style MOVEM figure for every mode. Both
//! `MOVEM.w` and `MOVEM.l` pass cycle checking unchanged when M1-31b (#40)
//! flipped them.

use crate::M68k;
use crate::bus::{ADDR_MASK, Bus};
use crate::ea::{Ea, Size, sign_extend_word};
use crate::ops::field;

/// Bit 10 of a MOVEM opcode, the manual's `d`: clear is registers to memory
/// (`$4880`), set is memory to registers (`$4C80`). The same bit
/// [`crate::decode::classify_movem`] reads to pick the operand rule.
const TO_MEMORY_BIT: u16 = 0x0400;
/// Bit 6, the manual's `s`: set is a long transfer, clear a word. MOVEM has one
/// bit here rather than the usual two because it has no byte form.
const LONG_SIZE_BIT: u16 = 0x0040;
/// EA mode `011`: `(An)+`, the load direction's stack mode.
const MODE_POST_INCREMENT: u8 = 0b011;
/// EA mode `100`: `-(An)`, the store direction's stack mode, and the reason the
/// register list has two orders.
const MODE_PRE_DECREMENT: u8 = 0b100;
/// The number of registers a MOVEM list can name: one mask bit each.
const LIST_BITS: u32 = 16;

impl M68k {
    /// `MOVEM <list>,<ea>` and `MOVEM <ea>,<list>` (`0100 1d00 1s eeeeee`): the
    /// registers named by the list word, transferred in order to or from `n`
    /// consecutive words or longs at the operand (M68000UM MOVEM; M1-24, #32).
    ///
    /// [`crate::decode`] has already rejected the operands MOVEM cannot encode, so
    /// the one way [`M68k::ea_address`] can decline here is a decoder/handler
    /// disagreement, turned into an illegal instruction the way [`M68k::op_lea`]
    /// turns it. Nothing moves a condition code, and the clock count is 0: the
    /// bus cycles are charged by the transfers themselves and the instruction's
    /// own 4/8/12-clock table is M1-31's (#39/#40).
    pub(crate) fn op_movem(&mut self, bus: &mut impl Bus, opcode: u16) -> u32 {
        let to_memory = opcode & TO_MEMORY_BIT == 0;
        let size = if opcode & LONG_SIZE_BIT == 0 {
            Size::Word
        } else {
            Size::Long
        };
        let mode = field(opcode, 3);
        let reg = field(opcode, 0);

        // The register list is the instruction's first extension word, so it is
        // popped before the operand's own displacement words — the order
        // `movem_reads_the_list_before_the_displacement` pins.
        let mask = self.fetch_ext_word(bus);
        let Some(base) = self.movem_ea(bus, mode, reg) else {
            return self.op_illegal(bus);
        };

        // A store through `-(An)` is the one transfer that reads the list
        // backwards and the only one that walks down instead of up.
        let reversed = to_memory && mode == MODE_PRE_DECREMENT;
        let step = size.bytes();
        let mut count: u32 = 0;
        for bit in 0..LIST_BITS {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let addr = if reversed {
                base.wrapping_sub(step * (count + 1))
            } else {
                base.wrapping_add(step * count)
            };
            let register = movem_list_ea(bit as u8, reversed);
            if to_memory {
                let value = self.read_ea(bus, register, size);
                self.write_ea(bus, Ea::Mem(addr & ADDR_MASK), size, value);
            } else {
                let raw = self.read_ea(bus, Ea::Mem(addr & ADDR_MASK), size);
                // Every word loaded back is sign extended to 32 bits, the data
                // registers included (M68000UM MOVEM, "Note"), and a long is
                // written whole. `Size::Long` on the write is therefore the point,
                // not an approximation of the transfer width: it is what keeps
                // `write_ea` from merging the load into the register's upper half
                // and what keeps the stack pointer's stored mirror in step.
                let value = if size == Size::Word {
                    sign_extend_word(raw as u16)
                } else {
                    raw
                };
                self.write_ea(bus, register, Size::Long, value);
            }
            count += 1;
        }

        // Where a walk upward from the operand ends up: the address after the last
        // transfer for every mode but the predecrement store, which walks down.
        let end_of_walk = base.wrapping_add(step * count);

        // The address register moves once, at the end: `-(An)` to the last address
        // it wrote, `(An)+` past the last it read, and not at all for any other
        // operand — including a list that named this very register, whose loaded
        // value this update overwrites. Both orders are measured; see the module
        // docs, and note that `decode` guarantees a store never sees mode `011`
        // and a load never sees mode `100`.
        match mode {
            MODE_PRE_DECREMENT => {
                self.write_ea(
                    bus,
                    Ea::AddrReg(reg),
                    Size::Long,
                    base.wrapping_sub(step * count),
                );
            }
            MODE_POST_INCREMENT => {
                self.write_ea(bus, Ea::AddrReg(reg), Size::Long, end_of_walk);
            }
            _ => {}
        }

        if !to_memory {
            // The extra word read after the last register, at the address the walk
            // ended at: see the module docs for what it is and what it can and
            // cannot change.
            self.read_w(bus, end_of_walk & ADDR_MASK);
        }

        self.advance_opcode(bus);
        0
    }

    /// The address a MOVEM operand names, without the `(An)+` / `-(An)` register
    /// update — see the module docs for why [`M68k::resolve_ea`] is the wrong
    /// helper for this instruction.
    ///
    /// The two stack modes are the whole difference: [`M68k::ea_address`] refuses
    /// them, as it must for `LEA` and `PEA`, and for MOVEM they are simply `An`
    /// read directly, the update having been deferred to the end of the transfer.
    /// Like `ea_address` the result is the address *as computed*, unmasked; the
    /// transfers mask on the way to the bus and `An` keeps all 32 bits (#193).
    fn movem_ea(&mut self, bus: &mut impl Bus, mode: u8, reg: u8) -> Option<u32> {
        if matches!(mode, MODE_POST_INCREMENT | MODE_PRE_DECREMENT) {
            return Some(self.a[usize::from(reg)]);
        }
        self.ea_address(bus, mode, reg)
    }
}

/// The register a MOVEM list word names at `bit`, as an [`Ea`] so the transfer
/// reuses the EA layer's register reads and its `An`-writes-through-the-stack-
/// pointer rule.
///
/// Normally bit 0 is `D0` up to bit 7 `D7` and bit 8 `A0` up to bit 15 `A7`.
/// Reversed — a store through `-(An)` — both runs are turned end for end: bit 0 is
/// `A7`, bit 7 `A0`, bit 8 `D7`, bit 15 `D0` (M68000UM MOVEM, "Note"; measured, as
/// the module docs describe).
const fn movem_list_ea(bit: u8, reversed: bool) -> Ea {
    if reversed {
        if bit < 8 {
            Ea::AddrReg(7 - bit)
        } else {
            Ea::DataReg(15 - bit)
        }
    } else if bit < 8 {
        Ea::DataReg(bit)
    } else {
        Ea::AddrReg(bit - 8)
    }
}

/// Tests for `MOVEM` (M1-24, #32).
///
/// The opcodes below are read off `MOVEM.w.json.gz` and `MOVEM.l.json.gz` rather
/// than assembled by hand: `$48E7` is the first `MOVEM.L -(A7)` the suite runs,
/// `$48A7` the first `MOVEM.W -(A7)`, and `$4C98`/`$4CD8` the first
/// `MOVEM.W`/`MOVEM.L (A0)+`.
#[cfg(test)]
mod tests {
    use crate::M68k;
    use crate::bus::Bus;
    use crate::sr::{SR_C, SR_N, SR_V, SR_X, SR_Z};
    use crate::testbus::{TestBus, cpu_with_program};

    /// Where [`cpu_with_program`] loads the program, so a test can name the
    /// address `pc` should reach.
    const PROGRAM: u32 = 0x0000_1000;
    /// A data address every memory test can use, clear of the program.
    const MEM: u32 = 0x0000_2000;

    /// The stack the `-(A7)` and `(A7)+` tests start from, clear of both.
    const STACK: u32 = 0x0000_4000;

    /// Every condition code as one CCR value, for "this instruction leaves the
    /// flags alone" tests.
    const ALL_FLAGS: u16 = SR_X | SR_N | SR_Z | SR_V | SR_C;

    /// Run one `step` on a CPU whose queue front is `opcode`, with `rest` as the
    /// following words.
    fn run(opcode: u16, rest: &[u16]) -> (M68k, TestBus) {
        let mut program = vec![opcode];
        program.extend_from_slice(rest);
        cpu_with_program(&program)
    }

    /// `MOVEM.L D0-D1/A0,-(A7)`: `$48E7`, and the list word has to be written the
    /// way a predecrement store reads it — `A7` at bit 0 … `A0` at bit 7, then `D7`
    /// at bit 8 … `D0` at bit 15 — so `D0`/`D1`/`A0` is bits 15, 14 and 7: `$C080`.
    ///
    /// The three registers land below the stack pointer with `D0` at the bottom and
    /// `A0` last, which is what "stored from A7 down to D0 at decreasing addresses"
    /// buys, and `A7` ends 12 bytes down (M68000UM MOVEM).
    #[test]
    fn movem_l_regs_to_predec() {
        let (mut cpu, mut bus) = run(0x48E7, &[0xC080, 0x4E71]);
        cpu.d[0] = 0x1111_2222;
        cpu.d[1] = 0x3333_4444;
        cpu.a[0] = 0x5555_6666;
        let sp = cpu.a[7];

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(sp - 4), 0x5555_6666, "A0 is written first");
        assert_eq!(bus.read_long(sp - 8), 0x3333_4444, "then D1");
        assert_eq!(bus.read_long(sp - 12), 0x1111_2222, "and D0 at the bottom");
        assert_eq!(cpu.a[7], sp - 12, "three longs");
        assert_eq!(
            cpu.ssp,
            sp - 12,
            "A7 is the stack pointer, so its stored half moves with it (#17's trap)"
        );
        assert_eq!(cpu.pc, PROGRAM + 4, "opcode and one extension word");
    }

    /// `MOVEM.W (A0)+,D0-D1` is `$4C98`: every word loaded back is sign extended
    /// into the full 32 bits of its register, **the data registers included**
    /// (M68000UM MOVEM, "Note"). A core that writes a word into `Dn` the way `MOVE`
    /// does — low 16 bits, upper kept — fails here rather than in the suite.
    #[test]
    fn movem_w_mem_to_regs_sign_extends() {
        let (mut cpu, mut bus) = run(0x4C98, &[0x0003, 0x4E71]);
        bus.write_long(MEM, 0x8001_FFFE);
        bus.write_long(MEM + 4, 0x1234_5678);
        cpu.a[0] = MEM;
        cpu.d[0] = 0xAAAA_AAAA;
        cpu.d[1] = 0xBBBB_BBBB;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0xFFFF_8001, "the low word, sign extended");
        assert_eq!(cpu.d[1], 0xFFFF_FFFE);
        assert_eq!(cpu.a[0], MEM + 4, "past the two words it read");
    }

    /// A word load into an address register is sign extended the same way —
    /// `MOVEM.W (A0),A1` is `$4C91`, and 903 cases of mode `(An)` in `MOVEM.w`
    /// include this one. Both files keep nonzero upper halves in `An`, so a core
    /// that zero extended would show up as `$0000_xxxx`.
    #[test]
    fn movem_w_mem_to_regs_sign_extends_an() {
        let (mut cpu, mut bus) = run(0x4C90, &[0x0200, 0x4E71]);
        bus.write_long(MEM, 0xFFFE_0000);
        cpu.a[0] = MEM;
        cpu.a[1] = 0xAAAA_AAAA;

        cpu.step(&mut bus);

        assert_eq!(cpu.a[1], 0xFFFF_FFFE);
    }

    /// `MOVEM.L (A0)+,D0-D1/A0` is `$4CD8`, with the list in the normal order — bit
    /// 0 `D0`, bit 1 `D1`, bit 8 `A0` — and `A0` named by its own list. `A0` ends
    /// pointing past the last register read, so the long it loaded into itself at
    /// `$2008` loses to the end-of-instruction update (M68000UM MOVEM).
    #[test]
    fn movem_l_postinc_updates_an() {
        let (mut cpu, mut bus) = run(0x4CD8, &[0x0103, 0x4E71]);
        bus.write_long(MEM, 0x1111_1111);
        bus.write_long(MEM + 4, 0x2222_2222);
        bus.write_long(MEM + 8, 0x3333_3333);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x1111_1111);
        assert_eq!(cpu.d[1], 0x2222_2222);
        assert_eq!(
            cpu.a[0],
            MEM + 12,
            "three longs past the base, not the $33333333 it read"
        );
    }

    /// `MOVEM.L #$0,(A0)` is legal and does nothing: `MOVEM.l.json.gz` case
    /// `48d0 [MOVEM.l #, (A0)] 6460` starts and ends with the same machine, and the
    /// empty list is not an illegal instruction.
    ///
    /// The load half is the update-by-`n` rule run with `n = 0`, so `A0` does not
    /// move. The suite has no empty-list case in a stack mode, so the `-(A7)` and
    /// `(A7)+` halves below are reasoned rather than measured: an empty list walks
    /// zero registers, and the register update is the walk (`An - n * width`, `An +
    /// n * width`), so `SP` cannot move either. If a real 68000 pre-decrements even
    /// for an empty list, this test is where that would be argued.
    #[test]
    fn movem_empty_mask() {
        let (mut cpu, mut bus) = run(0x48D0, &[0x0000, 0x4E71]);
        bus.write_long(MEM, 0xDEAD_BEEF);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0xDEAD_BEEF, "nothing was written");
        assert_eq!(cpu.a[0], MEM);
        assert_eq!(cpu.pc, PROGRAM + 4, "the list word is still consumed");

        let (mut cpu, mut bus) = run(0x4CD8, &[0x0000, 0x4E71]);
        bus.write_long(MEM, 0xDEAD_BEEF);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(cpu.a[0], MEM, "an empty list moves An nowhere");

        // The same in both stack modes, where "did not move" is the interesting
        // claim: `MOVEM.W #$0,-(A7)` is `$48A7` and `MOVEM.W #$0,(A7)+` is `$4CDF`.
        let (mut cpu, mut bus) = run(0x48A7, &[0x0000, 0x4E71]);
        cpu.a[7] = STACK;
        cpu.step(&mut bus);
        assert_eq!(cpu.a[7], STACK, "a store of nothing does not move SP");
        assert_eq!(bus.read_word(STACK - 2), 0, "and writes nothing to free up");

        let (mut cpu, mut bus) = run(0x4CDF, &[0x0000, 0x4E71]);
        cpu.a[7] = STACK;
        cpu.step(&mut bus);
        assert_eq!(cpu.a[7], STACK, "neither does a load of nothing");
    }

    /// The reversed order of a `-(An)` store, on a list whose two readings name
    /// different registers: `$0103` is `D0`/`D1`/`A0` normally and `A7`/`A6`/`D7`
    /// read backwards, and only one of the two can be right. The suite decides it —
    /// `MOVEM.l.json.gz` runs `$48E7` with the list `$0103` and reports `A7`, `A6`
    /// and `D7` below the stack pointer with `D0`, `D1` and `A0` untouched.
    #[test]
    fn movem_predec_reads_the_list_backwards() {
        let (mut cpu, mut bus) = run(0x48E7, &[0x0103, 0x4E71]);
        cpu.d[0] = 0xD0D0_D0D0;
        cpu.d[1] = 0xD1D1_D1D1;
        cpu.d[7] = 0xD7D7_D7D7;
        cpu.a[0] = 0xA0A0_A0A0;
        cpu.a[6] = 0xA6A6_A6A6;
        // Even, because a `MOVEM.L` push from an odd stack pointer is an address
        // error and writes nothing at all (#37, M1-29); this test is about the
        // order the list is pushed in.
        cpu.a[7] = 0xA7A7_A7A0;
        let sp = cpu.a[7];

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(sp - 4), sp, "bit 0 is A7 here");
        assert_eq!(bus.read_long(sp - 8), 0xA6A6_A6A6, "bit 1 is A6");
        assert_eq!(bus.read_long(sp - 12), 0xD7D7_D7D7, "bit 8 is D7");
        assert_eq!(cpu.a[0], 0xA0A0_A0A0, "A0 is not in the list at all");
    }

    /// The same list word at any other operand is read the normal way round, so the
    /// reversal really is a property of the mode and not of the instruction:
    /// `MOVEM.L #$0103,(A1)` is `$48D1` and stores `D0`, `D1`, `A0` upward from the
    /// operand (905 cases of `$48Dx` in `MOVEM.l.json.gz`).
    #[test]
    fn movem_stores_in_normal_mask_order_otherwise() {
        let (mut cpu, mut bus) = run(0x48D1, &[0x0103, 0x4E71]);
        cpu.d[0] = 0x1111_1111;
        cpu.d[1] = 0x2222_2222;
        cpu.a[0] = 0x3333_3333;
        cpu.a[6] = 0xA6A6_A6A6;
        cpu.a[1] = MEM;

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(MEM), 0x1111_1111, "bit 0 is D0 here");
        assert_eq!(bus.read_long(MEM + 4), 0x2222_2222);
        assert_eq!(bus.read_long(MEM + 8), 0x3333_3333);
        assert_eq!(cpu.a[1], MEM, "only (An)+ and -(An) move their register");
        assert_eq!(cpu.a[6], 0xA6A6_A6A6, "A6 is not in the list either");
    }

    /// The `-(An)` store of a list that names `An` writes the *initial* value of
    /// `An`, before the decrements: `MOVEM.L D0/A7,-(A7)` is `$48E7` with bit 15
    /// (`D0`) and bit 0 (`A7` in the reversed reading) set, and the long at `SP-4`
    /// is the stack pointer as the instruction started. Measured — 921 cases of the
    /// two files name their own mode register (M68000UM MOVEM).
    #[test]
    fn movem_predec_stores_initial_an() {
        let (mut cpu, mut bus) = run(0x48E7, &[0x8001, 0x4E71]);
        cpu.d[0] = 0x1234_5678;
        let sp = cpu.a[7];

        cpu.step(&mut bus);

        assert_eq!(bus.read_long(sp - 4), sp, "the pointer as it started");
        assert_eq!(bus.read_long(sp - 8), 0x1234_5678);
        assert_eq!(cpu.a[7], sp - 8);
    }

    /// A word transfer takes the low 16 bits of every register, address registers
    /// included, and steps by 2 (M68000UM MOVEM — the transfer width is the only
    /// thing that decides, and a `Dn` is not special). `MOVEM.W D0-D1/A0,-(A7)` is
    /// `$48A7` with the reversed list `A0` at bit 7, `D1` at bit 14, `D0` at bit 15:
    /// `$C080`.
    #[test]
    fn movem_w_takes_the_low_word_of_every_register() {
        let (mut cpu, mut bus) = run(0x48A7, &[0xC080, 0x4E71]);
        cpu.d[0] = 0xDEAD_BEE0;
        cpu.d[1] = 0xCAFE_BEE1;
        cpu.a[0] = 0xFFFF_FF02;
        let sp = cpu.a[7];

        cpu.step(&mut bus);

        assert_eq!(bus.read_word(sp - 2), 0xFF02, "A0's low word");
        assert_eq!(bus.read_word(sp - 4), 0xBEE1);
        assert_eq!(bus.read_word(sp - 6), 0xBEE0);
        assert_eq!(cpu.a[7], sp - 6, "three words");
    }

    /// MOVEM affects no condition code, in either direction: the `SR` of all 16 130
    /// cases of the two suite files is unchanged.
    #[test]
    fn movem_leaves_the_flags_alone() {
        let (mut cpu, mut bus) = run(0x4CD8, &[0x0103, 0x4E71]);
        cpu.set_ccr(ALL_FLAGS);
        bus.write_long(MEM, 0x1111_1111);
        bus.write_long(MEM + 4, 0x2222_2222);
        bus.write_long(MEM + 8, 0x3333_3333);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(cpu.ccr(), ALL_FLAGS, "neither direction touches a flag");
    }

    /// `MOVEM` is the only instruction that reaches the two stack modes without
    /// [`crate::M68k::resolve_ea`], so pin the order of the extension words too:
    /// for `MOVEM.L d16(A0),D0` — `$4CE8` — the list comes first at `pc+2` and the
    /// displacement second at `pc+4`, because `ea_address` is handed the queue
    /// after the list has been popped. Swapping the two reads is exactly what a
    /// `d16(An)` source catches.
    #[test]
    fn movem_reads_the_list_before_the_displacement() {
        let (mut cpu, mut bus) = run(0x4CE8, &[0x0001, 0x0010, 0x4E71]);
        bus.write_long(MEM + 0x10, 0x1234_5678);
        bus.write_long(MEM, 0xDEAD_BEEF);
        cpu.a[0] = MEM;

        cpu.step(&mut bus);

        assert_eq!(cpu.d[0], 0x1234_5678, "read from A0 + $10");
        assert_eq!(
            bus.read_long(MEM),
            0xDEAD_BEEF,
            "A0 itself is not the source"
        );
    }

    /// `TestBus` records nothing, and the one MOVEM behaviour that leaves no trace
    /// in the machine state is the extra word read a load issues after the last
    /// register (M68000UM MOVEM, "Note"), so this test wraps `TestBus` in a logging
    /// bus — the pattern `ea.rs`'s tests use — and reads the access list back.
    ///
    /// `MOVEM.W (A0)+,D0-D1` moves two words and touches memory three times: the
    /// extra read is the one at `A0 + 4`, above the last transfer. The store has no
    /// extra access at all, which is the half of this that shows the test is
    /// measuring the load rather than the queue.
    #[test]
    fn movem_load_reads_one_more_word_than_it_writes() {
        let writes = accessed_data_addresses(0x4890, &[0x0003, 0x4E71], |cpu, bus| {
            cpu.a[0] = MEM;
            let _ = bus;
        });
        assert_eq!(
            writes,
            vec![(true, MEM), (true, MEM + 2)],
            "a word store of D0/D1 to (A0) writes exactly two words"
        );

        let loads = accessed_data_addresses(0x4C98, &[0x0003, 0x4E71], |cpu, bus| {
            cpu.a[0] = MEM;
            bus.write_long(MEM, 0x1111_2222);
            bus.write_long(MEM + 4, 0x3333_4444);
        });
        assert_eq!(
            loads,
            vec![(false, MEM), (false, MEM + 2), (false, MEM + 4)],
            "the same list loaded back reads a third word: the prefetch cycle"
        );
    }

    /// The data-address accesses of one instruction, in order, as
    /// `(is_write, address)`: everything below [`MEM`] is the program stream and
    /// the queue, and is dropped.
    fn accessed_data_addresses(
        opcode: u16,
        rest: &[u16],
        setup: impl FnOnce(&mut M68k, &mut TestBus),
    ) -> Vec<(bool, u32)> {
        let (mut cpu, mut bus) = run(opcode, rest);
        setup(&mut cpu, &mut bus);
        let mut bus = Log::wrap(bus);
        cpu.step(&mut bus);
        bus.accesses
            .into_iter()
            .filter(|(_, addr)| *addr >= MEM)
            .collect()
    }

    /// A [`TestBus`] that records every access. `TestBus` deliberately has no log
    /// (its docs say so), and MOVEM is the first instruction whose extra bus traffic
    /// is worth pinning.
    struct Log {
        bus: TestBus,
        accesses: Vec<(bool, u32)>,
    }

    impl Log {
        fn wrap(bus: TestBus) -> Self {
            Self {
                bus,
                accesses: Vec::new(),
            }
        }
    }

    impl Bus for Log {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.accesses.push((false, addr));
            self.bus.read_byte(addr)
        }

        fn read_word(&mut self, addr: u32) -> u16 {
            self.accesses.push((false, addr));
            self.bus.read_word(addr)
        }

        fn write_byte(&mut self, addr: u32, value: u8) {
            self.accesses.push((true, addr));
            self.bus.write_byte(addr, value);
        }

        fn write_word(&mut self, addr: u32, value: u16) {
            self.accesses.push((true, addr));
            self.bus.write_word(addr, value);
        }
    }
}
