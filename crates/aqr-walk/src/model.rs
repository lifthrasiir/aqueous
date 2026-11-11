//! Table of adaptive probabilities, hashed by context. The nodes of four bits of a VLC's code tree share a line of 16
//! probabilities (64 bytes), found by one hash: [`Model::line`], then [`Model::at`] for each bit.
use crate::rc::Prob;
use alloc::vec;
use alloc::vec::Vec;

pub struct Model {
    t: Vec<Prob>,
    shift: u32,
}

impl Model {
    pub fn new(bits: u32) -> Self {
        Model { t: vec![Prob::INIT; 1 << bits], shift: 64 - bits }
    }

    /// Back to the probabilities of a new model, in place.
    pub fn reset(&mut self) {
        self.t.fill(Prob::INIT);
    }

    /// The line (the index of its first probability) of group `group` of four bits, after the bits `prefix`, under `ctx`.
    #[inline]
    pub fn line(&self, ctx: u32, group: u32, prefix: u32) -> usize {
        let h = (((ctx as u64) << 32) | ((group as u64) << 28) ^ prefix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let h = (h ^ h >> 31).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        ((h >> self.shift) as usize) & !15
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
