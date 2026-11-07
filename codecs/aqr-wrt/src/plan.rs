//! The memory plans of the modules `wrt-recons` and `wrt-recons-hangul` on WebAssembly
//! ([`aqr_api::stack`]), as [`crate::module`] lays them out: the instance's memory taken block
//! by block in the order the module takes it ([`Sim`]). The modules spawn no tasks (the pieces
//! of a text are decoded one after another in Wasm). What they take follows from the shape of
//! the dictionary, which their params tell (a resource's contents are not at hand for a plan),
//! and the lengths of their resources, inputs and output.

use aqr_api::stack::{Sim, whole};

use crate::{hangul, words};

/// The memory plan of `wrt-recons`: of its initial memory (`initial` bytes), parameters `params`,
/// resources and inputs of these lengths, and an output of `len`.
pub fn recons(initial: u64, params: &[u8], resources: &[u64], inputs: &[u64], len: u64) -> Option<u64> {
    let (entries, lists, _) = words::header(params).ok()?;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, resources, inputs));
    // (the dictionary parsed: the layout's lists, then the entries; then the text)
    for l in lists {
        m.take(l as u64);
    }
    m.take(words::ENTRY * entries as u64);
    m.take(len);
    Some(m.plan(1))
}

/// The memory plan of `wrt-recons-hangul`, as [`recons`].
pub fn recons_hangul(initial: u64, params: &[u8], resources: &[u64], inputs: &[u64], len: u64) -> Option<u64> {
    let (entries, _) = hangul::header(params).ok()?;
    let mut m = Sim::new(initial);
    m.take(whole(params.len() as u64, resources, inputs));
    m.take(hangul::ENTRY * entries as u64);
    m.take(len);
    Some(m.plan(1))
}
