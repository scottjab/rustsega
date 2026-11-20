//! The Fujitsu SSP1601 DSP core: the processor half of the SVP board.
//!
//! This is a from-scratch interpreter written from the documented behaviour:
//! notaz's *SVP doc* (bundled with Genesis Plus GX as `svpdoc.txt`) is the
//! instruction spec - section 2.1 the general registers, 2.2 the external
//! registers and the PMC two-write protocol of 2.2.7, 2.3 the pointer
//! registers and RAM banks, 2.4 the instruction set - and Genesis Plus GX /
//! Picodrive (`core/cart_hw/svp/ssp16.c`, `pico/carthw/svp/ssp16.c`, both
//! GPL, read for behaviour and encodings only) resolved the corners the doc
//! leaves dark. Three of those are worth stating up front, because the two
//! references agree with each other and (where noted) diverge from the doc:
//!
//! * **AL is the accumulator's low word** (svpdoc 2.2.8; Picodrive's `rAL`
//!   is `gr[SSP_A].l`). Genesis Plus GX keeps a separate storage, which the
//!   doc contradicts.
//! * **a programmed PMx access leaves its word in PMC** (address low, mode
//!   high), so a PMC read answers the last access; a PMC read in the middle
//!   of the two-write sequence returns the address word rotated by nibbles
//!   (`(w << 4) & 0xfff0 | (w >> 4) & 0xf`), both references.
//! * **the special-increment DRAM mode advances by 1 word on even addresses
//!   and 31 on odd ones** (GPGX's `(addr&1) ? 31 : 1`), where svpdoc 2.2.7
//!   sketches "1, else 32". GPGX's figure is the one a working emulator
//!   ships, and the cell-arrange fill the game drives is what it tunes.
//!
//! The core holds no cartridge memory: program words, IRAM, DRAM and the
//! 68k-shared XST/PM0 latches come in through [`SspMemory`], which the board
//! ([`super::Svp`]) implements. XST and PM0 live on the board because both
//! sides of the handshake touch them (svpdoc 2.2.1: bit 0 is the SSP's
//! "written XST" and the 68k's read clears it; bit 1 is the 68k's "written
//! XST" and the SSP's read clears it) - each side's effect lands in its own
//! [`SspMemory`] method.
//!
//! Cycles: svpdoc section 4 says the SVP code "can be emulated without
//! knowing timing of the instructions" and that 10-12M instructions/sec "or
//! possibly less" is sufficient, so every instruction costs one cycle and a
//! PC write two (Genesis Plus GX's `write_PC` charging `g_cycles--`), and
//! the board is clocked one cycle per 68000 cycle by the scheduler.

use serde::{Deserialize, Serialize};

/// Levels in the hardware stack (svpdoc section 1: "Hardware stack of 6
/// levels"; 2.1.6 "STACK").
const STACK_LEVELS: usize = 6;

/// Words in one RAM bank (svpdoc 2.3: "Each bank has 256 words of RAM").
const RAM_BANK: usize = 256;

/// Status register flag bits (svpdoc 2.1.5: L, Z, OV, N in bits c-f).
const FLAG_L: u16 = 1 << 12;
const FLAG_Z: u16 = 1 << 13;
const FLAG_V: u16 = 1 << 14;
const FLAG_N: u16 = 1 << 15;

/// What the DSP reads and writes outside its own registers: the board's
/// memories and the two handshake latches it shares with the 68k.
pub(crate) trait SspMemory {
    /// Program memory word at `addr`: IRAM words `0x000-0x3FF`, ROM above
    /// (svpdoc section 3's program-space table).
    fn program(&mut self, addr: u16) -> u16;

    /// IRAM word write through the external window (svpdoc section 3: SVP
    /// space `0x390000-0x3907FF`, word `0x1C8000-0x1C83FF`); `addr` is
    /// already masked to the 0x400-word IRAM.
    fn write_iram(&mut self, addr: u16, value: u16);

    /// DRAM word at the SVP-space word address (DRAM words
    /// `0x180000-0x18FFFF`; the core hands the full address, the board
    /// masks).
    fn dram(&mut self, word_addr: u32) -> u16;

    /// DRAM word write. `overwrite` is the mode's `v` bit (svpdoc 2.2.7):
    /// only the non-zero nibbles of `value` land.
    fn write_dram(&mut self, word_addr: u32, value: u16, overwrite: bool);

    /// ROM word read (the game image at word `word_addr` of the program
    /// space), open bus past the image end.
    fn rom(&mut self, word_addr: u32) -> u16;

    /// XST read: the command/result register the SSP and the 68k share
    /// (svpdoc 2.2.4).
    fn xst(&mut self) -> u16;

    /// XST write: stores the word and sets PM0 bit 0, "set when SSP160x has
    /// written something to XST" (svpdoc 2.2.1).
    fn write_xst(&mut self, value: u16);

    /// PM0 read: the SSP's side of the handshake clears bit 1, "cleared on
    /// PM0 read by SSP160x" (svpdoc 2.2.1).
    fn pm0(&mut self) -> u16;

    /// PM0 write: the raw register store.
    fn write_pm0(&mut self, value: u16);
}

/// The SSP1601's registers and execution state. Everything here is plain
/// data so the save-state derive carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Ssp1601 {
    /// X: generic register and multiplier 1 (svpdoc 2.1.2).
    x: u16,
    /// Y: generic register and multiplier 2 (svpdoc 2.1.3).
    y: u16,
    /// A: 32-bit accumulator; AL is its low word (svpdoc 2.1.4, 2.2.8).
    a: u32,
    /// ST: status register (svpdoc 2.1.5). Bits 0-2 are the RPL modulo size,
    /// bits 12-15 the L, Z, V, N flags.
    st: u16,
    /// The hardware stack (svpdoc 2.1.6).
    stack: [u16; STACK_LEVELS],
    /// The level the next stack push takes (GPGX's `rSTACK`: starts at 0,
    /// wraps through 0-5).
    sp: u8,
    /// PC: program counter, in words (svpdoc 2.1.7).
    pc: u16,
    /// P: multiply product = X * Y * 2 on sign-extended inputs (svpdoc
    /// 2.1.8). Kept rather than recomputed on every read because `mpya` and
    /// `mpys` accumulate it and then refresh X/Y for the *next* product.
    p: u32,
    /// PM1/PM2/PM4 raw registers (svpdoc 2.2.2, 2.2.3, 2.2.5). PM0 and XST
    /// are the board's, shared with the 68k.
    pm1: u16,
    pm2: u16,
    pm4: u16,
    /// Programmable memory access words, `[read, write][slot]`, one slot per
    /// programmable external register: PM0, PM1, PM2, XST, PM4 (GPGX's
    /// `pmac[2][6]`, with its unused AL slot dropped - AL has no external
    /// window, see [`Ssp1601::reg_read`]). Low word = address, high word =
    /// mode.
    pmac: [[u32; 5]; 2],
    /// PMC: the programmable memory access control (svpdoc 2.2.7). Low word
    /// = the address just written, high word = the mode just written.
    pmc_addr: u16,
    pmc_mode: u16,
    /// The PMC two-write state: an address has been written and the next
    /// PMC write or read completes the pair (svpdoc 2.2.7's "waiting for
    /// mode").
    pmc_have_addr: bool,
    /// The pair is complete: the next blind PMx access programs it.
    pmc_set: bool,
    /// Pointer registers r0-r7, 8-bit (svpdoc 2.3). r0-r2 live in
    /// `ptr[0..3]`, r4-r6 in `ptr[4..7]`; r3 and r7 have no storage - their
    /// "register number" selects a fixed RAM word (`|00`-`|11` forms).
    ptr: [u8; 8],
    /// The two RAM banks, RAM0 = `ram[0..256]`, RAM1 = `ram[256..512]`
    /// (svpdoc 2.3; also the `ld adr, a` address space, 9 bits over both).
    #[serde(with = "crate::big_array")]
    ram: Box<[u16; RAM_BANK * 2]>,
}

/// The `ooo` operation code of the ALU group (svpdoc 2.4.1: 001 subtract,
/// 011 compare, 100 add, 101 and, 110 or, 111 exclusive or; 000 is the load
/// the references name `ld A, adr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alu {
    Load,
    Sub,
    Cmp,
    Add,
    And,
    Or,
    Eor,
}

