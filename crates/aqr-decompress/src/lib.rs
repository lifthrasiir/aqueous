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
use aqr_formula::aqr::{self, Failure, Loaded, PartIn, PartOut, Runner};
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
#[doc(hidden)]
pub use wasm::load_released;
#[cfg(feature = "wasm")]
pub use wasm::set_application_bound;
#[cfg(feature = "wasm")]
pub use wasm::{Applied, initial_pages, record_applications};

/// aqr-exe's x86 disfilter on a code section. Parameters: the origin (u64, little endian),
/// the sentinel size (u8) and 1 for x86-64 or 0 for x86-32; resources: the opcode classes
/// (`x86::Opcodes`); inputs: the streams of `x86::STREAM_NAMES`.
pub const X86: Module = Module::new("exe-recons/x86", 4).with_plan(x86);

/// The memory of [`X86`], a part module: its plan (`aqr_exe::module::x86_part_plan`), whatever the
/// lengths (`None` for parameters of under 10 bytes).
fn x86(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    u.params.get(9)?;
    Some(aqr_exe::module::x86_part_plan(u.initial()))
}

/// aqr-exe's x86 disfilter on a code section, its references remapped to those of another
/// image. Parameters and resources as [`X86`]'s; inputs: the streams of
/// `x86::remap::STREAM_NAMES`.
pub const X86_REMAP: Module = Module::new("exe-recons/x86-remap", 1).with_plan(x86_remap);

/// The memory of [`X86_REMAP`]: its initial memory, and its plan
/// (`aqr_exe::module::x86_remap_plan`).
fn x86_remap(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    use aqr_exe::arch::x86::remap::CALLF;
    let flags = [u.content(CALLF)?, u.content(CALLF + 1)?, u.content(CALLF + 2)?, u.content(CALLF + 3)?];
    Some(u.initial() + aqr_exe::module::x86_remap_plan(u.params, u.resources, u.inputs, flags, u.output())?)
}

/// aqr-exe's ARM64 filter on a code section. Parameters: the origin (u64, little endian);
/// inputs: the streams of `arm64::STREAM_NAMES`.
pub const ARM64: Module = Module::new("exe-recons/arm64", 2).with_plan(arm64);

/// The memory of [`ARM64`], a part module: its plan (`aqr_exe::module::arm64_part_plan`), whatever
/// the lengths (`None` for parameters other than 8 bytes).
fn arm64(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    (u.params.len() == 8).then(|| aqr_exe::module::arm64_part_plan(u.initial()))
}

/// aqr-exe's ARM64 filter on a code section, its BL and ADRP targets remapped to those of
/// another image. Parameters: the origin (u64, little endian); inputs: the streams of
/// `arm64::remap::STREAM_NAMES`.
pub const ARM64_REMAP: Module = Module::new("exe-recons/arm64-remap", 1).with_plan(arm64_remap);

/// The memory of [`ARM64_REMAP`]: its initial memory, and its plan
/// (`aqr_exe::module::arm64_remap_plan`).
fn arm64_remap(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    use aqr_exe::arch::arm64::remap::{ADRPF, ADRPMAP, BLF, BLMAP};
    let flags = [u.content(BLF)?, u.content(ADRPF)?, u.content(BLMAP)?, u.content(ADRPMAP)?];
    Some(u.initial() + aqr_exe::module::arm64_remap_plan(u.params.len() as u64, u.inputs, flags, u.output()))
}

/// aqr-exe's stream of ARM64 code against that of another version (an input of
/// [`ARM64_REMAP`]'s). No parameters; inputs: the other version's stream, then the streams of
/// `arm64::delta::STREAM_NAMES`.
pub const ARM64_DELTA: Module = Module::new("exe-recons/arm64-delta", 1).with_plan(arm64_delta);

/// The memory of [`ARM64_DELTA`]: its initial memory, and its plan
/// (`aqr_exe::module::arm64_delta_plan`).
fn arm64_delta(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + aqr_exe::module::arm64_delta_plan(u.inputs, u.output()))
}

/// aqr-exe's Mach-O file with its code signatures' code slots derived. Parameters: the
/// signatures (`codesign`); inputs: the file without them, then the signatures.
pub const MACHO_CODESIGN: Module = Module::new("exe-recons/macho-codesign", 2).with_plan(macho_codesign);

/// The memory of [`MACHO_CODESIGN`], a part module: its plan (`aqr_exe::module::macho_codesign_part_plan`),
/// whatever the file's length (`None` for params that are not of a file of the output's length).
fn macho_codesign(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    aqr_exe::module::macho_codesign_part_plan(u.params, u.output(), u.initial())
}

/// aqr-exe's CIL filter on the section holding the metadata of a .NET image. Parameters: the
/// section's RVA and the offset of the metadata root in it (u32 each, little endian); inputs:
/// the streams of `cil::STREAM_NAMES`.
pub const CIL: Module = Module::new("exe-recons/cil", 1).with_plan(cil);

/// The memory of [`CIL`]: its initial memory, and its plan (`aqr_exe::module::cil_plan`).
fn cil(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    use aqr_exe::arch::cil::{BODYPOS, META, TOKT};
    let streams = [u.content(META)?, u.content(BODYPOS)?, u.content(TOKT)?];
    Some(u.initial() + aqr_exe::module::cil_plan(u.params.len() as u64, u.inputs, streams, u.output())?)
}

/// aqr-exe's base relocations of a PE image. Parameters: `log2` of what the blocks' sizes are
/// multiples of (u8); inputs: the streams of `reloc::STREAM_NAMES`.
pub const PE_RELOC: Module = Module::new("exe-recons/pe-reloc", 1).with_plan(exe);

/// aqr-exe's symbol table of a 64-bit Mach-O image, and its string table after it. Parameters:
/// the number of entries and the first external one (u32 each, little endian), then the string
/// table's head; inputs: the streams of `symtab::STREAM_NAMES`.
pub const MACHO_SYMTAB: Module = Module::new("exe-recons/macho-symtab", 1).with_plan(macho_symtab);

/// The memory of [`MACHO_SYMTAB`]: its initial memory, and its plan
/// (`aqr_exe::module::macho_symtab_plan`; the streams of the entries' types and names first and
/// sixth).
fn macho_symtab(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (types, names) = (u.content(0)?, u.content(5)?);
    Some(u.initial() + aqr_exe::module::macho_symtab_plan(u.params.len() as u64, u.inputs, types, names, u.output()))
}

/// The memory of aqr-exe's x86, ARM64 and relocation filters: their initial memory, and their
/// plan (`aqr_exe::module::plan`).
fn exe(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial() + aqr_exe::module::plan(u.params.len() as u64, u.resources, u.inputs, u.output()))
}

/// aqr-flate reconstruction of raw DEFLATE streams, one after another, and what is derived of
/// them (CRC-32 and sizes, by its parameters). Resources: the model data of the records'
/// profile; inputs: the data, the records and the index cutting them into the streams'.
pub const FLATE: Module = aqr_flate::formula::MODULE;

/// The data of a raw DEFLATE stream, the one input, for a file that a DEFLATE stream archived
/// along with it holds (not rebuilt).
pub const INFLATE: Module = aqr_flate::formula::INFLATE;

