//! A file split into its skeleton (the file without its images' data) and a record of the
//! images (`FORMAT.md` §4): where they are, their pixels (by [`model`]), the codes given the
//! pixels, and how the codes are cut into sub-blocks.
//!
//! The codes are coded against what an encoder of the longest match would write: where it
//! clears, whether a code is the longest match there, else whether a single pixel, else how
//! much shorter; and whether it is the first code defined for its pixels.

use aqr_common::arith::{ArithDecoder, Bit};

use crate::gif::{self, table_len};
use crate::lzw::{self, BitWriter, CODES, Dict};
use crate::model::{CATS, Cat, Coder, Dec, Enc, Model, Place};
use crate::{Error, Result, corrupt};

/// Pixels of a file at most.
pub const MAX_PIXELS: u64 = 1 << 28;

/// An image's data beyond its pixels' values.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Data {
    /// The pixels, in the order of the stream.
    pub pixels: Vec<u8>,
    pub codes: Vec<u16>,
    /// The bits above the last code's in its byte.
    pub pad: u8,
    /// The bytes after that one.
    pub after: Vec<u8>,
    /// The sizes of the whole sub-blocks.
    pub sizes: Vec<u8>,
    /// Where the chain is cut by the end of the file: the size of the sub-block cut, and how
    /// many of its bytes there are (0 and 0 where it is cut after a whole one).
    pub cut: Option<(u8, u8)>,
}

/// An image as the skeleton tells it.
#[derive(Clone, Debug)]
struct Shape {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    interlaced: bool,
    min: u8,
    /// Where its data goes in the skeleton.
    at: usize,
    /// Where its colors are in the skeleton.
    palette: std::ops::Range<usize>,
}

/// The image whose descriptor is at `at` of the skeleton `sk`.
fn shape(sk: &[u8], at: usize) -> Result<Shape> {
    let d = sk.get(at..at + 10).filter(|d| d[0] == 0x2C).ok_or_else(corrupt)?;
    let end = at + 10 + table_len(d[9]) + 1;
    let min = *sk.get(end - 1).ok_or_else(corrupt)?;
    Ok(Shape {
        left: gif::le16(d, 1) as usize,
        top: gif::le16(d, 3) as usize,
        width: gif::le16(d, 5) as usize,
        height: gif::le16(d, 7) as usize,
        interlaced: d[9] & 0x40 != 0,
        min,
        at: end,
        palette: if d[9] & 0x80 != 0 {
            at + 10..end - 1
        } else if let Some(&g) = sk.get(10).filter(|&&g| g & 0x80 != 0) {
            13..(13 + table_len(g)).min(sk.len())
        } else {
            0..0
        },
    })
}

/// The order of an image's pixels in rows (from the stream's): the rows of an interlaced image
/// as they are shown, where it has as many pixels as its rows hold or more.
fn raster_rows(s: &Shape, n: usize) -> Option<Vec<usize>> {
    (s.interlaced && s.width > 0 && n >= s.width * s.height).then(|| gif::interlaced_rows(s.height).collect())
}

/// `px` (in the stream's order) in rows, or back where `back`.
fn reorder(px: &[u8], s: &Shape, back: bool) -> Vec<u8> {
    let Some(rows) = raster_rows(s, px.len()) else { return px.to_vec() };
    let w = s.width;
    let mut out = px.to_vec();
    for (i, &r) in rows.iter().enumerate() {
        let (from, to) = if back { (r, i) } else { (i, r) };
        out[to * w..to * w + w].copy_from_slice(&px[from * w..from * w + w]);
    }
    out
}

/// The encoder's dictionary: the code of each pair of a code and a pixel, the first one
/// defined for its pixels. A code defined again for the pixels of another (the decoder's
/// dictionary has such where an encoder's differs) stands for that one.
struct Trie {
    keys: Vec<u32>,
    codes: Vec<u16>,
    /// The first code of the same pixels, of each.
    canon: Vec<u16>,
    /// The keys of this generation are those of the dictionary now.
    generation: u32,
    gens: Vec<u32>,
}

const TRIE_BITS: u32 = 14;
/// Bytes of the encoder's dictionary.
pub const TRIE_BYTES: u64 = (10 << TRIE_BITS) as u64 + 2 * CODES as u64;

impl Trie {
    fn new() -> Self {
        let n = 1 << TRIE_BITS;
        let canon = (0..CODES as u16).collect();
        Trie { keys: vec![0; n], codes: vec![0; n], canon, generation: 1, gens: vec![0; n] }
    }

