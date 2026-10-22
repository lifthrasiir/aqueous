//! aqr-pdf as a module of entry formulas (see aqr-formula, FORMAT.md): the table, the
//! skeleton and the items' data as input slots, no parameters.
//!
//! The items that other recompressors code are not decoded by the module. Their results are
//! input slots, which the formula fills with the children's formulas: the DEFLATE streams of
//! `FULL` items (aqr-flate applied to a slice of a pool and a slice of the metadata), the
//! PNG files that wrap `PNG` items (aqr-png) and the `JPEG` items (aqr-jpeg). The
//! module only places their bytes, with the zlib header and Adler-32 around a DEFLATE stream.

use crate::encode::{self, Options, Stats};
use crate::kind::CLASSES;
use crate::varint::Reader;
use crate::{Error, Result, flate, image, tok};
use aqr_formula::{Apply, Formula, Module};

pub const MODULE: Module = Module { name: "pdf-recons", version: 1 };

/// The kinds of item in the table.
pub const RAW: u8 = 0;
pub const DUP: u8 = 1;
pub const FULL: u8 = 2;
pub const JPEG: u8 = 3;
pub const TOK: u8 = 4;
pub const PNG: u8 = 5;
pub const NMODES: usize = 6;
pub const MODES: [&str; NMODES] = ["raw", "dup", "full", "jpeg", "tok", "png"];

/// The formula giving `input`, or `None` where the formula would not be smaller than `input`
/// (or no item is coded). `threads` as in [`Options::threads`].
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// [`encode`] with options.
pub fn encode_with(input: &[u8], opts: &Options) -> Option<Formula> {
    encode_stats(input, opts, &mut Stats::default())
}

/// [`encode_with`], and what the encoder did (as of the last attempt, if a panic or a failed
/// verification made it code the file again).
pub fn encode_stats(input: &[u8], opts: &Options, st: &mut Stats) -> Option<Formula> {
    let attempt = |o: &Options| {
        std::panic::catch_unwind(|| {
            let mut s = Stats::default();
            let f = encode::try_formula(input, o, &mut s);
            (f, s)
        })
        .ok()
    };
    let (mut res, mut s) = attempt(opts)?;
    if opts.verify
        && let Some(f) = &res
        && f.eval(&|a: &Apply, i: &[&[u8]]| apply(a, i, opts.threads)).as_deref() != Ok(input)
    {
        let o = Options { tok: false, full: false, png: false, ..opts.clone() };
        (res, s) = attempt(&o)?;
    }
    *st = s;
    res
}

/// Runs the modules a formula of this module has in it (this one, aqr-flate, aqr-png and
/// aqr-jpeg).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_flate::formula::MODULE {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        aqr_flate::formula::decode(&a.params, &resources, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_png::formula::MODULE {
        aqr_png::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_jpeg::formula::MODULE {
        aqr_jpeg::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else {
        Err(aqr_formula::Error::new(format!("no module {}", a.module)))
    }
}

/// Where the items' data is next in each slot.
#[derive(Default)]
struct Cursors {
    sk: usize,
    rw: usize,
    deflate: usize,
    png: usize,
    jpeg: usize,
}

/// The slots of the module.
struct Slots<'a> {
    skel: &'a [u8],
    raw: &'a [u8],
    deflate: &'a [u8],
    png: &'a [u8],
    jpeg: &'a [u8],
}

/// The module: the PDF file of `len` bytes from the eleven `inputs` (FORMAT.md): the table,
/// the skeleton, the raw bytes, the DEFLATE streams, the PNG files, the JPEG files and the five
/// parts of the token coder. The result does not depend on `threads`.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[table, skel, raw, deflate, png, jpeg, t0, t1, t2, t3, t4]) = (params, inputs) else {
        return Err(Error::Corrupt);
    };
    let len = usize::try_from(len).map_err(|_| Error::Corrupt)?;
    let toks = [t0, t1, t2, t3, t4];
    let tokens = if toks.iter().all(|t| t.is_empty()) { None } else { Some(tok::DecoderState::new(toks)?) };
    let slots = Slots { skel, raw, deflate, png, jpeg };
    let mut o = Out::new(len, skel, tokens);
    let (mut t, mut cur) = (Reader::new(table), Cursors::default());
    let mut index = 0;
    while !t.at_end() {
        let e = entry(&mut t, &slots, &mut cur, len, index)?;
        o.add(&e)?;
        index += 1;
    }
    let used = [(raw.len(), cur.rw), (deflate.len(), cur.deflate), (png.len(), cur.png), (jpeg.len(), cur.jpeg)];
    if used.iter().any(|(n, at)| n != at) {
        return Err(Error::Corrupt);
    }
    o.finish(cur.sk)
}

