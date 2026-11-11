//! The two ports the 68k programs the VDP through.
//!
//! Data port writes ([`Vdp::write_data`]) reach VRAM, CRAM and VSRAM at the
//! address the address register points at; control port writes
//! ([`Vdp::write_control`]) either set a register or lay down one half of a
//! 32-bit command word, which is how the address and code registers that the
//! data port uses get set (genvdp.txt section 3 "VDP port map": the data port
//! is `$C00000`/`$C00002` and the control port `$C00004`/`$C00006`).
//!
//! The 68k-side window that calls these two functions is `genesis`'s job
//! (M3-09, #69); this file is the chip.
//!
//! Data port reads ([`Vdp::read_data`]) are the other half of the same pair of
//! ports: the code register says which memory to read and the last word written
//! to the port fills the bits that memory does not drive. Not here either: the
//! control port read of the status register, which is `status`, and the FIFO
//! with the read prefetch that real hardware runs behind the read side
//! (M4-21a, #99).

use crate::Vdp;

/// Bit 5 of the code register: the DMA trigger. genvdp.txt section 7 "VDP
/// ports": "Setting CD5 will trigger a DMA operation."
const CODE_DMA: u8 = 1 << 5;

/// Mask for the two code register bits the first half of a command word
/// carries, CD1-CD0 (genvdp.txt section 7).
const CODE_LOW: u8 = 0x03;
/// Mask for the four the second half carries, CD5-CD2.
const CODE_HIGH: u8 = 0x3C;

/// What a data port write does, decoded from the code register.
///
/// The genvdp.txt section 7 table is bits CD3-CD0:
///
/// | CD3-CD0 | access |
/// | ------- | ------ |
/// | `0000`  | VRAM read |
/// | `0001`  | VRAM write |
/// | `0011`  | CRAM write |
/// | `0100`  | VSRAM read |
/// | `0101`  | VSRAM write |
/// | `1000`  | CRAM read |
/// | `1100`  | 8-bit VRAM read, undocumented |
///
/// "You cannot write data after setting up a read operation, or read data
/// after setting up a write operation. The write or read is ignored."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataTarget {
    /// `0001`: write a VRAM word.
    Vram,
    /// `0011`: write a CRAM word.
    Cram,
    /// `0101`: write a VSRAM word.
    Vsram,
    /// A read code, or a value with no write at all: the access is ignored.
    Ignored,
}

/// The two halves of a control port command word, for the tests here and for
/// the tickets that follow them (M4-03 #81 onwards).
///
/// Encodings are the command word of genvdp.txt section 7: CD1-CD0 and A13-A0
/// are the first half, CD5-CD2 and A15-A14 the second.
#[cfg(test)]
pub(crate) mod testutil {
    /// Control port words that set up a VRAM write at `addr`.
    pub(crate) fn vram_write_cmd(addr: u16) -> (u16, u16) {
        (0x4000 | (addr & 0x3FFF), (addr >> 14) & 0x0003)
    }

    /// Control port words that set up a CRAM write at `addr`.
    pub(crate) fn cram_write_cmd(addr: u16) -> (u16, u16) {
        (0xC000 | (addr & 0x3FFF), (addr >> 14) & 0x0003)
    }

    /// Control port words that set up a VSRAM write at `addr`. The code is
    /// `0101`, so CD0 comes from the first half and CD2 from the second.
    pub(crate) fn vsram_write_cmd(addr: u16) -> (u16, u16) {
        (0x4000 | (addr & 0x3FFF), 0x0010 | ((addr >> 14) & 0x0003))
    }

    /// Control port words that set up a VRAM read at `addr`: code `0000`, so
    /// CD1-CD0 are zero in the first half and the second half is A15-A14 alone.
    pub(crate) fn vram_read_cmd(addr: u16) -> (u16, u16) {
        (addr & 0x3FFF, (addr >> 14) & 0x0003)
    }

    /// Control port words that set up a CRAM read at `addr`. The code is
    /// `1000`, which is CD3, and CD3 rides in bit 5 of the second word.
    pub(crate) fn cram_read_cmd(addr: u16) -> (u16, u16) {
        (addr & 0x3FFF, 0x0020 | ((addr >> 14) & 0x0003))
    }

    /// Control port words that set up a VSRAM read at `addr` (code `0100`, so
    /// CD2, which rides in bit 4 of the second word).
    pub(crate) fn vsram_read_cmd(addr: u16) -> (u16, u16) {
        (addr & 0x3FFF, 0x0010 | ((addr >> 14) & 0x0003))
    }

    /// Control port words that set up the undocumented 8-bit VRAM read, whose
    /// code `1100` is not in the genvdp.txt table at all.
    pub(crate) fn vram_byte_read_cmd(addr: u16) -> (u16, u16) {
        (addr & 0x3FFF, 0x0030 | ((addr >> 14) & 0x0003))
    }

    /// Control port words that set up a VRAM copy to `addr`: the code of a
    /// VRAM read with CD4 and CD5 set - "CD4 is only set for the the VRAM copy
    /// DMA mode" and CD5 is the DMA itself (genvdp.txt section 7 "Here is a
    /// table of code register settings", section 11 "VRAM copy").
    pub(crate) fn copy_write_cmd(addr: u16) -> (u16, u16) {
        (addr & 0x3FFF, 0x00C0 | ((addr >> 14) & 0x0003))
    }
}

/// Is `value` a VDP register write?
///
/// "Bits 15 and 14 must be set to 1 and 0 respectively, otherwise the write
/// will be treated as the first half of a command word. The state of bit 13
/// does not matter" (genvdp.txt section 7 "Programming VDP registers").
///
/// `pending` belongs in this decision rather than in the caller: while the
/// second half of a command word is expected the same top bits are that word's
/// CD1-CD0, so a value that looks exactly like a register write is not one
/// ("You cannot write to a VDP register if the pending flag is set to one").
const fn is_register_write(value: u16, pending: bool) -> bool {
    !pending && value & 0xE000 == 0x8000
}

