//! Adaptive coding of the corrections (profiles from [`super::ADAPTIVE`] on): every token gets
//! a hit flag, and a miss's payload fields ([`tokens::payload`]) follow as their bucket
//! symbols ([`code::bucket`]) down binary trees, all binary arithmetic coded under adaptive
//! contexts that start from trained priors. The extra bits of a bucket are coded at
//! probability 1/2.
//!
//! Two predictors take part besides the rule: the match at the next position as the rule's
//! table sees it (for the contexts of a predicted match), and zlib emulation, which predicts
//! the tokens (class 2) for as long as it can follow them.

use super::arith::{ArithDecoder, ArithEncoder};
use super::code;
use super::table::Cands;
use super::tokens::{self, Corr, DIST_IDX, DIST_RANK, DIST_RAW, K_ALT258, LEN_DELTA, LEN_FRESH};
use crate::deflate::Token;
use crate::zlib::Emulator;
use crate::{Error, Result};

/// Classes: no candidate in the rule's table, a candidate, zlib's prediction.
pub const CLASSES: usize = 3;

/// Length buckets: 0 for a literal, then lengths 3, 4, 5, 6-8, 9-16, 17-32, 33-128, 129-257,
/// 258.
const LB: usize = 10;

fn len_bucket(len: usize) -> usize {
    match len {
        0..=2 => 0,
        3 => 1,
        4 => 2,
        5 => 3,
        6..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        33..=128 => 7,
        129..=257 => 8,
        _ => 9,
    }
}

fn pred_len(t: Token) -> usize {
    match t {
        Token::Lit(_) => 0,
        Token::Match { len, .. } => len as usize,
    }
}

/// Nodes of a field's symbol tree.
const TREE: usize = 128;
const TREE_BITS: u32 = 7;
const _: () = assert!(code::ALPHABET <= TREE);
/// Next-position comparisons: not longer, one longer, longer by more.
const CMP: usize = 3;
/// Outcome histories: the class's last two (bit set: miss).
const HIST: usize = 4;

// Context ids by group.
pub const FLAG: usize = 0;
pub const KINDS: usize = FLAG + CLASSES * LB * CMP * HIST;
pub const FRESH: usize = KINDS + CLASSES * LB * CMP * 4;
pub const DELTA: usize = FRESH + CLASSES * TREE;
pub const IDX: usize = DELTA + LB * CMP * TREE;
pub const RANK: usize = IDX + LB * TREE;
pub const RAW: usize = RANK + TREE;
/// Context ids in all.
pub const NUM: usize = RAW + TREE;
// FORMAT.md lists the groups' ids.
const _: () = assert!(KINDS == 360 && FRESH == 720 && DELTA == 1104 && IDX == 4944);
const _: () = assert!(RANK == 6224 && RAW == 6352 && NUM == 6480);

/// Uses of each context counted for training priors.
pub const FIRST: u32 = 16;

/// Probabilities are of a 1, in units of 2^-16.
pub const PROB_ONE: u32 = 1 << 16;
/// Probabilities are clamped to `PROB_MIN..=PROB_ONE - PROB_MIN` when coding.
const PROB_MIN: u32 = 16;
/// Adaptation slows down as 1/(n + 1.5) until it settles at about 1/(LIMIT + 1.5).
pub const LIMIT: u8 = 40;

/// The starting state of every context: its probability of a 1, and how many observations
/// that is worth.
#[derive(Debug, Clone)]
pub struct Priors {
    pub p1: Vec<u16>,
    pub n: Vec<u8>,
}

/// Adaptive probability per context.
#[derive(Clone)]
struct Model {
    p1: Vec<u16>,
    n: Vec<u8>,
}

impl Model {
    fn new(priors: &Priors) -> Self {
        Model {
            p1: priors.p1.clone(),
            n: priors.n.clone(),
        }
    }

    #[inline]
    fn p1(&self, ctx: usize) -> u32 {
        (self.p1[ctx] as u32).clamp(PROB_MIN, PROB_ONE - PROB_MIN)
    }

    #[inline]
    fn update(&mut self, ctx: usize, bit: bool) {
        // 65536 / (n + 1.5), rounded down.
        const RATE: [u32; LIMIT as usize + 1] = {
            let mut t = [0; LIMIT as usize + 1];
            let mut n = 0;
            while n <= LIMIT as usize {
                t[n] = 2 * 65536 / (2 * n as u32 + 3);
                n += 1;
            }
            t
        };
        let p = self.p1[ctx] as i64;
        let target = if bit { PROB_ONE as i64 - 1 } else { 0 };
        self.p1[ctx] = (p + (((target - p) * RATE[self.n[ctx] as usize] as i64) >> 16)) as u16;
        if self.n[ctx] < LIMIT {
            self.n[ctx] += 1;
        }
    }
}