/// liblz4's fast compressor on data, for an LZ4 block or frame. Parameters: the kind of stream
/// and the compressor's settings; inputs: the data.
pub const LZ4: Module = aqr_lz4::formula::MODULE;

/// aqr-lzma reconstruction of LZMA streams, one after another, from their data and records.
/// Inputs: the data, the records and the index cutting them into the streams'.
pub const LZMA: Module = aqr_lzma::formula::MODULE;

/// aqr-lzma reconstruction of the segments of LZMA2 streams (of xz and 7z), one after another,
/// from their data and records. Inputs: the data, the records and the index cutting them into
/// the segments'.
pub const LZMA2: Module = aqr_lzma::formula::LZMA2;

/// The filters of xz and 7z (aqr-bcj): a branch converter's encoding of the data (x86, PowerPC,
/// IA-64, ARM, ARM Thumb, SPARC, ARM64, RISC-V; parameters: the start address, le32 or none),
/// Delta's (parameters: the distance less one), and BCJ2's main, call and jump streams (inputs:
/// the data, and the range coder's stream). No resources.
pub const BCJ_X86: Module = aqr_bcj::formula::X86;
pub const BCJ_POWERPC: Module = aqr_bcj::formula::POWERPC;
pub const BCJ_IA64: Module = aqr_bcj::formula::IA64;
pub const BCJ_ARM: Module = aqr_bcj::formula::ARM;
pub const BCJ_ARM_THUMB: Module = aqr_bcj::formula::ARM_THUMB;
pub const BCJ_SPARC: Module = aqr_bcj::formula::SPARC;
pub const BCJ_ARM64: Module = aqr_bcj::formula::ARM64;
pub const BCJ_RISCV: Module = aqr_bcj::formula::RISCV;
pub const BCJ_DELTA: Module = aqr_bcj::formula::DELTA;
pub const BCJ2: Module = aqr_bcj::formula::BCJ2;

/// aqr-lzo reconstruction of LZO1X streams, one after another, from their data and records.
/// Inputs: the data, the records and the index cutting them into the streams'.
pub const LZO: Module = aqr_lzo::formula::MODULE;

/// aqr-azo reconstruction of AZO streams (ALZip's, in EGG archives), one after another, from
/// their data and records. Inputs: the data, the records and the index cutting them into the
/// streams'.
pub const AZO: Module = aqr_azo::formula::MODULE;

/// aqr-lha reconstruction of the streams of LHA's `-lh4-` to `-lh7-`, one after another, from
/// their data and records. Inputs: the data, the records and the index cutting them into the
/// streams'.
pub const LHA: Module = aqr_lha::formula::MODULE;

/// aqr-rar reconstruction of RAR5 streams (a solid group's files' packed data, or a file's), one
/// after another, from their data and records. Inputs: the data, the records and the index
/// cutting them into the streams'.
pub const RAR5: Module = aqr_rar::formula::MODULE5;

/// aqr-rar reconstruction of the streams of RAR 3.x and 4.x (RAR 2.9's algorithm: LZ and PPMd
/// blocks, filters), one after another, from their data and records. Inputs: as [`RAR5`].
pub const RAR3: Module = aqr_rar::formula::MODULE3;

/// aqr-bvx reconstruction of a stream of Apple's LZFSE and LZVN blocks from its data and
/// record. Inputs: the data and the record.
pub const BVX: Module = aqr_bvx::formula::MODULE;

/// libbzip2's compressor on data, for bzip2 streams one after another. Parameters: each
/// stream's level, bytes of data and the ranks of its periodic blocks. Inputs: the data.
pub const BZ2: Module = aqr_bz2::formula::MODULE;

/// libbzip2's compressor on data, for ALZip's bzip2 streams (ALZ archives' method 1) one after
/// another. Parameters: each stream's bytes of data and the ranks of its periodic blocks.
/// Inputs: the data.
pub const BZ2_ALZ: Module = aqr_bz2::formula::MODULE_ALZ;

/// aqr-lzx reconstruction of streams of LZX chunks (as WIM files hold them), one after another,
/// from their data and records. Inputs: the data, the records and the index cutting them into
/// the streams'.
pub const LZX: Module = aqr_lzx::formula::MODULE;

/// aqr-xpress reconstruction of streams of XPRESS Huffman chunks (WOF's, WIM's, or a buffer's),
/// one after another, from their data and records. Inputs: the data, the records, the bytes kept
/// and the index cutting them into the streams'.
pub const XPRESS: Module = aqr_xpress::formula::MODULE;

/// aqr-lznt1 reconstruction of streams of LZNT1 units (NTFS's compression units, or a buffer's
/// chunks), one after another, from their data and records. Inputs: the data, the records, the
/// bytes kept and the index cutting them into the streams'.
pub const LZNT1: Module = aqr_lznt1::formula::MODULE;

/// A CAB file's MSZIP and stored folders rebuilt from the joined DEFLATE streams of the MSZIP
/// folders (each its blocks' streams as one) and the stored folders' data. Inputs: the joined
/// streams, the stored data, the bytes kept (the data blocks' headers) and the index.
pub const MSZIP: Module = aqr_mszip::formula::MODULE;

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

/// An asar archive rebuilt from the data after its header. Inputs: the data, the header with
/// the values that follow from the data left out, and the index of the files.
pub const ASAR: Module = aqr_asar::formula::MODULE;

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
/// A GIF file from its skeleton and the record of its images.
pub const GIF: Module = aqr_gif::formula::MODULE;
/// A span of a QuickTime or MXF file from its ProRes frames' coefficients, coded, and the bytes
/// between them.
pub const PRORES: Module = aqr_prores::formula::MODULE;
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
/// An Android DEX file from aqr-dex's streams (strings, tables, classes' data and sections). No
/// params or resources.
pub const DEX: Module = aqr_dex::formula::MODULE;
/// An Apple accelerator table of DWARF (`__apple_names` and the like) from aqr-dwarf's streams
/// of its entries and `.debug_str`. Parameters: the table's header; no resources.
pub const DWARF_APPLE: Module = aqr_dwarf::formula::APPLE;
/// DWARF's `.debug_info` from aqr-dwarf's streams of its DIEs, `.debug_abbrev` and `.debug_str`.
/// No params or resources.
pub const DWARF_INFO: Module = aqr_dwarf::formula::INFO;
/// A PDB file (MSF) from aqr-pdb's streams: its streams one after another, its layout and the
/// rest of its blocks. No params or resources.
pub const PDB_MSF: Module = aqr_pdb::formula::MODULE;
/// The symbol substreams of a PDB file's modules from aqr-pdb's streams of their records' fields.
/// No params or resources.
pub const PDB_SYMBOLS: Module = aqr_pdb::formula::SYMBOLS;
/// An Android resource table from aqr-arsc's streams (string pools, entries, chunks). No params
/// or resources.
pub const ARSC: Module = aqr_arsc::formula::MODULE;
/// A WebAssembly module from aqr-wasm's streams (the code by the instructions' fields). No
/// params or resources.
pub const WASM: Module = aqr_wasm::formula::MODULE;

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
/// aqr-ogg's Ogg files from the layout record of their pages and their streams' packets (a
/// Vorbis stream's by [`VORBIS`]). No params or resources.
pub const OGG: Module = aqr_ogg::formula::MODULE;
/// aqr-ima's IMA ADPCM blocks from the record of their headers and nibbles. Parameters: the
/// channels and bytes per channel; no resources.
pub const IMA: Module = aqr_ima::formula::MODULE;
/// The chunks of an HDF5 dataset of floats from aqr-hdf5's float model (their order's
/// coefficients, the arithmetic code and the raw bits). Parameters: the chunks' layout; no
/// resources.
pub const HDF5_FLOAT: Module = aqr_hdf5::formula::FLOAT;
/// A delimited text table (CSV, TSV, USV and the like) from its columns (aqr-xsv).
pub const XSV: Module = aqr_xsv::formula::MODULE;
/// JSON lines from their values stored by key path (aqr-jsonl).
pub const JSONL: Module = aqr_jsonl::formula::MODULE;
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

