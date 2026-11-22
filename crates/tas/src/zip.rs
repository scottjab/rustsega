//! Just enough of the zip format to open a `.bk2` and the `.zip` TASVideos
//! serves every movie in (#673).
//!
//! The layout is PKWARE's APPNOTE.TXT, section 4.3: entries are found through
//! the central directory at the end of the file (4.3.16 "End of central
//! directory record", 4.3.12 "Central directory structure"), and each one's
//! data follows its local header (4.3.7). Sizes and offsets are read from the
//! central directory, which is always filled in, rather than from the local
//! header, which leaves them zero when the writer used a data descriptor.
//!
//! Stored (method 0) and DEFLATE (method 8) entries are read; anything else,
//! ZIP64, and encryption are refused with an error rather than misread. Every
//! movie zip seen on TASVideos is DEFLATE.

use miniz_oxide::inflate::decompress_to_vec_with_limit;

/// The largest entry this will inflate: a long movie's input log is a few
/// megabytes (S3K's 73306-frame log is 1.7 MB), so this is room to spare and a
/// limit on what a hostile file can make us allocate.
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// "End of central directory signature 4 bytes (0x06054b50)" (APPNOTE 4.3.16).
const END_SIGNATURE: u32 = 0x0605_4B50;
/// "Central file header signature 4 bytes (0x02014b50)" (APPNOTE 4.3.12).
const CENTRAL_SIGNATURE: u32 = 0x0201_4B50;
/// "Local file header signature 4 bytes (0x04034b50)" (APPNOTE 4.3.7).
const LOCAL_SIGNATURE: u32 = 0x0403_4B50;
/// The fixed part of the end record; a comment of up to 64 KiB may follow it.
const END_LEN: usize = 22;
/// The fixed part of a central directory header.
const CENTRAL_LEN: usize = 46;
/// The fixed part of a local file header.
const LOCAL_LEN: usize = 30;

/// Does `bytes` start like a zip file: a local file header, or the end record
/// of an empty archive?
pub fn is_zip(bytes: &[u8]) -> bool {
    matches!(u32_at(bytes, 0), Some(LOCAL_SIGNATURE | END_SIGNATURE))
}

/// One file inside a zip, inflated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path the archive gives it, `/`-separated.
    pub name: String,
    /// Its contents.
    pub data: Vec<u8>,
}

/// Every file entry of a zip, in central directory order. Directory entries
/// (names ending in `/`) are left out.
pub fn entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let end = find_end(bytes).ok_or("not a zip file: no end of central directory record")?;
    let count = usize::from(u16_at(bytes, end + 10).ok_or("truncated zip")?);
    let mut at = u32_at(bytes, end + 16).ok_or("truncated zip")? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(bytes, at) != Some(CENTRAL_SIGNATURE) {
            return Err("damaged zip: central directory entry not found".into());
        }
        let header = bytes.get(at..at + CENTRAL_LEN).ok_or("truncated zip")?;
        let flags = le16(header, 8);
        let method = le16(header, 10);
        let packed = le32(header, 20) as usize;
        let size = le32(header, 24) as usize;
        let name_len = usize::from(le16(header, 28));
        let extra_len = usize::from(le16(header, 30));
        let comment_len = usize::from(le16(header, 32));
        let local = le32(header, 42) as usize;
        let name_bytes = bytes
            .get(at + CENTRAL_LEN..at + CENTRAL_LEN + name_len)
            .ok_or("truncated zip")?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        at += CENTRAL_LEN + name_len + extra_len + comment_len;

        if name.ends_with('/') {
            continue;
        }
        if flags & 1 != 0 {
            return Err(format!("{name}: encrypted zip entries are not supported"));
        }
        if packed == u32::MAX as usize || size == u32::MAX as usize {
            return Err(format!("{name}: ZIP64 entries are not supported"));
        }
        let data = read_entry(bytes, local, method, packed, size)
            .map_err(|why| format!("{name}: {why}"))?;
        out.push(Entry { name, data });
    }
    Ok(out)
}

