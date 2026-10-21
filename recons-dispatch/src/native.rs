//! The modules formulas apply, implemented natively by the recompressors (§5.7).

use std::panic::{AssertUnwindSafe, catch_unwind};

use exe_recons::arch::x86;
use recons_formula::aqr::{Failure, NativeModule};
use recons_formula::{Apply, Error, Formula, Module, Modules, Result};

use crate::Nest;

/// exe-recons' x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; inputs: the streams of
/// [`x86::STREAM_NAMES`].
pub const X86: Module = Module { name: "exe-recons/x86", version: 1 };

/// flate-recons reconstruction of a raw DEFLATE stream. Inputs: the data and the record.
pub const FLATE: Module = flate_recons::formula::MODULE;

/// An input of a module's applications that is data as it is, a file or what a file holds,
/// rather than a representation of the module's own. A formula may recompress it in turn (see
/// [`Options::contents`](crate::Options::contents)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub module: Module,
    pub input: usize,
    pub nest: Nest,
}

/// The contents of this crate's modules: the data of [`FLATE`], scanned.
pub const CONTENTS: [Content; 1] = [Content { module: FLATE, input: 0, nest: Nest::Scan }];

/// The parameters of [`X86`].
pub(crate) fn x86_params(origin: u64, sentinel_size: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend_from_slice(&[sentinel_size, is64 as u8]);
    p
}

/// A recompressor as a module of its own (its `formula` module, see its FORMAT.md): the
/// formula it makes of a file, and the module rebuilding the file from its inputs. Files
/// embedded in the file are in the formula as applications of their own modules.
pub struct Recons {
    pub module: Module,
    pub encode: fn(&[u8], usize) -> Option<Formula>,
    pub decode: Decode,
}

/// A module's `decode`: params, inputs, output length and threads.
pub type Decode = fn(&[u8], &[&[u8]], u64, usize) -> std::result::Result<Vec<u8>, String>;

fn msg(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

pub const PNG: Recons = Recons {
    module: png_recons::formula::MODULE,
    encode: png_recons::formula::encode,
    decode: |p, i, n, t| png_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const JPEG: Recons = Recons {
    module: jpeg_recons::formula::MODULE,
    encode: jpeg_recons::formula::encode,
    decode: |p, i, n, t| jpeg_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const MP3: Recons = Recons {
    module: mp3_recons::formula::MODULE,
    encode: mp3_recons::formula::encode,
    decode: |p, i, n, t| mp3_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const FLAC: Recons = Recons {
    module: flac_recons::formula::MODULE,
    encode: flac_recons::formula::encode,
    decode: |p, i, n, t| flac_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const PSD: Recons = Recons {
    module: psd_recons::formula::MODULE,
    encode: psd_recons::formula::encode,
    decode: |p, i, n, t| psd_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const PDF: Recons = Recons {
    module: pdf_recons::formula::MODULE,
    encode: pdf_recons::formula::encode,
    decode: |p, i, n, t| pdf_recons::formula::decode(p, i, n, t).map_err(msg),
};
pub const SQLITE: Recons = Recons {
    module: sqlite_recons::formula::MODULE,
    encode: sqlite_recons::formula::encode,
    decode: |p, i, n, t| sqlite_recons::formula::decode(p, i, n, t).map_err(msg),
};

pub const RECONS: [&Recons; 7] = [&PNG, &JPEG, &MP3, &FLAC, &PSD, &PDF, &SQLITE];

/// Every module of this crate.
pub const MODULES: [Module; 9] =
    [X86, FLATE, PNG.module, JPEG.module, MP3.module, FLAC.module, PSD.module, PDF.module, SQLITE.module];

/// Runs every module of this crate natively. The output does not depend on `threads`.
#[derive(Debug, Clone, Copy)]
pub struct Native {
    pub threads: usize,
}

impl Default for Native {
    fn default() -> Native {
        Native { threads: 1 }
    }
}

impl Native {
    /// Every module, as archives apply them ([`recons_formula::aqr`]).
    pub fn modules(&self) -> Vec<Box<dyn NativeModule>> {
        MODULES.iter().map(|&module| Box::new(Applied { native: *self, module }) as Box<dyn NativeModule>).collect()
    }

    /// The output of `m` on `inputs` (see [`Modules::apply`]).
    pub fn run(&self, m: Module, params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
        catch_unwind(AssertUnwindSafe(|| self.run_unchecked(m, params, inputs, len)))
            .unwrap_or_else(|_| Err("panicked".into()))
            .map_err(|e| Error::new(format!("{m}: {e}")))
    }

    fn run_unchecked(
        &self,
        m: Module,
        params: &[u8],
        inputs: &[&[u8]],
        len: u64,
    ) -> std::result::Result<Vec<u8>, String> {
        let threads = self.threads.max(1);
        if m == X86 {
            let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs))
            else {
                return Err("bad parameters or inputs".into());
            };
            let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
            x86::decode(&x86::DecodedStreams::new(origin, p[8], streams), p[9] != 0).ok_or_else(|| "corrupt".into())
        } else if m == FLATE {
            flate_recons::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if let Some(r) = RECONS.iter().find(|r| r.module == m) {
            (r.decode)(params, inputs, len, threads)
        } else {
            Err("unknown module".into())
        }
    }
}

impl Modules for Native {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        self.run(a.module, &a.params, inputs, a.len)
    }
}

/// A module of this crate as an archive applies it: without resources, and with one output.
struct Applied {
    native: Native,
    module: Module,
}

impl NativeModule for Applied {
    fn module(&self) -> Module {
        self.module
    }

    fn apply(
        &self,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        sizes: &[u64],
    ) -> std::result::Result<Vec<Vec<u8>>, Failure> {
        let (&[len], []) = (sizes, resources) else {
            return Err(Failure::Invalid(format!(
                "{}: {} outputs, {} resources",
                self.module,
                sizes.len(),
                resources.len()
            )));
        };
        self.native.run(self.module, params, inputs, len).map(|out| vec![out]).map_err(|e| Failure::Invalid(e.0))
    }
}