/// A module's `decode`: params, resources, inputs, output length and threads.
pub type Decode = fn(&[u8], &[&[u8]], &[&[u8]], u64, usize) -> std::result::Result<Vec<u8>, String>;

/// A module of this crate as it runs natively: its `decode`, or the entry point of its code as a
/// part module (applied streamed, [`Loaded::apply_part`], and whole by `aqr_api`'s emulation);
/// and whether it takes resources (those that do not are given none).
#[derive(Clone, Copy)]
struct Entry {
    module: Module,
    resources: bool,
    decode: Run,
}

#[derive(Clone, Copy)]
enum Run {
    Whole(Decode),
    /// And its `decode` applied whole, where it has one (else the emulation's).
    Part(aqr_api::emu::Entry, Option<Decode>),
    /// A part module in Wasm that natively is applied whole, by its threaded `decode` (which
    /// spreads streams and parts over threads; the output is the same as the streamed one's).
    Threaded(Decode),
}

/// [`Entry`] of a module taking resources.
const fn takes(module: Module, decode: Decode) -> Entry {
    Entry { module, resources: true, decode: Run::Whole(decode) }
}

/// [`Entry`] of a module taking none.
const fn bare(module: Module, decode: Decode) -> Entry {
    Entry { module, resources: false, decode: Run::Whole(decode) }
}

/// [`Entry`] of a part module, of the entry point `entry` (`aqr_api::entry!`'s) and `decode`
/// applied whole, taking resources where `resources`.
const fn part(module: Module, resources: bool, entry: aqr_api::emu::Entry, decode: Decode) -> Entry {
    Entry { module, resources, decode: Run::Part(entry, Some(decode)) }
}

/// [`Entry`] of a module that is a part module in Wasm but natively is applied whole, on threads
/// (its `decode`), as streaming gives one thread: the memory of a native run is not held to the
/// module's plan.
const fn threaded(module: Module, decode: Decode) -> Entry {
    Entry { module, resources: false, decode: Run::Threaded(decode) }
}

