//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

/// `pcm-recons`.
pub mod pcm {
    use super::{Error, Result};
    use crate::audio::pcm::{self, Format, N_BYTE_STREAMS};
    use crate::audio::residual::{self, Coded};

    /// The module: the `len` bytes of samples from `params` (their format), `resources` (the
    /// tables) and `inputs` (the streams).
    pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
        let corrupt = |s: &str| Error(format!("corrupt: {s}"));
        let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
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
        pcm::decode_threads(&c, fmt, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
        if out.len() as u64 != len {
            return Err(corrupt("length"));
        }
        Ok(out)
    }
}

/// `raster-recons`.
pub mod raster {
    use super::{Error, Result};
    use crate::image::raster::{self, Format};

    /// The module: the raster of `len` bytes from `params` (its layout) and `inputs` (the coded
    /// samples and the padding).
    pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
        let _ = threads;
        let corrupt = |s: &str| Error(format!("corrupt: {s}"));
        let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
        let &[coded, pad] = inputs else { return Err(corrupt("inputs")) };
        if fmt.len() != Some(len as usize) || usize::try_from(len).is_err() {
            return Err(corrupt("length"));
        }
        let mut out = Vec::new();
        raster::decode(coded, pad, fmt, &mut out).map_err(|e| corrupt(e.0))?;
        Ok(out)
    }
}
