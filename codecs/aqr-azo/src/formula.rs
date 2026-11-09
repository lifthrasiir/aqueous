//! aqr-azo as a module of entry formulas (see aqr-formula): AZO streams rebuilt from their data
//! and records. `FORMAT.md` specifies it. The data is an input of its own, which the formula may
//! recompress in turn. AZO has no file of its own: aqr-egg takes the streams of EGG archives.

use aqr_formula::{Apply, Module, Node};

use crate::recons;

pub use crate::module::{decode, index};
pub use crate::recons::Split;

pub const MODULE: Module = Module::new("azo-recons", 1).with_plan(plan).with_contents(&[0]);

/// The memory of the module: its plan ([`crate::plan::decode`]).
fn plan(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let files = u.all_contents::<3>()?;
    crate::plan::decode(u.initial(), u.params, u.inputs, u.output(), files)
}

/// The AZO stream that is all of `d`, of `size` bytes of data (at most `limit`): its data and
/// record, where they rebuild it; its blocks on up to `threads` threads.
pub fn take(d: &[u8], size: u64, limit: usize, threads: usize) -> Option<Split> {
    let sp = recons::split(d, limit, threads).ok()?;
    (sp.used == d.len() && sp.data.len() as u64 == size).then_some(sp)
}

/// The application of [`MODULE`] rebuilding streams of `len` bytes in all from their data
/// `data`, their records `records` and the index of their lengths `index` ([`index`]).
pub fn apply(data: Node, records: Node, index: Node, len: u64) -> Apply {
    Apply {
        module: MODULE,
        resources: vec![],
        params: vec![],
        inputs: vec![data, records, index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    }
}
