//! Serialising the boxed fixed-size buffers of the machine state.
//!
//! `serde` implements `Serialize` and `Deserialize` for `[T; N]` only up to
//! thirty-two entries, so a `Box<[u8; 0x10000]>` - work RAM - and a
//! `Box<[u8; 8192]>` - the sound RAM - cannot be derived straight. This is the
//! `#[serde(with = ...)]` for them: the buffer goes out as a byte sequence and
//! comes back as one, which is what
//! [`serde_big_array`](https://docs.rs/serde-big-array) does and what #126
//! reached for by changing the field type to `Box<[u8]>`.
//!
//! The fields stay boxed arrays here because that is what they are: 64 KiB of
//! work RAM is a fixed region of the address map, and `Bus::ram` and
//! `Z80Side::ram` are read and written as `[u8; N]` by every device and test
//! that touches one. The length is checked on the way in, so a state whose RAM
//! is the wrong size is a decode error rather than a machine with a hole in it.
//!
//! The VDP has the same problem and a different answer - `vdp::big_array` -
//! because its arrays are copied once a line and cannot become heap buffers.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The buffer as a sequence of its bytes.
pub fn serialize<S, T, const N: usize>(data: &[T; N], ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    data.as_slice().serialize(ser)
}

/// A sequence of exactly `N` entries, boxed.
pub fn deserialize<'de, D, T, const N: usize>(de: D) -> Result<Box<[T; N]>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let entries = Vec::<T>::deserialize(de)?;
    <[T; N]>::try_from(entries)
        .map(Box::new)
        .map_err(|too_big: Vec<T>| {
            D::Error::invalid_length(too_big.len(), &"a buffer of exactly the right size")
        })
}
