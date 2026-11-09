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
use crate::frame::{self, Frame, Header};
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
    /// What the first stream tells (`encode_with` counts it): the frames' headers, slices and the frames
    /// kept, as decoding holds them for the stripes.
    pub side: u64,
    /// Blocks of a stripe of a picture.
    pub stripe_blocks: u64,
    /// Bytes each stripe writes (its slices, and their lengths), one for each stripe.
    pub results: Vec<u64>,
    /// What decoding the first stream makes ([`Counts`]).
    pub counts: Counts,
}

/// What the first stream tells, as decoding holds it: items, modelled frames, pictures, slices,
/// slices along a row of each picture, and the bytes kept (frames kept, the frames' and
/// pictures' headers where they change, the slices' headers and alpha, trailers not of zeros).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub items: u64,
    pub frames: u64,
    pub pics: u64,
    pub slices: u64,
    pub widths: u64,
    pub bytes: u64,
}

impl Counts {
    pub fn to_array(self) -> [u64; 6] {
        [self.items, self.frames, self.pics, self.slices, self.widths, self.bytes]
    }

    pub fn from_array([items, frames, pics, slices, widths, bytes]: [u64; 6]) -> Counts {
        Counts { items, frames, pics, slices, widths, bytes }
    }
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
    // (what decoding the first stream makes, as it makes it)
    let (a, r) = &streams[0];
    let (info, _) = side_dec(a, &padded(r), &limits, false).expect("the first stream decodes");
    limits.counts = info.counts();
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

/// A range of [`SideInfo::bytes`], or of one of its vectors.
#[derive(Debug, Clone, Copy, Default)]
struct Span {
    at: u32,
    len: u32,
}

impl Span {
    fn range(self) -> Range<usize> {
        self.at as usize..(self.at + self.len) as usize
    }
}

/// What comes after a gap: a frame kept as it is (its bytes), or one modelled (its frame).
#[derive(Debug, Clone, Copy)]
struct ItemRec {
    gap: u32,
    kept: bool,
    what: Span,
}

/// A modelled frame as the first stream tells it: its header's bytes and what decoding needs
/// of it, its first picture, and its trailer (`len` zeros where `zeros`).
#[derive(Debug, Clone, Copy)]
struct FrameRec {
    header: Span,
    width: u32,
    chroma: u8,
    pics: u8,
    zeros: bool,
    first: u32,
    trailer: Span,
}

/// A picture: its header (its size zeroed), the widths of its slices along a row, its rows of
/// slices, and its first slice.
#[derive(Debug, Clone, Copy)]
struct PicRec {
    header: Span,
    widths: Span,
    rows: u32,
    first: u32,
}

/// A slice: its header (its sizes zeroed), its alpha, its macroblocks and its tails.
#[derive(Debug, Clone, Copy)]
struct SliceRec {
    header: Span,
    alpha: Span,
    mbs: u32,
    tails: [Tail; 3],
}

/// Bytes of the records (the same on every target).
const ITEM_BYTES: u64 = 16;
const FRAME_BYTES: u64 = 28;
const PIC_BYTES: u64 = 24;
const SLICE_BYTES: u64 = 44;
const _: () = assert!(
    size_of::<ItemRec>() == ITEM_BYTES as usize
        && size_of::<FrameRec>() == FRAME_BYTES as usize
        && size_of::<PicRec>() == PIC_BYTES as usize
        && size_of::<SliceRec>() == SLICE_BYTES as usize
);

/// What the first stream tells, flat ([`Counts`]); of vectors reserved for the counts given,
/// none grown, or grown as they go where none are given (the encoder's count).
#[derive(Default)]
struct SideInfo {
    items: Vec<ItemRec>,
    frames: Vec<FrameRec>,
    pics: Vec<PicRec>,
    slices: Vec<SliceRec>,
    widths: Vec<u32>,
    bytes: Vec<u8>,
    bounded: bool,
}

impl SideInfo {
    fn with(c: &Counts) -> Result<SideInfo> {
        let n = |v: u64| usize::try_from(v).map_err(|_| Error("bad parameters"));
        Ok(SideInfo {
            items: Vec::with_capacity(n(c.items)?),
            frames: Vec::with_capacity(n(c.frames)?),
            pics: Vec::with_capacity(n(c.pics)?),
            slices: Vec::with_capacity(n(c.slices)?),
            widths: Vec::with_capacity(n(c.widths)?),
            bytes: Vec::with_capacity(n(c.bytes)?),
            bounded: true,
        })
    }

