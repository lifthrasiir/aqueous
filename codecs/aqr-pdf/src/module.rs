//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use aqr_common::cursor::take;
use aqr_common::packed;

use crate::crypt::Crypt;
use crate::kind::CLASSES;
use crate::varint::{Reader, Upto};
use crate::{Error, Result, image, tok};

/// The kinds of item in the table.
pub const RAW: u8 = 0;
pub const DUP: u8 = 1;
pub const FULL: u8 = 2;
pub const JPEG: u8 = 3;
pub const TOK: u8 = 4;
pub const PNG: u8 = 5;
/// Not a kind of item but a prefix of one: the item's bytes encrypted again.
pub const ENC: u8 = 6;
pub const NMODES: usize = 6;
pub const MODES: [&str; NMODES] = ["raw", "dup", "full", "jpeg", "tok", "png"];

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by assertions:
/// a block of a layout with its tokens or bytes, a token, a place in the output, a slice of an
/// item of a packed input, a word.
pub mod wasm {
    pub const BLOCK: u64 = 1008;
    pub const TOKEN: u64 = 6;
    pub const PLACED: u64 = 8;
    pub const ITEM: u64 = 8;
    pub const WORD: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<(aqr_flate::deflate::Block, usize)>() as u64 == wasm::BLOCK);
    assert!(size_of::<aqr_flate::deflate::Token>() as u64 == wasm::TOKEN);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PLACED);
    assert!(size_of::<&[u8]>() as u64 == wasm::ITEM);
    assert!(size_of::<usize>() as u64 == wasm::WORD);
};

/// Where the items' data is next in each slot: a byte, or an item of a packed one.
#[derive(Default)]
struct Cursors {
    sk: usize,
    rw: usize,
    deflate: usize,
    png: usize,
    jpeg: usize,
}

/// The slots of the module, the packed ones unpacked.
struct Slots<'a> {
    skel: &'a [u8],
    raw: &'a [u8],
    deflate: Vec<&'a [u8]>,
    png: Vec<&'a [u8]>,
    jpeg: Vec<&'a [u8]>,
}

/// The module: the PDF file of `len` bytes from the eleven `inputs` (FORMAT.md): the table,
/// the skeleton, the raw bytes, the DEFLATE streams, the PNG files, the JPEG files and the five
/// parts of the token coder; `params` is the cipher of an encrypted file or empty. The result
/// does not depend on `threads`.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[table, skel, raw, deflate, png, jpeg, t0, t1, t2, t3, t4] = inputs else {
        return Err(Error::Corrupt);
    };
    let crypt = Crypt::from_params(params)?;
    let len = usize::try_from(len).map_err(|_| Error::Corrupt)?;
    let toks = [t0, t1, t2, t3, t4];
    let tokens = if toks.iter().all(|t| t.is_empty()) { None } else { Some(tok::DecoderState::new(toks)?) };
    let unpack = |p| packed::unpack(p).ok_or(Error::Corrupt);
    let slots = Slots { skel, raw, deflate: unpack(deflate)?, png: unpack(png)?, jpeg: unpack(jpeg)? };
    // (the items counted first, so that the places they go to are reserved)
    let iv = iv_len(&crypt);
    let items = scan_table_with(table, len, iv, |_, _| ()).ok_or(Error::Corrupt)?;
    let mut o = Out::new(len, skel, tokens, items, crypt);
    let (mut t, mut cur) = (Reader::new(table), Cursors::default());
    let mut index = 0;
    while !t.at_end() {
        let e = entry(&mut t, &slots, &mut cur, len, index, iv)?;
        o.add(&e)?;
        index += 1;
    }
    let used = [
        (raw.len(), cur.rw),
        (slots.deflate.len(), cur.deflate),
        (slots.png.len(), cur.png),
        (slots.jpeg.len(), cur.jpeg),
    ];
    if used.iter().any(|(n, at)| n != at) {
        return Err(Error::Corrupt);
    }
    o.finish(cur.sk)
}

