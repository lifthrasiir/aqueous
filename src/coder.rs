//! The decision stream: the interface between the predictor and whatever entropy codes it.
//!
//! The predictor is written once against [`Coder`]: `x = c.bit(ctx, x)`. When deconstructing,
//! `x` is the actual value and is passed through (and recorded); when reconstructing, the
//! argument is a placeholder and the decoded value comes back. This keeps both directions
//! in lockstep by construction.

use crate::ctx::{Channel, Ctx, Grp};

pub trait Coder {
    /// Codes one binary decision under `ctx`.
    fn bit(&mut self, ctx: Ctx, bit: bool) -> bool;

    /// Whether decoding has gone wrong (exhausted or corrupt input); checked periodically
    /// so that corrupt metadata cannot make reconstruction loop forever.
    fn failed(&self) -> bool {
        false
    }
}

/// One recorded decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub ctx: Ctx,
    pub bit: bool,
}

/// The reconstruction metadata: decisions split by channel, each in coding order.
///
/// It is meant to be entropy coded by the caller, decision by decision, using `ctx` as the
/// context (or [`crate::model::ContextModel`] for ready-made probabilities). Decoding is
/// pull-based: [`crate::reconstruct_with`] asks for the next decision of a given context, so
/// the channels can be stored separately and decoded independently.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    pub channels: [Vec<Decision>; Channel::COUNT],
}

impl Metadata {
    pub fn len(&self) -> usize {
        self.channels.iter().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Decisions in coding order, two bytes each, in chunks (no reallocation, little slack).
#[derive(Debug, Default)]
pub struct Log {
    /// Full chunks of `LOG_CHUNK`, then the current one.
    chunks: Vec<Vec<u16>>,
    cur: Vec<u16>,
}

/// The decision bit goes above the context id.
const _: () = assert!(crate::ctx::NUM_CTX <= 1 << 15);

const LOG_CHUNK: usize = 1 << 16;

fn unpack_decision(v: u16) -> Decision {
    Decision {
        ctx: Ctx(v & 0x7fff),
        bit: v >> 15 != 0,
    }
}

impl Log {
    #[inline]
    pub fn push(&mut self, d: Decision) {
        // The first chunk grows as usual, for the many short logs.
        self.cur.push(d.ctx.0 | (d.bit as u16) << 15);
        if self.cur.len() == LOG_CHUNK {
            let full = std::mem::replace(&mut self.cur, Vec::with_capacity(LOG_CHUNK));
            self.chunks.push(full);
        }
    }

    pub fn len(&self) -> usize {
        self.chunks.len() * LOG_CHUNK + self.cur.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = Decision> + '_ {
        self.chunks
            .iter()
            .chain([&self.cur])
            .flatten()
            .map(|&v| unpack_decision(v))
    }
}

/// Consumes the log, freeing it chunk by chunk.
impl IntoIterator for Log {
    type Item = Decision;
    type IntoIter = std::iter::Map<
        std::iter::Flatten<
            std::iter::Chain<std::vec::IntoIter<Vec<u16>>, std::iter::Once<Vec<u16>>>,
        >,
        fn(u16) -> Decision,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.chunks
            .into_iter()
            .chain(std::iter::once(self.cur))
            .flatten()
            .map(unpack_decision)
    }
}

/// Splits the log by channel, allocating each exactly.
impl From<Log> for Metadata {
    fn from(log: Log) -> Metadata {
        let mut n = [0; Channel::COUNT];
        for d in log.iter() {
            n[d.ctx.channel() as usize] += 1;
        }
        let mut meta = Metadata {
            channels: n.map(Vec::with_capacity),
        };
        for d in log {
            meta.channels[d.ctx.channel() as usize].push(d);
        }
        meta
    }
}

/// Records decisions while deconstructing.
#[derive(Default)]
pub struct Recorder {
    pub meta: Metadata,
}

impl Coder for Recorder {
    fn bit(&mut self, ctx: Ctx, bit: bool) -> bool {
        self.meta.channels[ctx.channel() as usize].push(Decision { ctx, bit });
        bit
    }
}

/// Replays recorded decisions while reconstructing.
pub struct Player<'a> {
    meta: &'a Metadata,
    next: [usize; Channel::COUNT],
    /// Set when the metadata ran out or a context did not match.
    pub error: bool,
}

