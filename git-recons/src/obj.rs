//! Git's own encodings: pack entry headers, ofs-delta offsets, delta instructions, loose
//! object headers, ids.

use sha1::{Digest, Sha1};

pub type Id = [u8; 20];

pub const COMMIT: u8 = 1;
pub const TREE: u8 = 2;
pub const BLOB: u8 = 3;
pub const TAG: u8 = 4;
pub const OFS: u8 = 6;
pub const REF: u8 = 7;

pub fn type_name(t: u8) -> Option<&'static [u8]> {
    Some(match t {
        COMMIT => b"commit",
        TREE => b"tree",
        BLOB => b"blob",
        TAG => b"tag",
        _ => return None,
    })
}

pub fn sha1(parts: &[&[u8]]) -> Id {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// The id of an object of type `t`.
pub fn object_id(t: u8, content: &[u8]) -> Id {
    let mut hdr = Vec::with_capacity(24);
    loose_header(t, content.len() as u64, &mut hdr);
    sha1(&[&hdr, content])
}

/// `type size\0`.
pub fn loose_header(t: u8, size: u64, out: &mut Vec<u8>) {
    out.extend_from_slice(type_name(t).unwrap());
    out.push(b' ');
    out.extend_from_slice(size.to_string().as_bytes());
    out.push(0);
}

/// Parses `type size\0` as git writes it: (type, size, header length).
pub fn parse_loose_header(b: &[u8]) -> Option<(u8, u64, usize)> {
    let sp = b.iter().take(8).position(|&c| c == b' ')?;
    let t = [COMMIT, TREE, BLOB, TAG].into_iter().find(|&t| type_name(t).unwrap() == &b[..sp])?;
    let nul = sp + 1 + b[sp + 1..].iter().take(21).position(|&c| c == 0)?;
    let size: u64 = std::str::from_utf8(&b[sp + 1..nul]).ok()?.parse().ok()?;
    let mut canon = Vec::new();
    loose_header(t, size, &mut canon);
    (canon == b[..nul + 1]).then_some((t, size, nul + 1))
}

pub fn hex(id: &[u8]) -> String {
    let mut s = String::with_capacity(id.len() * 2);
    for b in id {
        s.push(b"0123456789abcdef"[(b >> 4) as usize] as char);
        s.push(b"0123456789abcdef"[(b & 15) as usize] as char);
    }
    s
}

pub fn put_hex(id: &[u8], out: &mut Vec<u8>) {
    for b in id {
        out.push(b"0123456789abcdef"[(b >> 4) as usize]);
        out.push(b"0123456789abcdef"[(b & 15) as usize]);
    }
}

#[inline]
pub fn is_lhex(c: u8) -> bool {
    c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
}

/// 40 lowercase hex digits as an id.
pub fn parse_hex(h: &[u8]) -> Option<Id> {
    if h.len() != 40 {
        return None;
    }
    let mut id = [0u8; 20];
    for i in 0..20 {
        let d = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        id[i] = d(h[2 * i])? << 4 | d(h[2 * i + 1])?;
    }
    Some(id)
}

/// A pack entry header: (type, size, length), if canonical (git's own encoding of the pair).
pub fn parse_entry_header(b: &[u8]) -> Option<(u8, u64, usize)> {
    let (t, size, i) = entry_header_any(b)?;
    let mut canon = Vec::new();
    entry_header(t, size, &mut canon);
    (canon == b[..i]).then_some((t, size, i))
}

/// A pack entry header however encoded, as git reads it.
pub fn entry_header_any(b: &[u8]) -> Option<(u8, u64, usize)> {
    let c = *b.first()?;
    let t = c >> 4 & 7;
    let mut size = (c & 15) as u64;
    let mut shift = 4;
    let mut i = 1;
    let mut c = c;
    while c & 0x80 != 0 {
        c = *b.get(i)?;
        if shift > 57 {
            return None;
        }
        size |= ((c & 0x7f) as u64) << shift;
        shift += 7;
        i += 1;
    }
    Some((t, size, i))
}

pub fn entry_header(t: u8, size: u64, out: &mut Vec<u8>) {
    let mut c = t << 4 | (size & 15) as u8;
    let mut s = size >> 4;
    while s != 0 {
        out.push(c | 0x80);
        c = (s & 0x7f) as u8;
        s >>= 7;
    }
    out.push(c);
}

/// An ofs-delta's base offset (distance back) and the bytes it took.
pub fn parse_ofs(b: &[u8]) -> Option<(u64, usize)> {
    let mut c = *b.first()?;
    let mut off = (c & 0x7f) as u64;
    let mut i = 1;
    while c & 0x80 != 0 {
        c = *b.get(i)?;
        i += 1;
        if off >= 1 << 56 {
            return None;
        }
        off = ((off + 1) << 7) | (c & 0x7f) as u64;
    }
    Some((off, i))
}

pub fn put_ofs(off: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 10];
    let mut p = 9;
    buf[p] = (off & 0x7f) as u8;
    let mut o = off >> 7;
    while o != 0 {
        o -= 1;
        p -= 1;
        buf[p] = 0x80 | (o & 0x7f) as u8;
        o >>= 7;
    }
    out.extend_from_slice(&buf[p..]);
}

