//! A chunk of frames coded frame by frame. A chunk is a span of data: its frames, and the bytes
//! between them, which go apart. Its frames are coded in order, each modelled ([`crate::model`])
//! with the one before as context, or kept as it is where it does not parse.
//!
//! The coded chunk is two streams of pieces, one for each frame: the first (the side stream)
//! holds what is not coefficients (the gaps' lengths, the frames kept, the headers as
//! differences from the previous ones, the slices' headers, tails and alpha); the second the
//! coefficients of a stripe of rows of slices of every picture, a piece for each stripe, each
//! stripe with a model of its own. Every piece restarts the arithmetic coder, so that a frame
//! decodes from its pieces and the state kept between frames. So the stripes decode at once (a
//! task each, [`Stripe`]), a frame at a time, and the frames are put together from what they
//! write ([`assemble`]). `FORMAT.md` specifies it.

use std::ops::Range;

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::msb::{BitReader, BitWriter, PAD};
use aqr_common::varint;

use crate::coef::Tail;
use crate::frame::{self, Frame, Header};
use crate::model::{Coder, Model};
use crate::plane::{self, Plane};
use crate::{Error, Result};

/// Bytes of frames a stripe codes, about: a chunk has `⌈its frames' bytes / STRIPE_BYTES⌉`
/// stripes, at most [`MAX_STRIPES`]. (Each stripe's model learns anew: an animated film lost
/// 0.6% at 8 stripes for 56 MB, 2.3% at 32; camera footage gained a little.)
pub const STRIPE_BYTES: usize = 4 << 20;
pub const MAX_STRIPES: usize = 16;
/// Bytes of a frame a stripe codes at least: with less, the messages of a frame (to every
/// stripe's task and back) cost more than the stripe's work. (Natively the channels wake only
/// their own waiters: 4 stripes of 4 KB frames decode 3.6 times as fast as one, 16 as fast as 4;
/// with the global wake-up of earlier emulations 16 stripes were 2 times slower than one.)
pub const STRIPE_FRAME_BYTES: usize = 1 << 10;

