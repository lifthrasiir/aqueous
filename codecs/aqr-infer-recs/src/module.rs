//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules: `recs-fields`, records from the byte planes of
//! their fields, and `recs-entries`, entries from their heads and tails by the lengths in the
//! heads.

use aqr_common::varint;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

/// How a field's values are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    Raw,
    /// Less the field of the record before (the first record less 0).
    Delta,
    /// Xor the field of the record before.
    Xor,
    /// Less the field of the same width at this offset of the record.
    Sub(usize),
    /// Xor the field of the same width at this offset of the record.
    XorRef(usize),
}

/// A field of a record: `width` bytes (1, 2, 4 or 8) at `off`, a number of either endianness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub off: usize,
    pub width: usize,
    pub big: bool,
    pub how: How,
}

/// The field kinds a length is read as: (width, big-endian, mask).
pub const KINDS: [(usize, bool, u32); 9] = [
    (1, false, 0xFF),
    (2, false, 0xFFFF),
    (2, false, 0x7FFF),
    (2, false, 0x0FFF),
    (2, true, 0xFFFF),
    (2, true, 0x7FFF),
    (2, true, 0x0FFF),
    (4, false, 0xFFFF_FFFF),
    (4, true, 0xFFFF_FFFF),
];

/// Entries with a length: at `d` of an entry, a field of kind KINDS[kind] holds v, and the entry
/// is align_up(b + v, align) bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rule {
    pub d: usize,
    pub kind: usize,
    pub b: usize,
    pub align: usize,
}

impl Rule {
    pub fn width(&self) -> usize {
        KINDS[self.kind].0
    }

    /// The length field of the entry at p (zero past the end of data).
    pub fn value(&self, data: &[u8], p: usize) -> u64 {
        let (w, big, mask) = KINDS[self.kind];
        let at = |i: usize| *data.get(p + self.d + i).unwrap_or(&0) as u64;
        let v = if big { (0..w).fold(0, |v, i| v << 8 | at(i)) } else { (0..w).rev().fold(0, |v, i| v << 8 | at(i)) };
        v & mask as u64
    }

    /// The length of the entry at p.
    pub fn length(&self, data: &[u8], p: usize) -> usize {
        let a = self.align as u64;
        ((self.b as u64 + self.value(data, p)).div_ceil(a) * a) as usize
    }

    pub fn describe(&self) -> String {
        let (w, big, mask) = KINDS[self.kind];
        format!(
            "length at {} as {}{}&{mask:#x}, +{} align {}",
            self.d,
            if big { "be" } else { "le" },
            8 * w,
            self.b,
            self.align
        )
    }
}

pub(crate) fn mask(width: usize) -> u64 {
    if width == 8 { !0 } else { (1 << (8 * width)) - 1 }
}

/// The field of each of the n records of m (rows of k bytes).
pub(crate) fn values(m: &[u8], k: usize, off: usize, width: usize, big: bool) -> Vec<u64> {
    m.chunks_exact(k)
        .map(|r| {
            let b = &r[off..off + width];
            if big {
                b.iter().fold(0, |v, &x| v << 8 | x as u64)
            } else {
                b.iter().rev().fold(0, |v, &x| v << 8 | x as u64)
            }
        })
        .collect()
}

// -- recs-fields

/// The params of `recs-fields`: n records of k bytes, by their fields (in order, covering the
/// record).
pub fn fields_params(n: usize, k: usize, fields: &[Field]) -> Vec<u8> {
    let mut p = Vec::new();
    for v in [n, k, fields.len()] {
        varint::put(&mut p, v as u64);
    }
    for f in fields {
        let (how, r) = match f.how {
            How::Raw => (0, None),
            How::Delta => (1, None),
            How::Xor => (2, None),
            How::Sub(r) => (3, Some(r)),
            How::XorRef(r) => (4, Some(r)),
        };
        p.push(f.width.trailing_zeros() as u8 | (f.big as u8) << 2 | how << 3);
        if let Some(r) = r {
            varint::put(&mut p, r as u64);
        }
    }
    p
}

