//! Static-model rANS ([`aqr_common::rans32`]) with 12-bit probabilities and byte
//! alphabets: per-context frequency tables counted in a first pass, stored with the data,
//! and four interleaved 32-bit states with 16-bit renormalisation.

use crate::Result;
use aqr_common::rans32;

pub use rans32::{Decoder, cost};

pub const PROB_BITS: u32 = 12;
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
    rans32::read_table::<PROB_BITS, false>(r, n).map_err(|_| crate::corrupt())
}

/// Encodes `syms` (table index, symbol), in order; returns the byte stream the decoder
/// reads forward. `freqs` are the tables' frequencies.
pub fn encode(syms: &[(u8, u8)], freqs: &[Vec<u32>]) -> Vec<u8> {
    rans32::encode::<PROB_BITS>(syms, freqs)
}
