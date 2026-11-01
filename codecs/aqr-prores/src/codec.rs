//! A chunk of frames coded as a whole. A chunk is a span of data: its frames, and the bytes
//! between them, which go apart. Its frames are coded in order, each modelled ([`crate::model`])
//! with the one before as context, or kept as it is where it does not parse.
//!
//! The coded chunk is streams: the first holds what is not coefficients (the bytes between
//! frames' lengths, the frames kept, the headers as differences from the previous ones, the
//! slices' headers, tails and alpha); each of the others the coefficients of a stripe of rows
//! of slices of every picture, with a model of its own, a stripe for about [`STRIPE_BYTES`] of
//! frames. So the stripes decode at
//! once, each writing its slices' bytes, and the frames are put together from them.
//! `FORMAT.md` specifies it.

use std::ops::Range;

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::msb::{BitReader, BitWriter, padded};

use crate::coef::Tail;
use crate::frame::{self, Frame, Header, Slice};
use crate::model::{Coder, Model};
use crate::plane::{self, Plane};
use crate::{Error, Result};

/// Bytes of frames a stripe codes, about: a chunk has `⌈its frames' bytes / STRIPE_BYTES⌉`
/// stripes, at most [`MAX_STRIPES`]. (Each stripe's model learns anew: an animated film lost
/// 0.6% at 8 stripes for 56 MB, 2.3% at 32; camera footage gained a little.)
pub const STRIPE_BYTES: usize = 4 << 20;
pub const MAX_STRIPES: usize = 64;

/// The slice rows of stripe `s` of `k` of a picture of `rows` of them: `rows * s / k` up to
/// `rows * (s + 1) / k`.
fn stripe_rows(rows: usize, s: usize, k: usize) -> Range<usize> {
    rows * s / k..rows * (s + 1) / k
}

struct Enc {
    a: ArithEncoder,
    raw: BitWriter,
}

impl Enc {
    fn new() -> Self {
        Enc { a: ArithEncoder::new(Vec::new()), raw: BitWriter::new() }
    }
}

impl Coder for Enc {
    const DECODE: bool = false;

    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, b: bool) -> bool {
        self.a.encode(b, p.p1());
        p.update(b);
        b
    }

    #[inline(always)]
    fn raw(&mut self, v: u32, n: u32) -> u32 {
        self.raw.put(v, n);
        v
    }
}

struct Dec<'a> {
    a: ArithDecoder<'a>,
    raw: BitReader<'a>,
    raw_bits: usize,
}

impl<'a> Dec<'a> {
    /// A stream's decoder; `raw` is its raw bits padded.
    fn new(arith: &'a [u8], raw: &'a [u8]) -> Self {
        Dec {
            a: ArithDecoder::new(arith),
            raw: BitReader::new(raw, 0),
            raw_bits: (raw.len() - aqr_common::msb::PAD) * 8,
        }
    }

    /// Whether it read past either of its ends.
    fn overrun(&self) -> bool {
        self.raw.pos > self.raw_bits || self.a.overrun()
    }
}

impl Coder for Dec<'_> {
    const DECODE: bool = true;

    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, _: bool) -> bool {
        let b = self.a.decode(p.p1());
        p.update(b);
        b
    }

    #[inline(always)]
    fn raw(&mut self, _: u32, n: u32) -> u32 {
        self.raw.get(n)
    }
}

/// The adaptive bits of the headers.
#[derive(Default)]
struct Side {
    modelled: Bit,
    same_header: Bit,
    same_pic: Bit,
    same_byte0: Bit,
    same_q: Bit,
    tail: Bit,
    trailer_zero: Bit,
}

/// What the next frame's headers are coded against.
#[derive(Default)]
struct SideState {
    bits: Side,
    header: Vec<u8>,
    pics: Vec<Vec<u8>>,
}

/// A stripe's model, and its planes of the frame before, by picture (none after a frame kept
/// as it is).
#[derive(Default)]
struct StripeState {
    model: Model,
    prev: Option<Vec<[Plane; 3]>>,
}

fn bytes<C: Coder>(c: &mut C, d: &mut Vec<u8>, n: usize) {
    if C::DECODE {
        d.clear();
        d.extend((0..n).map(|_| c.raw(0, 8) as u8));
    } else {
        d.iter().for_each(|&b| {
            c.raw(b as u32, 8);
        });
    }
}

