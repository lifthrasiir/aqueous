//! flate-recons as a module of entry formulas (see recons-formula): [`reconstruct`] on two
//! inputs, the data and the record, and no parameters.

use recons_formula::{Module, Node};

pub const MODULE: Module = Module {
    name: "flate-recons",
    version: 1,
};

/// The application rebuilding a DEFLATE stream of `len` bytes from `data` and `record`
/// (what [`crate::deconstruct`] gives).
pub fn apply(data: Node, record: Node, len: u64) -> Node {
    Node::apply(MODULE, Vec::new(), vec![data, record], len)
}

/// The module: the stream of `len` bytes rebuilt from `inputs`, the data and the record.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> crate::Result<Vec<u8>> {
    let (&[], &[data, record]) = (params, inputs) else {
        return Err(crate::Error::Corrupt);
    };
    let out = crate::reconstruct_with_threads(data, record, threads)?;
    if out.len() as u64 != len {
        return Err(crate::Error::Corrupt);
    }
    Ok(out)
}