/// [`threaded`] of a module taking resources.
const fn threaded_takes(module: Module, decode: Decode) -> Entry {
    Entry { module, resources: true, decode: Run::Threaded(decode) }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A filter of aqr-bcj's.
fn bcj(m: Module, p: &[u8], i: &[&[u8]], n: u64) -> std::result::Result<Vec<u8>, String> {
    aqr_bcj::formula::decode(m, p, i, n).ok_or("not of aqr-bcj")?.map_err(err)
}

/// [`Entry`] of a branch converter of aqr-bcj, a part module of windows of a page.
macro_rules! bra {
    ($module:ident, $arch:ident) => {
        part(
            $module,
            false,
            aqr_api::emu::Entry {
                kind: aqr_api::emu::Kind::Part,
                windows: &[
                    aqr_api::Window::Input(0, aqr_bcj::module::BRA_WINDOW),
                    aqr_api::Window::Output(0, aqr_bcj::module::BRA_WINDOW),
                ],
                arity: 3,
                call: |a| aqr_bcj::module::bra_part(aqr_bcj::bra::Arch::$arch, a[0], a[1], a[2]),
            },
            |p, _, i, n, _| bcj($module, p, i, n),
        )
    };
}

/// Every module of this crate, as it runs natively.
const NATIVE: &[Entry] = &[
    part(
        X86,
        true,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(2, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(3, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(4, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(5, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(6, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(7, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(8, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(9, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(10, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(11, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Input(12, aqr_exe::module::X86_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::X86_WINDOW),
            ],
            arity: 16,
            call: |a| {
                aqr_exe::module::x86_part(
                    a[0],
                    a[1],
                    [a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14]],
                    a[15],
                )
            },
        },
        |p, r, i, n, _| aqr_exe::module::x86(p, r, i, n).map_err(err),
    ),
    takes(X86_REMAP, |p, r, i, n, _| aqr_exe::module::x86_remap(p, r, i, n).map_err(err)),
    part(
        ARM64,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(2, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(3, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(4, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(5, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(6, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(7, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(8, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(9, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Input(10, aqr_exe::module::ARM64_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::ARM64_WINDOW),
            ],
            arity: 13,
            call: |a| {
                aqr_exe::module::arm64_part(
                    a[0],
                    [a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]],
                    a[12],
                )
            },
        },
        |p, _, i, n, _| aqr_exe::module::arm64(p, i, n).map_err(err),
    ),
    bare(ARM64_REMAP, |p, _, i, n, _| aqr_exe::module::arm64_remap(p, i, n).map_err(err)),
    bare(ARM64_DELTA, |p, _, i, n, _| aqr_exe::module::arm64_delta(p, i, n).map_err(err)),
    part(
        MACHO_CODESIGN,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_exe::module::MACHO_CODESIGN_WINDOW),
                aqr_api::Window::Input(1, aqr_exe::module::MACHO_CODESIGN_WINDOW),
                aqr_api::Window::Output(0, aqr_exe::module::MACHO_CODESIGN_WINDOW),
            ],
            arity: 4,
            call: |a| aqr_exe::module::macho_codesign_part(a[0], [a[1], a[2]], a[3]),
        },
        |p, _, i, n, _| aqr_exe::module::macho_codesign(p, i, n).map_err(err),
    ),
    bare(CIL, |p, _, i, n, _| aqr_exe::module::cil(p, i, n).map_err(err)),
    bare(PE_RELOC, |p, _, i, n, _| aqr_exe::module::reloc(p, i, n).map_err(err)),
    bare(MACHO_SYMTAB, |p, _, i, n, _| aqr_exe::module::macho_symtab(p, i, n).map_err(err)),
    part(
        FLATE,
        true,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_flate::module::SET_WINDOW),
                aqr_api::Window::Input(1, aqr_flate::module::SET_RECORDS_WINDOW),
                aqr_api::Window::Input(2, aqr_flate::module::SET_RECORDS_WINDOW),
                aqr_api::Window::Output(0, aqr_flate::module::SET_WINDOW),
            ],
            arity: 7,
            call: |a| aqr_flate::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5], a[6]),
        },
        |p, r, i, n, t| aqr_flate::formula::decode(p, r, i, n, t).map_err(err),
    ),
    part(
        INFLATE,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_flate::module::INFLATE_WINDOW),
                aqr_api::Window::Output(0, aqr_flate::module::INFLATE_WINDOW),
            ],
            arity: 3,
            call: |a| aqr_flate::module::inflate_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, _| aqr_flate::module::inflate(p, i, n).map_err(err),
    ),
    part(
        LZ4,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_lz4::part::WINDOW),
                aqr_api::Window::Output(0, aqr_lz4::part::WINDOW),
            ],
            arity: 3,
            call: |a| aqr_lz4::part::decode_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, t| aqr_lz4::formula::decode(p, i, n, t).map_err(err),
    ),
    threaded(LZMA, |p, _, i, n, t| aqr_lzma::formula::decode(p, i, n, t).map_err(err)),
    threaded(LZMA2, |p, _, i, n, t| aqr_lzma::formula::decode2(p, i, n, t).map_err(err)),
    bra!(BCJ_X86, X86),
    bra!(BCJ_POWERPC, PowerPc),
    bra!(BCJ_IA64, Ia64),
    bra!(BCJ_ARM, Arm),
    bra!(BCJ_ARM_THUMB, ArmThumb),
    bra!(BCJ_SPARC, Sparc),
    bra!(BCJ_ARM64, Arm64),
    bra!(BCJ_RISCV, RiscV),
    part(
        BCJ_DELTA,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bcj::module::DELTA_WINDOW),
                aqr_api::Window::Output(0, aqr_bcj::module::DELTA_WINDOW),
            ],
            arity: 3,
            call: |a| aqr_bcj::module::delta_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, _| bcj(BCJ_DELTA, p, i, n),
    ),
    part(
        BCJ2,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Input(1, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(0, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(1, aqr_bcj::module::BCJ2_WINDOW),
                aqr_api::Window::Output(2, aqr_bcj::module::BCJ2_WINDOW),
            ],
            arity: 6,
            call: |a| aqr_bcj::module::bcj2_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, _| bcj(BCJ2, p, i, n),
    ),
    bare(LZO, |p, _, i, n, t| aqr_lzo::formula::decode(p, i, n, t).map_err(err)),
    bare(AZO, |p, _, i, n, t| aqr_azo::formula::decode(p, i, n, t).map_err(err)),
    bare(LHA, |p, _, i, n, t| aqr_lha::formula::decode(p, i, n, t).map_err(err)),
    bare(RAR5, |p, _, i, n, t| aqr_rar::formula::decode5(p, i, n, t).map_err(err)),
    bare(RAR3, |p, _, i, n, t| aqr_rar::formula::decode3(p, i, n, t).map_err(err)),
    bare(BVX, |p, _, i, n, t| aqr_bvx::formula::decode(p, i, n, t).map_err(err)),
    threaded(BZ2, |p, _, i, n, t| aqr_bz2::formula::decode(p, i, n, t).map_err(err)),
    threaded(BZ2_ALZ, |p, _, i, n, t| aqr_bz2::formula::decode_alz(p, i, n, t).map_err(err)),
    bare(LZX, |p, _, i, n, t| aqr_lzx::formula::decode(p, i, n, t).map_err(err)),
    bare(XPRESS, |p, _, i, n, t| aqr_xpress::formula::decode(p, i, n, t).map_err(err)),
    bare(LZNT1, |p, _, i, n, t| aqr_lznt1::formula::decode(p, i, n, t).map_err(err)),
    part(
        MSZIP,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(1, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(2, aqr_mszip::module::WINDOW),
                aqr_api::Window::Input(3, aqr_mszip::module::WINDOW),
                aqr_api::Window::Output(0, aqr_mszip::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_mszip::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, _| aqr_mszip::module::decode(p, i, n).map_err(err),
    ),
    part(
        SNAPPY,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_snappy::module::WINDOW),
                aqr_api::Window::Output(0, aqr_snappy::module::WINDOW),
            ],
            arity: 3,
            call: |a| aqr_snappy::module::decode_part(a[0], a[1], a[2]),
        },
        |p, _, i, n, t| aqr_snappy::formula::decode(p, i, n, t).map_err(err),
    ),
    bare(LEVELDB, |p, _, i, n, t| aqr_snappy::formula::decode_table(p, i, n, t).map_err(err)),
    part(
        ZIP,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(1, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(2, aqr_zip::module::WINDOW),
                aqr_api::Window::Input(3, aqr_zip::module::WINDOW),
                aqr_api::Window::Output(0, aqr_zip::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_zip::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, t| aqr_zip::formula::decode(p, i, n, t).map_err(err),
    ),
    part(
        BUNDLE,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_bundle::module::WINDOW),
                aqr_api::Window::Input(1, aqr_bundle::module::WINDOW),
                aqr_api::Window::Output(0, aqr_bundle::module::WINDOW),
            ],
            arity: 4,
            call: |a| aqr_bundle::module::decode_part(a[0], a[1], a[2], a[3]),
        },
        |p, _, i, n, t| aqr_bundle::formula::decode(p, i, n, t).map_err(err),
    ),
    bare(ASAR, |p, _, i, n, t| aqr_asar::formula::decode(p, i, n, t).map_err(err)),
    part(
        GIT_PACK,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_git::module::WINDOW),
                aqr_api::Window::Input(1, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(2, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(3, aqr_git::module::WINDOW),
                aqr_api::Window::Output(0, aqr_git::module::WINDOW),
            ],
            arity: 6,
            call: |a| aqr_git::module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
        },
        |p, _, i, n, t| aqr_git::formula::decode(p, i, n, t).map_err(err),
    ),
    part(
        GIT_PACK_INDEX,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_git::module::WINDOW),
                aqr_api::Window::Input(1, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(2, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Input(3, aqr_git::module::WINDOW),
                aqr_api::Window::Input(4, aqr_git::module::SMALL_WINDOW),
                aqr_api::Window::Output(0, aqr_git::module::WINDOW),
            ],
            arity: 7,
            call: |a| aqr_git::module::decode_index_part(a[0], a[1], a[2], a[3], a[4], a[5], a[6]),
        },
        |p, _, i, n, t| aqr_git::formula::decode_index(p, i, n, t).map_err(err),
    ),
    threaded(PNG, |p, _, i, n, t| aqr_png::formula::decode(p, i, n, t).map_err(err)),
    bare(PNG_AGAINST, |p, _, i, n, t| aqr_png::formula::decode_against(p, i, n, t).map_err(err)),
    bare(PNG_SET, |p, _, i, n, t| aqr_png::formula::decode_set(p, i, n, t).map_err(err)),
    bare(GIF, |p, _, i, n, t| aqr_gif::formula::decode(p, i, n, t).map_err(err)),
    bare(PRORES, |p, _, i, n, t| aqr_prores::formula::decode(p, i, n, t).map_err(err)),
    bare(JPEG, |p, _, i, n, t| aqr_jpeg::formula::decode(p, i, n, t).map_err(err)),
    threaded_takes(MP3, |p, r, i, n, t| aqr_mp3::formula::decode(p, r, i, n, t).map_err(err)),
    threaded_takes(FLAC, |p, r, i, n, t| aqr_flac::formula::decode(p, r, i, n, t).map_err(err)),
    bare(PSD, |p, _, i, n, t| aqr_psd::formula::decode(p, i, n, t).map_err(err)),
    bare(CLIP, |p, _, i, n, t| aqr_clip::formula::decode(p, i, n, t).map_err(err)),
    bare(BCN, |p, _, i, n, t| aqr_tex::formula::decode(p, i, n, t).map_err(err)),
    bare(PDF, |p, _, i, n, t| aqr_pdf::formula::decode(p, i, n, t).map_err(err)),
    bare(SQLITE, |p, _, i, n, t| aqr_sqlite::formula::decode(p, i, n, t).map_err(err)),
    part(
        FASTA,
        false,
        aqr_api::emu::Entry {
            kind: aqr_api::emu::Kind::Part,
            windows: &[
                aqr_api::Window::Input(0, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(1, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(2, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(3, aqr_fasta::module::WINDOW),
                aqr_api::Window::Input(4, aqr_fasta::module::WINDOW),
                aqr_api::Window::Output(0, aqr_fasta::module::WINDOW),
            ],
            arity: 7,
            call: |a| aqr_fasta::module::decode_part(a[0], [a[1], a[2], a[3], a[4], a[5]], a[6]),
        },
        |p, _, i, n, t| aqr_fasta::formula::decode(p, i, n, t).map_err(err),
    ),
    bare(DEX, |p, _, i, n, _| aqr_dex::formula::decode(p, i, n).map_err(err)),
    bare(DWARF_APPLE, |p, _, i, n, _| aqr_dwarf::formula::decode_apple(p, i, n).map_err(err)),
    bare(DWARF_INFO, |p, _, i, n, _| aqr_dwarf::formula::decode_info(p, i, n).map_err(err)),
    bare(PDB_MSF, |p, _, i, n, _| aqr_pdb::formula::decode(p, i, n).map_err(err)),
    bare(PDB_SYMBOLS, |p, _, i, n, _| aqr_pdb::formula::decode_symbols(p, i, n).map_err(err)),
    bare(ARSC, |p, _, i, n, _| aqr_arsc::formula::decode(p, i, n).map_err(err)),
    bare(WASM, |p, _, i, n, _| aqr_wasm::formula::decode(p, i, n).map_err(err)),
    bare(GNU_GETTEXT, |p, _, i, n, _| aqr_gnu_gettext::formula::decode(p, i, n).map_err(err)),
    bare(FITS_IMAGE, |p, _, i, n, _| aqr_fits::formula::decode_image(p, i, n).map_err(err)),
    bare(FITS_RICE, |p, _, i, n, _| aqr_fits::formula::decode_rice(p, i, n).map_err(err)),
    bare(FITS_TABLE, |p, _, i, n, _| aqr_fits::formula::decode_table(p, i, n).map_err(err)),
    bare(OBJ, |p, _, i, n, t| aqr_obj::formula::decode(p, i, n, t).map_err(err)),
    bare(VORBIS, |p, _, i, n, _| aqr_vorbis::formula::decode(p, i, n).map_err(err)),
    bare(OGG, |p, _, i, n, _| aqr_ogg::formula::decode(p, i, n).map_err(err)),
    bare(IMA, |p, _, i, n, _| aqr_ima::formula::decode(p, i, n).map_err(err)),
    bare(HDF5_FLOAT, |p, _, i, n, _| aqr_hdf5::module::float(p, i, n).map_err(err)),
    bare(XSV, |p, _, i, n, t| aqr_xsv::formula::decode(p, i, n, t).map_err(err)),
    bare(JSONL, |p, _, i, n, t| aqr_jsonl::formula::decode(p, i, n, t).map_err(err)),
    takes(WRT, |p, r, i, n, t| aqr_wrt::formula::decode(p, r, i, n, t).map_err(err)),
    takes(WRT_HANGUL, |p, r, i, n, t| aqr_wrt::formula::decode_hangul(p, r, i, n, t).map_err(err)),
    threaded_takes(PCM, |p, r, i, n, t| aqr_raw::formula::pcm::decode(p, r, i, n, t).map_err(err)),
    bare(RASTER, |p, _, i, n, t| aqr_raw::formula::raster::decode(p, i, n, t).map_err(err)),
    bare(RASTER_16, |p, _, i, n, _| aqr_raw::formula::raster16::decode(p, i, n).map_err(err)),
    bare(RECS_FIELDS, |p, _, i, n, _| aqr_infer_recs::formula::decode_fields(p, i, n).map_err(err)),
    bare(RECS_ENTRIES, |p, _, i, n, _| aqr_infer_recs::formula::decode_entries(p, i, n).map_err(err)),
    bare(RECS_FLOATS, |p, _, i, n, _| aqr_infer_recs::formula::decode_floats(p, i, n).map_err(err)),
];

/// Every module of this crate.
pub const MODULES: [Module; NATIVE.len()] = {
    let mut out = [NATIVE[0].module; NATIVE.len()];
    let mut i = 0;
    while i < NATIVE.len() {
        out[i] = NATIVE[i].module;
        i += 1;
    }
    out
};

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

/// What a module's imports say of its memories, read once: the pages of its working memory as
/// it is instantiated, and the windows of its inputs and outputs (whether each is an output's,
/// its index and its pages).
#[cfg(feature = "wasm")]
type ImportShape = (Option<u64>, Vec<(bool, u32, u64)>);

#[cfg(feature = "wasm")]
fn import_shape(m: Module) -> Option<ImportShape> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Shapes = HashMap<(&'static str, u16), Option<ImportShape>>;
    static SHAPES: Mutex<Option<Shapes>> = Mutex::new(None);
    let k = built(m)?;
    let mut shapes = SHAPES.lock().unwrap();
    (shapes.get_or_insert_with(HashMap::new).entry((k.name, k.version)))
        .or_insert_with(|| {
            let wasm = aqr_formula::aqr::module_of_block_data(k.block)?;
            Some((wasm::initial_pages(&wasm), wasm::windows(&wasm)))
        })
        .clone()
}

/// The pages of the working memory of the Wasm module of `m` (aqr-modules') as it is
/// instantiated, for its memory plan ([`aqr_formula::Usage::initial_pages`]); read once.
#[cfg(feature = "wasm")]
pub fn initial_pages_of(m: Module) -> Option<u64> {
    import_shape(m)?.0
}

/// The windows of the memories of the inputs and outputs of the Wasm module of `m` that
/// aqr-modules built (`wasm::windows`): whether each is an output's, its index and its pages.
#[cfg(feature = "wasm")]
pub fn windows_of(m: Module) -> Vec<(bool, u32, u64)> {
    import_shape(m).map(|s| s.1).unwrap_or_default()
}

/// [`needed_by`]'s failure: the plan of a module that cannot be worked out (a bug of the plan, or
/// contents missing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPlan;

/// The memory an application of `m` needs, as aqr-compress declares it in the Apply instruction
/// and as the decoder checks it: the memories imported (the parameters, the resources, the
/// inputs and the outputs, the last two within their windows, a page at least each) with the
/// module's plan of its working memory for the instance, and the plan's task limits. The lengths
/// are those of the resources, inputs and outputs; `contents` the inputs, for plans that read
/// them. `Ok(None)` for a module of no plan or none built (tests'); [`NoPlan`] where its plan
/// cannot be worked out.
#[cfg(feature = "wasm")]
pub fn needed_by(
    m: Module,
    params: &[u8],
    resources: &[u64],
    inputs: &[u64],
    outputs: &[u64],
    contents: &dyn aqr_formula::Contents,
) -> std::result::Result<Option<aqr_formula::Plan>, NoPlan> {
    use aqr_formula::{PAGE, Plan, Usage};
    let Some(initial_pages) = m.plan.and(initial_pages_of(m)) else { return Ok(None) };
    let usage = Usage { params, resources, inputs, outputs, contents, initial_pages };
    let plan = m.plan_for(&usage).ok_or(NoPlan)?;
    // (an input's or output's within its window, of a part module)
    let windows = windows_of(m);
    let fit = |n: u64, window: Option<u64>| n.div_ceil(PAGE).min(window.unwrap_or(u64::MAX)).saturating_mul(PAGE);
    let window = |output: bool, i: usize| windows.iter().find(|w| (w.0, w.1 as usize) == (output, i)).map(|w| w.2);
    let imported: u64 = [&[params.len() as u64][..], resources].concat().iter().map(|&n| fit(n, None)).sum::<u64>()
        + inputs.iter().enumerate().map(|(k, &n)| fit(n, window(false, k))).sum::<u64>()
        + outputs.iter().enumerate().map(|(j, &n)| fit(n, window(true, j))).sum::<u64>();
    // (a plan is what the module takes at most: no room)
    Ok(Some(Plan { instance: imported.saturating_add(plan.instance), tasks: plan.tasks }))
}

/// The name of the module of `wasm` ([`known`]), for [`aqr::Natives::identified_by`].
pub fn identify(wasm: &[u8]) -> Option<String> {
    known(wasm).map(|m| m.to_string())
}

/// Whether the module named `name` (without its version) is a part module (in Wasm, applied as its
/// inputs stream; natively some are applied whole, on threads).
pub fn is_part(name: &str) -> bool {
    NATIVE.iter().any(|e| e.module.name == name && matches!(e.decode, Run::Part(..) | Run::Threaded(..)))
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

/// [`Wasm`] with its own bound on the memories of an application (`.0` bytes: the instance's
/// limit and, of each class of tasks, its concurrency times its task limit; see
/// [`set_application_bound`], which sets one for the process), for tests.
#[cfg(feature = "wasm")]
#[derive(Debug, Clone, Copy)]
pub struct WasmBounded(pub u64);

#[cfg(feature = "wasm")]
impl Runner for WasmBounded {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        wasm::load_bounded(wasm, Sha512_256::digest(wasm).into(), self.0)
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

/// An application of a module run natively, measured ([`record_native`]): as
/// [`aqr_common::meter`] tells of the heap it took.
#[derive(Debug, Clone)]
pub struct Metered {
    pub module: Module,
    pub params: Vec<u8>,
    /// The lengths of the inputs and the outputs.
    pub inputs: Vec<u64>,
    pub sizes: Vec<u64>,
    pub measured: aqr_common::meter::Measured,
}

static METERED: std::sync::Mutex<Option<Vec<Metered>>> = std::sync::Mutex::new(None);

/// Starts measuring the applications run natively from now on and keeping what they took
/// (`true`), or stops (`false`); returns those kept so far.
pub fn record_native(on: bool) -> Vec<Metered> {
    let mut a = METERED.lock().unwrap();
    let kept = a.take().unwrap_or_default();
    if on {
        *a = Some(Vec::new());
    }
    kept
}

/// `f`, an application of `m`, measured where [`record_native`] is on.
fn metered<R>(m: Module, params: &[u8], inputs: &[u64], sizes: &[u64], f: impl FnOnce() -> R) -> R {
    if METERED.lock().unwrap().is_none() {
        return f();
    }
    let (r, measured) = aqr_common::meter::measure(f);
    if let Some(log) = METERED.lock().unwrap().as_mut() {
        log.push(Metered {
            module: m,
            params: params.to_vec(),
            inputs: inputs.to_vec(),
            sizes: sizes.to_vec(),
            measured,
        });
    }
    r
}

/// A module run natively.
struct NativeLoaded<'a> {
    native: &'a Native,
    module: Module,
    /// Its Wasm module, which runs where the plan of an application cannot be worked out, as the
    /// memory limit is enforced there only.
    wasm: Vec<u8>,
    fallback: std::sync::OnceLock<aqr::Decoded<Box<dyn Loaded + 'a>>>,
}

/// How an application stands against the limits it declares ([`NativeLoaded::verdict`]).
enum Verdict {
    /// Within them: it runs natively.
    Within,
    /// Over them: the Wasm host would fail it too.
    Over,
    /// Not known: it runs as WebAssembly, where the limits are enforced.
    Unknown,
}

/// The inputs of a part module's application, pulled whole as a plan asks for their contents
/// ([`aqr_formula::Contents`]), those no longer than the memory limit: the application would
/// fail before its first pull otherwise.
struct Pulled<'a> {
    inputs: Vec<std::cell::RefCell<&'a mut dyn PartIn>>,
    contents: Vec<std::cell::OnceCell<Option<Vec<u8>>>>,
    failed: Vec<std::cell::RefCell<Option<Failure>>>,
    limit: u64,
}

impl<'a> Pulled<'a> {
    fn new<'b: 'a>(inputs: &'a mut [&'b mut dyn PartIn], limit: u64) -> Self {
        let n = inputs.len();
        Pulled {
            inputs: inputs
                .iter_mut()
                .map(|i| {
                    let i: &'a mut (dyn PartIn + 'a) = &mut **i;
                    std::cell::RefCell::new(i)
                })
                .collect(),
            contents: (0..n).map(|_| Default::default()).collect(),
            failed: (0..n).map(|_| Default::default()).collect(),
            limit,
        }
    }

    /// The inputs again, those pulled replayed.
    fn into_replays(self) -> Vec<Replay<'a>> {
        let parts = self.inputs.into_iter().zip(self.contents).zip(self.failed);
        parts
            .map(|((inner, c), f)| Replay {
                inner: inner.into_inner(),
                buf: c.into_inner().flatten(),
                pos: 0,
                failed: f.into_inner(),
            })
            .collect()
    }
}

