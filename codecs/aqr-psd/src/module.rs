//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec;
use crate::psd::{Error, Result};

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

/// Bytes of the structures the module holds, on WebAssembly (32-bit pointers; checked there by
/// assertions): what its memory plan ([`crate::plan`]) is made of.
pub mod wasm {
    /// A span, a group, a PackBits exception, a slot for a plane's rows and for a PackBits sink.
    pub const SPAN: u64 = 64;
    pub const GROUP: u64 = 28;
    pub const EXCEPTION: u64 = 12;
    pub const PLANE_ROWS: u64 = 44;
    pub const ROW_SINK: u64 = 60;
    /// A plane reference, an optional index, a vector (of the rows of a group).
    pub const PLANE_REF: u64 = 8;
    pub const OPT_INDEX: u64 = 8;
    pub const VEC: u64 = 12;
    /// A layer, its channel, a tag, a resource.
    pub const LAYER: u64 = 108;
    pub const CHANNEL: u64 = 32;
    pub const TAG: u64 = 24;
    pub const RESOURCE: u64 = 24;
    /// The renderer's operation, a slot for a layer's rows, a row of a canvas.
    pub const OP: u64 = 12;
    pub const LAYER_ROWS: u64 = 288;
    pub const ROW: u64 = 20;
}
