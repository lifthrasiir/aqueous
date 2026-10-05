//! The predictor: walks a DEFLATE stream in reconstruction order and turns every choice the
//! encoder made into binary decisions relative to a prediction.
//!
//! Order of coding (which need not follow the bitstream order):
//! 1. predictor configuration (bypass bits);
//! 2. per block: type, size (stored length or token count), tokens, then for dynamic blocks
//!    the Huffman header predicted from the token statistics, then the final flag;
//! 3. tail padding and trailing bytes.
//!
//! Tokens are predicted with an exact match finder ([`Matcher`]) limited by the configured
//! search depth and nice length. A match length is coded relative to the predicted length,
//! and a distance as its rank among the closer candidates matching at least that length, so
//! "the closest longest match" costs almost nothing whatever hash the encoder used.

use crate::coder::{Coder, bypass, exp_golomb, tree_bits};
use crate::ctx::{self, Channel, LB, len_bucket};
use crate::deflate::{Block, Body, DynHeader, Stream, Token};
use crate::huffman::{self, CL_ORDER, RleOp};
use crate::matchfind::{MAX_MATCH, Matcher};
use crate::{Error, Result};

/// Search depths selectable by [`Config::depth`].
pub const DEPTHS: [u16; 16] = [
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768,
];
/// Nice lengths selectable by [`Config::nice`].
pub const NICES: [u16; 8] = [8, 16, 24, 32, 64, 128, 192, 258];
/// Candidates walked when ranking a distance before falling back to coding it verbatim.
const RANK_LIMIT: usize = 8192;

/// Parameters of the token predictor, chosen per stream and stored in the metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Config {
    /// Index into [`DEPTHS`].
    pub depth: u8,
    /// Index into [`NICES`].
    pub nice: u8,
}

impl Config {
    pub fn all() -> impl Iterator<Item = Config> {
        (0..DEPTHS.len() as u8)
            .flat_map(|depth| (0..NICES.len() as u8).map(move |nice| Config { depth, nice }))
    }

    pub fn depth_value(self) -> usize {
        DEPTHS[self.depth as usize] as usize
    }

    pub fn nice_value(self) -> usize {
        NICES[self.nice as usize] as usize
    }
}

impl std::fmt::Display for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "depth={},nice={}", self.depth_value(), self.nice_value())
    }
}

impl std::str::FromStr for Config {
    type Err = Error;
    fn from_str(s: &str) -> Result<Config> {
        let mut cfg = Config { depth: 0, nice: 0 };
        let bad = || Error::Param(format!("bad predictor config {s:?}"));
        for kv in s.split(',') {
            let (k, v) = kv.split_once('=').ok_or_else(bad)?;
            let v: u16 = v.parse().map_err(|_| bad())?;
            match k {
                "depth" => cfg.depth = DEPTHS.iter().position(|&d| d == v).ok_or_else(bad)? as u8,
                "nice" => cfg.nice = NICES.iter().position(|&d| d == v).ok_or_else(bad)? as u8,
                _ => return Err(bad()),
            }
        }
        Ok(cfg)
    }
}

