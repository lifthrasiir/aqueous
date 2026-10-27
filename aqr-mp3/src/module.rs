//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec::{self, N_BYTE_STREAMS};
use crate::par;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Slots: the eight byte streams of the coded form, then the rANS words (u32 LE) and the raw
/// bits.
pub(crate) const SLOTS: usize = N_BYTE_STREAMS + 2;

/// The module: the file of `len` bytes rebuilt from the ten `inputs` and the `resources` (the
/// tables), with `threads` threads (the same output whatever the number).
pub fn decode(
    params: &[u8],
    resources: &[&[u8]],
    inputs: &[&[u8]],
    len: u64,
    threads: usize,
) -> Result<Vec<u8>, Error> {
    let corrupt = |m: &str| Error(format!("corrupt: {m}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
    let [res] = resources else { return Err(corrupt("resources")) };
    // The built-in tables, built once for the process, unless the resource is another (or
    // none is built in)
    let own;
    let t = if cfg!(feature = "encode") && *res == &codec::builtin_raw()[..] {
        codec::tables()
    } else {
        own = codec::tables_from(res).ok_or_else(|| corrupt("tables"))?;
        &own
    };
    let &[.., words, raw] = inputs else { return Err(corrupt("inputs")) };
    if inputs.len() != SLOTS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    let total: usize = inputs.iter().map(|s| s.len()).sum();
    let streams: [&[u8]; N_BYTE_STREAMS] = std::array::from_fn(|i| inputs[i]);
    let out = if threads <= 1 || total < par::PAR_MIN {
        codec::decode_parts(t, len, &streams, words, raw).map_err(|e| corrupt(e.0))?
    } else {
        par::decode(t, len, &streams, words, raw, threads).map_err(|e| corrupt(e.0))?
    };
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