/// The slice rows of stripe `s` of `k` of a picture of `rows` of them: `rows * s / k` up to
/// `rows * (s + 1) / k`.
pub(crate) fn stripe_rows(rows: usize, s: usize, k: usize) -> Range<usize> {
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

    /// The arithmetic code and the raw bits.
    fn finish(self) -> (Vec<u8>, Vec<u8>) {
        (self.a.finish(), self.raw.finish())
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
    /// A piece's decoder; `raw` is its raw bits followed by [`PAD`] bytes.
    fn new(arith: &'a [u8], raw: &'a [u8]) -> Self {
        Dec { a: ArithDecoder::new(arith), raw: BitReader::new(raw, 0), raw_bits: (raw.len() - PAD) * 8 }
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

/// Appends a piece to `out`: the lengths of its arithmetic code and raw bits (varints), then
/// them.
fn put_piece(out: &mut Vec<u8>, arith: &[u8], raw: &[u8]) {
    varint::put(out, arith.len() as u64);
    varint::put(out, raw.len() as u64);
    out.extend_from_slice(arith);
    out.extend_from_slice(raw);
}

/// The piece at `*at` of `d`, of which `end` bytes are the message and [`PAD`] more padding: its
/// arithmetic code, and its raw bits with the padding after them (so they are in `d` up to
/// `end + PAD` at most); `*at` moves past the piece.
pub(crate) fn take_piece<'a>(d: &'a [u8], at: &mut usize, end: usize) -> Result<(&'a [u8], &'a [u8])> {
    let bad = || Error("corrupt chunk");
    debug_assert!(d.len() >= end + PAD);
    let a = varint::get(&d[..end], at).map_err(|_| bad())?;
    let r = varint::get(&d[..end], at).map_err(|_| bad())?;
    let ra = at.checked_add(usize::try_from(a).map_err(|_| bad())?).filter(|&x| x <= end).ok_or_else(bad)?;
    let rb = ra.checked_add(usize::try_from(r).map_err(|_| bad())?).filter(|&x| x <= end).ok_or_else(bad)?;
    let arith = &d[*at..ra];
    *at = rb;
    Ok((arith, &d[ra..rb + PAD]))
}

/// The adaptive bits of the headers.
#[derive(Default)]
struct Side {
    same_header: Bit,
    same_pic: Bit,
    same_byte0: Bit,
    same_q: Bit,
    tail: Bit,
    trailer_zero: Bit,
}

/// Bytes of a picture header at most (its size, `hs`, is five bits).
const PIC_HEADER: usize = 32;

/// What the next frame's headers are coded against: the last frame header and picture headers
/// (those of the picture's size zeroed), as the first stream tells them.
#[derive(Default)]
pub(crate) struct SideState {
    bits: Side,
    header: Vec<u8>,
    pics: [Vec<u8>; 2],
}

impl SideState {
    /// The state for frame headers of `head` bytes at most.
    pub(crate) fn new(head: usize) -> SideState {
        SideState {
            bits: Side::default(),
            header: Vec::with_capacity(head),
            pics: [Vec::with_capacity(PIC_HEADER), Vec::with_capacity(PIC_HEADER)],
        }
    }

    /// Bytes of [`aqr_api::stack`] that [`SideState::new`] takes.
    pub(crate) fn plan(head: u64) -> u64 {
        use aqr_api::stack::block;
        block(head) + 2 * block(PIC_HEADER as u64)
    }
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
/// decoding holds to: of one frame, one stripe and one piece, never of the chunk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Limits {
    /// Blocks of a frame (of all its pictures and components).
    pub blocks: u64,
    /// Bytes of a frame.
    pub frame: u64,
    /// Blocks of a stripe of a picture.
    pub stripe_blocks: u64,
    /// The number of stripes.
    pub stripes: u64,
    /// Pictures of a frame (1 or 2).
    pub fields: u64,
    /// Bytes of a frame's headers: the frame header, the pictures' headers and a trailer not of
    /// zeros.
    pub head: u64,
    /// Slices along a row of each picture of a frame, added up.
    pub widths: u64,
    /// Slices of a stripe of a frame (all its pictures).
    pub stripe_slices: u64,
    /// Bytes of the headers and alpha of the slices of a stripe of a frame.
    pub stripe_bytes: u64,
    /// Bytes of a piece of the side stream (its code and raw bits).
    pub side_piece: u64,
    /// Bytes of a piece of a stripe.
    pub piece: u64,
    /// Bytes a stripe writes for a frame (its slices, and their lengths).
    pub result: u64,
}

impl Limits {
    /// The most bytes of a message from the instance to a stripe's task: a byte for its kind, the
    /// side piece and the stripe's, each with its two lengths (five bytes each at most).
    pub fn message(&self) -> u64 {
        1 + self.side_piece + self.piece + 20
    }

    /// The fields in the order of the params (`FORMAT.md` §3).
    pub fn to_array(&self) -> [u64; 12] {
        [
            self.blocks,
            self.frame,
            self.stripe_blocks,
            self.stripes,
            self.fields,
            self.head,
            self.widths,
            self.stripe_slices,
            self.stripe_bytes,
            self.side_piece,
            self.piece,
            self.result,
        ]
    }

    pub fn from_array(a: [u64; 12]) -> Limits {
        let [
            blocks,
            frame,
            stripe_blocks,
            stripes,
            fields,
            head,
            widths,
            stripe_slices,
            stripe_bytes,
            side_piece,
            piece,
            result,
        ] = a;
        Limits {
            blocks,
            frame,
            stripe_blocks,
            stripes,
            fields,
            head,
            widths,
            stripe_slices,
            stripe_bytes,
            side_piece,
            piece,
            result,
        }
    }
}

/// The blocks of `rows` slice rows of a picture `mbw` macroblocks wide.
fn picture_blocks(h: &Header, mbw: usize, rows: usize) -> u64 {
    (rows * mbw * (4 + if h.chroma == 3 { 8 } else { 4 })) as u64
}

/// A chunk coded ([`encode`]).
pub struct Coded {
    /// The side stream: the items, one for each frame, and an end.
    pub side: Vec<u8>,
    /// The stripes' pieces of each modelled frame.
    pub coded: Vec<u8>,
    /// The bytes of the span that are not in its frames, in order.
    pub gaps: Vec<u8>,
    pub limits: Limits,
}

/// Encodes `span`, whose frames (each from its size field on) are at `frames` (in order, apart),
/// as a chunk: the frames coded, the bytes between them given back as they are.
pub fn encode(span: &[u8], frames: &[Range<usize>]) -> Coded {
    let frame_bytes: usize = frames.iter().map(|r| r.len()).sum();
    let by_frame = (frame_bytes / frames.len().max(1)).div_ceil(STRIPE_FRAME_BYTES);
    encode_with(span, frames, frame_bytes.div_ceil(STRIPE_BYTES).min(by_frame).clamp(1, MAX_STRIPES))
}

/// [`encode`] in `k` stripes (1 to [`MAX_STRIPES`]).
pub fn encode_with(span: &[u8], frames: &[Range<usize>], k: usize) -> Coded {
    assert!((1..=MAX_STRIPES).contains(&k));
    let mut st = SideState::default();
    let mut stripes: Vec<(Model, Option<Vec<[Plane; 3]>>)> = (0..k).map(|_| (Model::new(), None)).collect();
    let (mut side, mut coded, mut gaps) = (Vec::new(), Vec::new(), Vec::new());
    let mut limits = Limits { stripes: k as u64, ..Limits::default() };
    let mut at = 0;
    for r in frames {
        debug_assert!(r.start >= at);
        let gap = (r.start - at) as u64;
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
                limits.fields = limits.fields.max(f.pictures.len() as u64);
                let trailer = if f.trailer.iter().all(|&b| b == 0) { 0 } else { f.trailer.len() };
                limits.head = limits.head.max(
                    (f.header.raw.len() + trailer + f.pictures.iter().map(|p| p.header.len()).sum::<usize>()) as u64,
                );
                let mbw = f.header.width.div_ceil(16);
                let mut widths = 0;
                for pic in &f.pictures {
                    widths += pic.widths(&f.header).len();
                }
                limits.widths = limits.widths.max(widths as u64);
                let mut enc = Enc::new();
                side_enc(&mut enc, &mut st, &f);
                let (a, r) = enc.finish();
                side.push(2);
                varint::put(&mut side, gap);
                put_piece(&mut side, &a, &r);
                limits.side_piece = limits.side_piece.max((a.len() + r.len()) as u64);
                for (s, (model, prev)) in stripes.iter_mut().enumerate() {
                    let mut c = Enc::new();
                    let mut planes = Vec::with_capacity(f.pictures.len());
                    let (mut slices, mut extra, mut result, mut any) = (0u64, 0u64, 0u64, false);
                    for (pi, pic) in f.pictures.iter().enumerate() {
                        let ws = pic.widths(&f.header);
                        let rows = pic.slices.len() / ws.len();
                        let rs = stripe_rows(rows, s, k);
                        limits.stripe_blocks = limits.stripe_blocks.max(picture_blocks(&f.header, mbw, rs.len()));
                        let own = &pic.slices[rs.start * ws.len()..rs.end * ws.len()];
                        slices += own.len() as u64;
                        for sl in own {
                            extra += (sl.header.len() + sl.alpha.len()) as u64;
                            result += 4 + (sl.header.len() + sl.sizes.iter().sum::<usize>() + sl.alpha.len()) as u64;
                        }
                        if own.is_empty() {
                            planes.push(plane::empty(&f.header, &ws, 0));
                            continue;
                        }
                        any = true;
                        let mut pl = plane::planes(own, &f.header, &ws);
                        let prev = prev.as_ref().and_then(|v| v.get(pi));
                        for (k, p) in pl.iter_mut().enumerate() {
                            model.plane(&mut c, p, prev.map(|x| &x[k]), (k > 0) as usize);
                        }
                        planes.push(pl);
                    }
                    limits.stripe_slices = limits.stripe_slices.max(slices);
                    limits.stripe_bytes = limits.stripe_bytes.max(extra);
                    limits.result = limits.result.max(result);
                    let (a, r) = if any { c.finish() } else { (Vec::new(), Vec::new()) };
                    limits.piece = limits.piece.max((a.len() + r.len()) as u64);
                    put_piece(&mut coded, &a, &r);
                    *prev = Some(planes);
                }
            }
            Err(_) => {
                side.push(1);
                varint::put(&mut side, gap);
                varint::put(&mut side, d.len() as u64);
                side.extend_from_slice(d);
                stripes.iter_mut().for_each(|(_, prev)| *prev = None);
            }
        }
    }
    side.push(0);
    varint::put(&mut side, (span.len() - at) as u64);
    gaps.extend_from_slice(&span[at..]);
    Coded { side, coded, gaps, limits }
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

/// A range of [`Info::bytes`].
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

/// A modelled frame as the side stream tells it: its header's bytes and what decoding needs
/// of it, and its trailer (`len` zeros where `zeros`).
#[derive(Debug, Clone, Copy, Default)]
struct FrameRec {
    header: Span,
    width: u32,
    chroma: u8,
    pics: u8,
    zeros: bool,
    trailer: Span,
}

/// A picture: its header (its size zeroed), the widths of its slices along a row, its rows of
/// slices, and the first of its slices kept (those of a stripe, in [`Info::slices`]).
#[derive(Debug, Clone, Copy, Default)]
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

/// Bytes of a record (the same on every target).
const SLICE_BYTES: u64 = 44;
const _: () = assert!(size_of::<SliceRec>() == SLICE_BYTES as usize);

/// What the side stream tells of a frame, flat, in room of fixed sizes (it holds the slices of one
/// stripe, or none).
pub(crate) struct Info {
    bytes: Vec<u8>,
    slices: Vec<SliceRec>,
    widths: Vec<u32>,
    cap_bytes: usize,
    cap_slices: usize,
    cap_widths: usize,
    frame: FrameRec,
    pics: [PicRec; 2],
}

impl Info {
    /// Room for `bytes` bytes, `slices` slices and `widths` widths.
    pub(crate) fn new(bytes: usize, slices: usize, widths: usize) -> Info {
        Info {
            bytes: Vec::with_capacity(bytes),
            slices: Vec::with_capacity(slices),
            widths: Vec::with_capacity(widths),
            cap_bytes: bytes,
            cap_slices: slices,
            cap_widths: widths,
            frame: FrameRec::default(),
            pics: [PicRec::default(); 2],
        }
    }

    /// Bytes of [`aqr_api::stack`] that [`Info::new`] takes.
    pub(crate) fn plan(bytes: u64, slices: u64, widths: u64) -> u64 {
        use aqr_api::stack::block;
        block(bytes) + block(SLICE_BYTES * slices) + block(4 * widths)
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.slices.clear();
        self.widths.clear();
    }

    /// `d` appended to the bytes.
    fn put(&mut self, d: &[u8]) -> Result<Span> {
        if self.bytes.len() + d.len() > self.cap_bytes {
            return Err(Error("corrupt chunk"));
        }
        let at = self.bytes.len();
        self.bytes.extend_from_slice(d);
        Ok(Span { at: at as u32, len: d.len() as u32 })
    }

    /// The pictures of the frame.
    fn pictures(&self) -> &[PicRec] {
        &self.pics[..self.frame.pics as usize]
    }
}

/// `n` bytes of `c`'s raw bits appended to `d` (within its capacity `cap`).
fn take_raw(c: &mut Dec, d: &mut Vec<u8>, n: usize, cap: usize) -> Result<()> {
    if n > cap || n > c.raw_bits / 8 {
        return Err(Error("corrupt chunk"));
    }
    d.clear();
    d.extend((0..n).map(|_| c.raw(0, 8) as u8));
    Ok(())
}

/// Drops `n` bytes of `c`'s raw bits.
fn skip_raw(c: &mut Dec, n: usize) -> Result<()> {
    if n > c.raw_bits / 8 {
        return Err(Error("corrupt chunk"));
    }
    (0..n).for_each(|_| {
        c.raw(0, 8);
    });
    Ok(())
}

/// Decodes the headers of a modelled frame from the side piece `(arith, raw)` into `info`, which
/// keeps the slices of stripe `keep.0` of `keep.1` (none where `keep` is `None`).
pub(crate) fn side_dec(
    (arith, raw): (&[u8], &[u8]),
    st: &mut SideState,
    info: &mut Info,
    l: &Limits,
    keep: Option<(usize, usize)>,
) -> Result<()> {
    let bad = |s| Error(s);
    let mut c = Dec::new(arith, raw);
    info.clear();
    if !c.bit(&mut st.bits.same_header, false) {
        let n = c.raw(0, 16) as usize;
        take_raw(&mut c, &mut st.header, n, l.head as usize)?;
    }
    let header = Header::fields(&st.header)?;
    let npics = header.pictures();
    if npics as u64 > l.fields {
        return Err(bad("too many pictures"));
    }
    let mbw = header.width.div_ceil(16);
    let mut rec = FrameRec {
        header: info.put(&st.header)?,
        width: header.width as u32,
        chroma: header.chroma,
        pics: npics as u8,
        ..FrameRec::default()
    };
    let mut blocks = 0u64;
    // (the bytes of the frame so far, but its slices' coefficients)
    let mut size = 8 + st.header.len() as u64;
    let mut pics = [PicRec::default(); 2];
    for (pi, pic) in pics.iter_mut().enumerate().take(npics) {
        if !c.bit(&mut st.bits.same_pic, false) {
            let n = c.raw(0, 8) as usize;
            if !(8..PIC_HEADER).contains(&n) {
                return Err(bad("bad picture header"));
            }
            take_raw(&mut c, &mut st.pics[pi], n, PIC_HEADER)?;
        }
        let ph = &st.pics[pi];
        if ph.len() < 8 || (ph[0] >> 3) as usize != ph.len() || ph[7] >> 4 > 3 || ph[1..5] != [0; 4] {
            return Err(bad("bad picture header"));
        }
        let (n, log2) = (u16::from_be_bytes([ph[5], ph[6]]) as usize, (ph[7] >> 4) as u32);
        let w0 = info.widths.len();
        for w in frame::row_slices(mbw, log2) {
            if info.widths.len() == info.cap_widths {
                return Err(bad("corrupt chunk"));
            }
            info.widths.push(w as u32);
        }
        let nw = info.widths.len() - w0;
        if n == 0 || !n.is_multiple_of(nw) || n / nw > 4096 || mbw > 8192 {
            return Err(bad("bad slice count"));
        }
        let rows = n / nw;
        blocks += picture_blocks(&header, mbw, rows);
        let k = l.stripes as usize;
        let most = (0..k).map(|s| picture_blocks(&header, mbw, stripe_rows(rows, s, k).len())).max().unwrap_or(0);
        if blocks > l.blocks || most > l.stripe_blocks {
            return Err(bad("frame past the chunk's limits"));
        }
        size += (ph.len() + 2 * n) as u64;
        *pic = PicRec {
            header: info.put(&st.pics[pi])?,
            widths: Span { at: w0 as u32, len: nw as u32 },
            rows: rows as u32,
            first: info.slices.len() as u32,
        };
        let mine = keep.map(|(s, k)| stripe_rows(rows, s, k)).unwrap_or(0..0);
        let (mut b0, mut q) = (0u8, 0u8);
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
            let kept = mine.contains(&(si / nw));
            let at = info.bytes.len();
            if kept {
                if at + hs > info.cap_bytes || info.slices.len() == info.cap_slices {
                    return Err(bad("corrupt chunk"));
                }
                info.bytes.extend_from_slice(&[b0, q]);
                info.bytes.resize(at + hs, 0);
            }
            for i in slice_extra(hs) {
                let x = c.raw(0, 8) as u8;
                if kept {
                    info.bytes[at + i] = x;
                }
            }
            let (mut alpha, mut alpha_len) = (Span::default(), 0);
            if header.alpha != 0 {
                alpha_len = c.raw(0, 16) as usize;
                if kept {
                    if info.bytes.len() + alpha_len > info.cap_bytes || alpha_len > c.raw_bits / 8 {
                        return Err(bad("corrupt chunk"));
                    }
                    let at = info.bytes.len();
                    info.bytes.extend((0..alpha_len).map(|_| c.raw(0, 8) as u8));
                    alpha = Span { at: at as u32, len: alpha_len as u32 };
                } else {
                    skip_raw(&mut c, alpha_len)?;
                }
            }
            size += (hs + alpha_len) as u64;
            let mut tails = [Tail::default(); 3];
            for t in &mut tails {
                if c.bit(&mut st.bits.tail, false) {
                    t.empty = c.raw(0, 1) != 0;
                    t.zero_bytes = c.raw(0, 16);
                    size += t.zero_bytes as u64;
                }
            }
            if kept {
                let sh = Span { at: at as u32, len: hs as u32 };
                info.slices.push(SliceRec { header: sh, alpha, mbs: info.widths[w0 + si % nw], tails });
            }
            if size > l.frame || c.overrun() {
                return Err(bad("corrupt chunk"));
            }
        }
    }
    let n = c.raw(0, 32) as usize;
    if size + n as u64 > l.frame {
        return Err(bad("corrupt chunk"));
    }
    (rec.zeros, rec.trailer) = match n > 0 && c.bit(&mut st.bits.trailer_zero, false) {
        true => (true, Span { at: 0, len: n as u32 }),
        false => {
            if info.bytes.len() + n > info.cap_bytes || n > c.raw_bits / 8 {
                return Err(bad("corrupt chunk"));
            }
            let at = info.bytes.len();
            info.bytes.extend((0..n).map(|_| c.raw(0, 8) as u8));
            (false, Span { at: at as u32, len: n as u32 })
        }
    };
    if c.overrun() {
        return Err(bad("corrupt chunk"));
    }
    info.frame = rec;
    info.pics = pics;
    Ok(())
}