/// Codes `actual` (when deconstructing) or rebuilds the stream from decisions (when
/// `actual` is `None`). `cfg` must be given when deconstructing. Returns the stream as coded
/// and the configuration used.
///
/// `matcher` may carry match searches over from earlier passes over the same `data`;
/// without one, a fresh one is used.
pub fn code_stream<C: Coder>(
    c: &mut C,
    data: &[u8],
    actual: Option<&Stream>,
    cfg: Option<Config>,
    matcher: Option<&mut Matcher>,
) -> Result<(Stream, Config)> {
    let cfg0 = cfg.unwrap_or(Config { depth: 0, nice: 0 });
    let cfg = Config {
        depth: bypass(c, Channel::Block, 4, cfg0.depth as u32) as u8,
        nice: bypass(c, Channel::Block, 3, cfg0.nice as u32) as u8,
    };
    let mut own;
    let m = match matcher {
        Some(m) => m,
        None => {
            own = Matcher::new(data);
            &mut own
        }
    };
    let mut p = Predictor {
        data,
        m,
        depth: cfg.depth_value(),
        nice: cfg.nice_value(),
        pos: 0,
        prev_match: false,
    };
    let mut st = BlockState {
        prev_btype: 3,
        prev_count: None,
        prev_eof: false,
        prev_stored: None,
    };
    let mut blocks = Vec::new();
    loop {
        let a = match actual {
            Some(s) => Some(s.blocks.get(blocks.len()).ok_or(Error::Inconsistent)?),
            None => None,
        };
        let block = p.code_block(c, &mut st, a)?;
        let last = block.last;
        blocks.push(block);
        if c.failed() {
            return Err(Error::Corrupt);
        }
        if last {
            break;
        }
    }

    let tail_pad = if c.bit(
        ctx::TAIL_PAD_ZERO.at(0),
        actual.is_none_or(|s| s.tail_pad == 0),
    ) {
        0
    } else {
        bypass(
            c,
            Channel::Block,
            7,
            actual.map_or(0, |s| s.tail_pad as u32),
        ) as u8
    };
    let ntrail = actual.map_or(0, |s| s.trailing.len() as u32);
    let ntrail = exp_golomb(c, ctx::TRAILING_EG, ntrail) as usize;
    if ntrail > 1 << 30 {
        return Err(Error::Corrupt);
    }
    let trailing = (0..ntrail)
        .map(|i| {
            bypass(
                c,
                Channel::Block,
                8,
                actual.map_or(0, |s| s.trailing[i] as u32),
            ) as u8
        })
        .collect();
    if c.failed() {
        return Err(Error::Corrupt);
    }
    Ok((
        Stream {
            blocks,
            tail_pad,
            trailing,
        },
        cfg,
    ))
}

struct BlockState {
    /// 0..=2, or 3 before the first block.
    prev_btype: usize,
    prev_count: Option<u32>,
    prev_eof: bool,
    prev_stored: Option<u16>,
}

struct Predictor<'a> {
    data: &'a [u8],
    m: &'a mut Matcher,
    depth: usize,
    nice: usize,
    pos: usize,
    prev_match: bool,
}

