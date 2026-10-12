//! Dynamic Huffman headers, coded against the headers known encoders would write for the
//! block's tokens.
//!
//! Each part comes from a list of candidates (code lengths from several builders, the
//! run-length coding of the lengths from several coders, the code-length code from two
//! builders): an exact candidate costs its index; otherwise the lengths are coded as
//! corrections to the closest candidate and the rest explicitly. zlib's header is checked
//! first, with a single bit.

use super::code::{get_usize, put_gamma};
use super::trees;
use super::write::{LEN_CODE, dist_code};
use crate::bits::{BitReader, BitWriter};
use crate::deflate::{DynHeader, Token};
use crate::huffman::{self, CL_ORDER, RleOp};
use crate::{Error, Result};

/// Symbol frequencies of a block: 288 literal/length (end of block counted once), 32
/// distance.
pub fn freqs(tokens: &[Token]) -> ([u32; 288], [u32; 32]) {
    let mut lf = [0u32; 288];
    let mut df = [0u32; 32];
    lf[256] = 1;
    for &t in tokens {
        match t {
            Token::Lit(b) => lf[b as usize] += 1,
            Token::Match { len, dist, alt258 } => {
                lf[257
                    + if alt258 {
                        27
                    } else {
                        LEN_CODE[len as usize] as usize
                    }] += 1;
                df[dist_code(dist as usize)] += 1;
            }
        }
    }
    (lf, df)
}

/// Number of candidate code lengths ([`lens_candidate`]).
const NUM_LENS: usize = 1 + 3 * 7 + 1;

/// The `k`-th candidate code lengths (288 literal/length, 32 distance), computed on its own
/// (the reader needs only the one it is told): zlib's, then package-merge (zopfli's) of the
/// counts as they are, smoothed by zopfli and by brotli (ECT) at limits 15 down to 9, then
/// package-merge without the distance patch.
fn lens_candidate(k: usize, lf: &[u32], df: &[u32]) -> Option<(Vec<u8>, Vec<u8>)> {
    builder(*ORDER.get(k)?, lf, df)
}

/// The builders by how often they are right: zlib, zopfli, ECT's default (brotli's
/// smoothing, limit 15), plain package-merge (Go and others, with and without zopfli's
/// distance patch), then the rest.
const ORDER: [usize; NUM_LENS] = [
    0, 8, 15, 1, 22, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21,
];

/// The writer looks among these first candidates only (and the previous block's), for
/// speed.
const FEW: usize = 5;

fn builder(k: usize, lf: &[u32], df: &[u32]) -> Option<(Vec<u8>, Vec<u8>)> {
    Some(match k {
        0 => {
            // zlib's trees (predict::predict_trees), from the counts at hand.
            let mut pl = trees::zlib_lens(&lf[..286], 15);
            pl.resize(288, 0);
            let mut pd = trees::zlib_lens(&df[..30], 15);
            pd.resize(32, 0);
            (pl, pd)
        }
        1..=21 => {
            let (v, mb) = ((k - 1) / 7, 15 - (k - 1) % 7);
            let wide = |f: &[u32]| f.iter().map(|&x| x as u64).collect::<Vec<u64>>();
            let (mut l, mut d) = (wide(lf), wide(df));
            match v {
                1 => {
                    trees::smooth_zopfli(&mut l);
                    trees::smooth_zopfli(&mut d);
                }
                2 => {
                    trees::smooth_brotli(&mut d);
                    trees::smooth_brotli(&mut l);
                }
                _ => {}
            }
            let ll = trees::package_merge(&l, mb);
            let mut dd = trees::package_merge(&d, mb);
            trees::patch_distances(&mut dd);
            (ll, dd)
        }
        22 => {
            let wide = |f: &[u32]| f.iter().map(|&x| x as u64).collect::<Vec<u64>>();
            (
                trees::package_merge(&wide(lf), 15),
                trees::package_merge(&wide(df), 15),
            )
        }
        _ => return None,
    })
}