impl Ssp1601 {
    /// The core as the board powers it up. The entry point is word 0x400
    /// (byte 0x800 of the program space - svpdoc section 4: "The entry point
    /// for the code seems to be at address 0x800 (word 0x400)"), ST and the
    /// PMC state clear. GR0 is the all-ones constant and has no storage:
    /// register 0 never stores.
    pub(crate) fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            a: 0,
            st: 0,
            stack: [0; STACK_LEVELS],
            sp: 0,
            pc: 0x400,
            p: 0,
            pm1: 0,
            pm2: 0,
            pm4: 0,
            pmac: [[0; 5]; 2],
            pmc_addr: 0,
            pmc_mode: 0,
            pmc_have_addr: false,
            pmc_set: false,
            ptr: [0; 8],
            ram: Box::new([0; RAM_BANK * 2]),
        }
    }

    /// What a soft reset does to the core: PC back to the entry point and ST
    /// and the PMC state clear (GPGX's `ssp1601_reset`, which touches PC, ST
    /// and its status flags and nothing else - the registers, the RAM banks
    /// and the stack survive, like the DRAM and the handshake latches do).
    pub(crate) fn reset(&mut self) {
        self.pc = 0x400;
        self.st = 0;
        self.pmc_have_addr = false;
        self.pmc_set = false;
    }

    /// Run up to `cycles` cycles and return how many were consumed: one per
    /// instruction, one more for a PC write (see the module docs).
    pub(crate) fn run(&mut self, mem: &mut impl SspMemory, cycles: u32) -> u32 {
        let mut used = 0;
        while used < cycles {
            let op = mem.program(self.pc);
            self.pc = self.pc.wrapping_add(1);
            used += 1 + u32::from(self.execute(op, mem));
        }
        used
    }

    /// Execute one decoded instruction; `true` if it wrote the PC (the
    /// extra-cycle case). The decode arm is the top seven bits of the
    /// instruction word, the rule svpdoc 2.4 opens with: "when writing an
    /// interpreter, 7 most significant bits are usually enough to determine
    /// which opcode it is".
    fn execute(&mut self, op: u16, mem: &mut impl SspMemory) -> bool {
        match op >> 9 {
            // ld d, s - the whole register-move group, with `ld -, -` the
            // nop (svpdoc 2.4.2) and `ld A, P` taking the whole 32-bit
            // product (svpdoc: "If dest. is A, and source is P, whole 32bit
            // value is transfered").
            0x00 if op == 0 => {}
            0x00 if op == (3 << 4) | 7 => {
                self.update_p();
                self.a = self.p;
            }
            0x00 => {
                let source = op & 0xF;
                let dest = (op >> 4) & 0xF;
                // The destination being `-` makes a PMx *read* blind; the
                // source being `-` makes a PMx *write* blind.
                let value = self.reg_read(mem, source, dest == 0);
                return self.reg_write(mem, dest, value, source == 0);
            }
            // ld d, (ri) / ld (ri), s
            0x01 => {
                let value = self.ptr1_read(op);
                self.reg_write(mem, (op >> 4) & 0xF, value, false);
            }
            0x02 => {
                let value = self.reg_read(mem, (op >> 4) & 0xF, false);
                self.ptr1_write(op, value);
            }
            // ldi d, imm / ldi (ri), imm
            0x04 => {
                let imm = mem.program(self.pc);
                self.pc += 1;
                self.reg_write(mem, (op >> 4) & 0xF, imm, false);
            }
            0x06 => {
                let imm = mem.program(self.pc);
                self.pc += 1;
                self.ptr1_write(op, imm);
            }
            // ld d, ((ri)) - program memory at the RAM cell, cell++
            0x05 => {
                let value = self.ptr2_read(op, mem);
                self.reg_write(mem, (op >> 4) & 0xF, value, false);
            }
            // ld adr, a - the 9-bit address spans both banks
            0x07 => self.ram[(op & 0x1FF) as usize] = (self.a >> 16) as u16,
            // ld d, ri / ld ri, s
            0x09 => {
                let value = u16::from(self.ptr[usize::from(Self::ij_index(op))]);
                self.reg_write(mem, (op >> 4) & 0xF, value, false);
            }
            0x0a => {
                let value = self.reg_read(mem, (op >> 4) & 0xF, false);
                self.ptr[usize::from(Self::ij_index(op))] = value as u8;
            }
            // ldi ri, simm - 8-bit immediate in the instruction word itself
            // (svpdoc 2.4.2: `0001 1jpp iiii iiii`, a signed byte)
            0x0c..=0x0f => self.ptr[((op >> 8) & 7) as usize] = op as u8,
            // call cond, addr (svpdoc 2.4.3)
            0x24 => {
                if self.cond(op) {
                    let target = mem.program(self.pc);
                    self.pc += 1;
                    self.stack_push(self.pc);
                    self.pc = target;
                    return true;
                }
                self.pc += 1;
            }
            // ld d, (a) - program memory at the accumulator's upper word
            0x25 => {
                let value = mem.program((self.a >> 16) as u16);
                self.reg_write(mem, (op >> 4) & 0xF, value, false);
            }
            // bra cond, addr
            0x26 => {
                if self.cond(op) {
                    self.pc = mem.program(self.pc);
                    return true;
                }
                self.pc += 1;
            }
            // mod cond, op - accumulator modifiers under a condition
            // (svpdoc 2.4.1's trailing table)
            0x48 => {
                if self.cond(op) {
                    match op & 7 {
                        2 => self.a = ((self.a as i32) >> 1) as u32,
                        3 => self.a = self.a.wrapping_shl(1),
                        6 => self.a = (self.a as i32).wrapping_neg() as u32,
                        7 if (self.a as i32) < 0 => self.a = (self.a as i32).wrapping_neg() as u32,
                        _ => {}
                    }
                    self.flags_zn();
                }
            }
            // mpys / mpya / mld (rj), (ri) - svpdoc 2.4.4
            0x1b => {
                self.update_p();
                self.a = self.a.wrapping_sub(self.p);
                self.flags_lzvn();
                self.mac_load(op);
            }
            0x4b => {
                self.update_p();
                self.a = self.a.wrapping_add(self.p);
                self.flags_lzvn();
                self.mac_load(op);
            }
            0x5b => {
                self.a = 0;
                self.st &= 0x0FFF;
                self.mac_load(op);
            }
            // OP A, s - the register source (svpdoc 2.4.1), where the two
            // 32-bit sources A and P take their whole words (GPGX's
            // OP_CHECK32)
            0x10 => self.op_reg(op, mem, Alu::Sub),
            0x30 => self.op_reg(op, mem, Alu::Cmp),
            0x40 => self.op_reg(op, mem, Alu::Add),
            0x50 => self.op_reg(op, mem, Alu::And),
            0x60 => self.op_reg(op, mem, Alu::Or),
            0x70 => self.op_reg(op, mem, Alu::Eor),
            // OP A, (ri)
            0x11 => self.op_ptr1(op, Alu::Sub),
            0x31 => self.op_ptr1(op, Alu::Cmp),
            0x41 => self.op_ptr1(op, Alu::Add),
            0x51 => self.op_ptr1(op, Alu::And),
            0x61 => self.op_ptr1(op, Alu::Or),
            0x71 => self.op_ptr1(op, Alu::Eor),
            // OP A, adr - and the ooo=000 form, a load: A <- RAM[adr] with
            // no flag change (GPGX's OP_LDA; svpdoc's table only lists
            // 001-111)
            0x03 => {
                let value = self.ram[(op & 0x1FF) as usize];
                self.a = (self.a & 0xFFFF) | (u32::from(value) << 16);
            }
            0x13 => self.op_adr(op, Alu::Sub),
            0x33 => self.op_adr(op, Alu::Cmp),
            0x43 => self.op_adr(op, Alu::Add),
            0x53 => self.op_adr(op, Alu::And),
            0x63 => self.op_adr(op, Alu::Or),
            0x73 => self.op_adr(op, Alu::Eor),
            // OP A, imm (16-bit immediate extension word)
            0x14 | 0x34 | 0x44 | 0x54 | 0x64 | 0x74 => {
                let imm = mem.program(self.pc);
                self.pc += 1;
                self.op_alu(imm, alu_of(op >> 9));
            }
            // OP A, ((ri))
            0x15 => self.op_ptr2(op, mem, Alu::Sub),
            0x35 => self.op_ptr2(op, mem, Alu::Cmp),
            0x45 => self.op_ptr2(op, mem, Alu::Add),
            0x55 => self.op_ptr2(op, mem, Alu::And),
            0x65 => self.op_ptr2(op, mem, Alu::Or),
            0x75 => self.op_ptr2(op, mem, Alu::Eor),
            // OP A, ri
            0x19 | 0x39 | 0x49 | 0x59 | 0x69 | 0x79 => {
                let value = u16::from(self.ptr[usize::from(Self::ij_index(op))]);
                self.op_alu(value, alu_of(op >> 9));
            }
            // OP simm - 8-bit immediate entering the upper word (svpdoc:
            // A OP i << 16; the references warn the upper bit is unused)
            0x1c | 0x3c | 0x4c | 0x5c | 0x6c | 0x7c => self.op_alu(op & 0xFF, alu_of(op >> 9)),
            _ => {}
        }
        false
    }

    /// The 16-bit value register `r` answers with: the general registers by
    /// number (svpdoc 2.4's ordering, "0 is '-', 1 'X', ... 0xf is 'AL'"),
    /// the external registers through their programmable window with a raw
    /// fallback. `dest_is_minus` says the destination is `-`, which makes a
    /// PMx read the blind access that programs the PMC (svpdoc 2.2.7: "PMx
    /// should be blind accessed using (ld -, PMx) or (ld PMx, -) to program
    /// it").
    fn reg_read(&mut self, mem: &mut impl SspMemory, r: u16, dest_is_minus: bool) -> u16 {
        match r {
            0 => 0xFFFF,
            1 => self.x,
            2 => self.y,
            // A answers its upper word (svpdoc 2.1.4)
            3 => (self.a >> 16) as u16,
            4 => self.st,
            5 => self.stack_pop(),
            6 => self.pc,
            7 => {
                self.update_p();
                (self.p >> 16) as u16
            }
            8 => self
                .pm_io(mem, 0, false, 0, dest_is_minus)
                .unwrap_or_else(|| mem.pm0()),
            9 => self
                .pm_io(mem, 1, false, 0, dest_is_minus)
                .unwrap_or(self.pm1),
            10 => self
                .pm_io(mem, 2, false, 0, dest_is_minus)
                .unwrap_or(self.pm2),
            11 => self
                .pm_io(mem, 3, false, 0, dest_is_minus)
                .unwrap_or_else(|| mem.xst()),
            12 => self
                .pm_io(mem, 4, false, 0, dest_is_minus)
                .unwrap_or(self.pm4),
            // EXT5: "Not used by SVP, so not covered by this document"
            // (svpdoc 2.2.6) - there is nothing behind it.
            13 => 0,
            14 => self.pmc_read(),
            // AL: the accumulator's low word (svpdoc 2.2.8; Picodrive's
            // `rAL = gr[SSP_A].l`). A blind AL access while a PMC pair is
            // armed is the "dummy programmed" that resets the pair instead
            // (svpdoc 2.2.8: "nothing happens and PMC is reset to 'waiting
            // for address' state").
            15 => {
                if dest_is_minus && self.pmc_set {
                    self.pmc_set = false;
                    self.pmc_have_addr = false;
                }
                self.a as u16
            }
            _ => 0,
        }
    }

    /// Store `value` into register `r`; `true` when `r` is the PC (the
    /// direct-write jump that costs the extra cycle). `source_is_minus` is
    /// the blind marker of [`Ssp1601::reg_read`], mirrored for writes.
    fn reg_write(
        &mut self,
        mem: &mut impl SspMemory,
        r: u16,
        value: u16,
        source_is_minus: bool,
    ) -> bool {
        match r {
            // `-` is a constant, and P is not writable (svpdoc 2.1.8: "It is
            // not clear if P can be destination operand (probably not)")
            0 | 7 => {}
            1 => self.x = value,
            2 => self.y = value,
            3 => self.a = (self.a & 0xFFFF) | (u32::from(value) << 16),
            4 => self.st = value,
            5 => self.stack_push(value),
            // a direct PC write is a jump
            6 => self.pc = value,
            8 => {
                if self.pm_io(mem, 0, true, value, source_is_minus).is_none() {
                    mem.write_pm0(value);
                }
            }
            9 => {
                if self.pm_io(mem, 1, true, value, source_is_minus).is_none() {
                    self.pm1 = value;
                }
            }
            10 => {
                if self.pm_io(mem, 2, true, value, source_is_minus).is_none() {
                    self.pm2 = value;
                }
            }
            11 => {
                if self.pm_io(mem, 3, true, value, source_is_minus).is_none() {
                    mem.write_xst(value);
                }
            }
            12 => {
                if self.pm_io(mem, 4, true, value, source_is_minus).is_none() {
                    self.pm4 = value;
                }
            }
            13 => {}
            14 => self.pmc_write(value),
            // AL writes the accumulator's low word, with the same blind
            // dummy as the read
            15 => {
                if source_is_minus && self.pmc_set {
                    self.pmc_set = false;
                    self.pmc_have_addr = false;
                }
                self.a = (self.a & 0xFFFF0000) | u32::from(value);
            }
            _ => {}
        }
        r == 6
    }

    /// The PMC register read, half of the two-write protocol (svpdoc 2.2.7:
    /// "It is set using 2 16bit writes, first address, then mode word", and
    /// reads shift the state the same way). The read after the mode word
    /// answers the address word rotated by nibbles - the transform both
    /// references ship.
    fn pmc_read(&mut self) -> u16 {
        if self.pmc_have_addr {
            self.pmc_set = true;
            self.pmc_have_addr = false;
            ((self.pmc_addr << 4) & 0xFFF0) | ((self.pmc_addr >> 4) & 0xF)
        } else {
            self.pmc_have_addr = true;
            self.pmc_addr
        }
    }

    /// The PMC register write: the address word, then the mode word, which
    /// arms the next blind access in [`Ssp1601::pm_io`].
    fn pmc_write(&mut self, value: u16) {
        if self.pmc_have_addr {
            self.pmc_set = true;
            self.pmc_have_addr = false;
            self.pmc_mode = value;
        } else {
            self.pmc_have_addr = true;
            self.pmc_addr = value;
        }
    }

    /// One access to external register `reg` (0 PM0, 1 PM1, 2 PM2, 3 XST,
    /// 4 PM4): either the programmed external window, or `None` for the raw
    /// register - which the caller answers, because the raw side effects are
    /// per register (XST writes set PM0 bit 0, PM0 reads clear bit 1, on the
    /// board).
    ///
    /// svpdoc 2.2.5 gives PM4 the always-external rule ("Not affected by ST5
    /// and ST6 bits, always stays in PMAR mode") and 2.2.1-2.2.4 the
    /// ST5/ST6 rule for the rest ("If ST5 or ST6 is set, acts as
    /// Programmable Memory access register").
    fn pm_io(
        &mut self,
        mem: &mut impl SspMemory,
        reg: usize,
        write: bool,
        value: u16,
        blind: bool,
    ) -> Option<u16> {
        // The programming stroke: the PMC pair, landed by the blind access.
        // A non-blind access while the pair is armed does not program - the
        // reference discards it (GPGX's "tried to set PM%i with non-blind
        // i/o") and the access reads zero.
        if self.pmc_set {
            self.pmc_set = false;
            if blind {
                self.pmac[usize::from(write)][reg] =
                    (u32::from(self.pmc_mode) << 16) | u32::from(self.pmc_addr);
                return Some(0);
            }
        }
        // An access with only half a pair is dropped like the reference's
        // ("FIXME: PM%i (%c) with only addr set"), leaving the raw register.
        if self.pmc_have_addr {
            self.pmc_have_addr = false;
        }
        if reg != 4 && self.st & 0x60 == 0 {
            return None;
        }
        let pmac = self.pmac[usize::from(write)][reg];
        let mode = (pmac >> 16) as u16;
        let addr = pmac & 0xFFFF;
        let word_addr = (u32::from(mode & 0x7F) << 16) | addr;
        let inc = Self::increment(mode);
        let mut answered = Some(0);
        if write {
            if mode & 0x43FF == 0x0018 {
                // DRAM (svpdoc section 3: SVP space 0x300000-0x31FFFF),
                // the mode's `v` bit splitting the word into nibbles
                mem.write_dram(word_addr, value, mode & 0x0400 != 0);
                self.pmac[1][reg] = pmac.wrapping_add(inc);
            } else if mode & 0xFBFF == 0x4018 {
                // DRAM, cell increment: the special-increment `s` mode of
                // svpdoc 2.2.7. +1 word on an even address, +31 on an odd
                // one - GPGX's `(addr&1) ? 31 : 1`, a word less than the
                // doc's sketch of 32 (see the module docs).
                mem.write_dram(word_addr, value, mode & 0x0400 != 0);
                self.pmac[1][reg] = pmac.wrapping_add(if addr & 1 == 1 { 31 } else { 1 });
            } else if mode & 0x47FF == 0x001C {
                // IRAM (SVP space 0x390000-0x3907FF, word 0x1C8000-0x1C83FF)
                mem.write_iram((word_addr & 0x3FF) as u16, value);
                self.pmac[1][reg] = pmac.wrapping_add(inc);
            }
        } else if mode & 0xFFF0 == 0x0800 {
            // ROM, increment 1 - "verified to be correct" in the reference
            answered = Some(mem.rom(word_addr));
            self.pmac[0][reg] = pmac.wrapping_add(1);
        } else if mode & 0x47FF == 0x0018 {
            answered = Some(mem.dram(word_addr));
            self.pmac[0][reg] = pmac.wrapping_add(inc);
        }
        // "PMC value corresponds to last PMR accessed" (the reference's
        // `rPMC.v = pmac[write][reg]`)
        let last = self.pmac[usize::from(write)][reg];
        self.pmc_addr = last as u16;
        self.pmc_mode = (last >> 16) as u16;
        answered
    }

    /// The auto-increment the mode's `nnn` bits name (svpdoc 2.2.7's table:
    /// 1-7 increment by 1, 2, 4, 8, 16, 32, 128 words), negated by the `d`
    /// bit, zero when `nnn` is 0.
    fn increment(mode: u16) -> u32 {
        let n = (mode >> 11) & 7;
        if n == 0 {
            return 0;
        }
        let inc = 1u32 << if n == 7 { 7 } else { n - 1 };
        if mode & 0x8000 != 0 {
            inc.wrapping_neg()
        } else {
            inc
        }
    }

    /// The 5-bit pointer field of an instruction: pp = bits 1-0, j = bit 7,
    /// modifier = bits 3-2 (GPGX's `t = (op&3) | ((op>>6)&4) | ((op<<1)&0x18)`).
    fn ptr_field(op: u16) -> u16 {
        (op & 3) | ((op >> 6) & 4) | ((op << 1) & 0x18)
    }

    /// Program memory through the first indirection `(rX)`: the RAM word the
    /// pointer names, with the modifier applied to the pointer (svpdoc 2.3's
    /// `pr_modif_read`; `+!` unaffected by RPL, `-` and `+` modulo by
    /// 2^RPL). Register 3 and 7 have no storage: their modifier selects the
    /// fixed RAM word `|00`-`|11` instead.
    fn ptr1_read(&mut self, op: u16) -> u16 {
        self.ptr_read(Self::ptr_field(op))
    }

    /// [`Ssp1601::ptr1_read`] over an already-assembled field.
    fn ptr_read(&mut self, t: u16) -> u16 {
        let (index, next) = self.ptr_index(t);
        if let Some(next) = next {
            self.ptr[Self::ptr_slot(t)] = next;
        }
        self.ram[bank_offset(t, index)]
    }

    /// The RAM word the pointer names, with the modifier applied, as
    /// [`Ssp1601::ptr_read`] resolves it. The write modifiers are cruder
    /// than the read's: `+` writes increment without the RPL modulo and
    /// `-` decrements without it, the way the reference's `ptr1_write`
    /// does.
    fn ptr1_write(&mut self, op: u16, value: u16) {
        let t = Self::ptr_field(op);
        let (index, next) = self.ptr_write_index(t);
        if let Some(next) = next {
            self.ptr[Self::ptr_slot(t)] = next;
        }
        self.ram[bank_offset(t, index)] = value;
    }

    /// The pointer array slot a field names: pp 0-2 of the bank (r0-r2 in
    /// `ptr[0..3]`, r4-r6 in `ptr[4..7]`; pp 3 is the fixed form and never
    /// indexes).
    fn ptr_slot(t: u16) -> usize {
        (usize::from(t >> 2 & 1)) * 4 + (t & 3) as usize
    }

    /// The RAM word index a read field answers at, and the pointer's next
    /// value if the modifier moves it. The fixed form (`pp == 3`) answers
    /// RAMx[modifier] and never moves; `+!` moves plain, `-` and `+` move
    /// under the RPL modulo (svpdoc 2.1.5: RPL "shows which power of 2 to
    /// use, i.e. 4 means modulo by 16"; 0 is no modulo).
    fn ptr_index(&self, t: u16) -> (usize, Option<u8>) {
        let modifier = (t >> 3) as usize;
        if t & 3 == 3 {
            return (modifier, None);
        }
        let ptr = self.ptr[Self::ptr_slot(t)];
        let rpl = self.st & 7;
        match t >> 3 {
            0 => (usize::from(ptr), None),
            // +!
            1 => (usize::from(ptr), Some(ptr.wrapping_add(1))),
            // - (RPL modulo)
            2 => (usize::from(ptr), Some(Self::rpl(ptr, -1, rpl))),
            // + (RPL modulo)
            _ => (usize::from(ptr), Some(Self::rpl(ptr, 1, rpl))),
        }
    }

    /// The write form's index and pointer move: no RPL anywhere, `-`
    /// plain decrement, `+!` and `+` plain increment.
    fn ptr_write_index(&self, t: u16) -> (usize, Option<u8>) {
        let modifier = (t >> 3) as usize;
        if t & 3 == 3 {
            return (modifier, None);
        }
        let ptr = self.ptr[Self::ptr_slot(t)];
        match t >> 3 {
            0 => (usize::from(ptr), None),
            2 => (usize::from(ptr), Some(ptr.wrapping_sub(1))),
            _ => (usize::from(ptr), Some(ptr.wrapping_add(1))),
        }
    }

    /// One pointer step under the RPL modulo.
    fn rpl(ptr: u8, add: i8, rpl: u16) -> u8 {
        if rpl == 0 {
            ptr.wrapping_add_signed(add)
        } else {
            let mask = ((1u16 << rpl) - 1) as u8;
            (ptr & !mask) | (ptr.wrapping_add_signed(add) & mask)
        }
    }

    /// Program memory through the second indirection `((rX))`: the RAM cell
    /// holds a program word address, read and then incremented in the cell
    /// (svpdoc 2.3: "((rX)) also performs post-increment"). Only the plain
    /// (`mod=0`) and fixed forms exist here - the reference answers zero
    /// for a second indirection through a modified pointer, without
    /// touching anything.
    fn ptr2_read(&mut self, op: u16, mem: &mut impl SspMemory) -> u16 {
        let t = Self::ptr_field(op);
        let pp = t & 3;
        let modifier = (t >> 3) as usize;
        let index = if pp == 3 {
            modifier
        } else if modifier == 0 {
            usize::from(self.ptr[Self::ptr_slot(t)])
        } else {
            return 0;
        };
        let cell = &mut self.ram[bank_offset(t, index)];
        let target = *cell;
        *cell = cell.wrapping_add(1);
        mem.program(target)
    }

    /// The `ri`/`rj` index an `ld d, ri`-form instruction names: bits 1-0
    /// plus bit 7 as the bank (`0001 001j dddd 00pp`).
    fn ij_index(op: u16) -> u8 {
        (((op >> 6) & 4) | (op & 3)) as u8
    }

    /// The condition of a `call`/`bra`/`mod`: `cccc` in bits 7-4 (0 always,
    /// 5 the Z flag, 7 the N flag) against the sense bit `f` at bit 8
    /// (svpdoc 2.4's `check_cond`; GPGX's COND_CHECK, whose `(op<<5)` and
    /// `(op<<7)` shifts land `f` on the flag). Conditions other than 0, 5
    /// and 7 are not used by SVP code and answer false like the reference's.
    fn cond(&self, op: u16) -> bool {
        let f = (op >> 8) & 1;
        match (op >> 4) & 0xF {
            0 => true,
            5 => u16::from(self.st & FLAG_Z != 0) == f,
            7 => u16::from(self.st & FLAG_N != 0) == f,
            _ => false,
        }
    }

    /// Push a word on the hardware stack (svpdoc 2.1.6: values are pushed
    /// "by directly writing to it"). The level wraps through 0-5 the way
    /// the reference's does.
    fn stack_push(&mut self, value: u16) {
        if usize::from(self.sp) >= STACK_LEVELS {
            self.sp = 0;
        }
        self.stack[usize::from(self.sp)] = value;
        self.sp += 1;
    }

    /// Pop the hardware stack.
    fn stack_pop(&mut self) -> u16 {
        self.sp = self.sp.wrapping_sub(1);
        if (self.sp as i8) < 0 {
            self.sp = (STACK_LEVELS - 1) as u8;
        }
        self.stack[usize::from(self.sp)]
    }

    /// P = X * Y * 2 on sign-extended inputs (svpdoc 2.1.8).
    fn update_p(&mut self) {
        self.p = ((self.x as i16 as i32).wrapping_mul(self.y as i16 as i32)).wrapping_mul(2) as u32;
    }

    /// Flags after an arithmetic ALU operation: L, V cleared, Z and N from
    /// the full 32-bit accumulator (the reference's UPD_LZVN; "The SVP code
    /// only checks N and Z", svpdoc 2.4.1).
    fn flags_lzvn(&mut self) {
        self.st &= !(FLAG_L | FLAG_Z | FLAG_V | FLAG_N);
        self.flags_zn();
    }

    /// Flags after a logic ALU operation or a `mod`: Z and N only, L and V
    /// untouched (the reference's UPD_ACC_ZN).
    fn flags_zn(&mut self) {
        self.st &= !(FLAG_Z | FLAG_N);
        if self.a == 0 {
            self.st |= FLAG_Z;
        } else {
            self.st |= ((self.a >> 16) as u16) & FLAG_N;
        }
    }

    /// `OP A, reg`: the register source, with the two 32-bit sources A and P
    /// taking their whole words (GPGX's OP_CHECK32).
    fn op_reg(&mut self, op: u16, mem: &mut impl SspMemory, alu: Alu) {
        let r = op & 0xF;
        if r == 7 {
            self.update_p();
            let p = self.p;
            self.op_alu32(p, alu);
        } else if r == 3 {
            let a = self.a;
            self.op_alu32(a, alu);
        } else {
            let value = self.reg_read(mem, r, false);
            self.op_alu(value, alu);
        }
    }

    /// `OP A, (ri)`.
    fn op_ptr1(&mut self, op: u16, alu: Alu) {
        let value = self.ptr1_read(op);
        self.op_alu(value, alu);
    }

    /// `OP A, ((ri))`.
    fn op_ptr2(&mut self, op: u16, mem: &mut impl SspMemory, alu: Alu) {
        let value = self.ptr2_read(op, mem);
        self.op_alu(value, alu);
    }

    /// `OP A, adr`.
    fn op_adr(&mut self, op: u16, alu: Alu) {
        let value = self.ram[(op & 0x1FF) as usize];
        self.op_alu(value, alu);
    }

    /// One 16-bit source ALU operation (svpdoc 2.4.1: the source enters the
    /// operation shifted into the accumulator's upper word, so `(andi A, 0)`
    /// clears all 32 bits of A). `cmp` runs the operation without keeping
    /// the result.
    fn op_alu(&mut self, value: u16, alu: Alu) {
        let source = u32::from(value) << 16;
        match alu {
            Alu::Load => {
                self.a = (self.a & 0xFFFF) | source;
            }
            Alu::Sub => {
                self.a = self.a.wrapping_sub(source);
                self.flags_lzvn();
            }
            Alu::Cmp => {
                let result = self.a.wrapping_sub(source);
                let saved = self.a;
                self.a = result;
                self.flags_lzvn();
                self.a = saved;
            }
            Alu::Add => {
                self.a = self.a.wrapping_add(source);
                self.flags_lzvn();
            }
            Alu::And => {
                self.a &= source;
                self.flags_zn();
            }
            Alu::Or => {
                self.a |= source;
                self.flags_zn();
            }
            Alu::Eor => {
                self.a ^= source;
                self.flags_zn();
            }
        }
    }

    /// One 32-bit source ALU operation (the A and P sources of `OP A, s`).
    fn op_alu32(&mut self, value: u32, alu: Alu) {
        match alu {
            Alu::Load => self.a = value,
            Alu::Sub => {
                self.a = self.a.wrapping_sub(value);
                self.flags_lzvn();
            }
            Alu::Cmp => {
                let result = self.a.wrapping_sub(value);
                let saved = self.a;
                self.a = result;
                self.flags_lzvn();
                self.a = saved;
            }
            Alu::Add => {
                self.a = self.a.wrapping_add(value);
                self.flags_lzvn();
            }
            Alu::And => {
                self.a &= value;
                self.flags_zn();
            }
            Alu::Or => {
                self.a |= value;
                self.flags_zn();
            }
            Alu::Eor => {
                self.a ^= value;
                self.flags_zn();
            }
        }
    }

    /// The MAC group's shared tail: load X from `(ri)` in RAM0 and Y from
    /// `(rj)` in RAM1, each with its own modifier field (X: instruction
    /// bits 1-0 index and 3-2 modifier; Y: bits 5-4 index and 7-6 modifier;
    /// the references' `ptr1_read_(op&3, 0, (op<<1)&0x18)` and
    /// `ptr1_read_((op>>4)&3, 4, (op>>3)&0x18)`).
    fn mac_load(&mut self, op: u16) {
        let ri = (op & 3) | ((op << 1) & 0x18);
        let rj = ((op >> 4) & 3) | 4 | ((op >> 3) & 0x18);
        self.x = self.ptr_read(ri);
        self.y = self.ptr_read(rj);
    }
}