impl Predictor<'_> {
    fn code_block<C: Coder>(
        &mut self,
        c: &mut C,
        st: &mut BlockState,
        a: Option<&Block>,
    ) -> Result<Block> {
        let n = self.data.len();
        let abt = a.map_or(0, |b| b.body.btype() as usize);
        let bt = if c.bit(ctx::BTYPE.at(st.prev_btype * 2), abt == 2) {
            2
        } else if c.bit(ctx::BTYPE.at(st.prev_btype * 2 + 1), abt == 1) {
            1
        } else {
            0
        };
        st.prev_btype = bt;

        let body = if bt == 0 {
            let (apad, alen) = match a.map(|b| &b.body) {
                Some(&Body::Stored { pad, len }) => (pad, len),
                _ => (0, 0),
            };
            let pad = if c.bit(ctx::STORED_PAD_ZERO.at(0), apad == 0) {
                0
            } else {
                bypass(c, Channel::Block, 7, apad as u32) as u8
            };
            let full = (n - self.pos).min(65535) as u16;
            let len = if c.bit(ctx::STORED_LEN_FULL.at(0), alen == full) {
                full
            } else if st.prev_stored.is_some()
                && c.bit(ctx::STORED_LEN_SAME.at(0), Some(alen) == st.prev_stored)
            {
                st.prev_stored.unwrap()
            } else {
                bypass(c, Channel::Block, 16, alen as u32) as u16
            };
            if self.pos + len as usize > n {
                return Err(Error::Corrupt);
            }
            st.prev_stored = Some(len);
            self.pos += len as usize;
            Body::Stored { pad, len }
        } else {
            let atokens = match a.map(|b| &b.body) {
                Some(Body::Fixed { tokens } | Body::Dynamic { tokens, .. }) => Some(tokens),
                _ => None,
            };
            let start = self.pos;
            let a_eof =
                atokens.is_none_or(|t| start + t.iter().map(|t| t.span()).sum::<usize>() == n);
            let eof = c.bit(
                ctx::COUNT_EOF.at((bt - 1) * 2 + st.prev_eof as usize),
                a_eof,
            );
            st.prev_eof = eof;
            let count = if eof {
                None
            } else {
                let ac = atokens.map_or(0, |t| t.len() as u32);
                let same = st.prev_count.is_some()
                    && c.bit(ctx::COUNT_SAME.at(bt - 1), Some(ac) == st.prev_count);
                let count = if same {
                    st.prev_count.unwrap()
                } else {
                    exp_golomb(c, ctx::COUNT_EG, ac)
                };
                st.prev_count = Some(count);
                Some(count as usize)
            };
            let mut tokens = Vec::new();
            loop {
                let more = match count {
                    Some(k) => tokens.len() < k,
                    None => self.pos < n,
                };
                if !more {
                    break;
                }
                if self.pos >= n || c.failed() {
                    return Err(Error::Corrupt);
                }
                let at = atokens.map(|t| t[tokens.len()]);
                tokens.push(self.code_token(c, at)?);
            }
            if bt == 1 {
                Body::Fixed { tokens }
            } else {
                let ah = match a.map(|b| &b.body) {
                    Some(Body::Dynamic { header, .. }) => Some(header),
                    _ => None,
                };
                let pred = self.m.trees(start, &tokens);
                let header = code_header(c, &pred, ah)?;
                Body::Dynamic { header, tokens }
            }
        };

        let last = self.pos == n && c.bit(ctx::FINAL.at(bt), a.is_none_or(|b| b.last));
        Ok(Block { last, body })
    }

    fn code_token<C: Coder>(&mut self, c: &mut C, a: Option<Token>) -> Result<Token> {
        let (p, n, d) = (self.pos, self.data.len(), self.data);
        let f = self.m.longest(d, p, self.depth, self.nice);
        let lp = f.len;
        let lnext = if lp >= 3 && p + 1 < n {
            self.m.longest(d, p + 1, self.depth, self.nice).len
        } else {
            0
        };
        let cmp = if lnext <= lp {
            0
        } else if lnext == lp + 1 {
            1
        } else {
            2
        };
        let far = lp == 3 && f.dist > 4096;
        let bl = len_bucket(lp);
        let ictx = ((bl * 3 + cmp) * 2 + self.prev_match as usize) * 2 + far as usize;
        let (alen, adist, aalt) = match a {
            Some(Token::Match { len, dist, alt258 }) => (len as usize, dist as usize, alt258),
            _ => (0, 0, false),
        };
        let is_match = c.bit(ctx::IS_MATCH.at(ictx), alen > 0);
        self.prev_match = is_match;
        if !is_match {
            self.pos += 1;
            return Ok(Token::Lit(self.data[p]));
        }

        let maxl = (n - p).min(MAX_MATCH);
        let len = if lp >= 3 {
            if c.bit(ctx::LEN_EQ.at(bl * 3 + cmp), alen == lp) {
                lp
            } else if c.bit(ctx::LEN_UP.at(bl), alen > lp) {
                lp + 1 + exp_golomb(c, ctx::LEN_MAG_UP, alen.wrapping_sub(lp + 1) as u32) as usize
            } else {
                let m = exp_golomb(c, ctx::LEN_MAG_DOWN, lp.wrapping_sub(alen + 1) as u32) as usize;
                lp.checked_sub(1 + m).ok_or(Error::Corrupt)?
            }
        } else {
            3 + exp_golomb(c, ctx::LEN_FRESH, alen.wrapping_sub(3) as u32) as usize
        };
        if !(3..=maxl).contains(&len) {
            return Err(Error::Corrupt);
        }
        let alt258 = len == 258 && c.bit(ctx::ALT258.at(0), aalt);

        let arank = if a.is_some() {
            self.m.rank(d, p, len, adist, RANK_LIMIT)
        } else {
            None
        };
        let dist = if c.bit(ctx::RANK_FOUND.at(0), a.is_none() || arank.is_some()) {
            let ar = arank.unwrap_or(0) as u32;
            let k = (len == lp) as usize;
            let r = if c.bit(ctx::RANK_ZERO.at(k * LB + len_bucket(len)), ar == 0) {
                0
            } else {
                1 + exp_golomb(c, ctx::RANK_EG.sub(k, 16), ar.wrapping_sub(1)) as usize
            };
            if a.is_some() {
                // select(rank(dist)) == dist; no need to walk again.
                adist
            } else {
                self.m
                    .select(d, p, len, r, RANK_LIMIT)
                    .ok_or(Error::Corrupt)?
            }
        } else {
            let dd = 1 + bypass(c, Channel::Distance, 15, adist.wrapping_sub(1) as u32) as usize;
            if !Matcher::valid(self.data, p, len, dd) {
                return Err(Error::Corrupt);
            }
            dd
        };
        self.pos += len;
        Ok(Token::Match {
            len: len as u16,
            dist: dist as u16,
            alt258,
        })
    }
}