impl<'a> Player<'a> {
    pub fn new(meta: &'a Metadata) -> Self {
        Player {
            meta,
            next: [0; Channel::COUNT],
            error: false,
        }
    }

    pub fn exhausted(&self) -> bool {
        self.next
            .iter()
            .zip(&self.meta.channels)
            .all(|(&n, c)| n == c.len())
    }
}

impl Coder for Player<'_> {
    fn bit(&mut self, ctx: Ctx, _: bool) -> bool {
        let ch = ctx.channel() as usize;
        match self.meta.channels[ch].get(self.next[ch]) {
            Some(d) if d.ctx == ctx => {
                self.next[ch] += 1;
                d.bit
            }
            _ => {
                self.error = true;
                false
            }
        }
    }

    fn failed(&self) -> bool {
        self.error
    }
}

/// `n` equiprobable bits of `v`, most significant first.
pub fn bypass<C: Coder>(c: &mut C, ch: Channel, n: u32, v: u32) -> u32 {
    let mut out = 0;
    for i in (0..n).rev() {
        out = out << 1 | c.bit(Ctx::bypass(ch), v >> i & 1 != 0) as u32;
    }
    out
}

/// Order-0 Exp-Golomb code of `v`: the unary prefix is context coded (prefix position `i`
/// uses `grp.at(min(i, size - 1))`), the suffix is bypass coded.
pub fn exp_golomb<C: Coder>(c: &mut C, grp: Grp, v: u32) -> u32 {
    // In u64: when reconstructing, `v` is a placeholder that may be u32::MAX.
    let v1 = v as u64 + 1;
    let nb = 63 - v1.leading_zeros();
    let mut k = 0;
    loop {
        let more = c.bit(grp.at((k as usize).min(grp.size as usize - 1)), k < nb);
        if !more {
            break;
        }
        k += 1;
        if k > 31 {
            // Only reachable with corrupt metadata.
            return 0;
        }
    }
    let suffix = bypass(c, grp.channel, k, v1.wrapping_sub(1 << k) as u32);
    ((1u64 << k) + suffix as u64 - 1) as u32
}

/// A `n`-bit value coded MSB first with a binary context tree: `grp.at(base + node)` where
/// node runs over 1..2^n.
pub fn tree_bits<C: Coder>(c: &mut C, grp: Grp, base: usize, n: u32, v: u32) -> u32 {
    let mut node = 1usize;
    for i in (0..n).rev() {
        let b = c.bit(grp.at(base + node), v >> i & 1 != 0);
        node = node << 1 | b as usize;
    }
    node as u32 - (1 << n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx;

    #[test]
    fn helpers_round_trip() {
        let vals = [0u32, 1, 2, 3, 7, 8, 100, 16383, 65535, 1 << 20];
        let mut rec = Recorder::default();
        for &v in &vals {
            assert_eq!(exp_golomb(&mut rec, ctx::COUNT_EG, v), v);
            assert_eq!(
                tree_bits(&mut rec, ctx::LENS_VAL, 16 * 3, 4, v & 15),
                v & 15
            );
            assert_eq!(bypass(&mut rec, Channel::Block, 16, v & 0xffff), v & 0xffff);
        }
        let mut play = Player::new(&rec.meta);
        for &v in &vals {
            assert_eq!(exp_golomb(&mut play, ctx::COUNT_EG, 0), v);
            assert_eq!(tree_bits(&mut play, ctx::LENS_VAL, 16 * 3, 4, 0), v & 15);
            assert_eq!(bypass(&mut play, Channel::Block, 16, 0), v & 0xffff);
        }
        assert!(play.exhausted() && !play.error);
    }

    #[test]
    fn log_round_trips() {
        let mut log = Log::default();
        let mut rec = Recorder::default();
        for i in 0..3 * LOG_CHUNK + 5 {
            let d = Decision {
                ctx: Ctx((i * 7 % ctx::NUM_CTX) as u16),
                bit: i % 3 == 0,
            };
            log.push(d);
            rec.bit(d.ctx, d.bit);
        }
        assert_eq!(log.len(), 3 * LOG_CHUNK + 5);
        assert_eq!(Metadata::from(log), rec.meta);
    }
}