/// What decoding a chunk holds at most, which its application gives (`FORMAT.md`), and
/// decoding holds to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limits {
    /// Blocks of a frame (of all its pictures and components).
    pub blocks: u64,
    /// Bytes of a frame.
    pub frame: u64,
    /// What the first stream tells, by [`side_cost`]: the frames' headers, slices and the frames
    /// kept, as decoding holds them for the stripes.
    pub side: u64,
    /// Blocks of a stripe of a picture.
    pub stripe_blocks: u64,
    /// Bytes each stripe writes (its slices, and their lengths), one for each stripe.
    pub results: Vec<u64>,
}

/// Bytes counted for what the first stream tells ([`Limits::side`]): of an item, a frame kept, a
/// frame's header and trailer, a picture and a slice (its header and alpha).
const SIDE_ITEM: u64 = 64;
const SIDE_PICTURE: u64 = 64;
const SIDE_SLICE: u64 = 256;

/// The blocks of `rows` slice rows of a picture `mbw` macroblocks wide.
fn picture_blocks(h: &Header, mbw: usize, rows: usize) -> u64 {
    (rows * mbw * (4 + if h.chroma == 3 { 8 } else { 4 })) as u64
}

/// A chunk coded ([`encode`]).
pub struct Coded {
    pub coded: Vec<u8>,
    /// The bytes of the span that are not in its frames, in order.
    pub gaps: Vec<u8>,
    pub limits: Limits,
}

/// Encodes `span`, whose frames (each from its size field on) are at `frames` (in order, apart),
/// as a chunk: the frames coded, the bytes between them given back as they are.
pub fn encode(span: &[u8], frames: &[Range<usize>]) -> Coded {
    let frame_bytes: usize = frames.iter().map(|r| r.len()).sum();
    encode_with(span, frames, frame_bytes.div_ceil(STRIPE_BYTES).clamp(1, MAX_STRIPES))
}

/// [`encode`] in `k` stripes.
fn encode_with(span: &[u8], frames: &[Range<usize>], k: usize) -> Coded {
    let mut side = Enc::new();
    let mut st = SideState::default();
    let mut stripes: Vec<(Enc, StripeState)> = (0..k).map(|_| (Enc::new(), StripeState::default())).collect();
    let (mut gaps, mut limits) = (Vec::new(), Limits { results: vec![0; k], ..Limits::default() });
    side.raw(frames.len() as u32, 32);
    let mut at = 0;
    for r in frames {
        debug_assert!(r.start >= at);
        side.raw((r.start - at) as u32, 32);
        gaps.extend_from_slice(&span[at..r.start]);
        at = r.end;
        let d = &span[r.clone()];
        limits.frame = limits.frame.max(d.len() as u64);
        // A frame that parses is written again bit for bit (codewords, signs and positions have
        // one coding each, padding is zero and recorded, sizes are derived and were checked
        // against the coded data), so it is not written to check.
        match Frame::parse(d) {
            Ok(f) => {
                let blocks = f.pictures.iter().flat_map(|p| &p.slices).flat_map(|s| &s.comps).map(|c| c.0.len() / 64);
                limits.blocks = limits.blocks.max(blocks.sum::<usize>() as u64);
                limits.side += SIDE_ITEM + (f.header.raw.len() + f.trailer.len()) as u64;
                let mbw = f.header.width.div_ceil(16);
                for pic in &f.pictures {
                    let nw = pic.widths(&f.header).len();
                    let rows = pic.slices.len() / nw;
                    limits.side += SIDE_PICTURE;
                    for s in &pic.slices {
                        limits.side += SIDE_SLICE + (s.header.len() + s.alpha.len()) as u64;
                    }
                    for (s, r) in limits.results.iter_mut().enumerate() {
                        let rs = stripe_rows(rows, s, k);
                        limits.stripe_blocks = limits.stripe_blocks.max(picture_blocks(&f.header, mbw, rs.len()));
                        for s in &pic.slices[rs.start * nw..rs.end * nw] {
                            *r += 4 + (s.header.len() + s.sizes.iter().sum::<usize>() + s.alpha.len()) as u64;
                        }
                    }
                }
                side.bit(&mut st.bits.modelled, true);
                side_enc(&mut side, &mut st, &f);
                for (s, (c, ss)) in stripes.iter_mut().enumerate() {
                    let mut planes = Vec::with_capacity(f.pictures.len());
                    for (pi, pic) in f.pictures.iter().enumerate() {
                        let ws = pic.widths(&f.header);
                        let rs = stripe_rows(pic.slices.len() / ws.len(), s, k);
                        let mut pl = plane::planes(&pic.slices[rs.start * ws.len()..rs.end * ws.len()], &f.header, &ws);
                        let prev = ss.prev.as_ref().and_then(|v| v.get(pi));
                        for (k, p) in pl.iter_mut().enumerate() {
                            ss.model.plane(c, p, prev.map(|x| &x[k]), (k > 0) as usize);
                        }
                        planes.push(pl);
                    }
                    ss.prev = Some(planes);
                }
            }
            Err(_) => {
                limits.side += SIDE_ITEM + d.len() as u64;
                side.bit(&mut st.bits.modelled, false);
                side.raw(d.len() as u32, 32);
                bytes(&mut side, &mut d.to_vec(), d.len());
                stripes.iter_mut().for_each(|(_, ss)| ss.prev = None);
            }
        }
    }
    side.raw((span.len() - at) as u32, 32);
    gaps.extend_from_slice(&span[at..]);
    let streams: Vec<(Vec<u8>, Vec<u8>)> =
        std::iter::once(side).chain(stripes.into_iter().map(|s| s.0)).map(|c| (c.a.finish(), c.raw.finish())).collect();
    let mut coded = Vec::new();
    for (a, r) in &streams {
        coded.extend_from_slice(&(a.len() as u32).to_le_bytes());
        coded.extend_from_slice(&(r.len() as u32).to_le_bytes());
    }
    for (a, r) in &streams {
        coded.extend_from_slice(a);
        coded.extend_from_slice(r);
    }
    Coded { coded, gaps, limits }
}