fn last_nonzero(l: &[u8]) -> usize {
    l.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1)
}

/// Codes a sequence of code lengths against a prediction: one "all as predicted" decision,
/// then per-symbol corrections if needed.
fn code_lens<C: Coder>(
    c: &mut C,
    pred: &[u8],
    actual: Option<&[u8]>,
    all_eq: ctx::Grp,
    eq: ctx::Grp,
    val: ctx::Grp,
    bits: u32,
) -> Vec<u8> {
    if c.bit(all_eq.at(0), actual.is_none_or(|a| a == pred)) {
        return pred.to_vec();
    }
    let mut miss = 0;
    let vals = 1usize << bits;
    pred.iter()
        .enumerate()
        .map(|(i, &q)| {
            let av = actual.map_or(0, |a| a[i]);
            if c.bit(eq.at(q as usize * 2 + miss), av == q) {
                miss = 0;
                q
            } else {
                miss = 1;
                tree_bits(c, val, q as usize * vals, bits, av as u32) as u8
            }
        })
        .collect()
}

/// Predicted literal/length (288) and distance (32) code lengths: zlib's trees over the
/// block's symbol statistics.
pub(crate) fn predict_trees(tokens: &[Token]) -> (Vec<u8>, Vec<u8>) {
    let mut lf = [0u32; 286];
    let mut df = [0u32; 30];
    lf[256] = 1;
    for &t in tokens {
        match t {
            Token::Lit(b) => lf[b as usize] += 1,
            Token::Match { len, dist, alt258 } => {
                lf[257 + if alt258 { 27 } else { huffman::len_code(len) }] += 1;
                df[huffman::dist_code(dist)] += 1;
            }
        }
    }
    let mut pl = huffman::zlib_lens(&lf, 15);
    pl.resize(288, 0);
    let mut pd = huffman::zlib_lens(&df, 15);
    pd.resize(32, 0);
    (pl, pd)
}

fn code_header<C: Coder>(
    c: &mut C,
    (pl, pd): &(Vec<u8>, Vec<u8>),
    a: Option<&DynHeader>,
) -> Result<DynHeader> {
    let pred_hlit = last_nonzero(pl).max(257) as u16;
    let hlit = if c.bit(ctx::HLIT_EQ.at(0), a.is_none_or(|h| h.hlit == pred_hlit)) {
        pred_hlit
    } else {
        257 + bypass(c, Channel::Tree, 5, a.map_or(0, |h| h.hlit as u32 - 257)) as u16
    };
    let pred_hdist = last_nonzero(pd).max(1) as u8;
    let hdist = if c.bit(ctx::HDIST_EQ.at(0), a.is_none_or(|h| h.hdist == pred_hdist)) {
        pred_hdist
    } else {
        1 + bypass(c, Channel::Tree, 5, a.map_or(0, |h| h.hdist as u32 - 1)) as u8
    };
    let pred: Vec<u8> = pl[..hlit as usize]
        .iter()
        .chain(&pd[..hdist as usize])
        .copied()
        .collect();
    let lens = code_lens(
        c,
        &pred,
        a.map(|h| &h.lens[..]),
        ctx::LENS_ALL_EQ,
        ctx::LENS_EQ,
        ctx::LENS_VAL,
        4,
    );
    if lens[256] == 0 {
        return Err(Error::Corrupt);
    }
    huffman::check_lens(&lens[..hlit as usize])?;
    huffman::check_lens(&lens[hlit as usize..])?;

    let ops = code_ops(c, &lens, hlit as usize, a.map(|h| &h.ops[..]))?;

    let mut cf = [0u32; 19];
    for op in &ops {
        cf[op.sym as usize] += 1;
    }
    let pc = huffman::zlib_lens(&cf, 7);
    let pred_hclen = (CL_ORDER.iter().rposition(|&s| pc[s] != 0).unwrap_or(0) + 1).max(4) as u8;
    let hclen = if c.bit(ctx::HCLEN_EQ.at(0), a.is_none_or(|h| h.hclen == pred_hclen)) {
        pred_hclen
    } else {
        4 + bypass(c, Channel::Tree, 4, a.map_or(0, |h| h.hclen as u32 - 4)) as u8
    };
    let order = &CL_ORDER[..hclen as usize];
    let pred: Vec<u8> = order.iter().map(|&s| pc[s]).collect();
    let actual: Option<Vec<u8>> = a.map(|h| order.iter().map(|&s| h.cl_lens[s]).collect());
    let sent = code_lens(
        c,
        &pred,
        actual.as_deref(),
        ctx::CL_ALL_EQ,
        ctx::CL_EQ,
        ctx::CL_VAL,
        3,
    );
    let mut cl_lens = [0u8; 19];
    for (&s, &l) in order.iter().zip(&sent) {
        cl_lens[s] = l;
    }
    huffman::check_lens(&cl_lens)?;
    if ops.iter().any(|op| cl_lens[op.sym as usize] == 0) {
        return Err(Error::Corrupt);
    }
    Ok(DynHeader {
        hlit,
        hdist,
        hclen,
        cl_lens,
        ops,
        lens,
    })
}

