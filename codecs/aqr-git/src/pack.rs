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

/// git's header of an entry of type `t` and data of `size` bytes, in the front of the array (of
/// the count of bytes).
pub fn header(t: u8, size: u64) -> ([u8; 10], usize) {
    let mut h = [0; 10];
    let mut n = 0;
    let mut c = t << 4 | (size & 15) as u8;
    let mut s = size >> 4;
    while s != 0 {
        h[n] = c | 0x80;
        n += 1;
        c = (s & 0x7f) as u8;
        s >>= 7;
    }
    h[n] = c;
    (h, n + 1)
}

/// [`header`] appended to `out`.
pub fn put_header(t: u8, size: u64, out: &mut Vec<u8>) {
    let (h, n) = header(t, size);
    out.extend_from_slice(&h[..n]);
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
        let e = entry_at(d, p)?;
        p = e.end();
        out.push(e);
    }
    Some(out)
}

/// The entry at `p` of `d`.
#[cfg(feature = "encode")]
fn entry_at(d: &[u8], p: usize) -> Option<Entry> {
    let h = header_len(d.get(p..)?)?;
    let t = header_type(&d[p..])?;
    let b = p + h;
    let z = b + match t {
        COMMIT | TREE | BLOB | TAG => 0,
        // (an ofs-delta's distance is continued as a header's size is)
        OFS_DELTA => header_len(d.get(b..)?)?,
        REF_DELTA => ID,
        _ => return None,
    };
    if !zlib_header(*d.get(z..)?.first_chunk()?) {
        return None;
    }
    let end = z + 2 + aqr_flate::inflate::parse_head(d.get(z + 2..)?, usize::MAX, &mut Skip).ok()?.end;
    d.get(end..end + 4)?;
    Some(Entry { t, header: p..b, base: b..z, deflate: z + 2..end })
}

/// Where the entries of the pack at the start of `d` end, where they start at `at` (as an
/// index tells, in order): those [`entries`] finds, each parsed on its own on up to `threads`
/// threads. None where they are not.
#[cfg(feature = "encode")]
pub fn entries_end(d: &[u8], at: &[usize], threads: usize) -> Option<usize> {
    if !starts(d) || count(d)? as usize != at.len() || at.first().is_some_and(|&p| p != HEADER) {
        return None;
    }
    let ends = aqr_common::pool::par_map(at.len(), threads, |j| entry_at(d, at[j]).map(|e| e.end()));
    let mut end = HEADER;
    for (&p, e) in at.iter().zip(ends) {
        if p != end {
            return None;
        }
        end = e?;
    }
    Some(end)
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

    /// An entry of type `t` (header of the size 3, a stored block of "abc"), with `base` between.
    #[cfg(feature = "encode")]
    fn entry(t: u8, base: &[u8], zlib: [u8; 2]) -> Vec<u8> {
        let mut e = Vec::new();
        put_header(t, 3, &mut e);
        e.extend_from_slice(base);
        e.extend_from_slice(&zlib);
        e.extend_from_slice(&[1, 3, 0, 0xfc, 0xff, b'a', b'b', b'c']);
        e.extend_from_slice(&[0x02, 0x4d, 0x01, 0x27]);
        e
    }

    #[cfg(feature = "encode")]
    fn pack_of(count: u32, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut p = b"PACK\0\0\0\x02".to_vec();
        p.extend_from_slice(&count.to_be_bytes());
        for e in entries {
            p.extend_from_slice(e);
        }
        p
    }

    #[cfg(feature = "encode")]
    #[test]
    fn entries_reject_one_by_one() {
        let ok = [0x78, 0x01];
        let good = entry(BLOB, &[], ok);
        let p = pack_of(1, std::slice::from_ref(&good));
        assert_eq!(entries(&p).unwrap().len(), 1);
        assert_eq!(extent(&p), Some(p.len()));
        // (a ofs-delta's distance, and a ref-delta's id)
        assert_eq!(entries(&pack_of(1, &[entry(OFS_DELTA, &[5], ok)])).unwrap()[0].base.len(), 1);
        assert_eq!(entries(&pack_of(1, &[entry(REF_DELTA, &[7; ID], ok)])).unwrap()[0].base.len(), ID);
        // the types 0 and 5 are no entry's
        for t in [0, 5] {
            assert!(entries(&pack_of(1, &[entry(t, &[], ok)])).is_none(), "type {t}");
        }
        // zlib headers: a preset dictionary (FDICT), not 0x78 (other windows are fine), not DEFLATE,
        // a bad check
        assert!(zlib_header([0x78, 0x9c]) && !zlib_header([0x78, 0xbb]));
        assert!(entries(&pack_of(1, &[entry(BLOB, &[], [0x78, 0xbb])])).is_none(), "FDICT");
        assert!(entries(&pack_of(1, &[entry(BLOB, &[], [0x78, 0x00])])).is_none(), "bad check");
        assert!(entries(&pack_of(1, &[entry(BLOB, &[], [0x79, 0x00])])).is_none(), "not DEFLATE");
        assert!(entries(&pack_of(1, &[entry(BLOB, &[], [0x68, 0x81])])).is_some(), "a smaller window");
        assert!(entries(&pack_of(1, &[entry(BLOB, &[], [0x88, 0x00])])).is_none(), "window 2^16");
        // version 3 is a pack, 4 not
        let mut v = p.clone();
        v[7] = 3;
        assert!(entries(&v).is_some());
        v[7] = 4;
        assert!(entries(&v).is_none());
        v[7] = 1;
        assert!(entries(&v).is_none());
        // the count past the entries; none where one is told; the count short of them is a prefix
        assert!(entries(&pack_of(2, std::slice::from_ref(&good))).is_none());
        assert!(entries(&pack_of(1, &[])).is_none());
        assert_eq!(entries(&pack_of(1, &[good.clone(), good.clone()])).unwrap().len(), 1);
        assert_eq!(entries(&pack_of(0, &[])).unwrap().len(), 0);
        assert_eq!(extent(&pack_of(0, &[])), Some(HEADER));
        // the Adler-32 cut off
        assert!(entries(&p[..p.len() - 1]).is_none());
        // too short for a header
        assert!(entries(&p[..11]).is_none() && entries(b"").is_none());
    }

    #[cfg(feature = "encode")]
    #[test]
    fn entries_of_huge_counts_and_damage() {
        use std::time::Duration;
        let good = entry(BLOB, &[], [0x78, 0x01]);
        let p = pack_of(2, &[good.clone(), good]);
        // (a count of 4 billion takes no allocation it cannot fill)
        let mut big = p.clone();
        big[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(aqr_testkit::within(Duration::from_secs(10), || entries(&big)).unwrap().is_none());
        for d in aqr_testkit::damages(p.len(), 64, p.len()) {
            let q = d.apply(&p);
            aqr_testkit::within(Duration::from_secs(10), || {
                let _ = entries(&q);
                let _ = extent(&q);
                let _ = entries_end(&q, &[HEADER, HEADER + 25], 2);
            })
            .unwrap_or_else(|e| panic!("{d}: {e}"));
        }
        // entries_end: the places must follow one another
        let e = entries(&p).unwrap();
        let at = [e[0].header.start, e[1].header.start];
        assert_eq!(entries_end(&p, &at, 2), Some(e[1].end()));
        assert_eq!(entries_end(&p, &[at[0], at[1] + 1], 1), None);
        assert_eq!(entries_end(&p, &at[..1], 1), None);
        assert_eq!(entries_end(&p, &[at[0] + 1, at[1]], 1), None);
    }
}