/// Planes of room for a stripe's picture: of `stripe_blocks` blocks (all components), luma takes
/// at most half of them (4:2:2) and each chroma component a third (4:4:4).
fn plane_caps(stripe_blocks: u64) -> (usize, usize) {
    ((stripe_blocks / 2) as usize, (stripe_blocks / 3) as usize)
}

/// Blocks of a slice's component at most (8 macroblocks of 4 blocks).
const SLICE_COMP_BLOCKS: usize = 32;

/// Bytes of a set of planes of a picture, and of the vector of `fields` of them, on Wasm (its
/// pointers have 32 bits).
const PLANES_BYTES: u64 = 96;
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<[Plane; 3]>() == PLANES_BYTES as usize);

/// The decoder of one stripe of a chunk, which the stripe's task runs: its model, its planes of
/// the frame before and the side stream's state, as frames go by.
pub(crate) struct Stripe {
    s: usize,
    k: usize,
    model: Model,
    /// The planes of the pictures of the frame before, and those being decoded; `before` is how
    /// many pictures the former has (none after a frame kept as it is).
    prev: Vec<[Plane; 3]>,
    cur: Vec<[Plane; 3]>,
    before: usize,
    slice: [(Vec<i16>, Tail); 3],
    info: Info,
    st: SideState,
}

