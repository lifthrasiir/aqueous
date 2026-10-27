//! The modules formulas apply, implemented natively by the recompressors (xform-design.md
//! §5.7), and [`decompress`] of an Aqueous archive with them ([`aqr_formula::aqr`]).
//!
//! Formulas are made by aqr-compress; [`Native`] evaluates them. In archives, a module is its
//! Wasm module, built from the same code by aqr-modules: [`Native`] runs those it knows (by
//! their hashes, [`identify`]) natively, and any other as WebAssembly ([`Wasm`], with the
//! feature `wasm`).

use std::panic::{AssertUnwindSafe, catch_unwind};

use aqr_formula::Resource;
use aqr_formula::aqr::{self, Failure, Loaded, Runner};
use sha2::{Digest, Sha512_256};

#[cfg(feature = "wasm")]
mod wasm;
use aqr_formula::{Apply, Error, Module, Modules, Result};

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

/// Snappy's compressor on data, for a Snappy stream. Parameters: the compressor's hash; inputs:
/// the data.
pub const SNAPPY: Module = aqr_snappy::formula::MODULE;

/// Snappy's compressor on the contents of the blocks of a LevelDB table, for the table.
/// Parameters: the compressor's hash; inputs: the contents, the layout and the blocks kept.
pub const LEVELDB: Module = aqr_snappy::formula::TABLE_MODULE;

/// A ZIP archive rebuilt from the contents of its entries, its DEFLATE bodies by aqr-flate.
/// Resources: aqr-flate's model data of the records' profile; inputs: the contents,
/// the records, the layout and the bytes kept.
pub const ZIP: Module = aqr_zip::formula::MODULE;
/// A ZIP archive without deflated bodies, rebuilt from the contents of its entries. Inputs: the
/// contents, the records (none), the layout and the bytes kept.
pub const ZIP_STORED: Module = aqr_zip::formula::STORED_MODULE;

/// An ar, tar or cpio archive rebuilt from the contents of its members. Inputs: the contents
/// and the layout.
pub const BUNDLE: Module = aqr_bundle::formula::MODULE;

// A recompressor as a module of its own (its `formula` module, see its FORMAT.md), rebuilding
// a file from its inputs. Files embedded in the file are in the formula as applications of
// their own modules.
pub const PNG: Module = aqr_png::formula::MODULE;
/// A PNG file coded against a reference file.
pub const PNG_AGAINST: Module = aqr_png::formula::AGAINST_MODULE;
/// Sets of PNG files coded against each other, the files one after another.
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
pub const MODULES: [Module; 23] = [
    X86,
    ARM64,
    CIL,
    FLATE,
    LZ4,
    SNAPPY,
    LEVELDB,
    ZIP,
    ZIP_STORED,
    BUNDLE,
    PNG,
    PNG_AGAINST,
    PNG_SET,
    JPEG,
    MP3,
    FLAC,
    PSD,
    PDF,
    SQLITE,
    FASTA,
    WRT,
    PCM,
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
const RECONS: [(Module, Decode); 13] = [
    (PNG, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode(p, i, n, t).map_err(msg))),
    (PNG_AGAINST, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode_against(p, i, n, t).map_err(msg))),
    (PNG_SET, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode_set(p, i, n, t).map_err(msg))),
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
    aqr::decode(c, &Native { threads })
}

/// What archive `c` holds: a single file, or a tree of them.
pub fn decompress_value(c: &[u8], threads: usize) -> std::result::Result<aqr::Value, Failure> {
    aqr::decode_value_with(c, &Native { threads }, threads)
}

/// The module of Wasm module `wasm` among those of this crate, if aqr-modules built it.
pub fn known(wasm: &[u8]) -> Option<Module> {
    let hash: [u8; 32] = Sha512_256::digest(wasm).into();
    let k = aqr_modules::KNOWN.iter().find(|k| k.sha512_256 == hash)?;
    MODULES.iter().copied().find(|m| m.name == k.name && m.version == k.version)
}

/// The Wasm module of `m` that aqr-modules built, if it did.
pub fn built(m: Module) -> Option<&'static aqr_modules::Known> {
    aqr_modules::KNOWN.iter().find(|k| k.name == m.name && k.version == m.version)
}

/// The name of the module of `wasm` ([`known`]), for [`aqr::Natives::identified_by`].
pub fn identify(wasm: &[u8]) -> Option<String> {
    known(wasm).map(|m| m.to_string())
}

/// The Wasm module of `m` that aqr-modules built, for [`aqr::Encoder::with_modules`].
pub fn wasm_of(m: Module) -> Option<aqr::Wasm> {
    use std::borrow::Cow;
    let k = built(m)?;
    Some(aqr::Wasm { bytes: Cow::Borrowed(k.wasm), block: Some(Cow::Borrowed(k.block)) })
}

/// The modules of this crate run by `m` where an archive has those of aqr-modules ([`known`]).
pub fn natives<M: Modules + Sync>(m: &M) -> aqr::Natives<'_, M> {
    aqr::natives(&MODULES, m).identified_by(identify)
}

/// Runs every module as WebAssembly, as the spec defines it, whether known or not.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Wasm;

#[cfg(feature = "wasm")]
impl Runner for Wasm {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        wasm::load(wasm, Sha512_256::digest(wasm).into())
    }
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

/// A module run natively.
struct NativeLoaded<'a> {
    native: &'a Native,
    module: Module,
}

impl Loaded for NativeLoaded<'_> {
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        _: u64,
    ) -> aqr::Decoded<Vec<Vec<u8>>> {
        let &[len] = sizes else { return Err(Failure::Invalid(format!("{}: {} outputs", self.module, sizes.len()))) };
        let resources: Vec<&[u8]> = resources.iter().map(|r| &r[..]).collect();
        let out = self.native.run(self.module, params, &resources, inputs, len);
        Ok(vec![out.map_err(|e| Failure::Invalid(e.0))?])
    }
}

/// The modules of this crate natively where an archive has the Wasm module that aqr-modules
/// built ([`known`]), and any other module as WebAssembly (with the feature `wasm`).
impl Runner for Native {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        if let Some(module) = known(wasm) {
            return Ok(Box::new(NativeLoaded { native: self, module }));
        }
        #[cfg(feature = "wasm")]
        return Wasm.load(wasm);
        #[cfg(not(feature = "wasm"))]
        Err(Failure::Resource("a module not known, which runs only as WebAssembly (the feature wasm)".into()))
    }
}

impl Native {
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
            aqr_exe::module::x86(params, inputs, len).map_err(str::to_owned)
        } else if m == ARM64 {
            none(resources)?;
            aqr_exe::module::arm64(params, inputs, len).map_err(str::to_owned)
        } else if m == CIL {
            none(resources)?;
            aqr_exe::module::cil(params, inputs, len).map_err(str::to_owned)
        } else if m == FLATE {
            aqr_flate::formula::decode(params, resources, inputs, len, threads).map_err(msg)
        } else if m == LZ4 {
            none(resources)?;
            aqr_lz4::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == SNAPPY {
            none(resources)?;
            aqr_snappy::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == LEVELDB {
            none(resources)?;
            aqr_snappy::formula::decode_table(params, inputs, len, threads).map_err(msg)
        } else if m == ZIP {
            aqr_zip::formula::decode(params, resources, inputs, len, threads).map_err(msg)
        } else if m == ZIP_STORED {
            none(resources)?;
            aqr_zip::formula::decode_stored(params, inputs, len, threads).map_err(msg)
        } else if m == BUNDLE {
            none(resources)?;
            aqr_bundle::formula::decode(params, inputs, len, threads).map_err(msg)
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
