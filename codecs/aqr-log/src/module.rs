//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module: the lines rebuilt from their templates and the
//! columns of their fields.

use aqr_common::varint;

use crate::{Result, corrupt};

/// The most templates a segment has (a line of another is kept as it is).
pub const MAX_TEMPLATES: usize = 1 << 16;
/// The most columns (fields of templates) a segment has (a line of a template that would have
/// more is kept as it is).
pub const MAX_COLUMNS: usize = 1 << 18;
/// The most bytes of the templates of a segment.
pub const MAX_TEMPLATE_BYTES: usize = 1 << 26;
/// The most digits of a digit run.
pub const MAX_DIGITS: usize = 18;

/// The marks of a template: a digit run, a word kept as it is (until the end of its line's
/// token), and a whole number (of aqr-num). A line holding any of the bytes is kept as it is.
pub const M_DIGITS: u8 = 1;
pub const M_TEXT: u8 = 2;
pub const M_NUM: u8 = 3;

/// A column's numbers as they are, as each the change from the column's previous one, or as
/// each the change from the same slot of the line before.
pub const P_RAW: u8 = 0;
pub const P_COLUMN: u8 = 1;
pub const P_LINE: u8 = 2;

/// A number's type (a byte of `types`): of the descriptor of the column's previous number, or
/// of a descriptor given (`desc`).
pub const K_SAME: u8 = 1;
pub const K_NEW: u8 = 2;

/// Bytes of the scratch buffer a number is printed in: more than any descriptor prints.
const SCRATCH: usize = 256;

/// What the params of the module hold: `flags` (bit 0: the last line has no LF), the numbers of
/// templates and columns of the streams, and the most numbers (digit runs and whole numbers) a
/// template has. The decoder takes them only to lay its memory out and checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Params {
    pub no_final_lf: bool,
    pub templates: usize,
    pub columns: usize,
    pub numbers: usize,
}

impl Params {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut p = vec![self.no_final_lf as u8];
        for n in [self.templates, self.columns, self.numbers] {
            varint::put(&mut p, n as u64);
        }
        p
    }

    pub fn from_bytes(p: &[u8]) -> Result<Params> {
        let (&flags, rest) = p.split_first().ok_or_else(corrupt)?;
        let mut at = 0;
        let mut next = |max: usize| -> Result<usize> {
            let v = varint::get(rest, &mut at).map_err(|_| corrupt())?;
            usize::try_from(v).ok().filter(|&v| v <= max).ok_or_else(corrupt)
        };
        let (templates, columns) = (next(MAX_TEMPLATES)?, next(MAX_COLUMNS)?);
        let numbers = next(MAX_COLUMNS)?;
        if flags > 1 || at != rest.len() || numbers > columns {
            return Err(corrupt());
        }
        Ok(Params { no_final_lf: flags == 1, templates, columns, numbers })
    }
}

/// A template: its bytes in the stream `templates`, and its first column.
#[repr(C)]
struct Template {
    start: u32,
    len: u32,
    col0: u32,
}

/// Bytes of a [`Template`] and of a [`Column`] on WebAssembly (32-bit pointers), for the plan.
const TEMPLATE_BYTES: u64 = 12;
const COLUMN_BYTES: u64 = 56;
const _: () = assert!(size_of::<Template>() as u64 == TEMPLATE_BYTES);

/// A column's parts of the streams, read from the front, and its state.
#[repr(C)]
struct Column<'a> {
    text: &'a [u8],
    nums: &'a [u8],
    widths: &'a [u8],
    types: &'a [u8],
    desc: &'a [u8],
    /// The integer of the column's previous number.
    prev: i64,
    /// The descriptor (scale, mode, flags) of the column's previous whole number, if `has`.
    scale: i32,
    mode: u8,
    flags: u8,
    has: bool,
    pred: u8,
}

#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<Column>() as u64 == COLUMN_BYTES);

/// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial memory
/// at most, of `params`, inputs of `inputs` bytes and an output of `len`: what `aqr_api::whole`
/// copies, the templates and columns, the numbers of two lines, the buffer a number is printed
/// in and the file. `None` for params it does not take.
pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
    use aqr_api::stack::{block, whole};
    let p = Params::from_bytes(params).ok()?;
    Some(
        whole(params.len() as u64, &[], inputs)
            + block(TEMPLATE_BYTES * p.templates as u64)
            + block(COLUMN_BYTES * p.columns as u64)
            + 2 * block(8 * p.numbers as u64)
            + block(SCRATCH as u64)
            + block(len),
    )
}

/// The module: the file of `len` bytes from `params` and `inputs` (the streams of
/// `formula::STREAM_NAMES`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[layout, raw, templates, columns, text, nums, widths, types, desc] = inputs else { return Err(corrupt()) };
    let p = Params::from_bytes(params)?;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let parts = Parts { columns, text, nums, widths, types, desc };
    build(&p, layout, raw, templates, parts, len)
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

/// The streams the columns' parts are taken from.
struct Parts<'a> {
    columns: &'a [u8],
    text: &'a [u8],
    nums: &'a [u8],
    widths: &'a [u8],
    types: &'a [u8],
    desc: &'a [u8],
}

/// The output: up to `cap` bytes.
struct Out {
    v: Vec<u8>,
    cap: usize,
}

impl Out {
    #[inline]
    fn put(&mut self, b: &[u8]) -> Result<()> {
        if b.len() > self.cap - self.v.len() {
            return Err(corrupt());
        }
        self.v.extend_from_slice(b);
        Ok(())
    }
}

/// The ten to the power [`MAX_DIGITS`]: the digit runs are below it.
const LIMIT: i64 = 1_000_000_000_000_000_000;

fn build(p: &Params, mut layout: &[u8], mut raw: &[u8], templates: &[u8], mut s: Parts, len: usize) -> Result<Vec<u8>> {
    // the templates and their columns
    let mut tpls: Vec<Template> = Vec::with_capacity(p.templates);
    let mut cols: Vec<Column> = Vec::with_capacity(p.columns);
    let mut most = 0;
    let mut at = 0;
    while at < templates.len() {
        let n = templates[at..].iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
        if tpls.len() == p.templates || templates.len() > MAX_TEMPLATE_BYTES {
            return Err(corrupt());
        }
        let t = &templates[at..at + n];
        tpls.push(Template { start: at as u32, len: n as u32, col0: cols.len() as u32 });
        let mut numbers = 0;
        for &m in t {
            if !matches!(m, M_DIGITS | M_TEXT | M_NUM) {
                continue;
            }
            if cols.len() == p.columns {
                return Err(corrupt());
            }
            // (a word's column: the length of its text; a number's: its predictor and the lengths
            // of its integers, and of its descriptors if a whole number; one width and one type
            // for each integer)
            let mut c = Column {
                text: &[],
                nums: &[],
                widths: &[],
                types: &[],
                desc: &[],
                prev: 0,
                scale: 0,
                mode: 0,
                flags: 0,
                has: false,
                pred: P_RAW,
            };
            if m == M_TEXT {
                c.text = take(&mut s.text, get(&mut s.columns)?)?;
            } else {
                c.pred = byte(&mut s.columns)?;
                if c.pred > P_LINE {
                    return Err(corrupt());
                }
                c.nums = take(&mut s.nums, get(&mut s.columns)?)?;
                // (each integer is a varint: the last byte ends one)
                if c.nums.last().is_some_and(|&b| b >= 0x80) {
                    return Err(corrupt());
                }
                let n = c.nums.iter().filter(|&&b| b < 0x80).count() as u64;
                match m {
                    M_DIGITS => c.widths = take(&mut s.widths, n)?,
                    _ => {
                        c.types = take(&mut s.types, n)?;
                        c.desc = take(&mut s.desc, get(&mut s.columns)?)?;
                    }
                }
            }
            numbers += (m != M_TEXT) as usize;
            cols.push(c);
        }
        most = most.max(numbers);
        at += n + 1;
    }
    if tpls.len() != p.templates || cols.len() != p.columns || most != p.numbers {
        return Err(corrupt());
    }
    if !(s.columns.is_empty()
        && s.text.is_empty()
        && s.nums.is_empty()
        && s.widths.is_empty()
        && s.types.is_empty()
        && s.desc.is_empty())
    {
        // (the parts of the columns, in order, are all of the streams)
        return Err(corrupt());
    }
    // (taken in the order of the plan)
    let mut prev: Vec<i64> = Vec::with_capacity(p.numbers);
    let mut cur: Vec<i64> = Vec::with_capacity(p.numbers);
    let mut scratch: Vec<u8> = Vec::with_capacity(SCRATCH);
    let mut out = Out { v: Vec::with_capacity(len), cap: len };
    let mut lines = 0usize;
    while !layout.is_empty() {
        let id = get(&mut layout)?;
        lines += 1;
        if id == 0 {
            let n = raw.iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
            out.put(&raw[..n])?;
            raw = &raw[n + 1..];
        } else {
            let t = usize::try_from(id - 1).ok().and_then(|i| tpls.get(i)).ok_or_else(corrupt)?;
            let tb = &templates[t.start as usize..][..t.len as usize];
            let mut col = t.col0 as usize;
            cur.clear();
            let mut from = 0;
            while let Some(i) = tb[from..].iter().position(|&b| matches!(b, M_DIGITS | M_TEXT | M_NUM)) {
                out.put(&tb[from..from + i])?;
                let c = &mut cols[col];
                col += 1;
                match tb[from + i] {
                    M_TEXT => {
                        let n = c.text.iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
                        out.put(&c.text[..n])?;
                        c.text = &c.text[n + 1..];
                    }
                    m => {
                        let r = varint::unzz(get(&mut c.nums)?);
                        let v = match c.pred {
                            P_RAW => r,
                            P_COLUMN => c.prev.wrapping_add(r),
                            _ => prev.get(cur.len()).copied().unwrap_or(0).wrapping_add(r),
                        };
                        c.prev = v;
                        cur.push(v);
                        if m == M_DIGITS {
                            digits(&mut out, v, byte(&mut c.widths)?)?;
                        } else {
                            number(&mut out, &mut scratch, c, v)?;
                        }
                    }
                }
                from += i + 1;
            }
            out.put(&tb[from..])?;
            std::mem::swap(&mut prev, &mut cur);
        }
        if !layout.is_empty() || !p.no_final_lf {
            out.put(b"\n")?;
        }
    }
    let left = cols.iter().any(|c| ![c.text, c.nums, c.widths, c.types, c.desc].iter().all(|x| x.is_empty()));
    if left || !raw.is_empty() || out.v.len() != len || (p.no_final_lf && lines == 0) {
        return Err(corrupt());
    }
    Ok(out.v)
}

