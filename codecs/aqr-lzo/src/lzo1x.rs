//! LZO1X streams: a decoder into the data and the parse, and the encoder of a parse
//! (`FORMAT.md` §3).
//!
//! A stream is literal runs and matches. Its bytes are the parse, together with two choices the
//! format leaves to the encoder: how the first run is written ([`Parse::first_short`]), and
//! where both a short form and M3 can write a match, which of them ([`Form`]).

use crate::{Error, Result, corrupt};

/// The farthest a match reaches back.
pub const WINDOW: usize = 0xbfff;
/// The end of a stream: M4 of distance 16384 (and length 3).
pub const END: [u8; 3] = [0x11, 0, 0];

/// How a match is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Two bytes: M1 (after 1-3 literals, length 2; after 4 or more, length 3 from 2049-3072
    /// back) or M2 (length 3-8, at most 2048 back).
    Short,
    /// M3 (`001LLLLL`, at most 16384 back) or M4 (`0001HLLL`, 16385-49151 back), as the
    /// distance says.
    Long,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Literals before the match.
    pub lits: u32,
    pub len: u32,
    pub dist: u32,
    pub form: Form,
}

/// A stream's parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parse {
    pub matches: Vec<Match>,
    /// Literals after the last match.
    pub tail: u32,
    /// Whether the first run (of 1-238 literals) is written as its own first byte (`17 + n`)
    /// rather than as a run instruction (`0000LLLL`, of 4 or more).
    pub first_short: bool,
}

/// The state of the decoder before a match after `lits` literals: how many follow, up to 4.
#[inline]
pub fn state(lits: u32) -> u32 {
    lits.min(4)
}

/// Whether the short form writes a match of `len` and `dist` after `lits` literals.
#[inline]
pub fn short_fits(lits: u32, len: u32, dist: u32) -> bool {
    match (state(lits), len) {
        (1..=3, 2) => dist <= 1024,
        (4, 3) => (2049..=3072).contains(&dist) || dist <= 2048,
        (_, 3..=8) => dist <= 2048,
        _ => false,
    }
}

/// Whether the long form writes a match of `len` and `dist`.
#[inline]
pub fn long_fits(len: u32, dist: u32) -> bool {
    len >= 3 && (1..=WINDOW as u32).contains(&dist)
}

/// A stream decoded.
pub struct Decoded {
    pub data: Vec<u8>,
    pub parse: Parse,
    /// Bytes of the stream, up to its end.
    pub used: usize,
}

struct Reader<'a> {
    s: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    #[inline]
    fn byte(&mut self) -> Result<u32> {
        let b = *self.s.get(self.p).ok_or_else(corrupt)?;
        self.p += 1;
        Ok(b as u32)
    }

    /// A length of `bits` bits in `field`, continued in bytes where it is 0.
    #[inline]
    fn len(&mut self, field: u32, mask: u32) -> Result<u32> {
        if field != 0 {
            return Ok(field);
        }
        let mut n = mask;
        loop {
            match self.byte()? {
                0 => n = n.checked_add(255).ok_or_else(corrupt)?,
                b => return n.checked_add(b).ok_or_else(corrupt),
            }
        }
    }

    fn le16(&mut self) -> Result<u32> {
        Ok(self.byte()? | self.byte()? << 8)
    }
}

/// The LZO1X stream at the start of `s`, decoded to at most `limit` bytes of data. A stream
/// that ends other than with [`END`] is refused, as [`encode`] would not give it back. (LZO-RLE
/// streams start with 17 and their version, which fails as a match before any data.)
pub fn decode(s: &[u8], limit: usize) -> Result<Decoded> {
    let mut r = Reader { s, p: 0 };
    let mut data: Vec<u8> = Vec::new();
    let mut parse = Parse::default();
    let mut lits = 0u32;
    let copy = |data: &mut Vec<u8>, r: &mut Reader, n: u32| -> Result<()> {
        let n = n as usize;
        let b = s.get(r.p..r.p + n).ok_or_else(corrupt)?;
        if data.len() + n > limit {
            return Err(Error("data over the limit"));
        }
        data.extend_from_slice(b);
        r.p += n;
        Ok(())
    };
    let first = *s.first().ok_or_else(corrupt)? as u32;
    if first > 17 {
        r.p = 1;
        lits = first - 17;
        copy(&mut data, &mut r, lits)?;
        parse.first_short = true;
    }
    loop {
        let t = r.byte()?;
        let (len, dist, s_bits, form);
        if t < 16 {
            if lits == 0 {
                // A run of literals.
                let n = r.len(t, 15)?.checked_add(3).ok_or_else(corrupt)?;
                copy(&mut data, &mut r, n)?;
                lits = n;
                continue;
            }
            let h = r.byte()?;
            if lits < 4 {
                (len, dist) = (2, (h << 2) + (t >> 2 & 3) + 1);
            } else {
                (len, dist) = (3, (h << 2) + (t >> 2 & 3) + 2049);
            }
            (s_bits, form) = (t & 3, Form::Short);
        } else if t < 32 {
            len = r.len(t & 7, 7)?.checked_add(2).ok_or_else(corrupt)?;
            let x = r.le16()?;
            let d = 16384 + ((t & 8) << 11) + (x >> 2);
            if d == 16384 {
                if t != 0x11 || x != 0 {
                    return Err(Error("unusual end of an LZO stream"));
                }
                break;
            }
            (dist, s_bits, form) = (d, x & 3, Form::Long);
        } else if t < 64 {
            len = r.len(t & 31, 31)?.checked_add(2).ok_or_else(corrupt)?;
            let x = r.le16()?;
            (dist, s_bits, form) = ((x >> 2) + 1, x & 3, Form::Long);
        } else {
            let h = r.byte()?;
            len = if t < 128 { 3 + (t >> 5 & 1) } else { 5 + (t >> 5 & 3) };
            (dist, s_bits, form) = ((h << 3) + (t >> 2 & 7) + 1, t & 3, Form::Short);
        }
        if dist as usize > data.len() || data.len() + len as usize > limit {
            return Err(corrupt());
        }
        let from = data.len() - dist as usize;
        if dist >= len {
            data.extend_from_within(from..from + len as usize);
        } else {
            for i in 0..len as usize {
                data.push(data[from + i]);
            }
        }
        parse.matches.push(Match { lits, len, dist, form });
        lits = s_bits;
        copy(&mut data, &mut r, s_bits)?;
    }
    parse.tail = lits;
    Ok(Decoded { data, parse, used: r.p })
}

