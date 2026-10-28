//! The pack format (git's `gitformat-pack`): a 12-byte header (`PACK`, the version 2 or 3 and
//! the count of entries, big endian), the entries, and the SHA-1 of all that.
//!
//! An entry is a header of its type and the size of its data (bits 4-6 of the first byte the
//! type, the size in 4 bits then 7 at a time, least significant first, while bit 7 is set),
//! then for an ofs-delta the distance back to its base's entry, for a ref-delta its base's
//! id (20 bytes), then a zlib stream of its data.

use sha1::{Digest, Sha1};

pub const HEADER: usize = 12;
pub const ID: usize = 20;

pub const COMMIT: u8 = 1;
pub const TREE: u8 = 2;
pub const BLOB: u8 = 3;
pub const TAG: u8 = 4;
pub const OFS_DELTA: u8 = 6;
pub const REF_DELTA: u8 = 7;

/// Whether `d` starts with a pack header (of version 2 or 3).
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(b"PACK") && matches!(d.get(4..8), Some([0, 0, 0, 2 | 3]))
}

/// The count of entries of the pack at the start of `d`.
pub fn count(d: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(8..)?.first_chunk()?))
}

/// The bytes of an entry's header at the start of `b`, however encoded (git reads more than it
/// writes): bytes while bit 7 is set, then one more, within 10.
pub fn header_len(b: &[u8]) -> Option<usize> {
    let n = b.iter().take(10).position(|&c| c & 0x80 == 0)? + 1;
    Some(n)
}

/// The type of the entry whose header starts `b`.
pub fn header_type(b: &[u8]) -> Option<u8> {
    Some(b.first()? >> 4 & 7)
}

/// The size an entry's header (`b`, all of it) tells.
pub fn header_size(b: &[u8]) -> u64 {
    let mut size = (b[0] & 15) as u64;
    for (i, &c) in b[1..].iter().enumerate() {
        size |= ((c & 0x7f) as u64).checked_shl(4 + 7 * i as u32).unwrap_or(0);
    }
    size
}

/// git's header of an entry of type `t` and data of `size` bytes.
pub fn put_header(t: u8, size: u64, out: &mut Vec<u8>) {
    let mut c = t << 4 | (size & 15) as u8;
    let mut s = size >> 4;
    while s != 0 {
        out.push(c | 0x80);
        c = (s & 0x7f) as u8;
        s >>= 7;
    }
    out.push(c);
}

/// The bytes of an ofs-delta's distance at the start of `b`: bytes while bit 7 is set, then one
/// more, within 10.
pub fn ofs_len(b: &[u8]) -> Option<usize> {
    header_len(b)
}

/// Whether `h` is a zlib header (RFC 1950) of DEFLATE without a preset dictionary.
pub fn zlib_header(h: [u8; 2]) -> bool {
    h[0] & 15 == 8 && h[0] >> 4 <= 7 && h[1] & 0x20 == 0 && (h[0] as u16 * 256 + h[1] as u16).is_multiple_of(31)
}

/// The SHA-1 of `d`, a pack's trailer.
pub fn sha1(d: &[u8]) -> [u8; ID] {
    Sha1::digest(d).into()
}

/// An entry of a pack: its type, and where its parts are in the pack.
#[cfg(feature = "encode")]
#[derive(Debug, Clone)]
pub struct Entry {
    pub t: u8,
    /// Its header.
    pub header: std::ops::Range<usize>,
    /// Its base: the distance of an ofs-delta, the id of a ref-delta; else empty.
    pub base: std::ops::Range<usize>,
    /// Its DEFLATE stream (after the zlib header, before the Adler-32).
    pub deflate: std::ops::Range<usize>,
}

#[cfg(feature = "encode")]
impl Entry {
    /// Where it ends: after the Adler-32.
    pub fn end(&self) -> usize {
        self.deflate.end + 4
    }

    pub fn zlib(&self, d: &[u8]) -> [u8; 2] {
        [d[self.deflate.start - 2], d[self.deflate.start - 1]]
    }
}

/// The entries of the pack at the start of `d`, or None where it is not one (the count of
/// entries runs past `d`, or one of them is not of a type of git's or its zlib stream does not
/// end). Data is inflated to find where streams end, but not kept.
#[cfg(feature = "encode")]
pub fn entries(d: &[u8]) -> Option<Vec<Entry>> {
    if !starts(d) {
        return None;
    }
    let n = count(d)? as usize;
    // (each entry takes 8 bytes at least)
    let mut out = Vec::with_capacity(n.min(d.len() / 8));
    let mut p = HEADER;
    for _ in 0..n {
        let h = header_len(d.get(p..)?)?;
        let t = header_type(&d[p..])?;
        let b = p + h;
        let z = b + match t {
            COMMIT | TREE | BLOB | TAG => 0,
            OFS_DELTA => ofs_len(d.get(b..)?)?,
            REF_DELTA => ID,
            _ => return None,
        };
        if !zlib_header(*d.get(z..)?.first_chunk()?) {
            return None;
        }
        let end = z + 2 + aqr_flate::inflate::parse_head(d.get(z + 2..)?, usize::MAX, &mut Skip).ok()?.end;
        d.get(end..end + 4)?;
        out.push(Entry { t, header: p..b, base: b..z, deflate: z + 2..end });
        p = end + 4;
    }
    Some(out)
}

/// Bytes of the pack at the start of `d`: its entries and its trailer (where there is room for
/// it), or None where it is not one.
#[cfg(feature = "encode")]
pub fn extent(d: &[u8]) -> Option<usize> {
    let end = entries(d)?.last().map_or(HEADER, Entry::end);
    Some((end + ID).min(d.len()))
}

/// A sink of nothing.
#[cfg(feature = "encode")]
struct Skip;

#[cfg(feature = "encode")]
impl aqr_flate::inflate::Sink for Skip {
    fn lit(&mut self, _: u8) {}
    fn mat(&mut self, _: u16, _: u16, _: bool) {}
    fn stored(&mut self, _: &[u8]) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        for (t, size) in [(1, 0), (3, 15), (3, 16), (2, 2047), (6, 2048), (7, 1 << 40)] {
            let mut h = Vec::new();
            put_header(t, size, &mut h);
            assert_eq!(header_len(&h), Some(h.len()));
            assert_eq!(header_type(&h), Some(t));
            assert_eq!(header_size(&h), size);
        }
        // (git reads a size of more bytes than it needs)
        assert_eq!(header_size(&[0x95, 0x80, 0]), 5);
        assert_eq!(header_len(&[0x80; 10]), None);
    }
}