/// Register number and data of a register write: `100r rrrr dddd dddd`
/// (genvdp.txt section 7).
const fn register_field(value: u16) -> (u8, u8) {
    (((value >> 8) & 0x1F) as u8, (value & 0xFF) as u8)
}

/// The code and address registers after the first half of a command word.
///
/// "It is perfectly valid to write the first half of the command word only. In
/// this case, _only_ A13-A00 and CD1-CD0 are updated to reflect the new
/// values, while the remaining address and code bits _retain_ their former
/// value" (genvdp.txt section 7). So this is where a command takes effect: a
/// game that sets up an address and then writes data without ever sending the
/// second half is using the address set here.
const fn first_word(code: u8, addr: u16, value: u16) -> (u8, u16) {
    let code = (code & !CODE_LOW) | ((value >> 14) & 0x03) as u8;
    let addr = (addr & 0xC000) | (value & 0x3FFF);
    (code, addr)
}

/// The code and address registers after the second half of a command word:
/// CD5-CD2 are its bits 7-4 and A15-A14 are its bits 1-0 (genvdp.txt section
/// 7).
const fn second_word(code: u8, addr: u16, value: u16) -> (u8, u16) {
    let code = (code & !CODE_HIGH) | ((value >> 2) & 0x3C) as u8;
    let addr = ((value & 0x0003) << 14) | (addr & 0x3FFF);
    (code, addr)
}

/// The code register entry a data port write works on.
///
/// Only CD3-CD0 matter: "CD4 is only set for the the VRAM copy DMA mode. For
/// data port accesses and 68k to VDP DMA, the state of CD4 is ignored"
/// (genvdp.txt section 7).
pub(crate) const fn data_target(code: u8) -> DataTarget {
    match code & 0x0F {
        0b0001 => DataTarget::Vram,
        0b0011 => DataTarget::Cram,
        0b0101 => DataTarget::Vsram,
        _ => DataTarget::Ignored,
    }
}

/// What a data port read does, decoded from the code register.
///
/// The read codes of the genvdp.txt section 7 table - `0000` VRAM, `0100`
/// VSRAM, `1000` CRAM - plus `1100`, the 8-bit VRAM read that table does not
/// list and that no document describes beyond the emulators that implement it
/// (Genesis Plus GX's `vdp_68k_data_r_m5` case `0x0C`).
///
/// CD4 is ignored exactly as it is for a write ("For data port accesses and
/// 68k to VDP DMA, the state of CD4 is ignored", genvdp.txt section 7).
/// Genesis Plus GX instead switches on `code & 0x1F`, so a read with CD4 set
/// falls into its default case and it comments that case "normally locks VDP,
/// hard reset required"; the doc says it should not, so this does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadTarget {
    /// `0000`: read a VRAM word.
    Vram,
    /// `0100`: read a VSRAM word.
    Vsram,
    /// `1000`: read a CRAM word.
    Cram,
    /// `1100`: read one byte of VRAM, undocumented.
    VramByte,
    /// A write code, or a code with no read at all: no memory is reached, see
    /// [`Vdp::read_data`].
    NoRead,
}

/// The code register entry a data port read works on.
///
/// Masked to CD3-CD0 for the reason [`data_target`] gives for masking it there.
const fn read_target(code: u8) -> ReadTarget {
    match code & 0x0F {
        0b0000 => ReadTarget::Vram,
        0b0100 => ReadTarget::Vsram,
        0b1000 => ReadTarget::Cram,
        0b1100 => ReadTarget::VramByte,
        _ => ReadTarget::NoRead,
    }
}

/// The address a VRAM word write goes to and the two bytes it writes there.
///
/// The address is forced to even and "when writing 16-bit data to VRAM, having
/// address bit 0 set will swap the upper and lower bytes of the data written"
/// (genvdp.txt section 8 "VRAM").
///
/// Without that swap the high byte of the word goes to the even byte address,
/// which is what makes the sprite attribute table of genvdp.txt section 15
/// (`Index + 2: ----hhvv`, `Index + 3: -lllllll`) come out of a single
/// `move.w`.
const fn vram_word_bytes(addr: u16, value: u16) -> (u16, [u8; 2]) {
    let high = (value >> 8) as u8;
    let low = value as u8;
    let bytes = if addr & 1 == 0 {
        [high, low]
    } else {
        [low, high]
    };
    (addr & !1, bytes)
}

/// Index of the CRAM word an access at `addr` names.
///
/// "When accessing CRAM, only address bits 6 through 1 are valid. The
/// high-order address bits are ignored. Since CRAM is word-wide, address bit
/// zero has no effect" (genvdp.txt section 9 "CRAM").
const fn cram_word_index(addr: u16) -> usize {
    ((addr >> 1) & 0x3F) as usize
}

/// Index of the VSRAM word an access at `addr` names, or `None` when there is
/// no word there.
///
/// Addressed like CRAM, and "even though there are 40 words of VSRAM, the
/// address register will wrap when it passes 7Fh. Writes to the addresses
/// beyond 50h are ignored" (genvdp.txt section 10 "VSRAM"): the 40 words live
/// at `$00` to `$4E`, so index 40 and above is nowhere.
const fn vsram_word_index(addr: u16) -> Option<usize> {
    let word = ((addr >> 1) & 0x3F) as usize;
    if word < crate::VSRAM_WORDS {
        Some(word)
    } else {
        None
    }
}