/// The records, the record size and the fields of `recs-fields`' params, checked: the fields
/// cover the record in order, and refer only to earlier fields' bytes.
pub fn parse_fields(p: &[u8]) -> Result<(usize, usize, Vec<Field>)> {
    let mut at = 0;
    let get = |at: &mut usize| {
        varint::get(p, at).map_err(|_| corrupt()).and_then(|v| usize::try_from(v).map_err(|_| corrupt()))
    };
    let (n, k, count) = (get(&mut at)?, get(&mut at)?, get(&mut at)?);
    if count > k {
        return Err(corrupt());
    }
    let mut fields = Vec::with_capacity(count);
    let mut off = 0;
    for _ in 0..count {
        let c = *p.get(at).ok_or_else(corrupt)?;
        at += 1;
        let width = 1 << (c & 3);
        let r = if matches!(c >> 3, 3 | 4) { Some(get(&mut at)?) } else { None };
        let how = match (c >> 3, r) {
            (0, _) => How::Raw,
            (1, _) => How::Delta,
            (2, _) => How::Xor,
            (3, Some(r)) => How::Sub(r),
            (4, Some(r)) => How::XorRef(r),
            _ => return Err(corrupt()),
        };
        if r.is_some_and(|r| r.checked_add(width).is_none_or(|e| e > off)) {
            return Err(corrupt());
        }
        fields.push(Field { off, width, big: c & 4 != 0, how });
        off += width;
    }
    if off != k || at != p.len() || (n > 0 && k == 0) {
        return Err(corrupt());
    }
    Ok((n, k, fields))
}

/// The n records of k bytes back from the planes of their fields (None if not exactly).
pub fn decode_planes(planes: &[u8], n: usize, k: usize, fields: &[Field]) -> Option<Vec<u8>> {
    if n.checked_mul(k)? != planes.len() {
        return None;
    }
    let mut m = vec![0u8; n * k];
    let mut at = 0;
    for f in fields {
        let p = &planes[at..at + n * f.width];
        at += n * f.width;
        let mk = mask(f.width);
        let refs = match f.how {
            How::Sub(r) | How::XorRef(r) => values(&m, k, r, f.width, f.big),
            _ => Vec::new(),
        };
        let mut prev = 0u64;
        for i in 0..n {
            let t = (0..f.width).fold(0u64, |t, j| t | (p[j * n + i] as u64) << (8 * j));
            let v = match f.how {
                How::Raw => t,
                How::Delta => t.wrapping_add(prev) & mk,
                How::Xor => t ^ prev,
                How::Sub(_) => t.wrapping_add(refs[i]) & mk,
                How::XorRef(_) => t ^ refs[i],
            };
            prev = v;
            let r = &mut m[i * k + f.off..i * k + f.off + f.width];
            for (j, b) in r.iter_mut().enumerate() {
                let sh = if f.big { 8 * (f.width - 1 - j) } else { 8 * j };
                *b = (v >> sh) as u8;
            }
        }
    }
    Some(m)
}

/// The module `recs-fields`: the records of `params` from the planes of their fields.
pub fn decode_fields(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[planes] = inputs else { return Err(corrupt()) };
    let (n, k, fields) = parse_fields(params)?;
    if n.checked_mul(k).is_none_or(|l| l as u64 != len) {
        return Err(corrupt());
    }
    decode_planes(planes, n, k, &fields).ok_or_else(corrupt)
}

// -- recs-entries

/// The params of `recs-entries`: n entries with heads of `head` bytes, by a rule.
pub fn entries_params(n: usize, head: usize, r: &Rule) -> Vec<u8> {
    let mut p = Vec::new();
    for v in [n, head, r.kind, r.d, r.b, r.align] {
        varint::put(&mut p, v as u64);
    }
    p
}

/// The entries, the heads' size and the rule of `recs-entries`' params, checked: the length
/// field is within the head.
pub fn parse_entries(p: &[u8]) -> Result<(usize, usize, Rule)> {
    let mut at = 0;
    let mut v = [0usize; 6];
    for x in &mut v {
        *x = usize::try_from(varint::get(p, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt())?;
    }
    let [n, head, kind, d, b, align] = v;
    if at != p.len() || kind >= KINDS.len() || !(1..=4096).contains(&align) {
        return Err(corrupt());
    }
    let r = Rule { d, kind, b, align };
    if d.checked_add(r.width()).is_none_or(|e| e > head) {
        return Err(corrupt());
    }
    Ok((n, head, r))
}

/// The module `recs-entries`: the entries of `params`, each its head (of `heads`, one after
/// another) and as much of `tails` as its length leaves.
pub fn decode_entries(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[heads, tails] = inputs else { return Err(corrupt()) };
    let (n, head, r) = parse_entries(params)?;
    if n.checked_mul(head).is_none_or(|l| l != heads.len()) || (heads.len() + tails.len()) as u64 != len {
        return Err(corrupt());
    }
    let mut out = Vec::with_capacity(len as usize);
    let mut at = 0usize;
    for h in heads.chunks_exact(head.max(1)).take(n) {
        let tail = r.length(h, 0).checked_sub(head).ok_or_else(corrupt)?;
        out.extend_from_slice(h);
        out.extend_from_slice(tails.get(at..at.checked_add(tail).ok_or_else(corrupt)?).ok_or_else(corrupt)?);
        at += tail;
    }
    if at != tails.len() {
        return Err(corrupt());
    }
    Ok(out)
}
