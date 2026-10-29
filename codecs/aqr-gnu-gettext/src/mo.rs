//! The layout of a `.mo` file (GNU gettext's manual, "The Format of GNU MO Files"): a header of
//! 7 words (magic, revision, `N` strings, the offsets of the table of the originals `O` and of
//! the translations `T`, the size `S` and the offset `H` of the hash table), each table `N`
//! pairs of words (a string's length, its offset), the hash table `S` words, and the strings,
//! each followed by a NUL. All in the byte order the magic is read in.
//!
//! What is derived: the two tables, where the strings lie one after another, originals then
//! translations, from a given start (their lengths told by the NULs in them: plural forms hold
//! several), and the hash table, where it is GNU msgfmt's. The rest of the file is kept as it
//! is, the header and the strings with it.

use crate::{Result, corrupt};

/// The magic, read in the file's byte order.
pub const MAGIC: u32 = 0x9504_12de;

/// Bytes of the header's 7 words.
pub const HEADER: usize = 28;

/// Whether `d` starts with the magic, and if so whether big-endian.
pub fn endianness(d: &[u8]) -> Option<bool> {
    let m = *d.first_chunk::<4>()?;
    if u32::from_le_bytes(m) == MAGIC {
        Some(false)
    } else if u32::from_be_bytes(m) == MAGIC {
        Some(true)
    } else {
        None
    }
}

/// The header of a file, its tables' places checked to be in order and apart: the originals'
/// table, the translations', then the hash table (left out where of size 0).
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub big: bool,
    pub n: usize,
    pub originals: usize,
    pub translations: usize,
    pub hash_size: usize,
    pub hash: usize,
}

impl Layout {
    /// The layout of the header at the start of `d`.
    pub fn of(d: &[u8]) -> Option<Layout> {
        let big = endianness(d)?;
        let w = |i: usize| Some(word(d.get(4 * i..4 * i + 4)?, big) as usize);
        let l = Layout { big, n: w(2)?, originals: w(3)?, translations: w(4)?, hash_size: w(5)?, hash: w(6)? };
        let table = 8usize.checked_mul(l.n)?;
        let ok = l.originals >= HEADER
            && l.translations >= l.originals.checked_add(table)?
            && (l.hash_size == 0 || (l.hash_size > 2 && l.hash >= l.translations.checked_add(table)?))
            && l.hash.checked_add(4usize.checked_mul(l.hash_size)?).is_some();
        ok.then_some(l)
    }

    /// The parts derived, `(offset, bytes)`, in order (the hash table, of size 0, after the
    /// translations' table where there is none).
    pub fn derived(&self) -> [(usize, usize); 3] {
        let tables = [(self.originals, 8 * self.n), (self.translations, 8 * self.n)];
        let hash = match self.hash_size {
            0 => (self.translations + 8 * self.n, 0),
            s => (self.hash, 4 * s),
        };
        [tables[0], tables[1], hash]
    }

    /// The bytes of the file the parts derived are of.
    pub fn end(&self) -> usize {
        self.derived().iter().map(|&(o, n)| o + n).max().unwrap()
    }
}

pub fn word(b: &[u8], big: bool) -> u32 {
    let b = [b[0], b[1], b[2], b[3]];
    if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}

fn put_word(out: &mut [u8], v: u32, big: bool) {
    out[..4].copy_from_slice(&if big { v.to_be_bytes() } else { v.to_le_bytes() });
}

/// GNU gettext's hash of a string (`hash_string`: hashpjw), up to its first NUL.
pub fn hash(s: &[u8]) -> u32 {
    let mut h = 0u32;
    for &c in s.iter().take_while(|&&c| c != 0) {
        h = (h << 4).wrapping_add(c as u32);
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
            h ^= g;
        }
    }
    h
}

/// The hash table of `size` slots of the originals `keys`, as msgfmt fills it: each original in
/// order at its hash modulo the size, else at steps of 1 plus its hash modulo the size less 2,
/// the slot holding its index plus 1. None where the table cannot hold them.
pub fn hash_table(keys: impl ExactSizeIterator<Item = impl AsRef<[u8]>>, size: usize) -> Option<Vec<u32>> {
    if keys.len() >= size {
        return None;
    }
    let mut t = vec![0u32; size];
    let size = size as u64;
    for (i, k) in keys.enumerate() {
        let h = hash(k.as_ref()) as u64;
        let (mut at, step) = (h % size, 1 + h % (size - 2));
        while t[at as usize] != 0 {
            at = if at >= size - step { at - (size - step) } else { at + step };
        }
        t[at as usize] = i as u32 + 1;
    }
    Some(t)
}