/// A delta header size (LEB128); canonical only.
pub fn parse_dsize(b: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    let start = *at;
    loop {
        let c = *b.get(*at)?;
        *at += 1;
        if shift > 63 {
            return None;
        }
        v |= ((c & 0x7f) as u64) << shift;
        shift += 7;
        if c & 0x80 == 0 {
            break;
        }
    }
    (*at - start == recons_common::varint::len(v)).then_some(v)
}

pub fn put_dsize(v: u64, out: &mut Vec<u8>) {
    recons_common::varint::put(out, v)
}

/// A delta's (base size, target size), however encoded.
pub fn delta_sizes(d: &[u8]) -> Option<(u64, u64)> {
    let mut at = 0;
    let mut rd = || {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let c = *d.get(at)?;
            at += 1;
            if shift < 64 {
                v |= ((c & 0x7f) as u64) << shift;
            }
            shift += 7;
            if c & 0x80 == 0 {
                return Some(v);
            }
        }
    };
    Some((rd()?, rd()?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Insert(usize),
    Copy { off: u64, size: u64 },
}

/// The next delta instruction at `*at` (an insert's data follows it), if git's canonical
/// encoding: offset and size bytes only where nonzero, 0x10000 as no size bytes.
pub fn parse_op(d: &[u8], at: &mut usize) -> Option<Op> {
    let c = *d.get(*at)?;
    *at += 1;
    if c & 0x80 == 0 {
        if c == 0 {
            return None;
        }
        return Some(Op::Insert(c as usize));
    }
    let mut off = 0u64;
    let mut size = 0u64;
    for i in 0..4 {
        if c & 1 << i != 0 {
            let b = *d.get(*at)?;
            *at += 1;
            if b == 0 {
                return None;
            }
            off |= (b as u64) << (8 * i);
        }
    }
    for i in 0..3 {
        if c & 0x10 << i != 0 {
            let b = *d.get(*at)?;
            *at += 1;
            if b == 0 {
                return None;
            }
            size |= (b as u64) << (8 * i);
        }
    }
    if size == 0x10000 {
        return None;
    }
    if size == 0 {
        size = 0x10000;
    }
    Some(Op::Copy { off, size })
}

pub fn put_copy(off: u64, size: u64, out: &mut Vec<u8>) {
    let at = out.len();
    out.push(0x80);
    let mut c = 0x80u8;
    for i in 0..4 {
        let b = (off >> (8 * i)) as u8;
        if b != 0 {
            c |= 1 << i;
            out.push(b);
        }
    }
    let s = if size == 0x10000 { 0 } else { size };
    for i in 0..3 {
        let b = (s >> (8 * i)) as u8;
        if b != 0 {
            c |= 0x10 << i;
            out.push(b);
        }
    }
    out[at] = c;
}

/// Applies a delta (any valid encoding, as git reads it).
pub fn apply_delta(base: &[u8], d: &[u8]) -> Option<Vec<u8>> {
    let (bs, ts) = delta_sizes(d)?;
    if bs != base.len() as u64 || ts > 1 << 40 {
        return None;
    }
    let mut at = 0;
    for _ in 0..2 {
        while d[at] & 0x80 != 0 {
            at += 1;
        }
        at += 1;
    }
    let mut out = Vec::with_capacity(ts.min(1 << 26) as usize);
    while at < d.len() {
        let c = d[at];
        at += 1;
        if c & 0x80 == 0 {
            let n = c as usize;
            if n == 0 {
                return None;
            }
            out.extend_from_slice(d.get(at..at + n)?);
            at += n;
        } else {
            let mut off = 0usize;
            let mut size = 0usize;
            for i in 0..4 {
                if c & 1 << i != 0 {
                    off |= (*d.get(at)? as usize) << (8 * i);
                    at += 1;
                }
            }
            for i in 0..3 {
                if c & 0x10 << i != 0 {
                    size |= (*d.get(at)? as usize) << (8 * i);
                    at += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            out.extend_from_slice(base.get(off..off.checked_add(size)?)?);
        }
    }
    (out.len() as u64 == ts).then_some(out)
}

/// Inflates a zlib stream: the data and the bytes consumed. `hint` sizes the output.
pub fn inflate(z: &[u8], hint: usize) -> Option<(Vec<u8>, usize)> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut d = Decompress::new(true);
    let mut out = Vec::with_capacity(hint.min(1 << 26) + 1);
    loop {
        let before = d.total_in() as usize;
        let st = d.decompress_vec(&z[before..], &mut out, FlushDecompress::Finish).ok()?;
        match st {
            Status::StreamEnd => return Some((out, d.total_in() as usize)),
            _ => {
                if out.len() < out.capacity() && d.total_in() as usize == z.len() {
                    return None;
                }
                out.reserve(out.capacity().max(64));
            }
        }
    }
}

/// The first `n` inflated bytes (fewer if the stream is shorter or broken).
pub fn inflate_prefix(z: &[u8], n: usize) -> Vec<u8> {
    use flate2::{Decompress, FlushDecompress};
    let mut d = Decompress::new(true);
    let mut out = Vec::with_capacity(n);
    let _ = d.decompress_vec(z, &mut out, FlushDecompress::Sync);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ofs_roundtrip() {
        for off in [0u64, 1, 127, 128, 129, 16511, 16512, 1 << 30, (1 << 40) + 12345] {
            let mut b = vec![];
            put_ofs(off, &mut b);
            assert_eq!(parse_ofs(&b), Some((off, b.len())), "{off}");
        }
    }

    #[test]
    fn header_roundtrip() {
        for (t, s) in [(1u8, 0u64), (3, 15), (3, 16), (7, 1 << 33), (2, 123456)] {
            let mut b = vec![];
            entry_header(t, s, &mut b);
            assert_eq!(parse_entry_header(&b), Some((t, s, b.len())));
        }
        // A redundant continuation byte is not canonical.
        assert_eq!(parse_entry_header(&[0xb5, 0x00]), None);
    }

    #[test]
    fn ops_roundtrip() {
        for (off, size) in [(0u64, 1u64), (5, 0x10000), (0x01020304, 0xff), (0x100, 0x10001), (7, 0xffffff)] {
            let mut b = vec![];
            put_copy(off, size, &mut b);
            let mut at = 0;
            assert_eq!(parse_op(&b, &mut at), Some(Op::Copy { off, size }));
            assert_eq!(at, b.len());
        }
        // 0x10000 spelled with a size byte.
        assert_eq!(parse_op(&[0xc0, 0x01], &mut 0), None);
    }

    #[test]
    fn loose_header_canonical() {
        assert_eq!(parse_loose_header(b"blob 12\0xyz"), Some((BLOB, 12, 8)));
        assert_eq!(parse_loose_header(b"blob 012\0"), None);
        assert_eq!(parse_loose_header(b"blub 12\0"), None);
    }
}
