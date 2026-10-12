//! Static-model rANS ([`recons_common::rans32`], after png-recons' coder) with alphabets of
//! more than 256 symbols: per-table frequencies counted in a first pass and stored as
//! quantised logarithms, four interleaved 32-bit states with 16-bit renormalisation. The
//! fields of the Elias gamma codes of the bit I/O are most significant bit first.

use crate::Result;
use recons_common::rans32;

pub use rans32::{Decoder, cost};

pub const PROB_BITS: u32 = 12;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;

pub type BitW = rans32::BitW<true>;
pub type BitR<'a> = rans32::BitR<'a, true>;
pub type Tables = rans32::Tables<u16, PROB_BITS>;

/// Writes the levels of `counts` (a used table) and returns the frequencies both sides use.
pub fn write_table(w: &mut BitW, counts: &[u32]) -> Vec<u32> {
    rans32::write_table::<PROB_BITS, true>(w, counts)
}

pub fn read_table(r: &mut BitR, n: usize) -> Result<Vec<u32>> {
    Ok(rans32::read_table::<PROB_BITS, true>(r, n)?)
}

/// Encodes `syms` (table, symbol) in order; returns the stream the decoder reads forward.
pub fn encode(syms: &[(u16, u16)], freqs: &[Vec<u32>]) -> Vec<u8> {
    rans32::encode::<PROB_BITS>(syms, freqs)
}