/// Number of candidate run-length codings ([`ops_candidate`]).
const NUM_OPS: usize = 3 + trees::ZOPFLI_FLAGS;

/// The `k`-th candidate run-length coding of `lens` (`hlit` literal/length lengths, then
/// distance): zlib's per tree and concatenated, libdeflate's, then zopfli/ECT's.
fn ops_candidate(k: usize, lens: &[u8], hlit: usize) -> Option<Vec<RleOp>> {
    let mut c = vec![];
    match k {
        0 => {
            huffman::rle_zlib(&lens[..hlit], &mut c);
            huffman::rle_zlib(&lens[hlit..], &mut c);
        }
        1 => huffman::rle_zlib(lens, &mut c),
        2 => huffman::rle_libdeflate(lens, &mut c),
        _ => return trees::zopfli_ops(lens, k - 3),
    }
    Some(c)
}

/// Candidate code-length code lengths for `ops`.
fn cl_candidates(ops: &[RleOp]) -> [[u8; 19]; 2] {
    [cl_candidate(ops, 0).unwrap(), cl_candidate(ops, 1).unwrap()]
}

/// The `k`-th candidate code-length code lengths for `ops`: zlib's, package-merge.
fn cl_candidate(ops: &[RleOp], k: usize) -> Option<[u8; 19]> {
    let mut cf = [0u32; 19];
    for op in ops {
        cf[op.sym as usize] += 1;
    }
    Some(match k {
        0 => trees::zlib_lens(&cf, 7).try_into().unwrap(),
        1 => {
            let cf64 = cf.map(|c| c as u64);
            trees::package_merge(&cf64, 7).try_into().unwrap()
        }
        _ => return None,
    })
}

/// HCLEN as encoders set it: up to the last code-length code used, at least 4.
fn natural_hclen(cl: &[u8; 19]) -> usize {
    (CL_ORDER.iter().rposition(|&s| cl[s] != 0).unwrap_or(0) + 1).max(4)
}

fn last_nonzero(l: &[u8]) -> usize {
    l.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1)
}

/// Lengths of a candidate cut to `hlit` and `hdist`.
fn cut(ll: &[u8], d: &[u8], hlit: usize, hdist: usize) -> Vec<u8> {
    ll[..hlit].iter().chain(&d[..hdist]).copied().collect()
}

/// How the previous block's header was made: which candidate lengths, run-length coding and
/// code-length code. Blocks of one stream are usually all made the same way; zlib's is the
/// first guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recipe {
    lens: usize,
    ops: usize,
    cl: usize,
    /// (Writer only) blocks since the candidates were last searched, and whether that
    /// search found exact lengths.
    since: usize,
    exact: bool,
    /// (Writer only) whether the last search of run-length codings found one.
    ops_exact: bool,
}

impl Recipe {
    /// What the writer found out of the headers so far, as a key: the candidate lengths if
    /// exact (else 255), the run-length coding and the code-length code.
    pub fn key(&self) -> u32 {
        (if self.exact { self.lens as u32 } else { 255 })
            | (self.ops as u32) << 8
            | (self.cl as u32) << 16
    }
}

/// Without exact candidate lengths, the writer searches again every this many blocks.
const RESEARCH: usize = 16;

impl Default for Recipe {
    /// zlib's, to be searched beyond at the first block that differs.
    fn default() -> Self {
        Recipe {
            lens: 0,
            ops: 0,
            cl: 0,
            since: RESEARCH,
            exact: false,
            ops_exact: true,
        }
    }
}

