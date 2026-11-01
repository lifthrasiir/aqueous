//! A chunk of frames coded as a whole: the frames in order, each modelled ([`crate::model`])
//! with the one before as context, or kept as it is when it does not parse or is not rebuilt
//! exactly. Headers are coded as differences from the previous ones where they repeat; the
//! sizes in them are derived.
//!
//! A chunk: the length of the arithmetic-coded stream (u32, little-endian), that stream, then
//! the raw bits (MSB first).

use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::msb::{BitReader, BitWriter, padded};

use crate::coef::Tail;
use crate::frame::{self, Frame, Header, Picture, Slice};
use crate::model::{Coder, Model};
use crate::plane::{self, Plane};
use crate::{Error, Result};

struct Enc {
    a: ArithEncoder,
    raw: BitWriter,
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

/// What the next frame is coded against.
#[derive(Default)]
struct State {
    model: Model,
    side: Side,
    header: Vec<u8>,
    pics: Vec<Vec<u8>>,
    /// The previous frame's planes, by picture (none after a frame kept as it is).
    planes: Option<Vec<[Plane; 3]>>,
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

/// Encodes the frames (each from its size field on) as a chunk.
pub fn encode(frames: &[&[u8]]) -> Vec<u8> {
    let mut c = Enc { a: ArithEncoder::new(Vec::new()), raw: BitWriter::new() };
    let mut st = State::default();
    c.raw(frames.len() as u32, 32);
    for &d in frames {
        // A frame that parses is written again bit for bit (codewords, signs and positions have
        // one coding each, padding is zero and recorded, sizes are derived and were checked
        // against the coded data), so it is not written to check.
        let f = Frame::parse(d).ok().filter(|f| f.header.raw.len() + 8 <= d.len());
        match f {
            Some(f) => {
                c.bit(&mut st.side.modelled, true);
                frame(&mut c, &mut st, f);
            }
            None => {
                c.bit(&mut st.side.modelled, false);
                c.raw(d.len() as u32, 32);
                bytes(&mut c, &mut d.to_vec(), d.len());
                st.planes = None;
            }
        }
    }
    let a = c.a.finish();
    let mut out = (a.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&a);
    out.extend_from_slice(&c.raw.finish());
    out
}

/// Decodes a chunk into its frames.
pub fn decode(d: &[u8]) -> Result<Vec<Vec<u8>>> {
    let bad = || Error("corrupt chunk");
    let n = u32::from_le_bytes(d.get(..4).ok_or(bad())?.try_into().unwrap()) as usize;
    let a = d.get(4..4 + n).ok_or(bad())?;
    let raw = padded(&d[4 + n..]);
    let mut c = Dec { a: ArithDecoder::new(a), raw: BitReader::new(&raw, 0), raw_bits: (d.len() - 4 - n) * 8 };
    let mut st = State::default();
    let count = c.raw(0, 32) as usize;
    let mut out = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        if c.raw.pos > c.raw_bits || c.a.overrun() {
            return Err(bad());
        }
        if c.bit(&mut st.side.modelled, false) {
            out.push(frame_dec(&mut c, &mut st)?.write());
        } else {
            let len = c.raw(0, 32) as usize;
            if len > c.raw_bits / 8 + 8 {
                return Err(bad());
            }
            let mut v = Vec::new();
            bytes(&mut c, &mut v, len);
            out.push(v);
            st.planes = None;
        }
    }
    if c.raw.pos > c.raw_bits || c.a.overrun() {
        return Err(bad());
    }
    Ok(out)
}

/// The header bytes of a slice that are neither its first two nor its sizes.
fn slice_extra(hs: usize) -> std::ops::Range<usize> {
    (if hs >= 8 { 8 } else { 6 })..hs
}

fn frame(c: &mut Enc, st: &mut State, mut f: Frame) {
    let same = f.header.raw == st.header;
    c.bit(&mut st.side.same_header, same);
    if !same {
        c.raw(f.header.raw.len() as u32, 16);
        bytes(c, &mut f.header.raw.clone(), f.header.raw.len());
        st.header = f.header.raw.clone();
    }
    st.pics.resize(f.pictures.len(), Vec::new());
    let mut planes_out = Vec::with_capacity(f.pictures.len());
    for (pi, pic) in f.pictures.iter_mut().enumerate() {
        // The picture header but its size.
        let mut ph = pic.header.clone();
        ph[1..5].fill(0);
        let same = ph == st.pics[pi];
        c.bit(&mut st.side.same_pic, same);
        if !same {
            c.raw(ph.len() as u32, 8);
            bytes(c, &mut ph.clone(), ph.len());
            st.pics[pi] = ph;
        }
        let (mut b0, mut q) = (0u8, 0u8);
        for s in &pic.slices {
            let h = &s.header;
            if !c.bit(&mut st.side.same_byte0, h[0] == b0) {
                c.raw(h[0] as u32, 8);
            }
            if !c.bit(&mut st.side.same_q, h[1] == q) {
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
                if c.bit(&mut st.side.tail, *t != Tail::default()) {
                    c.raw(t.empty as u32, 1);
                    c.raw(t.zero_bytes, 16);
                }
            }
        }
        let ws = pic.widths(&f.header);
        let mut pl = plane::planes(pic, &f.header, &ws);
        let prev = st.planes.as_ref().and_then(|v| v.get(pi));
        for (k, p) in pl.iter_mut().enumerate() {
            st.model.plane(c, p, prev.map(|x| &x[k]), (k > 0) as usize);
        }
        planes_out.push(pl);
    }
    c.raw(f.trailer.len() as u32, 32);
    if !f.trailer.is_empty() && !c.bit(&mut st.side.trailer_zero, f.trailer.iter().all(|&b| b == 0)) {
        bytes(c, &mut f.trailer.clone(), f.trailer.len());
    }
    st.planes = Some(planes_out);
}

fn frame_dec(c: &mut Dec, st: &mut State) -> Result<Frame> {
    let bad = |s| Error(s);
    if !c.bit(&mut st.side.same_header, false) {
        let n = c.raw(0, 16) as usize;
        bytes(c, &mut st.header, n);
    }
    let header = Header::new(st.header.clone())?;
    let npics = header.pictures();
    st.pics.resize(npics, Vec::new());
    let mbw = header.width.div_ceil(16);
    let mut pictures = Vec::with_capacity(npics);
    let mut planes_out = Vec::with_capacity(npics);
    for pi in 0..npics {
        if !c.bit(&mut st.side.same_pic, false) {
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
        let (mut b0, mut q) = (0u8, 0u8);
        let mut slices = Vec::with_capacity(n);
        for si in 0..n {
            if !c.bit(&mut st.side.same_byte0, false) {
                b0 = c.raw(0, 8) as u8;
            }
            if !c.bit(&mut st.side.same_q, false) {
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
            let mbs = ws[si % ws.len()];
            let cblocks = if header.chroma == 3 { 4 * mbs } else { 2 * mbs };
            let comps: [(Vec<i16>, Tail); 3] = std::array::from_fn(|k| {
                let blocks = if k == 0 { 4 * mbs } else { cblocks };
                let mut t = Tail::default();
                if c.bit(&mut st.side.tail, false) {
                    t.empty = c.raw(0, 1) != 0;
                    t.zero_bytes = c.raw(0, 16);
                }
                (vec![0; blocks * 64], t)
            });
            slices.push(Slice { header: sh, mbs, comps, alpha, sizes: [0; 3] });
            if c.raw.pos > c.raw_bits || c.a.overrun() {
                return Err(bad("corrupt chunk"));
            }
        }
        let mut pl = plane::empty(&header, &ws, rows);
        let prev = st.planes.as_ref().and_then(|v| v.get(pi));
        for (k, p) in pl.iter_mut().enumerate() {
            st.model.plane(c, p, prev.map(|x| &x[k]), (k > 0) as usize);
        }
        plane::scatter(&pl, &mut slices, &header, &ws);
        let mut header = ph;
        header[1..5].fill(0);
        pictures.push(Picture { header, slices });
        planes_out.push(pl);
    }
    let n = c.raw(0, 32) as usize;
    if n > c.raw_bits / 8 + (1 << 20) {
        return Err(bad("corrupt chunk"));
    }
    let trailer = if n > 0 && c.bit(&mut st.side.trailer_zero, false) {
        vec![0; n]
    } else {
        let mut v = Vec::new();
        bytes(c, &mut v, n);
        v
    };
    st.planes = Some(planes_out);
    Ok(Frame { header, pictures, trailer })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mbw = (w as usize).div_ceil(16);
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
        let refs: Vec<&[u8]> = frames.iter().map(|f| &f[..]).collect();
        let e = encode(&refs);
        assert_eq!(decode(&e).unwrap(), frames);
        assert!(decode(&e[..e.len() / 2]).is_err() || decode(&e[..e.len() / 2]).unwrap() != frames);
    }
}
