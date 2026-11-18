//! Recompression of a file into an entry formula (docs/xform-design.md §4.2, §6): [`detect`] picks
//! the recompressor by the magic bytes of a file, and [`recompress`] runs it on each file found,
//! giving a [`Formula`] rather than bytes.
//!
//! - PE images, Mach-O images (thin or universal) and ELF images: aqr-exe's x86 disfilter or
//!   ARM64 filter applied to each executable section, over its streams; the rest of the file is
//!   sliced in between, a content stream (below). Of an NSIS installer, the files its data
//!   stores as they are (`nsis`), each a content of its format, as deep as the installer.
//! - gzip and zlib: the container sliced around aqr-flate on the DEFLATE stream (data and
//!   record). Below level 4, DEFLATE streams (theirs and ZIP archives') are not rebuilt but
//!   kept as they are, each a slice of its own, and the files archived along with them that
//!   hold their data are made of them by aqr-flate's `flate-inflate` (`inflated`).
//! - mozlz4 files and LZ4 frames: the container sliced around aqr-lz4 on the data.
//! - `.lzma` files: the header sliced around aqr-lzma on the data and record of the stream.
//! - `.xz` files: the container sliced around aqr-lzma on the data and records of the LZMA2
//!   streams of its blocks. (These and 7z archives from level 4: `format_on`.)
//! - lzop files: the headers sliced around aqr-lzo on the data and records of the streams.
//! - Streams of Apple's LZFSE and LZVN blocks (`bvx2`, `bvxn`; asset catalogs hold them):
//!   aqr-bvx on the data and record of the stream (from level 9).
//! - bzip2 streams, one after another: aqr-bz2 on their data, what follows sliced.
//! - Zstandard files: kept as they are (not rebuilt); the files of the same batch that are
//!   their content, or a member of the tar archive it is, taken from their frames, which the
//!   archive's Patch instructions decompress (`zst`, aqr-zstd).
//! - LevelDB tables: aqr-snappy on the contents of their blocks.
//! - ZIP archives: aqr-zip, rebuilding the archive from the contents of its entries (the
//!   DEFLATE streams by aqr-flate) with its headers derived.
//! - 7z archives: aqr-7z, rebuilding the folders of LZMA, LZMA2 or Copy alone from the contents
//!   of the files they hold (by aqr-lzma), the rest of the archive kept; from level 4.
//! - git packs: aqr-git, rebuilding the pack from the data of its objects (the zlib streams by
//!   aqr-flate) with its headers, checksums and trailer derived; the blobs' data is found again
//!   where the files checked out are archived along with it.
//! - HDF5 files: aqr-hdf5, the deflated chunks of their datasets rebuilt by aqr-flate from
//!   their data, the rest of the file sliced around them.
//! - ar, tar and cpio archives: aqr-bundle, rebuilding the archive from the contents of its
//!   members with their sizes and checksums derived.
//! - Electron's asar archives: aqr-asar, rebuilding the archive from the data after its header,
//!   the files' sizes, offsets and SHA-256 integrity derived.
//! - ISO 9660 and UDF images: aqr-iso, the image cut into its files' contents and the rest.
//! - WIM files of LZX: aqr-wim, the resources' data, the LZX chunks rebuilt by aqr-lzx.
//! - Text, a whole file of text bytes or data of a name telling text: JSON lines by aqr-jsonl,
//!   their values stored by key path, or a delimited table (CSV, TSV, USV and the like) by
//!   aqr-xsv, its fields stored column by column, where either takes it; else aqr-wrt, its
//!   words and markup replaced by codes of a dictionary of its own.
//! - Data in which a scan finds no file (the rest of an executable, data of no format):
//!   aqr-infer-recs, its tables of records and chains of entries transposed by their fields,
//!   where a quick estimate tells they may save 5% at least and they do.
//! - QuickTime and MXF files of ProRes frames: aqr-prores, the frames' coefficients under a
//!   context model with the frame before, in chunks of about 32 MiB.
//! - Ogg files of Vorbis streams: aqr-ogg, rebuilding the pages from the streams' packets with
//!   their lacing and CRCs derived, the Vorbis audio packets by aqr-vorbis. Wwise's Vorbis
//!   media (`.wem`): aqr-wwise, the setup that their stripped one stands for rebuilt from
//!   aqr-vorbis's database, their packets by aqr-vorbis.
//! - PNG, GIF, JPEG, MP3, FLAC/WAV/AIFF, PSD/PSB, Clip Studio Paint, DDS, KTX/KTX 2/PVR/.astc, PDF, SQLite, FASTA, DEX, `resources.arsc`, FITS,
//!   PDB (its container and modules' symbols) and Wavefront OBJ (by its text, not its name): the recompressor's own formula, its module
//!   applied to its streams, and files it embeds (PDF's DEFLATE streams and images, PSD's and
//!   JPEG's embedded files, Clip Studio Paint's database) applied to theirs (see aqr-decompress
//!   for the modules).
//!
//! In whatever formula a recompressor gives, the data as it is ([`Stream::content`]) is
//! recompressed in turn, up to [`Options::depth`] formats deep, and its formula put in place
//! of the data: the slices of the streams that a recompressor gives as such (the rest of an
//! executable, around its code; the files of an image), and of those that its applications
//! take as such ([`Module::contents`]: the data of a DEFLATE stream, of gzip, zlib or PDF, of
//! an LZ4, LZMA or LZMA2 stream, of the blocks of a LevelDB table, of the entries of a ZIP
//! archive, of the members of an ar, tar or cpio archive, of the files of an asar archive;
//! [`Formula::mark_contents`]), wherever they are.
//!
//! Data is taken as one file by the format it starts like ([`Nest::Exact`]), or scanned for
//! files starting anywhere in it ([`Nest::Scan`], the default): each is cut to its end
//! ([`extent`]), and the data between them kept raw, in one stream. Data that recompressors
//! turn down counts against a scan, which gives up past twice the data (and 16 MiB).
//!
//! What a recompressor knows of the data it gives, the [`Kind`] of its stream, refines that
//! ([`Nest::of`]): pixels, fonts, tables and compressed data are not looked into, text is
//! taken as a whole by aqr-wrt, and data of [`Kind::Format`] is taken as a file of that format
//! from its start ([`Nest::As`]).
//! The contents of an application may be slices of several streams one after the other (a
//! ZIP archive's entries, by kind), each recompressed by its own kind. aqr-zip, aqr-bundle and
//! aqr-asar give the entries their kinds by their names ([`kind_by_name`]), as gzip's data is by its name, and
//! aqr-pdf the streams of a PDF file by what they are.
//!
//! Contents that are PNG files of the same geometry and alike (variants of a picture, with
//! layers shown or hidden) are coded against each other, as one set split into the files
//! (`sets`), where that is smaller than coding them alone.
//!
//! A file that a scan finds but its recompressor turns down is looked into by its parts where
//! the recompressor tells them ([`Region`]; aqr-psd does): the parts of a format are taken as
//! files of it, the parts of the other kinds are not looked into, and only the bytes no part
//! tells of (and parts of unknown kind) are scanned. Else the scan goes on from its next byte.
//!
//! What DEFLATE streams and streams of LZFSE and LZVN blocks inflate to, at every depth
//! together, is kept within [`Options::expansion`] times the file (and 64 MiB): past it,
//! streams are kept as they are, so that nested archives (42.zip) and bombs cost no more than
//! that.
//!
//! Before the formula is archived, its streams of each class and kind are put one after
//! another in one stream ([`Formula::merge_classes`]), as they are alike: the same
//! recompressor's, of the files of a container.
//!
//! A formula may be kept within [`Options::description`] bytes of description: of the files
//! found, or nested, those with the fewest bytes per byte of their description are kept raw
//! where all would not fit.
//!
//! [`compress`] stores the formula as an Aqueous archive of the file alone
//! ([`aqr_archive`]), with the modules of [`aqr_decompress::MODULES`], which
//! [`aqr_decompress::decompress`] decompresses. [`compress_tree`] archives several files, a
//! tree of them, each recompressed as the entries of a ZIP archive are.
//!
//! Whatever is not recognised, turned down by its recompressor or not made smaller is kept raw:
//! a slice of the input. So are, with [`Options::smaller`], a file and the files found in data
//! whose formula would archive no smaller than they do raw.
//!
//! Formulas are verified by whoever takes them, not as they are made: with
//! [`Options::verify`], [`compress`] decodes the archive it makes, and stores the file raw
//! instead where that does not give it back. [`recompress`] does not verify. The applications
//! of modules are evaluated beforehand, while the file is still recompressed, and their outputs
//! taken by the decoding (`verify`).
//! [`Native`] evaluates the formulas.
//!
//! ```no_run
//! # let file: Vec<u8> = vec![];
//! use aqr_archive::Evaluate;
//! let opt = aqr_compress::Options::default();
//! let formula = aqr_compress::recompress(&file, &opt);
//! assert_eq!(formula.eval(&aqr_decompress::Native::default())?, file);
//! # Ok::<(), aqr_formula::Error>(())
//! ```

