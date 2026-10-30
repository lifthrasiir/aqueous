//! The modules formulas apply, implemented natively by the recompressors (docs/xform-design.md
//! §5.7), and [`decompress`] of an Aqueous archive with them ([`aqr_formula::aqr`]).
//!
//! Formulas are made by aqr-compress; [`Native`] evaluates them. In archives, a module is its
//! Wasm module, built from the same code by aqr-modules: [`Native`] runs those it knows (by
//! their hashes, [`identify`]) natively, and any other as WebAssembly ([`Wasm`], with the
//! feature `wasm`), or so in a worker process locked down by the system ([`sandbox`], with the
//! feature `sandbox`).

use std::panic::{AssertUnwindSafe, catch_unwind};

use aqr_formula::Resource;
use aqr_formula::aqr::{self, Failure, Loaded, Runner};
use sha2::{Digest, Sha512_256};

#[cfg(feature = "wasm")]
mod cow;
#[cfg(feature = "wasm")]
mod exec;
#[cfg(feature = "sandbox")]
pub mod sandbox;
#[cfg(feature = "wasm")]
mod wasm;
use aqr_formula::{Apply, Error, Module, Modules, Result};
#[cfg(feature = "wasm")]
pub use wasm::{Applied, initial_pages, record_applications};

/// aqr-exe's x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; resources: the opcode classes
/// (`x86::Opcodes`); inputs: the streams of `x86::STREAM_NAMES`.
pub const X86: Module = Module::new("exe-recons/x86", 3).with_memory(exe);

/// aqr-exe's ARM64 filter on a code section. Parameters: the origin (u64, little endian);
/// inputs: the streams of `arm64::STREAM_NAMES`.
pub const ARM64: Module = Module::new("exe-recons/arm64", 1).with_memory(exe);

/// aqr-exe's CIL filter on the section holding the metadata of a .NET image. Parameters: the
/// section's RVA and the offset of the metadata root in it (u32 each, little endian); inputs:
/// the streams of `cil::STREAM_NAMES`.
pub const CIL: Module = Module::new("exe-recons/cil", 1).with_memory(cil);

/// The memory of [`CIL`]: the whole of it, with what the decoder keeps besides the output: the
/// root's stream headers (16 bytes each, at least 12 bytes of the metadata stream per header);
/// per method body (one per varint of the body positions' stream, counted), its range among
/// those taken (8 bytes) and its position (4), and the ranges of the root's streams; the column
/// of a table's values (4 bytes a row, reserved for the largest table: at most as many rows as
/// the tables' stream has bytes and the output has); and the tokens' caches of the 256 tables
/// (24 bytes each, and 1 KiB of each table a token is of, at most one a token's table byte).
fn cil(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    use aqr_exe::arch::cil::{BODYPOS, META, TABLES, TOKT};
    let (&meta, &tables, &tokt) = (u.inputs.get(META)?, u.inputs.get(TABLES)?, u.inputs.get(TOKT)?);
    let bodies = u.content(BODYPOS)?.iter().filter(|&&b| b < 0x80).count() as u64;
    let output = u.output();
    let roots = 16 * (meta / 12 + 1);
    let method_bodies = (8 + 4) * bodies + 8 * (meta / 12 + 2);
    let column = 4 * tables.min(output);
    let caches = 256 * 24 + 1024 * tokt.min(256);
    Some(u.whole(roots + method_bodies + column + caches, 4))
}

/// aqr-exe's base relocations of a PE image. Parameters: `log2` of what the blocks' sizes are
/// multiples of (u8); inputs: the streams of `reloc::STREAM_NAMES`.
pub const PE_RELOC: Module = Module::new("exe-recons/pe-reloc", 1).with_memory(exe);

/// The memory of aqr-exe's x86, ARM64 and relocation filters: the streams and the output, and
/// the small (the x86 opcode classes and call cache).
fn exe(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.whole(0, 0))
}

/// aqr-flate reconstruction of raw DEFLATE streams, one after another, and what is derived of
/// them (CRC-32 and sizes, by its parameters). Resources: the model data of the records'
/// profile; inputs: the data, the records and the index cutting them into the streams'.
pub const FLATE: Module = aqr_flate::formula::MODULE;

/// liblz4's fast compressor on data, for an LZ4 block or frame. Parameters: the kind of stream
/// and the compressor's settings; inputs: the data.
pub const LZ4: Module = aqr_lz4::formula::MODULE;

