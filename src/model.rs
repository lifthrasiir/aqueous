//! Reference probability model and entropy coder for the decision stream.
//!
//! [`ContextModel`] turns a context id into a probability, starting from trained priors and
//! adapting per context; it is what an external CABAC-style coder would use, or replace.
//! [`pack`] / [`unpack`] are a complete reference coding of [`Metadata`] with it (one binary
//! arithmetic coded stream per channel).

use std::sync::OnceLock;

use crate::coder::{Coder, Metadata};
use crate::ctx::{self, Channel, Ctx, NUM_CTX};
use crate::predict::Config;
use crate::{Error, Result};

/// Probabilities are of the bit being 1, in units of 2^-16.
pub const PROB_ONE: u32 = 1 << 16;
const PROB_MIN: u32 = 16;
/// Adaptation slows down as 1/(n+1.5) until it settles at about 1/(LIMIT+1.5).
const LIMIT: u8 = 40;

/// Trained starting state of each context plus the predictor configurations worth trying.
#[derive(Debug, Clone)]
pub struct Priors {
    pub p1: Vec<u16>,
    pub n: Vec<u8>,
    /// Candidate predictor configurations, most useful first.
    pub catalog: Vec<Config>,
    /// Identifies these priors; packed metadata is only decodable with the same priors.
    pub id: u32,
}

impl Default for Priors {
    fn default() -> Self {
        Priors {
            p1: vec![(PROB_ONE / 2) as u16; NUM_CTX],
            n: vec![0; NUM_CTX],
            catalog: vec!["depth=128,nice=128".parse().unwrap()],
            id: 0,
        }
    }
}

fn fnv1a(s: &[u8]) -> u32 {
    s.iter().fold(0x811c9dc5u32, |h, &b| {
        (h ^ b as u32).wrapping_mul(0x01000193)
    })
}

impl Priors {
    /// The priors compiled into the library (`model/priors.txt`).
    pub fn builtin() -> &'static Priors {
        static P: OnceLock<Priors> = OnceLock::new();
        P.get_or_init(|| {
            Priors::parse(include_str!("../model/priors.txt")).expect("bad built-in priors")
        })
    }

    /// Parses the text format written by [`Priors::to_text`]. Contexts that are not
    /// mentioned keep the uninformed default; unknown groups are an error.
    pub fn parse(text: &str) -> Result<Priors> {
        let mut pr = Priors::default();
        let bad = |l: &str| Error::Param(format!("bad priors line {l:?}"));
        let mut catalog = vec![];
        for line in text.lines() {
            let line = line.trim();
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.as_slice() {
                [] => {}
                [c, ..] if c.starts_with('#') => {}
                ["layout", v] => {
                    if v.parse::<u32>().ok() != Some(ctx::LAYOUT) {
                        return Err(Error::Param(format!("priors for context layout {v}")));
                    }
                }
                ["config", spec] => catalog.push(spec.parse()?),
                ["ctx", group, idx, p1, n] => {
                    let g = ctx::GROUPS
                        .iter()
                        .position(|g| g.name == *group)
                        .ok_or_else(|| bad(line))?;
                    let idx: u16 = idx.parse().map_err(|_| bad(line))?;
                    if idx >= ctx::GROUPS[g].size {
                        return Err(bad(line));
                    }
                    let i = (ctx::GROUP_OFFSETS[g] + idx) as usize;
                    pr.p1[i] = p1.parse().map_err(|_| bad(line))?;
                    pr.n[i] = n.parse::<u8>().map_err(|_| bad(line))?.min(LIMIT);
                }
                _ => return Err(bad(line)),
            }
        }
        if !catalog.is_empty() {
            pr.catalog = catalog;
        }
        pr.id = fnv1a(text.as_bytes());
        Ok(pr)
    }

    pub fn to_text(&self) -> String {
        let mut s = format!("# flate-recons priors\nlayout {}\n", ctx::LAYOUT);
        for c in &self.catalog {
            s += &format!("config {c}\n");
        }
        for i in Channel::COUNT..NUM_CTX {
            if self.p1[i] as u32 != PROB_ONE / 2 || self.n[i] != 0 {
                let c = Ctx(i as u16);
                let g = c.group();
                s += &format!(
                    "ctx {} {} {} {}\n",
                    ctx::GROUPS[g].name,
                    i as u16 - ctx::GROUP_OFFSETS[g],
                    self.p1[i],
                    self.n[i]
                );
            }
        }
        s
    }
}