impl aqr_formula::Contents for Pulled<'_> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        self.contents
            .get(k)?
            .get_or_init(|| {
                let mut i = self.inputs[k].borrow_mut();
                let n = i.len();
                if n > self.limit {
                    return None;
                }
                let mut buf = Vec::with_capacity(n as usize);
                match i.pull(n, &mut |d| buf.extend_from_slice(d)) {
                    Ok(()) => Some(buf),
                    Err(f) => {
                        *self.failed[k].borrow_mut() = Some(f);
                        None
                    }
                }
            })
            .as_deref()
    }
}

/// An input of a part module's application that may have been pulled whole already.
struct Replay<'a> {
    inner: &'a mut dyn PartIn,
    buf: Option<Vec<u8>>,
    pos: u64,
    /// Its failure to be pulled.
    failed: Option<Failure>,
}

impl PartIn for Replay<'_> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> aqr::Decoded<()> {
        let Some(buf) = &self.buf else { return self.inner.pull(hi, put) };
        let (lo, hi) = (self.pos.min(hi), hi.min(buf.len() as u64));
        put(&buf[lo as usize..hi as usize]);
        self.pos = hi;
        Ok(())
    }
}

/// The inputs of an application as a plan reads them.
struct Slices<'a>(&'a [&'a [u8]]);

impl aqr_formula::Contents for Slices<'_> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        self.0.get(k).copied()
    }
}