/// The `ooo` code of an `OP A, x` decode arm: the three bits above the
/// arm's low nibble are the operation, and arm 0 is the load (`ld A, adr`).
fn alu_of(arm: u16) -> Alu {
    match (arm >> 4) & 7 {
        0 => Alu::Load,
        1 => Alu::Sub,
        3 => Alu::Cmp,
        4 => Alu::Add,
        5 => Alu::And,
        6 => Alu::Or,
        _ => Alu::Eor,
    }
}

/// The word offset of RAM bank `bank` (0 or 1) at word `i`, from a pointer
/// field's bank bit and a masked index (the RAM is 256 words a bank; the
/// pointer is 8-bit and wraps into the bank).
fn bank_offset(t: u16, index: usize) -> usize {
    (usize::from(t >> 2 & 1)) * RAM_BANK + (index & (RAM_BANK - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal [`SspMemory`]: a ROM of words the program runs in, an IRAM,
    /// a DRAM of words, and the two handshake latches.
    struct TestMem {
        rom: Vec<u16>,
        iram: [u16; 0x400],
        dram: Vec<u16>,
        xst: u16,
        pm0: u16,
    }

    impl TestMem {
        /// A program built out of pieces, flattened in order.
        fn rom_of(pieces: &[&[u16]]) -> Self {
            Self::rom_words(&pieces.concat())
        }
        fn rom_words(words: &[u16]) -> Self {
            Self {
                rom: words.to_vec(),
                iram: [0; 0x400],
                dram: vec![0; 0x10000],
                xst: 0,
                pm0: 0,
            }
        }
    }

    impl SspMemory for TestMem {
        fn program(&mut self, addr: u16) -> u16 {
            if usize::from(addr) < 0x400 {
                self.iram[usize::from(addr)]
            } else {
                self.rom
                    .get(usize::from(addr) - 0x400)
                    .copied()
                    .unwrap_or(0xFFFF)
            }
        }
        fn write_iram(&mut self, addr: u16, value: u16) {
            self.iram[usize::from(addr)] = value;
        }
        fn dram(&mut self, word_addr: u32) -> u16 {
            self.dram[(word_addr & 0xFFFF) as usize]
        }
        fn write_dram(&mut self, word_addr: u32, value: u16, overwrite: bool) {
            let cell = &mut self.dram[(word_addr & 0xFFFF) as usize];
            if overwrite {
                for shift in [12, 8, 4, 0] {
                    let nibble = value & (0xF << shift);
                    if nibble != 0 {
                        *cell = (*cell & !(0xF << shift)) | nibble;
                    }
                }
            } else {
                *cell = value;
            }
        }
        fn rom(&mut self, word_addr: u32) -> u16 {
            self.rom.get(word_addr as usize).copied().unwrap_or(0xFFFF)
        }
        fn xst(&mut self) -> u16 {
            self.xst
        }
        fn write_xst(&mut self, value: u16) {
            self.xst = value;
            self.pm0 |= 1;
        }
        fn pm0(&mut self) -> u16 {
            let pm0 = self.pm0;
            self.pm0 &= !2;
            pm0
        }
        fn write_pm0(&mut self, value: u16) {
            self.pm0 = value;
        }
    }

    /// A core parked at the start of `rom`.
    fn fresh(mem: &mut TestMem) -> Ssp1601 {
        let mut core = Ssp1601::new();
        core.pc = 0x400;
        let _ = mem;
        core
    }

    /// Run `core` for `cycles`, returning the cycles consumed.
    fn run(core: &mut Ssp1601, mem: &mut TestMem, cycles: u32) -> u32 {
        core.run(mem, cycles)
    }

    /// A fresh core parked at the start of `rom`, run for `cycles`.
    fn run_fresh(mem: &mut TestMem, cycles: u32) -> (Ssp1601, u32) {
        let mut core = fresh(mem);
        let used = run(&mut core, mem, cycles);
        (core, used)
    }

    /// As [`run_fresh`], with `st` preloaded (flag and RPL tests).
    fn run_with_st(mem: &mut TestMem, st: u16, cycles: u32) -> Ssp1601 {
        let mut core = fresh(mem);
        core.st = st;
        run(&mut core, mem, cycles);
        core
    }

    #[test]
    fn dbg_four() {
        // 1. ld X, -
        let mut mem = TestMem::rom_of(&[&ldi(2, 0xBEEF), &[ld(1, 2), ld(1, 0)]]);
        let mut core = Ssp1601::new();
        core.pc = 0x400;
        for step in 0..4 {
            let op = mem.program(core.pc);
            core.run(&mut mem, 1);
            println!(
                "1. step {step}: op={op:04X} pc={:04X} x={:04X}",
                core.pc, core.x
            );
        }
        // 2. special increment
        let mut mem = TestMem::rom_of(&[
            &ldi(14, 0x0000),
            &ldi(14, 0x4018),
            &[ld(0, 12), ld(1, 12), ld(2, 12)],
        ]);
        let mut core = Ssp1601::new();
        core.pc = 0x400;
        for step in 0..7 {
            let op = mem.program(core.pc);
            core.run(&mut mem, 1);
            println!(
                "2. step {step}: op={op:04X} pc={:04X} pmac_r4={:08X} x={:04X}",
                core.pc, core.pmac[0][4], core.x
            );
        }
        // 3. plain pointer + : 15 -> 16 -> 17
        let mut mem = TestMem::rom_words(&[ldi_ri(0, 1, 15), ld_p1(1, 0, 3, 1), ld_p1(1, 0, 3, 1)]);
        let mut core = Ssp1601::new();
        core.pc = 0x400;
        for step in 0..4 {
            let op = mem.program(core.pc);
            core.run(&mut mem, 1);
            println!(
                "3. step {step}: op={op:04X} pc={:04X} r1={:04X}",
                core.pc, core.ptr[1]
            );
        }
        // 4. z condition fall-through
        let mut mem = TestMem::rom_of(&[
            &op_imm(3, 0x0000),
            &bra(0, 5, 0x404),
            &[ld(1, 1), 0x600D, 0x600D],
        ]);
        let mut core = Ssp1601::new();
        core.pc = 0x400;
        for step in 0..6 {
            let op = mem.program(core.pc);
            core.run(&mut mem, 1);
            println!(
                "4. step {step}: op={op:04X} pc={:04X} st={:04X}",
                core.pc, core.st
            );
        }
    }

    // ---- instruction encodings (svpdoc 2.4's tables) --------------------

    /// `ld d, s`: destination and source in the low byte.
    fn ld(dest: u16, source: u16) -> u16 {
        (dest << 4) | source
    }
    /// `ldi d, imm`: `0000 1000 dddd 0000` + word.
    fn ldi(dest: u16, imm: u16) -> [u16; 2] {
        [0x0800 | (dest << 4), imm]
    }
    /// `ld d, (ri)`: `0000 001j dddd mmpp` - j bit 8, d bits 7-4, modifier
    /// bits 3-2, pp bits 1-0.
    fn ld_p1(dest: u16, j: u16, modifier: u16, pp: u16) -> u16 {
        0x0200 | (j << 8) | (dest << 4) | (modifier << 2) | pp
    }
    /// `ld (ri), s`: `0000 010j ssss mmpp`.
    fn ld_p1_store(j: u16, source: u16, modifier: u16, pp: u16) -> u16 {
        0x0400 | (j << 8) | (source << 4) | (modifier << 2) | pp
    }
    /// `ldi (ri), imm`: `0000 110l 0000 mmpp` + word (the doc's `l` is the
    /// immediate's bit 8; the references store the whole word).
    fn ldi_p1(j: u16, modifier: u16, pp: u16, imm: u16) -> [u16; 2] {
        [0x0C00 | (j << 8) | (modifier << 2) | pp, imm]
    }
    /// `ld d, ((ri))`: `0000 101j dddd mmpp`.
    fn ld_p2(dest: u16, j: u16, modifier: u16, pp: u16) -> u16 {
        0x0A00 | (j << 8) | (dest << 4) | (modifier << 2) | pp
    }
    /// `ld d, ri`: `0001 001j dddd 00pp`.
    fn ld_ri(dest: u16, j: u16, pp: u16) -> u16 {
        0x1200 | (j << 8) | (dest << 4) | pp
    }
    /// `ldi ri, simm`: `0001 1jpp iiii iiii`, one word.
    fn ldi_ri(j: u16, pp: u16, imm: u8) -> u16 {
        0x1800 | (j << 10) | (pp << 8) | u16::from(imm)
    }
    /// `ld adr, a`: `0000 111j aaaa aaaa`.
    fn ld_adr_a(j: u16, addr: u16) -> u16 {
        0x0E00 | (j << 8) | addr
    }
    /// `bra cond, addr`: `0100 110f cccc 0000`.
    fn bra(f: u16, cond: u16, target: u16) -> [u16; 2] {
        [0x4C00 | (f << 8) | (cond << 4), target]
    }
    /// `call cond, addr`: `0100 100f cccc 0000`.
    fn call(f: u16, cond: u16, target: u16) -> [u16; 2] {
        [0x4900 | (f << 8) | (cond << 4), target]
    }
    /// `mod cond, op`: `1001 000f cccc 0ooo`.
    fn mod_op(f: u16, cond: u16, o: u16) -> u16 {
        0x9000 | (f << 8) | (cond << 4) | o
    }
    /// `OP A, s` for the register source (ooo = sub 1, cmp 3, add 4, and 5,
    /// or 6, eor 7).
    fn op_reg(ooo: u16, source: u16) -> u16 {
        (ooo << 13) | source
    }
    /// `OP A, imm` (ooo + the 16-bit extension word).
    fn op_imm(ooo: u16, imm: u16) -> [u16; 2] {
        [(ooo << 13) | 0x0800, imm]
    }
    /// `mpya` / `mpys` / `mld` (ooo = 100 1 011 -> 0x4b / 011 1 011 -> 0x3b
    /// / 101 1 011 -> 0x5b), with the modifier/index fields of the two MAC
    /// pointers (ri: bits 3-2 and 1-0; rj: bits 7-6 and 5-4) and the `b`
    /// bit 8 set.
    fn mac(ooo: u16, ri_m: u16, ri_pp: u16, rj_m: u16, rj_pp: u16) -> u16 {
        (ooo << 9) | 0x100 | (rj_m << 6) | (rj_pp << 4) | (ri_m << 2) | ri_pp
    }
    /// `ret` - `ld PC, STACK` (svpdoc 2.4.3's alias).
    fn ret() -> u16 {
        ld(6, 5)
    }
    /// `ld d, (a)`: `0100 1010 dddd 0000`.
    fn ld_from_a(dest: u16) -> u16 {
        0x4A00 | (dest << 4)
    }

    // ---- the move group (svpdoc 2.4.2) ---------------------------------

    /// `ld -, -` is the nop: it advances the program and changes nothing.
    #[test]
    fn ld_minus_minus_is_the_nop() {
        let mem = &mut TestMem::rom_words(&[0x0000, ld(1, 2)]);
        let (core, used) = run_fresh(mem, 2);
        assert_eq!(used, 2, "one cycle a word");
        assert_eq!(core.pc, 0x402);
        assert_eq!(core.x, 0, "the nop did not move Y into X");
    }

    /// `ldi` moves its immediate, `ld X, Y` moves a register, and reading
    /// `-` answers 0xFFFF (svpdoc 2.1.1: "Constant register with all bits
    /// set").
    #[test]
    fn ld_moves_registers_and_minus_reads_all_ones() {
        let mem = &mut TestMem::rom_of(&[&ldi(2, 0xBEEF), &[ld(1, 2), ld(1, 0)]]);
        let mut core = fresh(mem);
        run(&mut core, mem, 2);
        assert_eq!(core.y, 0xBEEF, "ldi Y");
        assert_eq!(core.x, 0xBEEF, "ld X, Y");
        run(&mut core, mem, 1);
        assert_eq!(core.x, 0xFFFF, "ld X, - reads the constant");
    }

    /// `ld A, imm` writes only the accumulator's upper word (svpdoc 2.4.2:
    /// "If destination is A, and source is 16bit, only upper word is
    /// transfered").
    #[test]
    fn ld_a_takes_the_upper_word_only() {
        let mem = &mut TestMem::rom_of(&[&ldi(3, 0xBEEF), &ldi(3, 0x1234)]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.a, 0x1234_0000, "both ldis landed in the upper word");
    }

    /// AL is the accumulator's low word (svpdoc 2.2.8): `ld AL, imm` then
    /// `ld X, AL` round-trips it.
    #[test]
    fn al_is_the_accumulator_low_word() {
        let mem = &mut TestMem::rom_of(&[&ldi(15, 0x1234), &[ld(1, 15)]]);
        let (core, _) = run_fresh(mem, 3);
        assert_eq!(core.a & 0xFFFF, 0x1234, "ld AL, imm");
        assert_eq!(core.x, 0x1234, "ld X, AL reads the low word");
    }

    /// The first indirection `(r0)` reads and writes RAM0, and the `+!`
    /// modifier post-increments the pointer (svpdoc 2.3).
    #[test]
    fn ld_through_ram_pointers_moves_and_modifies_the_pointer() {
        let mem = &mut TestMem::rom_of(&[
            &[ldi_ri(0, 0, 0)],       // ldi r0, 0
            &ldi_p1(0, 0, 0, 0x1234), // ldi (r0), 0x1234
            &[ld_p1(1, 0, 0, 0)],     // ld X, (r0)
            &[ld_p1(2, 0, 1, 0)],     // ld Y, (r0+!)
        ]);
        let (core, _) = run_fresh(mem, 5);
        assert_eq!(core.ram[0], 0x1234, "ldi (r0), imm wrote RAM0[0]");
        assert_eq!(core.x, 0x1234, "ld X, (r0) read it back");
        assert_eq!(core.y, 0x1234, "the +! read saw the same word");
        assert_eq!(core.ptr[0], 1, "the +! read post-incremented r0");
    }

    /// `+` post-increments under the RPL modulo when ST bits 0-2 say so
    /// (svpdoc 2.1.5: RPL 4 = modulo by 16) and runs plain when they are
    /// clear; `-` post-decrements.
    #[test]
    fn the_plus_and_minus_modifiers_move_under_rpl() {
        // ldi r1, 15; ld X, (r1+): j=0 m=3 pp=1.
        let program = [ldi_ri(0, 1, 15), ld_p1(1, 0, 3, 1), ld_p1(1, 0, 3, 1)];
        let mem = &mut TestMem::rom_words(&program);
        let core = run_with_st(mem, 4, 3);
        assert_eq!(core.ptr[1], 1, "r1 wrapped 15 -> 0 -> 1 modulo 16");

        let mem = &mut TestMem::rom_words(&program);
        let core = run_with_st(mem, 0, 3);
        assert_eq!(core.ptr[1], 17, "without RPL the pointer runs plain");

        // ldi r1, 3; ld Y, (r1-).
        let mem = &mut TestMem::rom_words(&[ldi_ri(0, 1, 3), ld_p1(2, 0, 2, 1)]);
        let core = run_with_st(mem, 0, 2);
        assert_eq!(core.ptr[1], 2, "the - modifier post-decremented");
    }

    /// The fixed forms: register 3 with modifier `nn` answers RAM0[nn]
    /// directly (svpdoc 2.3: "|00: RAMx[0], |01: RAMx[1] ...").
    #[test]
    fn register_3_is_the_fixed_ram_form() {
        let mem = &mut TestMem::rom_of(&[
            &[ld_p1_store(0, 1, 2, 3), 0xCAFE], // ld (r3|10), X... s is X - use ldi path
        ]);
        let _ = mem;
        // ld (r3|10), imm is not a form; write through ld (r3|10), s with X:
        let mem = &mut TestMem::rom_of(&[
            &ldi(1, 0xCAFE),
            &[ld_p1_store(0, 1, 2, 3)], // ld (r3|10), X: writes RAM0[2]
            &[ld_p1(1, 0, 2, 3)],       // ld X, (r3|10)
        ]);
        let (core, _) = run_fresh(mem, 3);
        assert_eq!(core.ram[2], 0xCAFE, "ld (r3|10), X wrote RAM0[2]");
        assert_eq!(core.x, 0xCAFE, "ld X, (r3|10) read it back");
    }

    /// `ld d, ((ri))`: program memory at the RAM cell, and the cell itself
    /// post-increments (svpdoc 2.3: "((rX)) also performs post-increment").
    #[test]
    fn double_indirection_reads_program_memory_and_increments_the_cell() {
        let mut mem = TestMem::rom_of(&[
            &ldi_p1(0, 0, 0, 0x402), // ldi (r0), 0x402
            &[0x600D],               // word 0x402: what ((r0)) fetches
            &[ld_p2(1, 0, 0, 0)],    // ld X, ((r0))
        ]);
        let (core, _) = run_fresh(&mut mem, 4);
        assert_eq!(core.x, 0x600D, "((r0)) fetched program word 0x402");
        assert_eq!(core.ram[0], 0x403, "the cell post-incremented");
    }

    /// `ld adr, a` stores the accumulator's upper word into the 9-bit RAM
    /// address, whose bit 8 picks the bank (svpdoc 2.4.2's `0000 111j`).
    #[test]
    fn ld_adr_a_stores_the_upper_word_across_both_banks() {
        let mem = &mut TestMem::rom_of(&[&ldi(3, 0xBEEF), &[ld_adr_a(0, 0x10), ld_adr_a(1, 0x10)]]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.ram[0x10], 0xBEEF, "RAM0[0x10]");
        assert_eq!(core.ram[0x100 + 0x10], 0xBEEF, "RAM1[0x10], j = bit 8");
    }

    /// `ldi ri, simm` stores the signed byte; `ld d, ri` reads the pointer
    /// zero-extended (svpdoc 2.4.2's `0001 1jpp iiiiiiii`).
    #[test]
    fn pointer_register_moves_and_immediates() {
        let mem = &mut TestMem::rom_words(&[ldi_ri(1, 1, 0xFE), ld_ri(1, 1, 1)]);
        let (core, _) = run_fresh(mem, 2);
        assert_eq!(core.ptr[5], 0xFE, "ldi r5, -2 stored the byte 0xFE");
        assert_eq!(core.x, 0x00FE, "ld X, r5 reads the pointer zero-extended");
    }

    /// `ld A, P` takes the whole 32-bit product; `ld X, P` its upper word
    /// (svpdoc 2.4.2: "whole 32bit value" vs the 16-bit rule).
    #[test]
    fn ld_from_p_moves_32_bits_into_a_and_the_upper_word_elsewhere() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(1, 0xFFFF), // X <- -1
            &ldi(2, 3),      // Y <- 3
            &[ld(3, 7), ld(1, 7)],
        ]);
        let (core, _) = run_fresh(mem, 6);
        assert_eq!(core.p, 0xFFFF_FFFA, "P = -1 * 3 * 2 = -6");
        assert_eq!(core.a, 0xFFFF_FFFA, "ld A, P moved the whole product");
        assert_eq!(core.x, 0xFFFF, "ld X, P reads the upper word");
    }

    /// `ld d, (a)`: program memory at the accumulator's upper word
    /// (svpdoc 2.4.2's `0100 1010 dddd 0000`).
    #[test]
    fn ld_d_a_reads_program_memory_at_the_accumulator_upper_word() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(3, 0x0404), // A upper = 0x404
            &[0x600D],       // word 0x402: filler
            &[ld_from_a(1)], // ld X, (a) at word 0x403
            &[0x7777],       // word 0x404: the answer
        ]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.x, 0x7777, "ld X, (a) fetched program word 0x404");
    }

    /// `ld ST, X` / `ld X, ST` round-trip the status register.
    #[test]
    fn st_round_trips_through_moves() {
        let mem = &mut TestMem::rom_of(&[&ldi(1, 0x1234), &[ld(4, 1), ld(1, 4)]]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.st, 0x1234);
    }

    // ---- the ALU group (svpdoc 2.4.1) ----------------------------------

    /// `sub A, imm` shifts the immediate into the upper word; N and Z come
    /// off the full 32-bit accumulator; `cmp` does not store.
    #[test]
    fn sub_and_cmp_with_immediate_and_the_flags() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(3, 0x0002),    // A <- 2
            &op_imm(1, 0x0001), // sub A, 1 => 0x0001_0000
            &op_imm(3, 0x0002), // cmp A, 2 => negative, not stored
            &op_imm(3, 0x0001), // cmp A, 1 => equal, Z
        ]);
        let (core, _) = run_fresh(mem, 7);
        assert_eq!(core.a, 0x0001_0000, "sub stored; cmp did not");
        assert_eq!(core.st & FLAG_N, 0, "0x0001_0000 is positive");
        assert_eq!(core.st & FLAG_Z, FLAG_Z, "the last cmp was equal");
    }

    /// `andi A, 0` clears all 32 bits of A (svpdoc 2.4.1: "Operations are
    /// performed on full A") and sets Z.
    #[test]
    fn andi_a_0_clears_all_32_bits() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(3, 0xBEEF),
            &op_imm(1, 0x0001), // sub A, 1 => 0xBEEE_0000, N set
            &op_imm(5, 0x0000), // and A, 0 => 0, Z set
        ]);
        let (core, _) = run_fresh(mem, 6);
        assert_eq!(core.a, 0);
        assert_eq!(core.st & FLAG_Z, FLAG_Z);
    }

    /// The 32-bit sources (the references' OP_CHECK32): `add A, P`
    /// accumulates the whole product and `sub A, A` gives zero.
    #[test]
    fn alu_with_a_and_p_take_32_bit_sources() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(1, 3),      // X <- 3
            &ldi(2, 4),      // Y <- 4
            &[ld(3, 7)],     // ld A, P => 24
            &ldi(2, 2),      // Y <- 2 => next P = 12
            &[op_reg(4, 7)], // add A, P => 24 + 12 = 36
            &[op_reg(1, 3)], // sub A, A => 0, Z set
        ]);
        let (core, _) = run_fresh(mem, 11);
        assert_eq!(core.a, 0);
        assert_eq!(core.st & FLAG_Z, FLAG_Z);
    }

    // ---- mod cond, op ---------------------------------------------------

    /// `mod cond, op`: shift, negate and absolute under cond 0, flags from
    /// the result (svpdoc 2.4.1's modifier table).
    #[test]
    fn mod_shifts_negates_and_absolutes() {
        let mem = &mut TestMem::rom_of(&[&ldi(3, 4), &[mod_op(0, 0, 3)]]);
        let core = run_with_st(mem, 0, 2);
        assert_eq!(core.a, 0x0008_0000, "shl doubled the accumulator");

        let mem = &mut TestMem::rom_of(&[&ldi(3, 8), &[mod_op(0, 0, 6)]]);
        let core = run_with_st(mem, 0, 2);
        assert_eq!(core.a, 0xFFF8_0000, "neg of 8");
        assert_eq!(core.st & FLAG_N, FLAG_N);

        let mem = &mut TestMem::rom_of(&[&ldi(3, 8), &[mod_op(0, 0, 6), mod_op(0, 0, 7)]]);
        let core = run_with_st(mem, 0, 3);
        assert_eq!(core.a, 0x0008_0000, "abs of -8");

        let mem = &mut TestMem::rom_of(&[&ldi(3, 8), &[mod_op(0, 0, 6), mod_op(0, 0, 2)]]);
        let core = run_with_st(mem, 0, 4);
        assert_eq!(core.a, 0xFFFC_0000, "arithmetic shr of -8");
    }

    /// A false condition skips the `mod`: Z clear and f=1 does nothing.
    #[test]
    fn a_false_condition_skips_the_mod() {
        let mem = &mut TestMem::rom_of(&[&ldi(3, 4), &[mod_op(1, 5, 3)]]);
        let core = run_with_st(mem, 0, 2);
        assert_eq!(core.a, 0x0004_0000, "Z clear, f=1: not taken");
    }

    // ---- program control (svpdoc 2.4.3) ---------------------------------

    /// `bra cond, addr` under cond 0 always branches and costs two cycles;
    /// the not-taken form skips the extension word in one.
    #[test]
    fn bra_taken_and_not_taken() {
        let mem = &mut TestMem::rom_of(&[&bra(0, 0, 0x404), &[0x600D, 0x600D, ld(1, 1)]]);
        let (core, used) = run_fresh(mem, 2);
        assert_eq!(core.pc, 0x404);
        assert_eq!(used, 2, "the taken bra costs two cycles");

        let mem = &mut TestMem::rom_of(&[
            &bra(1, 5, 0x404), // bra Z==1 with Z clear: not taken
            &[ld(1, 1)],
        ]);
        let (core, used) = run_fresh(mem, 2);
        assert_eq!(core.pc, 0x403, "the bra and its skipped extension word");
        assert_eq!(used, 2, "two words fetched, two cycles");
    }

    /// The Z condition branches by its sense bit `f`: `cmp` equal sets Z,
    /// then Z==1 takes and Z==0 does not.
    #[test]
    fn the_z_condition_branches_by_its_sense_bit() {
        let mem = &mut TestMem::rom_of(&[
            &op_imm(3, 0x0000), // cmp A, 0 (A comes up 0) => Z set
            &bra(1, 5, 0x404),  // bra Z==1
            &[0x600D, 0x600D, ld(1, 1)],
        ]);
        let (core, _) = run_fresh(mem, 3);
        assert_eq!(core.pc, 0x404, "Z set, f=1: taken");

        let mem = &mut TestMem::rom_of(&[
            &op_imm(3, 0x0000),
            &bra(0, 5, 0x404), // bra Z==0
            &[ld(1, 1), 0x600D, 0x600D],
        ]);
        let (core, _) = run_fresh(mem, 5);
        assert_eq!(core.pc, 0x407, "Z set, f=0: fell through and ran on");
    }

    /// `call cond, addr` pushes the address after its extension word and
    /// jumps; `ret` (`ld PC, STACK`) pops it back.
    #[test]
    fn call_and_ret_round_trip_through_the_hardware_stack() {
        let mem = &mut TestMem::rom_of(&[
            &call(0, 0, 0x406),
            &[ld(1, 1)], // return lands here (0x402)
            &[0x600D, 0x600D, 0x600D],
            &[ld(2, 1)], // the subroutine (0x406)
            &[ret()],
        ]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.pc, 0x402, "ret returned past the call");
        assert_eq!(core.sp, 0, "the stack is back where it started");
        assert_eq!(core.y, 0, "the subroutine ran");
    }

    // ---- the MAC group (svpdoc 2.4.4) -----------------------------------

    /// `mld (rj), (ri)` clears the accumulator, loads X from RAM0 and Y from
    /// RAM1 through the two modifier fields, and P becomes X*Y*2 on
    /// sign-extended inputs (svpdoc 2.1.8).
    #[test]
    fn mld_loads_xy_and_p_is_sign_extended_x_times_y_times_two() {
        let mem = &mut TestMem::rom_of(&[
            &ldi_p1(0, 0, 0, 0x0003), // ldi (r0), 3
            &ldi_p1(1, 0, 0, 0xFFFE), // ldi (r4), -2
            &[mac(0x5B, 0, 0, 0, 0)], // mld (r4), (r0)
        ]);
        let (core, _) = run_fresh(mem, 4);
        assert_eq!(core.a, 0, "mld cleared the accumulator");
        assert_eq!(core.x, 3, "X from RAM0[(r0)]");
        assert_eq!(core.y, 0xFFFE, "Y from RAM1[(r4)]");
        // P updates lazily: reading it now answers the product.
        let mut core = core;
        core.update_p();
        assert_eq!(core.p, 0xFFFF_FFF4, "P = 3 * -2 * 2 = -12");
    }

    /// `mpya` adds the standing product before loading the next X/Y; `mpys`
    /// subtracts it.
    #[test]
    fn mpya_accumulates_and_mpys_subtracts_the_standing_product() {
        let mem = &mut TestMem::rom_of(&[
            &[ldi_ri(0, 0, 0)],       // ldi r0, 0
            &[ldi_ri(0, 1, 1)],       // ldi r1, 1
            &[ldi_ri(1, 0, 0)],       // ldi r4, 0
            &[ldi_ri(1, 1, 1)],       // ldi r5, 1
            &ldi_p1(0, 0, 0, 0x0002), // ldi (r0), 2   => RAM0[0]
            &ldi_p1(0, 0, 1, 0x0005), // ldi (r1), 5   => RAM0[1]
            &ldi_p1(1, 0, 0, 0x0003), // ldi (r4), 3   => RAM1[0]
            &ldi_p1(1, 0, 1, 0x0007), // ldi (r5), 7   => RAM1[1]
            &[mac(0x5B, 1, 0, 1, 0)], // mld (r0+), (r4+): X = 2, Y = 3; pointers -> 1
            &[mac(0x4B, 1, 0, 1, 0)], // mpya: A += 12; X = RAM0[1] = 5, Y = RAM1[1] = 7
        ]);
        let (core, _) = run_fresh(mem, 10);
        assert_eq!(core.a, 12, "mpya accumulated the 2*3*2 product");
        assert_eq!(core.x, 5, "the next X");
        assert_eq!(core.y, 7, "the next Y");

        let mem = &mut TestMem::rom_of(&[
            &[
                ldi_ri(0, 0, 0),
                ldi_ri(0, 1, 1),
                ldi_ri(1, 0, 0),
                ldi_ri(1, 1, 1),
            ],
            &ldi_p1(0, 0, 0, 0x0002),
            &ldi_p1(0, 0, 1, 0x0005),
            &ldi_p1(1, 0, 0, 0x0003),
            &ldi_p1(1, 0, 1, 0x0007),
            &[mac(0x5B, 1, 0, 1, 0)], // mld (r0+), (r4+)
            &[mac(0x1B, 1, 0, 1, 0)], // mpys: A -= 12; X = 5, Y = 7
        ]);
        let (core, _) = run_fresh(mem, 13);
        assert_eq!(core.a, 12u32.wrapping_neg(), "mpys subtracted the product");
    }

    // ---- PMC and the external window (svpdoc 2.2.7) ----------------------

    /// The two-write PMC protocol arms the next blind access: address word,
    /// mode word, then `ld -, PM4` reads the programmed DRAM word.
    #[test]
    fn pmc_two_writes_then_a_blind_read_walks_dram() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x0005),        // PMC <- 5 (address word)
            &ldi(14, 0x0018),        // PMC <- 0x0018 (DRAM mode, nnn=0)
            &[ld(0, 12)],            // ld -, PM4: the blind access programs it
            &[ld(1, 12), ld(2, 12)], // real reads walk the window
        ]);
        mem.dram[5] = 0x1234;
        let (core, _) = run_fresh(mem, 8);
        assert_eq!(core.x, 0x1234, "the programmed window walked the DRAM");
        assert_eq!(core.y, 0x1234, "nnn=0: no increment, the same word");
    }

    /// The increment table (svpdoc 2.2.7: nnn 1-7 -> 1, 2, 4, 8, 16, 32,
    /// 128 words) and the `d` bit negating it.
    #[test]
    fn the_mode_increment_walks_dram() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x0010),                   // address: word 0x10
            &ldi(14, 0x9818),                   // mode: d + nnn=3 => -4 words
            &[ld(0, 12)],                       // ld -, PM4: program
            &[ld(1, 12), ld(2, 12), ld(3, 12)], // three real reads
        ]);
        mem.dram[0x10] = 0x1010;
        mem.dram[0xC] = 0x0C0C;
        mem.dram[8] = 0x0808;
        let (core, _) = run_fresh(mem, 7);
        assert_eq!(core.x, 0x1010, "first read at word 0x10");
        assert_eq!(core.y, 0x0C0C, "second at 0xC (decrement 4)");
        assert_eq!((core.a >> 16) as u16, 0x0808, "third at 8 (ld A, PM4)");
    }

    /// The overwrite (`v`) bit writes only the non-zero nibbles of the word
    /// (svpdoc 2.2.7: "only writes those half-bytes, which are not zero").
    #[test]
    fn the_overwrite_bit_writes_only_non_zero_nibbles() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x0020), // PMC address: word 0x20
            &ldi(14, 0x0418), // PMC mode: v + DRAM
            &[ld(12, 0)],     // ld PM4, -: the blind write programs it
            &ldi(1, 0x0F00),
            &[ld(12, 1)], // ld PM4, X: the real write
        ]);
        mem.dram[0x20] = 0x00FF;
        let (_, _) = run_fresh(mem, 6);
        assert_eq!(
            mem.dram[0x20], 0x0FFF,
            "only non-zero nibbles landed: 0x00FF kept its low byte"
        );
    }

    /// The IRAM window (mode 0x001C, word address 0x1C8000+) writes the
    /// DSP's own program RAM, which then executes (svpdoc section 3).
    #[test]
    fn the_iram_window_writes_program_ram() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x8000), // address: word 0x8000
            &ldi(14, 0x041C), // mode: IRAM
            &[ld(12, 0)],     // ld PM4, -: program the write window
            &ldi(1, 0x0000),  // X <- 0 (the nop)
            &[ld(12, 1)],     // ld PM4, X => IRAM[0] = 0
        ]);
        let (mut core, _) = run_fresh(mem, 6);
        assert_eq!(mem.iram[0], 0x0000, "the SSP wrote its IRAM");
        core.pc = 0;
        core.run(mem, 1);
        assert_eq!(core.pc, 1, "the IRAM nop executed");
    }

    /// The ROM read window (mode 0x0800, increment 1 - "verified to be
    /// correct" in the reference) walks the game image.
    #[test]
    fn the_rom_read_window_walks_the_image() {
        let pieces: [&[u16]; 3] = [
            &ldi(14, 0x0100),
            &ldi(14, 0x0800),
            &[ld(0, 12), ld(1, 12), ld(2, 12)],
        ];
        let program: Vec<u16> = pieces.concat();
        let mut mem = TestMem::rom_words(&program);
        // The window reads ROM words 0x100 and 0x101 of the image; the
        // program itself sits at words 0x0-0x6.
        mem.rom = vec![0x1111; 0x102];
        mem.rom[0..program.len()].copy_from_slice(&program);
        mem.rom[0x100] = 0xAAAA;
        mem.rom[0x101] = 0xBBBB;
        let (core, _) = run_fresh(&mut mem, 7);
        assert_eq!(core.x, 0xAAAA, "first ROM read at word 0x100");
        assert_eq!(core.y, 0xBBBB, "increment 1 walked to 0x101");
    }

    /// The cell-increment special mode (`s`, mode 0x4018): +1 word on an
    /// even address, +31 on an odd one (GPGX's `(addr&1) ? 31 : 1`).
    #[test]
    fn the_special_increment_walks_the_cell_arrange() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x0000),
            &ldi(14, 0x4018),
            &[ld(12, 0)], // ld PM4, -: programs the WRITE window
            &ldi(1, 0x1111),
            &[ld(12, 1), ld(12, 1)], // two writes: word 0 (even +1), word 1 (odd +31)
        ]);
        let (core, _) = run_fresh(mem, 8);
        assert_eq!(
            core.pmac[1][4],
            (u32::from(0x4018u16) << 16) | 32,
            "word 0 -> 1 (even, +1) -> 32 (odd, +31)"
        );
        assert_eq!(mem.dram[0], 0x1111, "the first write landed");
        assert_eq!(mem.dram[1], 0x1111, "the second write landed");
    }

    /// The XST/PM0 handshake, SSP side: an SSP write of XST sets PM0 bit 0
    /// and an SSP read of PM0 clears bit 1 (svpdoc 2.2.1).
    #[test]
    fn the_xst_pm0_handshake_from_the_ssp_side() {
        let mem = &mut TestMem::rom_words(&[ld(11, 1), ld(2, 8)]);
        let (_, _) = run_fresh(mem, 2);
        assert_eq!(mem.pm0 & 1, 1, "the SSP's XST write set bit 0");
        assert_eq!(mem.pm0 & 2, 0, "bit 1 untouched by the write");

        let mem = &mut TestMem::rom_words(&[ld(2, 8)]);
        mem.pm0 = 0x0002;
        let (_, _) = run_fresh(mem, 1);
        assert_eq!(mem.pm0 & 2, 0, "the SSP's PM0 read cleared bit 1");
        assert_eq!(mem.pm0 & 1, 0, "bit 0 untouched by the read");
    }

    /// The ST5/ST6 gate (svpdoc 2.2.1-2.2.4's "If ST5 or ST6 is set" rule,
    /// 2.2.5's always-external PM4): PM4 works without the ST bits; PM0
    /// without them stays the raw latch the 68k reads.
    #[test]
    fn the_st_gate_and_the_always_external_pm4() {
        let mem = &mut TestMem::rom_of(&[
            &ldi(14, 0x0005),
            &ldi(14, 0x0018),
            &[ld(0, 12)], // ld -, PM4: program (external whatever ST says)
            &[ld(1, 12)],
        ]);
        mem.dram[5] = 0x7777;
        let (core, _) = run_fresh(mem, 6);
        assert_eq!(core.x, 0x7777, "PM4 is always the window");

        let mem = &mut TestMem::rom_of(&[&ldi(1, 0x0042), &[ld(8, 1)]]);
        let (_, _) = run_fresh(mem, 3);
        assert_eq!(mem.pm0, 0x0042, "PM0 with ST clear wrote the raw latch");
    }
}