/// The next item of the table, the `index`th; `len` is the file's length.
fn entry<'a>(t: &mut Reader<'a>, s: &Slots<'a>, cur: &mut Cursors, len: usize, index: usize) -> Result<Entry<'a>> {
    let gap = t.usize(len)?;
    let s0 = cur.sk;
    take(s.skel, &mut cur.sk, gap)?;
    let mode = t.u8()?;
    let op = match mode {
        RAW => {
            let n = t.usize(len)?;
            Op::Raw(take(s.raw, &mut cur.rw, n)?)
        }
        DUP => {
            let back = t.usize(index)?;
            if back == 0 {
                return Err(Error::Corrupt);
            }
            Op::Dup(back)
        }
        TOK => {
            let (hdr, c, n, px, adler) = flate_fields(t, len)?;
            Op::Tok(hdr, c, n, px, adler)
        }
        FULL => {
            let flags = t.u8()?;
            if flags & !3 != 0 {
                return Err(Error::Corrupt);
            }
            let hdr = if flags & 1 != 0 { t.bytes(2)? } else { &[] };
            let n = t.usize(len)?;
            let adler = if flags & 2 != 0 { t.bytes(4)? } else { &[] };
            Op::Zlib(hdr, take(s.deflate, &mut cur.deflate, n)?, adler)
        }
        PNG => {
            let n = t.usize(s.png.len())?;
            let file = take(s.png, &mut cur.png, n)?;
            Op::Bytes(image::unwrap(file).ok_or(Error::Corrupt)?)
        }
        JPEG => {
            let n = t.usize(s.jpeg.len())?;
            Op::Bytes(take(s.jpeg, &mut cur.jpeg, n)?)
        }
        _ => return Err(Error::Corrupt),
    };
    Ok(Entry { gap: s0..cur.sk, op })
}

/// An item of the table, with its data in the slots.
struct Entry<'a> {
    /// The skeleton before it.
    gap: std::ops::Range<usize>,
    op: Op<'a>,
}

enum Op<'a> {
    Raw(&'a [u8]),
    /// Items back.
    Dup(usize),
    /// A FlateDecode stream in the token coder: zlib header, class, inflated length, pixel
    /// layout, and whether the Adler-32 follows.
    Tok(&'a [u8], usize, usize, Option<tok::Pixels>, bool),
    /// A FlateDecode stream: the zlib header, the DEFLATE data and the Adler-32 (each perhaps
    /// empty).
    Zlib(&'a [u8], &'a [u8], &'a [u8]),
    /// A PNG or JPEG item: its bytes.
    Bytes(&'a [u8]),
}

/// The next `n` bytes of `v` from `at`.
fn take<'a>(v: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = at.checked_add(n).filter(|&e| e <= v.len()).ok_or(Error::Corrupt)?;
    let r = &v[*at..end];
    *at = end;
    Ok(r)
}

/// What [`flate_fields`] reads.
type FlateFields<'a> = (&'a [u8], usize, usize, Option<tok::Pixels>, bool);

/// The table fields of a `TOK` item after its kind: the zlib header, the class, the inflated
/// length, the pixel layout and whether the Adler-32 follows the DEFLATE data. `len` is the
/// file's length.
fn flate_fields<'a>(t: &mut Reader<'a>, len: usize) -> Result<FlateFields<'a>> {
    let flags = t.u8()?;
    if flags & !7 != 0 {
        return Err(Error::Corrupt);
    }
    let hdr = if flags & 1 != 0 { t.bytes(2)? } else { &[] };
    let c = t.u8()? as usize;
    if c >= CLASSES.len() {
        return Err(Error::Corrupt);
    }
    let n = t.usize(len.saturating_mul(1032))?;
    let px = if flags & 4 != 0 { Some(tok::Pixels { bpp: t.usize(64)?, stride: t.usize(1 << 34)? }) } else { None };
    Ok((hdr, c, n, px, flags & 2 != 0))
}

/// The output so far, and what restoring the items in order needs.
struct Out<'a> {
    out: Vec<u8>,
    skel: &'a [u8],
    tokens: Option<tok::DecoderState<'a>>,
    /// Where each item went in the output.
    placed: Vec<(usize, usize)>,
    len: usize,
}

impl<'a> Out<'a> {
    fn new(len: usize, skel: &'a [u8], tokens: Option<tok::DecoderState<'a>>) -> Self {
        Out { out: Vec::with_capacity(len), skel, tokens, placed: vec![], len }
    }

    /// The output once all items are in: the rest of the skeleton, from `sk`, follows. It must
    /// be the original's length, and the token decoder (if any) at its end.
    fn finish(mut self, sk: usize) -> Result<Vec<u8>> {
        self.out.extend_from_slice(&self.skel[sk..]);
        if self.tokens.is_some_and(|d| !d.finished()) || self.out.len() != self.len {
            return Err(Error::Corrupt);
        }
        Ok(self.out)
    }

    /// Appends item `e`.
    fn add(&mut self, e: &Entry) -> Result<()> {
        let out = &mut self.out;
        out.extend_from_slice(&self.skel[e.gap.clone()]);
        let start = out.len();
        match e.op {
            Op::Raw(d) | Op::Bytes(d) => out.extend_from_slice(d),
            Op::Dup(back) => {
                let (s, n) = self.placed[self.placed.len() - back];
                out.extend_from_within(s..s + n);
            }
            Op::Tok(hdr, c, n, px, adler) => {
                out.extend_from_slice(hdr);
                let dec = self.tokens.as_mut().ok_or(Error::Corrupt)?;
                let (d, plain) = dec.next(CLASSES[c], n, px)?;
                out.extend_from_slice(&d);
                if adler {
                    out.extend_from_slice(&flate::adler32(&plain).to_be_bytes());
                }
            }
            Op::Zlib(hdr, d, adler) => {
                out.extend_from_slice(hdr);
                out.extend_from_slice(d);
                out.extend_from_slice(adler);
            }
        }
        self.placed.push((start, out.len() - start));
        if out.len() > self.len {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}