/// The data of the entry whose local header is at `local`.
fn read_entry(
    bytes: &[u8],
    local: usize,
    method: u16,
    packed: usize,
    size: usize,
) -> Result<Vec<u8>, String> {
    if u32_at(bytes, local) != Some(LOCAL_SIGNATURE) {
        return Err("local header not found".into());
    }
    let name_len = usize::from(u16_at(bytes, local + 26).ok_or("truncated zip")?);
    let extra_len = usize::from(u16_at(bytes, local + 28).ok_or("truncated zip")?);
    let start = local + LOCAL_LEN + name_len + extra_len;
    let raw = bytes.get(start..start + packed).ok_or("truncated zip")?;
    if size > MAX_ENTRY_BYTES {
        return Err(format!("{size} bytes is more than a movie can need"));
    }
    match method {
        0 => Ok(raw.to_vec()),
        8 => decompress_to_vec_with_limit(raw, MAX_ENTRY_BYTES)
            .map_err(|_| "damaged DEFLATE data".to_string()),
        other => Err(format!("compression method {other} is not supported")),
    }
}

/// The offset of the end of central directory record: searched for backwards,
/// since an archive comment of up to 65535 bytes may follow it.
fn find_end(bytes: &[u8]) -> Option<usize> {
    let last = bytes.len().checked_sub(END_LEN)?;
    let first = last.saturating_sub(usize::from(u16::MAX));
    (first..=last)
        .rev()
        .find(|&at| u32_at(bytes, at) == Some(END_SIGNATURE))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// A zip writer for the tests: stored or DEFLATE entries, no data descriptors.
#[cfg(test)]
pub(crate) fn build(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    use miniz_oxide::deflate::compress_to_vec;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data, deflate) in files {
        let packed = if *deflate {
            compress_to_vec(data, 6)
        } else {
            data.to_vec()
        };
        let method: u16 = if *deflate { 8 } else { 0 };
        let local = out.len() as u32;
        let mut header = Vec::new();
        header.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        header.extend_from_slice(&[20, 0, 0, 0]);
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&[0; 8]); // time, date, crc: not checked here
        header.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);

        central.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&END_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_deflated_entries_come_back_whole() {
        let log = b"|..|........|........|\n".repeat(500);
        let zip = build(&[
            ("Header.txt", b"Platform GEN\n", false),
            ("Input Log.txt", &log, true),
        ]);
        assert!(is_zip(&zip));
        let files = entries(&zip).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "Header.txt");
        assert_eq!(files[0].data, b"Platform GEN\n");
        assert_eq!(files[1].data, log);
    }

    #[test]
    fn a_trailing_archive_comment_does_not_hide_the_directory() {
        let mut zip = build(&[("a.gmv", b"abc", true)]);
        let comment = b"published by tasvideos";
        let len = zip.len();
        zip[len - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        zip.extend_from_slice(comment);
        assert_eq!(entries(&zip).unwrap()[0].data, b"abc");
    }

    #[test]
    fn something_that_is_not_a_zip_is_refused() {
        assert!(!is_zip(b"Gens Movie TESTA"));
        assert!(entries(b"Gens Movie TESTA").is_err());
    }

    #[test]
    fn a_truncated_zip_is_an_error_not_a_panic() {
        let zip = build(&[("Input Log.txt", &b"|..|\n".repeat(100), true)]);
        let cut = |at: usize| {
            let mut short = zip[..at].to_vec();
            short.extend_from_slice(&zip[zip.len() - END_LEN..]);
            short
        };
        // Cut inside the entry's data or its headers: there is nothing to read.
        for at in [10, zip.len() / 2] {
            assert!(entries(&cut(at)).is_err(), "cut at {at}");
        }
        // Every other cut: whatever comes back, it comes back without a panic.
        for at in 0..zip.len() - END_LEN {
            let _ = entries(&cut(at));
        }
    }
}
