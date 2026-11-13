//! Blocks as the blocks around them or before (FORMAT.md, "`bc7-recons`", "The choices"), for
//! the families whose blocks are of many layouts (BC7, BC6H, ASTC, ETC): a block is the block to
//! its left or above, or its *head* (all but its indices, as the family tells) is theirs or that
//! of a block before (a cache by a hash of the head, each use keeping its block), or it is new.
//! The family codes the rest: a head's indices, a new block's fields.

use aqr_common::arith::Bit;

use crate::coder::{Cat, Side};
use crate::model::{RECENT, Recent};
use crate::{Result, corrupt};

/// Where a block comes from, as the contexts of the blocks after tell it: it is the left block,
/// or the upper one; its head is the left one's, the upper one's, or of the cache; it is new; no
/// block there.
pub const BLOCK_LEFT: u8 = 0;
pub const BLOCK_UP: u8 = 1;
pub const HEAD_LEFT: u8 = 2;
pub const HEAD_UP: u8 = 3;
pub const CACHED: u8 = 4;
pub const NEW: u8 = 5;
pub const NONE: u8 = 6;

/// The key of a head in the cache: a hash of its bits.
#[inline(always)]
pub fn key(h: u128) -> u32 {
    let x = ((h as u64) ^ ((h >> 64) as u64).rotate_left(29)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (x >> 32) as u32 ^ x as u32
}

/// The models of the choices of blocks of `N` bytes, and their cache.
#[derive(Clone)]
pub struct Copies<const N: usize> {
    /// Whether the block is the left one, or the upper one, by the choices of those (7 each).
    copy: Vec<Bit>,
    /// Whether its head is the left one's, the upper one's (by their choices), or a block's
    /// before (by the left one's choice): the cache's, by rank.
    head: Vec<Bit>,
    rank: Vec<Bit>,
    rank_next: Vec<Bit>,
    /// Whether a block of the cache is the block, its indices too (by the left one's choice).
    whole: Vec<Bit>,
    /// The blocks before by the hash of their heads, the last of each head.
    cache: Recent<[u8; N]>,
}

/// What [`Copies::code`] gives: the block whole, or the block whose head it has (to code its
/// indices), each of its choice; or nothing (a new block).
pub enum Got<const N: usize> {
    Block([u8; N], u8),
    Head([u8; N], u8),
    New,
}

impl<const N: usize> Copies<N> {
    const SIZES: [usize; 5] = [2 * 49, 3 * 7, 16, 16, 7];

    pub fn new() -> Self {
        let v = |n| vec![Bit::default(); n];
        let [copy, head, rank, rank_next, whole] = Self::SIZES;
        Copies {
            copy: v(copy),
            head: v(head),
            rank: v(rank),
            rank_next: v(rank_next),
            whole: v(whole),
            cache: Recent::new(),
        }
    }

    /// Bytes that [`Copies::new`] takes of [`aqr_api::stack`].
    pub fn plan() -> u64 {
        let bits: u64 = Self::SIZES.iter().map(|&n| aqr_api::stack::block((n * size_of::<Bit>()) as u64)).sum();
        bits + Recent::<[u8; N]>::plan()
    }

    pub fn copy_from(&mut self, o: &Self) {
        self.copy.clone_from(&o.copy);
        self.head.clone_from(&o.head);
        self.rank.clone_from(&o.rank);
        self.rank_next.clone_from(&o.rank_next);
        self.whole.clone_from(&o.whole);
        self.cache.copy_from(&o.cache);
    }

    /// The choice of block `raw` (when encoding), between the blocks to its `left` and `up` of
    /// choices `lc` and `uc` and those of the cache, `head` giving a block's head.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn code<S: Side>(
        &mut self,
        s: &mut S,
        raw: &[u8; N],
        left: Option<&[u8; N]>,
        up: Option<&[u8; N]>,
        lc: u8,
        uc: u8,
        head: impl Fn(&[u8; N]) -> u128,
    ) -> Result<Got<N>> {
        let (lc, uc) = (lc as usize, uc as usize);
        let ctx = lc * 7 + uc;
        // the block to the left, or above
        if let Some(l) = left
            && s.bit(&mut self.copy[ctx], raw == l, Cat::Choice)
        {
            return Ok(Got::Block(*l, BLOCK_LEFT));
        }
        if let Some(u) = up.filter(|&u| Some(u) != left)
            && s.bit(&mut self.copy[49 + ctx], raw == u, Cat::Choice)
        {
            return Ok(Got::Block(*u, BLOCK_UP));
        }
        // its head: the left one's, the upper one's, of the cache, or new
        let wh = head(raw);
        let lh = left.map(&head);
        let uh = up.map(&head).filter(|&h| Some(h) != lh);
        if let Some(h) = lh
            && s.bit(&mut self.head[lc], wh == h, Cat::Choice)
        {
            return Ok(Got::Head(*left.unwrap(), HEAD_LEFT));
        }
        if let Some(h) = uh
            && s.bit(&mut self.head[7 + uc], wh == h, Cat::Choice)
        {
            return Ok(Got::Head(*up.unwrap(), HEAD_UP));
        }
        let k = key(wh);
        let rank = match s.encoding() {
            true => self.cache.rank(k).filter(|&r| r < RECENT && self.cache.payload(k).map(&head) == Some(wh)),
            false => None,
        };
        if s.bit(&mut self.head[14 + lc], rank.is_some(), Cat::Choice) {
            let r = s.token(&mut self.rank, &mut self.rank_next, rank.unwrap_or(0), Cat::Cache);
            let c = *self.cache.at_with(r).filter(|_| r < RECENT).ok_or_else(corrupt)?.1;
            if s.bit(&mut self.whole[lc], raw == &c, Cat::Choice) {
                return Ok(Got::Block(c, CACHED));
            }
            return Ok(Got::Head(c, CACHED));
        }
        Ok(Got::New)
    }

    /// Block `raw`, of head `h`, used.
    #[inline]
    pub fn used(&mut self, h: u128, raw: [u8; N]) {
        self.cache.touch_with(key(h), raw);
    }
}

impl<const N: usize> Default for Copies<N> {
    fn default() -> Self {
        Self::new()
    }
}