/// The file of `len` bytes from `rest` (the file without the parts derived), `start` (where the
/// strings start) and `nuls` (for each string, originals then translations, the NULs within
/// it: varints).
pub fn build(rest: &[u8], start: usize, nuls: &[u8], len: usize) -> Result<Vec<u8>> {
    let l = Layout::of(rest).ok_or_else(corrupt)?;
    let derived = l.derived();
    let holes: usize = derived.iter().map(|d| d.1).sum();
    if rest.len().checked_add(holes) != Some(len) || l.end() > len {
        return Err(corrupt());
    }
    // the rest with zeros where the parts derived go
    let mut out = Vec::with_capacity(len);
    let mut at = 0;
    for &(o, n) in &derived {
        let take = o - out.len();
        out.extend_from_slice(rest.get(at..at + take).ok_or_else(corrupt)?);
        out.resize(out.len() + n, 0);
        at += take;
    }
    out.extend_from_slice(&rest[at..]);
    // the strings' lengths and offsets
    let mut p = 0;
    let mut off = start;
    let mut strings = Vec::with_capacity(2 * l.n);
    for _ in 0..2 * l.n {
        let k = aqr_common::varint::get(nuls, &mut p).map_err(|_| corrupt())?;
        let mut end = off;
        for _ in 0..=k {
            end += out.get(end..).and_then(|s| s.iter().position(|&c| c == 0)).ok_or_else(corrupt)? + 1;
        }
        // (the strings must not run into the tables)
        if derived.iter().any(|&(o, n)| n > 0 && off < o + n && o < end) {
            return Err(corrupt());
        }
        strings.push((off, end - 1 - off));
        off = end;
    }
    if p != nuls.len() {
        return Err(corrupt());
    }
    for (i, &(off, n)) in strings.iter().enumerate() {
        let table = if i < l.n { l.originals + 8 * i } else { l.translations + 8 * (i - l.n) };
        put_word(&mut out[table..], n as u32, l.big);
        put_word(&mut out[table + 4..], off as u32, l.big);
    }
    if l.hash_size > 0 {
        let keys = strings[..l.n].iter().map(|&(o, n)| &out[o..o + n]).collect::<Vec<_>>();
        let t = hash_table(keys.iter(), l.hash_size).ok_or_else(corrupt)?;
        for (i, v) in t.into_iter().enumerate() {
            put_word(&mut out[l.hash + 4 * i..], v, l.big);
        }
    }
    Ok(out)
}

/// The parts of `.mo` file `d` that [`build`] takes back to it: the file without the parts
/// derived, where the strings start and their NULs; None where it is not one or not so laid
/// out.
#[cfg(feature = "encode")]
pub fn split(d: &[u8]) -> Option<(Vec<u8>, usize, Vec<u8>)> {
    let l = Layout::of(d)?;
    if l.end() > d.len() || u32::try_from(d.len()).is_err() {
        return None;
    }
    let entry = |table: usize, i: usize| {
        let e = &d[table + 8 * i..];
        (word(e, l.big) as usize, word(&e[4..], l.big) as usize)
    };
    let start = if l.n > 0 { entry(l.originals, 0).1 } else { 0 };
    let mut nuls = Vec::new();
    for (table, i) in (0..l.n).map(|i| (l.originals, i)).chain((0..l.n).map(|i| (l.translations, i))) {
        let (n, off) = entry(table, i);
        let s = d.get(off..off.checked_add(n)?.checked_add(1)?)?;
        aqr_common::varint::put(&mut nuls, s[..n].iter().filter(|&&c| c == 0).count() as u64);
    }
    let mut rest = Vec::with_capacity(d.len());
    let mut at = 0;
    for (o, n) in l.derived() {
        rest.extend_from_slice(&d[at..o]);
        at = o + n;
    }
    rest.extend_from_slice(&d[at..]);
    (build(&rest, start, &nuls, d.len()).ok()? == d).then_some((rest, start, nuls))
}
