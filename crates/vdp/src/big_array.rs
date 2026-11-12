//! Serialising the fixed-size buffers of a chip state.
//!
//! `serde` implements `Serialize` and `Deserialize` for `[T; N]` only up to
//! thirty-two entries, so the 64-word CRAM, the 40-word VSRAM, the 80-line
//! sprite cache and the 80-entry sprite list of a line cannot be derived
//! straight. This module is the `#[serde(with = ...)]` for them: a buffer goes
//! out as a sequence and comes back as one, which is what
//! [`serde_big_array`](https://docs.rs/serde-big-array) does and what #126
//! reached for with `Box<[u8]>` in the YM2612's register banks.
//!
//! A `Box<[T]>` is not open to the VDP. `crate::latch::LineState` is written
//! once a line by copying the live chip's register file and VSRAM into it, and
//! `#[derive(Copy)]`-style struct literals of arrays are what make that cost
//! nothing (docs/ARCHITECTURE.md: "Core crates must not allocate per frame in
//! hot paths"). A heap buffer per line latch is the one thing that rule says
//! not to do, so the arrays stay arrays and only their serialisation is
//! described here.
//!
//! The length is checked on the way in: a state whose buffer is the wrong size
//! is a decode error rather than a truncated chip.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A buffer as a sequence of its `N` entries.
pub fn serialize<S, T, const N: usize>(data: &[T; N], ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    data.as_slice().serialize(ser)
}

/// A sequence of exactly `N` entries, which is the buffer it was written from.
pub fn deserialize<'de, D, T, const N: usize>(de: D) -> Result<[T; N], D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let entries = Vec::<T>::deserialize(de)?;
    <[T; N]>::try_from(entries).map_err(|too_big| {
        D::Error::invalid_length(too_big.len(), &"a buffer of exactly the right size")
    })
}
