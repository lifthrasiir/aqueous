//! The memory plan of the module `fasta-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). The module spawns no tasks, and its blocks depend on the
//! lengths of its inputs and output alone.

use aqr_api::stack::{Sim, whole};

use crate::bases::MODEL_BYTES;

/// The memory plan of `fasta-recons`: of its initial memory (`initial` bytes), parameters
/// `params`, inputs of `inputs` bytes and an output of `len`.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64) -> u64 {
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, &[], inputs));
    // (the bases' decoder makes its model, then the file is reserved)
    m.take(MODEL_BYTES);
    m.take(len);
    m.plan()
}
