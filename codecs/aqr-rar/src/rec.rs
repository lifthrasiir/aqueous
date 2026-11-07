//! What the records of RAR5's and RAR 3.x's streams share: their bits coded arithmetically
//! under adaptive contexts (`Bits`), and the match finder whose matches they are coded
//! against (`Mf`).

pub use aqr_common::arith::Coder;
use aqr_common::arith::{ArithDecoder, Bit, Costed};

use crate::{Result, corrupt};

pub use aqr_common::lz::lcp_up;

/// What the record codes, for the statistics.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Cat {
    /// The files' sizes, the filters, the corrections.
    Head,
    /// Whether a match starts.
    Start,
    /// Which match: a repeated distance or a pair.
    Pair,
    /// A length shorter than the match's.
    Len,
    /// Matches not among the pairs.
    Escape,
    /// How a match is written (explicit, repeated, the last again).
    Form,
    /// The blocks: their sizes, tables.
    Blocks,
}
pub const CATS: usize = 7;

/// What codes the record's bits ([`Coder`]: the arithmetic encoder, which writes the value
/// given, or decoder, which returns what it reads), of this crate's [`Cat`]s and errors.
pub trait Bits: Coder {
    /// What the bits after are, for the statistics.
    #[inline]
    fn cat(&mut self, c: Cat) {
        self.set_cat(c as usize)
    }

    /// [`Coder::gamma64`], failing as corrupt input.
    fn gamma(&mut self, t: &mut [Bit], v: u64) -> Result<u64> {
        self.gamma64(t, v, corrupt)
    }
}

impl<C: Coder> Bits for C {}

/// The encoder, counting the bits of each [`Cat`].
pub type Enc = Costed<CATS>;

/// The decoder, its adaptive bits read out of line in the modules: inlined (as they are of
/// `aqr_common::arith`'s decoder), rar3-recons came to some 6 KB more and rar5-recons 3 KB.
pub struct Dec<'a>(pub ArithDecoder<'a>);

impl Coder for Dec<'_> {
    #[cfg_attr(target_family = "wasm", inline(never))]
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        self.0.bit(b, v)
    }
    fn raw(&mut self, v: bool) -> bool {
        self.0.raw(v)
    }
    fn failed(&self) -> bool {
        self.0.failed()
    }
}

/// `n` adaptive bits.
pub fn bits(n: usize) -> Vec<Bit> {
    vec![Bit::default(); n]
}

const NONE: u32 = u32::MAX;

/// The match finder: the positions before the current one, on a chain for each key length of
/// [`Mf::KEYS`], each chain holding the positions of its ring.
pub struct Mf<'a> {
    d: &'a [u8],
    reach: usize,
    /// The heads of the chains of each key, `1 << bits` each, one after another.
    head: Vec<u32>,
    /// The previous position on each key's chain, of the positions within a ring ([`Mf::ring`]),
    /// one ring after another.
    prev: Vec<u32>,
    /// The start and mask of each ring in `prev`.
    ring_at: [usize; 5],
    mask: [usize; 5],
    /// Positions below this are inserted.
    next: usize,
    found: Vec<(usize, usize)>,
    /// The bits of the heads ([`head_bits`]).
    bits: u32,
}

/// Bits of the chains' heads for `n` bytes of data: as many as the data's positions need (12 to
/// 20), so that a chain holds few other keys' positions.
pub fn head_bits(n: u64) -> u32 {
    (64 - n.leading_zeros()).clamp(12, 20)
}

/// The key of `k` bytes at `p` (`p + k` within the data), of `bits` bits.
#[inline]
fn key(d: &[u8], p: usize, k: usize, bits: u32) -> usize {
    let mut h = 0u64;
    for &b in &d[p..p + k] {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - bits)) as usize
}

impl<'a> Mf<'a> {
    /// Key lengths of the chains, and the most positions each holds (log 2): the chain of 2
    /// bytes serves near matches only.
    pub const KEYS: [(usize, u32); 5] = [(2, 10), (3, 18), (5, 22), (8, 22), (16, 22)];
    /// Candidates looked at on each chain for the longest matches.
    pub const DEPTH: usize = 32;
    /// Candidates looked at for an escape's rank.
    pub const LIMIT: usize = 4096;

    /// The most pairs found at a position: [`Mf::DEPTH`] candidates on each key's chain.
    pub const PAIRS: usize = Self::DEPTH * Self::KEYS.len();

    /// Slots of the ring of the chain `c` for `n` bytes of data.
    pub fn ring(n: usize, c: usize) -> usize {
        n.max(2).next_power_of_two().min(1 << Self::KEYS[c].1)
    }

    /// Slots of the heads and of the rings of a match finder of `n` bytes of data.
    pub fn slots(n: usize) -> (usize, usize) {
        (Self::KEYS.len() << head_bits(n as u64), (0..Self::KEYS.len()).map(|c| Self::ring(n, c)).sum())
    }

    pub fn new(d: &'a [u8], reach: usize) -> Self {
        let bits = head_bits(d.len() as u64);
        let (heads, rings) = Self::slots(d.len());
        let (mut ring_at, mut mask, mut at) = ([0; 5], [0; 5], 0);
        for c in 0..Self::KEYS.len() {
            let r = Self::ring(d.len(), c);
            (ring_at[c], mask[c]) = (at, r - 1);
            at += r;
        }
        Mf {
            d,
            reach,
            head: vec![NONE; heads],
            prev: vec![NONE; rings],
            ring_at,
            mask,
            next: 0,
            found: Vec::with_capacity(Self::PAIRS),
            bits,
        }
    }