    /// Bytes of [`aqr_api::stack`] that [`SideInfo::with`] takes.
    fn plan(c: &Counts) -> u64 {
        use aqr_api::stack::block;
        block(ITEM_BYTES * c.items)
            + block(FRAME_BYTES * c.frames)
            + block(PIC_BYTES * c.pics)
            + block(SLICE_BYTES * c.slices)
            + block(4 * c.widths)
            + block(c.bytes)
    }

    fn counts(&self) -> Counts {
        let n = |v: usize| v as u64;
        Counts {
            items: n(self.items.len()),
            frames: n(self.frames.len()),
            pics: n(self.pics.len()),
            slices: n(self.slices.len()),
            widths: n(self.widths.len()),
            bytes: n(self.bytes.len()),
        }
    }

    /// Whether every vector was filled to the counts given.
    fn full(&self) -> bool {
        let f = |len: usize, cap: usize| len == cap;
        f(self.items.len(), self.items.capacity())
            && f(self.frames.len(), self.frames.capacity())
            && f(self.pics.len(), self.pics.capacity())
            && f(self.slices.len(), self.slices.capacity())
            && f(self.widths.len(), self.widths.capacity())
            && f(self.bytes.len(), self.bytes.capacity())
    }

    /// Pictures of a frame, at most.
    fn fields(&self) -> usize {
        self.frames.iter().map(|f| f.pics as usize).max().unwrap_or(1)
    }
}

/// Pushes `x` onto `v`, where it has room (or grows as it goes, where `!bounded`).
fn push<T>(v: &mut Vec<T>, x: T, bounded: bool) -> Result<u32> {
    if bounded && v.len() == v.capacity() {
        return Err(Error("corrupt chunk"));
    }
    v.push(x);
    u32::try_from(v.len() - 1).map_err(|_| Error("corrupt chunk"))
}

/// `n` bytes of `c`'s raw bits appended to `info`'s bytes.
fn take_bytes(c: &mut Dec, info: &mut SideInfo, n: usize) -> Result<Span> {
    if (info.bounded && info.bytes.capacity() - info.bytes.len() < n) || n > c.raw_bits / 8 {
        return Err(Error("corrupt chunk"));
    }
    let at = info.bytes.len();
    info.bytes.extend((0..n).map(|_| c.raw(0, 8) as u8));
    Ok(Span { at: at as u32, len: n as u32 })
}

/// Decodes a chunk into its span of `len` bytes, with `gaps` the bytes between its frames, its
/// stripes on up to `threads` threads (tasks, in WebAssembly).
pub fn decode(coded: &[u8], gaps: &[u8], limits: &Limits, len: usize, threads: usize) -> Result<Vec<u8>> {
    let bad = || Error("corrupt chunk");
    let stripes = limits.results.len();
    if !(1..=MAX_STRIPES).contains(&stripes) {
        return Err(bad());
    }
    let streams = streams(coded, stripes)?;
    let side_raw = padded(streams[0].1);
    let (info, last) = side_dec(streams[0].0, &side_raw, limits, true)?;
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(stripes);
    aqr_common::pool::par_each_apart(
        stripes,
        threads,
        2,
        |s| stripe_dec(s, &info, streams[1 + s], limits).unwrap_or_default(),
        || (),
        |s, part| {
            // (an empty part where a stripe failed: it has a byte at least otherwise)
            if Some(part.len() as u64) != limits.results[s].checked_add(1) {
                return Err(bad());
            }
            parts.push(part);
            Ok(())
        },
    )?;
    let mut cursors: Vec<&[u8]> = parts.iter().map(|p| &p[1..]).collect();
    let mut out = Vec::with_capacity(len);
    let mut g = 0;
    let mut gap = |out: &mut Vec<u8>, n: usize| -> Result<()> {
        out.extend_from_slice(gaps.get(g..g + n).ok_or(bad())?);
        g += n;
        if out.len() > len { Err(bad()) } else { Ok(()) }
    };
    let b = &info.bytes;
    for item in &info.items {
        gap(&mut out, item.gap as usize)?;
        if item.kept {
            out.extend_from_slice(&b[item.what.range()]);
        } else {
            let f = &info.frames[item.what.at as usize];
            let base = out.len();
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(b"icpf");
            out.extend_from_slice(&b[f.header.range()]);
            for p in &info.pics[f.first as usize..][..f.pics as usize] {
                let p0 = out.len();
                out.extend_from_slice(&b[p.header.range()]);
                let mut table = out.len();
                let n = p.rows as usize * p.widths.len as usize;
                if out.len() + 2 * n > len {
                    return Err(bad());
                }
                out.resize(table + 2 * n, 0);
                for (s, cur) in cursors.iter_mut().enumerate() {
                    let count = stripe_rows(p.rows as usize, s, stripes).len() * p.widths.len as usize;
                    let lens = cur.get(..4 * count).ok_or(bad())?;
                    let mut total = 0;
                    for l in lens.as_chunks::<4>().0 {
                        let l = u32::from_le_bytes(*l) as usize;
                        out[table..table + 2].copy_from_slice(&u16::try_from(l).map_err(|_| bad())?.to_be_bytes());
                        table += 2;
                        total += l;
                    }
                    let d = cur.get(4 * count..4 * count + total).ok_or(bad())?;
                    if out.len() + d.len() > len {
                        return Err(bad());
                    }
                    out.extend_from_slice(d);
                    *cur = &cur[4 * count + total..];
                }
                let size = u32::try_from(out.len() - p0).map_err(|_| bad())?;
                out[p0 + 1..p0 + 5].copy_from_slice(&size.to_be_bytes());
            }
            if out.len() + f.trailer.len as usize > len {
                return Err(bad());
            }
            match f.zeros {
                true => out.resize(out.len() + f.trailer.len as usize, 0),
                false => out.extend_from_slice(&b[f.trailer.range()]),
            }
            if (out.len() - base) as u64 > limits.frame {
                return Err(bad());
            }
            let size = u32::try_from(out.len() - base).map_err(|_| bad())?;
            out[base..base + 4].copy_from_slice(&size.to_be_bytes());
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

/// The streams of a coded chunk of `stripes` stripes: their lengths, then them (the arithmetic
/// coded bytes and the raw bits of each).
fn streams(coded: &[u8], stripes: usize) -> Result<Vec<(&[u8], &[u8])>> {
    let bad = || Error("corrupt chunk");
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
        streams.push((arith, raw));
    }
    if at != coded.len() {
        return Err(bad());
    }
    Ok(streams)
}

/// Decodes the first stream (`arith`, and `raw` its raw bits padded): each frame after its gap,
/// and the last gap; into vectors of [`Counts`] of `limits` where `bounded`.
fn side_dec(arith: &[u8], raw: &[u8], limits: &Limits, bounded: bool) -> Result<(SideInfo, usize)> {
    let bad = || Error("corrupt chunk");
    let mut info = if bounded { SideInfo::with(&limits.counts)? } else { SideInfo::default() };
    let mut c = Dec::new(arith, raw);
    let mut st = SideSpans::default();
    let count = c.raw(0, 32) as usize;
    let mut side = 0;
    for _ in 0..count {
        let gap = c.raw(0, 32);
        side += SIDE_ITEM;
        let item = if c.bit(&mut st.bits.modelled, false) {
            let f = frame_side_dec(&mut c, &mut st, &mut info, limits, &mut side)?;
            ItemRec { gap, kept: false, what: Span { at: push(&mut info.frames, f, bounded)?, len: 0 } }
        } else {
            let n = c.raw(0, 32) as usize;
            side += n as u64;
            if n as u64 > limits.frame || side > limits.side {
                return Err(bad());
            }
            ItemRec { gap, kept: true, what: take_bytes(&mut c, &mut info, n)? }
        };
        if c.overrun() {
            return Err(bad());
        }
        push(&mut info.items, item, bounded)?;
    }
    let last = c.raw(0, 32) as usize;
    if c.overrun() || (bounded && !info.full()) {
        return Err(bad());
    }
    Ok((info, last))
}

/// What the next frame's headers are coded against, as decoding keeps it: the spans of the
/// last frame header and picture headers.
#[derive(Default)]
struct SideSpans {
    bits: Side,
    header: Span,
    pics: [Span; 2],
}

fn frame_side_dec(
    c: &mut Dec,
    st: &mut SideSpans,
    info: &mut SideInfo,
    limits: &Limits,
    side: &mut u64,
) -> Result<FrameRec> {
    let bad = |s| Error(s);
    let bounded = info.bounded;
    if !c.bit(&mut st.bits.same_header, false) {
        let n = c.raw(0, 16) as usize;
        st.header = take_bytes(c, info, n)?;
    }
    let header = Header::fields(&info.bytes[st.header.range()])?;
    let npics = header.pictures();
    let mbw = header.width.div_ceil(16);
    let first = info.pics.len() as u32;
    let mut blocks = 0u64;
    // (the bytes of the frame so far, but its slices' coefficients)
    let mut size = 8 + st.header.len as u64;
    for pi in 0..npics {
        if !c.bit(&mut st.bits.same_pic, false) {
            let n = c.raw(0, 8) as usize;
            st.pics[pi] = take_bytes(c, info, n)?;
        }
        let ph = &info.bytes[st.pics[pi].range()];
        if ph.len() < 8 || (ph[0] >> 3) as usize != ph.len() || ph[7] >> 4 > 3 || ph[1..5] != [0; 4] {
            return Err(bad("bad picture header"));
        }
        let (n, log2) = (u16::from_be_bytes([ph[5], ph[6]]) as usize, (ph[7] >> 4) as u32);
        let w0 = info.widths.len();
        for w in frame::row_slices(mbw, log2) {
            push(&mut info.widths, w as u32, bounded)?;
        }
        let nw = info.widths.len() - w0;
        if n == 0 || !n.is_multiple_of(nw) || n / nw > 4096 || mbw > 8192 {
            return Err(bad("bad slice count"));
        }
        let rows = n / nw;
        blocks += picture_blocks(&header, mbw, rows);
        *side += SIDE_PICTURE;
        let k = limits.results.len();
        let most = (0..k).map(|s| picture_blocks(&header, mbw, stripe_rows(rows, s, k).len())).max().unwrap_or(0);
        if blocks > limits.blocks || most > limits.stripe_blocks || *side > limits.side {
            return Err(bad("frame past the chunk's limits"));
        }
        size += (st.pics[pi].len as usize + 2 * n) as u64;
        let (mut b0, mut q) = (0u8, 0u8);
        let first_slice = info.slices.len() as u32;
        for si in 0..n {
            if !c.bit(&mut st.bits.same_byte0, false) {
                b0 = c.raw(0, 8) as u8;
            }
            if !c.bit(&mut st.bits.same_q, false) {
                q = c.raw(0, 8) as u8;
            }
            let hs = (b0 >> 3) as usize;
            if hs < 6 || (bounded && info.bytes.capacity() - info.bytes.len() < hs) {
                return Err(bad("bad slice header"));
            }
            let at = info.bytes.len();
            info.bytes.extend_from_slice(&[b0, q]);
            info.bytes.resize(at + hs, 0);
            for i in slice_extra(hs) {
                info.bytes[at + i] = c.raw(0, 8) as u8;
            }
            let sh = Span { at: at as u32, len: hs as u32 };
            let alpha = match header.alpha {
                0 => Span::default(),
                _ => {
                    let n = c.raw(0, 16) as usize;
                    take_bytes(c, info, n)?
                }
            };
            size += (hs + alpha.len as usize) as u64;
            *side += SIDE_SLICE + (hs + alpha.len as usize) as u64;
            let mbs = info.widths[w0 + si % nw];
            let mut tails = [Tail::default(); 3];
            for t in &mut tails {
                if c.bit(&mut st.bits.tail, false) {
                    t.empty = c.raw(0, 1) != 0;
                    t.zero_bytes = c.raw(0, 16);
                    size += t.zero_bytes as u64;
                }
            }
            push(&mut info.slices, SliceRec { header: sh, alpha, mbs, tails }, bounded)?;
            if size > limits.frame || *side > limits.side || c.overrun() {
                return Err(bad("corrupt chunk"));
            }
        }
        let p = PicRec {
            header: st.pics[pi],
            widths: Span { at: w0 as u32, len: nw as u32 },
            rows: rows as u32,
            first: first_slice,
        };
        push(&mut info.pics, p, bounded)?;
    }
    let n = c.raw(0, 32) as usize;
    *side += (st.header.len as usize + n) as u64;
    if size + n as u64 > limits.frame || *side > limits.side {
        return Err(bad("corrupt chunk"));
    }
    let (zeros, trailer) = match n > 0 && c.bit(&mut st.bits.trailer_zero, false) {
        true => (true, Span { at: 0, len: n as u32 }),
        false => (false, take_bytes(c, info, n)?),
    };
    Ok(FrameRec {
        header: st.header,
        width: header.width as u32,
        chroma: header.chroma,
        pics: npics as u8,
        zeros,
        first,
        trailer,
    })
}

/// The planes a stripe decodes its pictures' components into, of luma and chroma: of
/// `stripe_blocks` blocks of a picture's stripe (all components), luma takes at most half of
/// them (4:2:2) and each chroma component a third (4:4:4).
fn plane_caps(stripe_blocks: u64) -> (usize, usize) {
    ((stripe_blocks / 2) as usize, (stripe_blocks / 3) as usize)
}

/// Blocks of a slice's component at most (8 macroblocks of 4 blocks).
const SLICE_COMP_BLOCKS: usize = 32;

/// Bytes of [`aqr_api::stack`] that decoding stripe `s` of `limits` takes in its task, of
/// `raw` bytes of raw bits: those padded, its result, its model, its planes (two sets of
/// `fields` pictures: of the frame before and of this one), and a slice's coefficients.
fn stripe_plan(limits: &Limits, s: usize, raw: u64, fields: u64) -> u64 {
    use aqr_api::stack::block;
    let (luma, chroma) = plane_caps(limits.stripe_blocks);
    let plane = |n: usize| block(128 * n as u64) + block(n as u64);
    let planes = 2 * fields * (plane(luma) + 2 * plane(chroma));
    block(raw + aqr_common::msb::PAD as u64)
        + block(limits.results[s] + 1)
        + Model::plan(luma)
        + planes
        + 3 * block(2 * 64 * SLICE_COMP_BLOCKS as u64)
}

/// The memory plan of decoding a chunk on WebAssembly ([`aqr_api::stack`]), of params `limits`,
/// the coded chunk `coded` and an output of `len` bytes, given the instance's memory as it
/// starts (`initial` bytes, whole pages), `held` bytes of the stack taken before: the most the
/// instance takes and the most a task does, in bytes.
/// The instance takes the streams' slices, the first stream's raw bits padded, what it tells
/// (`SideInfo`) and the vector of the stripes' results; then the tasks run, the instance
/// taking each result as it is joined; then the output.
pub fn plan(limits: &Limits, coded: &[u8], len: u64, initial: u64, held: u64) -> Option<(u64, u64)> {
    use aqr_api::stack::block;
    let pages = |n: u64| n.div_ceil(1 << 16) << 16;
    let k = limits.results.len();
    let streams = streams(coded, k).ok()?;
    let c = &limits.counts;
    let fields = if c.pics > c.frames { 2 } else { 1 };
    let top = initial
        + held
        + block(16 * (1 + k as u64))
        + block((streams[0].1.len() + aqr_common::msb::PAD) as u64)
        + SideInfo::plan(c)
        + block(12 * k as u64);
    let end = pages(top);
    let tasks: Vec<u64> =
        (0..k).map(|s| end.max(pages(top + stripe_plan(limits, s, streams[1 + s].1.len() as u64, fields)))).collect();
    let results: u64 = limits.results.iter().map(|&r| block(r + 1)).sum();
    // (the handles taken, then each result as it is joined; the handles given back after)
    let taken = if k > 1 { block(k as u64) } else { 0 };
    let joined = pages(top + taken + results);
    let (most, task) = match k {
        1 => (0, 0),
        _ => (joined, tasks.into_iter().max().unwrap_or(0)),
    };
    // (on one stripe, decoded in the instance: its work on top, then given back)
    let alone = match k {
        1 => pages(top + stripe_plan(limits, 0, streams[1].1.len() as u64, fields)),
        _ => 0,
    };
    Some((most.max(alone).max(pages(top + taken + results + block(len))), task))
}

/// Decodes stripe `s` of every frame of `info` and writes its slices: for each modelled frame
/// and picture, the lengths of its slices in the stripe (u32, little-endian), then their bytes;
/// after a byte 1. None where the stream does not decode.
fn stripe_dec(s: usize, info: &SideInfo, (arith, raw): (&[u8], &[u8]), limits: &Limits) -> Option<Vec<u8>> {
    let k = limits.results.len();
    // (the result first: below what is given back as the stripe ends)
    let cap = usize::try_from(limits.results[s].checked_add(1)?).ok()?;
    let mut out = Vec::with_capacity(cap);
    out.push(1u8);
    let raw = padded(raw);
    let mut c = Dec::new(arith, &raw);
    let (luma, chroma) = plane_caps(limits.stripe_blocks);
    let mut model = Model::with_scratch(luma);
    let fields = info.fields();
    let set = || -> Vec<[Plane; 3]> { (0..fields).map(|_| plane::with_capacity(luma, chroma)).collect() };
    let (mut prev, mut cur) = (set(), set());
    let mut slice: [(Vec<i16>, Tail); 3] =
        std::array::from_fn(|_| (Vec::with_capacity(64 * SLICE_COMP_BLOCKS), Tail::default()));
    // (the pictures of the frame before, none after a frame kept as it is)
    let mut before = 0;
    let b = &info.bytes;
    for item in &info.items {
        if item.kept {
            before = 0;
            continue;
        }
        let f = &info.frames[item.what.at as usize];
        let mbw = (f.width as usize).div_ceil(16);
        for (pi, p) in info.pics[f.first as usize..][..f.pics as usize].iter().enumerate() {
            let rs = stripe_rows(p.rows as usize, s, k);
            let widths = &info.widths[p.widths.range()];
            let pl = &mut cur[pi];
            if !plane::reset(pl, f.chroma, mbw, rs.len()) {
                return None;
            }
            let prev = (pi < before).then(|| &prev[pi]);
            for (k, q) in pl.iter_mut().enumerate() {
                model.plane(&mut c, q, prev.map(|x| &x[k]), (k > 0) as usize);
            }
            if c.overrun() {
                return None;
            }
            let nw = widths.len();
            let first = p.first as usize + rs.start * nw;
            let n = rs.len() * nw;
            let lens = out.len();
            if lens + 4 * n > cap {
                return None;
            }
            out.resize(lens + 4 * n, 0);
            for (i, sl) in info.slices[first..first + n].iter().enumerate() {
                let x0: usize = widths[..i % nw].iter().map(|&w| w as usize).sum();
                plane::gather(pl, f.chroma, i / nw, x0, sl.mbs as usize, &mut slice, sl.tails);
                let n = frame::write_slice(&mut out, &b[sl.header.range()], &slice, &b[sl.alpha.range()]) as u32;
                out[lens + 4 * i..lens + 4 * i + 4].copy_from_slice(&n.to_le_bytes());
                if out.len() > cap {
                    return None;
                }
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        before = f.pics as usize;
    }
    (!c.overrun()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Picture, Slice};

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
