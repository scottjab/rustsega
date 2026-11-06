//! Status register (SR) bit constants and masks.
//!
//! The SR is 16 bits. Bit layout, from the M68000UM programming-model chapter,
//! section "Status Register (SR)" and "Condition Code Register (CCR)":
//!
//! ```text
//!  15  14  13  12  11  10  9   8   7   6   5   4   3   2   1   0
//! +---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
//! | T | 0 | S | 0 | 0 | I2| I1| I0| 0 | 0 | 0 | X | N | Z | V | C |
//! +---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
//! ```
//!
//! Bits 14, 12, 11, 7, 6 and 5 do not exist on the 68000. Every write to SR
//! must be masked with [`SR_MASK`]; the CCR is the low byte and is masked with
//! [`CCR_MASK`].

/// Trace bit (bit 15). Taking a trace exception is not implemented yet.
pub const SR_T: u16 = 0x8000;
/// Supervisor bit (bit 13). 1 = supervisor mode, 0 = user mode. Changing it
/// swaps the active stack pointer, see `M68k::set_sr`.
pub const SR_S: u16 = 0x2000;
/// Interrupt mask, bits 10..8. 0 accepts any interrupt, 7 blocks all
/// (non-NMI) interrupts.
pub const SR_INT_MASK: u16 = 0x0700;
/// Extend flag (bit 4), used by add/subtract with carry and by shifts.
pub const SR_X: u16 = 0x0010;
/// Negative flag (bit 3): result bit 15 (or 31) of the last operation.
pub const SR_N: u16 = 0x0008;
/// Zero flag (bit 2): the last result was zero.
pub const SR_Z: u16 = 0x0004;
/// Overflow flag (bit 1): signed overflow of the last operation.
pub const SR_V: u16 = 0x0002;
/// Carry flag (bit 0): carry out of the most significant bit.
pub const SR_C: u16 = 0x0001;

/// Every bit that actually exists on the 68000 SR: T, S, I2–I0, X, N, Z, V, C.
/// All SR writes go through `M68k::set_sr`, which ANDs with this.
pub const SR_MASK: u16 = 0xA71F;

/// The CCR is the low byte of the SR: X, N, Z, V, C.
pub const CCR_MASK: u16 = 0x1F;

/// SR right after a reset: S = 1 (supervisor), T = 0, interrupt mask = 7, all
/// condition codes clear. See M68000UM chapter 5 "Reset".
pub const SR_RESET: u16 = 0x2700;
