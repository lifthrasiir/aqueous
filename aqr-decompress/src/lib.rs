//! The modules formulas apply, implemented natively by the recompressors (xform-design.md
//! §5.7), and [`decompress`] of an Aqueous archive with them ([`aqr_formula::aqr`]).
//!
//! Formulas are made by aqr-compress; [`Native`] evaluates them.

use std::panic::{AssertUnwindSafe, catch_unwind};

use aqr_exe::arch::x86;
use aqr_formula::aqr::{self, Failure, NativeModule};
use aqr_formula::{Apply, Error, Module, Modules, Result};

/// aqr-exe's x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; inputs: the streams of
/// [`x86::STREAM_NAMES`].
pub const X86: Module = Module { name: "exe-recons/x86", version: 1 };

/// aqr-flate reconstruction of a raw DEFLATE stream. Inputs: the data and the record.
pub const FLATE: Module = aqr_flate::formula::MODULE;

// A recompressor as a module of its own (its `formula` module, see its FORMAT.md), rebuilding
// a file from its inputs. Files embedded in the file are in the formula as applications of
// their own modules.
pub const PNG: Module = aqr_png::formula::MODULE;
pub const JPEG: Module = aqr_jpeg::formula::MODULE;
pub const MP3: Module = aqr_mp3::formula::MODULE;
pub const FLAC: Module = aqr_flac::formula::MODULE;
pub const PSD: Module = aqr_psd::formula::MODULE;
pub const PDF: Module = aqr_pdf::formula::MODULE;
pub const SQLITE: Module = aqr_sqlite::formula::MODULE;

/// Every module of this crate.
pub const MODULES: [Module; 9] = [X86, FLATE, PNG, JPEG, MP3, FLAC, PSD, PDF, SQLITE];

/// A module's `decode`: params, inputs, output length and threads.
pub type Decode = fn(&[u8], &[&[u8]], u64, usize) -> std::result::Result<Vec<u8>, String>;

fn msg(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

/// The recompressors' own modules, by their `decode`.
const RECONS: [(Module, Decode); 7] = [
    (PNG, |p, i, n, t| aqr_png::formula::decode(p, i, n, t).map_err(msg)),
    (JPEG, |p, i, n, t| aqr_jpeg::formula::decode(p, i, n, t).map_err(msg)),
    (MP3, |p, i, n, t| aqr_mp3::formula::decode(p, i, n, t).map_err(msg)),
    (FLAC, |p, i, n, t| aqr_flac::formula::decode(p, i, n, t).map_err(msg)),
    (PSD, |p, i, n, t| aqr_psd::formula::decode(p, i, n, t).map_err(msg)),
    (PDF, |p, i, n, t| aqr_pdf::formula::decode(p, i, n, t).map_err(msg)),
    (SQLITE, |p, i, n, t| aqr_sqlite::formula::decode(p, i, n, t).map_err(msg)),
];

/// The file in archive `c`.
pub fn decompress(c: &[u8], threads: usize) -> std::result::Result<Vec<u8>, Failure> {
    aqr::decode(c, &Native { threads }.modules())
}

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
    /// Every module, as archives apply them ([`aqr_formula::aqr`]).
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
            aqr_flate::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if let Some((_, decode)) = RECONS.iter().find(|r| r.0 == m) {
            decode(params, inputs, len, threads)
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