/// Adaptive probability per context.
#[derive(Clone)]
pub struct ContextModel {
    p1: Vec<u16>,
    n: Vec<u8>,
}

#[inline]
fn rate(n: u8) -> u32 {
    // 65536 / (n + 1.5), rounded down.
    const T: [u32; LIMIT as usize + 1] = {
        let mut t = [0; LIMIT as usize + 1];
        let mut n = 0;
        while n <= LIMIT as usize {
            t[n] = 2 * 65536 / (2 * n as u32 + 3);
            n += 1;
        }
        t
    };
    T[n as usize]
}

impl ContextModel {
    pub fn new(priors: &Priors) -> Self {
        ContextModel {
            p1: priors.p1.clone(),
            n: priors.n.clone(),
        }
    }

    /// Probability of `ctx` coding a 1, in (0, 65536).
    #[inline]
    pub fn p1(&self, ctx: Ctx) -> u32 {
        if ctx.is_bypass() {
            PROB_ONE / 2
        } else {
            (self.p1[ctx.0 as usize] as u32).clamp(PROB_MIN, PROB_ONE - PROB_MIN)
        }
    }

    #[inline]
    pub fn update(&mut self, ctx: Ctx, bit: bool) {
        if ctx.is_bypass() {
            return;
        }
        let i = ctx.0 as usize;
        let p = self.p1[i] as i64;
        let target = if bit { PROB_ONE as i64 - 1 } else { 0 };
        self.p1[i] = (p + (((target - p) * rate(self.n[i]) as i64) >> 16)) as u16;
        if self.n[i] < LIMIT {
            self.n[i] += 1;
        }
    }

    /// Cost in bits of coding `bit` now.
    #[inline]
    pub fn cost(&self, ctx: Ctx, bit: bool) -> f64 {
        static COST: OnceLock<Vec<f32>> = OnceLock::new();
        let t = COST.get_or_init(|| {
            (0..=PROB_ONE)
                .map(|p| -(p.max(1) as f64 / PROB_ONE as f64).log2() as f32)
                .collect()
        });
        let p = self.p1(ctx);
        t[if bit { p } else { PROB_ONE - p } as usize] as f64
    }
}

/// A [`Coder`] that only measures: total cost of the decisions per channel under a
/// [`ContextModel`].
pub struct CostCoder {
    pub model: ContextModel,
    pub bits: [f64; Channel::COUNT],
    pub count: [u64; Channel::COUNT],
    total: f64,
    /// Coding is abandoned ([`Coder::failed`]) once the total exceeds this.
    pub budget: f64,
}

impl CostCoder {
    pub fn new(priors: &Priors) -> Self {
        CostCoder {
            model: ContextModel::new(priors),
            bits: [0.0; Channel::COUNT],
            count: [0; Channel::COUNT],
            total: 0.0,
            budget: f64::INFINITY,
        }
    }

    pub fn total(&self) -> f64 {
        self.total
    }
}

impl Coder for CostCoder {
    fn bit(&mut self, ctx: Ctx, bit: bool) -> bool {
        let ch = ctx.channel() as usize;
        let cost = self.model.cost(ctx, bit);
        self.bits[ch] += cost;
        self.total += cost;
        self.count[ch] += 1;
        self.model.update(ctx, bit);
        bit
    }

    fn failed(&self) -> bool {
        self.total > self.budget
    }
}

/// Carry-less binary arithmetic coder (32-bit, after lpaq).
#[derive(Default)]
struct ArithEncoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
    used: bool,
}

impl ArithEncoder {
    fn new() -> Self {
        ArithEncoder {
            x1: 0,
            x2: u32::MAX,
            out: vec![],
            used: false,
        }
    }

    fn encode(&mut self, bit: bool, p1: u32) {
        self.used = true;
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
        }
    }

    /// Ends the stream with a single byte: any value in (x1, x2] decodes correctly, and the
    /// decoder reads zeros past the end, so `(x1 >> 24) + 1` (which the normalization keeps
    /// at most `x2 >> 24`) followed by zeros will do. An unused coder emits nothing.
    fn finish(mut self) -> Vec<u8> {
        if self.used {
            self.out.push((self.x1 >> 24) as u8 + 1);
        }
        self.out
    }
}

struct ArithDecoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    input: &'a [u8],
    pos: usize,
}