/// The most an item's inflated length is of its file's (DEFLATE's limit, 1032 bytes for a bit).
const MAX_EXPANSION: usize = 1032;

/// The most bytes per pixel of an item's layout.
const MAX_BPP: usize = 64;

/// The most bytes in a row of an item's layout.
const MAX_STRIDE: usize = if (1u64 << 34) < usize::MAX as u64 { 1 << 34 } else { usize::MAX };

/// The items of `table` (of a file of `len` bytes), calling `stream` for each token stream with
/// its inflated length and pixel layout (bytes per pixel and stride); None where the table is
/// no table. What the memory plan and the decoder both count by.
pub fn scan_table(table: &[u8], len: usize, stream: impl FnMut(usize, Option<(u64, u64)>)) -> Option<usize> {
    scan_table_with(table, len, None, stream)
}

/// Bytes of the IV that an encrypted item has in the table, for a file's cipher (if any).
pub fn iv_len(crypt: &Option<Crypt>) -> Option<usize> {
    Some(crypt.as_ref()?.iv_len())
}

/// [`scan_table`] of a file whose cipher's IVs are `iv` bytes (None: unencrypted, where an
/// encrypted item is invalid).
pub fn scan_table_with(
    table: &[u8],
    len: usize,
    iv: Option<usize>,
    mut stream: impl FnMut(usize, Option<(u64, u64)>),
) -> Option<usize> {
    let (mut t, mut items) = (Reader::new(table), 0usize);
    while !t.at_end() {
        t.upto(len).ok()?;
        let mut mode = t.u8().ok()?;
        if mode == ENC {
            t.upto(u32::MAX as usize).ok()?;
            t.upto(u16::MAX as usize).ok()?;
            t.bytes(iv?).ok()?;
            let tail = t.upto(len).ok()?;
            t.bytes(tail).ok()?;
            mode = t.u8().ok()?;
            if matches!(mode, DUP | ENC) {
                return None;
            }
        }
        match mode {
            RAW => drop(t.upto(len).ok()?),
            DUP => drop(t.upto(items).ok()?),
            TOK => {
                let flags = t.u8().ok()?;
                if flags & !7 != 0 {
                    return None;
                }
                if flags & 1 != 0 {
                    t.bytes(2).ok()?;
                }
                t.u8().ok().filter(|&c| (c as usize) < CLASSES.len())?;
                let n = t.upto(len.saturating_mul(MAX_EXPANSION)).ok()?;
                let px = if flags & 4 != 0 {
                    let bpp = t.upto(MAX_BPP).ok()? as u64;
                    Some((bpp, t.upto(MAX_STRIDE).ok()? as u64))
                } else {
                    None
                };
                stream(n, px);
            }
            FULL => {
                let flags = t.u8().ok()?;
                if flags & !3 != 0 {
                    return None;
                }
                t.bytes(2 * (flags & 1) as usize + 4 * (flags >> 1) as usize).ok()?;
            }
            JPEG | PNG => {}
            _ => return None,
        }
        items += 1;
    }
    Some(items)
}

/// The next item of the table, the `index`th; `len` is the file's length.
fn entry<'a>(
    t: &mut Reader<'a>,
    s: &Slots<'a>,
    cur: &mut Cursors,
    len: usize,
    index: usize,
    iv: Option<usize>,
) -> Result<Entry<'a>> {
    let gap = t.upto(len)?;
    let s0 = cur.sk;
    take(s.skel, &mut cur.sk, gap)?;
    let mut mode = t.u8()?;
    let mut enc = None;
    if mode == ENC {
        let num = t.upto(u32::MAX as usize)? as u32;
        let gen_ = t.upto(u16::MAX as usize)? as u16;
        let iv = t.bytes(iv.ok_or(Error::Corrupt)?)?;
        let n = t.upto(len)?;
        enc = Some(Sealed { num, gen_, iv, tail: t.bytes(n)? });
        mode = t.u8()?;
        if matches!(mode, DUP | ENC) {
            return Err(Error::Corrupt);
        }
    }
    let op = match mode {
        RAW => {
            let n = t.upto(len)?;
            Op::Raw(take(s.raw, &mut cur.rw, n)?)
        }
        DUP => {
            let back = t.upto(index)?;
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
            let adler = if flags & 2 != 0 { t.bytes(4)? } else { &[] };
            Op::Zlib(hdr, next(&s.deflate, &mut cur.deflate)?, adler)
        }
        PNG => Op::Bytes(image::unwrap(next(&s.png, &mut cur.png)?).ok_or(Error::Corrupt)?),
        JPEG => Op::Bytes(next(&s.jpeg, &mut cur.jpeg)?),
        _ => return Err(Error::Corrupt),
    };
    Ok(Entry { gap: s0..cur.sk, enc, op })
}