/// The header bytes of a slice that are neither its first two nor its sizes.
fn slice_extra(hs: usize) -> Range<usize> {
    (if hs >= 8 { 8 } else { 6 })..hs
}

/// Codes `f`'s headers, slices' headers, tails and alpha, and trailer.
fn side_enc(c: &mut Enc, st: &mut SideState, f: &Frame) {
    let same = f.header.raw == st.header;
    c.bit(&mut st.bits.same_header, same);
    if !same {
        c.raw(f.header.raw.len() as u32, 16);
        bytes(c, &mut f.header.raw.clone(), f.header.raw.len());
        st.header = f.header.raw.clone();
    }
    st.pics.resize(f.pictures.len(), Vec::new());
    for (pi, pic) in f.pictures.iter().enumerate() {
        // The picture header but its size.
        let mut ph = pic.header.clone();
        ph[1..5].fill(0);
        let same = ph == st.pics[pi];
        c.bit(&mut st.bits.same_pic, same);
        if !same {
            c.raw(ph.len() as u32, 8);
            bytes(c, &mut ph.clone(), ph.len());
            st.pics[pi] = ph;
        }
        let (mut b0, mut q) = (0u8, 0u8);
        for s in &pic.slices {
            let h = &s.header;
            if !c.bit(&mut st.bits.same_byte0, h[0] == b0) {
                c.raw(h[0] as u32, 8);
            }
            if !c.bit(&mut st.bits.same_q, h[1] == q) {
                c.raw(h[1] as u32, 8);
            }
            (b0, q) = (h[0], h[1]);
            for &x in &h[slice_extra(h.len())] {
                c.raw(x as u32, 8);
            }
            if f.header.alpha != 0 {
                c.raw(s.alpha.len() as u32, 16);
                bytes(c, &mut s.alpha.clone(), s.alpha.len());
            }
            for (_, t) in &s.comps {
                if c.bit(&mut st.bits.tail, *t != Tail::default()) {
                    c.raw(t.empty as u32, 1);
                    c.raw(t.zero_bytes, 16);
                }
            }
        }
    }
    c.raw(f.trailer.len() as u32, 32);
    if !f.trailer.is_empty() && !c.bit(&mut st.bits.trailer_zero, f.trailer.iter().all(|&b| b == 0)) {
        bytes(c, &mut f.trailer.clone(), f.trailer.len());
    }
}