impl<'a> NativeLoaded<'a> {
    fn new(native: &'a Native, module: Module, wasm: &[u8]) -> Self {
        NativeLoaded { native, module, wasm: wasm.to_vec(), fallback: Default::default() }
    }

    /// The module as WebAssembly.
    fn wasm(&self) -> aqr::Decoded<&(dyn Loaded + 'a)> {
        #[cfg(feature = "wasm")]
        {
            let w = self.fallback.get_or_init(|| Wasm.load(&self.wasm));
            w.as_ref().map(|b| &**b).map_err(Failure::clone)
        }
        #[cfg(not(feature = "wasm"))]
        Err(Failure::Resource(
            "memory limit not known, which is enforced only as WebAssembly (the feature wasm)".into(),
        ))
    }

    /// Whether the application is within the memory limit and task limits it declares, which the
    /// Wasm host enforces and native code does not: its plan (what a Wasm instance would need
    /// at most, [`needed_by`]) against them. The earlier Apply (`NO_LIMIT` and no task limits, of
    /// the modules of releases) has none to enforce. A plan that cannot be worked out is
    /// `Unknown`: the module's bytes are at hand, and WebAssembly enforces the limits whatever
    /// the plan says.
    #[allow(clippy::too_many_arguments)]
    fn verdict(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[u64],
        sizes: &[u64],
        contents: &dyn aqr_formula::Contents,
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Verdict {
        if memory_limit == aqr_formula::NO_LIMIT && task_limits.is_empty() {
            return Verdict::Within;
        }
        #[cfg(feature = "wasm")]
        {
            let resources: Vec<u64> = resources.iter().map(|r| r.len() as u64).collect();
            match needed_by(self.module, params, &resources, inputs, sizes, contents) {
                Ok(Some(p)) => {
                    let tasks = p.tasks.iter().zip(task_limits).all(|(&n, &l)| n <= l);
                    if p.instance <= memory_limit && tasks { Verdict::Within } else { Verdict::Over }
                }
                Ok(None) | Err(NoPlan) => Verdict::Unknown,
            }
        }
        #[cfg(not(feature = "wasm"))]
        {
            let _ = (params, resources, inputs, sizes, contents);
            Verdict::Within
        }
    }

    /// The entry point of its code as a part module, where it is one.
    fn part(&self) -> Option<aqr_api::emu::Entry> {
        NATIVE.iter().find(|e| e.module == self.module).and_then(|e| match e.decode {
            Run::Part(entry, _) => Some(entry),
            Run::Whole(_) | Run::Threaded(..) => None,
        })
    }
}

/// The message of a failure of `aqr_api`'s emulation: what went wrong, and the module's last
/// message before it.
fn emulated(f: aqr_api::emu::Failure) -> String {
    match f.log {
        Some(log) => format!("{}: {}", f.what, String::from_utf8_lossy(&log)),
        None => f.what,
    }
}

/// Applies part module `entry` natively ([`aqr_api::emu::apply_streaming`]), its inputs pulled
/// and its outputs pushed as it goes: a failure to pull or push is the application's.
fn streamed(
    entry: &aqr_api::emu::Entry,
    params: &[u8],
    resources: &[&[u8]],
    inputs: &mut [&mut dyn PartIn],
    outputs: &mut [&mut dyn PartOut],
    sizes: &[u64],
) -> aqr::Decoded<()> {
    use std::cell::RefCell;
    let failed: RefCell<Option<Failure>> = RefCell::new(None);
    struct Feed<'a, 'b>(&'a mut dyn PartIn, &'b RefCell<Option<Failure>>);
    impl aqr_api::emu::Feed for Feed<'_, '_> {
        fn len(&self) -> u64 {
            self.0.len()
        }
        fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> std::result::Result<(), String> {
            self.0.pull(hi, put).map_err(|f| {
                let m = format!("{f:?}");
                self.1.borrow_mut().get_or_insert(f);
                m
            })
        }
    }
    struct Drain<'a, 'b>(&'a mut dyn PartOut, &'b RefCell<Option<Failure>>);
    impl aqr_api::emu::Drain for Drain<'_, '_> {
        fn push(&mut self, d: &[u8]) -> std::result::Result<(), String> {
            self.0.push(d).map_err(|f| {
                let m = format!("{f:?}");
                self.1.borrow_mut().get_or_insert(f);
                m
            })
        }
    }
    let mut feeds: Vec<Feed> = inputs.iter_mut().map(|i| Feed(&mut **i, &failed)).collect();
    let mut drains: Vec<Drain> = outputs.iter_mut().map(|o| Drain(&mut **o, &failed)).collect();
    let mut feeds: Vec<&mut dyn aqr_api::emu::Feed> = feeds.iter_mut().map(|f| f as _).collect();
    let mut drains: Vec<&mut dyn aqr_api::emu::Drain> = drains.iter_mut().map(|d| d as _).collect();
    let r = aqr_api::emu::apply_streaming(entry, &mut feeds, resources, params, &mut drains, sizes);
    match (failed.into_inner(), r) {
        (Some(f), _) => Err(f),
        (None, r) => r.map_err(|f| Failure::Invalid(emulated(f))),
    }
}

impl Loaded for NativeLoaded<'_> {
    fn is_part(&self) -> bool {
        self.part().is_some()
    }

    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> aqr::Decoded<()> {
        let Some(entry) = self.part() else { return Err(Failure::Resource("not a part module".into())) };
        let lens: Vec<u64> = inputs.iter().map(|i| i.len()).collect();
        // (a plan reading the inputs' contents has them pulled, those short of the memory limit)
        let pulled = Pulled::new(inputs, memory_limit);
        let verdict = self.verdict(params, resources, &lens, sizes, &pulled, memory_limit, task_limits);
        let mut replays = pulled.into_replays();
        if let Some(f) = replays.iter().find_map(|r| r.failed.clone()) {
            return Err(f);
        }
        let mut inputs: Vec<&mut dyn PartIn> = replays.iter_mut().map(|r| r as &mut dyn PartIn).collect();
        match verdict {
            Verdict::Within => {}
            Verdict::Over => return Err(Failure::Resource("memory limit exceeded".into())),
            Verdict::Unknown => {
                return self.wasm()?.apply_part(
                    params,
                    resources,
                    &mut inputs,
                    outputs,
                    sizes,
                    memory_limit,
                    task_limits,
                );
            }
        }
        let _s = aqr_common::trace::span(self.module.name);
        let resources: Vec<&[u8]> = resources.iter().map(|r| &r[..]).collect();
        metered(self.module, params, &lens, sizes, || {
            streamed(&entry, params, &resources, &mut inputs, outputs, sizes).map_err(|f| match f {
                Failure::Invalid(m) => Failure::Invalid(format!("{}: {m}", self.module)),
                f => f,
            })
        })
    }

    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> aqr::Decoded<Vec<Vec<u8>>> {
        let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
        match self.verdict(params, resources, &lens, sizes, &Slices(inputs), memory_limit, task_limits) {
            Verdict::Within => {}
            Verdict::Over => return Err(Failure::Resource("memory limit exceeded".into())),
            Verdict::Unknown => {
                return self.wasm()?.apply(params, resources, inputs, sizes, memory_limit, task_limits);
            }
        }
        let resources: Vec<&[u8]> = resources.iter().map(|r| &r[..]).collect();
        let total: u64 = sizes.iter().sum();
        let out = self.native.run(self.module, params, &resources, inputs, total).map_err(|e| Failure::Invalid(e.0))?;
        // (a module of several outputs gives them one after another, as its whole `decode` does)
        split_outputs(self.module, out, sizes).map_err(Failure::Invalid)
    }
}