/// The header `recipe` makes for a block (HLIT, HDIST and HCLEN as encoders set them).
fn build(recipe: Recipe, lf: &[u32], df: &[u32]) -> Option<DynHeader> {
    let (ll, d) = lens_candidate(recipe.lens, lf, df)?;
    let (hlit, hdist) = (last_nonzero(&ll).max(257), last_nonzero(&d).max(1));
    let lens = cut(&ll, &d, hlit, hdist);
    let ops = ops_candidate(recipe.ops, &lens, hlit)?;
    let cl = cl_candidate(&ops, recipe.cl)?;
    let hclen = natural_hclen(&cl);
    let mut cl_lens = [0u8; 19];
    for &s in &CL_ORDER[..hclen] {
        cl_lens[s] = cl[s];
    }
    Some(DynHeader {
        hlit: hlit as u16,
        hdist: hdist as u8,
        hclen: hclen as u8,
        cl_lens,
        ops,
        lens,
    })
}

/// Whether `recipe`, whose candidate lengths are `ll` and `d`, makes `h`, checked part by
/// part.
fn makes(recipe: Recipe, ll: &[u8], d: &[u8], h: &DynHeader) -> bool {
    let (hlit, hdist) = (last_nonzero(ll).max(257), last_nonzero(d).max(1));
    if (hlit, hdist) != (h.hlit as usize, h.hdist as usize)
        || ll[..hlit] != h.lens[..hlit]
        || d[..hdist] != h.lens[hlit..]
    {
        return false;
    }
    if ops_candidate(recipe.ops, &h.lens, hlit).as_ref() != Some(&h.ops) {
        return false;
    }
    let Some(cl) = cl_candidate(&h.ops, recipe.cl) else {
        return false;
    };
    let hclen = natural_hclen(&cl);
    hclen == h.hclen as usize
        && CL_ORDER
            .iter()
            .enumerate()
            .all(|(i, &s)| h.cl_lens[s] == if i < hclen { cl[s] } else { 0 })
}

/// Writes `h`, the header of a block whose symbol frequencies are `lf` and `df` (see
/// [`freqs`]); `recipe` carries over from block to block.
pub fn write(
    w: &mut BitWriter,
    lf: &[u32; 288],
    df: &[u32; 32],
    h: &DynHeader,
    recipe: &mut Recipe,
) {
    let (lf, df) = (&lf[..], &df[..]);
    let (ll, d) = lens_candidate(recipe.lens, lf, df).unwrap();
    if makes(*recipe, &ll, &d, h) {
        w.bits(1, 1);
        recipe.exact = true;
        return;
    }
    w.bits(1, 0);
    let (hlit, hdist) = (h.hlit as usize, h.hdist as usize);
    // The closest candidate (the first among equals): differences, index, lengths.
    type Closest = (usize, usize, (Vec<u8>, Vec<u8>));
    let mut best: Option<Closest>;
    // The recipe's lengths first. Without exact candidates last time, they are all there is
    // to it (searching is costly), short of a periodic look.
    let pred = cut(&ll, &d, hlit, hdist);
    let diffs = pred.iter().zip(&h.lens).filter(|(a, b)| a != b).count();
    best = Some((diffs, recipe.lens, (ll, d)));
    let search = diffs > 0 && (recipe.exact || recipe.since >= RESEARCH);
    recipe.since += 1;
    for k in (0..FEW).filter(|_| search) {
        let (ll, d) = lens_candidate(k, lf, df).unwrap();
        let pred = cut(&ll, &d, hlit, hdist);
        let diffs = pred.iter().zip(&h.lens).filter(|(a, b)| a != b).count();
        if diffs == 0 || (k < FEW && best.as_ref().is_none_or(|b| diffs < b.0)) {
            best = Some((diffs, k, (ll, d)));
            if diffs == 0 {
                break;
            }
        }
    }
    if search {
        recipe.since = 0;
        recipe.exact = best.as_ref().is_some_and(|b| b.0 == 0);
    }
    let (_, k, (ll, d)) = best.unwrap();
    put_gamma(w, k as u64);
    recipe.lens = k;
    put_field(
        w,
        hlit as u32 - 257,
        last_nonzero(&ll).max(257) as u32 - 257,
        5,
    );
    put_field(w, hdist as u32 - 1, last_nonzero(&d).max(1) as u32 - 1, 5);
    let pred = cut(&ll, &d, hlit, hdist);
    let diffs: Vec<usize> = (0..pred.len()).filter(|&i| h.lens[i] != pred[i]).collect();
    put_gamma(w, diffs.len() as u64);
    let mut last = 0;
    for &i in &diffs {
        put_gamma(w, (i - last) as u64);
        w.bits(4, h.lens[i] as u32);
        last = i + 1;
    }

    // The recipe's run-length coding first; the others only when they were right last time
    // (or now and then).
    let is = |k: usize| ops_candidate(k, &h.lens, hlit).as_ref() == Some(&h.ops);
    let found = if is(recipe.ops) {
        Some(recipe.ops)
    } else if recipe.ops_exact || recipe.since == 1 {
        let f = (0..NUM_OPS).position(is);
        recipe.ops_exact = f.is_some();
        f
    } else {
        None
    };
    match found {
        Some(k) => {
            w.bits(1, 1);
            put_gamma(w, k as u64);
            recipe.ops = k;
        }
        None => {
            w.bits(1, 0);
            write_ops(w, &h.lens, &h.ops);
        }
    }

    let cands = cl_candidates(&h.ops);
    let order = &CL_ORDER[..h.hclen as usize];
    let k = cands
        .iter()
        .position(|c| order.iter().all(|&s| c[s] == h.cl_lens[s]));
    w.bits(2, k.unwrap_or(2) as u32);
    if let Some(k) = k {
        recipe.cl = k;
    }
    put_field(
        w,
        h.hclen as u32 - 4,
        natural_hclen(&cands[k.unwrap_or(0)]) as u32 - 4,
        4,
    );
    if k.is_none() {
        for &s in order {
            w.bits(3, h.cl_lens[s] as u32);
        }
    }
}