/// A picture as the first stream tells it: its header (its size zeroed), the widths of its
/// slices along a row, its rows, and its slices with empty coefficients.
struct PicSide {
    header: Vec<u8>,
    widths: Vec<usize>,
    rows: usize,
    slices: Vec<Slice>,
}

/// A frame as the first stream tells it.
struct FrameSide {
    header: Header,
    pics: Vec<PicSide>,
    trailer: Vec<u8>,
}

/// What comes after a gap: a frame kept as it is, or one modelled.
enum Item {
    Kept(Vec<u8>),
    Modelled(FrameSide),
}

/// Decodes a chunk into its span of `len` bytes, with `gaps` the bytes between its frames, its
/// stripes on up to `threads` threads (tasks, in WebAssembly).
pub fn decode(coded: &[u8], gaps: &[u8], limits: &Limits, len: usize, threads: usize) -> Result<Vec<u8>> {
    let bad = || Error("corrupt chunk");
    let stripes = limits.results.len();
    if !(1..=MAX_STRIPES).contains(&stripes) {
        return Err(bad());
    }
    // The streams: their lengths, then them.
    let table = 8 * (1 + stripes);
    let mut at = coded.get(..table).ok_or(bad())?.len();
    let mut streams = Vec::with_capacity(1 + stripes);
    for k in 0..1 + stripes {
        let n = |o: usize| u32::from_le_bytes(coded[8 * k + o..8 * k + o + 4].try_into().unwrap()) as usize;
        let (a, r) = (n(0), n(4));
        let arith = coded.get(at..at.checked_add(a).ok_or(bad())?).ok_or(bad())?;
        at += a;
        let raw = coded.get(at..at.checked_add(r).ok_or(bad())?).ok_or(bad())?;
        at += r;
        streams.push((arith, padded(raw)));
    }
    if at != coded.len() {
        return Err(bad());
    }
    let (items, last) = side_dec(&streams[0], limits)?;
    let parts = aqr_common::pool::par_map_apart(stripes, threads, |s| {
        let (arith, raw) = &streams[1 + s];
        stripe_dec(s, &items, Dec::new(arith, raw), limits).unwrap_or_default()
    });
    // (an empty part where a stripe failed: it has a byte at least otherwise)
    if parts.iter().zip(&limits.results).any(|(p, n)| Some(p.len() as u64) != n.checked_add(1)) {
        return Err(bad());
    }
    let mut cursors: Vec<&[u8]> = parts.iter().map(|p| &p[1..]).collect();
    let mut out = Vec::with_capacity(len);
    let mut g = 0;
    let mut gap = |out: &mut Vec<u8>, n: usize| -> Result<()> {
        out.extend_from_slice(gaps.get(g..g + n).ok_or(bad())?);
        g += n;
        if out.len() > len { Err(bad()) } else { Ok(()) }
    };
    for (n, item) in &items {
        gap(&mut out, *n)?;
        match item {
            Item::Kept(d) => out.extend_from_slice(d),
            Item::Modelled(f) => {
                let base = out.len();
                out.extend_from_slice(&[0, 0, 0, 0]);
                out.extend_from_slice(b"icpf");
                out.extend_from_slice(&f.header.raw);
                for p in &f.pics {
                    let p0 = out.len();
                    out.extend_from_slice(&p.header);
                    let mut table = out.len();
                    out.resize(table + 2 * p.slices.len(), 0);
                    for (s, cur) in cursors.iter_mut().enumerate() {
                        let count = stripe_rows(p.rows, s, stripes).len() * p.widths.len();
                        let lens = cur.get(..4 * count).ok_or(bad())?;
                        let mut total = 0;
                        for l in lens.as_chunks::<4>().0 {
                            let l = u32::from_le_bytes(*l) as usize;
                            out[table..table + 2].copy_from_slice(&u16::try_from(l).map_err(|_| bad())?.to_be_bytes());
                            table += 2;
                            total += l;
                        }
                        out.extend_from_slice(cur.get(4 * count..4 * count + total).ok_or(bad())?);
                        *cur = &cur[4 * count + total..];
                    }
                    let size = u32::try_from(out.len() - p0).map_err(|_| bad())?;
                    out[p0 + 1..p0 + 5].copy_from_slice(&size.to_be_bytes());
                }
                out.extend_from_slice(&f.trailer);
                if (out.len() - base) as u64 > limits.frame {
                    return Err(bad());
                }
                let size = u32::try_from(out.len() - base).map_err(|_| bad())?;
                out[base..base + 4].copy_from_slice(&size.to_be_bytes());
            }
        }
        if out.len() > len {
            return Err(bad());
        }
    }
    gap(&mut out, last)?;
    if out.len() != len || g != gaps.len() || cursors.iter().any(|c| !c.is_empty()) {
        return Err(bad());
    }
    Ok(out)
}

