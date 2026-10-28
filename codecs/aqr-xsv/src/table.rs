//! What the encoder and the module share: the dialect of a table (its params), the codes of the
//! streams, and the table rebuilt from its streams (`FORMAT.md`, "Rebuilding the file").

use aqr_common::varint;

use crate::{Result, corrupt};

/// The most columns a table has; fields after the last are in the last.
pub const MAX_COLUMNS: usize = 4096;
/// The longest delimiter or record terminator.
pub const MAX_SEP: usize = 4;

/// A field's type (a byte of `types`): its kind shifted left by one, and whether it is quoted.
pub const K_TEXT: u8 = 0;
/// A number of the descriptor of the column's previous number.
pub const K_NUM: u8 = 1;
/// A number of a descriptor given (`desc`).
pub const K_NEW: u8 = 2;

/// The end of a record: the terminator (LF where the dialect has none), CR LF (only where it
/// has none), or nothing (the last record of a file).
pub const E_TERM: u64 = 0;
pub const E_CRLF: u64 = 1;
pub const E_NONE: u64 = 2;

/// A column's numbers as they are, or each as the change from the column's previous one.
pub const P_RAW: u8 = 0;
pub const P_DELTA: u8 = 1;

/// How the fields of a table are separated and quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialect {
    /// Between two fields of a record: 1 to [`MAX_SEP`] bytes.
    pub delim: Vec<u8>,
    /// The byte that quotes fields, if any.
    pub quote: Option<u8>,
    /// The end of a record, up to [`MAX_SEP`] bytes; none for a line's end (LF, or CR LF).
    pub term: Vec<u8>,
}

impl Dialect {
    /// The params of the module: the dialect, then the number of columns.
    pub fn to_params(&self, columns: usize) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(self.delim.len() as u8);
        p.extend_from_slice(&self.delim);
        p.push(self.quote.unwrap_or(0));
        p.push(self.term.len() as u8);
        p.extend_from_slice(&self.term);
        varint::put(&mut p, columns as u64);
        p
    }

    /// The dialect and the number of columns of `params`.
    pub fn from_params(p: &[u8]) -> Result<(Dialect, usize)> {
        let mut at = 0;
        let mut bytes = |min: usize| -> Result<Vec<u8>> {
            let n = *p.get(at).ok_or_else(corrupt)? as usize;
            let b = p.get(at + 1..at + 1 + n).filter(|_| (min..=MAX_SEP).contains(&n)).ok_or_else(corrupt)?;
            at += 1 + n;
            Ok(b.to_vec())
        };
        let delim = bytes(1)?;
        let quote = *p.get(at).ok_or_else(corrupt)?;
        at += 1;
        let term = {
            let n = *p.get(at).ok_or_else(corrupt)? as usize;
            let b = p.get(at + 1..at + 1 + n).filter(|_| n <= MAX_SEP).ok_or_else(corrupt)?;
            at += 1 + n;
            b.to_vec()
        };
        let columns = varint::get(p, &mut at).map_err(|_| corrupt())?;
        if at != p.len() || !(1..=MAX_COLUMNS as u64).contains(&columns) {
            return Err(corrupt());
        }
        Ok((Dialect { delim, quote: (quote != 0).then_some(quote), term }, columns as usize))
    }
}

/// The streams of a table (`FORMAT.md`, "Streams"), as the module takes them.
pub struct View<'a> {
    pub layout: &'a [u8],
    pub raw: &'a [u8],
    pub columns: &'a [u8],
    pub types: &'a [u8],
    pub text: &'a [u8],
    pub nums: &'a [u8],
    pub desc: &'a [u8],
}

/// A column's parts of the streams, read from the front, and its state.
struct Column<'a> {
    pred: u8,
    types: &'a [u8],
    text: &'a [u8],
    nums: &'a [u8],
    desc: &'a [u8],
    /// The descriptor (mode, flags, scale) of the column's previous number, if any.
    last: Option<(u8, u8, i32)>,
    prev: i64,
}

/// Takes the first `n` bytes of `s`.
fn take<'a>(s: &mut &'a [u8], n: u64) -> Result<&'a [u8]> {
    let n = usize::try_from(n).map_err(|_| corrupt())?;
    if n > s.len() {
        return Err(corrupt());
    }
    let (a, b) = s.split_at(n);
    *s = b;
    Ok(a)
}