/// Writes a length of `v` (at least 1) in a field of `mask`, continued in bytes beyond.
fn put_len(out: &mut Vec<u8>, op: u8, v: u32, mask: u32) {
    if v <= mask {
        out.push(op | v as u8);
        return;
    }
    out.push(op);
    let mut v = v - mask;
    while v > 255 {
        out.push(0);
        v -= 255;
    }
    out.push(v as u8);
}

/// Writes literal run `lits` of `data` at `at`, after a match whose last byte `s_at` (if any)
/// holds the count of up to 3.
fn put_lits(out: &mut Vec<u8>, data: &[u8], at: usize, lits: u32, s_at: Option<usize>) {
    if lits == 0 {
        return;
    }
    if lits <= 3 {
        out[s_at.expect("short runs follow a match")] |= lits as u8;
    } else {
        put_len(out, 0, lits - 3, 15);
    }
    out.extend_from_slice(&data[at..at + lits as usize]);
}

/// The stream of `parse` over `data`, which must cover it exactly with matches that
/// [`short_fits`] or [`long_fits`] as their forms say.
pub fn encode(data: &[u8], parse: &Parse) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 16);
    let mut w = Writer::new(data, &mut out);
    for m in &parse.matches {
        w.push(m, parse.first_short);
    }
    w.finish(parse.tail, parse.first_short);
    out
}

/// A stream over `data` appended to a vector a match at a time, as [`encode`] writes it of a
/// parse: so that a parse need not be held to be written.
pub struct Writer<'a> {
    data: &'a [u8],
    out: &'a mut Vec<u8>,
    /// Where the data and the instructions written are, the last match's instruction.
    at: usize,
    s_at: Option<usize>,
    first: bool,
}

impl<'a> Writer<'a> {
    pub fn new(data: &'a [u8], out: &'a mut Vec<u8>) -> Writer<'a> {
        Writer { data, out, at: 0, s_at: None, first: true }
    }

    /// Appends the match `m` after its literals; `first_short` as [`Parse::first_short`] (known
    /// by the first match).
    pub fn push(&mut self, m: &Match, first_short: bool) {
        let (data, out) = (self.data, &mut *self.out);
        if std::mem::take(&mut self.first) && first_short {
            out.push(17 + m.lits as u8);
            out.extend_from_slice(&data[..m.lits as usize]);
        } else {
            put_lits(out, data, self.at, m.lits, self.s_at);
        }
        self.at += m.lits as usize;
        let d = m.dist;
        match m.form {
            Form::Short if m.len <= 3 && state(m.lits) != 0 && !(m.len == 3 && d <= 2048) => {
                let d = if m.len == 2 { d - 1 } else { d - 2049 };
                out.extend_from_slice(&[((d & 3) << 2) as u8, (d >> 2) as u8]);
            }
            Form::Short => {
                let d = d - 1;
                let op = if m.len <= 4 { 0x40 | (m.len - 3) << 5 } else { 0x80 | (m.len - 5) << 5 };
                out.extend_from_slice(&[(op | (d & 7) << 2) as u8, (d >> 3) as u8]);
            }
            Form::Long if d <= 16384 => {
                put_len(out, 0x20, m.len - 2, 31);
                out.extend_from_slice(&(((d - 1) << 2) as u16).to_le_bytes());
            }
            Form::Long => {
                let d = d - 16384;
                put_len(out, 0x10 | (d >> 11 & 8) as u8, m.len - 2, 7);
                out.extend_from_slice(&(((d & 0x3fff) << 2) as u16).to_le_bytes());
            }
        }
        self.s_at = Some(out.len() - 2);
        self.at += m.len as usize;
    }

    /// Appends the literals after the last match, `tail` of them, and the end.
    pub fn finish(self, tail: u32, first_short: bool) {
        let (data, out) = (self.data, self.out);
        if self.first && first_short {
            out.push(17 + tail as u8);
            out.extend_from_slice(&data[..tail as usize]);
        } else {
            put_lits(out, data, self.at, tail, self.s_at);
        }
        out.extend_from_slice(&END);
    }
}