/// The address register after one data port access.
///
/// genvdp.txt section 17 "$0F": "Bits 7-0 specify the value to be added to the
/// VDP's address register after every read or write to the data port. A
/// setting of zero means the address register is not incremented."
///
/// So the step is unsigned, all eight bits of it, and the address register is
/// 16 bits wide and wraps: "the address register wraps past address FFFFh"
/// (genvdp.txt section 8). Nothing in genvdp.txt or in the plutiedev register
/// reference ("INC7-0 : autoincrement amount (in bytes)") describes a sign, and
/// neither does Genesis Plus GX (`addr += reg[15]`, `addr` being a `uint16`)
/// or BlastEm (`address += regs[REG_AUTOINC]`, `regs` being `uint8_t`):
/// `0xFF` is +255 and not -1.
///
/// The full address register is kept here, unmasked, for the memory the code
/// register names. [`Vdp::read_data`] masks the address it uses per memory (VRAM
/// whole 64 KiB, CRAM and VSRAM bits 6-1); reads increment with this same
/// function, because the access happened either way.
pub(crate) const fn increment(addr: u16, step: u8) -> u16 {
    addr.wrapping_add(step as u16)
}

/// Does a command word with this code register ask for a DMA, and is DMA
/// allowed to run it?
///
/// "Setting CD5 will trigger a DMA operation" (genvdp.txt section 7), and the
/// trigger only counts while R1 M1 is set, which is what makes M1 "1 to allow
/// DMA operations" (plutiedev "$81xx"; genvdp.txt section 11 "DMA"). The M1
/// bit itself is [`Vdp::dma_enabled`].
const fn dma_requested_by(code: u8, dma_enabled: bool) -> bool {
    code & CODE_DMA != 0 && dma_enabled
}

/// The 11 bits of a VSRAM word (genvdp.txt section 10 gives the 10-bit scroll
/// value, and interlace mode 2 uses an 11th; see the note on `Vdp::vsram`).
const VSRAM_WORD_BITS: u16 = 0x07FF;

/// The colour bits of a CRAM word as the 68k writes them: `----bbb0 ggg0 rrr0`
/// (genvdp.txt section 9 "CRAM").
const CRAM_COLOR_BITS: u16 = 0x0EEE;
/// The four bits a CRAM word has no colour in, and bit 0: a CRAM read answers
/// those from the port (`----bbb0 ggg0 rrr0` again, and Genesis Plus GX's
/// `fifo[fifo_idx] & ~0xEEE`).
const CRAM_UNUSED_BITS: u16 = !CRAM_COLOR_BITS;
/// The top five bits of a VSRAM word, which hold no scroll value and come back
/// from the port on a read.
const VSRAM_UNUSED_BITS: u16 = !VSRAM_WORD_BITS;

/// Index of the VSRAM word a read at `addr` names.
///
/// The addresses are the write's - bits 6 through 1 - but there is no "ignore
/// it" for a read. Genesis Plus GX wraps an index past the 40th word back to
/// address 0 ("Wrap to address 0 (TODO: check if still true with Genesis 3
/// model)", `vdp_68k_data_r_m5` case `0x04`), which is what this does; genvdp.txt
/// section 10 only covers writes ("Writes to the addresses beyond 50h are
/// ignored").
const fn vsram_read_index(addr: u16) -> usize {
    let word = ((addr >> 1) & 0x3F) as usize;
    if word < crate::VSRAM_WORDS { word } else { 0 }
}

/// The CRAM word a read answers: the colour from CRAM, the rest from the port.
///
/// "Unused bits are set using data from next available FIFO entry" (Genesis Plus
/// GX's `vdp_68k_data_r_m5` case `0x08`), where the FIFO entry is what
/// `Vdp::fifo_last` stands in for until M4-21a (#99).
const fn cram_read_word(stored: u16, port: u16) -> u16 {
    (stored & CRAM_COLOR_BITS) | (port & CRAM_UNUSED_BITS)
}

/// The VSRAM word a read answers: the 11 bits of scroll value from VSRAM, the
/// top five from the port (see [`cram_read_word`] for where that comes from).
const fn vsram_read_word(stored: u16, port: u16) -> u16 {
    (stored & VSRAM_WORD_BITS) | (port & VSRAM_UNUSED_BITS)
}

/// The word an 8-bit VRAM read answers: the one byte it read (zero-extended) as
/// the low byte, the port's own value on top of it (`data |= fifo[fifo_idx] &
/// ~0xFF`, Genesis Plus GX's case `0x0C`).
const fn vram_byte_read_word(byte: u16, port: u16) -> u16 {
    (byte & 0x00FF) | (port & 0xFF00)
}

impl Vdp {
    /// Write the control port (`$C00004`, and its mirror at `$C00006`).
    ///
    /// Either a register write, or one half of a command word that sets up the
    /// address and code registers the data port uses. An 8-bit write to the
    /// port is seen by the VDP as a 16-bit word with the byte duplicated
    /// (genvdp.txt section 7 "8-bit port access"); that duplication is the
    /// 68k-side glue's job, which is #69.
    pub fn write_control(&mut self, value: u16) {
        if self.pending {
            // The second half of a command word, whatever its top bits look
            // like (genvdp.txt section 7: "You cannot write to a VDP register
            // if the pending flag is set to one, since the VDP is expecting
            // the 2nd half of a command word").
            self.pending = false;
            let (code, addr) = second_word(self.code, self.addr, value);
            self.code = code;
            self.addr = addr;
            if dma_requested_by(code, self.dma_enabled()) {
                self.dma_requested = true;
            }
        } else if is_register_write(value, self.pending) {
            let (reg, data) = register_field(value);
            self.write_register(reg, data);
            // genvdp.txt section 7: "Writing to a VDP register will clear the
            // code register. Games that rely on this are Golden Axe II (will
            // display missing SEGA logo) and Sonic 3D." Genesis Plus GX
            // agrees in effect: it leaves the code register with CD1-CD0 = 10,
            // which is not a write code either.
            self.code = 0;
        } else {
            let (code, addr) = first_word(self.code, self.addr, value);
            self.code = code;
            self.addr = addr;
            self.pending = true;
        }
    }