/// Reads a header written by [`write`] for a block of `tokens`.
pub fn read(r: &mut BitReader, tokens: &[Token], recipe: &mut Recipe) -> Result<DynHeader> {
    let (lf, df) = freqs(tokens);
    read_freqs(r, &lf, &df, recipe)
}

/// Reads a header written by [`write`] for a block whose symbol frequencies (see [`freqs`])
/// the caller counted.
pub fn read_freqs(
    r: &mut BitReader,
    lf: &[u32; 288],
    df: &[u32; 32],
    recipe: &mut Recipe,
) -> Result<DynHeader> {
    let (lf, df) = (*lf, *df);
    if r.bits(1)? == 1 {
        return build(*recipe, &lf, &df).ok_or(Error::Corrupt);
    }
    let k = get_usize(r, 64)?;
    recipe.lens = k;
    let (ll, d) = lens_candidate(k, &lf, &df).ok_or(Error::Corrupt)?;
    let hlit = get_field(r, last_nonzero(&ll).max(257) as u32 - 257, 5)? as usize + 257;
    let hdist = get_field(r, last_nonzero(&d).max(1) as u32 - 1, 5)? as usize + 1;
    if hlit > ll.len() || hdist > d.len() {
        return Err(Error::Corrupt);
    }
    let mut lens = cut(&ll, &d, hlit, hdist);
    let ndiff = get_usize(r, lens.len())?;
    let mut i = 0;
    for _ in 0..ndiff {
        i += get_usize(r, lens.len())?;
        let l = lens.get_mut(i).ok_or(Error::Corrupt)?;
        *l = r.bits(4)? as u8;
        i += 1;
    }
    if lens[256] == 0
        || huffman::check_lens(&lens[..hlit]).is_err()
        || huffman::check_lens(&lens[hlit..]).is_err()
    {
        return Err(Error::Corrupt);
    }
    let ops = if r.bits(1)? == 1 {
        let k = get_usize(r, 64)?;
        recipe.ops = k;
        ops_candidate(k, &lens, hlit).ok_or(Error::Corrupt)?
    } else {
        read_ops(r, &lens)?
    };
    let cands = cl_candidates(&ops);
    let k = r.bits(2)? as usize;
    if k > 2 {
        return Err(Error::Corrupt);
    }
    if k < 2 {
        recipe.cl = k;
    }
    let hclen = get_field(
        r,
        natural_hclen(&cands[k.min(1) * (k < 2) as usize]) as u32 - 4,
        4,
    )? as usize
        + 4;
    let order = &CL_ORDER[..hclen];
    let mut cl_lens = [0u8; 19];
    for &s in order {
        cl_lens[s] = if k < 2 { cands[k][s] } else { r.bits(3)? as u8 };
    }
    if huffman::check_lens(&cl_lens).is_err() || ops.iter().any(|op| cl_lens[op.sym as usize] == 0)
    {
        return Err(Error::Corrupt);
    }
    Ok(DynHeader {
        hlit: hlit as u16,
        hdist: hdist as u8,
        hclen: hclen as u8,
        cl_lens,
        ops,
        lens,
    })
}