impl Stripe {
    /// The decoder of stripe `s`, with room as the limits `l` say.
    pub(crate) fn new(l: &Limits, s: usize) -> Stripe {
        let (luma, chroma) = plane_caps(l.stripe_blocks);
        let model = Model::with_scratch(luma);
        let set = || -> Vec<[Plane; 3]> { (0..l.fields).map(|_| plane::with_capacity(luma, chroma)).collect() };
        let (prev, cur) = (set(), set());
        let slice = std::array::from_fn(|_| (Vec::with_capacity(64 * SLICE_COMP_BLOCKS), Tail::default()));
        Stripe {
            s,
            k: l.stripes as usize,
            model,
            prev,
            cur,
            before: 0,
            slice,
            info: Info::new((l.head + l.stripe_bytes) as usize, l.stripe_slices as usize, l.widths as usize),
            st: SideState::new(l.head as usize),
        }
    }

    /// Bytes of [`aqr_api::stack`] that [`Stripe::new`] takes.
    pub(crate) fn plan(l: &Limits) -> u64 {
        use aqr_api::stack::block;
        let (luma, chroma) = plane_caps(l.stripe_blocks);
        let plane = |n: usize| block(128 * n as u64) + block(n as u64);
        let set = block(PLANES_BYTES * l.fields) + l.fields * (plane(luma) + 2 * plane(chroma));
        Model::plan(luma)
            + 2 * set
            + 3 * block(2 * 64 * SLICE_COMP_BLOCKS as u64)
            + Info::plan(l.head + l.stripe_bytes, l.stripe_slices, l.widths)
            + SideState::plan(l.head)
    }

