//! Static-model rANS ([`recons_common::rans32`], after png-recons' coder) with byte
//! alphabets. Probabilities have 11 bits (rather than 12) so that the decoding tables of a
//! chunk's contexts mostly fit in the L1 cache: the model's next context depends on the
//! symbol just decoded, so table lookups are on the critical path.

use crate::psd::{Error, Result};
use recons_common::rans32;

pub use rans32::{Decoder, cost};

pub const PROB_BITS: u32 = 11;
pub const PROB_SCALE: u32 = 1 << PROB_BITS;

pub type BitW = rans32::BitW<false>;
pub type BitR<'a> = rans32::BitR<'a, false>;
/// Decoding tables of all contexts.
pub type Tables = rans32::Tables<u8, PROB_BITS>;

/// Writes the levels of `counts` (a used context) and returns the frequencies both sides
/// will use.
pub fn write_table(w: &mut BitW, counts: &[u32]) -> Vec<u32> {
    rans32::write_table::<PROB_BITS, false>(w, counts)
}

pub fn read_table(r: &mut BitR, n: usize) -> Result<Vec<u32>> {
    rans32::read_table::<PROB_BITS, false>(r, n).map_err(|_| Error("corrupt entropy coded data".into()))
}

/// Encodes `syms` (table index, symbol), in order; returns the byte stream the decoder
/// reads forward. `freqs` are the tables' frequencies.
pub fn encode(syms: &[(u16, u16)], freqs: &[Vec<u32>]) -> Vec<u8> {
    rans32::encode::<PROB_BITS>(syms, freqs)
}