/// An item of the table, with its data in the slots.
struct Entry<'a> {
    /// The skeleton before it.
    gap: std::ops::Range<usize>,
    /// Where it is encrypted again, if it is.
    enc: Option<Sealed<'a>>,
    op: Op<'a>,
}

/// The object an encrypted item belongs to (its key), and the IV that starts its bytes (none
/// for RC4).
struct Sealed<'a> {
    num: u32,
    gen_: u16,
    iv: &'a [u8],
    /// Bytes that follow the item's before they are encrypted.
    tail: &'a [u8],
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

/// The item `at` of `items`, the next.
fn next<'a>(items: &[&'a [u8]], at: &mut usize) -> Result<&'a [u8]> {
    let item = *items.get(*at).ok_or(Error::Corrupt)?;
    *at += 1;
    Ok(item)
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
    let n = t.upto(len.saturating_mul(MAX_EXPANSION))?;
    let px =
        if flags & 4 != 0 { Some(tok::Pixels { bpp: t.upto(MAX_BPP)?, stride: t.upto(MAX_STRIDE)? }) } else { None };
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
    crypt: Option<Crypt>,
}

impl<'a> Out<'a> {
    fn new(
        len: usize,
        skel: &'a [u8],
        tokens: Option<tok::DecoderState<'a>>,
        items: usize,
        crypt: Option<Crypt>,
    ) -> Self {
        Out { out: Vec::with_capacity(len), skel, tokens, placed: Vec::with_capacity(items), len, crypt }
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
        // (an encrypted item: the IV first, then the bytes it encrypts)
        let body = start + e.enc.as_ref().map_or(0, |s| s.iv.len());
        if let Some(s) = &e.enc {
            out.extend_from_slice(s.iv);
        }
        match e.op {
            Op::Raw(d) | Op::Bytes(d) => out.extend_from_slice(d),
            Op::Dup(back) => {
                let (s, n) = self.placed[self.placed.len() - back];
                out.extend_from_within(s..s + n);
            }
            Op::Tok(hdr, c, n, px, adler) => {
                out.extend_from_slice(hdr);
                let dec = self.tokens.as_mut().ok_or(Error::Corrupt)?;
                let sum = dec.next(CLASSES[c], n, px, out)?;
                if adler {
                    out.extend_from_slice(&sum.to_be_bytes());
                }
            }
            Op::Zlib(hdr, d, adler) => {
                out.extend_from_slice(hdr);
                out.extend_from_slice(d);
                out.extend_from_slice(adler);
            }
        }
        if let Some(s) = &e.enc {
            let c = self.crypt.as_ref().ok_or(Error::Corrupt)?;
            if out.len() + s.tail.len() > self.len {
                return Err(Error::Corrupt);
            }
            out.extend_from_slice(s.tail);
            if c.iv_len() != 0 {
                // PKCS#5 padding: 1 to 16 bytes, each the count
                let pad = 16 - (out.len() - body) % 16;
                if out.len() + pad > self.len {
                    return Err(Error::Corrupt);
                }
                out.resize(out.len() + pad, pad as u8);
            }
            c.seal(s.num, s.gen_, s.iv, &mut out[body..])?;
        }
        self.placed.push((start, out.len() - start));
        if out.len() > self.len {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}
