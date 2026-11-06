//! The code of the modules (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm modules.

use crate::{Result, corrupt};

/// `pdb-msf`: the file of `len` bytes from `params` (none) and `inputs` (those of
/// [`crate::msf::INPUT_NAMES`]).
/// The memory plan of `pdb-msf` ([`aqr_api::stack`]): what it takes beyond its initial memory at
/// most, of inputs of `inputs` bytes (the second the layout `layout`) and an output of `len`: what
/// `aqr_api::whole` copies, and what decoding takes ([`crate::msf::decode_plan`]).
pub fn msf_plan(inputs: &[u64], layout: &[u8], len: u64) -> Option<u64> {
    Some(aqr_api::stack::whole(0, &[], inputs) + crate::msf::decode_plan(layout, len)?)
}

pub fn msf(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    if !params.is_empty() {
        return Err(corrupt());
    }
    crate::msf::decode(inputs, len)
}

/// The memory plan of `pdb-symbols` ([`aqr_api::stack`]): what it takes beyond its initial
/// memory at most, of params `params`, inputs of `inputs` bytes (the first `head`, the fifth
/// `c13`) and an output of `len` (`None` where they are not so): what `aqr_api::whole` copies,
/// the streams' list, and the inputs' lengths and streams as each unpacks or what decoding takes
/// ([`crate::symbols::decode_plan`]).
pub fn symbols_plan(params: &[u8], inputs: &[u64], head: &[u8], c13: &[u8], len: u64) -> Option<u64> {
    use crate::symbols::{INPUTS, NUM_STREAMS};
    use aqr_api::stack::block;
    let kinds = crate::unpack(head, INPUTS[0].1.len()).ok()?[0];
    let ids = crate::unpack(c13, INPUTS[4].1.len()).ok()?.last()?.len();
    let unpacking =
        INPUTS.iter().map(|(_, w)| block(4 * w.len() as u64) + block(SLICE_BYTES * w.len() as u64)).max()?;
    let decoding = crate::symbols::decode_plan(params, kinds, ids, len)?;
    Some(
        aqr_api::stack::whole(params.len() as u64, &[], inputs)
            + block(SLICE_BYTES * NUM_STREAMS as u64)
            + unpacking.max(decoding),
    )
}

/// Bytes of a slice on WebAssembly.
const SLICE_BYTES: u64 = 8;
#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<&[u8]>() as u64 == SLICE_BYTES);

/// `pdb-symbols`: the modules' symbol substreams, one after another, from `params` and
/// `inputs` (those of [`crate::symbols::INPUTS`]), `len` bytes in all.
pub fn symbols(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    use crate::symbols::{INPUTS, NUM_STREAMS};
    if inputs.len() != INPUTS.len() {
        return Err(corrupt());
    }
    let mut streams: Vec<&[u8]> = vec![&[]; NUM_STREAMS];
    for (input, (_, which)) in inputs.iter().zip(INPUTS) {
        for (s, &k) in crate::unpack(input, which.len())?.into_iter().zip(which) {
            streams[k] = s;
        }
    }
    let out = crate::symbols::decode_sized(params, &streams, usize::try_from(len).map_err(|_| corrupt())?)?;
    if out.len() as u64 != len {
        return Err(corrupt());
    }
    Ok(out)
}
