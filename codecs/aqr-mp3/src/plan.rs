//! The memory plan of the module `mp3-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]). What it takes follows from the lengths of its inputs and
//! output, the tables' layout, and the counts of runs and frames that SEGS tells.

use aqr_api::stack::{Sim, block};

use crate::codec::{self, WINDOW, counts};
use crate::module::{SLOTS, wasm};
use crate::stream::R_PAD;

/// The memory plan of `mp3-recons`: of its initial memory (`initial` bytes), parameters
/// `params`, resources and inputs of these lengths, an output of `len`, and SEGS.
pub fn decode(initial: u64, params: &[u8], resources: &[u64], inputs: &[u64], len: u64, segs: &[u8]) -> Option<u64> {
    if !params.is_empty() || resources.len() != 1 || inputs.len() != SLOTS {
        return None;
    }
    let (runs, frames) = counts(segs).ok()?;
    let (runs, frames) = (runs as u64, frames as u64);
    let n = crate::model::N_CTX as u64;
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(0, resources, inputs));
    // `tables_from`: the alphabets, the canonical contexts and the frequencies, the tables' places
    // over them for a while, then the offsets; `prepare_dec`
    m.take(2 * n);
    m.take(4 * n);
    m.take(2 * codec::total_symbols() as u64);
    m.peak(block(8 * n));
    m.take(4 * n);
    let alphabets: Vec<u16> = (0..crate::model::N_CTX).map(|c| crate::model::alphabet(c) as u16).collect();
    m.take(aqr_common::rans64::Tables::dec_plan(&alphabets, block));
    // `decode_parts`: the output, the frames (`parse_frames`), the raw bits and the rANS words,
    // the tail, raw granules and gaps, the side information's model, the window
    m.take(len);
    m.take(4 * (runs + 1));
    m.take(8 * runs);
    m.take(wasm::FRAME * frames);
    m.take(4 * frames);
    m.take(inputs[9] + R_PAD as u64);
    m.take(4 * ((inputs[8] / 4).max(16) + aqr_common::rans64::SRC_PAD as u64));
    for k in [4, 5, 6] {
        m.take(inputs[k] + R_PAD as u64);
    }
    m.take(wasm::SIDE_MODEL);
    m.take(WINDOW as u64 + 16);
    // `main_pass`'s state, and a frame's side information written
    m.take(wasm::STATE);
    m.peak(block(codec::SIDE_ROOM as u64));
    Some(m.plan())
}