    /// A frame kept as it is goes by: the next has no frame before it as context.
    pub(crate) fn kept(&mut self) {
        self.before = 0;
    }

    /// Decodes the stripe of a modelled frame from the message `d` (of which `end` bytes are the
    /// message, [`PAD`] bytes more padding): the side piece, then the stripe's. Appends to `out`
    /// (cleared first) a byte 1, then for each picture the lengths of its slices in the stripe
    /// (u32, little-endian) and their bytes. False where it does not decode (and then it must
    /// not be given another frame).
    pub(crate) fn frame(&mut self, l: &Limits, d: &[u8], end: usize, out: &mut Vec<u8>) -> bool {
        self.frame_inner(l, d, end, out).is_some()
    }

    fn frame_inner(&mut self, l: &Limits, d: &[u8], end: usize, out: &mut Vec<u8>) -> Option<()> {
        let mut at = 0;
        let side = take_piece(d, &mut at, end).ok()?;
        let piece = take_piece(d, &mut at, end).ok()?;
        if at != end {
            return None;
        }
        side_dec(side, &mut self.st, &mut self.info, l, Some((self.s, self.k))).ok()?;
        let cap = usize::try_from(l.result.checked_add(1)?).ok()?;
        out.clear();
        out.push(1u8);
        let mut c = Dec::new(piece.0, piece.1);
        let (info, k, s) = (&self.info, self.k, self.s);
        let f = &info.frame;
        let mbw = (f.width as usize).div_ceil(16);
        for (pi, p) in info.pictures().iter().enumerate() {
            let rs = stripe_rows(p.rows as usize, s, k);
            let widths = &info.widths[p.widths.range()];
            let pl = &mut self.cur[pi];
            if !plane::reset(pl, f.chroma, mbw, rs.len()) {
                return None;
            }
            let prev = (pi < self.before).then(|| &self.prev[pi]);
            if !rs.is_empty() {
                for (k, q) in pl.iter_mut().enumerate() {
                    self.model.plane(&mut c, q, prev.map(|x| &x[k]), (k > 0) as usize);
                }
            }
            if c.overrun() {
                return None;
            }
            let nw = widths.len();
            let first = p.first as usize;
            let n = rs.len() * nw;
            let lens = out.len();
            if lens + 4 * n > cap {
                return None;
            }
            out.resize(lens + 4 * n, 0);
            for (i, sl) in info.slices.get(first..first + n)?.iter().enumerate() {
                let x0: usize = widths[..i % nw].iter().map(|&w| w as usize).sum();
                plane::gather(pl, f.chroma, i / nw, x0, sl.mbs as usize, &mut self.slice, sl.tails);
                let b = &info.bytes;
                let n = frame::write_slice(out, &b[sl.header.range()], &self.slice, &b[sl.alpha.range()]) as u32;
                out[lens + 4 * i..lens + 4 * i + 4].copy_from_slice(&n.to_le_bytes());
                if out.len() > cap {
                    return None;
                }
            }
        }
        if c.overrun() {
            return None;
        }
        self.before = info.frame.pics as usize;
        std::mem::swap(&mut self.prev, &mut self.cur);
        Some(())
    }
}