    fn clear(&mut self) {
        self.generation += 1;
    }

    #[inline]
    fn slot(key: u32) -> usize {
        (key.wrapping_mul(0x9e37_79b1) >> (32 - TRIE_BITS)) as usize
    }

    #[inline]
    fn get(&self, code: u16, px: u8) -> Option<u16> {
        let key = (code as u32) << 8 | px as u32;
        let mut s = Self::slot(key);
        loop {
            if self.gens[s] != self.generation {
                return None;
            }
            if self.keys[s] == key {
                return Some(self.codes[s]);
            }
            s = (s + 1) & ((1 << TRIE_BITS) - 1);
        }
    }

    /// Defines `to` as `code` and `px`.
    #[inline]
    fn insert(&mut self, code: u16, px: u8, to: u16) {
        let key = (self.canon[code as usize] as u32) << 8 | px as u32;
        let mut s = Self::slot(key);
        loop {
            if self.gens[s] != self.generation {
                self.gens[s] = self.generation;
                self.keys[s] = key;
                self.codes[s] = to;
                self.canon[to as usize] = to;
                return;
            }
            if self.keys[s] == key {
                self.canon[to as usize] = self.codes[s];
                return;
            }
            s = (s + 1) & ((1 << TRIE_BITS) - 1);
        }
    }
}

/// The contexts of the codes.
struct Ctx {
    clear: [Bit; 16],
    end: Bit,
    longest: [Bit; 3 * 4],
    single: [Bit; 3],
    shorter: [Bit; 16],
    first: Bit,
    pad: Bit,
    after: Bit,
    after_len: [Bit; 32],
    size: [Bit; 4],
    size_v: [Bit; 16],
    count: [Bit; 32],
    pixels: [Bit; 32],
    whole: Bit,
    whole_chain: Bit,
}

impl Ctx {
    fn new() -> Self {
        let b = Bit::default();
        Ctx {
            clear: [b; 16],
            end: b,
            longest: [b; 12],
            single: [b; 3],
            shorter: [b; 16],
            first: b,
            pad: b,
            after: b,
            after_len: [b; 32],
            size: [b; 4],
            size_v: [b; 16],
            count: [b; 32],
            pixels: [b; 32],
            whole: b,
            whole_chain: b,
        }
    }
}

/// Clear codes in a row at most.
const CLEARS: u32 = 64;

/// Codes the codes `codes` (given, or filled in) that give `px` with minimum code size `min`.
fn walk(
    c: &mut impl Coder,
    x: &mut Ctx,
    trie: &mut Trie,
    px: &[u8],
    min: u8,
    codes: &mut Vec<u16>,
    encoding: bool,
) -> Result<()> {
    let want = std::mem::take(codes);
    let mut dict = Dict::new(min);
    trie.clear();
    let clear = dict.clear_code() as u16;
    let n = px.len();
    let (mut p, mut k) = (0, 0);
    let mut prev: Option<u16> = None;
    let mut started = false;
    let mut clears = 0;
    // where the last clear code was: the next code then
    let mut last_clear = 0;
    // the last code: 0 the longest match, 1 a single pixel, 2 else
    let mut was = 0;
    let mut path = Vec::with_capacity(64);
    loop {
        let w = want.get(k).copied();
        let ctx = !started as usize
            | ((dict.next >= CODES) as usize) << 1
            | ((dict.next == last_clear) as usize) << 2
            | ((p == n) as usize) << 3;
        if c.bit(&mut x.clear[ctx], w == Some(clear), Cat::Parse) {
            if prev.is_some() {
                last_clear = dict.next;
            }
            clears += 1;
            if clears > CLEARS {
                return Err(corrupt());
            }
            dict.clear();
            trie.clear();
            prev = None;
            started = true;
            codes.push(clear);
            k += 1;
            continue;
        }
        started = true;
        clears = 0;
        if p == n {
            if c.bit(&mut x.end, w == Some(clear + 1), Cat::Parse) {
                codes.push(clear + 1);
                k += 1;
            }
            break;
        }
        if let Some(pr) = prev
            && dict.next < CODES
        {
            let to = dict.next as u16;
            dict.add(pr, px[p]);
            trie.insert(pr, px[p], to);
        }
        // the longest match, and the codes of its prefixes
        path.clear();
        let mut g = px[p] as u16;
        if g >= clear {
            return Err(corrupt());
        }
        path.push(g);
        while p + path.len() < n {
            match trie.get(g, px[p + path.len()]) {
                Some(next) => {
                    g = next;
                    path.push(g);
                }
                None => break,
            }
        }
        let longest = path.len();
        let want_len = w.map_or(0, |w| dict.len[w as usize] as usize);
        let len = if longest == 1 {
            1
        } else if c.bit(&mut x.longest[was * 4 + lb(longest)], want_len == longest, Cat::Parse) {
            longest
        } else if longest == 2 || c.bit(&mut x.single[was], want_len == 1, Cat::Parse) {
            1
        } else {
            let v = c.gamma(&mut x.shorter, (longest - 1).wrapping_sub(want_len) as u32, Cat::Parse);
            let v = v.ok_or_else(corrupt)? as usize;
            (longest - 1).checked_sub(v).filter(|&l| l >= 2).ok_or_else(corrupt)?
        };
        let first = path[len - 1];
        let code = if c.bit(&mut x.first, w.is_none_or(|w| w == first), Cat::Parse) {
            first
        } else {
            let code = c.raw(dict.width.min(12), w.unwrap_or(0) as u32, Cat::Parse) as u16;
            // one of the same pixels, defined
            if code as usize >= dict.next || code == clear || code == clear + 1 || !same(&dict, code, &px[p..p + len]) {
                return Err(corrupt());
            }
            code
        };
        if encoding && w != Some(code) {
            return Err(Error("codes not coded"));
        }
        was = if len == longest {
            0
        } else if len == 1 {
            1
        } else {
            2
        };
        codes.push(code);
        k += 1;
        p += len;
        prev = Some(code);
    }
    if encoding && k != want.len() {
        return Err(Error("codes beyond the pixels"));
    }
    Ok(())
}

