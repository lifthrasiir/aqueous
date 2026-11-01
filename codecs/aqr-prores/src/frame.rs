//! A frame's layout: its size and `icpf`, the frame header, one picture (two fields if
//! interlaced), each a picture header, the slices' sizes and the slices; a slice is a header
//! (with its quantizer and its components' sizes), then Y, Cb, Cr and alpha.
//!
//! Headers are kept as they are; the slices' and pictures' sizes are what the coded data
//! gives, and are checked against the frame's.

use aqr_common::msb::{BitWriter, padded};

use crate::coef::{self, Tail};
use crate::{Error, Result};

fn be16(d: &[u8], at: usize) -> usize {
    u16::from_be_bytes([d[at], d[at + 1]]) as usize
}

fn be32(d: &[u8], at: usize) -> usize {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize
}

fn bad(what: &'static str) -> Error {
    Error(what)
}

#[derive(Debug, Clone)]
pub struct Header {
    /// From the frame header's size on, as stored.
    pub raw: Vec<u8>,
    pub version: u16,
    pub creator: [u8; 4],
    pub width: usize,
    pub height: usize,
    /// 2: 4:2:2, 3: 4:4:4.
    pub chroma: u8,
    /// 0: progressive, 1: top field first, 2: bottom field first.
    pub interlace: u8,
    /// 0: none, 1: 8 bits, 2: 16 bits.
    pub alpha: u8,
}

/// The frame's size (from the size field on) if `d` starts with a frame whose header makes
/// sense.
pub fn plausible(d: &[u8]) -> Option<usize> {
    if d.len() < 8 + 20 || &d[4..8] != b"icpf" {
        return None;
    }
    let size = be32(d, 0);
    let hs = be16(d, 8);
    let ok = size <= d.len()
        && hs >= 20
        && 8 + hs + 8 <= size
        && be16(d, 10) <= 1
        && be16(d, 16) > 0
        && be16(d, 18) > 0
        && d[20] >> 6 >= 2;
    ok.then_some(size)
}

#[derive(Debug, Clone)]
pub struct Slice {
    /// The slice header as stored (its sizes included).
    pub header: Vec<u8>,
    pub mbs: usize,
    /// Y, Cb, Cr coefficients (see [`coef`]) and what followed them.
    pub comps: [(Vec<i16>, Tail); 3],
    /// The alpha plane's bits, kept as they are.
    pub alpha: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Picture {
    pub header: Vec<u8>,
    pub slices: Vec<Slice>,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub header: Header,
    pub pictures: Vec<Picture>,
    /// Bytes after the last picture, within the frame's size.
    pub trailer: Vec<u8>,
}

/// Slice widths in macroblocks along a row of `mbw`: `1 << log2` while they fit, then halves.
fn row_slices(mbw: usize, log2: u32) -> impl Iterator<Item = usize> {
    let mut x = 0;
    std::iter::from_fn(move || {
        if x >= mbw {
            return None;
        }
        let mut w = 1 << log2;
        while x + w > mbw {
            w >>= 1;
        }
        x += w;
        Some(w)
    })
}

impl Frame {
    /// Parses the frame at the start of `d`.
    pub fn parse(d: &[u8]) -> Result<Frame> {
        let size = plausible(d).ok_or(bad("not a frame"))?;
        let d = &d[..size];
        let hs = be16(d, 8);
        let h = &d[8..8 + hs];
        let header = Header {
            raw: h.to_vec(),
            version: be16(h, 2) as u16,
            creator: h[4..8].try_into().unwrap(),
            width: be16(h, 8),
            height: be16(h, 10),
            chroma: h[12] >> 6,
            interlace: (h[12] >> 2) & 3,
            alpha: h[17] & 15,
        };
        let npics = if header.interlace == 0 { 1 } else { 2 };
        let mbw = header.width.div_ceil(16);
        let mut at = 8 + hs;
        let mut pictures = Vec::with_capacity(npics);
        for _ in 0..npics {
            let (pic, n) = Self::picture(&d[at..], &header, mbw)?;
            pictures.push(pic);
            at += n;
        }
        Ok(Frame { header, pictures, trailer: d[at..].to_vec() })
    }