/// Puts a modelled frame together in `out` (cleared first): its headers from `info` (whose
/// slices are none, it is the instance's), and the stripes' `results` (what [`Stripe::frame`]
/// writes). Fails where they do not fit the frame.
pub(crate) fn assemble(info: &Info, results: &[Vec<u8>], l: &Limits, out: &mut Vec<u8>) -> Result<()> {
    let bad = || Error("corrupt chunk");
    let stripes = results.len();
    let mut cursors: Vec<&[u8]> = Vec::with_capacity(stripes);
    cursors.extend(results.iter().map(|r| &r[1..]));
    let f = &info.frame;
    let b = &info.bytes;
    out.clear();
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(b"icpf");
    out.extend_from_slice(&b[f.header.range()]);
    for p in info.pictures() {
        let p0 = out.len();
        out.extend_from_slice(&b[p.header.range()]);
        let mut table = out.len();
        let nw = p.widths.len as usize;
        let n = p.rows as usize * nw;
        if out.len() + 2 * n > l.frame as usize {
            return Err(bad());
        }
        out.resize(table + 2 * n, 0);
        for (s, cur) in cursors.iter_mut().enumerate() {
            let count = stripe_rows(p.rows as usize, s, stripes).len() * nw;
            let lens = cur.get(..4 * count).ok_or_else(bad)?;
            let mut total = 0;
            for len in lens.as_chunks::<4>().0 {
                let len = u32::from_le_bytes(*len) as usize;
                out[table..table + 2].copy_from_slice(&u16::try_from(len).map_err(|_| bad())?.to_be_bytes());
                table += 2;
                total += len;
            }
            let d = cur.get(4 * count..4 * count + total).ok_or_else(bad)?;
            if out.len() + d.len() > l.frame as usize {
                return Err(bad());
            }
            out.extend_from_slice(d);
            *cur = &cur[4 * count + total..];
        }
        let size = u32::try_from(out.len() - p0).map_err(|_| bad())?;
        out[p0 + 1..p0 + 5].copy_from_slice(&size.to_be_bytes());
    }
    if out.len() + f.trailer.len as usize > l.frame as usize {
        return Err(bad());
    }
    match f.zeros {
        true => out.resize(out.len() + f.trailer.len as usize, 0),
        false => out.extend_from_slice(&b[f.trailer.range()]),
    }
    let size = u32::try_from(out.len()).map_err(|_| bad())?;
    out[..4].copy_from_slice(&size.to_be_bytes());
    if cursors.iter().any(|c| !c.is_empty()) {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::synthetic;

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
        assert_eq!(e.limits.stripes, k as u64);
        assert_eq!(e.limits.frame, frames.iter().map(|f| f.len() as u64).max().unwrap());
        let params = crate::module::params(&e.limits);
        let decode = |side: &[u8], coded: &[u8], gaps: &[u8], params: &[u8]| {
            crate::module::decode(params, &[side, coded, gaps], span.len() as u64)
        };
        assert_eq!(decode(&e.side, &e.coded, &e.gaps, &params).unwrap(), span);
        let with = |f: &dyn Fn(&mut Limits)| {
            let mut l = e.limits.clone();
            f(&mut l);
            crate::module::params(&l)
        };
        // (limits one short, each failing; a stripe more)
        let tight: [&dyn Fn(&mut Limits); 8] = [
            &|l| l.blocks -= 1,
            &|l| l.stripe_slices -= 1,
            &|l| l.widths -= 1,
            &|l| l.head -= 1,
            &|l| l.side_piece -= 1,
            &|l| l.piece -= 1,
            &|l| l.result -= 1,
            &|l| l.stripes += 1,
        ];
        for (i, f) in tight.iter().enumerate() {
            assert!(decode(&e.side, &e.coded, &e.gaps, &with(f)).is_err(), "limit {i}");
        }
        assert!(decode(&e.side[..e.side.len() / 2], &e.coded, &e.gaps, &params).is_err());
        assert!(decode(&e.side, &e.coded[..e.coded.len() / 2], &e.gaps, &params).is_err());
        assert!(decode(&e.side, &e.coded, &e.gaps[1..], &params).is_err());
    }
}