/// Priors from their text form: `ctx ID P1 N` lines (the probability of a 1 in units of
/// 2^-16, and how many observations it is worth); contexts not listed start at 1/2.
pub fn parse_priors(text: &str) -> Result<Priors> {
    let mut p = Priors {
        p1: vec![(PROB_ONE / 2) as u16; NUM],
        n: vec![0; NUM],
    };
    for line in text.lines() {
        let bad = || Error::Param(format!("bad priors line {line:?}"));
        let f: Vec<&str> = line.split_whitespace().collect();
        match f[..] {
            [] => {}
            [c, ..] if c.starts_with('#') => {}
            ["ctx", i, p1, n] => {
                let i: usize = i.parse().map_err(|_| bad())?;
                if !(FLAG..NUM).contains(&i) {
                    return Err(bad());
                }
                p.p1[i] = p1.parse().map_err(|_| bad())?;
                p.n[i] = n.parse::<u8>().map_err(|_| bad())?.min(LIMIT);
            }
            _ => return Err(bad()),
        }
    }
    Ok(p)
}

/// Uninformed priors: every context at 1/2.
pub fn uninformed() -> Priors {
    parse_priors("").unwrap()
}

/// The text form of priors fitted to `seen` (the first uses of each context, summed over
/// training streams), each worth `n` observations.
pub fn priors_text(seen: &[[u32; 2]], n: u8, comment: &str) -> String {
    let mut s = String::new();
    for l in comment.lines() {
        s += &format!("# {l}\n");
    }
    for (i, &[uses, ones]) in seen.iter().enumerate() {
        if uses > 0 {
            let q = (ones as f64 + 0.4) / (uses as f64 + 0.8);
            let p1 = (q * PROB_ONE as f64)
                .round()
                .clamp(1.0, (PROB_ONE - 1) as f64) as u32;
            s += &format!("ctx {i} {p1} {n}\n");
        }
    }
    s
}

/// Where the bits go.
pub trait Backend {
    /// Codes `bit` (ignored when decoding) with probability `p1` of a 1, in units of 2^-16;
    /// returns the bit.
    fn code(&mut self, p1: u32, bit: bool) -> bool;
    fn failed(&self) -> bool {
        false
    }
}

pub struct Enc(ArithEncoder);

impl Enc {
    pub fn new() -> Self {
        Enc(ArithEncoder::new())
    }

    pub fn finish(self) -> Vec<u8> {
        self.0.finish()
    }
}

impl Default for Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for Enc {
    #[inline]
    fn code(&mut self, p1: u32, bit: bool) -> bool {
        self.0.encode(bit, p1);
        bit
    }
}

pub struct Dec<'a>(ArithDecoder<'a>);

impl<'a> Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Dec(ArithDecoder::new(buf))
    }
}

impl Backend for Dec<'_> {
    #[inline]
    fn code(&mut self, p1: u32, _: bool) -> bool {
        self.0.decode(p1)
    }

    fn failed(&self) -> bool {
        self.0.overrun()
    }
}

/// Measures the cost in bits instead (deconstruction only).
#[derive(Default)]
pub struct Cost(pub f64);

impl Backend for Cost {
    #[inline]
    fn code(&mut self, p1: u32, bit: bool) -> bool {
        let p = if bit { p1 } else { PROB_ONE - p1 };
        self.0 -= (p as f64 / PROB_ONE as f64).log2();
        bit
    }
}

/// The corrections of a range of tokens, coded both ways ([`Corr`]): the actual tokens are
/// coded when given (deconstructing), decoded otherwise.
///
/// The hit flag's context is the class, the predicted length's bucket, the next-position
/// comparison (for a match the rule predicts) and the class's last two outcomes. The kind
/// takes the same but the outcomes, a length delta the bucket and comparison, a fresh
/// length the class, a bucket entry the actual length's bucket; ranks and verbatim
/// distances have one tree each.
pub struct Adaptive<'a, B> {
    pub b: B,
    model: Model,
    hist: [usize; CLASSES],
    /// What [`Corr::next_len`] gave for the current token.
    next: usize,
    emu: Option<Emulator<'a>>,
    /// The payload fields of the current miss, when deconstructing.
    fields: [(usize, u32); 5],
    nfields: usize,
    /// The miss's length as coded so far (for the distance's context).
    alen: usize,
    pub misses: usize,
    /// For training priors: per context, uses among the first [`FIRST`] and ones among them.
    pub seen: Option<Vec<[u32; 2]>>,
}

impl<'a, B: Backend> Adaptive<'a, B> {
    /// Starting from `priors` (of [`NUM`] contexts), with zlib emulation `emu` if any.
    pub fn new(b: B, priors: &Priors, emu: Option<Emulator<'a>>) -> Self {
        debug_assert_eq!(priors.p1.len(), NUM);
        Adaptive {
            b,
            model: Model::new(priors),
            hist: [0; CLASSES],
            next: 0,
            emu,
            fields: [(0, 0); 5],
            nfields: 0,
            alen: 0,
            misses: 0,
            seen: None,
        }
    }