/// Whether `code` gives `px`.
fn same(dict: &Dict, code: u16, px: &[u8]) -> bool {
    if dict.len[code as usize] as usize != px.len() {
        return false;
    }
    let mut c = code as usize;
    for &v in px.iter().rev() {
        if dict.last[c] != v {
            return false;
        }
        c = dict.prefix[c] as usize;
    }
    true
}

/// Bucket of a length: 1, 2, 3 to 7, 8 and more.
#[inline]
fn lb(l: usize) -> usize {
    match l {
        0..=1 => 0,
        2 => 1,
        3..=7 => 2,
        _ => 3,
    }
}

/// The codes written as a stream: its bytes, and the bits of the last.
pub fn write(codes: &[u16], min: u8) -> Result<BitWriter> {
    let mut dict = Dict::new(min);
    let clear = dict.clear_code() as u16;
    let mut w = BitWriter::default();
    let mut prev: Option<u16> = None;
    for &c in codes {
        w.put(c as u32, dict.width);
        if c == clear {
            dict.clear();
            prev = None;
        } else if c == clear + 1 {
            break;
        } else {
            if let Some(p) = prev {
                let f = if (c as usize) < dict.next { dict.first[c as usize] } else { dict.first[p as usize] };
                if c as usize > dict.next {
                    return Err(corrupt());
                }
                dict.add(p, f);
            }
            prev = Some(c);
        }
    }
    Ok(w)
}

/// Codes what an image's codes do not tell: the padding of their last byte, the bytes after
/// it, and the sizes of the sub-blocks.
fn code_rest(c: &mut impl Coder, x: &mut Ctx, bits: usize, d: &mut Data, encoding: bool) -> Result<()> {
    let pad_bits = ((8 - bits % 8) % 8) as u32;
    if pad_bits > 0 && !c.bit(&mut x.pad, d.pad == 0, Cat::Layout) {
        d.pad = c.raw(pad_bits, d.pad as u32, Cat::Layout) as u8;
        if !encoding && d.pad == 0 {
            return Err(corrupt());
        }
    } else if !encoding {
        d.pad = 0;
    }
    if !c.bit(&mut x.after, d.after.is_empty(), Cat::Layout) {
        let n =
            1 + c.gamma(&mut x.after_len, (d.after.len() as u32).wrapping_sub(1), Cat::Layout).ok_or_else(corrupt)?;
        if n > 1 << 24 {
            return Err(corrupt());
        }
        d.after.resize(n as usize, 0);
        for b in d.after.iter_mut() {
            *b = c.raw(8, *b as u32, Cat::Layout) as u8;
        }
    }
    // a chain cut by the end of the file: the size of the sub-block cut, and its bytes there
    if !c.bit(&mut x.whole_chain, d.cut.is_none(), Cat::Layout) {
        let (s, n) = d.cut.unwrap_or_default();
        let s = c.raw(8, s as u32, Cat::Layout) as u8;
        let n = if s > 0 { c.raw(8, n as u32, Cat::Layout) as u8 } else { 0 };
        if n >= s.max(1) {
            return Err(corrupt());
        }
        d.cut = Some((s, n));
    }
    // the sub-blocks: of the size of the one before (at first 255), or what is left
    let mut left =
        (bits.div_ceil(8) + d.after.len()).checked_sub(d.cut.map_or(0, |c| c.1 as usize)).ok_or_else(corrupt)?;
    let want = std::mem::take(&mut d.sizes);
    let mut last = 255;
    while left > 0 {
        let usual = last.min(left);
        let s = want.get(d.sizes.len()).map_or(0, |&s| s as usize);
        let ctx = d.sizes.is_empty() as usize | ((usual < last) as usize) << 1;
        let s = if c.bit(&mut x.size[ctx], s == usual, Cat::Layout) {
            usual
        } else {
            let s = 1 + c.gamma(&mut x.size_v, (s as u32).wrapping_sub(1), Cat::Layout).ok_or_else(corrupt)? as usize;
            if s > 255 || s > left || s == usual {
                return Err(corrupt());
            }
            s
        };
        d.sizes.push(s as u8);
        left -= s;
        last = s;
    }
    if encoding && d.sizes != want {
        return Err(Error("sub-blocks not coded"));
    }
    Ok(())
}

