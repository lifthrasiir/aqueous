//! The modules formulas apply, implemented natively by the recompressors (xform-design.md
//! §5.7), and [`decompress`] of an Aqueous archive with them ([`aqr_formula::aqr`]).
//!
//! Formulas are made by aqr-compress; [`Native`] evaluates them.

use std::panic::{AssertUnwindSafe, catch_unwind};

use aqr_exe::arch::{arm64, cil, x86};
use aqr_formula::aqr::{self, Failure, NativeModule};
use aqr_formula::{Apply, Error, Module, Modules, Result, Shared};

/// aqr-exe's x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; inputs: the streams of
/// [`x86::STREAM_NAMES`].
pub const X86: Module = Module { name: "exe-recons/x86", version: 1 };

/// aqr-exe's ARM64 filter on a code section. Parameters: the origin (u64, little endian);
/// inputs: the streams of [`arm64::STREAM_NAMES`].
pub const ARM64: Module = Module { name: "exe-recons/arm64", version: 1 };

/// aqr-exe's CIL filter on the section holding the metadata of a .NET image. Parameters: the
/// section's RVA and the offset of the metadata root in it (u32 each, little endian); inputs:
/// the streams of [`cil::STREAM_NAMES`].
pub const CIL: Module = Module { name: "exe-recons/cil", version: 1 };

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

/// An ar, tar or cpio archive rebuilt from the contents of its members. Inputs: the contents
/// and the layout.
pub const BUNDLE: Module = aqr_bundle::formula::MODULE;

// A recompressor as a module of its own (its `formula` module, see its FORMAT.md), rebuilding
// a file from its inputs. Files embedded in the file are in the formula as applications of
// their own modules.
pub const PNG: Module = aqr_png::formula::MODULE;
/// Sets of PNG files coded against each other, of an output per file.
pub const PNG_SET: Module = aqr_png::formula::SET_MODULE;
pub const JPEG: Module = aqr_jpeg::formula::MODULE;
pub const MP3: Module = aqr_mp3::formula::MODULE;
pub const FLAC: Module = aqr_flac::formula::MODULE;
pub const PSD: Module = aqr_psd::formula::MODULE;
pub const PDF: Module = aqr_pdf::formula::MODULE;
pub const SQLITE: Module = aqr_sqlite::formula::MODULE;
pub const FASTA: Module = aqr_fasta::formula::MODULE;
/// Text of words and markup replaced by codes of a dictionary (aqr-wrt). Resources: the
/// dictionary; inputs: the coded text.
pub const WRT: Module = aqr_wrt::formula::MODULE;

/// aqr-raw's samples of a layout (their params), for the trivial containers around them or
/// for any recompressor that finds them: PCM audio (resources: the residual model's tables,
/// as aqr-flac's) and raw rasters (no resources).
pub const PCM: Module = aqr_raw::formula::pcm::MODULE;
pub const RASTER: Module = aqr_raw::formula::raster::MODULE;

/// Every module of this crate.
pub const MODULES: [Module; 20] = [
    X86, ARM64, CIL, FLATE, LZ4, SNAPPY, ZIP, BUNDLE, PNG, PNG_SET, JPEG, MP3, FLAC, PSD, PDF, SQLITE, FASTA, WRT, PCM,
    RASTER,
];

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
const RECONS: [(Module, Decode); 11] = [
    (PNG, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode(p, i, n, t).map_err(msg))),
    (JPEG, |p, r, i, n, t| none(r).and_then(|_| aqr_jpeg::formula::decode(p, i, n, t).map_err(msg))),
    (MP3, |p, r, i, n, t| aqr_mp3::formula::decode(p, r, i, n, t).map_err(msg)),
    (FLAC, |p, r, i, n, t| aqr_flac::formula::decode(p, r, i, n, t).map_err(msg)),
    (PSD, |p, r, i, n, t| none(r).and_then(|_| aqr_psd::formula::decode(p, i, n, t).map_err(msg))),
    (PDF, |p, r, i, n, t| none(r).and_then(|_| aqr_pdf::formula::decode(p, i, n, t).map_err(msg))),
    (SQLITE, |p, r, i, n, t| none(r).and_then(|_| aqr_sqlite::formula::decode(p, i, n, t).map_err(msg))),
    (FASTA, |p, r, i, n, t| none(r).and_then(|_| aqr_fasta::formula::decode(p, i, n, t).map_err(msg))),
    (WRT, |p, r, i, n, t| aqr_wrt::formula::decode(p, r, i, n, t).map_err(msg)),
    (PCM, |p, r, i, n, t| aqr_raw::formula::pcm::decode(p, r, i, n, t).map_err(msg)),
    (RASTER, |p, r, i, n, t| none(r).and_then(|_| aqr_raw::formula::raster::decode(p, i, n, t).map_err(msg))),
];