/// The modules of this crate natively where an archive has the Wasm module that aqr-modules
/// built ([`known`]), and any other module as WebAssembly (with the feature `wasm`).
impl Runner for Native {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        if let Some(module) = known(wasm) {
            return Ok(Box::new(NativeLoaded::new(self, module, wasm)));
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
        let _s = aqr_common::trace::span(m.name);
        let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
        metered(m, params, &lens, &[len], || {
            catch_unwind(AssertUnwindSafe(|| self.run_unchecked(m, params, resources, inputs, len)))
                .unwrap_or_else(|_| Err("panicked".into()))
                .map_err(|e| Error::new(format!("{m}: {e}")))
        })
    }

    fn run_unchecked(
        &self,
        m: Module,
        params: &[u8],
        resources: &[&[u8]],
        inputs: &[&[u8]],
        len: u64,
    ) -> std::result::Result<Vec<u8>, String> {
        let e = NATIVE.iter().find(|e| e.module == m).ok_or("unknown module")?;
        if !e.resources && !resources.is_empty() {
            return Err("resources given".into());
        }
        match e.decode {
            Run::Whole(decode) | Run::Part(_, Some(decode)) | Run::Threaded(decode) => {
                decode(params, resources, inputs, len, self.threads.max(1))
            }
            Run::Part(entry, None) => match aqr_api::emu::apply(&entry, inputs, resources, params, &[len]) {
                Ok(mut out) => Ok(out.pop().expect("an output")),
                Err(f) => Err(emulated(f)),
            },
        }
    }
}