/// aqr-lzma reconstruction of LZMA streams, one after another, from their data and records.
/// Inputs: the data, the records and the index cutting them into the streams'.
pub const LZMA: Module = aqr_lzma::formula::MODULE;

/// aqr-lzo reconstruction of LZO1X streams, one after another, from their data and records.
/// Inputs: the data, the records and the index cutting them into the streams'.
pub const LZO: Module = aqr_lzo::formula::MODULE;

/// Snappy's compressor on data, for a Snappy stream. Parameters: the compressor's hash; inputs:
/// the data.
pub const SNAPPY: Module = aqr_snappy::formula::MODULE;

/// Snappy's compressor on the contents of the blocks of a LevelDB table, for the table.
/// Parameters: the compressor's hash; inputs: the contents, the layout and the blocks kept.
pub const LEVELDB: Module = aqr_snappy::formula::TABLE_MODULE;

/// A ZIP archive rebuilt from the contents of its entries. Inputs: the contents of the stored
/// bodies, the deflated bodies with what [`FLATE`] derives of them, the layout and the bytes
/// kept.
pub const ZIP: Module = aqr_zip::formula::MODULE;

/// An ar, tar or cpio archive rebuilt from the contents of its members. Inputs: the contents
/// and the layout.
pub const BUNDLE: Module = aqr_bundle::formula::MODULE;

/// A git pack rebuilt from the data of its entries. Inputs: the entries' DEFLATE streams with
/// what [`FLATE`] derives of them, the layout and the bytes kept.
pub const GIT_PACK: Module = aqr_git::formula::MODULE;
/// A git pack as [`GIT_PACK`] makes it, with its index and reverse index after it. Params: what
/// follows and the pack's length; inputs: [`GIT_PACK`]'s, and the ids of the entries in order.
pub const GIT_PACK_INDEX: Module = aqr_git::formula::INDEX_MODULE;

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
/// A Clip Studio Paint file: its skeleton, its tiles coded and their zlib streams made again,
/// and its database (an application of aqr-sqlite's module).
pub const CLIP: Module = aqr_clip::formula::MODULE;
/// The blocks of a BC1 to BC5 texture of a layout (its params), for a DDS file's header around
/// them or for any recompressor that finds them (aqr-tex).
pub const BCN: Module = aqr_tex::formula::MODULE;
pub const PDF: Module = aqr_pdf::formula::MODULE;
pub const SQLITE: Module = aqr_sqlite::formula::MODULE;
pub const FASTA: Module = aqr_fasta::formula::MODULE;

/// aqr-gnu-gettext's `.mo` files from the file without its tables and the NULs in its strings.
/// Parameters: where the strings start; no resources.
pub const GNU_GETTEXT: Module = aqr_gnu_gettext::formula::MODULE;
/// The data of a FITS file's HDUs (aqr-fits): an image, a tile-compressed image of Rice tiles,
/// the rows of a binary table.
pub const FITS_IMAGE: Module = aqr_fits::formula::IMAGE;
pub const FITS_RICE: Module = aqr_fits::formula::RICE;
pub const FITS_TABLE: Module = aqr_fits::formula::TABLE;
pub const OBJ: Module = aqr_obj::formula::MODULE;
/// aqr-vorbis's Vorbis packets (each after its 16-bit length) from the stream's setup header and
/// the record of their values. Parameters: the channels and block sizes; no resources.
pub const VORBIS: Module = aqr_vorbis::formula::MODULE;
/// aqr-ima's IMA ADPCM blocks from the record of their headers and nibbles. Parameters: the
/// channels and bytes per channel; no resources.
pub const IMA: Module = aqr_ima::formula::MODULE;
/// The chunks of an HDF5 dataset of floats from aqr-hdf5's float model (their order's
/// coefficients, the arithmetic code and the raw bits). Parameters: the chunks' layout; no
/// resources.
pub const HDF5_FLOAT: Module = aqr_hdf5::formula::FLOAT;
/// A delimited text table (CSV, TSV, USV and the like) from its columns (aqr-xsv).
pub const XSV: Module = aqr_xsv::formula::MODULE;
/// Text of words and markup replaced by codes of a dictionary (aqr-wrt). Resources: the
/// dictionary; inputs: the coded text.
pub const WRT: Module = aqr_wrt::formula::MODULE;
/// Text of Hangul, its syllables in two bytes and its words replaced by codes of a dictionary
/// (aqr-wrt). Resources: the dictionary; inputs: the coded text, in the pieces of the params.
pub const WRT_HANGUL: Module = aqr_wrt::formula::HANGUL;