impl<'a> ArithDecoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut d = ArithDecoder {
            x1: 0,
            x2: u32::MAX,
            x: 0,
            input,
            pos: 0,
        };
        for _ in 0..4 {
            d.x = d.x << 8 | d.byte() as u32;
        }
        d
    }

    fn byte(&mut self) -> u8 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    fn decode(&mut self, p1: u32) -> bool {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p1 as u64) >> 16) as u32;
        let bit = self.x <= xmid;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xff00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = self.x2 << 8 | 0xff;
            self.x = self.x << 8 | self.byte() as u32;
        }
        bit
    }

    fn overrun(&self) -> bool {
        self.pos > self.input.len() + 4
    }
}

/// Format version, the first byte of packed metadata.
const VERSION: u8 = 3;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(input: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *input.get(*pos).ok_or(Error::Corrupt)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(Error::Corrupt)
}

/// Entropy codes `meta` with the given priors: a header, then one arithmetic coded stream
/// per channel.
pub fn pack(meta: &Metadata, priors: &Priors) -> Vec<u8> {
    let mut p = Packer::new(priors);
    for ch in &meta.channels {
        for d in ch {
            p.bit(d.ctx, d.bit);
        }
    }
    p.finish()
}

/// A streaming [`pack`], usable with [`crate::deconstruct_into`]: no [`Metadata`] is kept.
/// Contexts belong to one channel each, so coding the channels interleaved gives the same
/// streams as coding them one after another.
pub struct Packer {
    model: ContextModel,
    id: u16,
    encoders: [ArithEncoder; Channel::COUNT],
}

impl Packer {
    pub fn new(priors: &Priors) -> Self {
        Packer {
            model: ContextModel::new(priors),
            id: priors.id as u16,
            encoders: std::array::from_fn(|_| ArithEncoder::new()),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = vec![VERSION];
        out.extend_from_slice(&self.id.to_le_bytes());
        let streams = self.encoders.map(ArithEncoder::finish);
        for s in &streams[..streams.len() - 1] {
            put_varint(&mut out, s.len() as u64);
        }
        for s in &streams {
            out.extend_from_slice(s);
        }
        out
    }
}

impl Coder for Packer {
    #[inline]
    fn bit(&mut self, ctx: Ctx, bit: bool) -> bool {
        self.encoders[ctx.channel() as usize].encode(bit, self.model.p1(ctx));
        self.model.update(ctx, bit);
        bit
    }
}

/// A pull decoder over packed metadata, usable with [`crate::reconstruct_with`].
pub struct Unpacker<'a> {
    model: ContextModel,
    decoders: Vec<ArithDecoder<'a>>,
}

impl<'a> Unpacker<'a> {
    pub fn new(packed: &'a [u8], priors: &Priors) -> Result<Self> {
        if packed.len() < 3 || packed[0] != VERSION {
            return Err(Error::Corrupt);
        }
        if packed[1..3] != (priors.id as u16).to_le_bytes() {
            return Err(Error::Param(
                "metadata was packed with different priors".into(),
            ));
        }
        let mut pos = 3;
        let mut lens = vec![];
        for _ in 0..Channel::COUNT - 1 {
            lens.push(get_varint(packed, &mut pos)? as usize);
        }
        let mut decoders = vec![];
        for &l in &lens {
            let s = packed.get(pos..pos + l).ok_or(Error::Corrupt)?;
            decoders.push(ArithDecoder::new(s));
            pos += l;
        }
        decoders.push(ArithDecoder::new(&packed[pos..]));
        Ok(Unpacker {
            model: ContextModel::new(priors),
            decoders,
        })
    }
}

impl Coder for Unpacker<'_> {
    #[inline]
    fn bit(&mut self, ctx: Ctx, _: bool) -> bool {
        let dec = &mut self.decoders[ctx.channel() as usize];
        if ctx.is_bypass() {
            return dec.decode(PROB_ONE / 2);
        }
        let b = dec.decode(self.model.p1(ctx));
        self.model.update(ctx, b);
        b
    }

    fn failed(&self) -> bool {
        self.decoders.iter().any(ArithDecoder::overrun)
    }
}

/// Reconstructs the original DEFLATE stream from the data and [`pack`]ed metadata.
pub fn unpack(data: &[u8], packed: &[u8], priors: &Priors) -> Result<Vec<u8>> {
    unpack_with_threads(data, packed, priors, crate::default_threads())
}

/// Like [`unpack`], on up to `threads` threads (see [`crate::reconstruct_with_threads`]).
pub fn unpack_with_threads(
    data: &[u8],
    packed: &[u8],
    priors: &Priors,
    threads: usize,
) -> Result<Vec<u8>> {
    crate::reconstruct_with_threads(data, &mut Unpacker::new(packed, priors)?, threads)
}
