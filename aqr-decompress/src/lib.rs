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

/// aqr-flate reconstruction of a raw DEFLATE stream. Resources: the model data of the record's
/// profile; inputs: the data and the record.
pub const FLATE: Module = aqr_flate::formula::MODULE;

/// liblz4's fast compressor on data, for an LZ4 block or frame. Parameters: the kind of stream
/// and the compressor's settings; inputs: the data.
pub const LZ4: Module = aqr_lz4::formula::MODULE;

/// Snappy's compressor on data, for a Snappy stream, or on the contents of the blocks of a
/// LevelDB table, for the table. Parameters: the kind and the compressor's hash; inputs: the
/// data, or the contents, the layout and the blocks kept of the table.
pub const SNAPPY: Module = aqr_snappy::formula::MODULE;

/// A ZIP archive rebuilt from the contents of its entries, its DEFLATE bodies by aqr-flate.
/// Resources: aqr-flate's model data of the records' profile, or none; inputs: the contents,
/// the records, the layout and the bytes kept.
pub const ZIP: Module = aqr_zip::formula::MODULE;

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
pub const MODULES: [Module; 12] = [X86, FLATE, LZ4, SNAPPY, ZIP, PNG, JPEG, MP3, FLAC, PSD, PDF, SQLITE];

/// A module's `decode`: params, resources, inputs, output length and threads.
pub type Decode = fn(&[u8], &[&[u8]], &[&[u8]], u64, usize) -> std::result::Result<Vec<u8>, String>;

fn msg(e: impl std::fmt::Debug) -> String {
    format!("{e:?}")
}

/// No resources, for the modules that take none.
fn none(r: &[&[u8]]) -> std::result::Result<(), String> {
    if r.is_empty() { Ok(()) } else { Err("resources given".into()) }
}

/// The recompressors' own modules, by their `decode`.
const RECONS: [(Module, Decode); 7] = [
    (PNG, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode(p, i, n, t).map_err(msg))),
    (JPEG, |p, r, i, n, t| none(r).and_then(|_| aqr_jpeg::formula::decode(p, i, n, t).map_err(msg))),
    (MP3, |p, r, i, n, t| aqr_mp3::formula::decode(p, r, i, n, t).map_err(msg)),
    (FLAC, |p, r, i, n, t| aqr_flac::formula::decode(p, r, i, n, t).map_err(msg)),
    (PSD, |p, r, i, n, t| none(r).and_then(|_| aqr_psd::formula::decode(p, i, n, t).map_err(msg))),
    (PDF, |p, r, i, n, t| none(r).and_then(|_| aqr_pdf::formula::decode(p, i, n, t).map_err(msg))),
    (SQLITE, |p, r, i, n, t| none(r).and_then(|_| aqr_sqlite::formula::decode(p, i, n, t).map_err(msg))),
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
    pub fn modules(&self) -> Vec<Box<dyn NativeModule + '_>> {
        aqr::natives(&MODULES, self)
    }

    /// The output of `m` on `inputs` with `resources` (see [`Modules::apply`]).
    pub fn run(&self, m: Module, params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
        catch_unwind(AssertUnwindSafe(|| self.run_unchecked(m, params, resources, inputs, len)))
            .unwrap_or_else(|_| Err("panicked".into()))
            .map_err(|e| Error::new(format!("{m}: {e}")))
    }

    fn run_unchecked(
        &self,
        m: Module,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        len: u64,
    ) -> std::result::Result<Vec<u8>, String> {
        let threads = self.threads.max(1);
        if m == X86 {
            none(resources)?;
            let (Some(p), Ok(streams)) = (params.first_chunk::<10>(), <[&[u8]; x86::NUM_STREAMS]>::try_from(inputs))
            else {
                return Err("bad parameters or inputs".into());
            };
            let origin = u64::from_le_bytes(*p.first_chunk().unwrap());
            x86::decode(&x86::DecodedStreams::new(origin, p[8], streams), p[9] != 0).ok_or_else(|| "corrupt".into())
        } else if m == FLATE {
            aqr_flate::formula::decode(params, resources, inputs, len, threads).map_err(msg)
        } else if m == LZ4 {
            none(resources)?;
            aqr_lz4::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == SNAPPY {
            none(resources)?;
            aqr_snappy::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == ZIP {
            aqr_zip::formula::decode(params, resources, inputs, len, threads).map_err(msg)
        } else if let Some((_, decode)) = RECONS.iter().find(|r| r.0 == m) {
            decode(params, resources, inputs, len, threads)
        } else {
            Err("unknown module".into())
        }
    }
}

impl Modules for Native {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        self.run(a.module, &a.params, &resources, inputs, a.len)
    }
}
