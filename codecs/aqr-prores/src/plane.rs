//! A picture's coefficients as planes of blocks: the slices' blocks put in place.
//!
//! In a slice, a macroblock's luma blocks come in the order top left, top right, bottom left,
//! bottom right; 4:2:2 chroma has two blocks per macroblock, top then bottom, and 4:4:4 chroma
//! four, as luma. Slices are one macroblock high, laid left to right along a row.

use crate::frame::{Header, Picture, Slice};

/// Blocks of one component, in raster order, each in scan order.
pub struct Plane {
    /// Width and height in blocks.
    pub bw: usize,
    pub bh: usize,
    pub blocks: Vec<[i16; 64]>,
    /// Each block's slice quantizer index.
    pub q: Vec<u8>,
}

/// Where block `b` of a slice's component goes, in blocks from the slice's top left, given
/// the blocks per macroblock.
#[inline]
pub fn place(b: usize, per_mb: usize) -> (usize, usize) {
    match per_mb {
        4 => ((b >> 2) * 2 + (b & 1), (b >> 1) & 1),
        _ => (b >> 1, b & 1),
    }
}

/// The planes (Y, Cb, Cr) of `pic`, whose slices are `widths` macroblocks wide along a row.
pub fn planes(pic: &Picture, h: &Header, widths: &[usize]) -> [Plane; 3] {
    let mbw: usize = widths.iter().sum();
    let rows = pic.slices.len() / widths.len();
    std::array::from_fn(|k| {
        let per_mb = if k == 0 || h.chroma == 3 { 4 } else { 2 };
        let bw = if per_mb == 4 { 2 * mbw } else { mbw };
        let bh = 2 * rows;
        let mut p = Plane { bw, bh, blocks: vec![[0; 64]; bw * bh], q: vec![0; bw * bh] };
        let mut s = 0;
        for r in 0..rows {
            let mut x0 = 0;
            for &w in widths {
                let sl = &pic.slices[s];
                let q = sl.header[1];
                for (b, c) in sl.comps[k].0.as_chunks::<64>().0.iter().enumerate() {
                    let (x, y) = place(b, per_mb);
                    let i = (2 * r + y) * bw + x0 + x;
                    p.blocks[i].copy_from_slice(c);
                    p.q[i] = q;
                }
                x0 += if per_mb == 4 { 2 * w } else { w };
                s += 1;
            }
        }
        p
    })
}

/// Empty planes (all zero) for a picture of `rows` slice rows of `widths`.
pub fn empty(h: &Header, widths: &[usize], rows: usize) -> [Plane; 3] {
    let mbw: usize = widths.iter().sum();
    std::array::from_fn(|k| {
        let per_mb = if k == 0 || h.chroma == 3 { 4 } else { 2 };
        let bw = if per_mb == 4 { 2 * mbw } else { mbw };
        let bh = 2 * rows;
        Plane { bw, bh, blocks: vec![[0; 64]; bw * bh], q: vec![0; bw * bh] }
    })
}

/// Puts the planes' blocks back into the slices (whose components are sized already).
pub fn scatter(planes: &[Plane; 3], slices: &mut [Slice], h: &Header, widths: &[usize]) {
    for (k, p) in planes.iter().enumerate() {
        let per_mb = if k == 0 || h.chroma == 3 { 4 } else { 2 };
        let mut s = 0;
        for r in 0..slices.len() / widths.len() {
            let mut x0 = 0;
            for &w in widths {
                for (b, c) in slices[s].comps[k].0.as_chunks_mut::<64>().0.iter_mut().enumerate() {
                    let (x, y) = place(b, per_mb);
                    c.copy_from_slice(&p.blocks[(2 * r + y) * p.bw + x0 + x]);
                }
                x0 += if per_mb == 4 { 2 * w } else { w };
                s += 1;
            }
        }
    }
}