    /// Write the data port (`$C00000`, and its mirror at `$C00002`).
    ///
    /// What happens is decided by the code register, which of the three
    /// memories is being written and how the word is masked; the address
    /// register then moves on by R15 whatever happened, because the access
    /// itself did happen (genvdp.txt section 17 "$0F": the increment follows
    /// "every read or write to the data port").
    pub fn write_data(&mut self, value: u16) {
        // Whatever the code register makes of the access, it happened: the
        // write-pending flag "is cleared when the data port is written to or
        // read from" (genvdp.txt section 7), and the port now carries this word
        // for the read side of the chip ([`Vdp::read_data`]).
        self.pending = false;
        self.fifo_last = value;
        self.write_target_word(value);

        // The data port write is also what starts a DMA fill that a command
        // word has armed: "Any write to the data port will then start a VRAM
        // fill" (genvdp.txt section 11 "VRAM fill"). `dma::Vdp::run_fill` is
        // where that lives, and it does nothing at all whenever no fill is
        // waiting - which is every other write to the port, the words a 68k to
        // VDP transfer writes included.
        self.run_fill(value);
    }

    /// The word goes to the memory the code register names, and the address
    /// register moves on by R15.
    ///
    /// Split out of [`Vdp::write_data`] because a DMA fill to CRAM or VSRAM
    /// repeats this whole-word write on every step of its own (`dma`, #86).
    pub(crate) fn write_target_word(&mut self, value: u16) {
        match data_target(self.code) {
            DataTarget::Vram => {
                let (even, bytes) = vram_word_bytes(self.addr, value);
                let base = usize::from(even);
                self.vram[base] = bytes[0];
                // `even` is `addr & !1`, so at worst it is 0xFFFE and the pair
                // stays inside VRAM rather than wrapping.
                self.vram[base + 1] = bytes[1];
            }
            DataTarget::Cram => {
                self.cram[cram_word_index(self.addr)] = value & CRAM_COLOR_BITS;
            }
            DataTarget::Vsram => {
                if let Some(word) = vsram_word_index(self.addr) {
                    self.vsram[word] = value & VSRAM_WORD_BITS;
                }
            }
            DataTarget::Ignored => {}
        }

        self.addr = increment(self.addr, self.auto_increment());
    }

