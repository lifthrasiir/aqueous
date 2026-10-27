//! The code of the module (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::codec::{self, Coded, N_BYTE_STREAMS};
use aqr_raw::audio::residual;

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

/// The module: the file of `len` bytes from `resources` (the tables) and `inputs` (the
/// streams).
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let corrupt = |s: &str| Error(format!("corrupt: {s}"));
    if !params.is_empty() {
        return Err(corrupt("params"));
    }
    let [res] = resources else { return Err(corrupt("resources")) };
    let t = residual::tables_from(res).ok_or_else(|| corrupt("tables"))?;
    let [streams @ .., words, raw0, raw1] = inputs else { return Err(corrupt("inputs")) };
    if streams.len() != N_BYTE_STREAMS || words.len() % 4 != 0 {
        return Err(corrupt("inputs"));
    }
    // No output is larger than 64 times its coded form.
    let coded_len = inputs.iter().map(|i| i.len() as u64).sum::<u64>();
    if len > coded_len.saturating_mul(64) + (1 << 20) {
        return Err(Error("length".into()));
    }
    let c = Coded {
        streams: streams.iter().map(|s| s.to_vec()).collect(),
        words: words.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect(),
        raw: [raw0.to_vec(), raw1.to_vec()],
    };
    let mut out = Vec::new();
    codec::decode_threads(&c, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
    if out.len() as u64 != len {
        return Err(corrupt("length"));
    }
    Ok(out)
}