mod aqueous;
mod archive;
mod bundle;
mod choose;
mod chunked;
mod context;
mod detect;
mod dwarf;
mod exe;
mod extent;
mod flate;
mod formats;
mod git;
mod hdf5;
mod inflated;
mod lzma;
mod made;
pub mod mapped;
mod memory;
mod options;
mod output;
mod placing;
mod recompressors;
mod scanning;
mod verdict;
pub use memory::{DEFAULT_BOUND, Fallback, bound_text, fallbacks};
mod nsis;
mod progress;
mod seal;
mod sets;
mod siblings;
pub mod spill;
mod together;
mod verify;
mod zip;
mod zst;

use context::*;
use made::*;
pub use options::*;
pub use output::*;
use placing::*;
pub use recompressors::*;
use scanning::*;
use verdict::*;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{self, Seek, Write};
use std::ops::{Deref, Range};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

pub use aqr_formula::{Formula, Kind, Module, Node, Region};
pub use archive::{
    Appending, Archived, ClassBytes, FileReport, Input, Output, Plain, Report, Solid, Source, Stop, Stopped,
    compress_tree, compress_tree_into, compress_tree_to,
};
pub use detect::{Format, detect, kind_by_name};
pub use extent::extent;
pub use progress::Event;

use aqr_archive::Evaluate;
use aqr_archive::{Encoder, Level, description_bound};
use aqr_decompress::Native;
use aqr_formula::probe::{Held, Look, Probe};
use aqr_formula::{Bytes, Stream};

#[cfg(test)]
mod tests;