    /// Inserts the positions up to `pos`.
    pub fn advance(&mut self, pos: usize) {
        let d = self.d;
        while self.next < pos {
            let p = self.next;
            for (c, &(k, _)) in Self::KEYS.iter().enumerate() {
                if p + k <= d.len() {
                    let h = c << self.bits | key(d, p, k, self.bits);
                    self.prev[self.ring_at[c] + (p & self.mask[c])] = self.head[h];
                    self.head[h] = p as u32;
                }
            }
            self.next += 1;
        }
    }

    /// The farthest a chain `c` reaches back from `pos`.
    #[inline]
    fn far(&self, c: usize, pos: usize) -> usize {
        self.reach.min(self.mask[c]).min(pos)
    }

    /// The longest matches at `pos` along the first [`Mf::DEPTH`] candidates of each chain,
    /// nearest first: each `(length, distance)` longer than those nearer, at most `cap` long,
    /// those that `ok` takes.
    pub fn pairs(&mut self, pos: usize, cap: usize, ok: impl Fn(usize, usize) -> bool, out: &mut Vec<(usize, usize)>) {
        out.clear();
        let d = self.d;
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        let mut best = 1;
        for (c, &(k, _)) in Self::KEYS.iter().enumerate() {
            if pos + k > d.len() || k > cap {
                break;
            }
            let (mask, at) = (self.mask[c], self.ring_at[c]);
            let far = self.far(c, pos);
            let mut q = self.head[c << self.bits | key(d, pos, k, self.bits)];
            let mut top = k - 1;
            for _ in 0..Self::DEPTH {
                if q == NONE {
                    break;
                }
                let p = q as usize;
                // (the position itself, inserted where the next was looked at ahead)
                if p >= pos {
                    q = self.prev[at + (p & mask)];
                    continue;
                }
                if pos - p > far {
                    break;
                }
                if d[p + top] == d[pos + top] {
                    let l = lcp_up(d, p, pos, cap);
                    if l > top && ok(l, pos - p) {
                        top = l;
                        found.push((pos - p, l));
                        if l == cap {
                            break;
                        }
                    }
                }
                q = self.prev[at + (p & mask)];
            }
            best = best.max(top);
            if best == cap {
                break;
            }
        }
        // the nearest of each length
        found.sort_unstable();
        let mut best = 1;
        for &(dist, l) in found.iter() {
            if l > best {
                best = l;
                out.push((l, dist));
            }
        }
        self.found = found;
    }

    /// The chain an escape of `len` bytes is ranked on: the longest key within it.
    fn rank_chain(len: usize) -> usize {
        Self::KEYS.iter().rposition(|&(k, _)| k <= len.min(5)).unwrap_or(0)
    }

    /// The rank of the match `dist` back of at least `len` bytes at `pos` among those that
    /// `ok` takes on its chain, nearest first, among the first [`Mf::LIMIT`] candidates; where
    /// it is not among them, how many there are.
    pub fn rank(&self, pos: usize, len: usize, dist: usize, ok: impl Fn(usize) -> bool) -> usize {
        let mut count = 0;
        let mut found = false;
        self.walk(pos, len, |q| {
            if !ok(pos - q) {
                return true;
            }
            found = pos - q == dist;
            count += !found as usize;
            !found
        });
        count
    }

    /// The distance of the match of rank `r` (as [`Mf::rank`]).
    pub fn nth(&self, pos: usize, len: usize, r: usize, ok: impl Fn(usize) -> bool) -> Option<usize> {
        let (mut k, mut out) = (0, None);
        self.walk(pos, len, |q| {
            if !ok(pos - q) {
                return true;
            }
            if k == r {
                out = Some(pos - q);
            }
            k += 1;
            out.is_none()
        });
        out
    }

    /// Calls `f` with each position matching at least `len` bytes at `pos` on the chain of
    /// [`Mf::rank_chain`], nearest first, among its first [`Mf::LIMIT`] within reach, while it
    /// returns true.
    fn walk(&self, pos: usize, len: usize, mut f: impl FnMut(usize) -> bool) {
        let d = self.d;
        let c = Self::rank_chain(len);
        let k = Self::KEYS[c].0;
        if pos + k > d.len() || pos + len > d.len() {
            return;
        }
        let (mask, at) = (self.mask[c], self.ring_at[c]);
        let far = self.far(c, pos);
        let mut q = self.head[c << self.bits | key(d, pos, k, self.bits)];
        for _ in 0..Self::LIMIT {
            if q == NONE {
                return;
            }
            let p = q as usize;
            // (the position itself, inserted where the next was looked at ahead)
            if p >= pos {
                q = self.prev[at + (p & mask)];
                continue;
            }
            if pos - p > far {
                return;
            }
            if d[p + len - 1] == d[pos + len - 1] && d[p..p + len] == d[pos..pos + len] && !f(p) {
                return;
            }
            q = self.prev[at + (p & mask)];
        }
    }
}