impl Modules for Native {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        self.run(a.module, &a.params, &resources, inputs, a.len)
    }

    /// A module of several outputs gives them one after another natively (its whole `decode`).
    fn apply_shared(&self, s: &aqr_formula::Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        let Some(a) = s.apply() else { return s.patched(inputs).expect("a patch or a split") };
        let all = self.apply(a, inputs)?;
        if all.len() as u64 != a.len {
            return Err(Error::new(format!("{} made {} bytes, not {}", a.module, all.len(), a.len)));
        }
        match (s.split, &s.sizes[..]) {
            (false, &[_]) => Ok(vec![all]),
            (_, sizes) => split_outputs(a.module, all, sizes).map_err(Error::new),
        }
    }
}

/// The outputs of `module` of `sizes` bytes, cut from `all`, the outputs one after another (what
/// the whole `decode` of a module of several outputs gives).
fn split_outputs(module: Module, mut all: Vec<u8>, sizes: &[u64]) -> std::result::Result<Vec<Vec<u8>>, String> {
    if let &[_] = sizes {
        return Ok(vec![all]);
    }
    if all.len() as u64 != sizes.iter().sum::<u64>() {
        return Err(format!("{module}: output of another length"));
    }
    let mut rest = &mut all[..];
    let mut outs = Vec::with_capacity(sizes.len());
    for &n in sizes {
        let (a, b) = std::mem::take(&mut rest).split_at_mut(n as usize);
        outs.push(a.to_vec());
        rest = b;
    }
    Ok(outs)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use aqr_api::{Input, Memory, Output};

    use super::*;

    aqr_api::entry! {
        // its input copied to its output through windows of a page, in pieces of 4 KiB
        part fn copy(params_len, in0_len, size) windows(Input 0 = 1, Output 0 = 1) {
            let _ = params_len;
            assert_eq!(size, in0_len);
            let (w_in, w_out) = (Input::<0>::size(), Output::<0>::size());
            let mut buf = [0; 4096];
            let mut pos = 0;
            while pos < size {
                let n = (size - pos).min(buf.len() as u64).min(w_in - pos % w_in).min(w_out - pos % w_out);
                let chunk = &mut buf[..n as usize];
                Input::<0>::request(pos + n);
                Input::<0>::read(pos % w_in, chunk);
                Input::<0>::release(pos + n);
                Output::<0>::write(pos % w_out, chunk);
                Output::<0>::commit(pos + n);
                pos += n;
            }
        }
    }

    struct Feed<'a>(&'a [u8], Arc<AtomicU64>, Option<Failure>);

    impl PartIn for Feed<'_> {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }

        fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> aqr::Decoded<()> {
            if let Some(f) = &self.2 {
                return Err(f.clone());
            }
            put(&self.0[self.1.load(Relaxed) as usize..hi as usize]);
            self.1.store(hi, Relaxed);
            Ok(())
        }
    }

    /// The bytes pushed, and how much of the input was pulled as the first were.
    struct Drain(Vec<u8>, Arc<AtomicU64>, Option<u64>);

    impl PartOut for Drain {
        fn push(&mut self, d: &[u8]) -> aqr::Decoded<()> {
            self.2.get_or_insert(self.1.load(Relaxed));
            self.0.extend_from_slice(d);
            Ok(())
        }
    }

    /// A part module run natively streamed, through its windows: its first bytes pushed before
    /// most of its input is pulled; a failure to pull is the application's, not a trap's.
    #[test]
    fn natively_streamed() {
        let d: Vec<u8> = (0..300_001u32).map(|i| (i * 7) as u8).collect();
        let pulled = Arc::new(AtomicU64::new(0));
        let (mut feed, mut drain) = (Feed(&d, pulled.clone(), None), Drain(Vec::new(), pulled, None));
        streamed(&copy::ENTRY, b"", &[], &mut [&mut feed], &mut [&mut drain], &[d.len() as u64]).unwrap();
        assert!(drain.0 == d);
        assert!(drain.2.is_some_and(|n| n < 1 << 16), "{:?}", drain.2);
        let upstream = Failure::Invalid("upstream".into());
        let mut feed = Feed(&d, Arc::default(), Some(upstream.clone()));
        let mut drain = Drain(Vec::new(), Arc::default(), None);
        let r = streamed(&copy::ENTRY, b"", &[], &mut [&mut feed], &mut [&mut drain], &[d.len() as u64]);
        assert_eq!(r, Err(upstream));
        // of another size than declared
        let mut feed = Feed(&d, Arc::default(), None);
        let mut drain = Drain(Vec::new(), Arc::default(), None);
        let r = streamed(&copy::ENTRY, b"", &[], &mut [&mut feed], &mut [&mut drain], &[10]);
        assert!(matches!(r, Err(Failure::Invalid(_))), "{r:?}");
    }
}