/// aqr-raw's samples of a layout (their params), for the trivial containers around them or
/// for any recompressor that finds them: PCM audio (resources: the residual model's tables,
/// as aqr-flac's) and raw rasters (no resources).
pub const PCM: Module = aqr_raw::formula::pcm::MODULE;
pub const RASTER: Module = aqr_raw::formula::raster::MODULE;
pub const RASTER_16: Module = aqr_raw::formula::raster16::MODULE;
/// Records from the byte planes of their fields, a transpose for any recompressor to apply to
/// records of a fixed size, entries from their heads and tails by the lengths in the heads, and
/// records' floats from the whole numbers they are multiples of (aqr-infer-recs, which finds
/// them in files of unknown formats). No resources.
pub const RECS_FIELDS: Module = aqr_infer_recs::formula::FIELDS;
pub const RECS_ENTRIES: Module = aqr_infer_recs::formula::ENTRIES;
pub const RECS_FLOATS: Module = aqr_infer_recs::formula::FLOATS;

/// Every module of this crate.
pub const MODULES: [Module; 43] = [
    X86,
    ARM64,
    CIL,
    PE_RELOC,
    FLATE,
    LZ4,
    LZMA,
    LZO,
    SNAPPY,
    LEVELDB,
    ZIP,
    BUNDLE,
    GIT_PACK,
    GIT_PACK_INDEX,
    PNG,
    PNG_AGAINST,
    PNG_SET,
    JPEG,
    MP3,
    FLAC,
    PSD,
    CLIP,
    BCN,
    PDF,
    SQLITE,
    FASTA,
    GNU_GETTEXT,
    FITS_IMAGE,
    FITS_RICE,
    FITS_TABLE,
    OBJ,
    VORBIS,
    IMA,
    HDF5_FLOAT,
    XSV,
    WRT,
    WRT_HANGUL,
    PCM,
    RASTER,
    RASTER_16,
    RECS_FIELDS,
    RECS_ENTRIES,
    RECS_FLOATS,
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
const RECONS: [(Module, Decode); 29] = [
    (PNG, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode(p, i, n, t).map_err(msg))),
    (PNG_AGAINST, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode_against(p, i, n, t).map_err(msg))),
    (PNG_SET, |p, r, i, n, t| none(r).and_then(|_| aqr_png::formula::decode_set(p, i, n, t).map_err(msg))),
    (JPEG, |p, r, i, n, t| none(r).and_then(|_| aqr_jpeg::formula::decode(p, i, n, t).map_err(msg))),
    (MP3, |p, r, i, n, t| aqr_mp3::formula::decode(p, r, i, n, t).map_err(msg)),
    (FLAC, |p, r, i, n, t| aqr_flac::formula::decode(p, r, i, n, t).map_err(msg)),
    (PSD, |p, r, i, n, t| none(r).and_then(|_| aqr_psd::formula::decode(p, i, n, t).map_err(msg))),
    (CLIP, |p, r, i, n, t| none(r).and_then(|_| aqr_clip::formula::decode(p, i, n, t).map_err(msg))),
    (BCN, |p, r, i, n, t| none(r).and_then(|_| aqr_tex::formula::decode(p, i, n, t).map_err(msg))),
    (PDF, |p, r, i, n, t| none(r).and_then(|_| aqr_pdf::formula::decode(p, i, n, t).map_err(msg))),
    (SQLITE, |p, r, i, n, t| none(r).and_then(|_| aqr_sqlite::formula::decode(p, i, n, t).map_err(msg))),
    (FASTA, |p, r, i, n, t| none(r).and_then(|_| aqr_fasta::formula::decode(p, i, n, t).map_err(msg))),
    (GNU_GETTEXT, |p, r, i, n, _| none(r).and_then(|_| aqr_gnu_gettext::formula::decode(p, i, n).map_err(msg))),
    (FITS_IMAGE, |p, r, i, n, _| none(r).and_then(|_| aqr_fits::formula::decode_image(p, i, n).map_err(msg))),
    (FITS_RICE, |p, r, i, n, _| none(r).and_then(|_| aqr_fits::formula::decode_rice(p, i, n).map_err(msg))),
    (FITS_TABLE, |p, r, i, n, _| none(r).and_then(|_| aqr_fits::formula::decode_table(p, i, n).map_err(msg))),
    (OBJ, |p, r, i, n, t| none(r).and_then(|_| aqr_obj::formula::decode(p, i, n, t).map_err(msg))),
    (VORBIS, |p, r, i, n, _| none(r).and_then(|_| aqr_vorbis::formula::decode(p, i, n).map_err(msg))),
    (IMA, |p, r, i, n, _| none(r).and_then(|_| aqr_ima::formula::decode(p, i, n).map_err(msg))),
    (HDF5_FLOAT, |p, r, i, n, _| none(r).and_then(|_| aqr_hdf5::module::float(p, i, n).map_err(msg))),
    (XSV, |p, r, i, n, t| none(r).and_then(|_| aqr_xsv::formula::decode(p, i, n, t).map_err(msg))),
    (WRT, |p, r, i, n, t| aqr_wrt::formula::decode(p, r, i, n, t).map_err(msg)),
    (WRT_HANGUL, |p, r, i, n, t| aqr_wrt::formula::decode_hangul(p, r, i, n, t).map_err(msg)),
    (PCM, |p, r, i, n, t| aqr_raw::formula::pcm::decode(p, r, i, n, t).map_err(msg)),
    (RASTER, |p, r, i, n, t| none(r).and_then(|_| aqr_raw::formula::raster::decode(p, i, n, t).map_err(msg))),
    (RASTER_16, |p, r, i, n, _| none(r).and_then(|_| aqr_raw::formula::raster16::decode(p, i, n).map_err(msg))),
    (RECS_FIELDS, |p, r, i, n, _| none(r).and_then(|_| aqr_infer_recs::formula::decode_fields(p, i, n).map_err(msg))),
    (RECS_ENTRIES, |p, r, i, n, _| none(r).and_then(|_| aqr_infer_recs::formula::decode_entries(p, i, n).map_err(msg))),
    (RECS_FLOATS, |p, r, i, n, _| none(r).and_then(|_| aqr_infer_recs::formula::decode_floats(p, i, n).map_err(msg))),
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