/// The file in archive `c`, an archive of a single file.
pub fn decompress(c: &[u8], threads: usize) -> std::result::Result<Vec<u8>, Failure> {
    aqr::decode(c, &Native { threads }.modules())
}

/// What archive `c` holds: a single file, or a tree of them.
pub fn decompress_value(c: &[u8], threads: usize) -> std::result::Result<aqr::Value, Failure> {
    aqr::decode_value_with(c, &Native { threads }.modules(), threads)
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
        } else if m == ARM64 {
            none(resources)?;
            let (Some(p), Ok(streams), Ok(len)) =
                (params.first_chunk::<8>(), <[&[u8]; arm64::NUM_STREAMS]>::try_from(inputs), usize::try_from(len))
            else {
                return Err("bad parameters or inputs".into());
            };
            arm64::decode(streams, u64::from_le_bytes(*p), len).ok_or_else(|| "corrupt".into())
        } else if m == CIL {
            none(resources)?;
            let (Some(p), Ok(streams), Ok(len)) =
                (params.first_chunk::<8>(), <[&[u8]; cil::NUM_STREAMS]>::try_from(inputs), usize::try_from(len))
            else {
                return Err("bad parameters or inputs".into());
            };
            let (origin, meta) =
                (u32::from_le_bytes(p[..4].try_into().unwrap()), u32::from_le_bytes(p[4..].try_into().unwrap()));
            cil::decode(streams, origin, meta as usize, len).ok_or_else(|| "corrupt".into())
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
        } else if m == BUNDLE {
            none(resources)?;
            aqr_bundle::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == PNG_SET {
            none(resources)?;
            Ok(aqr_png::formula::decode_set(params, inputs, &[len], threads).map_err(msg)?.remove(0))
        } else if let Some((_, decode)) = RECONS.iter().find(|r| r.0 == m) {
            decode(params, resources, inputs, len, threads)
        } else {
            Err("unknown module".into())
        }
    }
}

impl Native {
    /// The outputs of `m` on `inputs` with `resources`, of `sizes` bytes (see
    /// [`Modules::apply_shared`]).
    pub fn run_shared(
        &self,
        m: Module,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        sizes: &[u64],
    ) -> Result<Vec<Vec<u8>>> {
        let run = || -> std::result::Result<Vec<Vec<u8>>, String> {
            if m == PNG_SET {
                none(resources)?;
                aqr_png::formula::decode_set(params, inputs, sizes, self.threads.max(1)).map_err(msg)
            } else {
                Err(format!("{} outputs", sizes.len()))
            }
        };
        catch_unwind(AssertUnwindSafe(run))
            .unwrap_or_else(|_| Err("panicked".into()))
            .map_err(|e| Error::new(format!("{m}: {e}")))
    }
}

impl Modules for Native {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        self.run(a.module, &a.params, &resources, inputs, a.len)
    }

    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        let a = &s.apply;
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        self.run_shared(a.module, &a.params, &resources, inputs, &s.sizes)
    }
}
