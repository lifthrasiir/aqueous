//! Table of adaptive probabilities, hashed by context. The nodes of four bits of a VLC's code tree share a line of 16
//! probabilities (64 bytes), found by one hash: [`Model::line`], then [`Model::at`] for each bit.
use crate::rc::Prob;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

/// Probabilities (4 bytes each) in a block whose use is noted: 4 KiB.
const BLOCK_SHIFT: u32 = 10;

pub struct Model {
    t: Vec<Prob>,
    shift: u32,
    /// The blocks of `t` a line was looked up in since it was new or reset, one bit each: the others are still
    /// `Prob::INIT`, so that [`Model::reset`] costs what a chunk used, not what the model holds.
    dirty: Vec<Cell<u64>>,
}

impl Model {
    pub fn new(bits: u32) -> Self {
        let blocks = ((1usize << bits) >> BLOCK_SHIFT).max(1);
        Model { t: vec![Prob::INIT; 1 << bits], shift: 64 - bits, dirty: vec![Cell::new(0); blocks.div_ceil(64)] }
    }

    /// The bytes a model of `bits` takes.
    pub const fn bytes(bits: u32) -> u64 {
        (core::mem::size_of::<Prob>() as u64) << bits
    }

    /// Back to the probabilities of a new model, in place.
    pub fn reset(&mut self) {
        let Model { t, dirty, .. } = self;
        for (w, word) in dirty.iter_mut().enumerate() {
            let mut bits = core::mem::take(word.get_mut());
            while bits != 0 {
                let block = w * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let from = block << BLOCK_SHIFT;
                let to = (from + (1 << BLOCK_SHIFT)).min(t.len());
                t[from..to].fill(Prob::INIT);
            }
        }
    }

    /// The line (the index of its first probability) of group `group` of four bits, after the bits `prefix`, under `ctx`.
    #[inline]
    pub fn line(&self, ctx: u32, group: u32, prefix: u32) -> usize {
        let h = (((ctx as u64) << 32) | ((group as u64) << 28) ^ prefix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let h = (h ^ h >> 31).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        let line = ((h >> self.shift) as usize) & !15;
        let block = line >> BLOCK_SHIFT;
        let word = &self.dirty[block / 64];
        word.set(word.get() | 1 << (block % 64));
        line
    }

    /// The probability of node `node` (1 to 15) of the line at `base`.
    #[inline]
    pub fn at(&mut self, base: usize, node: u32) -> &mut Prob {
        &mut self.t[base + node as usize]
    }

    /// A probability of its own for (`ctx`, `node`), for the elements that are single bits.
    #[inline]
    pub fn slot(&mut self, ctx: u32, node: u32) -> &mut Prob {
        let base = self.line(ctx, 15, node);
        &mut self.t[base + 1]
    }
}