    /// The emulation after the range, if it still follows the tokens.
    pub fn take_emulator(&mut self) -> Option<Emulator<'a>> {
        self.emu.take()
    }

    #[inline]
    fn bit(&mut self, ctx: usize, bit: bool) -> bool {
        let b = self.b.code(self.model.p1(ctx), bit);
        self.model.update(ctx, b);
        if let Some(seen) = &mut self.seen {
            let s = &mut seen[ctx];
            if s[0] < FIRST {
                s[0] += 1;
                s[1] += b as u32;
            }
        }
        b
    }

    fn tree(&mut self, base: usize, nbits: u32, sym: usize) -> usize {
        let mut node = 1;
        for i in (0..nbits).rev() {
            let b = self.bit(base + node, sym >> i & 1 != 0);
            node = node << 1 | b as usize;
        }
        node - (1 << nbits)
    }

    /// Codes field `f` (value `v` when deconstructing) of a miss of `class`, where `pred` was
    /// predicted and the next position compared as `cmp`.
    fn field(&mut self, f: usize, v: u32, class: usize, pred: Token, cmp: usize) -> Result<u32> {
        let pl = pred_len(pred);
        let pb = len_bucket(pl);
        let base = match f {
            LEN_FRESH => FRESH + class * TREE,
            LEN_DELTA => DELTA + (pb * CMP + cmp) * TREE,
            DIST_IDX => IDX + len_bucket(self.alen) * TREE,
            DIST_RANK => RANK,
            DIST_RAW => RAW,
            _ => {
                // The kind (field KIND or KIND + 1).
                let k = (class * LB + pb) * CMP + cmp;
                let kind = self.tree(KINDS + k * 4, 2, v as usize) as u32;
                if kind == K_ALT258 {
                    self.alen = 258;
                }
                return Ok(kind);
            }
        };
        let (sym, _, x) = code::bucket(v);
        let sym = self.tree(base, TREE_BITS, sym);
        if sym >= code::ALPHABET {
            return Err(Error::Corrupt);
        }
        let mut xv = 0;
        for i in (0..code::extra_bits(sym)).rev() {
            let b = self.b.code(PROB_ONE / 2, x >> i & 1 != 0);
            xv = xv << 1 | b as u32;
        }
        let v = code::unbucket(sym, xv);
        match f {
            LEN_FRESH => self.alen = 3 + v as usize,
            LEN_DELTA => self.alen = (pl as i64 + tokens::unzigzag(v) as i64).max(0) as usize,
            _ => {}
        }
        Ok(v)
    }
}

impl<B: Backend> Corr for Adaptive<'_, B> {
    #[inline(always)]
    fn predict(&mut self, _: &[u8], p: usize) -> Option<Token> {
        let e = self.emu.as_mut()?;
        match e.predict() {
            Some((at, t)) if at == p => Some(t),
            // The emulation lost track; the rule carries on.
            _ => {
                self.emu = None;
                None
            }
        }
    }

    #[inline(always)]
    fn commit(&mut self, _: usize, tok: Token) {
        if let Some(e) = &mut self.emu
            && !e.commit(tok)
        {
            self.emu = None;
        }
    }

    fn skip(&mut self, end: usize) {
        if let Some(e) = &mut self.emu
            && !e.skip_to(end)
        {
            self.emu = None;
        }
    }

    #[inline(always)]
    fn lookahead(&self) -> bool {
        true
    }

    #[inline(always)]
    fn next_len(&mut self, len: usize) {
        self.next = len;
    }

    fn token(
        &mut self,
        t: &mut dyn Cands,
        d: &[u8],
        p: usize,
        pred: Token,
        class: usize,
        actual: Option<Token>,
    ) -> Result<Token> {
        let pl = pred_len(pred);
        let cmp = if class == 1 && pl > 0 {
            (self.next > pl) as usize + (self.next > pl + 1) as usize
        } else {
            0
        };
        self.next = 0;
        let ctx = FLAG + ((class * LB + len_bucket(pl)) * CMP + cmp) * HIST + self.hist[class];
        let miss = self.bit(ctx, actual.is_some_and(|a| a != pred));
        self.hist[class] = (self.hist[class] << 1 | miss as usize) & (HIST - 1);
        if !miss {
            return Ok(pred);
        }
        self.misses += 1;
        // The payload is that of the rule's classes; zlib's predictions take class 1's.
        let pc = class.min(1);
        self.nfields = 0;
        if let Some(a) = actual {
            let (fields, n) = (&mut self.fields, &mut self.nfields);
            tokens::payload(t, d, p, pred, pc, a, |f, v| {
                fields[*n] = (f, v);
                *n += 1;
            });
        }
        self.alen = 0;
        let mut k = 0;
        let tok = tokens::read_miss(t, d, p, pred, pc, |f| {
            let v = match actual {
                Some(_) => {
                    let (af, v) = *self.fields[..self.nfields]
                        .get(k)
                        .ok_or(Error::Inconsistent)?;
                    if af != f {
                        return Err(Error::Inconsistent);
                    }
                    k += 1;
                    v
                }
                None => 0,
            };
            self.field(f, v, class, pred, cmp)
        })?;
        if self.b.failed() {
            return Err(Error::Corrupt);
        }
        match actual {
            Some(a) if a != tok => Err(Error::Inconsistent),
            _ => Ok(tok),
        }
    }
}
