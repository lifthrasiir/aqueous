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
use crate::ctx::{self, Channel, Ctx, LB, len_bucket};
use crate::deflate::{Block, Body, DynHeader, Stream, Token};
use crate::huffman::{self, CL_ORDER, RleOp};
use crate::matchfind::{MAX_MATCH, Matcher};
use crate::zlib;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Config {
    /// Index into [`DEPTHS`].
    pub depth: u8,
    /// Index into [`NICES`].
    pub nice: u8,
    /// Predict tokens and block sizes by emulating zlib with these parameters first; the
    /// generic predictor above only codes the tokens it gets wrong.
    pub zlib: Option<zlib::Params>,
}

impl Config {
    pub fn all() -> impl Iterator<Item = Config> {
        (0..DEPTHS.len() as u8).flat_map(|depth| {
            (0..NICES.len() as u8).map(move |nice| Config {
                depth,
                nice,
                zlib: None,
            })
        })
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
        write!(f, "depth={},nice={}", self.depth_value(), self.nice_value())?;
        if let Some(z) = self.zlib {
            write!(f, ";{z}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Config {
    type Err = Error;
    fn from_str(s: &str) -> Result<Config> {
        let mut cfg = Config {
            depth: 0,
            nice: 0,
            zlib: None,
        };
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

/// What [`code_stream`] coded.
#[derive(Debug)]
pub struct Coded {
    pub stream: Stream,
    pub config: Config,
    /// Leading blocks reproduced exactly by zlib emulation (zlib mode only).
    pub exact_blocks: usize,
    /// Checkpoints within those blocks, from which they are regenerated in parallel.
    pub checkpoints: usize,
}

/// Codes `actual` (when deconstructing) or rebuilds the stream from decisions (when
/// `actual` is `None`). `cfg` must be given when deconstructing. Returns the stream as coded
/// and the configuration used.
///
/// `matcher` may carry match searches over from earlier passes over the same `data`; it is
/// only built when the generic predictor is needed. Reconstruction regenerates the blocks
/// zlib reproduces exactly on up to `threads` threads.
pub fn code_stream<C: Coder>(
    c: &mut C,
    data: &[u8],
    actual: Option<&Stream>,
    cfg: Option<Config>,
    matcher: &mut Option<Matcher>,
    threads: usize,
) -> Result<Coded> {
    let cfg0 = cfg.unwrap_or(Config {
        depth: 0,
        nice: 0,
        zlib: None,
    });
    let depth = bypass(c, Channel::Block, 4, cfg0.depth as u32) as u8;
    let nice = bypass(c, Channel::Block, 3, cfg0.nice as u32) as u8;
    let z0 = cfg0.zlib.unwrap_or_default();
    let zlib = if bypass(c, Channel::Block, 1, cfg0.zlib.is_some() as u32) == 1 {
        let level = bypass(c, Channel::Block, 4, z0.level as u32) as u8;
        let strategy = bypass(c, Channel::Block, 3, z0.strategy as u32) as usize;
        let wbits = 9 + bypass(c, Channel::Block, 3, z0.wbits as u32 - 9) as u8;
        let mem = bypass(c, Channel::Block, 4, z0.mem as u32) as u8;
        let p = zlib::Params {
            level,
            strategy: *zlib::Strategy::ALL.get(strategy).ok_or(Error::Corrupt)?,
            wbits,
            mem,
        };
        if !(1..=9).contains(&level) || !(1..=9).contains(&mem) {
            return Err(Error::Corrupt);
        }
        Some(p)
    } else {
        None
    };
    let cfg = Config { depth, nice, zlib };
    let mut p = Predictor {
        data,
        deconstructing: actual.is_some(),
        m: matcher,
        depth: cfg.depth_value(),
        nice: cfg.nice_value(),
        pos: 0,
        prev_match: false,
        emu: zlib.map(|p| zlib::Emulator::new(data, p)),
        zmiss: false,
        zrule: false,
    };
    let mut st = BlockState {
        prev_btype: 3,
        prev_count: None,
        prev_eof: false,
        prev_stored: None,
    };
    let mut blocks = Vec::new();
    let mut exact_blocks = 0;
    let mut checkpoints = 0;
    if let Some(mut e) = p.emu.take() {
        // The leading blocks that zlib reproduces exactly cost nothing but their number, plus
        // checkpoints in long streams so that they can be regenerated in parallel.
        let mut generated = vec![];
        let mut cps = vec![];
        let mut pos = 0;
        if let Some(s) = actual {
            let spacing = checkpoint_spacing(data.len());
            let mut next = spacing.unwrap_or(usize::MAX);
            for b in &s.blocks {
                let snapshot = e.clone();
                match zlib_block(&mut e, pos) {
                    Some(g) if g == *b => {
                        pos += block_span(&g);
                        generated.push(g);
                        if let Some(spacing) = spacing
                            && pos >= next
                            && let Some(cp) = e.checkpoint()
                        {
                            cps.push((generated.len(), cp));
                            next = pos + spacing;
                        }
                    }
                    _ => {
                        e = snapshot;
                        break;
                    }
                }
            }
            // One right before the first block zlib gets wrong would start an empty segment.
            cps.retain(|&(i, _)| i < generated.len());
        }
        let k = generated.len() as u32;
        let all = actual.is_some_and(|s| generated.len() == s.blocks.len());
        let k = if c.bit(ctx::Z_ALL_EXACT.at(0), all) {
            None
        } else {
            Some(exp_golomb(c, ctx::Z_EXACT_EG, k) as usize)
        };
        let cps = code_checkpoints(c, data.len(), e.params().wbits, &cps)?;
        if actual.is_none() {
            if c.failed() {
                return Err(Error::Corrupt);
            }
            (generated, e) = regenerate_exact(data, e, k, &cps, threads)?;
            pos = generated.iter().map(block_span).sum();
        }
        p.emu = Some(e);
        p.pos = pos;
        if let Some(b) = generated.last() {
            st.prev_btype = b.body.btype() as usize;
        }
        exact_blocks = generated.len();
        checkpoints = cps.len();
        blocks = generated;
    }
    while !blocks.last().is_some_and(|b: &Block| b.last) {
        let a = match actual {
            Some(s) => Some(s.blocks.get(blocks.len()).ok_or(Error::Inconsistent)?),
            None => None,
        };
        let block = p.code_block(c, &mut st, a)?;
        blocks.push(block);
        if c.failed() {
            return Err(Error::Corrupt);
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
    Ok(Coded {
        stream: Stream {
            blocks,
            tail_pad,
            trailing,
        },
        config: cfg,
        exact_blocks,
        checkpoints,
    })
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
    deconstructing: bool,
    m: &'a mut Option<Matcher>,
    depth: usize,
    nice: usize,
    pos: usize,
    prev_match: bool,
    /// zlib emulation, dropped if it ever loses track of the stream.
    emu: Option<zlib::Emulator<'a>>,
    /// Whether the previous token was mispredicted by the emulation.
    zmiss: bool,
    /// Whether the previous block followed zlib's block size rule.
    zrule: bool,
}

impl Predictor<'_> {
    fn m(&mut self) -> &mut Matcher {
        let data = self.data;
        // Deconstruction codes the same data under several configurations, so searches are
        // cached; reconstruction makes a single pass.
        let cached = self.deconstructing;
        self.m.get_or_insert_with(|| {
            if cached {
                Matcher::new(data)
            } else {
                Matcher::single(data)
            }
        })
    }

    fn code_block<C: Coder>(
        &mut self,
        c: &mut C,
        st: &mut BlockState,
        a: Option<&Block>,
    ) -> Result<Block> {
        let n = self.data.len();
        let abt = a.map_or(0, |b| b.body.btype() as usize);
        // With zlib emulation, only "stored or not" is coded up front; fixed versus dynamic
        // follows the tokens, predicted by zlib's own cost comparison.
        let zl = self.emu.is_some();
        let mut bt = if zl {
            if c.bit(ctx::Z_STORED.at(st.prev_btype), abt == 0) {
                0
            } else {
                2
            }
        } else if c.bit(ctx::BTYPE.at(st.prev_btype * 2), abt == 2) {
            2
        } else if c.bit(ctx::BTYPE.at(st.prev_btype * 2 + 1), abt == 1) {
            1
        } else {
            0
        };

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
            // zlib stores what its next block of tokens covers: let a copy run ahead.
            let ahead = self.emu.as_ref().map(|e| {
                let mut e = e.clone();
                let mut end = self.pos;
                for _ in 0..e.params().block_tokens() {
                    match e.predict() {
                        Some((pp, t)) if e.commit(t) => end = pp + t.span(),
                        _ => break,
                    }
                }
                (e, end - self.pos)
            });
            let full = (n - self.pos).min(65535) as u16;
            let zlen = ahead.as_ref().map(|(_, l)| *l).filter(|&l| l <= 65535);
            let len = if let Some(zlen) = zlen
                && c.bit(ctx::Z_STORED_LEN.at(0), alen as usize == zlen)
            {
                self.emu = ahead.map(|(e, _)| e);
                self.pos += zlen;
                st.prev_stored = Some(zlen as u16);
                return self.finish_block(
                    c,
                    st,
                    a,
                    0,
                    Body::Stored {
                        pad,
                        len: zlen as u16,
                    },
                );
            } else if c.bit(ctx::STORED_LEN_FULL.at(0), alen == full) {
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
            if let Some(e) = &mut self.emu
                && !e.skip_to(self.pos)
            {
                self.emu = None;
            }
            Body::Stored { pad, len }
        } else {
            let atokens = match a.map(|b| &b.body) {
                Some(Body::Fixed { tokens } | Body::Dynamic { tokens, .. }) => Some(tokens),
                _ => None,
            };
            let start = self.pos;
            let a_eof =
                atokens.is_none_or(|t| start + t.iter().map(|t| t.span()).sum::<usize>() == n);
            // zlib flushes a block after lit_bufsize - 1 tokens, or at the end of the data.
            let zlimit = self.emu.as_ref().map(|e| e.params().block_tokens());
            let cbt = if zl { 2 } else { bt };
            let rule = match zlimit {
                Some(lim) => {
                    let a_rule = atokens.is_none_or(|t| t.len() == lim || (t.len() < lim && a_eof));
                    let r = c.bit(ctx::Z_COUNT.at(self.zrule as usize), a_rule);
                    self.zrule = r;
                    r
                }
                None => false,
            };
            let eof = !rule
                && c.bit(
                    ctx::COUNT_EOF.at((cbt - 1) * 2 + st.prev_eof as usize),
                    a_eof,
                );
            st.prev_eof = eof;
            let count = if rule {
                zlimit
            } else if eof {
                None
            } else {
                let ac = atokens.map_or(0, |t| t.len() as u32);
                let same = st.prev_count.is_some()
                    && c.bit(ctx::COUNT_SAME.at(cbt - 1), Some(ac) == st.prev_count);
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
                    Some(k) if rule => tokens.len() < k && self.pos < n,
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
            if zl {
                let fixed = zlib_prefers_fixed(&tokens);
                bt = if c.bit(ctx::Z_DYNAMIC.at(fixed as usize), abt == 2) {
                    2
                } else {
                    1
                };
            }
            if bt == 1 {
                Body::Fixed { tokens }
            } else {
                let ah = match a.map(|b| &b.body) {
                    Some(Body::Dynamic { header, .. }) => Some(header),
                    _ => None,
                };
                let pred = self.m().trees(start, &tokens);
                let header = code_header(c, &pred, ah)?;
                Body::Dynamic { header, tokens }
            }
        };

        self.finish_block(c, st, a, bt, body)
    }

    fn finish_block<C: Coder>(
        &mut self,
        c: &mut C,
        st: &mut BlockState,
        a: Option<&Block>,
        bt: usize,
        body: Body,
    ) -> Result<Block> {
        st.prev_btype = bt;
        let last =
            self.pos == self.data.len() && c.bit(ctx::FINAL.at(bt), a.is_none_or(|b| b.last));
        Ok(Block { last, body })
    }

    fn code_token<C: Coder>(&mut self, c: &mut C, a: Option<Token>) -> Result<Token> {
        let Some(mut e) = self.emu.take() else {
            return self.code_token_generic(c, a);
        };
        let Some((pp, pt)) = e.predict().filter(|&(pp, _)| pp == self.pos) else {
            // The emulation lost track; carry on without it.
            return self.code_token_generic(c, a);
        };
        let (kind, plen) = match pt {
            Token::Lit(_) => (0, 0),
            Token::Match { len, .. } => (1, len as usize),
        };
        let zctx = (kind * 2 + self.zmiss as usize) * LB + len_bucket(plen);
        let t = if c.bit(ctx::Z_HIT.at(zctx), a.is_none_or(|a| a == pt)) {
            self.prev_match = kind == 1;
            self.pos = pp + pt.span();
            self.zmiss = false;
            pt
        } else {
            self.zmiss = true;
            self.code_token_generic(c, a)?
        };
        if e.commit(t) {
            self.emu = Some(e);
        }
        Ok(t)
    }

    fn code_token_generic<C: Coder>(&mut self, c: &mut C, a: Option<Token>) -> Result<Token> {
        let (p, n, d) = (self.pos, self.data.len(), self.data);
        let (depth, nice) = (self.depth, self.nice);
        let f = self.m().longest(d, p, depth, nice);
        let lp = f.len;
        let lnext = if lp >= 3 && p + 1 < n {
            self.m().longest(d, p + 1, depth, nice).len
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
            self.m().rank(d, p, len, adist, RANK_LIMIT)
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
                self.m()
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
pub fn predict_trees(tokens: &[Token]) -> (Vec<u8>, Vec<u8>) {
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

/// The dynamic block header zlib writes for `tokens`: every header decision taken as
/// predicted.
pub fn zlib_header(tokens: &[Token]) -> DynHeader {
    struct Yes;
    impl Coder for Yes {
        fn bit(&mut self, _: crate::ctx::Ctx, _: bool) -> bool {
            true
        }
    }
    code_header(&mut Yes, &predict_trees(tokens), None).expect("zlib's own header is valid")
}

/// Whether zlib's `_tr_flush_block` would rather use fixed than dynamic codes for `tokens`
/// (Z_FIXED aside): fixed when `static_lenb <= opt_lenb`.
pub fn zlib_prefers_fixed(tokens: &[Token]) -> bool {
    let (opt, stat) = zlib_lenb(tokens, &zlib_header(tokens));
    stat <= opt
}

/// zlib's `opt_lenb` and `static_lenb`: the sizes in bytes of the block with dynamic and with
/// fixed codes, as `_tr_flush_block` computes them.
fn zlib_lenb(tokens: &[Token], h: &DynHeader) -> (i64, i64) {
    let mut lf = [0u64; 288];
    let mut df = [0u64; 32];
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
    let fixed_lit = huffman::fixed_lit_lens();
    let (mut opt, mut stat) = (0i64, 0i64);
    // gen_bitlen's accounting; symbols zlib forced into a tree count with frequency 1 and
    // get one bit (and their static length) taken off again.
    let mut add = |freq: u64, len: u8, slen: Option<u8>, xbits: u8| {
        if len == 0 {
            return;
        }
        let f = freq.max(1) as i64;
        opt += f * (len + xbits) as i64;
        if let Some(sl) = slen {
            stat += f * (sl + xbits) as i64;
        }
        if freq == 0 {
            opt -= 1;
            if let Some(sl) = slen {
                stat -= sl as i64;
            }
        }
    };
    for (n, &l) in h.lit_lens().iter().enumerate() {
        let x = if n >= 257 {
            huffman::LEN_EXTRA.get(n - 257).copied().unwrap_or(0)
        } else {
            0
        };
        add(lf[n], l, Some(fixed_lit[n]), x);
    }
    for (n, &l) in h.dist_lens().iter().enumerate() {
        add(
            df[n],
            l,
            Some(5),
            huffman::DIST_EXTRA.get(n).copied().unwrap_or(0),
        );
    }
    let mut cf = [0u64; 19];
    for op in &h.ops {
        cf[op.sym as usize] += 1;
    }
    for (n, &l) in h.cl_lens.iter().enumerate() {
        add(cf[n], l, None, RleOp::extra_bits(n as u8) as u8);
    }
    opt += 3 * h.hclen as i64 + 14;
    ((opt + 3 + 7) >> 3, (stat + 3 + 7) >> 3)
}

/// The next block exactly as zlib writes it: its tokens by emulation (up to `lit_bufsize - 1`
/// of them, starting at `start`), then `_tr_flush_block`'s choice between stored, fixed and
/// dynamic. `None` if the emulation cannot continue.
pub fn zlib_block(e: &mut zlib::Emulator, start: usize) -> Option<Block> {
    let limit = e.params().block_tokens();
    let mut tokens = vec![];
    let mut end = start;
    let last = loop {
        if tokens.len() == limit {
            break false;
        }
        match e.predict() {
            None => break true,
            Some((pos, t)) => {
                if pos != end || !e.commit(t) {
                    return None;
                }
                end += t.span();
                tokens.push(t);
            }
        }
    };
    let header = zlib_header(&tokens);
    let (opt_lenb, static_lenb) = zlib_lenb(&tokens, &header);
    let mut opt = opt_lenb;
    if static_lenb <= opt || e.params().strategy == zlib::Strategy::Fixed {
        opt = static_lenb;
    }
    let stored_len = (end - start) as i64;
    // The data must still be in zlib's window to be stored.
    let body = if stored_len + 4 <= opt && start >= e.window_base() && stored_len <= 65535 {
        Body::Stored {
            pad: 0,
            len: stored_len as u16,
        }
    } else if static_lenb == opt {
        Body::Fixed { tokens }
    } else {
        Body::Dynamic { header, tokens }
    };
    Some(Block { last, body })
}

/// Checkpoints of the exact blocks are at least this far apart (bytes of data)...
const CHECKPOINT_SPACING: usize = 256 << 10;
/// ...and about this many at most.
const MAX_CHECKPOINTS: usize = 32;

/// How far apart checkpoints are for `n` bytes of data, or `None` if there are none.
fn checkpoint_spacing(n: usize) -> Option<usize> {
    (n >= 4 * CHECKPOINT_SPACING).then(|| (n / MAX_CHECKPOINTS).max(CHECKPOINT_SPACING))
}

/// Elias gamma code of `v` in bypass bits of the block channel.
fn gamma<C: Coder>(c: &mut C, v: u64) -> Result<u64> {
    let v1 = v + 1;
    let nb = 63 - v1.leading_zeros();
    let mut k = 0;
    while c.bit(Ctx::bypass(Channel::Block), k < nb) {
        k += 1;
        if k > 48 {
            return Err(Error::Corrupt);
        }
    }
    let mut x = 1u64;
    for i in (0..k).rev() {
        x = x << 1 | c.bit(Ctx::bypass(Channel::Block), v1 >> i & 1 != 0) as u64;
    }
    Ok(x - 1)
}

/// Codes the checkpoints of the exact blocks (for streams long enough to have them): the
/// index of the block each is taken before, and the emulator's state there. Every field is
/// coded relative to what it normally is.
fn code_checkpoints<C: Coder>(
    c: &mut C,
    n: usize,
    wbits: u8,
    actual: &[(usize, zlib::Checkpoint)],
) -> Result<Vec<(usize, zlib::Checkpoint)>> {
    if checkpoint_spacing(n).is_none() {
        return Ok(vec![]);
    }
    let count = gamma(c, actual.len() as u64)? as usize;
    if count > n / CHECKPOINT_SPACING {
        return Err(Error::Corrupt);
    }
    let w = 1u64 << wbits;
    let mut out = Vec::with_capacity(count);
    let (mut idx, mut strstart) = (0u64, 0u64);
    for i in 0..count {
        // The coded values, when deconstructing.
        let v = actual.get(i).map_or([0; 7], |&(j, cp)| {
            let (s, b) = (cp.strstart as u64, cp.base as u64);
            [
                (j as u64).wrapping_sub(idx + 1),
                s.wrapping_sub(strstart),
                (s / w).wrapping_sub(b / w),
                (b + 2 * w).wrapping_sub(cp.read as u64),
                (2 * w).wrapping_sub(cp.high_water as u64),
                (cp.match_length as u64).wrapping_sub(2),
                s.wrapping_sub(cp.match_start as u64),
            ]
        });
        idx += 1 + gamma(c, v[0])?;
        strstart += gamma(c, v[1])?;
        let base = (strstart / w)
            .checked_sub(gamma(c, v[2])?)
            .ok_or(Error::Corrupt)?
            * w;
        let read = (base + 2 * w)
            .checked_sub(gamma(c, v[3])?)
            .ok_or(Error::Corrupt)?;
        let high_water = (2 * w).checked_sub(gamma(c, v[4])?).ok_or(Error::Corrupt)?;
        let match_available = c.bit(
            Ctx::bypass(Channel::Block),
            actual.get(i).is_some_and(|(_, cp)| cp.match_available),
        );
        let match_length = 2 + gamma(c, v[5])?;
        let match_start = if match_length > 2 {
            strstart
                .checked_sub(gamma(c, v[6])?)
                .ok_or(Error::Corrupt)?
        } else {
            0
        };
        let usz = |x: u64| usize::try_from(x).map_err(|_| Error::Corrupt);
        let cp = zlib::Checkpoint {
            strstart: usz(strstart)?,
            base: usz(base)?,
            read: usz(read)?,
            high_water: usz(high_water)?,
            match_available,
            match_length: usz(match_length)?,
            match_start: usz(match_start)?,
        };
        if actual
            .get(i)
            .is_some_and(|&(j, acp)| j as u64 != idx || acp != cp)
        {
            return Err(Error::Inconsistent);
        }
        out.push((usz(idx)?, cp));
    }
    Ok(out)
}

/// The blocks zlib reproduces exactly at the start of the stream (`k` of them, or up to the
/// last one), generated by `e`, and the emulator after them. With checkpoints, the segments
/// between them are generated on up to `threads` threads, each checked to end in the state
/// the next one starts from.
fn regenerate_exact<'a>(
    data: &'a [u8],
    e: zlib::Emulator<'a>,
    k: Option<usize>,
    cps: &[(usize, zlib::Checkpoint)],
    threads: usize,
) -> Result<(Vec<Block>, zlib::Emulator<'a>)> {
    if cps.is_empty() || threads <= 1 {
        // One pass, checking the checkpoints on the way.
        return exact_segment(e, 0, 0, k, cps);
    }
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let params = e.params();
    let n = cps.len() + 1;
    let first = Mutex::new(Some(e));
    let results: Vec<_> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = (|| {
                        let (e, idx, pos) = match i.checked_sub(1).map(|j| cps[j]) {
                            None => (first.lock().unwrap().take().unwrap(), 0, 0),
                            Some((j, cp)) => (
                                zlib::Emulator::resume(data, params, &cp).ok_or(Error::Corrupt)?,
                                j,
                                cp.strstart - cp.match_available as usize,
                            ),
                        };
                        let end = cps.get(i).map(|&(j, _)| j).or(k);
                        exact_segment(e, idx, pos, end, &cps[i.min(n - 1)..(i + 1).min(n - 1)])
                    })();
                    *results[i].lock().unwrap() = Some(r);
                }
            });
        }
    });
    let mut blocks = vec![];
    let mut last = None;
    for r in results {
        let (b, e) = r.into_inner().unwrap().expect("every segment runs")?;
        blocks.extend(b);
        last = Some(e);
    }
    Ok((blocks, last.expect("at least one segment")))
}

/// Generates exact blocks with `e` from block `idx` at data position `pos`, up to block `end`
/// (or the last one), checking that `e` is in the state of each of `cps` when it gets there.
fn exact_segment<'a>(
    mut e: zlib::Emulator<'a>,
    mut idx: usize,
    mut pos: usize,
    end: Option<usize>,
    cps: &[(usize, zlib::Checkpoint)],
) -> Result<(Vec<Block>, zlib::Emulator<'a>)> {
    let mut cps = cps.iter().peekable();
    let mut blocks = vec![];
    loop {
        if let Some(&&(j, cp)) = cps.peek()
            && j == idx
        {
            if e.checkpoint() != Some(cp) {
                return Err(Error::Corrupt);
            }
            cps.next();
        }
        if end.is_some_and(|end| idx >= end) {
            break;
        }
        let g = zlib_block(&mut e, pos).ok_or(Error::Corrupt)?;
        pos += block_span(&g);
        idx += 1;
        let last = g.last;
        blocks.push(g);
        if last {
            break;
        }
    }
    // A checkpoint past the end, or the stream ending before the segment does.
    if cps.next().is_some() || end.is_some_and(|end| idx != end) {
        return Err(Error::Corrupt);
    }
    Ok((blocks, e))
}

/// Bytes of data a block covers.
fn block_span(b: &Block) -> usize {
    match &b.body {
        Body::Stored { len, .. } => *len as usize,
        Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
            tokens.iter().map(|t| t.span()).sum()
        }
    }
}