/// Codes where the images of the skeleton `sk` are (given, or filled in), and gives them.
fn code_layout(c: &mut impl Coder, x: &mut Ctx, sk: &[u8], at: &mut Vec<usize>, encoding: bool) -> Result<Vec<Shape>> {
    let n = c.gamma(&mut x.count, at.len() as u32, Cat::Layout).ok_or_else(corrupt)? as usize;
    if n > sk.len() / 11 {
        return Err(corrupt());
    }
    let mut shapes = Vec::with_capacity(n);
    let mut from = 0;
    for i in 0..n {
        let d = c.gamma(&mut x.count, at.get(i).map_or(0, |&a| (a - from) as u32), Cat::Layout).ok_or_else(corrupt)?;
        let s = shape(sk, from + d as usize)?;
        if !encoding {
            at.push(from + d as usize);
        }
        from = s.at;
        shapes.push(s);
    }
    Ok(shapes)
}

/// The size of the logical screen.
fn screen(sk: &[u8]) -> (usize, usize) {
    if sk.len() >= 13 { (gif::le16(sk, 6) as usize, gif::le16(sk, 8) as usize) } else { (0, 0) }
}

/// The pixels of the images of a record as their descriptors tell, for the module's memory.
pub struct Layout {
    pub area: u64,
    /// Of the largest image.
    pub largest: u64,
    pub screen: (usize, usize),
}

/// The [`Layout`] of the images that `record` codes in the skeleton `sk`.
pub fn layout(sk: &[u8], record: &[u8]) -> Result<Layout> {
    let mut dec = Dec { c: ArithDecoder::new(record) };
    let shapes = code_layout(&mut dec, &mut Ctx::new(), sk, &mut vec![], false)?;
    let area = |s: &Shape| (s.width * s.height) as u64;
    Ok(Layout {
        area: shapes.iter().map(area).sum(),
        largest: shapes.iter().map(area).max().unwrap_or(0),
        screen: screen(sk),
    })
}

/// Codes the record of the images of the skeleton `sk` (given, or filled in): where they are,
/// then each one's pixels, codes and the rest. Decoding (where `out` is given), the file is
/// written to `out` as each image is decoded, up to the place of the last one, which it gives.
fn code(
    c: &mut impl Coder,
    sk: &[u8],
    at: &mut Vec<usize>,
    data: &mut [Data],
    mut out: Option<&mut Vec<u8>>,
) -> Result<usize> {
    let encoding = out.is_none();
    let mut x = Ctx::new();
    let shapes = code_layout(c, &mut x, sk, at, encoding)?;
    let area: u64 = shapes.iter().map(|s| (s.width * s.height) as u64).sum();
    let screen = screen(sk);
    let mut model = Model::new(area.min(MAX_PIXELS), screen);
    let mut trie = Trie::new();
    let mut total = 0u64;
    let mut from = 0;
    for (i, s) in shapes.iter().enumerate() {
        if !(2..=8).contains(&s.min) {
            return Err(corrupt());
        }
        let mut d = if encoding { data[i].clone() } else { Data::default() };
        let area = (s.width * s.height) as u32;
        let np = if c.bit(&mut x.whole, d.pixels.len() == area as usize, Cat::Layout) {
            area
        } else {
            c.gamma(&mut x.pixels, d.pixels.len() as u32, Cat::Layout).ok_or_else(corrupt)?
        };
        total += np as u64;
        if total > MAX_PIXELS {
            return Err(Error("pixels beyond the limit"));
        }
        let mut raster = if encoding { reorder(&d.pixels, s, false) } else { vec![0; np as usize] };
        let place = Place { palette: &sk[s.palette.clone()], left: s.left, top: s.top, width: s.width, min: s.min };
        model.image(c, &mut raster, &place);
        if !encoding {
            d.pixels = reorder(&raster, s, true);
        }
        walk(c, &mut x, &mut trie, &d.pixels, s.min, &mut d.codes, encoding)?;
        let bits = write(&d.codes, s.min)?.bits();
        code_rest(c, &mut x, bits, &mut d, encoding)?;
        if let Some(out) = out.as_deref_mut() {
            out.extend_from_slice(&sk[from..s.at]);
            put_chain(&d, s.min, out)?;
            from = s.at;
        }
    }
    Ok(from)
}

