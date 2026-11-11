//! aqr-ac3 as a module of entry formulas (see aqr-formula): an AC-3 byte stream from its coded form. `FORMAT.md`
//! specifies it.
use std::vec::Vec;

use aqr_formula::Module;
use aqr_walk::rc::Encoder;

pub use crate::module::decode;

/// The class of the coded stream.
pub const CODED: &str = "ac3/frames";

/// The module `ac3-recons`: no params; the input the coded stream.
pub const MODULE: Module = Module::new("ac3-recons", 1).with_plan(memory);

fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_] = u.inputs else { return None };
    Some(crate::module::plan(u.initial()))
}

/// The coded form of the byte stream `data`, if it comes to less than 97% of it.
pub fn encode(data: &[u8]) -> Option<Vec<u8>> {
    let mut enc = Encoder::new();
    crate::stream::encode(&mut enc, data);
    let coded = enc.finish();
    (coded.len() as f64 <= 0.97 * data.len() as f64).then_some(coded)
}