fn get(s: &mut &[u8]) -> Result<u64> {
    let mut at = 0;
    let v = varint::get(s, &mut at).map_err(|_| corrupt())?;
    *s = &s[at..];
    Ok(v)
}

fn byte(s: &mut &[u8]) -> Result<u8> {
    let (&b, rest) = s.split_first().ok_or_else(corrupt)?;
    *s = rest;
    Ok(b)
}

/// The file of `len` bytes of the table of dialect `d` and `ncols` columns from its streams `v`.
pub fn build(d: &Dialect, ncols: usize, v: View, len: usize) -> Result<Vec<u8>> {
    let View { mut layout, mut raw, mut columns, mut types, mut text, mut nums, mut desc } = v;
    let mut cols = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let pred = byte(&mut columns)?;
        if pred > P_DELTA {
            return Err(corrupt());
        }
        let n = [get(&mut columns)?, get(&mut columns)?, get(&mut columns)?, get(&mut columns)?];
        cols.push(Column {
            pred,
            types: take(&mut types, n[0])?,
            text: take(&mut text, n[1])?,
            nums: take(&mut nums, n[2])?,
            desc: take(&mut desc, n[3])?,
            last: None,
            prev: 0,
        });
    }
    if !(columns.is_empty() && types.is_empty() && text.is_empty() && nums.is_empty() && desc.is_empty()) {
        return Err(corrupt());
    }
    let mut out = Vec::with_capacity(len);
    while !layout.is_empty() {
        let r = get(&mut layout)?;
        if r == 0 {
            let n = get(&mut layout)?;
            out.extend_from_slice(take(&mut raw, n)?);
        } else {
            let (n, end) = (r >> 2, r & 3);
            for j in 0..n {
                if j > 0 {
                    out.extend_from_slice(&d.delim);
                }
                field(d, &mut cols[(j as usize).min(ncols - 1)], &mut out)?;
                // (a corrupt layout of many empty fields stops here)
                if out.len() > len {
                    return Err(corrupt());
                }
            }
            match (end, d.term.is_empty()) {
                (E_TERM, true) => out.push(b'\n'),
                (E_TERM, false) => out.extend_from_slice(&d.term),
                (E_CRLF, true) => out.extend_from_slice(b"\r\n"),
                (E_NONE, _) => {}
                _ => return Err(corrupt()),
            }
        }
        if out.len() > len {
            return Err(corrupt());
        }
    }
    let left =
        cols.iter().any(|c| !(c.types.is_empty() && c.text.is_empty() && c.nums.is_empty() && c.desc.is_empty()));
    if left || !raw.is_empty() || out.len() != len {
        return Err(corrupt());
    }
    Ok(out)
}

/// The next field of column `c`, appended to `out`.
fn field(d: &Dialect, c: &mut Column, out: &mut Vec<u8>) -> Result<()> {
    let t = byte(&mut c.types)?;
    let quote = match t & 1 {
        0 => None,
        _ => Some(d.quote.ok_or_else(corrupt)?),
    };
    out.extend(quote);
    match t >> 1 {
        K_TEXT => {
            let n = c.text.iter().position(|&b| b == 0).ok_or_else(corrupt)?;
            let s = &c.text[..n];
            c.text = &c.text[n + 1..];
            match quote {
                // (the quote doubled within)
                Some(q) => s.split(|&b| b == q).enumerate().for_each(|(i, p)| {
                    if i > 0 {
                        out.extend_from_slice(&[q, q]);
                    }
                    out.extend_from_slice(p);
                }),
                None => out.extend_from_slice(s),
            }
        }
        k @ (K_NUM | K_NEW) => {
            if k == K_NEW {
                let (mode, f) = (byte(&mut c.desc)?, byte(&mut c.desc)?);
                let s = aqr_common::varint::unzz(get(&mut c.desc)?);
                if !aqr_num::valid(mode, f) || !(aqr_num::MIN_SCALE as i64..=aqr_num::MAX_SCALE as i64).contains(&s) {
                    return Err(corrupt());
                }
                c.last = Some((mode, f, s as i32));
            }
            let (mode, f, s) = c.last.ok_or_else(corrupt)?;
            let r = aqr_common::varint::unzz(get(&mut c.nums)?);
            let v = if c.pred == P_DELTA { c.prev.wrapping_add(r) } else { r };
            c.prev = v;
            aqr_num::put(out, v, s, mode, f);
        }
        _ => return Err(corrupt()),
    }
    out.extend(quote);
    Ok(())
}
