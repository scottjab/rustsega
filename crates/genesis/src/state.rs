//! The save state container of `docs/save-state-format.md` (M6-07, #127).
//!
//! Twenty bytes of header and then the postcard encoding of the machine:
//!
//! ```text
//! 0  magic          4 bytes, b"RSGS"
//! 4  format version u32, little endian
//! 8  ROM checksum   8 bytes, FNV-1a 64 of the loaded image, little endian
//! 16 payload length u32, little endian
//! 20 payload        postcard, the state of every chip and device in the machine
//! ```
//!
//! The header is hand-rolled and the payload is not self-describing, so both
//! halves of that contract matter: the version says which payload shape follows,
//! and the ROM checksum says which cartridge it follows for. A state carries no
//! ROM bytes - a front end has the file, and duplicating a 2 MiB cartridge into
//! every save slot to prove which game a save slot is for would be the
//! expensive way to make the same check.
//!
//! Every failure the reader can make is one of [`StateError`], and every check
//! is answered before a byte of the payload is applied, so a refused load leaves
//! the machine alone.

use std::fmt;

use crate::{Bus, Genesis, M68k, Scheduler, Z80};

/// Magic bytes at the front of every save state: the "RSGS" of rustsega, which
/// is what a caller checks before it believes anything else.
pub const MAGIC: &[u8; 4] = b"RSGS";
/// The format version this build writes, and the only one it reads.
///
/// Bumped by the change that breaks compatibility with old states. Migrating
/// one version to another is out of scope of #127 and does not exist yet.
pub const FORMAT_VERSION: u32 = 1;
/// Bytes of header: [`MAGIC`], the version, the ROM checksum and the length.
const HEADER_LEN: usize = 4 + 4 + 8 + 4;

/// FNV-1a 64's offset basis, the 64-bit widening of the 32-bit one.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64's prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Why a save state could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    /// The bytes do not start with [`MAGIC`], so they are not a save state.
    BadMagic,
    /// The state is for format version `n`, and this build knows
    /// [`FORMAT_VERSION`] only - newer or older, since there is no migration
    /// either way yet.
    UnsupportedVersion(u32),
    /// The state was written for a different cartridge: its ROM checksum is not
    /// [`rom_checksum`] of the ROM this machine has loaded.
    WrongRom,
    /// The header does not hold, or the payload does not decode: the length
    /// field disagrees with the bytes that follow it, or the postcard of the
    /// machine state is truncated or foreign.
    Corrupt,
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a save state: no RSGS magic"),
            Self::UnsupportedVersion(got) => {
                write!(
                    f,
                    "save state version {got}, this build knows {FORMAT_VERSION}"
                )
            }
            Self::WrongRom => write!(f, "save state is for a different cartridge"),
            Self::Corrupt => write!(f, "save state is truncated or does not decode"),
        }
    }
}

impl std::error::Error for StateError {}

