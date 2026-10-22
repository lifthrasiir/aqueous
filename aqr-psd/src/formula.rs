//! aqr-psd as a module of entry formulas (see aqr-formula): [`decode`] on four inputs,
//! the head, the skeleton, the embedded files and the model stream, and no parameters. See
//! FORMAT.md, *Formula module*.
//!
//! Embedded files coded by a recompressor (PSD/PSB, PNG, JPEG) are not decoded by the module:
//! their bytes are the third input, which the formula fills with the children's formulas.

use crate::codec::{self, Par};
use crate::psd::{Error, Result};
use aqr_formula::{Apply, Formula, Module, Node};

pub const MODULE: Module = Module { name: "psd-recons", version: 1 };

/// The formula giving `input`, or `None` where it is not coded: not a PSD/PSB
/// that can be parsed, or no pixel data or embedded file to take out.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_at(input, 0, &Par::new(threads))
}

/// [`encode`] for a file `depth` deep in the embedded files of another.
pub(crate) fn encode_at(input: &[u8], depth: usize, par: &Par) -> Option<Formula> {
    let parts =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec::formula_parts(input, depth, par))).ok()??;
    let mut f = Formula::default();
    let head = f.push("psd/head", parts.head);
    let skeleton = f.push("psd/skeleton", parts.skeleton);
    let embedded = Node::concat(parts.embedded.into_iter().map(|child| f.embed(child)).collect::<Vec<_>>());
    let model = f.push("psd/model", parts.model);
    f.root = Node::apply(MODULE, Vec::new(), vec![head, skeleton, embedded, model], input.len() as u64);
    Some(f)
}

/// The module: the file of `len` bytes rebuilt from `inputs`, the head, the skeleton (the
/// bytes outside the spans), the embedded files (their bytes one after another, in file
/// order) and the model stream. `threads` is the number of threads to decode with (the same bytes whatever
/// the number).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let (&[], &[head, skeleton, embedded, model]) = (params, inputs) else {
        return Err(Error("bad parameters or inputs".into()));
    };
    codec::decode_formula(head, skeleton, embedded, model, len, threads)
}

/// Runs the modules a formula of this module has in it (this one, aqr-png and aqr-jpeg).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_png::formula::MODULE {
        aqr_png::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_jpeg::formula::MODULE {
        aqr_jpeg::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else {
        Err(aqr_formula::Error::new(format!("no module {}", a.module)))
    }
}