/// Ops one by one: their kind, and for repeats whether they run as far as they can.
fn write_ops(w: &mut BitWriter, lens: &[u8], ops: &[RleOp]) {
    put_gamma(w, ops.len() as u64);
    let mut i = 0;
    for op in ops {
        let (ty, max) = match op.sym {
            0..=15 => (0, 0),
            16 => (1, run(lens, i, lens.get(i.wrapping_sub(1)).copied(), 6)),
            17 => (2, run(lens, i, Some(0), 10)),
            _ => (3, run(lens, i, Some(0), 138)),
        };
        w.bits(2, ty);
        if ty > 0 {
            let full = op.count() == max;
            w.bits(1, full as u32);
            if !full {
                w.bits(RleOp::extra_bits(op.sym), op.extra as u32);
            }
        }
        i += op.count();
    }
}

fn read_ops(r: &mut BitReader, lens: &[u8]) -> Result<Vec<RleOp>> {
    let n = get_usize(r, lens.len())?;
    let mut ops = Vec::with_capacity(n);
    let mut i = 0;
    for _ in 0..n {
        let cur = *lens.get(i).ok_or(Error::Corrupt)?;
        let (sym, value, min, max) = match r.bits(2)? {
            0 => {
                ops.push(RleOp { sym: cur, extra: 0 });
                i += 1;
                continue;
            }
            1 => (16, lens.get(i.wrapping_sub(1)).copied(), 3, 6),
            2 => (17, Some(0), 3, 10),
            _ => (18, Some(0), 11, 138),
        };
        let full = run(lens, i, value, max);
        let count = if r.bits(1)? == 1 {
            full
        } else {
            min + r.bits(RleOp::extra_bits(sym))? as usize
        };
        if count < min || count > full {
            return Err(Error::Corrupt);
        }
        ops.push(RleOp {
            sym,
            extra: (count - min) as u8,
        });
        i += count;
    }
    if i != lens.len() {
        return Err(Error::Corrupt);
    }
    Ok(ops)
}

/// The longest run at `i` of `value` (none if `None`), up to `max`.
fn run(lens: &[u8], i: usize, value: Option<u8>, max: usize) -> usize {
    value.map_or(0, |v| {
        lens[i.min(lens.len())..]
            .iter()
            .take_while(|&&l| l == v)
            .count()
            .min(max)
    })
}

/// A value that usually equals `pred`: one bit, or the value in `n` bits.
fn put_field(w: &mut BitWriter, v: u32, pred: u32, n: u32) {
    if v == pred {
        w.bits(1, 1);
    } else {
        w.bits(1, 0);
        w.bits(n, v);
    }
}

fn get_field(r: &mut BitReader, pred: u32, n: u32) -> Result<u32> {
    if r.bits(1)? == 1 {
        Ok(pred)
    } else {
        Ok(r.bits(n)?)
    }
}
