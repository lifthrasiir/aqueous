//! aqr-pdf as a module of entry formulas (see aqr-formula, FORMAT.md "Formula module"):
//! the container's sections as input slots, no parameters.
//!
//! The items that other recompressors code are not decoded by the module. Their results are
//! input slots, which the formula fills with the children's formulas: the DEFLATE streams of
//! `FULL` items (aqr-flate applied to a slice of a pool and a slice of the metadata), the
//! PNG files that wrap `PNG` items (aqr-png) and the `JPEG` items (aqr-jpeg). The
//! module only places their bytes, with the zlib header and Adler-32 around a DEFLATE stream.

use crate::container::{self, Entry, Op, Options, Out, Stats, flate_fields, take};
use crate::container::{DUP, FULL, JPEG, PNG, RAW, TOK};
use crate::varint::Reader;
use crate::{Error, Result, image, tok};
use aqr_formula::{Apply, Formula, Module};

pub const MODULE: Module = Module { name: "pdf-recons", version: 1 };

/// The formula giving `input`, or `None` where the container would store it (or the formula
/// would not be smaller than `input`). `threads` as in [`Options::threads`].
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// [`encode`] with the container's options.
pub fn encode_with(input: &[u8], opts: &Options) -> Option<Formula> {
    let attempt = |o: &Options| {
        std::panic::catch_unwind(|| container::try_formula(input, o, &mut Stats::default())).ok().flatten()
    };
    let mut res = attempt(opts);
    if opts.verify
        && let Some(f) = &res
        && f.eval(&|a: &Apply, i: &[&[u8]]| apply(a, i, opts.threads)).as_deref() != Ok(input)
    {
        res = attempt(&Options { tok: false, full: false, png: false, ..opts.clone() });
    }
    res
}

/// Runs the modules a formula of this module has in it.
fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_flate::formula::MODULE {
        aqr_flate::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
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
    let mut ds = container::DStats::default();
    let (mut t, mut cur) = (Reader::new(table), Cursors::default());
    let mut index = 0;
    while !t.at_end() {
        let t_item = std::time::Instant::now();
        let e = entry(&mut t, &slots, &mut cur, len, index)?;
        o.add(&e, None, &mut ds, t_item, 0.0)?;
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
    let (mut ci, mut plain_n) = (6, 0);
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
            let (hdr, c, n, px, adler) = flate_fields(t, true, len)?;
            (ci, plain_n) = (if px.is_some() { 6 } else { c }, n);
            Op::Flate(hdr, c, n, px, adler, None)
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
            ci = crate::kind::Class::Image as usize;
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
    Ok(Entry { gap: s0..cur.sk, mode, ci, plain_n, op })
}
