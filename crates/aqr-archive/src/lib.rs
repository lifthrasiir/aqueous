//! Aqueous archives (`spec/index.bs`) of entry formulas ([`aqr_formula`]): the encoder and the
//! decoder, the evaluation of formulas ([`Evaluate`], by the decoder's evaluator of formula
//! blocks), and the archive's metadata, segments, envelopes and mirroring.
//!
//! A formula as an Aqueous archive of a single file, without a metadata block:
//!
//! [`encode`] lays the formula out as blocks: a module block for each module, a resource block
//! for each resource, the formula block, and the non-leaf blocks of each stream it slices, in
//! the order of the streams: a stream of more than [`BLOCK`] bytes is several blocks of that
//! many, one after another, which a slice over several takes as a concatenation of slices of
//! each, so that a decoder holds a block at a time rather than the stream (and may read part of
//! a file). A formula of a single slice is a leaf block of its own.
//!
//! A module block holds the Wasm module of a [`Module`], which the [`Encoder`] is given
//! ([`Encoder::with_modules`]), and a [`Runner`] runs it in decoding: natively where it knows
//! the module, say. Applications declare the memory limit of [`aqr_formula::Apply::memory_limit`]
//! (set by [`Evaluate::declare_memory`]). [`natives`] runs modules by
//! [`Modules`], on [`stand_in`]s by default, for tests and measurements.
//!
//! [`Encoder`] encodes several formulas, compressing each stream and resource once however
//! many hold it, at the zstd level that its [`Level`] picks for it, a long one (8 MiB or more)
//! with long-distance matching over a window of all of it (up to the 128 MiB every decoder
//! takes); streams of [`Kind::Compressed`] are stored as they are, unless the probe level
//! compresses all of them.
//!
//! [`decode`] evaluates such archives, and any other without a metadata block. An encrypted
//! archive is sealed and opened by [`envelope`], its payload then opened by [`open_payload`].
//!
//! An archive of several files is of one or more segments (`segment`): [`Encoder::encode_segment`]
//! lays out the formulas of the files over streams they may share, with the metadata block of
//! their [`Tree`], and [`SegmentWriter`] writes them a batch at a time, closing a segment where
//! it is told to. [`decode_value`] decodes any archive (shorthand module blocks resolved through
//! the runner's registry; registered dictionaries and imports apart) to its files, the trees of
//! its segments merged. The blocks alike of a batch of its
//! leaves may take a dictionary trained for them (`dict`).

mod blocks;
pub mod chash;
mod compress;
mod dict;
mod emit;
mod encoder;
pub mod envelope;
pub(crate) mod eval;
mod evaluate;
mod index;
pub mod inspect;
mod level;
mod listing;
mod locate;
pub mod mirror;
mod open;
mod parse;
mod program;
pub mod released;
mod rewrite;
mod runner;
mod segment;
pub mod sink;
mod slices;
mod spool;
mod stand_in;
mod tree;

use blocks::*;
pub use compress::*;
pub use emit::*;
pub use encoder::*;
pub use evaluate::{Evaluate, Modules};
pub use level::*;
pub use listing::{Applied, Listed};
pub use locate::{block_at, footer_of, locate, trim};
pub use mirror::Mirroring;
pub use open::*;
use parse::*;
pub use program::*;
pub use rewrite::{History, Rewritten, rewrite, rewrite_as};
pub use runner::*;
pub use segment::{Batch, ContentAt, Layout, Leaf, Placed, Segment, SegmentWriter, crossing, kill, precompress_leaves};
pub use slices::*;
pub use stand_in::*;
pub use tree::{
    Entry, EntryNode, Extra, MAC_FLAGS_ALLOWED, Member, Status, Time, Tree, WIN_ATTRS_ALLOWED, Xattr, Zone,
    archive_key_name, diagnostic, entry_key_name, name_bytes, name_from,
};

use std::borrow::Cow;
use std::cell::{Cell, RefCell, RefMut};
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use aqr_common::budget::{Budget, Taken};
use aqr_common::varint;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh64::{Xxh64, xxh64};
use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;
use zstd::zstd_safe::{self, CCtx, CParameter, InBuffer, OutBuffer, ResetDirective};

