//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `raster-recons`.
pub mod raster {
    use super::{Error, Result};
    use crate::image::raster::{self, Format};

    /// The memory plan of the module ([`aqr_api::stack`]): what it takes beyond its initial
    /// memory at most, of params `params` (`None` where they are not a layout), inputs of
    /// `inputs` bytes and an output of `len`: what `aqr_api::whole` copies, and what decoding
    /// takes ([`raster::decode_plan`]).
    pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
        let fmt = Format::from_bytes(params)?;
        Some(aqr_api::stack::whole(params.len() as u64, &[], inputs) + raster::decode_plan(&fmt, len))
    }

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

/// `raster-recons-16`.
pub mod raster16 {
    use super::{Error, Result};
    use crate::image::raster16::{self, Format};

    /// The memory plan of the module, as [`super::raster::plan`]'s ([`raster16::decode_plan`]).
    pub fn plan(params: &[u8], inputs: &[u64], len: u64) -> Option<u64> {
        let fmt = Format::from_bytes(params)?;
        Some(aqr_api::stack::whole(params.len() as u64, &[], inputs) + raster16::decode_plan(&fmt, len))
    }

    /// The module: the raster of `len` bytes from `params` (its layout) and `inputs` (the coded
    /// samples and the padding).
    pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
        let corrupt = |s: &str| Error(format!("corrupt: {s}"));
        let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
        let &[coded, pad] = inputs else { return Err(corrupt("inputs")) };
        if fmt.len() != Some(len as usize) || usize::try_from(len).is_err() {
            return Err(corrupt("length"));
        }
        let mut out = Vec::new();
        raster16::decode(coded, pad, fmt, &mut out).map_err(|e| corrupt(e.0))?;
        Ok(out)
    }
}

/// `pcm-recons`.
pub mod pcm {
    use super::{Error, Result};
    use crate::audio::pcm::{self, Format, N_BYTE_STREAMS};
    use crate::audio::residual::{self, Coded};
    use aqr_api::stream::{In, Pull, Written};
    use aqr_api::{Memory, Params, Resource};
    use std::borrow::Cow;

    /// The tasks of the module's one gang (aqr-modules' `pcm-recons`): the LMS stage and the
    /// workers.
    pub const SLOTS: usize = 8;

    /// Pages of the windows of the module as a part module ([`decode_part`]): of each input and
    /// of the output.
    pub const WINDOW: u32 = 16;

    /// The memory plan of the module as a part module ([`aqr_api::stack`]): the limits of its
    /// instance and its tasks, its initial memory (`initial` bytes) with it, of params `params`,
    /// resources and inputs of these lengths and `len` bytes of samples (`None` where they are not
    /// so): the tables, the pieces of the streams held ahead, and the pipeline ([`pcm::plan`]); and
    /// what its channels' messages (in the instance's) may hold. Whatever the lengths of the
    /// inputs and the samples.
    pub fn plan(params: &[u8], resources: &[u64], inputs: &[u64], len: u64, initial: u64) -> Option<((u64, u64), u64)> {
        let fmt = Format::from_bytes(params)?;
        let (&[res], true) = (resources, inputs.len() == N_BYTE_STREAMS + 3) else { return None };
        Some(pcm::plan(fmt, len, res, initial, SLOTS - 1))
    }

    /// The module: the `len` bytes of samples from `params` (their format), `resources` (the
    /// tables) and `inputs` (the streams), whole, on `threads` threads.
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
            streams: streams.iter().map(|&s| Cow::Borrowed(s)).collect(),
            words: Cow::Borrowed(words),
            raw: [Cow::Borrowed(raw0), Cow::Borrowed(raw1)],
        };
        let mut out = Vec::new();
        pcm::decode_threads(&c, fmt, &t, len as usize, &mut out, threads).map_err(|e| corrupt(e.0))?;
        if out.len() as u64 != len {
            return Err(corrupt("length"));
        }
        Ok(out)
    }

    /// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`]
    /// gives, its inputs taken and its output put in order through windows ([`WINDOW`]), in
    /// pieces: it holds neither, whatever their lengths.
    pub fn decode_part(params_len: u64, res_len: u64, lens: [u64; 6], size: u64) {
        aqr_api::finish(part(params_len, res_len, lens, size));
    }

    fn part(params_len: u64, res_len: u64, lens: [u64; 6], size: u64) -> Result<()> {
        let corrupt = |s: &str| Error(format!("corrupt: {s}"));
        let mut params = [0; 3];
        let params = params.get_mut(..params_len as usize).ok_or_else(|| corrupt("params"))?;
        Params::read(0, params);
        let fmt = Format::from_bytes(params).ok_or_else(|| corrupt("params"))?;
        if lens[3] % 4 != 0 {
            return Err(corrupt("inputs"));
        }
        // No output is larger than 64 times its coded form.
        let coded_len = lens.iter().sum::<u64>();
        if size > coded_len.saturating_mul(64) + (1 << 20) {
            return Err(Error("length".into()));
        }
        let (mut i0, mut i1, mut i2) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
        let (mut i3, mut i4, mut i5) = (In::<3>::new(lens[3]), In::<4>::new(lens[4]), In::<5>::new(lens[5]));
        let mut out = Written::new(size);
        let tables = || {
            let mut raw = vec![0; usize::try_from(res_len).ok()?];
            Resource::<0>::read(0, &mut raw);
            residual::tables_from(&raw)
        };
        let inputs: [(&mut dyn Pull, u64); 6] = [
            (&mut i0, lens[0]),
            (&mut i1, lens[1]),
            (&mut i2, lens[2]),
            (&mut i3, lens[3]),
            (&mut i4, lens[4]),
            (&mut i5, lens[5]),
        ];
        pcm::decode_part(fmt, size, tables, inputs, &mut out, SLOTS - 1).map_err(|e| corrupt(e.0))
    }
}