/// Appends digit run `v` (below ten to the 18th) padded with zeros to `width` digits (0: not).
fn digits(out: &mut Out, v: i64, width: u8) -> Result<()> {
    let mut buf = [b'0'; MAX_DIGITS];
    let mut i = MAX_DIGITS;
    let mut a = u64::try_from(v).ok().filter(|_| v < LIMIT).ok_or_else(corrupt)?;
    loop {
        i -= 1;
        buf[i] = b'0' + (a % 10) as u8;
        a /= 10;
        if a == 0 {
            break;
        }
    }
    let w = width as usize;
    if w != 0 && !(MAX_DIGITS - i < w && w <= MAX_DIGITS) {
        return Err(corrupt());
    }
    out.put(&buf[if w == 0 { i } else { MAX_DIGITS - w }..])
}

/// Appends the whole number `v` of column `c`: its descriptor from `types` (and `desc`).
fn number(out: &mut Out, scratch: &mut Vec<u8>, c: &mut Column, v: i64) -> Result<()> {
    match byte(&mut c.types)? {
        K_SAME if c.has => {}
        K_NEW => {
            let (mode, f) = (byte(&mut c.desc)?, byte(&mut c.desc)?);
            let s = varint::unzz(get(&mut c.desc)?);
            if !aqr_num::valid(mode, f) || !(aqr_num::MIN_SCALE as i64..=aqr_num::MAX_SCALE as i64).contains(&s) {
                return Err(corrupt());
            }
            (c.mode, c.flags, c.scale, c.has) = (mode, f, s as i32, true);
        }
        _ => return Err(corrupt()),
    }
    scratch.clear();
    aqr_num::put(scratch, v, c.scale, c.mode, c.flags);
    out.put(scratch)
}
