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
/// one version to another is out of scope of #127 and does not exist yet, so a
/// state of any other version is refused with [`StateError::UnsupportedVersion`]
/// rather than half-decoded.
///
/// Version 2 is `vdp::frame_state` carrying the one field of an interlace mode 2
/// picture that no later frame draws (#457). Version 1 wrote the frame buffer as
/// its size alone, and since a postcard payload is not self-describing a v1 mode-2
/// payload would have decoded a plausible-looking machine out of the wrong bytes
/// rather than failing - which is the case the version field exists for.
///
/// Version 3 is the two freeze gates of `Scheduler::m68k_frozen_until` and
/// `Scheduler::z80_frozen_until` (#517): two `u64`s added to `Scheduler`, so the
/// payload of a machine mid-freeze is eight-odd bytes longer and a v2 reader would
/// have run out of message in the middle of the last field. It is not a bump taken
/// for the shape alone - a state that does not carry a gate resumes a frame with the
/// rest of a 68k -> VDP transfer silently dropped, which is the same class of wrong
/// as the mode-2 field that made 2 what it is.
///
/// Version 5 is the Acclaim 32 Mbit board's read-window latch (#271): one enum
/// field added to `cart::eeprom::Eeprom`, so a v4 payload's EEPROM bytes are the
/// v5 reader's enum and a machine restored from one would come back with a
/// window decoded out of the array bytes. The latch being in the wrong state is
/// the same class of wrong as the two above - the ROM window shows, but a game
/// that reads its EEPROM answer through the armed quadrant reads the image's
/// bit 0 instead of the chip's line.
///
/// Version 7 is the YM2612's `&A4` F-Num latch (#336): two fields moved off
/// `phase::Channel` onto `phase::PhaseGen`, because the chip has one `&A4`
/// register, not six. A v6 payload decodes the channel array into a reader whose
/// channel no longer expects the fields and whose generator does, so a restored
/// chip would run with its latch lost and the next `&A0` write applying block
/// 0 rather than what the driver had latched.
///
/// Version 8 is the SVP board (#433): an `Option<cart::svp::Svp>` field added to
/// [`Bus`], after the J-CART field that made 6 (a field that was not there when
/// a v7 payload was written). The same class of wrong as every bump above: a
/// v7 payload reads into a struct that expects the field, and postcard fails
/// or - worse - shifts the tail of the payload into it.
///
/// Version 9 is the SVP's DSP (#433 stage 2): two fields added to the board
/// struct the v8 payload carries - the DSP's IRAM and the `ssp1601::Ssp1601`
/// core. A v8 payload would decode into a reader whose board expects both,
/// so the state of the running DSP would be lost with the machine.
pub const FORMAT_VERSION: u32 = 9;
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
/// The order of those four parts *is* the payload format, and one of them is
/// conditional inside: the VDP's frame state rides with [`Bus`], and the mode it
/// is woven in comes from the VDP's register file, which is a little earlier in
/// the same struct. Nothing here relies on that. The conditional field carries
/// its own presence tag - see `vdp::frame_state` and #457 - so a reader needs
/// nothing it has already decoded to know how many bytes `frame_state` is, and
/// reordering this tuple stays a pure version bump rather than a trap.
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
    // The mixer is not in the state - it is the host's output path, and a state
    // written at 44.1 kHz must not rewrite the settings of a machine now running
    // at 48 - but it is now behind the machine it has to follow, because the
    // machine has been put back. Re-derive its position from the machine that was
    // just loaded and leave the settings alone: see [`crate::audio::Mixer::seek`]
    // and #362.
    let now = machine.bus.audio.now();
    machine.mixer.seek(now);
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

    /// A state of another version is refused with its version in the error
    /// rather than decoded as this version's payload: the fields one build
    /// writes after the header are not the fields another build wrote there,
    /// and the version is the only thing that says so. Both directions, the
    /// older one being the shape the versions before this one wrote.
    #[test]
    fn a_foreign_version_is_refused_not_misparsed() {
        let state = container(rom_checksum(b"rom"), b"payload");
        for version in [FORMAT_VERSION + 1, FORMAT_VERSION - 1] {
            let mut foreign = state.clone();
            foreign[4..8].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                payload_of(&foreign, b"rom"),
                Err(StateError::UnsupportedVersion(version)),
                "version {version}"
            );
        }
    }
}
