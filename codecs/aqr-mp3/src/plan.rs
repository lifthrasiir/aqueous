//! The memory plan of the module `mp3-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode_part`] lays it out: the instance's memory taken block by block in the
//! order the module takes it ([`Sim`]). It follows the tables' layout and nothing of the lengths of
//! the inputs or the output: all of them are read through windows.

use aqr_api::stack::{Sim, block};

use crate::module::{SLOTS, wasm};

/// The longest file the module as a part module rebuilds, in bytes: the reservoir's positions are
/// in bits, in 32 (`usize` on WebAssembly).
pub const MAX_FILE: u64 = u32::MAX as u64 / 8;

/// The memory plan of `mp3-recons`: of its initial memory (`initial` bytes), parameters
/// `params`, resources and inputs of these lengths and an output of `len`.
pub fn decode(initial: u64, params: &[u8], resources: &[u64], inputs: &[u64], len: u64) -> Option<u64> {
    if !params.is_empty() || resources.len() != 1 || inputs.len() != SLOTS || len > MAX_FILE {
        return None;
    }
    let n = crate::model::N_CTX as u64;
    let mut m = Sim::new(initial);
    // the resource, read
    m.take(resources[0]);
    // `tables_from`: the alphabets, the canonical contexts and the frequencies, the tables' places
    // over them for a while, then the offsets; `prepare_dec`
    m.take(2 * n);
    m.take(4 * n);
    m.take(2 * crate::codec::total_symbols() as u64);
    m.peak(block(8 * n));
    m.take(4 * n);
    let alphabets: Vec<u16> = (0..crate::model::N_CTX).map(|c| crate::model::alphabet(c) as u16).collect();
    m.take(aqr_common::rans64::Tables::dec_plan(&alphabets, block));
    crate::part::plan(&mut m, wasm::REC, wasm::STATE, wasm::SIDE_MODEL);
    Some(m.plan())
}