/// Codes the run-length encoding of `lens`: first as one of the known encoders' outputs,
/// otherwise op by op.
fn code_ops<C: Coder>(
    c: &mut C,
    lens: &[u8],
    hlit: usize,
    a: Option<&[RleOp]>,
) -> Result<Vec<RleOp>> {
    let mut cands = [Vec::new(), Vec::new(), Vec::new()];
    huffman::rle_zlib(&lens[..hlit], &mut cands[0]);
    huffman::rle_zlib(&lens[hlit..], &mut cands[0]);
    huffman::rle_zlib(lens, &mut cands[1]);
    huffman::rle_libdeflate(lens, &mut cands[2]);
    for (k, cand) in cands.iter().enumerate() {
        if c.bit(ctx::RLE_CAND.at(k), a.is_none_or(|a| a == cand)) {
            return Ok(cands[k].clone());
        }
    }

    let mut ops = Vec::new();
    let mut i = 0;
    let mut prev_type = 0;
    while i < lens.len() {
        if c.failed() {
            return Err(Error::Corrupt);
        }
        let aop = a.map_or(RleOp { sym: 0, extra: 0 }, |a| a[ops.len()]);
        let atype = match aop.sym {
            0..=15 => 0,
            16 => 1,
            17 => 2,
            _ => 3,
        };
        let cur = lens[i];
        let rep_prev = i > 0 && lens[i - 1] == cur;
        let base = (prev_type * 4 + rep_prev as usize * 2 + (cur == 0) as usize) * 3;
        let ty = if c.bit(ctx::RLE_TYPE.at(base), atype == 0) {
            0
        } else if c.bit(ctx::RLE_TYPE.at(base + 1), atype == 1) {
            1
        } else if c.bit(ctx::RLE_TYPE.at(base + 2), atype == 3) {
            3
        } else {
            2
        };
        let op = if ty == 0 {
            RleOp { sym: cur, extra: 0 }
        } else {
            let (sym, value, min, max) = match ty {
                1 => (
                    16,
                    *lens.get(i.wrapping_sub(1)).ok_or(Error::Corrupt)?,
                    3,
                    6,
                ),
                2 => (17, 0, 3, 10),
                _ => (18, 0, 11, 138),
            };
            let run = lens[i..]
                .iter()
                .take_while(|&&l| l == value)
                .count()
                .min(max);
            if run < min {
                return Err(Error::Corrupt);
            }
            let extra = if c.bit(ctx::RLE_MAXRUN.at(ty - 1), aop.extra as usize == run - min) {
                (run - min) as u8
            } else {
                let e = bypass(c, Channel::Tree, RleOp::extra_bits(sym), aop.extra as u32) as usize;
                if e > run - min {
                    return Err(Error::Corrupt);
                }
                e as u8
            };
            RleOp { sym, extra }
        };
        i += op.count();
        ops.push(op);
        prev_type = ty;
    }
    Ok(ops)
}