/// Decodes the first stream: each frame after its gap, and the last gap.
fn side_dec(stream: &(&[u8], Vec<u8>), limits: &Limits) -> Result<(Vec<(usize, Item)>, usize)> {
    let bad = || Error("corrupt chunk");
    let mut c = Dec::new(stream.0, &stream.1);
    let mut st = SideState::default();
    let count = c.raw(0, 32) as usize;
    let mut items = Vec::with_capacity(count.min(1 << 16));
    let mut side = 0;
    for _ in 0..count {
        let gap = c.raw(0, 32) as usize;
        side += SIDE_ITEM;
        let item = if c.bit(&mut st.bits.modelled, false) {
            Item::Modelled(frame_side_dec(&mut c, &mut st, limits, &mut side)?)
        } else {
            let n = c.raw(0, 32) as usize;
            side += n as u64;
            if n as u64 > limits.frame || side > limits.side || n > c.raw_bits / 8 {
                return Err(bad());
            }
            let mut v = Vec::new();
            bytes(&mut c, &mut v, n);
            Item::Kept(v)
        };
        if c.overrun() {
            return Err(bad());
        }
        items.push((gap, item));
    }
    let last = c.raw(0, 32) as usize;
    if c.overrun() {
        return Err(bad());
    }
    Ok((items, last))
}

fn frame_side_dec(c: &mut Dec, st: &mut SideState, limits: &Limits, side: &mut u64) -> Result<FrameSide> {
    let bad = |s| Error(s);
    if !c.bit(&mut st.bits.same_header, false) {
        let n = c.raw(0, 16) as usize;
        bytes(c, &mut st.header, n);
    }
    let header = Header::new(st.header.clone())?;
    let npics = header.pictures();
    st.pics.resize(npics, Vec::new());
    let mbw = header.width.div_ceil(16);
    let mut pics = Vec::with_capacity(npics);
    let mut blocks = 0u64;
    // (the bytes of the frame so far, but its slices' coefficients)
    let mut size = 8 + header.raw.len() as u64;
    for pi in 0..npics {
        if !c.bit(&mut st.bits.same_pic, false) {
            let n = c.raw(0, 8) as usize;
            bytes(c, &mut st.pics[pi], n);
        }
        let ph = st.pics[pi].clone();
        if ph.len() < 8 || (ph[0] >> 3) as usize != ph.len() || ph[7] >> 4 > 3 {
            return Err(bad("bad picture header"));
        }
        let ws = frame::widths(&header, &ph);
        let n = u16::from_be_bytes([ph[5], ph[6]]) as usize;
        if n == 0 || !n.is_multiple_of(ws.len()) || n / ws.len() > 4096 || mbw > 8192 {
            return Err(bad("bad slice count"));
        }
        let rows = n / ws.len();
        blocks += picture_blocks(&header, mbw, rows);
        *side += SIDE_PICTURE;
        let k = limits.results.len();
        let most = (0..k).map(|s| picture_blocks(&header, mbw, stripe_rows(rows, s, k).len())).max().unwrap_or(0);
        if blocks > limits.blocks || most > limits.stripe_blocks || *side > limits.side {
            return Err(bad("frame past the chunk's limits"));
        }
        size += (ph.len() + 2 * n) as u64;
        let (mut b0, mut q) = (0u8, 0u8);
        let mut slices = Vec::with_capacity(n);
        for si in 0..n {
            if !c.bit(&mut st.bits.same_byte0, false) {
                b0 = c.raw(0, 8) as u8;
            }
            if !c.bit(&mut st.bits.same_q, false) {
                q = c.raw(0, 8) as u8;
            }
            let hs = (b0 >> 3) as usize;
            if hs < 6 {
                return Err(bad("bad slice header"));
            }
            let mut sh = vec![0u8; hs];
            (sh[0], sh[1]) = (b0, q);
            for i in slice_extra(hs) {
                sh[i] = c.raw(0, 8) as u8;
            }
            let mut alpha = Vec::new();
            if header.alpha != 0 {
                let n = c.raw(0, 16) as usize;
                bytes(c, &mut alpha, n);
            }
            size += (hs + alpha.len()) as u64;
            *side += SIDE_SLICE + (hs + alpha.len()) as u64;
            let mbs = ws[si % ws.len()];
            let mut tails = [Tail::default(); 3];
            for t in &mut tails {
                if c.bit(&mut st.bits.tail, false) {
                    t.empty = c.raw(0, 1) != 0;
                    t.zero_bytes = c.raw(0, 16);
                    size += t.zero_bytes as u64;
                }
            }
            // (the coefficients are allocated by the stripes)
            let comps = tails.map(|t| (Vec::new(), t));
            slices.push(Slice { header: sh, mbs, comps, alpha, sizes: [0; 3] });
            if size > limits.frame || *side > limits.side || c.overrun() {
                return Err(bad("corrupt chunk"));
            }
        }
        let mut header = ph;
        header[1..5].fill(0);
        pics.push(PicSide { header, widths: ws, rows, slices });
    }
    let n = c.raw(0, 32) as usize;
    *side += (header.raw.len() + n) as u64;
    if size + n as u64 > limits.frame || *side > limits.side {
        return Err(bad("corrupt chunk"));
    }
    let trailer = if n > 0 && c.bit(&mut st.bits.trailer_zero, false) {
        vec![0; n]
    } else {
        let mut v = Vec::new();
        bytes(c, &mut v, n);
        v
    };
    Ok(FrameSide { header, pics, trailer })
}

