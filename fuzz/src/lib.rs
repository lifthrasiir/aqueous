//! What the fuzz targets share: a runner of toy modules, known by their stand-ins
//! ([`aqr_archive::stand_in`]), so that formulas applying modules are evaluated without the
//! codecs (whose own decoding wants targets of its own).

use aqr_archive::{Natives, PartIn, PartOut, natives};
use aqr_formula::{Apply, Error, Module, Result, Shared};

/// The concatenation of its inputs reversed.
pub const REV: Module = Module::new("rev", 1);
/// The concatenation of its inputs cut into the outputs' sizes.
pub const CUT: Module = Module::new("cut", 1);
/// The concatenation of its inputs, as a part module (streamed).
pub const COPY: Module = Module::new("copy", 1);

/// The most bytes a toy application makes: a declared size over it is the fuzzer's, not a
/// bug of the decoder's (which leaves sizes to the modules' memory limits).
pub const OUTPUT_MAX: u64 = 1 << 24;

/// The largest allocation the targets make (their global allocator, `aqr_testkit::Refusing`,
/// refusing those of it and above as an exhausted heap does): what tries a reservation goes on
/// without it, what does not aborts, which the fuzzer takes for a crash. libFuzzer's own
/// `-malloc_limit_mb` would take a reservation tried for one too.
pub const ALLOCATION_MAX: usize = 1 << 30;

pub struct Toys;

fn bounded(n: u64) -> Result<()> {
    match n <= OUTPUT_MAX {
        true => Ok(()),
        false => Err(Error::new("output over the toys' bound")),
    }
}

impl aqr_archive::Modules for Toys {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        bounded(a.len)?;
        match a.module.name {
            "rev" => Ok(inputs.concat().into_iter().rev().collect()),
            "copy" => Ok(inputs.concat()),
            _ => Err(Error::new("not a toy of one output")),
        }
    }

    fn is_part(&self, a: &Apply) -> bool {
        a.module == COPY
    }

    fn apply_part(&self, a: &Apply, inputs: &mut [&mut dyn PartIn], output: &mut dyn PartOut) -> Result<()> {
        bounded(a.len)?;
        for i in inputs {
            let mut failed = None;
            let n = i.len();
            i.pull(n, &mut |d| {
                if failed.is_none()
                    && let Err(e) = output.push(d)
                {
                    failed = Some(e);
                }
            })
            .map_err(|e| Error::new(e.to_string()))?;
            if let Some(e) = failed {
                return Err(Error::new(e.to_string()));
            }
        }
        Ok(())
    }

    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        if s.apply().is_none_or(|a| a.module != CUT) {
            return Err(Error::new("not cut"));
        }
        bounded(s.sizes.iter().fold(0u64, |a, &n| a.saturating_add(n)))?;
        let (all, mut at) = (inputs.concat(), 0usize);
        Ok(s.sizes
            .iter()
            .map(|&n| {
                let a = at.min(all.len());
                at = at.saturating_add(n as usize);
                all[a..at.min(all.len())].to_vec()
            })
            .collect())
    }
}

pub fn toys() -> Natives<'static, Toys> {
    natives(&[REV, CUT, COPY], &Toys)
}