/// The checksum a save state carries for the ROM it was written on: FNV-1a 64
/// over the loaded image.
///
/// Fowler-Noll-Vo "The FNV non-cryptographic hash algorithm", 32-bit offset
/// basis and prime widened to 64: per byte, `hash ^= byte` then
/// `hash = hash.wrapping_mul(FNV_PRIME)`. The same hash every golden value in
/// this repo is built from - `testkit::fnv1a_64`, reimplemented here because
/// `testkit` is a dev-dependency and a save state has to work in a release
/// build.
///
/// **What it is taken over**: the image as the machine holds it, [`Bus::rom`] -
/// [`crate::normalize_rom`]'s output, so an SMD image is checksummed
/// deinterleaved. A front end that wants the checksum of the file it read off
/// disk has to normalise it first: the state identifies the machine, not the
/// container.
///
/// It is an identity check and not a security check. It says "the same
/// cartridge", and nothing weaker than a hash that says that is available
/// without carrying the ROM, which the format does not do.
pub fn rom_checksum(rom: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in rom {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// The whole save state of `machine`: [`container`] over the postcard of the
/// machine's four moving parts.
///
/// What a state carries is the 68000, the sound CPU, [`Bus`] - and with it the
/// VDP, the I/O chip, the two working RAMs, the cartridge's save and bank
/// state, and the two sound chips - and the [`Scheduler`]'s clock position.
/// What it does not is the ROM, which the header names by [`rom_checksum`], and
/// the [`crate::audio::Mixer`]: the resampler, the levels and the filter are the
/// host's output path rather than part of the machine, they are built from
/// [`crate::Config`] by [`Genesis::new`], and a state written at 44.1 kHz must
/// not quietly rewrite the audio settings of a machine now running at 48.
pub(crate) fn encode(machine: &Genesis) -> Vec<u8> {
    let parts = (&machine.cpu, &machine.z80, &machine.bus, &machine.scheduler);
    // `to_allocvec` grows its own buffer, so the only postcard errors - a fixed
    // size limit and a serializer that refuses a type - cannot happen here: the
    // state is made of buffers and integers, and the size is the heap's.
    let payload = postcard::to_allocvec(&parts).expect("a Genesis state encodes");
    container(rom_checksum(&machine.bus.rom), &payload)
}

/// Put the machine `data` describes back into `machine`, whole or not at all:
/// every check in [`payload_of`] is answered before any part of the machine is
/// replaced, so a state that is refused leaves it running where it was.
pub(crate) fn decode(data: &[u8], machine: &mut Genesis) -> Result<(), StateError> {
    let payload = payload_of(data, &machine.bus.rom)?;
    let (cpu, z80, mut bus, scheduler) =
        postcard::from_bytes::<(M68k, Z80, Bus, Scheduler)>(payload)
            .map_err(|_| StateError::Corrupt)?;
    // The ROM is the one part of the machine a state names instead of carrying,
    // so this machine's own image - the one whose checksum the header just
    // matched - goes back on the bus the state replaced. `Bus`'s save RAM, its
    // EEPROM and its bank registers *are* in the state, and they came from this
    // same image, because the checksum says so.
    bus.rom = std::mem::take(&mut machine.bus.rom);
    machine.cpu = cpu;
    machine.z80 = z80;
    machine.bus = bus;
    machine.scheduler = scheduler;
    Ok(())
}

/// The bytes of a save state of `checksum` carrying `payload`.
fn container(checksum: u64, payload: &[u8]) -> Vec<u8> {
    let mut state = Vec::with_capacity(HEADER_LEN + payload.len());
    state.extend_from_slice(&header(checksum, payload.len()));
    state.extend_from_slice(payload);
    state
}

/// The payload of a save state, having checked `data`'s header against the ROM
/// `rom` that the state has to be for.
///
/// The order of the checks is what a caller can rely on: magic first, because it
/// is what says whether the rest of the header means anything; then the version,
/// because it says what the following bytes are; then the ROM, because it says
/// who they are for; then the length of the payload itself.
pub(crate) fn payload_of<'a>(data: &'a [u8], rom: &[u8]) -> Result<&'a [u8], StateError> {
    if data.len() < HEADER_LEN || &data[..4] != MAGIC {
        return Err(StateError::BadMagic);
    }
    let version = u32::from_le_bytes(data[4..8].try_into().expect("4 bytes, checked above"));
    if version != FORMAT_VERSION {
        return Err(StateError::UnsupportedVersion(version));
    }
    let checksum = u64::from_le_bytes(data[8..16].try_into().expect("8 bytes, checked above"));
    if checksum != rom_checksum(rom) {
        return Err(StateError::WrongRom);
    }
    let length =
        u32::from_le_bytes(data[16..20].try_into().expect("4 bytes, checked above")) as usize;
    if data.len() - HEADER_LEN != length {
        return Err(StateError::Corrupt);
    }
    Ok(&data[HEADER_LEN..])
}

/// The twenty header bytes of a state of `checksum` carrying `payload_len`
/// bytes.
fn header(checksum: u64, payload_len: usize) -> [u8; HEADER_LEN] {
    let mut header = [0_u8; HEADER_LEN];
    header[0..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[8..16].copy_from_slice(&checksum.to_le_bytes());
    header[16..20].copy_from_slice(&(payload_len as u32).to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash itself, against the two values the FNV-1a definition gives: the
    /// offset basis with nothing mixed in, and one byte of `a`.
    #[test]
    fn rom_checksum_is_fnv1a_64() {
        assert_eq!(rom_checksum(&[]), 0xcbf2_9ce4_8422_2325);
        assert_eq!(rom_checksum(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(rom_checksum(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// The container's header, written and read back: magic, version, the
    /// checksum of the ROM the payload is for, and the payload itself.
    #[test]
    fn container_header_round_trips() {
        let state = container(rom_checksum(b"rom"), b"payload");
        assert_eq!(&state[..4], MAGIC);
        assert_eq!(
            u32::from_le_bytes(state[4..8].try_into().unwrap()),
            FORMAT_VERSION
        );
        assert_eq!(payload_of(&state, b"rom").unwrap(), b"payload");
        assert_eq!(
            payload_of(&state, b"other").unwrap_err(),
            StateError::WrongRom
        );
    }

    #[test]
    fn a_short_header_is_bad_magic() {
        assert_eq!(payload_of(b"RSG", &[]), Err(StateError::BadMagic));
        assert_eq!(payload_of(&[], &[]), Err(StateError::BadMagic));
    }
}