use aqr_formula::{Error, Formula, Kind, Module, Node, Resource, Result, packed};

pub const HEADER_MAGIC: [u8; 6] = [0xf8, 0x04, 0x03, 0x6a, 0xd6, 0xbe];
pub const FOOTER_MAGIC: [u8; 6] = [0xfd, 0xaa, 0xae, 0xde, 0x85, 0x12];
pub const OVERLAY_HEADER_MAGIC: [u8; 6] = [0x69, 0x0d, 0x15, 0xaf, 0xf6, 0xb2];
pub const OVERLAY_FOOTER_MAGIC: [u8; 6] = [0xfc, 0x0f, 0x6e, 0x7b, 0x4b, 0xac];
/// The type of a size hint block.
const SIZE_HINT: u64 = 0x04;
/// A size hint block of a padded LEB128 to be filled in ([`Encoder::with_size_hint`], and
/// [`envelope::Sealer`] fills it in).
pub const SIZE_HINT_PLACEHOLDER: [u8; 12] = [0x04, 10, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0];
// The block types of block-like structures, invalid as blocks.
const HEADER_TYPE: u64 = 0x278;
const OVERLAY_HEADER_TYPE: u64 = 0x69;
const FOOTER_TYPE: u64 = 0x90_5bcb_957d;
const OVERLAY_FOOTER_TYPE: u64 = 0x7fc;
const ENCRYPTED_HEADER_TYPE: u64 = 0x155_04dc_a579;

/// The name of the custom section naming the module of a [`stand_in`].
const STAND_IN_SECTION: &str = "aqueous-stand-in";
/// The largest memory limit, of an application declaring none ([`aqr_formula::NO_LIMIT`]): what the
/// instruction bounds are made for.
pub const MEMORY_LIMIT: u64 = aqr_formula::NO_LIMIT;

// Block types with a u8 size field, all with checksums; `+ 0..=3` widens the size field.
const LEAF_RAW: u64 = 0x24;
const LEAF_ZSTD: u64 = 0x2c;
const NON_LEAF_RAW: u64 = 0x40;
const NON_LEAF_ZSTD: u64 = 0x44;
/// Leaf and non-leaf data blocks compressed with a dictionary, and dictionary blocks with entropy
/// tables.
const LEAF_ZSTD_DICT: u64 = 0x34;
const NON_LEAF_ZSTD_DICT: u64 = 0x48;
const DICTIONARY: u64 = 0x54;
const DICTIONARY_RAW: u64 = 0x50;
const MODULE: u64 = 0x60;
const RESOURCE: u64 = 0x64;
const FORMULA: u64 = 0x70;

const NOP: u8 = 0x00;
const LITERAL: u8 = 0x01;
const MODULE_BYTES: u8 = 0x02;
const RESOURCE_BYTES: u8 = 0x03;
const SWAP: u8 = 0x0f;
const SLICE: u8 = 0x10;
const APPLY: u8 = 0x16;
/// The earlier Apply, which gave no task limits (and a memory limit of all of the application's
/// memories): reserved, an invalid archive but for the modules of the releases that wrote it
/// ([`released`]).
const APPLY_OLD: u8 = 0x11;
const CONCAT: u8 = 0x12;
const PACK: u8 = 0x13;
const SPLIT: u8 = 0x14;
const PATCH: u8 = 0x15;
const CHECKSUM: u8 = 0x20;

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const WASM_HEADER: [u8; 8] = *b"\0asm\x01\0\0\0";

/// The checksum of the output, as [`encode`] takes it.
pub fn checksum(d: &[u8]) -> u64 {
    xxh64(d, 0)
}

/// `f` as an archive, its blocks compressed at `level` ([`Encoder`]), with `checksum` of the
/// output ([`checksum`]) if given. A leaf block checksums itself, which is the output: a
/// `checksum` given must be its own. Fails on slices out of their stream, and on such a
/// checksum that is not the leaf's.
pub fn encode(f: &Formula, checksum: Option<u64>, level: Level) -> Result<Vec<u8>> {
    Encoder::new(level).encode(f, checksum)
}

#[cfg(test)]
mod tests;