/// The pages of the working memory of the Wasm module of `m` (aqr-modules') as it is
/// instantiated, for its memory estimate ([`aqr_formula::Usage::initial_pages`]); read once.
#[cfg(feature = "wasm")]
pub fn initial_pages_of(m: Module) -> Option<u64> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Pages = HashMap<(&'static str, u16), Option<u64>>;
    static PAGES: Mutex<Option<Pages>> = Mutex::new(None);
    let k = built(m)?;
    let mut pages = PAGES.lock().unwrap();
    *pages
        .get_or_insert_with(HashMap::new)
        .entry((k.name, k.version))
        .or_insert_with(|| aqr_formula::aqr::module_of_block_data(k.block).and_then(|wasm| wasm::initial_pages(&wasm)))
}

/// The name of the module of `wasm` ([`known`]), for [`aqr::Natives::identified_by`].
pub fn identify(wasm: &[u8]) -> Option<String> {
    known(wasm).map(|m| m.to_string())
}

/// The Wasm module of `m` that aqr-modules built, for [`aqr::Encoder::with_modules`].
pub fn wasm_of(m: Module) -> Option<aqr::Wasm> {
    use std::borrow::Cow;
    let k = built(m)?;
    Some(aqr::Wasm::Block(Cow::Borrowed(k.block)))
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
            aqr_exe::module::x86(params, resources, inputs, len).map_err(str::to_owned)
        } else if m == ARM64 {
            none(resources)?;
            aqr_exe::module::arm64(params, inputs, len).map_err(str::to_owned)
        } else if m == CIL {
            none(resources)?;
            aqr_exe::module::cil(params, inputs, len).map_err(str::to_owned)
        } else if m == PE_RELOC {
            none(resources)?;
            aqr_exe::module::reloc(params, inputs, len).map_err(str::to_owned)
        } else if m == FLATE {
            aqr_flate::formula::decode(params, resources, inputs, len, threads).map_err(msg)
        } else if m == LZ4 {
            none(resources)?;
            aqr_lz4::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == LZMA {
            none(resources)?;
            aqr_lzma::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == LZO {
            none(resources)?;
            aqr_lzo::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == SNAPPY {
            none(resources)?;
            aqr_snappy::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == LEVELDB {
            none(resources)?;
            aqr_snappy::formula::decode_table(params, inputs, len, threads).map_err(msg)
        } else if m == ZIP {
            none(resources)?;
            aqr_zip::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == BUNDLE {
            none(resources)?;
            aqr_bundle::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == GIT_PACK {
            none(resources)?;
            aqr_git::formula::decode(params, inputs, len, threads).map_err(msg)
        } else if m == GIT_PACK_INDEX {
            none(resources)?;
            aqr_git::formula::decode_index(params, inputs, len, threads).map_err(msg)
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