    fn picture(d: &[u8], header: &Header, mbw: usize) -> Result<(Picture, usize)> {
        if d.len() < 8 {
            return Err(bad("picture header past the frame"));
        }
        let hs = (d[0] >> 3) as usize;
        let size = be32(d, 1);
        let n = be16(d, 5);
        let log2w = (d[7] >> 4) as u32;
        if hs < 8 || size > d.len() || hs + 2 * n > size || log2w > 3 {
            return Err(bad("bad picture header"));
        }
        let per_row = row_slices(mbw, log2w).count();
        if n == 0 || !n.is_multiple_of(per_row) {
            return Err(bad("slice count not whole rows"));
        }
        let d = &d[..size];
        let p = padded(d);
        let mut at = hs + 2 * n;
        let mut slices = Vec::with_capacity(n);
        let widths: Vec<usize> = row_slices(mbw, log2w).collect();
        for i in 0..n {
            let len = be16(d, hs + 2 * i);
            if at + len > size {
                return Err(bad("slice past the picture"));
            }
            slices.push(Self::slice(&p, at, len, widths[i % per_row], header)?);
            at += len;
        }
        if at != size {
            return Err(bad("picture size disagrees"));
        }
        Ok((Picture { header: d[..hs].to_vec(), slices }, size))
    }

    fn slice(p: &[u8], at: usize, len: usize, mbs: usize, header: &Header) -> Result<Slice> {
        if len < 6 {
            return Err(bad("slice too short"));
        }
        let hs = (p[at] >> 3) as usize;
        if hs < 6 || hs > len {
            return Err(bad("bad slice header"));
        }
        let ys = be16(p, at + 2);
        let us = be16(p, at + 4);
        let vs = if hs >= 8 { be16(p, at + 6) } else { len.checked_sub(hs + ys + us).ok_or(bad("slice sizes"))? };
        let rest = (len - hs).checked_sub(ys + us + vs).ok_or(bad("slice sizes"))?;
        if rest != 0 && header.alpha == 0 {
            return Err(bad("bytes after Cr without alpha"));
        }
        let cblocks = if header.chroma == 3 { 4 * mbs } else { 2 * mbs };
        let mut start = at + hs;
        let mut comps: [(Vec<i16>, Tail); 3] = Default::default();
        for (k, (sz, blocks)) in [(ys, 4 * mbs), (us, cblocks), (vs, cblocks)].into_iter().enumerate() {
            let mut c = vec![0i16; blocks * 64];
            let tail = coef::decode(p, start * 8, (start + sz) * 8, blocks, &mut c)?;
            comps[k] = (c, tail);
            start += sz;
        }
        Ok(Slice { header: p[at..at + hs].to_vec(), mbs, comps, alpha: p[start..at + len].to_vec() })
    }

    /// The frame's bytes, from its size field on.
    pub fn write(&self) -> Vec<u8> {
        let mut out = vec![0; 8];
        out[4..8].copy_from_slice(b"icpf");
        out.extend_from_slice(&self.header.raw);
        for pic in &self.pictures {
            let p0 = out.len();
            out.extend_from_slice(&pic.header);
            let table = out.len();
            out.resize(table + 2 * pic.slices.len(), 0);
            for (i, s) in pic.slices.iter().enumerate() {
                let s0 = out.len();
                out.extend_from_slice(&s.header);
                let mut sizes = [0usize; 3];
                for (k, (c, tail)) in s.comps.iter().enumerate() {
                    let mut w = BitWriter::with_capacity(c.len());
                    coef::encode(&mut w, c, *tail);
                    let b = w.finish();
                    sizes[k] = b.len();
                    out.extend_from_slice(&b);
                }
                out.extend_from_slice(&s.alpha);
                for (k, n) in sizes.into_iter().enumerate() {
                    if k < 2 || s.header.len() >= 8 {
                        out[s0 + 2 + 2 * k..s0 + 4 + 2 * k].copy_from_slice(&(n as u16).to_be_bytes());
                    }
                }
                let len = (out.len() - s0) as u16;
                out[table + 2 * i..table + 2 * i + 2].copy_from_slice(&len.to_be_bytes());
            }
            let size = (out.len() - p0) as u32;
            out[p0 + 1..p0 + 5].copy_from_slice(&size.to_be_bytes());
        }
        out.extend_from_slice(&self.trailer);
        let size = out.len() as u32;
        out[..4].copy_from_slice(&size.to_be_bytes());
        out
    }
}