    /// Read the data port (`$C00000`, and its mirror at `$C00002`).
    ///
    /// The code register picks the memory, and each memory drives only part of
    /// the 16-bit bus: a CRAM word has no colour in five of its bits, a VSRAM
    /// word has no scroll value in its top five, and the undocumented 8-bit VRAM
    /// read moves one byte. The rest of the word comes back from
    /// [`Vdp::fifo_last`], the last word written to the port, which is where
    /// real hardware uses the FIFO it keeps between the port and the memories
    /// (Nemesis's FIFO research on SpritesMind; Genesis Plus GX's
    /// `vdp_68k_data_r_m5` says so for each of the four read codes).
    ///
    /// Reading with a write code reaches no memory at all and returns
    /// [`Vdp::fifo_last`] whole. "You cannot write data after setting up a read
    /// operation, or read data after setting up a write operation. The write or
    /// read is ignored" (genvdp.txt section 7) says the read does nothing, but
    /// not what the port answers; Genesis Plus GX leaves its read switch with
    /// `data = 0`, which would be all-zero. This returns the last word the bus
    /// was driven with instead, the same value a read of a partial word falls
    /// back on. TODO(M4-21a #99): the FIFO decides this properly.
    ///
    /// Like a write, the access happened whatever the code made of it, so the
    /// address register moves on by R15 and `pending` clears.
    ///
    /// TODO(M4-21a #99): real hardware prefetches the word when the read command
    /// is set up, which is what makes reading the port right after a command
    /// word return the old word and the VDP lock up if the command was a write.
    /// There is no prefetch here, so no delay and no lock.
    pub fn read_data(&mut self) -> u16 {
        self.pending = false;

        let value = match read_target(self.code) {
            ReadTarget::Vram => {
                // Bit 0 of the address does not swap anything on a read: the
                // word is the two bytes of the even address, high byte first
                // (Genesis Plus GX reads `vram[addr & 0xFFFE]`).
                let base = usize::from(self.addr & !1);
                u16::from_be_bytes([self.vram[base], self.vram[base + 1]])
            }
            ReadTarget::Cram => {
                cram_read_word(self.cram[cram_word_index(self.addr)], self.fifo_last)
            }
            ReadTarget::Vsram => {
                vsram_read_word(self.vsram[vsram_read_index(self.addr)], self.fifo_last)
            }
            // "Read one byte from VRAM adjacent address" (Genesis Plus GX's
            // `vdp_68k_data_r_m5` case `0x0C`): the byte on the other side of
            // the even/odd pair the address sits in.
            ReadTarget::VramByte => vram_byte_read_word(
                u16::from(self.vram[usize::from(self.addr ^ 1)]),
                self.fifo_last,
            ),
            ReadTarget::NoRead => self.fifo_last,
        };

        self.addr = increment(self.addr, self.auto_increment());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{
        cram_read_cmd, cram_write_cmd, vram_byte_read_cmd, vram_read_cmd, vram_write_cmd,
        vsram_read_cmd, vsram_write_cmd,
    };
    use super::*;

    /// A control port register write, the way the 68k sends one.
    ///
    /// This has to come before the command word that sets up a data port
    /// write, not after it: a register write clears the code register, see
    /// `a_register_write_clears_the_code_register`.
    fn set_reg(vdp: &mut Vdp, reg: u8, value: u8) {
        let word = 0x8000 | ((u16::from(reg) & 0x1F) << 8) | u16::from(value);
        vdp.write_control(word);
    }

    /// Point the data port at `addr` for a VRAM write and prove it is set up.
    fn vram_write_at(vdp: &mut Vdp, addr: u16) {
        let (first, second) = vram_write_cmd(addr);
        vdp.write_control(first);
        vdp.write_control(second);
    }

    #[test]
    fn control_write_0x8f02_sets_r15() {
        let mut vdp = Vdp::new(false);
        vdp.write_control(0x8F02);
        assert_eq!(vdp.reg(15), 2);
    }

    #[test]
    fn control_write_0x8104_sets_r1() {
        let mut vdp = Vdp::new(false);
        vdp.write_control(0x8104);
        assert_eq!(vdp.reg(1), 0x04);
        assert!(!vdp.pending, "a register write is not half a command word");
    }

    #[test]
    fn vram_writes_walk_the_address_register_by_r15() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        vdp.write_data(0x1234);
        vdp.write_data(0x5678);
        assert_eq!(&vdp.vram[0..4], [0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn vram_write_command_reaches_the_top_half_of_vram() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        assert_eq!(
            vram_write_cmd(0xC000),
            (0x4000, 0x0003),
            "A15-A14 travel in the second word, A13-A0 in the first"
        );
        vram_write_at(&mut vdp, 0xC000);
        vdp.write_data(0xABCD);
        assert_eq!(&vdp.vram[0xC000..0xC002], [0xAB, 0xCD]);
    }

    #[test]
    fn an_odd_vram_address_swaps_the_two_bytes() {
        let mut vdp = Vdp::new(false);
        vram_write_at(&mut vdp, 0x0001);
        vdp.write_data(0x1234);
        assert_eq!(&vdp.vram[0..2], [0x34, 0x12]);
    }

    #[test]
    fn cram_write_lands_on_the_word_the_address_names() {
        let mut vdp = Vdp::new(false);
        let (first, second) = cram_write_cmd(0x0004);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0EEE);
        assert_eq!(vdp.cram[2], 0x0EEE);

        // R15 is 0 here, so this is the same word again: only the colour bits
        // of `----bbb0 ggg0 rrr0` survive.
        vdp.write_data(0xFFFF);
        assert_eq!(vdp.cram[2], 0x0EEE);
    }

    #[test]
    fn vsram_write_works_to_the_last_word_and_not_past_it() {
        let mut vdp = Vdp::new(false);
        let (first, second) = vsram_write_cmd(0x004E);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0123);
        assert_eq!(vdp.vsram[39], 0x0123);

        let (first, second) = vsram_write_cmd(0x0050);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x03FF);
        assert!(
            vdp.vsram
                .iter()
                .enumerate()
                .all(|(word, &value)| value == if word == 39 { 0x0123 } else { 0 }),
            "the write at index 40 was not ignored: {vdp_vsram:?}",
            vdp_vsram = &vdp.vsram[..]
        );
    }

    #[test]
    fn vsram_write_keeps_eleven_bits() {
        let mut vdp = Vdp::new(false);
        let (first, second) = vsram_write_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0xFFFF);
        assert_eq!(vdp.vsram[0], 0x07FF);
    }

    #[test]
    fn the_address_register_wraps_past_0xffff() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0xFFFE);
        vdp.write_data(0x1122);
        vdp.write_data(0x3344);
        assert_eq!(&vdp.vram[0xFFFE..], [0x11, 0x22]);
        assert_eq!(&vdp.vram[0..2], [0x33, 0x44]);
    }

    #[test]
    fn a_zero_r15_does_not_move_the_address() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 0);
        vram_write_at(&mut vdp, 0x1234);
        vdp.write_data(0x1122);
        vdp.write_data(0x3344);
        assert_eq!(&vdp.vram[0x1234..0x1236], [0x33, 0x44]);
        assert_eq!(&vdp.vram[0x1236..0x1238], [0, 0]);
    }

    #[test]
    fn the_first_command_word_is_enough_to_write_data() {
        let mut vdp = Vdp::new(false);

        // Something to notice the difference with: a CRAM write at $02, and
        // then the first half of a VRAM write at $06 with no second half.
        let (first, second) = cram_write_cmd(0x0002);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0EEE);
        assert_eq!(vdp.cram[1], 0x0EEE);

        vdp.write_control(0x4006);
        assert!(vdp.pending, "the second half is still expected");
        vdp.write_data(0xBEEF);
        assert_eq!(
            &vdp.vram[6..8],
            [0xBE, 0xEF],
            "the address of a half-written command word is live immediately"
        );
    }

    #[test]
    fn a_data_port_write_clears_pending() {
        let mut vdp = Vdp::new(false);
        vdp.write_control(vram_write_cmd(0x0000).0);
        assert!(vdp.pending);
        vdp.write_data(0x0000);
        assert!(!vdp.pending);
    }

    #[test]
    fn a_register_looking_word_after_the_first_half_is_the_second_half() {
        let mut vdp = Vdp::new(false);
        vdp.write_control(vram_write_cmd(0x0000).0);
        vdp.write_control(0x8F02);
        assert_eq!(
            vdp.reg(15),
            0,
            "0x8F02 arrived with pending set, so it is CD5-CD2 and A15-A14"
        );
        assert!(!vdp.pending);
        assert_eq!(vdp.code, 0b00_0001, "CD5-CD2 of 0x02 are all zero");
        assert_eq!(vdp.addr, 0x8000, "A15-A14 are the low two bits of 0x02");
    }

    #[test]
    fn a_register_write_clears_the_code_register() {
        let mut vdp = Vdp::new(false);
        vram_write_at(&mut vdp, 0x0000);
        vdp.write_control(0x8100);
        vdp.write_data(0x1234);
        assert!(
            vdp.vram[0..4].iter().all(|&byte| byte == 0),
            "the code register is a read code after a register write, so the \
             write is ignored (genvdp.txt section 7)"
        );
    }

    #[test]
    fn cd5_asks_for_a_dma_only_while_dma_is_enabled() {
        // CD5 is bit 7 of the second word of the command.
        const DMA_CMD: (u16, u16) = (0x4000, 0x0080);

        let mut vdp = Vdp::new(false);
        vdp.write_control(DMA_CMD.0);
        vdp.write_control(DMA_CMD.1);
        assert!(
            !vdp.dma_requested(),
            "R1 M1 clear: no DMA, though the command asked for one"
        );

        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 1, 0x10);
        vdp.write_control(DMA_CMD.0);
        vdp.write_control(DMA_CMD.1);
        assert!(vdp.dma_requested());
        assert_eq!(vdp.code, 0x21, "the command still sets up a VRAM write");
    }

    #[test]
    fn writing_after_a_read_command_is_ignored_but_still_increments() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        // First half of a command with CD1-CD0 = 00: a VRAM read at $0004.
        vdp.write_control(0x0004);
        vdp.write_data(0x1234);
        assert!(
            vdp.vram[4..6].iter().all(|&byte| byte == 0),
            "writes after a read operation are ignored"
        );
        assert_eq!(
            vdp.addr, 0x0006,
            "the port was still written to, so the address moved on"
        );
    }

    #[test]
    fn vram_read_returns_the_word_the_data_port_put_there() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xBEEF);

        let (first, second) = vram_read_cmd(0x0100);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0xBEEF);
    }

    /// A read does not have the odd-address byte swap a write has: genvdp.txt
    /// section 8 documents the swap for writing 16-bit data, and Genesis Plus
    /// GX reads `vram[addr & 0xFFFE]` with bit 0 of the address doing nothing
    /// (`vdp_68k_data_r_m5`, case `0x00`).
    #[test]
    fn vram_read_at_an_odd_address_reads_the_same_word() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0100);
        vdp.write_data(0xBEEF);

        let (first, second) = vram_read_cmd(0x0101);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0xBEEF, "no byte swap on a read");
    }

    /// Two reads in a row with R15 = 2 are the two words the address walked
    /// over, which is how a game reads a palette back off the chip.
    #[test]
    fn vram_reads_walk_the_address_register_by_r15() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 2);
        vram_write_at(&mut vdp, 0x0000);
        vdp.write_data(0x1122);
        vdp.write_data(0x3344);

        let (first, second) = vram_read_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0x1122);
        assert_eq!(vdp.read_data(), 0x3344);
    }

    /// A CRAM read is the stored colour word back again (`----bbb0 ggg0 rrr0`,
    /// genvdp.txt section 9), which is only true because the colour bits of
    /// `0x0ACE` are all live bits.
    #[test]
    fn cram_read_returns_the_colour_that_was_written() {
        let mut vdp = Vdp::new(false);
        let (first, second) = cram_write_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0ACE);
        assert_eq!(vdp.cram[0], 0x0ACE);

        let (first, second) = cram_read_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0x0ACE);
    }

    /// The four bits of a CRAM word that are not colour, and bit 0, come back
    /// as the last word written to the data port: "Unused bits are set using
    /// data from next available FIFO entry" (Genesis Plus GX's
    /// `vdp_68k_data_r_m5`, case `0x08`, where the FIFO entry stands where
    /// `fifo_last` stands here until M4-21a #99).
    #[test]
    fn cram_read_fills_its_unused_bits_from_the_last_written_word() {
        let mut vdp = Vdp::new(false);
        let (first, second) = cram_write_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0ACE);

        // Something else through the port to give `fifo_last` the bits CRAM
        // cannot hold, then back to the CRAM word.
        set_reg(&mut vdp, 15, 0);
        vram_write_at(&mut vdp, 0x0200);
        vdp.write_data(0xF111);

        let (first, second) = cram_read_cmd(0x0000);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0x0ACE | 0xF111);
    }

    #[test]
    fn vsram_read_returns_the_word_that_was_written() {
        let mut vdp = Vdp::new(false);
        let (first, second) = vsram_write_cmd(0x0004);
        vdp.write_control(first);
        vdp.write_control(second);
        vdp.write_data(0x0123);

        let (first, second) = vsram_read_cmd(0x0004);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0x0123);
    }

    /// Reading VSRAM where no word lives gives word 0 again.
    ///
    /// genvdp.txt section 10 only says "Writes to the addresses beyond 50h are
    /// ignored"; for reads Genesis Plus GX wraps the index back to address 0
    /// ("Wrap to address 0 (TODO: check if still true with Genesis 3 model)",
    /// `vdp_68k_data_r_m5`, case `0x04`). Its own TODO is why this is a wrap and
    /// not a hard fact.
    #[test]
    fn vsram_read_past_the_fortieth_word_wraps_to_the_first() {
        let mut vdp = Vdp::new(false);
        vdp.vsram[0] = 0x00AA;

        for addr in [0x0050u16, 0x0052, 0x007E] {
            let (first, second) = vsram_read_cmd(addr);
            vdp.write_control(first);
            vdp.write_control(second);
            assert_eq!(vdp.read_data(), 0x00AA, "read at {addr:#05x}");
        }
    }

    /// The undocumented 8-bit VRAM read returns the byte on the other side of
    /// the address: `1100` reads `vram[addr ^ 1]`, so the byte at $201 is what a
    /// read at $200 answers (Genesis Plus GX's `vdp_68k_data_r_m5`, case `0x0C`:
    /// "Read one byte from VRAM adjacent address").
    #[test]
    fn eight_bit_vram_read_returns_the_adjacent_byte() {
        let mut vdp = Vdp::new(false);
        vdp.vram[0x0200] = 0x12;
        vdp.vram[0x0201] = 0x34;

        let (first, second) = vram_byte_read_cmd(0x0200);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data() & 0xFF, 0x34);
    }

    /// ...and the top byte of that read is the port's own, the same open bus as
    /// the CRAM and VSRAM reads ("Unused bits are set using data from next
    /// available FIFO entry", Genesis Plus GX case `0x0C`).
    #[test]
    fn eight_bit_vram_read_fills_its_top_byte_from_the_last_written_word() {
        let mut vdp = Vdp::new(false);
        set_reg(&mut vdp, 15, 0);
        vram_write_at(&mut vdp, 0x0000);
        vdp.write_data(0xABCD);
        vdp.vram[0x0201] = 0x34;

        let (first, second) = vram_byte_read_cmd(0x0200);
        vdp.write_control(first);
        vdp.write_control(second);
        assert_eq!(vdp.read_data(), 0xAB34);
    }

    /// A read is a data port access like a write is, so the write-pending flag
    /// goes: "It is cleared when the data port is written to or read from"
    /// (genvdp.txt section 7).
    #[test]
    fn a_data_port_read_clears_pending() {
        let mut vdp = Vdp::new(false);
        vdp.write_control(vram_read_cmd(0x0000).0);
        assert!(vdp.pending);
        vdp.read_data();
        assert!(!vdp.pending);
    }

    /// Reading with a write code in the code register reads nothing.
    ///
    /// "You cannot write data after setting up a read operation, or read data
    /// after setting up a write operation" (genvdp.txt section 7). What the port
    /// answers instead is not in any document: Genesis Plus GX leaves its read
    /// switch and returns 0 (`vdp_68k_data_r_m5`, `data = 0` and the default
    /// case), while the 68k side of a real machine still has the last value the
    /// bus was holding. This returns `fifo_last`, the last word written through
    /// the port, because that is the value the bus was last driven with and it
    /// is the same answer the three read codes give for the bits their memory
    /// does not drive. TODO(M4-21a #99): a real FIFO decides it.
    ///
    /// The access still happened, so the address still moves on.
    #[test]
    fn reading_after_a_write_command_is_the_last_written_word() {
        for code in [0b0001u8, 0b0011, 0b0101] {
            let mut vdp = Vdp::new(false);
            set_reg(&mut vdp, 15, 2);
            // A command word for the write mode `code` at address $0100: CD1-CD0
            // in the first half, CD5-CD2 and A15-A14 in the second.
            vdp.write_control(((u16::from(code) & 0x03) << 14) | 0x0100);
            vdp.write_control((u16::from(code) & 0x3C) << 2);
            assert_eq!(vdp.code, code);

            vdp.write_data(0xBEEF);
            assert_eq!(vdp.read_data(), 0xBEEF, "code {code:#06b} is a write");
            assert_eq!(
                vdp.addr, 0x0104,
                "code {code:#06b}: the port was read, so the address moved on"
            );
        }
    }

    #[test]
    fn register_writes_need_bits_15_14_and_a_clear_pending_flag() {
        assert!(is_register_write(0x8F02, false));
        assert!(is_register_write(0x9FFF, false), "bit 13 does not matter");
        assert!(
            !is_register_write(0x8F02, true),
            "with a command word half done it is the second half instead"
        );
        assert!(!is_register_write(0x4000, false), "CD1-CD0 = 01");
        assert!(!is_register_write(0xC000, false), "CD1-CD0 = 11");
        assert!(!is_register_write(0x0000, false), "CD1-CD0 = 00");
    }

    #[test]
    fn register_fields_are_the_number_and_the_data() {
        assert_eq!(register_field(0x8F02), (15, 2));
        assert_eq!(register_field(0x97FF), (23, 0xFF));
        assert_eq!(
            register_field(0xBFFF),
            (31, 0xFF),
            "the 5 bits can name a register that does not exist"
        );
    }

    #[test]
    fn the_first_half_updates_only_cd1_cd0_and_a13_a0() {
        assert_eq!(first_word(0b00_0011, 0xFFFF, 0x4006), (0b00_0001, 0xC006));
        assert_eq!(
            first_word(0b00_1000, 0xD234, 0x0006),
            (0b00_1000, 0xC006),
            "CD5-CD2 and A15-A14 keep their values, A13-A0 and CD1-CD0 do not"
        );
        assert_eq!(first_word(0, 0, 0xC004), (0b00_0011, 0x0004));
    }

    #[test]
    fn the_second_half_fills_in_cd5_cd2_and_a15_a14() {
        assert_eq!(second_word(0b00_0001, 0x0006, 0x0000), (0b00_0001, 0x0006));
        assert_eq!(
            second_word(0b00_0000, 0x0004, 0x0080),
            (0x20, 0x0004),
            "CD5 is bit 7 of the second word"
        );
        assert_eq!(second_word(0b00_0001, 0x0004, 0x0010), (0b00_0101, 0x0004));
        assert_eq!(second_word(0, 0x3FFF, 0x0002), (0, 0xBFFF));
    }

    #[test]
    fn the_code_register_picks_the_memory_a_write_reaches() {
        assert_eq!(data_target(0x01), DataTarget::Vram);
        assert_eq!(data_target(0x03), DataTarget::Cram);
        assert_eq!(data_target(0x05), DataTarget::Vsram);
        assert_eq!(
            data_target(0x21),
            DataTarget::Vram,
            "CD5 (0x20) is the DMA trigger, not another destination"
        );
        assert_eq!(
            data_target(0x11),
            DataTarget::Vram,
            "CD4 (0x10) is ignored for data port accesses"
        );
        assert_eq!(data_target(0x00), DataTarget::Ignored, "VRAM read");
        assert_eq!(data_target(0x04), DataTarget::Ignored, "VSRAM read");
        assert_eq!(data_target(0x08), DataTarget::Ignored, "CRAM read");
        assert_eq!(
            data_target(0x0C),
            DataTarget::Ignored,
            "the undocumented 8-bit VRAM read"
        );
    }

    #[test]
    fn the_code_register_picks_the_memory_a_read_reaches() {
        assert_eq!(read_target(0x00), ReadTarget::Vram);
        assert_eq!(read_target(0x04), ReadTarget::Vsram);
        assert_eq!(read_target(0x08), ReadTarget::Cram);
        assert_eq!(
            read_target(0x0C),
            ReadTarget::VramByte,
            "the undocumented 8-bit VRAM read"
        );
        assert_eq!(
            read_target(0x1C),
            ReadTarget::VramByte,
            "CD4 (0x10) is ignored for data port accesses"
        );
        assert_eq!(
            read_target(0x28),
            ReadTarget::Cram,
            "CD5 (0x20) is the DMA trigger, not another destination"
        );
        assert_eq!(read_target(0x01), ReadTarget::NoRead, "VRAM write");
        assert_eq!(read_target(0x03), ReadTarget::NoRead, "CRAM write");
        assert_eq!(read_target(0x05), ReadTarget::NoRead, "VSRAM write");
        assert_eq!(
            read_target(0x02),
            ReadTarget::NoRead,
            "and so are the codes with no access at all"
        );
    }

    #[test]
    fn a_cram_read_is_the_colour_plus_the_port() {
        assert_eq!(cram_read_word(0x0ACE, 0xFFFF), 0x0ACE | 0xF111);
        assert_eq!(
            cram_read_word(0xFFFF, 0x0000),
            0x0EEE,
            "CRAM itself never holds the other five bits"
        );
        assert_eq!(cram_read_word(0x0000, 0xF111), 0xF111);
    }

    #[test]
    fn a_vsram_read_is_the_scroll_value_plus_the_port() {
        assert_eq!(vsram_read_word(0x0123, 0xFFFF), 0x0123 | 0xF800);
        assert_eq!(vsram_read_word(0xFFFF, 0x0000), 0x07FF);
        assert_eq!(vsram_read_word(0x0000, 0xF800), 0xF800);
    }

    #[test]
    fn an_eight_bit_vram_read_is_one_byte_low_and_the_port_high() {
        assert_eq!(vram_byte_read_word(0x34, 0xAB00), 0xAB34);
        assert_eq!(vram_byte_read_word(0x00, 0xFFFF), 0xFF00);
        assert_eq!(vram_byte_read_word(0xFF, 0x0000), 0x00FF);
    }

    #[test]
    fn a_vsram_read_index_wraps_past_the_last_word() {
        assert_eq!(vsram_read_index(0x0004), 2);
        assert_eq!(vsram_read_index(0x0005), 2, "address bit 0 has no effect");
        assert_eq!(vsram_read_index(0x004E), 39);
        assert_eq!(
            vsram_read_index(0x0050),
            0,
            "index 40 is nowhere: read word 0"
        );
        assert_eq!(vsram_read_index(0x007E), 0);
    }

    #[test]
    fn vram_word_bytes_are_big_endian_and_swap_on_an_odd_address() {
        assert_eq!(vram_word_bytes(0x0002, 0x1234), (0x0002, [0x12, 0x34]));
        assert_eq!(vram_word_bytes(0x0003, 0x1234), (0x0002, [0x34, 0x12]));
        assert_eq!(vram_word_bytes(0xFFFE, 0xABCD), (0xFFFE, [0xAB, 0xCD]));
        assert_eq!(vram_word_bytes(0xFFFF, 0xABCD), (0xFFFE, [0xCD, 0xAB]));
    }

    #[test]
    fn cram_index_is_bits_6_through_1() {
        assert_eq!(cram_word_index(0x0004), 2);
        assert_eq!(cram_word_index(0x0005), 2, "address bit 0 has no effect");
        assert_eq!(cram_word_index(0x007E), 63);
        assert_eq!(cram_word_index(0x8042), 33, "the high bits are ignored");
    }

    #[test]
    fn vsram_index_stops_at_the_40th_word() {
        assert_eq!(vsram_word_index(0x0004), Some(2));
        assert_eq!(vsram_word_index(0x0005), Some(2));
        assert_eq!(vsram_word_index(0x004E), Some(39));
        assert_eq!(vsram_word_index(0x0050), None);
        assert_eq!(vsram_word_index(0x007E), None);
    }

    #[test]
    fn auto_increment_is_an_unsigned_wrapping_add() {
        assert_eq!(increment(0x0000, 0), 0x0000, "zero means no increment");
        assert_eq!(increment(0xFFFE, 2), 0x0000);
        assert_eq!(
            increment(0x0001, 0xFF),
            0x0100,
            "R15 is unsigned: 0xFF is +255, not -1"
        );
        assert_eq!(increment(0x0000, 0x80), 0x0080, "bit 7 is a value bit");
        assert_eq!(increment(0xFFFF, 0xFF), 0x00FE);
    }

    #[test]
    fn a_dma_needs_both_cd5_and_r1_m1() {
        // 0x21 is a VRAM write with CD5 (0x20) set.
        assert!(dma_requested_by(0x21, true));
        assert!(
            !dma_requested_by(0x21, false),
            "R1 M1 clear means no DMA is allowed"
        );
        assert!(!dma_requested_by(0x01, true), "no CD5, no DMA");
    }
}