/// Writes the chain of the image of `d`, of minimum code size `min`.
fn put_chain(d: &Data, min: u8, out: &mut Vec<u8>) -> Result<()> {
    let mut bytes = write(&d.codes, min)?.finish(d.pad);
    bytes.extend_from_slice(&d.after);
    let mut p = 0;
    for &n in &d.sizes {
        out.push(n);
        out.extend_from_slice(bytes.get(p..p + n as usize).ok_or_else(corrupt)?);
        p += n as usize;
    }
    match d.cut {
        None => out.push(0),
        Some((0, _)) => {}
        Some((s, _)) => {
            out.push(s);
            out.extend_from_slice(&bytes[p..]);
        }
    }
    Ok(())
}

/// A file split.
pub struct Split {
    pub skeleton: Vec<u8>,
    pub record: Vec<u8>,
    /// Images coded, of all.
    pub coded: usize,
    pub images: usize,
    /// Bits of the record by [`Cat`].
    pub cost: [f64; CATS],
}

/// An image's data taken apart, where its codes decode.
fn take(d: &[u8], im: &gif::Image, limit: usize) -> Option<Data> {
    let u = gif::unchain(&d[im.data.clone()]);
    let dec = lzw::decode(&u.bytes, im.min, limit).ok()?;
    let end = dec.end_bit.div_ceil(8);
    let pad = if dec.end_bit % 8 != 0 { u.bytes[dec.end_bit / 8] >> (dec.end_bit % 8) } else { 0 };
    let whole: usize = u.sizes.iter().map(|&s| s as usize).sum();
    let cut = u.cut.map(|s| (s, (u.bytes.len() - whole) as u8));
    Some(Data { pixels: dec.pixels, codes: dec.codes, pad, after: u.bytes[end..].to_vec(), sizes: u.sizes, cut })
}

/// Splits the GIF file `d` into its skeleton and record; None where it holds no image whose
/// data decodes.
pub fn split(d: &[u8]) -> Result<Split> {
    let g = gif::parse(d).ok_or(Error("not a GIF file"))?;
    let mut sk = Vec::with_capacity(d.len() / 8);
    let (mut at, mut data) = (vec![], vec![]);
    let mut from = 0;
    let mut left = MAX_PIXELS as usize;
    for im in &g.images {
        let Some(x) = take(d, im, left) else { continue };
        // (an image that decodes to nothing, its codes written as a clear and an end code alone or
        // less, is not worth it)
        if x.pixels.is_empty() {
            continue;
        }
        left -= x.pixels.len();
        sk.extend_from_slice(&d[from..im.at]);
        at.push(sk.len());
        sk.extend_from_slice(&d[im.at..im.data.start]);
        from = im.data.end;
        data.push(x);
    }
    if data.is_empty() {
        return Err(Error("no image coded"));
    }
    sk.extend_from_slice(&d[from..]);
    let mut e = Enc::new();
    let coded = data.len();
    code(&mut e, &sk, &mut at, &mut data, None)?;
    Ok(Split { skeleton: sk, record: e.c.finish(), coded, images: g.images.len(), cost: e.cost })
}

/// The file that `sk` and `record` rebuild, appended to `out`.
pub fn join_onto(sk: &[u8], record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut dec = Dec { c: ArithDecoder::new(record) };
    let from = code(&mut dec, sk, &mut vec![], &mut [], Some(out))?;
    if dec.c.overrun() {
        return Err(corrupt());
    }
    out.extend_from_slice(&sk[from..]);
    Ok(())
}