/// Decodes stripe `s` of every frame of `items` and writes its slices: for each modelled frame
/// and picture, the lengths of its slices in the stripe (u32, little-endian), then their bytes;
/// after a byte 1. None where the stream does not decode.
fn stripe_dec(s: usize, items: &[(usize, Item)], mut c: Dec, limits: &Limits) -> Option<Vec<u8>> {
    let k = limits.results.len();
    let mut ss = StripeState::default();
    let mut out = vec![1u8];
    for (_, item) in items {
        let Item::Modelled(f) = item else {
            ss.prev = None;
            continue;
        };
        let mut planes = Vec::with_capacity(f.pics.len());
        for (pi, p) in f.pics.iter().enumerate() {
            let rs = stripe_rows(p.rows, s, k);
            let mut pl = plane::empty(&f.header, &p.widths, rs.len());
            let prev = ss.prev.as_ref().and_then(|v| v.get(pi));
            for (k, q) in pl.iter_mut().enumerate() {
                ss.model.plane(&mut c, q, prev.map(|x| &x[k]), (k > 0) as usize);
            }
            if c.overrun() {
                return None;
            }
            let nw = p.widths.len();
            let mut slices: Vec<Slice> = p.slices[rs.start * nw..rs.end * nw]
                .iter()
                .map(|sl| {
                    let cb = if f.header.chroma == 3 { 4 * sl.mbs } else { 2 * sl.mbs };
                    let blocks = [4 * sl.mbs, cb, cb];
                    let comps = std::array::from_fn(|k| (vec![0; 64 * blocks[k]], sl.comps[k].1));
                    Slice { header: sl.header.clone(), mbs: sl.mbs, comps, alpha: sl.alpha.clone(), sizes: [0; 3] }
                })
                .collect();
            plane::scatter(&pl, &mut slices, &f.header, &p.widths);
            let lens = out.len();
            out.resize(lens + 4 * slices.len(), 0);
            for (i, sl) in slices.iter().enumerate() {
                let n = sl.write_onto(&mut out) as u32;
                out[lens + 4 * i..lens + 4 * i + 4].copy_from_slice(&n.to_le_bytes());
            }
            if out.len() as u64 > limits.results[s].saturating_add(1) {
                return None;
            }
            planes.push(pl);
        }
        ss.prev = Some(planes);
    }
    (!c.overrun()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Picture;

    /// A frame of `w` x `h` (4:2:2 or 4:4:4) with sparse random coefficients from `seed`.
    fn synthetic(w: u16, h: u16, chroma: u8, interlace: u8, seed: u64) -> Vec<u8> {
        let mut rng = seed;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as u32
        };
        let mut raw = vec![0u8; 20];
        raw[..2].copy_from_slice(&20u16.to_be_bytes());
        raw[4..8].copy_from_slice(b"test");
        raw[8..10].copy_from_slice(&w.to_be_bytes());
        raw[10..12].copy_from_slice(&h.to_be_bytes());
        raw[12] = chroma << 6 | interlace << 2;
        let header = Header::new(raw).unwrap();
        let rows = (h as usize).div_ceil(16 * header.pictures());
        let pictures = (0..header.pictures())
            .map(|_| {
                let mut ph = vec![8 << 3, 0, 0, 0, 0, 0, 0, 1 << 4];
                let ws = frame::widths(&header, &ph);
                ph[5..7].copy_from_slice(&((ws.len() * rows) as u16).to_be_bytes());
                let slices = (0..ws.len() * rows)
                    .map(|i| {
                        let mbs = ws[i % ws.len()];
                        let comps = std::array::from_fn(|k| {
                            let blocks = if k == 0 || chroma == 3 { 4 * mbs } else { 2 * mbs };
                            let c = (0..blocks * 64)
                                .map(|j| match next() % 8 {
                                    _ if j % 64 == 0 => (next() % 4000) as i16 - 2000,
                                    0 => (next() % 600) as i16 - 300,
                                    1 => (next() % 6) as i16 - 3,
                                    _ => 0,
                                })
                                .collect();
                            (c, Tail { zero_bytes: (next() % 50 == 0) as u32, empty: false })
                        });
                        let q = 1 + (next() % 3) as u8;
                        Slice { header: vec![6 << 3, q, 0, 0, 0, 0], mbs, comps, alpha: Vec::new(), sizes: [0; 3] }
                    })
                    .collect();
                Picture { header: ph, slices }
            })
            .collect();
        Frame { header, pictures, trailer: vec![0; (next() % 3) as usize * 16] }.write()
    }

    #[test]
    fn chunks_round_trip() {
        let mut frames = Vec::new();
        for (i, (w, h, chroma, il)) in
            [(64, 32, 2, 0), (64, 32, 2, 0), (200, 72, 3, 0), (48, 64, 2, 1), (48, 64, 2, 1)].into_iter().enumerate()
        {
            let f = synthetic(w, h, chroma, il, i as u64);
            assert_eq!(Frame::parse(&f).unwrap().write(), f);
            frames.push(f);
        }
        // A frame kept as it is.
        let mut broken = frames[0].clone();
        broken[40] ^= 0x55;
        frames.push(broken);
        // A span of the frames with bytes between and around them.
        let (mut span, mut ranges) = (b"head".to_vec(), Vec::new());
        for (i, f) in frames.iter().enumerate() {
            span.extend(std::iter::repeat_n(i as u8, i * 3));
            ranges.push(span.len()..span.len() + f.len());
            span.extend_from_slice(f);
        }
        span.extend_from_slice(b"tail");
        for k in [1, 3, 8] {
            stripes_round_trip(&span, &ranges, &frames, k);
        }
    }

    fn stripes_round_trip(span: &[u8], ranges: &[std::ops::Range<usize>], frames: &[Vec<u8>], k: usize) {
        let e = encode_with(span, ranges, k);
        assert_eq!(e.limits.results.len(), k);
        assert_eq!(e.limits.frame, frames.iter().map(|f| f.len() as u64).max().unwrap());
        assert_eq!(decode(&e.coded, &e.gaps, &e.limits, span.len(), 2).unwrap(), span);
        let mut more = e.limits.clone();
        more.results.push(0);
        assert!(decode(&e.coded, &e.gaps, &more, span.len(), 2).is_err());
        assert!(decode(&e.coded[..e.coded.len() / 2], &e.gaps, &e.limits, span.len(), 2).is_err());
        let tight = Limits { blocks: e.limits.blocks - 1, ..e.limits.clone() };
        assert!(decode(&e.coded, &e.gaps, &tight, span.len(), 2).is_err());
        let tight = Limits { side: e.limits.side - 1, ..e.limits.clone() };
        assert!(decode(&e.coded, &e.gaps, &tight, span.len(), 2).is_err());
        let mut tight = e.limits.clone();
        *tight.results.iter_mut().find(|r| **r > 0).unwrap() -= 1;
        assert!(decode(&e.coded, &e.gaps, &tight, span.len(), 2).is_err());
    }
}
