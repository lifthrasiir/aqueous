//! Hashed table of adaptive probabilities: one per (context, node of a VLC's code tree).
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
    #[inline]
    pub fn slot(&mut self, ctx: u32, node: u32) -> &mut Prob {
        let h = (ctx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (node as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let h = (h ^ h >> 29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        &mut self.t[(h >> self.shift) as usize]
    }
}
