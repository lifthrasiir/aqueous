//! A formula as an Aqueous archive (`spec/index.bs`) of a single file, without a metadata
//! block.
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
//! the module, say. Applications declare the memory limit of [`crate::Apply::memory_limit`]
//! (set by [`crate::Formula::declare_memory`]). [`natives`] runs modules by
//! [`crate::Modules`], on [`stand_in`]s by default, for tests and measurements.
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
//! it is told to. [`decode_value`] decodes any archive (without registered dictionaries or
//! imports) to its files, the trees of its segments merged. The blocks alike of a batch of its
//! leaves may take a dictionary trained for them ([`dict`]).

mod dict;
pub mod envelope;
pub(crate) mod eval;
mod index;
pub mod inspect;
mod listing;
mod locate;
pub mod mirror;
mod segment;
pub mod sink;
mod spool;
mod tree;

pub use listing::{Applied, Listed};
pub use locate::{block_at, footer_of, locate, trim};
pub use mirror::Mirroring;
pub use segment::{Batch, ContentAt, Layout, Leaf, Placed, Segment, SegmentWriter, crossing, kill};
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

use crate::{Error, Formula, Kind, Module, Node, Resource, Result, packed};

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
/// The largest memory limit, of an application declaring none ([`crate::NO_LIMIT`]): what the
/// instruction bounds are made for.
pub const MEMORY_LIMIT: u64 = crate::NO_LIMIT;

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

/// [`checksum`] of `d`, its pages let go of as they are read through where it is of a file mapped
/// ([`crate::store::let_go`]): not all of them resident at once.
pub fn checksum_through(d: &[u8]) -> u64 {
    let mut h = xxhash_rust::xxh64::Xxh64::new(0);
    for part in d.chunks(4 << 20) {
        h.update(part);
        crate::store::let_go(part);
    }
    h.digest()
}

/// `f` as an archive, its blocks compressed at `level` ([`Encoder`]), with `checksum` of the
/// output ([`checksum`]) if given. A leaf block checksums itself, which is the output: a
/// `checksum` given must be its own. Fails on slices out of their stream, and on such a
/// checksum that is not the leaf's.
pub fn encode(f: &Formula, checksum: Option<u64>, level: Level) -> Result<Vec<u8>> {
    Encoder::new(level).encode(f, checksum)
}

/// How hard an [`Encoder`] compresses: a level of the archive, which picks the zstd level of
/// each stream (and how hard aqr-compress recompresses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A level from 0 (fastest) to 9 (smallest), [`Level::LOW`], [`Level::MID`] and
    /// [`Level::HIGH`] being 2, 5 and 8. At 0 ([`Level::STORED`]) nothing is compressed: every
    /// block is uncompressed, but for those that are always Zstandard (formula, module and
    /// resource blocks), which hold raw Zstandard blocks. Otherwise each stream is uncompressed
    /// if incompressible (`PROBE`), by a sample and then, where the sample is not all of it, by
    /// all of it (which finds what repeats from afar), compressed at the probe level where only
    /// all of it compresses; else at the base zstd level of the level's `Pick`, or:
    ///
    /// - at a higher level of it where that saves enough for its time, at its rate of bytes
    ///   per second of compression;
    /// - at a lower level of it where that is smaller by 1/32 at least (beyond what a sample
    ///   may be off by), as it is faster too;
    /// - at its dense level, where it has one, if of PE code or text (`Encoder::how`): if
    ///   longer than a sample, always; else where that is smaller.
    ///
    /// Module and formula blocks, and streams shorter than `SMALL`, are at the base. Streams
    /// of [`Kind::Compressed`] skip the sample: all of each is compressed at the probe level,
    /// at any level but 0.
    Of(u8),
    /// Every stream at this zstd level, but those of [`Kind::Compressed`] (as for
    /// [`Level::Of`]).
    Zstd(i32),
}

impl Level {
    /// The fastest of [`Level::Of`].
    pub const MIN: u8 = 0;
    /// The smallest of [`Level::Of`].
    pub const MAX: u8 = 9;
    /// Low: compressing about 3 times as fast as [`Level::MID`] (the archiver's serial paths
    /// aside), decoding about twice (as it is now: the same modules).
    pub const LOW: Level = Level::Of(2);
    /// The default.
    pub const MID: Level = Level::Of(5);
    /// Nothing compressed.
    pub const STORED: Level = Level::Of(0);
    /// High: compressing at 40 MB/s and decoding at 200 MB/s at least, on 10gb.
    pub const HIGH: Level = Level::Of(8);

    /// The level of [`Level::Of`] that what is not zstd's goes by: [`Level::MID`]'s for a zstd
    /// level.
    pub fn scale(self) -> u8 {
        match self {
            Level::Of(n) => n.clamp(Level::MIN, Level::MAX),
            Level::Zstd(_) => 5,
        }
    }
}

/// `low`, `mid`, `high`, `0` to `9`, or `zstdN` (zstd level `N`).
impl std::str::FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Level, String> {
        match s {
            "low" => Ok(Level::LOW),
            "mid" => Ok(Level::MID),
            "high" => Ok(Level::HIGH),
            _ => match (s.strip_prefix("zstd"), s.parse::<u8>()) {
                (Some(l), _) => l.parse().map(Level::Zstd).map_err(|_| format!("not a zstd level: {l}")),
                (None, Ok(n)) if (Level::MIN..=Level::MAX).contains(&n) => Ok(Level::Of(n)),
                _ => Err(format!("not a level (low, mid, high, 0-9 or zstdN): {s}")),
            },
        }
    }
}

/// The zstd level at which a stream is incompressible if it saves less than 1/32 (as a
/// recompressor's entropy-coded output does), whatever level would compress it otherwise.
/// A stream that its sample does not compress so, or of [`Kind::Compressed`], is compressed at
/// this level all of it (some 1 GB/s on a core, of 13 instructions a byte with long-distance
/// matching, on noise), and kept so where that saves 1/32: a sample of chunks of 64 KiB does
/// not see what repeats from afar (a video's keyframes, the same each time).
const PROBE: i32 = 1;
/// The base zstd level of [`Level::Of`] from 2 on, with the bytes per second it compresses (as
/// measured on text and PSD skeletons, as are the others).
const BASE: (i32, f64) = (3, 350e6);
/// The zstd levels above [`BASE`] that [`Level::Of`] picks from, with their speed: of level 5
/// the first three, below as many fewer as it is lower; of level 9 all.
const HIGHER: [(i32, f64); 4] = [(4, 330e6), (6, 140e6), (9, 100e6), (DENSE, 8.2e6)];
/// The bytes that [`Level::MID`] would save for a second of compression above [`BASE`]:
/// level 9 where it saves 0.71% of the data over level 3 (2??% of what they compress to,
/// where 10gb's solid blocks save 8% at the median); zstd at 100 MB/s on a core is still far
/// from all of compression's time.
const MID_RATE: f64 = 1e6;
/// The zstd level of dense streams from level 8 ([`Level::HIGH`]) on: level 16 (btultra, 8.2
/// MB/s on a core; 9 at 89) was smaller than 9 on every block of 1 MiB or more of 10gb's PE
/// code (0.949 of it at the median, 0.996 at most) and text (0.935, 0.957 at most), about ??9
/// MB for some 200 s of a core; not so on other data (raw 1.006 at most). Not in estimates
/// ([`Encoder::least`]): a probe goes by the level picked.
const DENSE: i32 = 16;
/// Streams shorter than this are compressed at the base by [`Level::Of`], as picking would
/// take longer than any level.
const SMALL: usize = 64 << 10;
/// [`Level::Of`] picks the level by this many bytes of the stream, in [`SAMPLE_CHUNKS`]
/// chunks spread evenly over it (or by the whole stream if no longer).
pub(crate) const SAMPLE: usize = 1 << 20;
pub(crate) const SAMPLE_CHUNKS: usize = 16;

/// How [`Level::Of`] picks the zstd level of a stream (see there).
struct Pick {
    base: (i32, f64),
    /// Fastest first.
    lower: &'static [i32],
    higher: &'static [(i32, f64)],
    /// The bytes worth saving for a second of compression above the base.
    rate: f64,
    dense: Option<i32>,
}

impl Pick {
    /// Level 1: zstd level 1 alone. 2??: the base, level 1 where smaller, and as many of
    /// [`HIGHER`] as the level is above 2. From 6 on, the higher level by the sample alone, as a
    /// sample of chunks of 64 KiB does not see the matches from afar that a higher level's
    /// larger window finds (10gb: some ??1 MB at 5's rate); from 8 on, [`DENSE`] for dense
    /// streams, and at 9 for any where the sample is smaller so.
    /// (Level 0 compresses nothing, and picks nothing: taken as 1.)
    fn of(level: u8) -> Pick {
        let n = level.clamp(1, Level::MAX);
        Pick {
            // (level 1's speed compared with no other)
            base: if n == 1 { (1, BASE.1) } else { BASE },
            lower: if n == 1 { &[] } else { &[1] },
            higher: &HIGHER[..match n {
                1 | 2 => 0,
                3 => 1,
                4 => 2,
                5..=8 => 3,
                _ => 4,
            }],
            rate: if n >= 6 { 0.0 } else { MID_RATE },
            dense: (n >= 8).then_some(DENSE),
        }
    }

    /// The zstd level the most compressing of the levels picked from (not [`Pick::dense`]).
    fn top(&self) -> i32 {
        self.higher.last().map_or(self.base.0, |h| h.0)
    }
}

/// How a stream is compressed, by what it is ([`Encoder::how`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// Of [`Kind::Compressed`]: stored as it is, unless all of it compresses at [`PROBE`].
    Light,
    Usual,
    /// PE code or text, at the dense level of the [`Pick`] (which has one).
    Dense,
}

/// What an [`Encoder`] makes of a stream: compressed already ([`compressed`]), to be
/// compressed at a zstd level, to about so many bytes (by a sample), or to be compressed at
/// [`PROBE`] all of it and stored unless that saves enough ([`saves`]).
enum Plan {
    Done(Option<Vec<u8>>, i32),
    Level(i32, usize),
    Probe,
}

impl Plan {
    /// `data` compressed (or not) as planned, on up to `threads` threads, and at what level.
    fn carry_out(self, c: &mut CCtx, data: &[&[u8]], threads: usize) -> Result<(Compressed, i32)> {
        match self {
            Plan::Done(c, l) => Ok((c, l)),
            Plan::Level(l, size) => Ok((compressed_to(c, data, l, threads, size)?, l)),
            Plan::Probe => {
                let n = total(data);
                match compressed_to(c, data, PROBE, threads, n)?.filter(|c| saves(c.len(), n)) {
                    Some(c) => Ok((Some(c), PROBE)),
                    None => Ok((None, STORED)),
                }
            }
        }
    }
}

/// Whether compressing `n` bytes to `len` is worth it rather than storing them ([`PROBE`]).
fn saves(len: usize, n: usize) -> bool {
    len < n - n / 32
}

/// Encodes formulas as archives at a [`Level`], keeping the compressed streams and resources
/// by their contents: [`Encoder::len`] sizes a formula, and the same streams and resources in a
/// later formula are not compressed again.
pub struct Encoder {
    level: Level,
    /// Threads compressing a stream (the archive does not depend on them).
    threads: usize,
    /// Compressed streams (as [`compressed`] gives) by their [`StreamKey`], of the store where one
    /// is set ([`kept`]).
    streams: RefCell<HashMap<StreamKey, Option<crate::Bytes>>>,
    /// Whether the streams compressed are kept in [`Encoder::streams`] (not while
    /// [`Encoder::encode_pieces`] writes).
    keep: std::cell::Cell<bool>,
    /// Compressed resources by their [`StreamKey`].
    resources: RefCell<HashMap<StreamKey, Vec<u8>>>,
    /// The archived lengths ([`Encoder::raw_len`]) of streams forgotten compressed
    /// ([`Encoder::forget`]), or compared with a formula at the base level of [`Level::Of`]
    /// ([`Encoder::smaller`]), by their [`StreamKey`]: a stream is compared with formulas of it
    /// again without being compressed again.
    raw_lens: RefCell<HashMap<StreamKey, usize>>,
    /// Samples of streams compressed by [`Encoder::least`], by their stream's [`StreamKey`]
    /// and level, for picking the level of the stream without compressing them again.
    trials: RefCell<HashMap<(StreamKey, i32), Compressed>>,
    /// The Zstandard context every stream is compressed with, so that its workers and buffers
    /// are made once rather than for each stream (while they take at most [`CCTX_KEPT`]).
    cctx: RefCell<CCtx<'static>>,
    /// Whether formulas are sized without their resource blocks ([`Encoder::share_resources`]).
    shared: bool,
    /// Whether archives begin with [`SIZE_HINT_PLACEHOLDER`] ([`Encoder::with_size_hint`]).
    size_hint: bool,
    /// Whether formulas are sized without their module blocks ([`Encoder::share_modules`]).
    shared_modules: bool,
    /// The resources that formulas are sized without ([`Encoder::hold_resource`]).
    held: RefCell<HashSet<StreamKey>>,
    /// The Wasm module of each module ([`Encoder::with_modules`]).
    wasm: WasmOf,
    /// Module blocks compressed, by their module and level.
    module_blocks: RefCell<HashMap<(Module, i32), ModuleBlock>>,
    /// Told of the streams compressed at once ([`Encoder::on_precompress`]).
    progress: Option<Progress>,
    /// The zstd level each stream compressed by [`Encoder::precompress`] was compressed at, by
    /// its [`StreamKey`] (for the dictionaries of a batch, [`dict`]).
    levels: RefCell<HashMap<StreamKey, i32>>,
    /// Whether a batch's blocks alike are compressed with dictionaries trained for them
    /// ([`Encoder::with_dictionaries`]).
    trains: bool,
}

/// The dictionaries of a batch where its blocks take them, and the index of their first in
/// the segment ([`Encoder::content_block`]).
type Dicts<'a> = Option<(&'a dict::Planned, usize)>;

/// What [`Encoder::precompress`] tells as it compresses: the bytes of the streams compressed so
/// far, of all of them, from any of its threads.
pub type Progress = std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>;

/// A module block compressed, shared by the formulas that have it.
type ModuleBlock = std::sync::Arc<Vec<u8>>;

/// The Wasm module of a module, if there is one ([`Encoder::with_modules`]).
pub type WasmOf = fn(Module) -> Option<Wasm>;

/// A Wasm module as an encoder writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wasm {
    /// Its bytes, compressed into its module block.
    Bytes(Cow<'static, [u8]>),
    /// The data of its module block made beforehand ([`module_block_data`]), written as it is.
    Block(Cow<'static, [u8]>),
}

/// The zstd level of the module blocks made beforehand: modules are made once and archived
/// often.
const MODULE_LEVEL: i32 = 22;

/// The data of the module block of Wasm module `wasm`, compressed at the highest level, to be
/// made once ([`Wasm::Block`]).
pub fn module_block_data(wasm: &[u8]) -> Result<Vec<u8>> {
    always_compressed(&mut CCtx::create(), &[wasm], MODULE_LEVEL, 1)
}

/// The Wasm module of the data of its module block ([`module_block_data`]).
pub fn module_of_block_data(d: &[u8]) -> Option<Vec<u8>> {
    stored_decoded(d)
}

/// The decoded data of checksummed Zstandard data `d` (the data of a module or resource block).
pub(crate) fn stored_decoded(d: &[u8]) -> Option<Vec<u8>> {
    decoded(d, true, true).ok()
}

/// A stream's (or resource's) hash and length.
type StreamKey = (u128, usize);

/// What [`compressed`] gives: the compressed stream, or `None` where it does not compress.
type Compressed = Option<Vec<u8>>;

/// A compressed stream as [`Encoder::streams`] keeps it: moved to the store where one is set
/// ([`crate::store`]), so that what is kept to be written later (all the streams of a file, as
/// its formula is sized) is not held in memory.
fn kept(c: Compressed) -> Option<crate::Bytes> {
    c.map(|c| {
        let mut b = crate::Bytes::from(c);
        crate::store::spill(&mut b);
        b
    })
}

/// What an [`Encoder`] kept of the streams, resources and module blocks it compressed
/// ([`Encoder::into_kept`]), without its Zstandard context.
pub struct Kept {
    streams: HashMap<StreamKey, Option<crate::Bytes>>,
    resources: HashMap<StreamKey, Vec<u8>>,
    raw_lens: HashMap<StreamKey, usize>,
    trials: HashMap<(StreamKey, i32), Compressed>,
    levels: HashMap<StreamKey, i32>,
    module_blocks: HashMap<(Module, i32), ModuleBlock>,
}

/// The Zstandard contexts of encoders alive (each [`Encoder`] has one), and the most at once
/// since [`reset_contexts_peak`]. Zstandard allocates outside the Rust heap, so that what they
/// hold (up to [`CCTX_KEPT`] each) is out of sight of the heap's accounting.
static CONTEXTS: (AtomicUsize, AtomicUsize) = (AtomicUsize::new(0), AtomicUsize::new(0));

/// The Zstandard contexts of encoders alive, and the most at once since
/// [`reset_contexts_peak`].
pub fn contexts() -> (usize, usize) {
    (CONTEXTS.0.load(Relaxed), CONTEXTS.1.load(Relaxed))
}

/// Starts the peak of [`contexts`] again from those alive.
pub fn reset_contexts_peak() {
    CONTEXTS.1.store(CONTEXTS.0.load(Relaxed), Relaxed);
}

impl Drop for Encoder {
    fn drop(&mut self) {
        CONTEXTS.0.fetch_sub(1, Relaxed);
    }
}

impl Encoder {
    pub fn new(level: Level) -> Encoder {
        Encoder::with_threads(level, 1)
    }

    /// Drops what is kept of stream `d` compressed (by [`Encoder::smaller`], say), where it will
    /// not be archived; only its archived length is kept, for [`Encoder::smaller`].
    pub fn forget(&self, d: &[u8]) {
        // (none of its length kept: not hashed, which reads all of it, an image of GBs mapped)
        let n = d.len();
        if !self.trials.borrow().keys().any(|t| t.0.1 == n) && !self.streams.borrow().keys().any(|k| k.1 == n) {
            return;
        }
        let k = key(&[d]);
        self.trials.borrow_mut().retain(|t, _| t.0 != k);
        if self.streams.borrow().contains_key(&k) {
            // from what is kept, compressing nothing
            let n = self.raw_len(d).expect("a stream compressed already");
            self.raw_lens.borrow_mut().insert(k, n);
            self.streams.borrow_mut().remove(&k);
        }
    }

    /// What it kept of the streams, resources and module blocks it compressed, for an encoder
    /// alike to [`adopt`](Encoder::adopt); its Zstandard context dropped with it, so that what
    /// is kept for later holds none (a context takes up to [`CCTX_KEPT`] outside the heap: one
    /// kept for each of the 16,421 files of a batch made apart once took 10 GB).
    pub fn into_kept(self) -> Kept {
        Kept {
            streams: self.streams.take(),
            resources: self.resources.take(),
            raw_lens: self.raw_lens.take(),
            trials: self.trials.take(),
            levels: self.levels.take(),
            module_blocks: self.module_blocks.take(),
        }
    }

    /// Keeps what an encoder alike (for data compressed into the same archive) kept of the
    /// streams, resources and module blocks it compressed ([`Encoder::into_kept`]), as if
    /// compressed here.
    pub fn adopt(&self, kept: Kept) {
        self.streams.borrow_mut().extend(kept.streams);
        self.resources.borrow_mut().extend(kept.resources);
        self.raw_lens.borrow_mut().extend(kept.raw_lens);
        self.trials.borrow_mut().extend(kept.trials);
        self.levels.borrow_mut().extend(kept.levels);
        self.module_blocks.borrow_mut().extend(kept.module_blocks);
    }

    /// Drops everything kept of the streams and resources compressed.
    pub fn clear(&self) {
        self.streams.borrow_mut().clear();
        self.levels.borrow_mut().clear();
        self.resources.borrow_mut().clear();
        self.raw_lens.borrow_mut().clear();
        self.trials.borrow_mut().clear();
    }

    /// Archives written ([`Encoder::encode_pieces_to`] and the like) beginning with a size hint
    /// block to be filled in, [`SIZE_HINT_PLACEHOLDER`] (where `on`), as an encrypted archive's
    /// payload has to ([`envelope::Sealer`]).
    pub fn with_size_hint(mut self, on: bool) -> Encoder {
        self.size_hint = on;
        self
    }

    /// An encoder compressing each stream on `threads` threads, into the same archive as on one.
    pub fn with_threads(level: Level, threads: usize) -> Encoder {
        let alive = CONTEXTS.0.fetch_add(1, Relaxed) + 1;
        CONTEXTS.1.fetch_max(alive, Relaxed);
        Encoder {
            level,
            threads: threads.max(1),
            streams: RefCell::default(),
            keep: std::cell::Cell::new(true),
            resources: RefCell::default(),
            raw_lens: RefCell::default(),
            trials: RefCell::default(),
            cctx: RefCell::new(CCtx::create()),
            shared: false,
            shared_modules: false,
            held: RefCell::default(),
            wasm: |m| Some(Wasm::Bytes(Cow::Owned(stand_in(m)))),
            module_blocks: RefCell::default(),
            progress: None,
            levels: RefCell::default(),
            trains: true,
            size_hint: false,
        }
    }

    /// Tells `progress` of the streams that [`Encoder::precompress`] compresses (none: tells
    /// nothing).
    pub fn on_precompress(&mut self, progress: Option<Progress>) {
        self.progress = progress;
    }

    /// The encoder writing the Wasm module that `wasm` gives of each module into its module
    /// block (the data made beforehand where there is, else compressed at the level of the
    /// formula blocks), rather than its [`stand_in`]. Encoding fails on a module that has none.
    pub fn with_modules(mut self, wasm: WasmOf) -> Encoder {
        self.wasm = wasm;
        self.module_blocks.borrow_mut().clear();
        self
    }

    /// The classes of tasks of each of `modules` ([`task_classes`] of its Wasm module).
    pub(crate) fn classes_of(&self, modules: &[Module]) -> Vec<usize> {
        let class = |m: &Module| match (self.wasm)(*m) {
            Some(Wasm::Bytes(b)) => task_classes(&b),
            Some(Wasm::Block(d)) => module_of_block_data(&d).map_or(0, |w| task_classes(&w)),
            None => 0,
        };
        modules.iter().map(class).collect()
    }

    /// The encoder compressing a batch's blocks alike with dictionaries trained for them where
    /// they pay ([`SegmentWriter::batch`]; [`dict`]), as by default, or not.
    pub fn with_dictionaries(mut self, on: bool) -> Encoder {
        self.trains = on;
        self
    }

    /// The data of the module block of `m`, compressed at `level`, once.
    /// Checks bytes `d` that a [`Node::ModuleBytes`] takes of the block of module `m` against
    /// the block it writes (at `level`): the same.
    fn check_module_bytes(&self, m: Module, d: &[u8], level: i32) -> Result<()> {
        match self.module_block(m, level)?[..] == *d {
            true => Ok(()),
            false => Err(Error::new(format!("the bytes of a module block of {m} not its own"))),
        }
    }

    fn module_block(&self, m: Module, level: i32) -> Result<ModuleBlock> {
        if let Some(b) = self.module_blocks.borrow().get(&(m, level)) {
            return Ok(b.clone());
        }
        let wasm = (self.wasm)(m).ok_or_else(|| Error::new(format!("no Wasm module of {m}")))?;
        let b = ModuleBlock::new(match wasm {
            Wasm::Block(b) => b.into_owned(),
            Wasm::Bytes(w) => always_compressed(&mut self.cctx(), &[&w], level, self.threads)?,
        });
        self.module_blocks.borrow_mut().insert((m, level), b.clone());
        Ok(b)
    }

    /// Sizes formulas ([`Encoder::len`], [`Encoder::smaller`]) without their resource blocks,
    /// as if the archive held them already: as in an archive of many files that share them,
    /// where they come to nothing per file. For measuring; the archives are the same.
    pub fn share_resources(&mut self) {
        self.shared = true;
    }

    /// Sizes formulas ([`Encoder::len`], [`Encoder::smaller`]) without their module blocks, as
    /// in an archive of many files, which has a module block of each module once (whose cost is
    /// then no one file's). The archives are the same.
    pub fn share_modules(&mut self) {
        self.shared_modules = true;
    }

    /// Whether formulas are sized without their module blocks ([`Encoder::share_modules`]).
    pub fn shares_modules(&self) -> bool {
        self.shared_modules
    }

    /// The bytes of the module blocks in the archive of `f`.
    pub fn modules_len(&self, f: &Formula) -> Result<usize> {
        let mut n = Count(0);
        let (modules, _) = f.blocks();
        for m in modules {
            block(&mut n, MODULE, &self.module_block(m, self.base_level())?);
        }
        Ok(n.0)
    }

    /// Sizes formulas without resource `r`, as if the archive held it already: where the
    /// formulas of the files archived together share it (a dictionary made of them all), whose
    /// cost is then no one file's. The archives are the same.
    pub fn hold_resource(&self, r: &Resource) {
        self.held.borrow_mut().insert(key(&[&r[..]]));
    }

    /// The bytes of the resource blocks in the archive of `f`.
    pub fn resources_len(&self, f: &Formula) -> Result<usize> {
        self.resources_len_of(f, |_| true)
    }

    /// [`Encoder::resources_len`] of the resources `which` takes.
    fn resources_len_of(&self, f: &Formula, which: impl Fn(&Resource) -> bool) -> Result<usize> {
        let (_, mut resources) = f.blocks();
        resources.retain(|r| which(r));
        let mut n = Count(0);
        resources.into_iter().try_for_each(|r| self.resource_block(&mut n, r))?;
        Ok(n.0)
    }

    /// The Zstandard context of the encoder.
    fn cctx(&self) -> RefMut<'_, CCtx<'static>> {
        self.cctx.borrow_mut()
    }

    /// The length of the archive of `f` without a checksum, nor the blocks whose data it takes as
    /// stored ([`Encoder::module_bytes_len`]), as the archive of it is made: its streams of a
    /// class and kind merged ([`Formula::merged`], as aqr-compress writes a file alone and an
    /// archive's batches group them). Apart, the many small streams of an archive's entries
    /// (a ZIP of 1,399 files) would count far more than they take.
    pub fn len(&self, f: &Formula) -> Result<usize> {
        self.len_with(f, None)
    }

    /// [`Encoder::len`], with the stream of `stand_in` (by index) its bytes rather than its data.
    /// Where that stream is merged with others, the streams are counted apart.
    fn len_with(&self, f: &Formula, stand_in: Option<(usize, &[&[u8]])>) -> Result<usize> {
        let (merged, groups) = f.merged();
        let mut streams: Vec<Vec<&[u8]>> = Vec::with_capacity(groups.len());
        for g in &groups {
            match stand_in {
                // (alone, as a stream that a slice goes past the end of is)
                Some((t, bytes)) if g[..] == [(t, 0, f.streams[t].data.len() as u64)] => streams.push(bytes.to_vec()),
                Some((t, _)) if g.iter().any(|p| p.0 == t) => {
                    return self.len_of(f, |n| self.write(f, stand_in, None, n));
                }
                _ => streams.push(g.iter().map(|&(s, a, b)| &f.streams[s].data[a as usize..b as usize]).collect()),
            }
        }
        let refs: Vec<&[&[u8]]> = streams.iter().map(|p| &p[..]).collect();
        self.len_of(f, |n| self.write_streams(&merged, &refs, &[], None, n))
    }

    /// The length of the archive of `f` that `write` writes, as [`Encoder::len`] counts it.
    fn len_of(&self, f: &Formula, write: impl FnOnce(&mut Count) -> Result<()>) -> Result<usize> {
        let mut n = Count(0);
        write(&mut n)?;
        let held = self.held.borrow();
        let modules = if self.shared_modules { self.modules_len(f)? } else { self.module_bytes_len(f)? };
        let resources = match self.shared {
            true => self.resources_len(f)?,
            false => self.resources_len_of(f, |r| r.stored_data().is_some() || held.contains(&key(&[&r[..]])))?,
        };
        Ok(n.0 - resources - modules)
    }

    /// The bytes of the module blocks of `f` whose bytes it takes ([`Node::ModuleBytes`]), which
    /// [`Encoder::len`] does not count, as those of resources stored ([`Resource::stored`]): the
    /// archive holds them as they are either way, typically shared with the archive around them.
    fn module_bytes_len(&self, f: &Formula) -> Result<usize> {
        let mut modules: Vec<Module> = Vec::new();
        for n in f.shared.iter().flat_map(|s| &s.apply.inputs).chain([&f.root]) {
            n.visit(&mut |n| {
                if let &Node::ModuleBytes(m, _) = n
                    && !modules.contains(&m)
                {
                    modules.push(m);
                }
            });
        }
        let mut n = Count(0);
        for m in modules {
            block(&mut n, MODULE, &self.module_block(m, self.base_level())?);
        }
        Ok(n.0)
    }

    /// The length of the archive of `d` as it is, without a checksum (`d` itself not copied).
    fn raw_len(&self, d: &[u8]) -> Result<usize> {
        let mut n = Count(0);
        n.put(&HEADER_MAGIC);
        self.content_block(&mut n, true, &[d], How::Usual, None)?;
        let blocks = n.0 - HEADER_MAGIC.len();
        footer(&mut n, blocks);
        Ok(n.0)
    }

    /// The zstd level of the module and formula blocks.
    fn base_level(&self) -> i32 {
        match self.level {
            Level::STORED => STORED,
            Level::Of(n) => Pick::of(n).base.0,
            Level::Zstd(l) => l,
        }
    }

    /// How stream `s` is compressed: light if of [`Kind::Compressed`]; dense where the level
    /// has a dense level ([`Pick`]), if PE code (an x86 or ARM64 filter's streams, or the rest
    /// of a PE file) or text.
    fn how(&self, s: &crate::Stream) -> How {
        how_of(self.level, &s.class, s.kind)
    }

    /// [`encode`] at the level of the encoder. Fails on slices out of their stream.
    pub fn encode(&self, f: &Formula, checksum: Option<u64>) -> Result<Vec<u8>> {
        // sized first (the streams compressed then are kept), so as to be written at its size
        let mut out = Vec::with_capacity(self.len_of(f, |n| self.write(f, None, None, n))? + 16);
        self.write(f, None, checksum, &mut out)?;
        Ok(out)
    }

    /// [`Encoder::encode`] of `f` whose streams' bytes are those of `pieces` (each the
    /// concatenation of its pieces; the bytes of `f`'s own streams not read), as
    /// [`crate::Formula::merged`] gives them. The blocks are compressed as they are written and
    /// not kept, but those compressed already (which are let go).
    pub fn encode_pieces(&self, f: &Formula, pieces: &[Vec<&[u8]>], checksum: Option<u64>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.encode_pieces_to(f, pieces, checksum, &mut out).expect("written to memory")?;
        Ok(out)
    }

    /// [`Encoder::encode_pieces`] written to `out` as it is made, of the bytes written. Fails
    /// where `out` does (the archive written in part, nothing more written after the failure),
    /// else gives what [`Encoder::encode_pieces`] would.
    pub fn encode_pieces_to(
        &self,
        f: &Formula,
        pieces: &[Vec<&[u8]>],
        checksum: Option<u64>,
        out: &mut dyn io::Write,
    ) -> io::Result<Result<usize>> {
        let streams: Vec<&[&[u8]]> = pieces.iter().map(|p| &p[..]).collect();
        let mut w = Writing { out, len: 0, failed: None };
        self.keep.set(false);
        let r = self.write_streams(f, &streams, &[], checksum, &mut w);
        self.keep.set(true);
        match w.failed {
            Some(e) => Err(e),
            None => Ok(r.map(|_| w.len)),
        }
    }

    /// [`Encoder::encode_pieces_to`] of `f` whose streams are those of `pieces`, then those of
    /// `sunk` (of a [`sink::Sink`]), whose blocks are written as they are.
    pub fn encode_sunk_to(
        &self,
        f: &Formula,
        pieces: &[Vec<&[u8]>],
        sunk: &[sink::Sunk],
        checksum: Option<u64>,
        out: &mut dyn io::Write,
    ) -> io::Result<Result<usize>> {
        let streams: Vec<&[&[u8]]> = pieces.iter().map(|p| &p[..]).collect();
        let mut w = Writing { out, len: 0, failed: None };
        self.keep.set(false);
        let r = self.write_streams(f, &streams, sunk, checksum, &mut w);
        self.keep.set(true);
        match w.failed {
            Some(e) => Err(e),
            None => Ok(r.map(|_| w.len)),
        }
    }

    /// [`Encoder::encode`] into `out`, with the stream of `stand_in` (by index) its bytes rather
    /// than its data.
    fn write(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        checksum: Option<u64>,
        out: &mut impl Out,
    ) -> Result<()> {
        let data: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let streams: Vec<&[&[u8]]> = (0..data.len())
            .map(|s| match stand_in {
                Some((t, bytes)) if t == s => bytes,
                _ => std::slice::from_ref(&data[s]),
            })
            .collect();
        self.write_streams(f, &streams, &[], checksum, out)
    }

    /// [`Encoder::write`] of `f` whose streams' bytes are `streams` (each in pieces), then the
    /// streams of a sink, `sunk`, their blocks encoded already.
    fn write_streams(
        &self,
        f: &Formula,
        streams: &[&[&[u8]]],
        sunk: &[sink::Sunk],
        checksum: Option<u64>,
        out: &mut impl Out,
    ) -> Result<()> {
        // of the module and formula blocks
        let level = self.base_level();
        f.check_outputs()?;
        let own = streams.len();
        let len_of = |s: usize| match s < own {
            true => Some(total(streams[s])),
            false => sunk.get(s - own).map(|s| s.len as usize),
        };
        let mut slices = Ok(());
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && len_of(stream).is_none_or(|l| start.checked_add(len).is_none_or(|e| e > l as u64))
            {
                slices = Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
        });
        slices?;

        out.put(&HEADER_MAGIC);
        let blocks = out.len();
        if self.size_hint {
            out.put(&SIZE_HINT_PLACEHOLDER);
        }
        if let (&Node::Slice { stream, start, len }, []) = (&f.root, &f.shared[..])
            && stream < own
        {
            let leaf = cut(streams[stream], start as usize, len as usize);
            if checksum.is_some_and(|c| c != checksum_of(&leaf)) {
                return Err(Error::new("output checksum not that of the leaf"));
            }
            self.content_block(out, true, &leaf, self.how(&f.streams[stream]), None)?;
        } else {
            // the non-leaf blocks sliced, of each stream in order, by their ordinals
            let mut nodes: Vec<&Node> = f.shared.iter().flat_map(|s| &s.apply.inputs).collect();
            nodes.push(&f.root);
            let used = sliced(f.streams.len(), |s| len_of(s).unwrap_or(0), nodes);
            let (ordinals, next) = listed(&used, 0..f.streams.len());
            let (modules, resources) = f.blocks();
            let mut checked = Ok(());
            for n in f.shared.iter().flat_map(|s| &s.apply.inputs).chain([&f.root]) {
                n.visit(&mut |n| {
                    if let Node::ModuleBytes(m, d) = n
                        && checked.is_ok()
                    {
                        checked = self.check_module_bytes(*m, d, level);
                    }
                });
            }
            checked?;

            let mut data = Vec::new();
            varint::put(&mut data, next);
            (0..next).for_each(|_| varint::put(&mut data, 0));
            for n in [resources.len(), modules.len()] {
                varint::put(&mut data, n as u64);
                (0..n).for_each(|i| varint::put(&mut data, i as u64));
            }
            let classes = self.classes_of(&modules);
            let mut e = Emit {
                ordinals: &ordinals,
                modules: &modules,
                classes: &classes,
                resources: &resources,
                out: &mut data,
                stack: vec![],
            };
            for (i, s) in f.shared.iter().enumerate() {
                e.shared(i, s);
            }
            e.node(&f.root);
            debug_assert_eq!(e.stack, [None]);
            if let Some(c) = checksum {
                data.push(CHECKSUM);
                data.extend_from_slice(&c.to_le_bytes());
            }

            // (the blocks compressed at once beforehand, kept or not: where kept, as a formula is
            // compared with its data, those compressed already are skipped)
            {
                let mut blocks = Vec::new();
                for (s, used) in used.iter().enumerate().take(own) {
                    let (data, how) = (streams[s], self.how(&f.streams[s]));
                    for (r, _) in block_ranges(total(data)).zip(used).filter(|(_, u)| **u) {
                        blocks.push((cut(data, r.start, r.len()), how));
                    }
                }
                self.precompress_blocks(blocks, true)?;
            }
            for &m in &modules {
                block(out, MODULE, &self.module_block(m, level)?);
            }
            for r in resources {
                self.resource_block(out, r)?;
            }
            formula_block(out, 1, &always_compressed(&mut self.cctx(), &[&data], level, self.threads)?);
            for (s, used) in used.iter().enumerate() {
                if s >= own {
                    for (b, _) in sunk[s - own].blocks.iter().zip(used).filter(|(_, u)| **u) {
                        out.put(b);
                    }
                    continue;
                }
                let (data, how) = (streams[s], self.how(&f.streams[s]));
                for (r, _) in block_ranges(total(data)).zip(used).filter(|(_, u)| **u) {
                    self.content_block(out, false, &cut(data, r.start, r.len()), how, None)?;
                }
            }
        }

        footer(out, out.len() - blocks);
        Ok(())
    }
}

/// [`Encoder::how`] of a stream of `class` and `kind` at `level`.
fn how_of(level: Level, class: &str, kind: Kind) -> How {
    let dense = matches!(level, Level::Of(n) if Pick::of(n).dense.is_some())
        && (kind == Kind::Text || class == "pe" || class.starts_with("x86/") || class.starts_with("arm64/"));
    match kind {
        Kind::Compressed => How::Light,
        _ if dense => How::Dense,
        _ => How::Usual,
    }
}

/// The instructions pushing node `n`, its streams' blocks at `ordinals` and its modules and
/// resources at their indices in `modules` and `resources`.
/// Instructions as nodes push their values, with the stack they leave: the outputs of
/// applications of several outputs not taken yet by (application, output), the rest `None`.
struct Emit<'a> {
    /// The index of each block of each stream in the formula block's list ([`listed`]).
    ordinals: &'a [Vec<Option<u64>>],
    modules: &'a [Module],
    /// The classes of tasks of each module (by index; those past the end have none): a
    /// application declaring fewer task limits is written with [`crate::NO_LIMIT`] for the rest.
    classes: &'a [usize],
    resources: &'a [&'a Resource],
    out: &'a mut Vec<u8>,
    stack: Vec<Option<(usize, usize)>>,
}

impl Emit<'_> {
    /// The instructions pushing node `n`.
    fn node(&mut self, n: &Node) {
        match n {
            // (nothing of a block: an empty concatenation)
            Node::Slice { len: 0, .. } => {
                self.out.push(CONCAT);
                varint::put(self.out, 0);
                self.stack.push(None);
            }
            &Node::Slice { stream, start, len } => {
                // (a slice of each block it is over, concatenated)
                let pieces = slice_blocks(start, len);
                let n = pieces.len();
                for (k, at, n) in pieces {
                    self.out.push(SLICE);
                    varint::put(self.out, self.ordinals[stream][k as usize].unwrap());
                    varint::put(self.out, at);
                    varint::put(self.out, n);
                    self.stack.push(None);
                }
                if n > 1 {
                    self.concat(CONCAT, n);
                }
            }
            Node::Concat(ns) => {
                // (before a part taking an output, the items so far put together as a counter
                // of base [`FOLD`] counts, so that the Swaps bringing the output up pass few
                // items of each concatenation open, and each byte is copied into few
                // concatenations: a concatenation of concatenations is the same)
                let mut levels: Vec<u32> = Vec::new();
                for n in ns {
                    if takes_output(n) {
                        self.fold(&mut levels);
                    }
                    self.node(n);
                    levels.push(0);
                }
                self.concat(CONCAT, levels.len());
            }
            Node::Pack(ns) => {
                ns.iter().for_each(|n| self.node(n));
                self.concat(PACK, ns.len());
            }
            Node::Apply(a) => {
                a.inputs.iter().for_each(|n| self.node(n));
                self.apply(a, &[a.len]);
            }
            &Node::Output { shared, index, .. } => {
                // up from where it is, the outputs not taken yet (all below the other items,
                // which are together at the top) going anywhere below the others: the other
                // items each a place down, the output nearest them to its place. The top swapped
                // with each of those items below it in turn, from the top down, then with that
                // output, then with the one taken.
                let at = self.stack.iter().rposition(|&o| o == Some((shared, index))).unwrap();
                let top = self.stack.len() - 1;
                if at < top {
                    let first = (at + 1..=top).find(|&p| self.stack[p].is_none());
                    debug_assert!(first.is_none_or(|f| self.stack[f..].iter().all(Option::is_none)));
                    let mut seq: Vec<usize> = first.map_or(Vec::new(), |f| (f..top).rev().collect());
                    if let Some(f) = first.filter(|&f| f > at + 1) {
                        seq.push(f - 1);
                    }
                    for p in seq.into_iter().chain([at]) {
                        self.out.push(SWAP);
                        varint::put(self.out, (top - p) as u64);
                        self.stack.swap(p, top);
                    }
                }
                debug_assert_eq!(self.stack[top], Some((shared, index)));
                self.stack[top] = None;
            }
            Node::ModuleBytes(m, d) => {
                self.out.push(MODULE_BYTES);
                varint::put(self.out, self.modules.iter().position(|n| n == m).unwrap() as u64);
                varint::put(self.out, d.len() as u64);
                self.stack.push(None);
            }
            Node::ResourceBytes(r) => {
                self.out.push(RESOURCE_BYTES);
                varint::put(self.out, self.resources.iter().position(|&s| s == r).unwrap() as u64);
                varint::put(self.out, n.len());
                self.stack.push(None);
            }
            Node::Patch(p) => {
                self.node(&p.source);
                self.node(&p.patch);
                self.out.push(PATCH);
                varint::put(self.out, p.len);
                self.stack.truncate(self.stack.len() - 2);
                self.stack.push(None);
            }
        }
    }

    /// Puts together the items of a concatenation at the top of the stack, of `levels` (each
    /// the number of times [`FOLD`] items were put together into it), [`FOLD`] of the same
    /// level at a time from the top.
    fn fold(&mut self, levels: &mut Vec<u32>) {
        while let Some(&top) = levels.last() {
            let n = levels.iter().rev().take_while(|&&l| l == top).count();
            if n < FOLD {
                break;
            }
            self.concat(CONCAT, n);
            levels.truncate(levels.len() - n);
            levels.push(top + 1);
        }
    }

    /// The Concat or Pack instruction `op` of the top `n` items.
    fn concat(&mut self, op: u8, n: usize) {
        self.out.push(op);
        varint::put(self.out, n as u64);
        self.stack.truncate(self.stack.len() - n);
        self.stack.push(None);
    }

    /// The Apply instruction of `a` (its inputs pushed), of outputs of `sizes`.
    /// The instructions pushing the outputs of `s`, the `i`-th application of several outputs.
    fn shared(&mut self, i: usize, s: &crate::Shared) {
        s.apply.inputs.iter().for_each(|n| self.node(n));
        if s.split {
            self.apply(&s.apply, &[s.apply.len]);
            self.out.push(SPLIT);
            varint::put(self.out, s.sizes.len() as u64);
            s.sizes.iter().for_each(|&n| varint::put(self.out, n));
            self.stack.pop();
            self.stack.extend(s.sizes.iter().map(|_| None));
        } else {
            self.apply(&s.apply, &s.sizes);
        }
        let k = self.stack.len() - s.sizes.len();
        (0..s.sizes.len()).for_each(|j| self.stack[k + j] = Some((i, j)));
    }

    fn apply(&mut self, a: &crate::Apply, sizes: &[u64]) {
        let at = self.modules.iter().position(|&m| m == a.module).unwrap();
        let out = &mut *self.out;
        out.push(APPLY);
        varint::put(out, at as u64);
        varint::put(out, a.resources.len() as u64);
        for r in &a.resources {
            varint::put(out, self.resources.iter().position(|&s| s == r).unwrap() as u64);
        }
        varint::put(out, a.params.len() as u64);
        out.extend_from_slice(&a.params);
        varint::put(out, a.memory_limit);
        let classes = self.classes.get(at).copied().unwrap_or(0).max(a.task_limits.len());
        debug_assert!(classes <= TASK_CLASSES, "{classes} classes of tasks, past the description bound's");
        varint::put(out, classes as u64);
        let limits = a.task_limits.iter().copied().chain(std::iter::repeat(crate::NO_LIMIT));
        limits.take(classes).for_each(|n| varint::put(out, n));
        varint::put(out, a.inputs.len() as u64);
        varint::put(out, sizes.len() as u64);
        sizes.iter().for_each(|&n| varint::put(out, n));
        self.stack.truncate(self.stack.len() - a.inputs.len());
        self.stack.extend(sizes.iter().map(|_| None));
    }
}

/// Items of a level of a concatenation put together at a time ([`Emit::fold`]).
const FOLD: usize = 8;

/// Whether node `n` takes an output of an application of several outputs.
fn takes_output(n: &Node) -> bool {
    match n {
        Node::Output { .. } => true,
        Node::Slice { .. } | Node::ModuleBytes(..) | Node::ResourceBytes(_) => false,
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().any(takes_output),
        Node::Apply(a) => a.inputs.iter().any(takes_output),
        Node::Patch(p) => takes_output(&p.source) || takes_output(&p.patch),
    }
}

/// The classes of tasks that [`description_bound`] allows an application's module, of task limits
/// not yet declared (each as large as a memory limit): no module has more (checked as
/// instructions are written).
const TASK_CLASSES: usize = 2;

/// Bytes of the instructions pushing node `n` at most, wherever the streams of its formula are
/// among others (up to 2^21 of them, and 2^14 modules and resources each): the bounds of
/// formulas put together ([`Formula::embed`]) add up, with the nodes around them. An output of
/// an application of several outputs ([`Node::Output`]) is counted as one Swap: the Swaps
/// bringing it up to the top of the stack depend on the formula around it.
pub fn description_bound(n: &Node) -> usize {
    match n {
        Node::Output { .. } => 1 + 3,
        Node::ModuleBytes(..) | Node::ResourceBytes(_) => 1 + 2 + varint::len(n.len()),
        Node::Patch(p) => description_bound(&p.source) + description_bound(&p.patch) + 1 + varint::len(p.len),
        &Node::Slice { start, len, .. } => match slice_blocks(start, len)[..] {
            [(_, at, n)] => 1 + 3 + varint::len(at) + varint::len(n),
            ref pieces => {
                let each = 1 + 3 + 2 * varint::len(BLOCK as u64);
                pieces.len() * each + 1 + varint::len(pieces.len() as u64)
            }
        },
        Node::Concat(ns) | Node::Pack(ns) => {
            ns.iter().map(description_bound).sum::<usize>() + 1 + varint::len(ns.len() as u64)
        }
        Node::Apply(a) => {
            let inputs: usize = a.inputs.iter().map(description_bound).sum();
            inputs
                + 1
                + 2
                + varint::len(a.resources.len() as u64)
                + 2 * a.resources.len()
                + varint::len(a.params.len() as u64)
                + a.params.len()
                + varint::len(MEMORY_LIMIT)
                + varint::len(TASK_CLASSES as u64)
                + a.task_limits.iter().map(|&n| varint::len(n)).sum::<usize>()
                + varint::len(MEMORY_LIMIT) * TASK_CLASSES.saturating_sub(a.task_limits.len())
                + varint::len(a.inputs.len() as u64)
                + 1
                + varint::len(a.len)
        }
    }
}

/// A Wasm module of a custom section only, naming `m`: what stands in for its Wasm module where
/// there is none (in tests, and to size formulas apart from modules), and which [`natives`]
/// knows `m` by. No other [`Runner`] runs it.
pub fn stand_in(m: Module) -> Vec<u8> {
    fn custom(w: &mut Vec<u8>, name: &str, payload: &[u8]) {
        let mut s = Vec::new();
        varint::put(&mut s, name.len() as u64);
        s.extend_from_slice(name.as_bytes());
        s.extend_from_slice(payload);
        w.push(0);
        varint::put(w, s.len() as u64);
        w.extend_from_slice(&s);
    }
    let mut w = WASM_HEADER.to_vec();
    custom(&mut w, STAND_IN_SECTION, m.to_string().as_bytes());
    w
}

/// The name of the module that Wasm module `w` is the [`stand_in`] of, if any.
fn stand_in_name(w: &[u8]) -> Option<&str> {
    std::str::from_utf8(custom_section(w, STAND_IN_SECTION)?).ok()
}

/// The task slots of Wasm module `w` (its custom section `aqr_task_slots`, the spec's "Tasks"),
/// those of its classes added up (the units that may run at once), 0 where it has none, or not
/// a valid one (which loading it fails on).
fn task_slots(w: &[u8]) -> u64 {
    match custom_section(w, "aqr_task_slots") {
        Some(b) if !b.is_empty() && b.iter().all(|&n| matches!(n & 0x7f, 1..=64)) => {
            b.iter().map(|&n| u64::from(n & 0x7f)).sum()
        }
        _ => 0,
    }
}

/// The task slots of each class of Wasm module `w` (none where it has none, or not a valid one).
pub fn class_slots(w: &[u8]) -> Vec<u64> {
    match custom_section(w, "aqr_task_slots") {
        Some(b) if !b.is_empty() && b.iter().all(|&n| matches!(n & 0x7f, 1..=64)) => {
            b.iter().map(|&n| u64::from(n & 0x7f)).collect()
        }
        _ => Vec::new(),
    }
}

/// The classes of tasks of Wasm module `w`: the bytes of its custom section `aqr_task_slots`,
/// none where it has none (a module that is not valid fails when it is loaded).
pub fn task_classes(w: &[u8]) -> usize {
    custom_section(w, "aqr_task_slots").map_or(0, <[u8]>::len)
}

/// The content of the first custom section `name` of Wasm module `w`, if any.
fn custom_section<'w>(w: &'w [u8], name: &str) -> Option<&'w [u8]> {
    let mut at = WASM_HEADER.len();
    if w.get(..at)? != WASM_HEADER {
        return None;
    }
    while at < w.len() {
        let id = w[at];
        at += 1;
        let size = usize::try_from(varint::get(w, &mut at).ok()?).ok()?;
        let s = w.get(at..at.checked_add(size)?)?;
        at += size;
        if id == 0 {
            let mut p = 0;
            let n = usize::try_from(varint::get(s, &mut p).ok()?).ok()?;
            if s.get(p..p.checked_add(n)?)? == name.as_bytes() {
                return Some(&s[p + n..]);
            }
        }
    }
    None
}

/// Where an archive is written: its bytes, or only how many ([`Count`], to size it).
trait Out {
    fn put(&mut self, b: &[u8]);
    fn len(&self) -> usize;
}

impl Out for Vec<u8> {
    fn put(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }

    fn len(&self) -> usize {
        Vec::len(self)
    }
}

/// Bytes written to a writer, up to the first failure (and nothing after it).
struct Writing<'a> {
    out: &'a mut dyn io::Write,
    len: usize,
    failed: Option<io::Error>,
}

impl Out for Writing<'_> {
    fn put(&mut self, b: &[u8]) {
        if self.failed.is_none() {
            match self.out.write_all(b) {
                Ok(()) => self.len += b.len(),
                Err(e) => self.failed = Some(e),
            }
        }
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// Bytes written, counted only.
struct Count(usize);

impl Out for Count {
    fn put(&mut self, b: &[u8]) {
        self.0 += b.len();
    }

    fn len(&self) -> usize {
        self.0
    }
}

/// The end of an archive whose blocks take `blocks` bytes, without a metadata block.
fn footer(out: &mut impl Out, blocks: usize) {
    footer_with(out, blocks, 0);
}

/// The end of an archive whose blocks take `blocks` bytes, its last metadata block `metadata`
/// bytes before the footer's fields (0: none).
fn footer_with(out: &mut impl Out, blocks: usize, metadata: u64) {
    let mut f = Vec::new();
    varint::put(&mut f, blocks as u64);
    varint::put(&mut f, metadata);
    out.put(&FOOTER_MAGIC);
    out.put(&(f.len() as u16).to_le_bytes());
    out.put(&f);
    out.put(&(f.len() as u16).to_le_bytes());
}

/// Appends a block of `ty` (with a u8 size field) and `data`, widening the size field as
/// needed and padding the block to an even length.
fn block(out: &mut impl Out, ty: u64, data: &[u8]) {
    block_of(out, ty, &[data]);
}

/// Appends a formula block of `num_outputs` outputs and the compressed formula data `data`.
fn formula_block(out: &mut impl Out, num_outputs: u64, data: &[u8]) {
    let mut n = Vec::new();
    varint::put(&mut n, num_outputs);
    block_of(out, FORMULA, &[&n, data]);
}

/// A block of the bytes of `parts` one after the other. A dictionary or module block in full
/// takes a 2-byte size at least, as its type with a 1-byte size is a shorthand block's.
fn block_of(out: &mut impl Out, ty: u64, parts: &[&[u8]]) {
    let start = out.len();
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let ss = match len as u64 {
        0..0x100 if ty != DICTIONARY_RAW && ty != MODULE => 0,
        0..0x1_0000 => 1,
        0x1_0000..0x1_0000_0000 => 2,
        _ => 3,
    };
    let mut head = Vec::with_capacity(16);
    varint::put(&mut head, ty + ss);
    head.extend_from_slice(&(len as u64).to_le_bytes()[..1 << ss]);
    out.put(&head);
    parts.iter().for_each(|p| out.put(p));
    if (out.len() - start) % 2 == 1 {
        out.put(&[0]);
    }
}

/// Appends a dictionary block of `data` (with entropy tables), compressed.
fn dictionary_block(out: &mut impl Out, data: &[u8]) {
    block(out, DICTIONARY, data);
}

/// Appends a dictionary block standing in for another, where an archive is to have as many
/// before its own (as a batch alone has, [`SegmentWriter::standalone`]): raw content of 256
/// bytes, uncompressed, so that it is no shorthand block (spec, "Registered dictionaries and
/// modules").
fn dictionary_stand_in(out: &mut impl Out) {
    let data = stored_external(&[&[0; 256]]);
    debug_assert!(data.len() >= 0x100);
    block(out, DICTIONARY_RAW, &data);
}

impl Encoder {
    /// Appends a compressed or uncompressed (leaf or non-leaf) block of `data`, whichever the
    /// spec allows, compressed as `how` tells (uncompressed where stored, and not kept
    /// compressed already), or with a dictionary where `dicts` has it compressed so (the
    /// dictionaries of a batch, and the index of their first in the segment).
    fn content_block(&self, out: &mut impl Out, leaf: bool, data: &[&[u8]], how: How, dicts: Dicts) -> Result<()> {
        if let Some((p, first)) = dicts
            && let Some((i, c)) = p.used.get(&key_how(data, how))
        {
            let mut index = Vec::new();
            varint::put(&mut index, (first + i) as u64);
            block_of(out, if leaf { LEAF_ZSTD_DICT } else { NON_LEAF_ZSTD_DICT }, &[&index, c]);
            return Ok(());
        }
        let mut streams = self.streams.borrow_mut();
        // (written once and not kept: compressed now, or let go where it was kept)
        if !self.keep.get() {
            let c = match streams.remove(&key_how(data, how)) {
                Some(c) => c,
                None => {
                    drop(streams);
                    let c = self.plan(key(data), data, how)?.carry_out(&mut self.cctx(), data, self.threads)?.0;
                    c.map(crate::Bytes::from)
                }
            };
            content_block(out, leaf, data, c.as_deref());
            return Ok(());
        }
        let c = match streams.entry(key_how(data, how)) {
            MapEntry::Occupied(e) => e.into_mut(),
            MapEntry::Vacant(e) => {
                let plan = self.plan(key(data), data, how)?;
                e.insert(kept(plan.carry_out(&mut self.cctx(), data, self.threads)?.0))
            }
        };
        content_block(out, leaf, data, c.as_deref());
        Ok(())
    }

    /// Appends the non-leaf blocks of a stream of `data`: [`BLOCK`] bytes each but the last
    /// (with a dictionary where `dicts` tells, [`Encoder::content_block`]).
    fn stream_blocks(&self, out: &mut impl Out, data: &[&[u8]], how: How, dicts: Dicts) -> Result<()> {
        for r in block_ranges(total(data)) {
            self.content_block(out, false, &cut(data, r.start, r.len()), how, dicts)?;
        }
        Ok(())
    }

    /// Appends a resource block of `r`, compressed at the level the encoder picks for it, or at
    /// the base level where it does not compress: the block is always compressed.
    fn resource_block(&self, out: &mut impl Out, r: &Resource) -> Result<()> {
        if let Some(s) = r.stored_data() {
            block(out, RESOURCE, s);
            return Ok(());
        }
        let mut resources = self.resources.borrow_mut();
        let c = match resources.entry(key(&[r])) {
            MapEntry::Occupied(e) => e.into_mut(),
            MapEntry::Vacant(e) => e.insert(self.always(r)?),
        };
        block(out, RESOURCE, c);
        Ok(())
    }

    /// The compressed external structure of `r`, compressed at the level the encoder picks for
    /// it, or at the base level where it does not compress: for a block always compressed.
    fn always(&self, r: &[u8]) -> Result<Vec<u8>> {
        let c = self.plan(key(&[r]), &[r], How::Usual)?.carry_out(&mut self.cctx(), &[r], self.threads)?.0;
        match c {
            Some(c) => Ok(c),
            None => always_compressed(&mut self.cctx(), &[r], self.base_level(), self.threads),
        }
    }

    /// About the fewest bytes stream `d` archives to at the level of the encoder, by a sample,
    /// or as soon as it is known to be at most `enough`: for [`Level::Of`], at the lowest and
    /// the highest zstd levels it picks from (its lower ones, the last higher one: some
    /// data, and data of a few hundred KB, which zstd compresses with parameters of its own,
    /// compress better at lower levels), or as it is where they do not compress it. It
    /// compresses the sample at those levels only, the faster first, where picking the level
    /// compresses it at every level: a bound, for telling whether a formula of `d` may archive
    /// smaller.
    pub fn least(&self, d: &[u8], enough: usize) -> Result<usize> {
        let levels = match self.level {
            Level::STORED => return Ok(d.len()),
            Level::Of(n) => {
                let p = Pick::of(n);
                [p.lower, &[p.top()]].concat()
            }
            Level::Zstd(l) => vec![l],
        };
        let (k, sample) = (key(&[d]), sampled(&[d]));
        let mut least = d.len();
        for level in levels {
            let c = self.trial(k, &sample, level)?;
            if let Some(c) = &c {
                least = least.min(scaled(c.len(), sample.len(), d.len()));
            }
            self.trials.borrow_mut().insert((k, level), c);
            if least <= enough {
                break;
            }
        }
        Ok(least)
    }

    /// Whether the encoder would compress stream `d` rather than store it as it is, by a sample
    /// at the probe level (zstd level 1): it stores what that saves less than 1/32 of, as
    /// [`Level::Of`] does (the sample is kept, for when it is compressed).
    pub fn compresses(&self, d: &[u8]) -> Result<bool> {
        if self.level == Level::STORED {
            return Ok(false);
        }
        let (k, sample) = (key(&[d]), sampled(&[d]));
        let c = self.trial(k, &sample, PROBE)?;
        let saves = c.as_ref().is_some_and(|c| saves(c.len(), sample.len()));
        self.trials.borrow_mut().insert((k, PROBE), c);
        Ok(saves)
    }

    /// [`Encoder::compresses`] of each of streams `ds`, on up to the encoder's threads.
    pub fn compresses_each(&self, ds: &[&[u8]]) -> Result<Vec<bool>> {
        if self.level == Level::STORED {
            return Ok(vec![false; ds.len()]);
        }
        let trials = aqr_common::pool::par_map(ds.len(), self.threads, |i| {
            let (k, sample) = (key(&[ds[i]]), sampled(&[ds[i]]));
            THREAD_CCTX.with(|c| Ok((k, sample.len(), compressed(&mut c.borrow_mut(), &[&sample], PROBE, 1)?)))
        });
        let mut kept = self.trials.borrow_mut();
        trials
            .into_iter()
            .map(|t| {
                let (k, n, c): (StreamKey, usize, Compressed) = t?;
                let saves = c.as_ref().is_some_and(|c| saves(c.len(), n));
                kept.insert((k, PROBE), c);
                Ok(saves)
            })
            .collect()
    }

    /// `sample` of the stream whose key is `k` compressed at `level`, as [`Encoder::least`]
    /// compressed it if it did.
    fn trial(&self, k: StreamKey, sample: &[u8], level: i32) -> Result<Option<Vec<u8>>> {
        match self.trials.borrow_mut().remove(&(k, level)) {
            Some(c) => Ok(c),
            None => compressed(&mut self.cctx(), &[sample], level, self.threads),
        }
    }

    /// What to make of `data`, whose key is `k`, at the level of the encoder, compressed as
    /// `how` tells (not stored).
    fn plan(&self, k: StreamKey, data: &[&[u8]], how: How) -> Result<Plan> {
        plan_by(self.level, how, data, &mut |sample, level| match sample {
            Some(sample) => match self.trials.borrow_mut().remove(&(k, level)) {
                Some(c) => Ok(c),
                None => compressed(&mut self.cctx(), &[sample], level, self.threads),
            },
            None => compressed(&mut self.cctx(), data, level, self.threads),
        })
    }

    /// Compresses `streams` as [`Encoder::encode`] would, each on a thread of its own (on up to the encoder's
    /// threads, the calling one and workers of [`aqr_common::pool`]), but those of more than
    /// two of Zstandard's jobs, each on up to `LARGE_THREADS` beforehand: for the blocks of
    /// them written next, the same bytes as one after another.
    pub fn precompress(&self, streams: &[&crate::Stream]) -> Result<()> {
        let blocks =
            streams.iter().flat_map(|s| block_ranges(s.data.len()).map(move |r| (vec![&s.data[r]], self.how(s))));
        self.precompress_blocks(blocks.collect(), false)
    }

    /// [`Encoder::precompress`], but each block of more than one of Zstandard's jobs on as many
    /// workers as are free for its jobs, the largest first (as each block written alone is):
    /// for streams that are all there is to compress, nothing else made meanwhile.
    pub fn precompress_wide(&self, streams: &[&crate::Stream]) -> Result<()> {
        let blocks =
            streams.iter().flat_map(|s| block_ranges(s.data.len()).map(move |r| (vec![&s.data[r]], self.how(s))));
        self.precompress_blocks(blocks.collect(), true)
    }

    /// [`Encoder::precompress`] of `blocks` (each its bytes in pieces, and how it is compressed),
    /// those kept already, or listed before, skipped. Where `wide`, those of more than one of
    /// Zstandard's jobs are compressed first and at once, the largest first, each on as many
    /// workers as are free for its jobs (as each block written alone is): for an archive of one
    /// file, whose blocks are all there is to compress.
    fn precompress_blocks(&self, blocks: Vec<(Vec<&[u8]>, How)>, wide: bool) -> Result<()> {
        let todo: Vec<(Vec<&[u8]>, How)> = {
            let kept = self.streams.borrow();
            let mut seen = std::collections::HashSet::new();
            blocks
                .into_iter()
                .filter(|(d, how)| {
                    let k = key_how(d, *how);
                    !kept.contains_key(&k) && seen.insert(k)
                })
                .collect()
        };
        let level = self.level;
        let total = todo.iter().map(|t| self::total(&t.0) as u64).sum();
        let (done, progress) = (std::sync::atomic::AtomicU64::new(0), self.progress.as_deref());
        let one = |(d, how): &(Vec<&[u8]>, How), threads: usize| -> Result<(StreamKey, Compressed, i32)> {
            let (d, how) = (&d[..], *how);
            let made = THREAD_CCTX.with(|c| {
                let c = &mut c.borrow_mut();
                let plan = plan_by(level, how, d, &mut |sample, l| match sample {
                    Some(sample) => compressed(c, &[sample], l, 1),
                    None => compressed(c, d, l, 1),
                })?;
                let (c, l) = plan.carry_out(c, d, threads)?;
                Ok((key_how(d, how), c, l))
            });
            let n = self::total(d) as u64;
            if let Some(p) = progress {
                p(done.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n, total);
            }
            made
        };
        let jobs = if wide { 1 } else { 2 };
        let (mut large, small): (Vec<_>, Vec<_>) =
            todo.iter().partition(|t| self::total(&t.0) > jobs * ZSTD_JOB as usize);
        let mut made: Vec<_> = match wide {
            true => {
                large.sort_by_key(|t| std::cmp::Reverse(self::total(&t.0)));
                let threads = self.threads;
                aqr_common::pool::par_map(large.len(), threads, |i| one(large[i], threads))
            }
            false => large.into_iter().map(|t| one(t, self.threads.min(LARGE_THREADS))).collect(),
        };
        made.extend(aqr_common::pool::par_map(small.len(), self.threads, |i| one(small[i], 1)));
        let (mut kept, mut levels) = (self.streams.borrow_mut(), self.levels.borrow_mut());
        for m in made {
            let (k, c, l) = m?;
            kept.insert(k, self::kept(c));
            levels.insert(k, l);
        }
        Ok(())
    }

    /// Whether `f` archives smaller than `d` raw. A formula of at most half of the bytes
    /// that `d` is estimated to compress to (by a sample) is taken as smaller without
    /// compressing `d`; for [`Level::Of`], `d` is compressed at most at its base level
    /// to be compared.
    pub fn smaller(&self, f: &Formula, d: &[u8]) -> Result<bool> {
        self.smaller_of(f, None, &[], d)
    }

    /// [`Encoder::smaller`], with stream `s` of `f` the bytes of `pieces` one after the other
    /// rather than its data (which is not read): a stream yet to be put together, sized as it
    /// will be archived.
    pub fn smaller_with(&self, f: &Formula, s: usize, pieces: &[&[u8]], d: &[u8]) -> Result<bool> {
        self.smaller_of(f, Some((s, pieces)), &[], d)
    }

    /// [`Encoder::smaller`] (or [`Encoder::smaller_with`] the stream of `stand_in`), the
    /// non-leaf blocks of streams `free` (by index) not counted: data the archive holds anyway,
    /// as the same data elsewhere ([`Formula::share_slices`]).
    pub fn smaller_but(
        &self,
        f: &Formula,
        stand_in: Option<(usize, &[&[u8]])>,
        free: &[usize],
        d: &[u8],
    ) -> Result<bool> {
        self.smaller_of(f, stand_in, free, d)
    }

    fn smaller_of(&self, f: &Formula, stand_in: Option<(usize, &[&[u8]])>, free: &[usize], d: &[u8]) -> Result<bool> {
        // (the last of the work on `d`, told as its streams and `d` are compressed)
        let streams = f.streams.iter().enumerate().map(|(i, s)| match stand_in {
            Some((t, pieces)) if t == i => total(pieces),
            _ => s.data.len(),
        });
        let _sweep = Swept::over(aqr_common::progress::Sweep::new(d, (streams.sum::<usize>() + d.len()) as u64));
        let mut n = self.len_with(f, stand_in)?;
        if !matches!((&f.root, &f.shared[..]), (Node::Slice { .. }, [])) {
            for &s in free {
                let mut c = Count(0);
                self.stream_blocks(&mut c, &[&f.streams[s].data], self.how(&f.streams[s]), None)?;
                n = n.saturating_sub(c.0);
            }
        }
        // (`d` was compressed, so a formula of it of at most half of its estimate was not one)
        if let Some(&raw) = self.raw_lens.borrow().get(&key(&[d])) {
            return Ok(n < raw);
        }
        if !self.streams.borrow().contains_key(&key(&[d])) {
            let c = match self.plan(key(&[d]), &[d], How::Usual)? {
                Plan::Done(c, _) => c,
                Plan::Level(level, estimate) => {
                    if n <= estimate / 2 {
                        return Ok(true);
                    }
                    // Of compares at the base level, as a formula mostly is smaller, and what
                    // was compared is then not archived: a formula is taken over data that only
                    // a higher level would have compressed to less (by up to what they differ)
                    // (counted, not held, where it is not kept)
                    let counted = match matches!(self.level, Level::Of(_)) && level > self.base_level() {
                        true => Some(self.base_level()),
                        false => (d.len() > RAW_KEPT_MAX).then_some(level),
                    };
                    if let Some(counted) = counted {
                        let c = compressed_len(&mut self.cctx(), &[d], counted, self.threads)?;
                        let raw = archived_raw_len(d, c);
                        self.raw_lens.borrow_mut().insert(key(&[d]), raw);
                        return Ok(n < raw);
                    }
                    compressed_to(&mut self.cctx(), &[d], level, self.threads, estimate)?
                }
                // (counted, not held, where it is not kept, as above)
                Plan::Probe if d.len() > RAW_KEPT_MAX => {
                    let c = compressed_len(&mut self.cctx(), &[d], PROBE, self.threads)?;
                    let raw = archived_raw_len(d, c.filter(|&c| saves(c, d.len())));
                    self.raw_lens.borrow_mut().insert(key(&[d]), raw);
                    return Ok(n < raw);
                }
                Plan::Probe => Plan::Probe.carry_out(&mut self.cctx(), &[d], self.threads)?.0,
            };
            self.streams.borrow_mut().insert(key(&[d]), kept(c));
        }
        Ok(n < self.raw_len(d)?)
    }
}

thread_local! {
    /// The work that the data compressed on this thread is told of as ([`Swept`]).
    static SWEEP: RefCell<Option<aqr_common::progress::Sweep>> = const { RefCell::new(None) };
}

/// The data compressed on this thread told of as a sweep, until dropped ([`external`]); the one
/// before, if any, then as it was.
struct Swept(Option<aqr_common::progress::Sweep>);

impl Swept {
    fn over(s: aqr_common::progress::Sweep) -> Swept {
        Swept(SWEEP.with(|t| t.borrow_mut().replace(s)))
    }
}

impl Drop for Swept {
    fn drop(&mut self) {
        SWEEP.with(|t| *t.borrow_mut() = self.0.take());
    }
}

/// The output that Zstandard is given at a time where it is told of how far it is ([`Swept`]),
/// so that it hands back now and then, for data of more than [`SWEPT_MIN`] bytes: up to 512
/// KiB, Zstandard compresses on the calling thread, which makes other output with less room
/// than it may take.
const SWEPT_WINDOW: usize = 256 << 10;
const SWEPT_MIN: usize = 1 << 20;

thread_local! {
    /// The Zstandard context of a thread compressing streams for [`Encoder::precompress`].
    static THREAD_CCTX: RefCell<CCtx<'static>> = RefCell::new(CCtx::create());
}

/// A compression of a sample at a level (or of all the data, given none).
type Compress<'a> = dyn FnMut(Option<&[u8]>, i32) -> Result<Compressed> + 'a;

/// What to make of `data` at `level`: `compress` compresses a sample at a level (or all of
/// `data`, given none), as [`compressed`] does.
fn plan_by(level: Level, how: How, data: &[&[u8]], compress: &mut Compress) -> Result<Plan> {
    let mut trial = |sample: &[u8], level| compress(Some(sample), level);
    let n = total(data);
    // (where all of it is at hand already, compressed at the probe level now)
    let probe_all = |trial: &mut dyn FnMut(&[u8], i32) -> Result<Compressed>| match n <= SAMPLE {
        true => trial(&data.concat(), PROBE).map(|c| match c.filter(|c| saves(c.len(), n)) {
            Some(c) => Plan::Done(Some(c), PROBE),
            None => Plan::Done(None, STORED),
        }),
        false => Ok(Plan::Probe),
    };
    match level {
        Level::STORED => return Ok(Plan::Done(None, STORED)),
        _ if how == How::Light => return probe_all(&mut trial),
        _ => {}
    }
    match level {
        Level::Zstd(l) if n <= SAMPLE => Ok(Plan::Done(trial(&data.concat(), l)?, l)),
        Level::Zstd(l) => {
            let sample = sampled(data);
            let c = trial(&sample, l)?.map_or(sample.len(), |c| c.len());
            Ok(Plan::Level(l, scaled(c, sample.len(), n)))
        }
        Level::STORED => Ok(Plan::Done(None, STORED)),
        Level::Of(level) => {
            let p = Pick::of(level);
            let sample = sampled(data);
            // (planned before, of the same data: its trials not made again)
            let k = (n >= SMALL).then(|| (key_how(data, how), level));
            let before = k.and_then(|k| PLANNED.lock().unwrap().as_ref().and_then(|m| m.get(&k).copied()));
            if let Some(b) = before {
                return Ok(match b {
                    Planned::Probe => Plan::Probe,
                    Planned::Stored => Plan::Done(None, STORED),
                    Planned::Done(l) => Plan::Done(compress(Some(&sample), l)?, l),
                    Planned::Level(l, size) => Plan::Level(l, size),
                });
            }
            let plan = plan_of(&p, how, n, &sample, compress)?;
            if let Some(k) = k {
                let b = match plan {
                    Plan::Probe => Planned::Probe,
                    Plan::Done(None, STORED) => Planned::Stored,
                    Plan::Done(_, l) => Planned::Done(l),
                    Plan::Level(l, size) => Planned::Level(l, size),
                };
                let mut m = PLANNED.lock().unwrap();
                let m = m.get_or_insert_with(HashMap::new);
                if m.len() >= PLANNED_MAX {
                    m.clear();
                }
                m.insert(k, b);
            }
            Ok(plan)
        }
    }
}

/// What [`plan_by`] made of data of at least [`SMALL`] bytes at a level of [`Level::Of`], by
/// the data's [`key_how`] and the level: the same data met again (a file in two images, of
/// two encoders) planned alike without its trials, the sample of all of it compressed at the
/// level planned. A plan is a function of the data alone, so that what is planned does not
/// depend on which was met first. (Of the Windows 7 SP1 DVD's trials, 15% of the bytes were
/// of data met before.)
#[derive(Clone, Copy)]
enum Planned {
    Probe,
    Stored,
    Done(i32),
    Level(i32, usize),
}

static PLANNED: std::sync::Mutex<Option<HashMap<(StreamKey, u8), Planned>>> = std::sync::Mutex::new(None);

/// Plans [`PLANNED`] holds at most, before it is cleared.
const PLANNED_MAX: usize = 1 << 20;

/// [`plan_by`] at a level of [`Level::Of`] picking `p`, of `n` bytes of data of which `sample` is
/// the sample, by `compress`.
fn plan_of(p: &Pick, how: How, n: usize, sample: &[u8], compress: &mut Compress) -> Result<Plan> {
    let len = |c: &Option<Vec<u8>>| c.as_ref().map_or(sample.len(), Vec::len);
    let probe = compress(Some(sample), PROBE)?;
    if !saves(len(&probe), sample.len()) {
        return match sample.len() < n {
            true => Ok(Plan::Probe),
            false => Ok(Plan::Done(None, STORED)),
        };
    }
    if n < SMALL {
        return Ok(Plan::Done(compress(None, p.base.0)?, p.base.0));
    }
    let mut tried = Vec::new();
    for level in [p.base.0].into_iter().chain(p.higher.iter().map(|h| h.0)).chain(p.lower.iter().copied()) {
        tried.push((level, if level == PROBE { probe.clone() } else { compress(Some(sample), level)? }));
    }
    let lens: Vec<usize> = tried.iter().map(|(_, c)| len(c)).collect();
    let (higher, lower) = lens[1..].split_at(p.higher.len());
    let level = pick(p, sample.len(), lens[0], higher, lower);
    let best = tried.into_iter().find(|t| t.0 == level).expect("a level tried");
    let size = scaled(len(&best.1), sample.len(), n);
    // (dense data at the dense level above any picked: where larger than a sample, as
    // a sample does not tell; else where it is smaller, as the sample is all of it)
    match p.dense {
        Some(dense) if how == How::Dense && sample.len() < n && level >= p.base.0 => Ok(Plan::Level(dense, size)),
        Some(dense) if how == How::Dense && sample.len() == n && level >= p.base.0 => {
            let c = compress(Some(sample), dense)?;
            Ok(if len(&c) < len(&best.1) { Plan::Done(c, dense) } else { Plan::Done(best.1, best.0) })
        }
        _ if sample.len() == n => Ok(Plan::Done(best.1, best.0)),
        _ => Ok(Plan::Level(best.0, size)),
    }
}

/// The zstd level that `p` picks for a sample of `n` bytes, by the bytes it compresses to at
/// its base, its higher and its lower levels (in their order): above the base by the time to
/// compress, else below where smaller by 1/32 at least.
fn pick(p: &Pick, n: usize, base: usize, higher: &[usize], lower: &[usize]) -> i32 {
    let cost = |len: usize, speed: f64| len as f64 + p.rate * n as f64 / speed;
    let (mut best, mut least) = (p.base.0, cost(base, p.base.1));
    for (&(level, speed), &len) in p.higher.iter().zip(higher) {
        if cost(len, speed) < least {
            (best, least) = (level, cost(len, speed));
        }
    }
    if best == p.base.0
        && let Some((&level, _)) = p.lower.iter().zip(lower).find(|&(_, &len)| len <= base - base / 32)
    {
        best = level;
    }
    best
}

/// Bytes of `pieces`, one after the other.
fn total(pieces: &[&[u8]]) -> usize {
    pieces.iter().map(|p| p.len()).sum()
}

/// The `len` bytes from `start` of `pieces`, as pieces.
/// Bytes of a non-leaf block at most: a longer stream is several ([`Encoder::stream_blocks`]),
/// as the spec has content larger than a unit of random access split into blocks. As large as
/// the window of long-distance matching (which every decoder takes): of boot.wim's 494 MB of
/// data, 32 MiB blocks archived 3.1% larger, 64 MiB 2.7%, 128 MiB 0.5%, and decoding held
/// 0.39, 0.48 and 0.71 GB at most (0.66 GB of a block). 1 MiB in this crate's tests.
#[cfg(not(test))]
pub const BLOCK: usize = 128 << 20;
#[cfg(test)]
pub const BLOCK: usize = 1 << 20;

/// Non-leaf blocks of a stream of `len` bytes (one at least).
fn blocks_in(len: usize) -> u64 {
    len.div_ceil(BLOCK).max(1) as u64
}

/// The blocks of each of `streams` streams (of `len_of` bytes) that the slices below `nodes`
/// are over, by stream and block; none for a stream not sliced.
fn sliced<'a>(
    streams: usize,
    len_of: impl Fn(usize) -> usize,
    nodes: impl IntoIterator<Item = &'a Node>,
) -> Vec<Vec<bool>> {
    let mut used: Vec<Vec<bool>> = vec![Vec::new(); streams];
    for n in nodes {
        n.visit(&mut |n| {
            if let &Node::Slice { stream, start, len: len @ 1.. } = n {
                let u = &mut used[stream];
                if u.is_empty() {
                    *u = vec![false; blocks_in(len_of(stream)) as usize];
                }
                slice_blocks(start, len).into_iter().for_each(|(k, _, _)| u[k as usize] = true);
            }
        });
    }
    used
}

/// The index in a formula block's list of each block `used` ([`sliced`]), those of the streams
/// in `order` one after another; and how many are listed.
fn listed(used: &[Vec<bool>], order: impl IntoIterator<Item = usize>) -> (Vec<Vec<Option<u64>>>, u64) {
    let mut ordinals: Vec<Vec<Option<u64>>> = used.iter().map(|u| vec![None; u.len()]).collect();
    let mut next = 0;
    for st in order {
        for (o, &u) in ordinals[st].iter_mut().zip(&used[st]) {
            if u {
                *o = Some(next);
                next += 1;
            }
        }
    }
    (ordinals, next)
}

/// The ranges of the non-leaf blocks of a stream of `len` bytes.
fn block_ranges(len: usize) -> impl Iterator<Item = Range<usize>> {
    (0..blocks_in(len) as usize).map(move |k| k * BLOCK..((k + 1) * BLOCK).min(len))
}

/// The pieces of a slice of a stream at `start` of `len` bytes (one at least): of each block
/// it is over, the block's index in the stream and the start and length in it.
fn slice_blocks(start: u64, len: u64) -> Vec<(u64, u64, u64)> {
    let b = BLOCK as u64;
    if len == 0 {
        return vec![(start / b, start % b, 0)];
    }
    let (first, last) = (start / b, (start + len - 1) / b);
    (first..=last)
        .map(|k| {
            let (s, e) = (start.max(k * b), (start + len).min((k + 1) * b));
            (k, s - k * b, e - s)
        })
        .collect()
}

fn cut<'a>(pieces: &[&'a [u8]], mut start: usize, mut len: usize) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    for p in pieces {
        if len == 0 {
            break;
        }
        if start >= p.len() {
            start -= p.len();
            continue;
        }
        let n = len.min(p.len() - start);
        out.push(&p[start..][..n]);
        (start, len) = (0, len - n);
    }
    out
}

/// [`SAMPLE`] bytes of `data` in [`SAMPLE_CHUNKS`] chunks spread evenly over it, or all of it.
fn sampled<'a>(data: &[&'a [u8]]) -> Cow<'a, [u8]> {
    let n = total(data);
    if let [d] = data
        && n <= SAMPLE
    {
        return (*d).into();
    }
    if n <= SAMPLE {
        return data.concat().into();
    }
    let chunk = SAMPLE / SAMPLE_CHUNKS;
    let step = (n - chunk) / (SAMPLE_CHUNKS - 1);
    (0..SAMPLE_CHUNKS).flat_map(|i| cut(data, i * step, chunk)).flatten().copied().collect::<Vec<_>>().into()
}

/// The [`StreamKey`] of `data`.
fn key(data: &[&[u8]]) -> StreamKey {
    let mut h = Xxh3::new();
    data.iter().for_each(|p| h.update(p));
    (h.digest128(), total(data))
}

/// [`key`] of `data` compressed as `how` tells: a dense or light stream is kept apart from the
/// same bytes compressed otherwise (as a file compared with its formula, say).
fn key_how(data: &[&[u8]], how: How) -> StreamKey {
    let (h, n) = key(data);
    let h = match how {
        How::Usual => h,
        How::Dense => !h,
        How::Light => h.rotate_left(64),
    };
    (h, n)
}

/// [`checksum`] of `data`.
fn checksum_of(data: &[&[u8]]) -> u64 {
    let mut h = Xxh64::new(0);
    data.iter().for_each(|p| h.update(p));
    h.digest()
}

/// The length of the archive of `d` as it is ([`Encoder::raw_len`]), compressed as `c`
/// ([`compressed`]) if given ([`archived_raw_len`]'s reference).
#[cfg(test)]
fn archived_raw(d: &[u8], c: Option<&[u8]>) -> usize {
    let mut n = Count(0);
    n.put(&HEADER_MAGIC);
    content_block(&mut n, true, &[d], c);
    let blocks = n.0 - HEADER_MAGIC.len();
    footer(&mut n, blocks);
    n.0
}

/// Data longer than this that [`Encoder::smaller`] compresses to compare with a formula is only
/// counted, not kept to be archived if it is (compressed again then): what compressing it holds
/// is then a window of its output, whatever its length. (Below, it is kept, of the store.)
const RAW_KEPT_MAX: usize = 4 * BLOCK;

/// [`archived_raw`] of `d` compressed to `c` bytes ([`compressed_len`]) if given, the compressed
/// bytes not at hand.
fn archived_raw_len(d: &[u8], c: Option<usize>) -> usize {
    static ZEROS: [u8; 1 << 16] = [0; 1 << 16];
    let mut n = Count(0);
    n.put(&HEADER_MAGIC);
    match c {
        // (bytes standing in for the compressed ones, which a count does not read)
        Some(c) => {
            let parts: Vec<&[u8]> = (0..c).step_by(ZEROS.len()).map(|i| &ZEROS[..(c - i).min(ZEROS.len())]).collect();
            block_of(&mut n, LEAF_ZSTD, &parts);
        }
        None => block_of(&mut n, LEAF_RAW, &[d, &[0; 8]]),
    }
    let blocks = n.0 - HEADER_MAGIC.len();
    footer(&mut n, blocks);
    n.0
}

/// Appends a block of `data`: compressed as `c` ([`compressed`]) if given, else uncompressed
/// (leaf or non-leaf).
fn content_block(out: &mut impl Out, leaf: bool, data: &[&[u8]], c: Option<&[u8]>) {
    match c {
        Some(c) => block(out, if leaf { LEAF_ZSTD } else { NON_LEAF_ZSTD }, c),
        None => {
            let sum = checksum_of(data).to_le_bytes();
            block_of(out, if leaf { LEAF_RAW } else { NON_LEAF_RAW }, &[data, &[&sum]].concat())
        }
    }
}

/// The compressed external structure of `data` (Zstandard, XXH64), even where the Zstandard
/// blocks are longer than `data`: for blocks that are always compressed. At level [`STORED`],
/// raw Zstandard blocks.
fn always_compressed(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Vec<u8>> {
    match level {
        STORED => Ok(stored_external(data)),
        _ => Ok(external(c, data, level, threads, usize::MAX)?.0),
    }
}

/// The zstd level that [`always_compressed`] takes for raw Zstandard blocks (zstd's own level 0
/// is its default level).
const STORED: i32 = 0;

/// The largest Zstandard block (RFC 8878, `Block_Maximum_Size`).
const ZSTD_BLOCK_MAX: usize = 128 << 10;

/// The compressed external structure of `data` (Zstandard, XXH64) of raw Zstandard blocks.
fn stored_external(data: &[&[u8]]) -> Vec<u8> {
    let data = data.concat();
    let chunks: Vec<&[u8]> = match data.len() {
        0 => vec![&[]],
        _ => data.chunks(ZSTD_BLOCK_MAX).collect(),
    };
    let mut out = Vec::with_capacity(data.len() + 3 * chunks.len() + 16);
    // longer by the block headers: 0, then by how much
    out.push(0);
    varint::put(&mut out, 3 * chunks.len() as u64);
    // the window descriptor (a mantissa of 0): as large as a block at least (Block_Maximum_Size
    // is the smaller of the window and 128 KiB), of 1 KiB at least
    let block = data.len().clamp(1 << 10, ZSTD_BLOCK_MAX);
    out.push(((block.next_power_of_two().trailing_zeros() - 10) << 3) as u8);
    for (i, c) in chunks.iter().enumerate() {
        // Last_Block, Block_Type 0 (raw), Block_Size
        let head = (c.len() as u32) << 3 | (i + 1 == chunks.len()) as u32;
        out.extend_from_slice(&head.to_le_bytes()[..3]);
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&checksum_of(&[&data]).to_le_bytes());
    out
}

/// Zstandard's jobs of multithreaded compression, and how much of the window each job takes
/// over from the data before it (9: all of it). Data is cut in jobs however many the threads,
/// so that it compresses the same on any; this loses 0.1-0.2% against compressing it in one
/// piece, at levels 3 and 4.
const ZSTD_JOB: u32 = 16 << 20;

/// Threads compressing a stream of more than two of Zstandard's jobs in
/// [`Encoder::precompress`], which the rest would wait for on one (10gb's enwik batch: a stream
/// of 0.5 GB, 5.3 s at level 9). Each of Zstandard's workers holds a context of the level and
/// window of its own (all of them, 10, took 0.2-0.3 GB more of 10gb's peak).
const LARGE_THREADS: usize = 4;

/// Bytes of data above which it is compressed with long-distance matching, over a window of
/// all of it up to [`WINDOW_LOG_MAX`]. Below, the window is the level's own (at level 3, 2 MiB
/// of most data), which misses what repeats from farther: at 8 MiB, of the files of an archive
/// of Deskflow.app, the rests of its two programs (a stream of 8.3 MB that a stream class
/// put together) archived to 3.07 MB, and to 2.28 MB with a window of all of it and
/// long-distance matching. From 1 MiB on rather than 8 MiB, the applications of
/// `docs/macos-app-todo.md` archive to 1088.91 -> 1080.60 MB (-0.76%; a window alone, without
/// long-distance matching, 1085.29 MB) and 10gb to 2464.75 -> 2462.14 MB (-0.11%), the
/// instructions retired 0.4% more.
pub(crate) const LONG: usize = 1 << 20;

/// The largest window, as a power of two, that every decoder takes (spec, "Zstandard
/// compression method").
pub(crate) const WINDOW_LOG_MAX: u32 = 27;
const ZSTD_OVERLAP: u32 = 9;

/// The compressed external structure of `data` (Zstandard, XXH64), unless the Zstandard
/// blocks would be longer than `data`. It is compressed on up to `threads` threads (those of
/// the pool there are, [`aqr_common::pool`]), always in
/// Zstandard's multithreaded mode: the output does not depend on how many (up to 1 MiB of data,
/// it is also that of the single-threaded mode), nor on what context `c` compressed before.
fn compressed(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Option<Vec<u8>>> {
    compressed_to(c, data, level, threads, usize::MAX)
}

/// The bytes a stream of `n` bytes compresses to, where a sample of `of` bytes of it compresses
/// to `to`.
fn scaled(to: usize, of: usize, n: usize) -> usize {
    (to as f64 * n as f64 / of.max(1) as f64) as usize
}

/// The most memory a context is kept with ([`compressed`]): it holds what the largest data
/// and highest level it compressed took, and a new one gives that back.
const CCTX_KEPT: usize = 4 << 20;

/// [`compressed`], expected to give about `size` bytes: room is made for a little more, and
/// for as much as Zstandard may give only where that does not do (the output does not depend
/// on the room it has). A stream is not given room for its own size again for nothing.
fn compressed_to(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize, size: usize) -> Result<Option<Vec<u8>>> {
    let (c, longer) = external(c, data, level, threads, size)?;
    Ok((!longer).then_some(c))
}

/// The compressed external structure of `data` as [`compressed_to`] makes it, and whether its
/// Zstandard blocks are longer than `data`.
fn external(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize, size: usize) -> Result<(Vec<u8>, bool)> {
    external_with(c, data, level, threads, size, dict::With::None)
}

fn zstd_error(e: zstd_safe::ErrorCode) -> Error {
    Error::new(format!("zstd: {}", zstd_safe::get_error_name(e)))
}

/// Sets context `c` to compress `data` at `level` on up to `threads` threads, with `dict`:
/// the workers of the pool lent while it compresses, and the length of `data`.
fn begin<'a>(
    c: &mut CCtx<'a>,
    data: &[&[u8]],
    level: i32,
    threads: usize,
    dict: dict::With<'a>,
) -> Result<(aqr_common::pool::Reservation, usize)> {
    let zstd = zstd_error;
    // A context compressed with before keeps its workers and buffers, not its parameters.
    c.reset(ResetDirective::SessionAndParameters).map_err(zstd)?;
    // Zstandard's workers counted in aqr-common's pool: as many workers of it reserved, but
    // for the calling thread, which waits while they compress (one job a worker at most).
    let jobs = total(data).div_ceil(ZSTD_JOB as usize).max(1);
    let lent = aqr_common::pool::reserve(threads.min(jobs).saturating_sub(1));
    let threads = lent.len() + 1;
    for p in [
        CParameter::CompressionLevel(level),
        CParameter::ContentSizeFlag(false),
        CParameter::ChecksumFlag(false),
        CParameter::DictIdFlag(false),
        CParameter::NbWorkers(threads.max(1) as u32),
        CParameter::JobSize(ZSTD_JOB),
        CParameter::OverlapSizeLog(ZSTD_OVERLAP),
    ] {
        c.set_parameter(p).map_err(zstd)?;
    }
    // Compressed as data in one piece is (of a size known, ended with its last bytes: ended
    // apart, data ending at the end of a job ends with an empty block).
    let len = total(data);
    // Long data matched from afar, over a window of all of it as far as decoders must take.
    if len > LONG {
        let window = (64 - (len as u64 - 1).leading_zeros()).min(WINDOW_LOG_MAX);
        c.set_parameter(CParameter::WindowLog(window)).map_err(zstd)?;
        c.set_parameter(CParameter::EnableLongDistanceMatching(true)).map_err(zstd)?;
    }
    match dict {
        dict::With::None => {}
        dict::With::Tables(d) => c.ref_cdict(d).map(drop).map_err(zstd)?,
        dict::With::Raw(d) => c.ref_prefix(d).map(drop).map_err(zstd)?,
    }
    c.set_pledged_src_size(Some(len as u64)).map_err(zstd)?;
    Ok((lent, len))
}

/// The length of what [`compressed`] gives of `data` at `level` (`None` where it does not
/// compress), its output made a window at a time and counted rather than held: the output does
/// not depend on the room it has (Zstandard's multithreaded mode).
fn compressed_len(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Option<usize>> {
    let _s = aqr_common::trace::span("zstd");
    let zstd = zstd_error;
    let (_lent, len) = begin(c, data, level, threads, dict::With::None)?;
    let pieces: Vec<&[u8]> = data.iter().copied().filter(|p| !p.is_empty()).collect();
    let swept = SWEEP.with(|s| s.borrow().is_some());
    let (mut window, mut told) = (Vec::with_capacity(SWEPT_WINDOW), 0);
    // the frame's header, and the bytes of the frame
    let (mut header, mut n) = (Vec::with_capacity(6), 0usize);
    for i in 0..pieces.len().max(1) {
        let (piece, last) = (pieces.get(i).copied().unwrap_or_default(), i + 1 >= pieces.len());
        let end = if last { ZSTD_EndDirective::ZSTD_e_end } else { ZSTD_EndDirective::ZSTD_e_continue };
        let mut input = InBuffer::around(piece);
        loop {
            window.clear();
            let left = c.compress_stream2(&mut OutBuffer::around(&mut window), &mut input, end).map_err(zstd)?;
            let more = (6 - header.len()).min(window.len());
            header.extend_from_slice(&window[..more]);
            n += window.len();
            if swept {
                let consumed = c.get_frame_progression().consumed;
                SWEEP.with(|s| s.borrow_mut().as_mut().map(|s| s.add(consumed.saturating_sub(told))));
                told = consumed;
            }
            if if last { left == 0 } else { input.pos() == piece.len() } {
                break;
            }
        }
    }
    if c.sizeof() > CCTX_KEPT {
        *c = CCtx::create();
    }
    // (as [`external_with`] puts the header in place: a frame header of no flags, then the
    // window descriptor)
    let [m0, m1, m2, m3, 0, _] = header[..] else {
        return Err(Error::new("zstd: unexpected frame header"));
    };
    assert_eq!([m0, m1, m2, m3], ZSTD_MAGIC);
    let blocks = n - 6;
    if blocks > len {
        return Ok(None);
    }
    let head = match len - blocks {
        0 => 1 + varint::len(0),
        delta => varint::len(delta as u64),
    } + 1;
    Ok(Some(head + blocks + 8))
}

/// [`external`], compressed with dictionary `dict` (one a context of a lifetime as long as
/// `c`'s may refer to).
fn external_with<'a>(
    c: &mut CCtx<'a>,
    data: &[&[u8]],
    level: i32,
    threads: usize,
    size: usize,
    dict: dict::With<'a>,
) -> Result<(Vec<u8>, bool)> {
    let _s = aqr_common::trace::span("zstd");
    #[cfg(test)]
    tests::COMPRESSED.with(|n| n.set(n.get() + 1));
    let zstd = zstd_error;
    let (_lent, len) = begin(c, data, level, threads, dict)?;
    let pieces: Vec<&[u8]> = data.iter().copied().filter(|p| !p.is_empty()).collect();
    // the frame, then its checksum
    let bound = zstd_safe::compress_bound(len).saturating_add(8);
    let room = size.saturating_add(size / 8).saturating_add(4096).saturating_add(8);
    let mut frame = Vec::with_capacity(room.min(bound));
    // (told of, the bytes compressed so far after each call, and the output a window at a time
    // where Zstandard's workers make it: on this thread, it depends on the room it has)
    let swept = SWEEP.with(|s| s.borrow().is_some());
    let windowed = swept && len > SWEPT_MIN;
    let (mut window, mut told) = (Vec::with_capacity(if windowed { SWEPT_WINDOW } else { 0 }), 0);
    for i in 0..pieces.len().max(1) {
        let (piece, last) = (pieces.get(i).copied().unwrap_or_default(), i + 1 >= pieces.len());
        let end = if last { ZSTD_EndDirective::ZSTD_e_end } else { ZSTD_EndDirective::ZSTD_e_continue };
        let mut input = InBuffer::around(piece);
        loop {
            let left = if windowed {
                window.clear();
                let left = c.compress_stream2(&mut OutBuffer::around(&mut window), &mut input, end).map_err(zstd)?;
                frame.extend_from_slice(&window);
                left
            } else {
                if frame.len() == frame.capacity() {
                    frame.reserve_exact(bound.saturating_sub(frame.len()).max(4096));
                }
                let pos = frame.len();
                c.compress_stream2(&mut OutBuffer::around_pos(&mut frame, pos), &mut input, end).map_err(zstd)?
            };
            if swept {
                let consumed = c.get_frame_progression().consumed;
                SWEEP.with(|s| s.borrow_mut().as_mut().map(|s| s.add(consumed.saturating_sub(told))));
                told = consumed;
            }
            if if last { left == 0 } else { input.pos() == piece.len() } {
                break;
            }
        }
    }
    if c.sizeof() > CCTX_KEPT {
        *c = CCtx::create();
    }
    // a frame header of no flags, then the window descriptor
    let [m0, m1, m2, m3, 0, window, blocks @ ..] = &frame[..] else {
        return Err(Error::new("zstd: unexpected frame header"));
    };
    assert_eq!([*m0, *m1, *m2, *m3], ZSTD_MAGIC);
    let longer = blocks.len() > len;
    // the header put in place of the frame's, in the same vector: the original size by how much
    // longer it is, or `0` then by how much shorter
    let mut head = Vec::with_capacity(12);
    match len - blocks.len().min(len) {
        0 => {
            head.push(0);
            varint::put(&mut head, (blocks.len() - len) as u64);
        }
        delta => varint::put(&mut head, delta as u64),
    }
    head.push(*window);
    let sum = checksum_of(data).to_le_bytes();
    let Some(at) = 6usize.checked_sub(head.len()) else {
        // (a varint of 6 bytes: 32 GiB that compress to nothing)
        return Ok(([&head[..], &frame[6..], &sum].concat(), longer));
    };
    frame[at..6].copy_from_slice(&head);
    frame.drain(..at);
    frame.extend_from_slice(&sum);
    Ok((frame, longer))
}

/// Why an archive does not decode, as the spec classifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The archive violates the spec (or a module fails on its inputs).
    Invalid(String),
    /// The archive is encrypted, and no passphrase given unwraps its file key.
    Decryption(String),
    /// The archive exceeds what this decoder supports: a limit, or a feature not implemented
    /// yet (a module without a native implementation among them).
    Resource(String),
    /// The archive has a block reserved for critical extensions, which a later version of the
    /// spec may define.
    Unsupported(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Invalid(m) => write!(f, "invalid archive: {m}"),
            Failure::Resource(m) => write!(f, "resource failure: {m}"),
            Failure::Unsupported(m) => write!(f, "unsupported failure: {m}"),
            Failure::Decryption(m) => write!(f, "decryption failure: {m}"),
        }
    }
}

impl std::error::Error for Failure {}

/// A result of decoding.
pub type Decoded<T> = std::result::Result<T, Failure>;

fn invalid<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Invalid(msg.into()))
}

fn resource<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Resource(msg.into()))
}

/// Runs the modules of archives: applies the Wasm module of a module block as the spec's
/// "apply a module" does, natively where it knows the module, say.
pub trait Runner: Sync {
    /// Wasm module `wasm` made ready to apply, once for each module block applied; a failure if
    /// it is not a module, or cannot be run here.
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>>;
}

/// A module made ready to apply ([`Runner::load`]).
pub trait Loaded: Send + Sync {
    /// The outputs of an application, of `sizes` bytes each: from `inputs`, the resource
    /// blocks given in order and `params`, with the memories of at most `memory_limit` bytes and
    /// those of each task of class `c` of at most `task_limits[c]` (the spec's task limits; none:
    /// all of the application's under `memory_limit`). The decoder checks the outputs against
    /// `sizes`.
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>>;

    /// Whether it is applied as a part module ([`Loaded::apply_part`]) rather than whole.
    fn is_part(&self) -> bool {
        false
    }

    /// An application as a part module: its inputs pulled as it requests them, its outputs (of
    /// `sizes` bytes) pushed as it commits them, all of them by the time it returns.
    // ([`Loaded::apply`]'s arguments, and the outputs)
    #[allow(clippy::too_many_arguments)]
    fn apply_part(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<()> {
        let _ = (params, resources, inputs, outputs, sizes, memory_limit, task_limits);
        resource("not a part module")
    }
}

/// An input of a part module's application ([`Loaded::apply_part`]), pulled as it is requested.
pub trait PartIn: Send {
    /// Its length in bytes.
    fn len(&self) -> u64;

    /// Its bytes from where they were pulled to before up to `hi` (at most [`PartIn::len`]),
    /// given to `put` in order, in pieces.
    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Decoded<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An output of a part module's application ([`Loaded::apply_part`]), taking its bytes in
/// order as they are committed.
pub trait PartOut: Send {
    fn push(&mut self, d: &[u8]) -> Decoded<()>;
}

/// Runs no module: for archives of none.
pub struct NoModules;

impl Runner for NoModules {
    fn load(&self, _: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        resource("no module runs here")
    }
}

/// `modules` run by `m` ([`Natives`]), known by their [`stand_in`]s.
pub fn natives<'a, M: crate::Modules + Sync>(modules: &[Module], m: &'a M) -> Natives<'a, M> {
    Natives { modules: modules.to_vec(), m, identify: |w| stand_in_name(w).map(str::to_owned) }
}

/// Modules run by [`crate::Modules`], as archives apply them: [`crate::Modules::apply`] for one
/// output, else [`crate::Modules::apply_shared`]. The [`crate::Apply`] that `m` is given has no
/// inputs of its own (they are the evaluated ones). A module block is that of one of them where
/// its Wasm module is identified as it, by its [`stand_in`] unless [`Natives::identified_by`].
pub struct Natives<'a, M> {
    modules: Vec<Module>,
    m: &'a M,
    identify: fn(&[u8]) -> Option<String>,
}

impl<M> Natives<'_, M> {
    /// The modules known by `identify`, which gives the name of the module of a Wasm module
    /// (`name@version`).
    pub fn identified_by(mut self, identify: fn(&[u8]) -> Option<String>) -> Self {
        self.identify = identify;
        self
    }
}

impl<M: crate::Modules + Sync> Runner for Natives<'_, M> {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        struct Native<'a, M> {
            module: Module,
            m: &'a M,
        }

        impl<M: crate::Modules + Sync> Loaded for Native<'_, M> {
            fn apply(
                &self,
                params: &[u8],
                resources: &[Resource],
                inputs: &[&[u8]],
                sizes: &[u64],
                memory_limit: u64,
                task_limits: &[u64],
            ) -> Decoded<Vec<Vec<u8>>> {
                let a = crate::Apply {
                    module: self.module,
                    resources: resources.to_vec(),
                    params: params.to_vec(),
                    inputs: Vec::new(),
                    len: sizes.iter().sum(),
                    memory_limit,
                    task_limits: task_limits.to_vec(),
                };
                match sizes {
                    [_] => self.m.apply(&a, inputs).map(|out| vec![out]),
                    _ => self.m.apply_shared(&crate::Shared { apply: a, sizes: sizes.to_vec(), split: false }, inputs),
                }
                .map_err(|e| Failure::Invalid(e.0))
            }
        }

        let name = (self.identify)(wasm).ok_or_else(|| Failure::Resource("an unknown module".into()))?;
        let module = self.modules.iter().copied().find(|m| m.to_string() == name);
        let module = module.ok_or_else(|| Failure::Resource(format!("no native implementation of {name}")))?;
        Ok(Box::new(Native { module, m: self.m }))
    }
}

/// What an archive holds: a single file (where it has no metadata block), or files in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    File(Vec<u8>),
    /// The entries, and the contents of their files (by [`EntryNode::File`]).
    Tree(Box<Tree>, Vec<Vec<u8>>),
}

/// The file in archive `c`, an archive without a metadata block, its modules run by `runner`.
pub fn decode(c: &[u8], runner: &dyn Runner) -> Decoded<Vec<u8>> {
    match decode_value(c, runner)? {
        Value::File(d) => Ok(d),
        Value::Tree(..) => resource("an archive of several files"),
    }
}

/// What archive `c` holds, its modules run by `runner`.
pub fn decode_value(c: &[u8], runner: &dyn Runner) -> Decoded<Value> {
    decode_value_with(c, runner, 1)
}

/// [`decode_value`], its leaf blocks evaluated at once on up to `threads` threads (the calling
/// one and workers of [`aqr_common::pool`]). The first failure of a leaf, in their order, is
/// the archive's.
pub fn decode_value_with(c: &[u8], runner: &dyn Runner, threads: usize) -> Decoded<Value> {
    open(c, runner)?.value(threads)
}

/// How far [`Archive::each_leaf`] is, as it tells [`Archive::on_progress`]: the bytes of the
/// data blocks decompressed (as they are in the archive, whether Zstandard data or not), and of
/// the leaves' outputs evaluated, each of all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reached {
    pub decompressed: (u64, u64),
    pub evaluated: (u64, u64),
}

/// What [`Archive::each_leaf`] tells as it goes ([`Archive::on_progress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// How far it is, as each data block is decoded and each leaf evaluated.
    Reached(Reached),
    /// Leaf `i` (by its index among the leaf blocks) being evaluated, then done with (its outputs
    /// taken by the caller too).
    Started(usize),
    Ended(usize),
}

/// What is told of the [`Step`]s, from any of the threads evaluating the leaves (not always in
/// order: a later [`Step::Reached`] may tell less).
pub type Telling = Arc<dyn Fn(Step) + Send + Sync>;

/// A content of a tree: its leaf, its output of it and its size.
type ContentRef = (usize, usize, u64);

/// How far an archive conforms to the archival profile ([`Archive::archival`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Archival {
    /// Leaf data blocks without a checksum.
    pub unchecked_leaves: usize,
    /// Formula blocks with an output no Checksum instruction covers.
    pub unchecked_formulas: usize,
}

impl Archival {
    pub fn conforms(&self) -> bool {
        self.unchecked_leaves == 0 && self.unchecked_formulas == 0
    }
}

/// An archive opened ([`open`]): its blocks framed, and its tree read where it has one. The
/// data of its blocks is decoded as the leaves are evaluated, a non-leaf block when first sliced
/// (and dropped once no slice of it is left), so that evaluating them takes about the memory of
/// those evaluated at once.
pub struct Archive<'c, 'n> {
    kept: Archived<'c>,
    runner: &'n dyn Runner,
    /// The modules loaded, by module block, as they are first applied.
    loaded: Vec<OnceLock<Decoded<Box<dyn Loaded + 'n>>>>,
    /// The tree, and its contents (each a leaf, an output of it and its size).
    tree: Option<(Tree, Vec<ContentRef>)>,
    /// What the last segment tells of the writing it ends ([`Archive::status`]).
    status: Option<Status>,
    /// The budget the leaves evaluated take of by their weights ([`Archive::set_budget`]).
    budget: Option<&'n Budget>,
    /// Told how far the evaluation is ([`Archive::on_progress`]).
    tell: Option<Telling>,
    /// By leaf, the item its evaluation is told of as ([`Archive::set_items`]).
    items: Vec<Option<usize>>,
    /// Whether the pages of the blocks are let go of as they are read ([`Archive::let_go_read`]).
    let_go: bool,
}

/// An archive's blocks: all of them read ([`open`]), or as its metadata blocks tell where they
/// are, read as they are needed ([`open_indexed`]).
enum Archived<'c> {
    Parsed(Box<Blocks<'c>>),
    Indexed(Box<index::Index<'c>>),
}

/// Archive `c` opened, its modules run by `runner`. An encrypted archive is a decryption
/// failure: its payload decrypted ([`envelope`]) is opened by [`open_payload`].
pub fn open<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    if envelope::is_encrypted(c) {
        return Err(Failure::Decryption("an encrypted archive, and no passphrase".into()));
    }
    open_as(c, runner, false)
}

/// The decrypted payload `c` of an encrypted archive opened, as [`open`] does, but for the
/// constraints of a payload: a size hint block, and no spacer blocks.
pub fn open_payload<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    open_as(c, runner, true)
}

/// Archive `c` (the payload of an encrypted archive where `sealed`) opened as [`open`] does, but
/// salvaged (spec, "Salvage"): a file whose block is not there (an archive repaired from a
/// truncation, [`mirror::repair`]) is lost rather than the archive invalid, and so is a file of a
/// formula block that does not read, or that refers to blocks not there; its evaluation fails,
/// the others' as they do. Files are checked by their checksums where they have them.
pub fn open_salvaging<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, sealed: bool) -> Decoded<Archive<'c, 'n>> {
    if envelope::is_encrypted(c) {
        return Err(Failure::Decryption("an encrypted archive, and no passphrase".into()));
    }
    open_with(c, runner, sealed, true)
}

/// The bytes of an archive (from its header magic to the end of its footer) as they are read: a
/// file's (mapped), or the payload of an encrypted archive, decrypted a chunk at a time as it is
/// read ([`envelope::Decrypting`]).
#[derive(Clone, Copy)]
pub enum Payload<'c> {
    Plain(&'c [u8]),
    Sealed(&'c envelope::Decrypting<'c>),
}

impl<'c> Payload<'c> {
    pub fn len(&self) -> usize {
        match self {
            Payload::Plain(c) => c.len(),
            Payload::Sealed(d) => d.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn sealed(&self) -> bool {
        matches!(self, Payload::Sealed(_))
    }

    /// Whether it is an archive of a tree (of a metadata block), as its footer tells: one that
    /// [`open_indexed`] reads only in part.
    pub fn of_tree(&self) -> bool {
        footer_fields(*self).is_ok_and(|f| f.3 != 0)
    }

    /// Bytes `r`: an invalid archive where they are not there (or of chunks that do not
    /// authenticate).
    fn get(&self, r: std::ops::Range<usize>) -> Decoded<&'c [u8]> {
        match *self {
            Payload::Plain(c) => c.get(r).ok_or_else(|| Failure::Invalid("truncated block".into())),
            Payload::Sealed(d) => d.get(r),
        }
    }

    /// All of its bytes (decrypted on up to `threads` threads).
    fn whole(&self, threads: usize) -> Decoded<&'c [u8]> {
        match *self {
            Payload::Plain(c) => Ok(c),
            Payload::Sealed(d) => d.all(threads),
        }
    }
}

/// Archive `p` opened as [`open`] does (an encrypted archive's payload as [`open_payload`]
/// does), but reading only what tells where its blocks are: the footer and the metadata blocks.
/// The blocks of a leaf are read as it is evaluated, those of the leaves taken alone
/// ([`Archive::contents_of`]) only (spec, "Partial access"): a file read alone reads its own
/// blocks, those it refers to, and the blocks between a formula block and the non-leaf blocks
/// it refers to (by their ordinals). The whole archive is read where all of it is evaluated or
/// told of (decrypted on up to `threads` threads), and an archive without a metadata block at
/// once.
pub fn open_indexed<'c, 'n>(p: Payload<'c>, runner: &'n dyn Runner, threads: usize) -> Decoded<Archive<'c, 'n>> {
    open_indexed_with(p, runner, threads, None)
}

/// [`open_indexed`], of the entries at `paths` (each its names in turn, as the tree's) alone:
/// the metadata blocks read back from the last as far as each path is settled, it or an entry
/// above it found as other than a directory (spec, "Partial access"). Its tree has those paths
/// (and what is under them) as the archive's has them, but maybe not its other entries; its
/// leaves are evaluated only as they are taken alone ([`Archive::contents_of`]).
pub fn open_indexed_at<'c, 'n>(
    p: Payload<'c>,
    runner: &'n dyn Runner,
    threads: usize,
    paths: &[Vec<Vec<u8>>],
) -> Decoded<Archive<'c, 'n>> {
    open_indexed_with(p, runner, threads, Some(paths))
}

fn open_indexed_with<'c, 'n>(
    p: Payload<'c>,
    runner: &'n dyn Runner,
    threads: usize,
    paths: Option<&[Vec<Vec<u8>>]>,
) -> Decoded<Archive<'c, 'n>> {
    let footer = footer_fields(p)?;
    if footer.3 == 0 {
        return open_as(p.whole(threads)?, runner, p.sealed());
    }
    let (index, tree, status) = index::open(p, threads, footer, paths)?;
    let loaded = (0..index.modules_len()).map(|_| OnceLock::new()).collect();
    let kept = Archived::Indexed(Box::new(index));
    Ok(Archive { kept, runner, loaded, tree, status, budget: None, tell: None, items: Vec::new(), let_go: false })
}

/// [`open`] of an archive, the payload of an encrypted one where `sealed`.
fn open_as<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, sealed: bool) -> Decoded<Archive<'c, 'n>> {
    open_with(c, runner, sealed, false)
}

/// [`open_as`], salvaging where `salvaging` ([`open_salvaging`]).
fn open_with<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner, sealed: bool, salvaging: bool) -> Decoded<Archive<'c, 'n>> {
    let (blocks, tree, status) = opened(c, sealed, salvaging)?;
    let loaded = blocks.modules.iter().map(|_| OnceLock::new()).collect();
    let kept = Archived::Parsed(Box::new(blocks));
    Ok(Archive { kept, runner, loaded, tree, status, budget: None, tell: None, items: Vec::new(), let_go: false })
}

/// Of archive `p`, as its footer tells: where the footer's fields start, where the archive's
/// blocks end (its footer starts), its size hint and the backward offset of its last metadata
/// block.
fn footer_fields(p: Payload) -> Decoded<(usize, usize, u64, u64)> {
    if p.get(0..HEADER_MAGIC.len().min(p.len()))? != HEADER_MAGIC {
        return invalid("bad header magic");
    }
    let truncated = || Failure::Invalid("truncated footer".into());
    let copy = p.len().checked_sub(2).ok_or_else(truncated)?;
    let u16_of = |d: &[u8]| u16::from_le_bytes([d[0], d[1]]) as usize;
    let n = u16_of(p.get(copy..p.len())?);
    let (fields_start, archive_end) = copy
        .checked_sub(n)
        .and_then(|fields| Some((fields, fields.checked_sub(8).filter(|&end| end >= HEADER_MAGIC.len())?)))
        .ok_or_else(truncated)?;
    let c = p.get(archive_end..p.len())?;
    if c[..6] != FOOTER_MAGIC || u16_of(&c[6..8]) != n {
        return invalid("bad footer");
    }
    let fields = &c[8..c.len() - 2];
    let mut at = 0;
    let (size_hint, metadata) = (leb(fields, &mut at)?, leb(fields, &mut at)?);
    if at != fields.len() || size_hint != (archive_end - HEADER_MAGIC.len()) as u64 {
        return invalid("bad footer");
    }
    Ok((fields_start, archive_end, size_hint, metadata))
}

/// The blocks of archive `c` (the payload of an encrypted one where `sealed`, salvaged where
/// `salvaging`), its tree and its status: what [`open_with`] opens.
#[allow(clippy::type_complexity)]
fn opened(
    c: &[u8],
    sealed: bool,
    salvaging: bool,
) -> Decoded<(Blocks<'_>, Option<(Tree, Vec<ContentRef>)>, Option<Status>)> {
    let (fields_start, archive_end, size_hint, metadata) = footer_fields(Payload::Plain(c))?;
    // (salvaging an encrypted archive's payload, its blocks lost are spacer blocks, its size
    // hint block among them maybe)
    let mut blocks = parse(&c[HEADER_MAGIC.len()..archive_end], 0, size_hint, metadata != 0, sealed && !salvaging)?;
    if metadata != 0 {
        let at = usize::try_from(metadata).ok().and_then(|b| fields_start.checked_sub(b));
        if blocks.segments.last().and_then(|s| s.metadata.as_ref()).map(|m| m.0) != at {
            return invalid("no metadata block where the footer points");
        }
    }
    let outputs: Vec<u64> =
        blocks.leaves.iter().map(|l| if let LeafBlock::Formula { outputs, .. } = l { *outputs } else { 1 }).collect();
    let mut lost = Vec::new();
    // each segment's tree, then merged (spec, "Interpreting an archive")
    // (the metadata blocks' data, no longer held once read)
    let metas: Vec<(usize, Vec<u8>)> = blocks
        .segments
        .iter_mut()
        .filter_map(|s| s.metadata.as_mut().map(|(at, d)| (*at, std::mem::take(d))))
        .collect();
    let mut trees = Vec::with_capacity(metas.len());
    // (where the metadata block before each is: the end of the header magic, less 1, before the first)
    let mut prev = HEADER_MAGIC.len() - 1;
    let mut status = None;
    for (k, (at, d)) in metas.into_iter().enumerate() {
        let m = tree::read(&d)?;
        drop(d);
        status = Some(m.status);
        let seg = &blocks.segments[k];
        let (first, end) = (seg.first, seg.end);
        // (salvaging, those there those the first offsets tell: the others lost with what follows)
        let to = |offsets: &[u64], at_: &[usize]| {
            (offsets.len() == at_.len() || salvaging && offsets.len() > at_.len())
                && offsets.iter().zip(at_).all(|(&o, &b)| at.checked_sub(o as usize) == Some(b))
        };
        if !to(&m.modules, &blocks.module_at[first.modules..end.modules])
            || !to(&m.resources, &blocks.resource_at[first.resources..end.resources])
            || !to(&m.dictionaries, &blocks.dictionary_at[first.dictionaries..end.dictionaries])
        {
            return invalid("module, resource or dictionary offsets not those of the blocks");
        }
        // (a leaf of the segment's, by where it is: its outputs, and the dictionary of a file at it
        // checked)
        let mut leaf_at = |b: usize, dictionary: Option<u64>| -> Decoded<Option<(usize, Option<u64>)>> {
            let leaves = &blocks.leaf_at[first.leaves..end.leaves];
            let Some(leaf) = leaves.binary_search(&b).ok().map(|l| first.leaves + l) else { return Ok(None) };
            let own = blocks.leaf_dictionary[leaf].map_or(0, |d| (d - first.dictionaries) as u64 + 1);
            if dictionary.is_some_and(|d| d != own) {
                return invalid("a file's dictionary not that of its leaf block");
            }
            Ok(Some((leaf, Some(outputs[leaf]))))
        };
        trees.push(tree_of(m, at, prev, &mut leaf_at, outputs.len(), salvaging.then_some(&mut lost))?);
        prev = at;
    }
    let tree = match trees.len() {
        0 => None,
        1 => trees.pop().map(|t| (t.tree, t.refs)),
        _ => Some(tree::merge(trees)),
    };
    // (the leaves lost, after those there: never where they are, never with a dictionary)
    for _ in lost {
        blocks.leaves.push(LeafBlock::Lost);
        blocks.leaf_at.push(usize::MAX);
        blocks.leaf_dictionary.push(None);
        blocks.sizes.leaves.push(0);
    }
    Ok((blocks, tree, status))
}

impl<'c, 'n> Archive<'c, 'n> {
    /// The archive's blocks, all of them read (where they are not yet, now, as [`open`] reads
    /// them).
    fn parsed(&self) -> Decoded<&Blocks<'c>> {
        match &self.kept {
            Archived::Parsed(b) => Ok(b.as_ref()),
            Archived::Indexed(i) => i.parsed(self.tree.as_ref().map(|t| &t.1)),
        }
    }

    /// The tree, where the archive has a metadata block.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref().map(|t| &t.0)
    }

    /// What its last segment's metadata block tells of the writing it ends (`segment-status`;
    /// none where it has no metadata block): whether that writing completed, or what it left out.
    /// An archive whose footer was made again after a metadata block telling nothing may have been
    /// cut short after it, segments after it lost whole.
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// Has leaves evaluated at once only while their weights fit in `budget` beside what the
    /// rest of the process holds of it (or one alone, of those of this evaluation), taken in
    /// their order but for those of long work on a thread, first, and those going before one
    /// waiting for room ([`Gate`]): the weight of a leaf is the most bytes its evaluation holds
    /// at once, as its formula tells (its literals and slices, the inputs and outputs of an
    /// application as it is made, and its memory limit). None by default.
    pub fn set_budget(&mut self, budget: &'n Budget) {
        self.budget = Some(budget);
    }

    /// Lets go of the pages of the archive's blocks once they are read (decoded) in evaluating
    /// ([`aqr_common::pages::let_go`]): an archive mapped from its file, read back to be verified,
    /// so that it is not all resident by the end.
    ///
    /// # Safety
    ///
    /// The archive's bytes are a mapping, read only, of a file (not memory of its own: pages
    /// let go of read as zeros there).
    pub unsafe fn let_go_read(&mut self) {
        self.let_go = true;
    }

    /// Tells `tell` how far the evaluation of the leaves is as it goes (none: tells nothing).
    pub fn on_progress(&mut self, tell: Option<Telling>) {
        self.tell = tell;
    }

    /// Has the evaluation of each leaf (by its index) told of as of item `items[i]` to
    /// [`aqr_common::trace`] (a content, say; none past them).
    pub fn set_items(&mut self, items: Vec<Option<usize>>) {
        self.items = items;
    }

    /// The number of contents of the tree's files.
    pub fn contents_len(&self) -> usize {
        self.tree.as_ref().map_or(0, |t| t.1.len())
    }

    /// The number of leaf blocks.
    pub fn leaves_len(&self) -> usize {
        match &self.kept {
            Archived::Parsed(b) => b.leaves.len(),
            Archived::Indexed(i) => i.leaves_len(),
        }
    }

    /// The bytes of content `c` of the tree, as its metadata block tells.
    pub fn content_size(&self, c: usize) -> Option<u64> {
        self.tree.as_ref()?.1.get(c).map(|r| r.2)
    }

    /// The leaf (by its index among the leaf blocks) of content `c` of the tree.
    pub fn leaf_of(&self, c: usize) -> Option<usize> {
        self.tree.as_ref()?.1.get(c).map(|r| r.0)
    }

    /// Evaluates every leaf, at once on up to `threads` threads, giving `f` each one's outputs
    /// (by its index among the leaf blocks) as it is evaluated. The first failure, in the order
    /// of the leaves, is returned (as is one of `f`); the others may have been given theirs.
    pub fn leaves(&self, threads: usize, f: impl Fn(usize, Vec<Vec<u8>>) -> Decoded<()> + Sync) -> Decoded<()> {
        self.each_leaf(threads, |i, r| f(i, r?))
    }

    /// [`Archive::leaves`], giving `f` each leaf's failure too (that of reading the formula
    /// blocks, before any is evaluated, is returned).
    pub fn each_leaf(
        &self,
        threads: usize,
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        self.each_leaf_into(threads, None, None, f)
    }

    /// [`Archive::each_leaf`] of the leaves (by index) marked in `only`: the others not evaluated,
    /// nor their formula blocks read, nor the non-leaf blocks only they slice decoded.
    pub fn each_leaf_only(
        &self,
        threads: usize,
        only: &[bool],
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        self.each_leaf_into(threads, Some(only), None, f)
    }

    /// The file of an archive of a single file, written to `sink` as it is evaluated: of a
    /// single leaf, a formula's output is not held whole, but written as its pieces are pulled
    /// ([`eval::run`]). Returns its length.
    pub fn write_file(&self, threads: usize, sink: &mut Sink) -> Decoded<u64> {
        if self.tree.is_some() {
            return invalid("an archive of a tree");
        }
        let mut n = 0u64;
        if self.parsed()?.leaves.len() != 1 {
            let Value::File(d) = self.value(threads)? else { unreachable!("an archive of a single file") };
            sink(&d)?;
            return Ok(d.len() as u64);
        }
        let mut counted = |d: &[u8]| {
            n += d.len() as u64;
            sink(d)
        };
        let streaming: Mutex<&mut Sink> = Mutex::new(&mut counted);
        self.each_leaf_into(threads, None, Some(&streaming), |_, outputs| {
            let mut sink = streaming.lock().unwrap();
            outputs?.iter().try_for_each(|d| sink(d))
        })?;
        Ok(n)
    }

    /// The content hashes of the outputs of an archive without a metadata block, in order (of
    /// the leaves, and of each leaf's outputs), each with the checksums its formula block takes
    /// of it: [`Archive::hashed_leaves`], None where any leaf's is not told.
    pub fn hashed(
        &self,
        records: &dyn crate::chash::Records,
        identify: &dyn Fn(&[u8]) -> Option<String>,
        threads: usize,
    ) -> Decoded<Option<Vec<crate::chash::Hashed>>> {
        if self.tree.is_some() {
            return invalid("an archive of a tree");
        }
        let leaves = self.hashed_leaves(records, identify, threads)?;
        Ok(leaves.into_iter().collect::<Option<Vec<_>>>().map(|l| l.concat()))
    }

    /// The content hashes of each leaf's outputs, each with the checksums its formula block takes
    /// of it, worked out without running a module where `records` tell the applications' outputs
    /// by the hashes of their inputs (of their modules as `identify` tells them by their Wasm
    /// modules), the blocks decoded on up to `threads` threads: see [`eval`]'s `hashed`. None
    /// for a leaf whose are not told so.
    pub fn hashed_leaves(
        &self,
        records: &dyn crate::chash::Records,
        identify: &dyn Fn(&[u8]) -> Option<String>,
        threads: usize,
    ) -> Decoded<Vec<Option<Vec<crate::chash::Hashed>>>> {
        use crate::chash::Ch;
        let blocks = self.parsed()?;
        let mut used = vec![false; blocks.non_leaf.len()];
        let env = |leaf: &LeafBlock| blocks.env(leaf, &self.loaded, self.runner);
        let formulas: Vec<Option<Vec<u8>>> = (blocks.leaves.iter())
            .map(|leaf| match *leaf {
                LeafBlock::Formula { data, .. } => {
                    let (d, dict) = blocks.formulas[data];
                    decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d])).map(Some)
                }
                _ => Ok(None),
            })
            .collect::<Decoded<_>>()?;
        let programs: Vec<Option<Program>> = (blocks.leaves.iter().zip(&formulas))
            .map(|(leaf, d)| match (leaf, d) {
                (&LeafBlock::Formula { outputs, .. }, Some(d)) => {
                    let (env, end) = env(leaf);
                    read(d, outputs, &env, end, &mut used).map(Some)
                }
                _ => Ok(None),
            })
            .collect::<Decoded<_>>()?;
        let mut refs = vec![0; blocks.non_leaf.len()];
        for p in programs.iter().flatten() {
            for insn in &p.insns {
                if let &Insn::Slice { block, .. } = insn {
                    refs[p.blocks[block]] += 1;
                }
            }
        }
        let tally = Tally { tell: None, decompressed: AtomicU64::new(0), evaluated: AtomicU64::new(0), totals: (0, 0) };
        let contents = Contents {
            refs: refs.into_iter().map(AtomicUsize::new).collect(),
            data: blocks.non_leaf.iter().map(|&(d, zstd, dict)| Waited::new(Slot::Raw(d, zstd, dict))).collect(),
            dictionaries: &blocks.dictionaries,
            tally: &tally,
            let_go: self.let_go,
        };
        let mut out = Vec::new();
        for (leaf, p) in blocks.leaves.iter().zip(&programs) {
            out.push(match (leaf, p) {
                (&LeafBlock::Stored(raw, zstd, checked, dict), _) => {
                    let d = decoded_with(raw, zstd, checked, dict.map(|d| &blocks.dictionaries[d]))?;
                    contents.read(raw);
                    Some(vec![(Ch::of(&d), Vec::new())])
                }
                (LeafBlock::Formula { .. }, Some(p)) => eval::hashed(
                    p,
                    &env(leaf).0,
                    &contents,
                    &blocks.non_leaf,
                    &blocks.dictionaries,
                    records,
                    identify,
                    threads,
                )?,
                _ => return invalid("a file whose block is lost"),
            });
        }
        Ok(out)
    }

    /// How far the archive conforms to the archival profile (spec, "Archival profile"), as its
    /// blocks tell: its leaf data blocks without a checksum, and its formula blocks with an output
    /// no Checksum instruction covers (every formula block read). An archive opened is standalone
    /// and has no shorthand blocks (which are not supported yet).
    pub fn archival(&self) -> Decoded<Archival> {
        let blocks = self.parsed()?;
        let mut a = Archival::default();
        let mut used = vec![false; blocks.non_leaf.len()];
        for leaf in &blocks.leaves {
            match *leaf {
                LeafBlock::Stored(_, _, checked, _) => a.unchecked_leaves += !checked as usize,
                LeafBlock::Lost | LeafBlock::Unread | LeafBlock::Bad(_) => {}
                LeafBlock::Formula { outputs, data, .. } => {
                    let (d, dict) = blocks.formulas[data];
                    let d = decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))?;
                    let (env, end) = blocks.env(leaf, &self.loaded, self.runner);
                    let p = read(&d, outputs, &env, end, &mut used)?;
                    a.unchecked_formulas += !p.outputs_checked() as usize;
                }
            }
        }
        Ok(a)
    }

    /// [`Archive::each_leaf`], a leaf of a formula written to `sink` as it is evaluated where
    /// it is given ([`eval::run`]): `f` given no output of it then. Where `only` is given, the
    /// leaves (by index) not marked there are not evaluated, nor their formula blocks read, nor
    /// the non-leaf blocks only they slice decoded.
    fn each_leaf_into(
        &self,
        threads: usize,
        only: Option<&[bool]>,
        sink: Option<&Mutex<&mut Sink>>,
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        // (of an archive indexed, those of the leaves taken alone read)
        let partial;
        let blocks = match (&self.kept, only) {
            (Archived::Indexed(i), Some(o)) => {
                let picked: Vec<usize> = (0..i.leaves_len()).filter(|&l| o.get(l) == Some(&true)).collect();
                partial = i.blocks_for(&picked);
                &partial
            }
            _ => self.parsed()?,
        };
        // the leaves evaluated, in order
        let taken = |i: usize| only.is_none_or(|o| o.get(i) == Some(&true));
        let picked: Vec<usize> = (0..blocks.leaves.len()).filter(|&i| taken(i)).collect();
        // the formula blocks of those read first (every one where all are, the archive checked
        // then for non-leaf blocks no formula refers to): a non-leaf block is dropped once nothing
        // is to slice it
        let mut used = vec![false; blocks.non_leaf.len()];
        let env = |leaf: &LeafBlock| blocks.env(leaf, &self.loaded, self.runner);
        let formulas = blocks
            .leaves
            .iter()
            .enumerate()
            .map(|(i, leaf)| match *leaf {
                LeafBlock::Formula { data, .. } if taken(i) => {
                    match blocks.decoded_formulas.get(data).and_then(Option::as_deref) {
                        Some(d) => Ok(Some(std::borrow::Cow::Borrowed(d))),
                        None => {
                            let (d, dict) = blocks.formulas[data];
                            decoded_with(d, true, true, dict.map(|d| &blocks.dictionaries[d]))
                                .map(|d| Some(std::borrow::Cow::Owned(d)))
                        }
                    }
                }
                _ => Ok(None),
            })
            .collect::<Vec<Decoded<_>>>();
        let programs = blocks
            .leaves
            .iter()
            .zip(&formulas)
            .map(|(leaf, d)| match (leaf, d) {
                (&LeafBlock::Formula { outputs, .. }, Ok(Some(d))) => {
                    let (env, end) = env(leaf);
                    read(d, outputs, &env, end, &mut used).map(Some)
                }
                (_, Err(e)) => Err(e.clone()),
                _ => Ok(None),
            })
            .collect::<Vec<Decoded<_>>>();
        // (a formula block that does not read is its leaf's failure, the others evaluated as they
        // are: a file damaged loses itself alone)
        let mut failed: Vec<Option<Failure>> = Vec::with_capacity(programs.len());
        let programs: Vec<Option<Program>> = programs
            .into_iter()
            .map(|p| {
                failed.push(p.as_ref().err().cloned());
                p.ok().flatten()
            })
            .collect();
        // (a non-leaf block of a formula block failing or lost not referenced)
        if failed.iter().any(Option::is_some) || blocks.leaves.iter().any(|l| matches!(l, LeafBlock::Lost)) {
            used.fill(true);
        }
        if only.is_none() && used.contains(&false) {
            return invalid("a non-leaf block no formula block references");
        }
        let mut refs = vec![0; blocks.non_leaf.len()];
        for p in picked.iter().filter_map(|&i| programs[i].as_ref()) {
            for insn in &p.insns {
                if let &Insn::Slice { block, .. } = insn {
                    refs[p.blocks[block]] += 1;
                }
            }
        }
        // by leaf, its weight, the bytes of its outputs and of its work on a thread
        let slots: Vec<u64> = blocks.modules.iter().map(|w| task_slots(w)).collect();
        let (mut weights, mut lens, mut serial) = (Vec::new(), Vec::new(), Vec::new());
        for (leaf, p) in blocks.leaves.iter().zip(&programs) {
            let (w, l, t) = match (leaf, p) {
                (&LeafBlock::Stored(d, zstd, checked, _), _) => {
                    let n = stored_len(d, zstd, checked);
                    (n, n, 0)
                }
                (_, Some(p)) => p.weight(&slots),
                _ => (0, 0, 0),
            };
            weights.push(w);
            lens.push(l);
            serial.push(t);
        }
        // the leaves of long work on a thread first, the longest first, for it to be done beside
        // the others; the others in their order
        // (by their places among those picked)
        let long = |k: &usize| serial[picked[*k]] >= LONG_SERIAL;
        let mut order: Vec<usize> = (0..picked.len()).filter(long).collect();
        order.sort_by_key(|&k| std::cmp::Reverse(serial[picked[k]]));
        order.extend((0..picked.len()).filter(|k| !long(k)));
        let stored = picked.iter().map(|&i| match blocks.leaves[i] {
            LeafBlock::Stored(d, ..) => d.len() as u64,
            _ => 0,
        });
        let sliced = blocks.non_leaf.iter().zip(&refs).filter(|(_, n)| **n > 0).map(|((d, ..), _)| d.len() as u64);
        let data_len = stored.sum::<u64>() + sliced.sum::<u64>();
        let tally = Tally {
            tell: self.tell.as_deref(),
            decompressed: AtomicU64::new(0),
            evaluated: AtomicU64::new(0),
            totals: (data_len, picked.iter().map(|&i| lens[i]).sum()),
        };
        tally.add(0, 0);
        let contents = Contents {
            refs: refs.into_iter().map(AtomicUsize::new).collect(),
            data: blocks.non_leaf.iter().map(|&(d, zstd, dict)| Waited::new(Slot::Raw(d, zstd, dict))).collect(),
            dictionaries: &blocks.dictionaries,
            tally: &tally,
            let_go: self.let_go,
        };
        let weights: Vec<u64> = picked.iter().map(|&i| weights[i]).collect();
        let gate = Gate::new(self.budget, &weights, order);
        let programs: Vec<Mutex<Option<Program>>> = programs.into_iter().map(Mutex::new).collect();
        // each leaf on a worker of its own, which is given back as it is done, for those still
        // running to take for their own work (a stream that takes long alone)
        let done = aqr_common::pool::dispatch_any(
            picked.len(),
            threads,
            || {
                let _w = aqr_common::trace::wait("budget");
                gate.next()
            },
            |k, _admitted| {
                let i = picked[k];
                let (_s, _i) =
                    (aqr_common::trace::span("leaf"), aqr_common::trace::item(self.items.get(i).copied().flatten()));
                tally.step(Step::Started(i));
                let outputs = match blocks.leaves[i] {
                    _ if failed[i].is_some() => Err(failed[i].clone().expect("a failure")),
                    LeafBlock::Lost => invalid("a file whose block is lost"),
                    LeafBlock::Bad(ref e) => Err(e.clone()),
                    LeafBlock::Unread => unreachable!("a leaf taken"),
                    LeafBlock::Stored(raw, zstd, checked, dict) => {
                        let d = decoded_with(raw, zstd, checked, dict.map(|d| &blocks.dictionaries[d]));
                        contents.read(raw);
                        tally.add(raw.len() as u64, 0);
                        d.map(|d| vec![d])
                    }
                    ref leaf @ LeafBlock::Formula { .. } => {
                        let p = programs[i].lock().unwrap().take().expect("each leaf once");
                        let env = env(leaf).0;
                        match sink {
                            Some(sink) => eval::run(p, &env, &contents, Some(&mut **sink.lock().unwrap()), threads),
                            None => eval::run(p, &env, &contents, None, threads),
                        }
                    }
                };
                tally.add(0, lens[i]);
                let r = f(i, outputs);
                tally.step(Step::Ended(i));
                r
            },
        );
        done.into_iter().collect()
    }

    /// What the archive holds, its leaves evaluated as [`Archive::leaves`] does.
    pub fn value(&self, threads: usize) -> Decoded<Value> {
        match self.tree() {
            None => {
                let leaves: Mutex<Vec<Option<Vec<Vec<u8>>>>> =
                    Mutex::new((0..self.leaves_len()).map(|_| None).collect());
                self.leaves(threads, |i, outputs| {
                    leaves.lock().unwrap()[i] = Some(outputs);
                    Ok(())
                })?;
                // the first leaf as it is
                let mut out = Vec::new();
                for d in leaves.into_inner().unwrap().into_iter().flatten().flatten() {
                    if out.is_empty() {
                        out = d;
                    } else {
                        out.extend_from_slice(&d);
                    }
                }
                Ok(Value::File(out))
            }
            Some(t) => {
                let files: Mutex<Vec<Vec<u8>>> = Mutex::new(vec![Vec::new(); self.contents_len()]);
                self.contents(threads, |c, d| {
                    files.lock().unwrap()[c] = d;
                    Ok(())
                })?;
                Ok(Value::Tree(Box::new(t.clone()), files.into_inner().unwrap()))
            }
        }
    }

    /// Evaluates every leaf as [`Archive::leaves`] does, giving `f` each content of the tree
    /// (by [`EntryNode::File`]) once, as it is evaluated; checked against the size of its files.
    pub fn contents(&self, threads: usize, f: impl Fn(usize, Vec<u8>) -> Decoded<()> + Sync) -> Decoded<()> {
        self.contents_of(threads, None, f)
    }

    /// [`Archive::contents`] of the contents marked in `only` alone (by content; all where
    /// `None`): only the leaves of those evaluated, and only the non-leaf blocks they slice
    /// decoded. The other contents of those leaves are not given.
    pub fn contents_of(
        &self,
        threads: usize,
        only: Option<&[bool]>,
        f: impl Fn(usize, Vec<u8>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        let Some((_, refs)) = &self.tree else { return invalid("an archive of a single file") };
        let wanted = |c: usize| only.is_none_or(|o| o.get(c) == Some(&true));
        // by leaf, its outputs taken: output, content, size
        let mut of: Vec<Vec<(usize, usize, u64)>> = vec![Vec::new(); self.leaves_len()];
        refs.iter().enumerate().filter(|&(c, _)| wanted(c)).for_each(|(c, &(l, o, n))| of[l].push((o, c, n)));
        let leaves: Option<Vec<bool>> = only.map(|_| of.iter().map(|o| !o.is_empty()).collect());
        self.each_leaf_into(threads, leaves.as_deref(), None, |i, outputs| {
            let mut outputs = outputs?;
            for &(o, c, n) in &of[i] {
                let Some(d) = outputs.get_mut(o).map(std::mem::take) else {
                    return invalid("a file or a fork of an output its leaf block does not have");
                };
                if d.len() as u64 != n {
                    return invalid("a file of another size than its entry's");
                }
                f(c, d)?;
            }
            Ok(())
        })
    }
}

/// What tells the leaf block at a place of an archive ([`tree_of`]): its index and its outputs
/// (where they are known), a file's dictionary (1-based in its segment) given to be checked; none
/// where none is there.
type LeafAt<'a> = dyn FnMut(usize, Option<u64>) -> Decoded<Option<(usize, Option<u64>)>> + 'a;

/// The tree of metadata block `m` at `at`, the metadata block before it at `prev` (the end of the
/// header magic, less 1, where it is the first), `leaf_at` telling the leaf block at a place (by
/// its index, and its outputs where they are known; a file's dictionary given to be checked),
/// none where none is there; and its contents, each a leaf and an output of it, and the size of
/// the files of it. Salvaging (`lost`), the leaves not there are numbered from `lost_from`.
fn tree_of(
    m: tree::Metadata,
    at: usize,
    prev: usize,
    leaf_at: &mut LeafAt,
    lost_from: usize,
    mut lost: Option<&mut Vec<()>>,
) -> Decoded<tree::SegmentTree> {
    if m.prev != (at - prev) as u64 {
        return invalid(match prev {
            p if p < HEADER_MAGIC.len() => "a first metadata block not pointing to the header",
            _ => "a metadata block not pointing to the one before it",
        });
    }
    let mut refs: Vec<(usize, usize, u64)> = Vec::new();
    let mut ref_of: HashMap<(usize, usize), usize> = HashMap::new();
    // the content of output `output` of the leaf block at relative offset `offset`, of `size`
    // bytes; and the leaf
    // (salvaging, the leaves lost by where they were, numbered after those there)
    let mut lost_at: HashMap<usize, usize> = HashMap::new();
    let mut content = |offset: u64, output: u64, size: u64, dictionary: Option<u64>| -> Decoded<(usize, usize)> {
        let found = match at.checked_sub(offset as usize) {
            Some(b) => leaf_at(b, dictionary)?,
            None => None,
        };
        let (leaf, outputs) = match (found, lost.as_deref_mut()) {
            (Some(found), _) => found,
            (None, Some(lost)) => {
                let k = *lost_at.entry(at.wrapping_sub(offset as usize)).or_insert_with(|| {
                    lost.push(());
                    lost.len() - 1
                });
                (lost_from + k, None)
            }
            (None, None) => return invalid("a file or a fork not at a leaf block"),
        };
        if leaf >= lost_from {
            let c = match ref_of.entry((leaf, output as usize)) {
                MapEntry::Occupied(c) => *c.get(),
                MapEntry::Vacant(e) => {
                    refs.push((leaf, output as usize, size));
                    *e.insert(refs.len() - 1)
                }
            };
            return Ok((c, leaf));
        }
        if outputs.is_some_and(|n| output >= n) {
            return invalid("a file or a fork of an output its leaf block does not have");
        }
        let output = output as usize;
        let c = match ref_of.entry((leaf, output)) {
            MapEntry::Occupied(c) if refs[*c.get()].2 == size => *c.get(),
            MapEntry::Occupied(_) => return invalid("files or forks of an output of other sizes"),
            MapEntry::Vacant(e) => {
                refs.push((leaf, output, size));
                *e.insert(refs.len() - 1)
            }
        };
        Ok((c, leaf))
    };
    let mut extras = m.extras;
    let mut t = Tree { root: std::mem::take(&mut extras[0]), entries: Vec::new() };
    // (whether the root and each entry of the tree has an entry extra, not null)
    let mut given = vec![m.given[0]];
    let mut tombstones = Vec::new();
    // the tree's index of each entry by number, tombstones left out
    let mut index = vec![None; m.entries.len() + 1];
    let mut names = HashSet::with_capacity(m.entries.len());
    if !m.entries.iter().all(|(parent, name, _)| names.insert((*parent, &name[..]))) {
        return invalid("two entries of a name in a directory");
    }
    drop(names);
    for (k, (parent, name, read)) in m.entries.into_iter().enumerate() {
        let k = k + 1;
        let parent_index = match parent {
            0 => None,
            p => Some(index[p].expect("a directory")),
        };
        let node = match read {
            tree::Read::Dir => EntryNode::Dir,
            tree::Read::File { offset, output, size, dictionary } => {
                let (c, _) = content(offset, output, size, Some(dictionary))?;
                EntryNode::File(c)
            }
            tree::Read::Link(target) => EntryNode::Link(target),
            tree::Read::Fifo => EntryNode::Fifo,
            tree::Read::Device { block, major, minor } => EntryNode::Device { block, major, minor },
            tree::Read::Special(0) => {
                if m.given[k] {
                    return invalid("a tombstone of an extra");
                }
                tombstones.push((parent_index, name));
                continue;
            }
            tree::Read::Special(kind) => EntryNode::Special(kind),
        };
        index[k] = Some(t.entries.len());
        given.push(m.given[k]);
        t.entries.push(Entry { parent: parent_index, name, node, extra: std::mem::take(&mut extras[k]) });
    }
    // (after the files, so that their contents are numbered as they are)
    for (k, forks) in m.forks.into_iter().enumerate() {
        for (name, (offset, output, size)) in forks {
            let c = content(offset, output, size, None)?.0;
            match k {
                0 => &mut t.root,
                k => &mut t.entries[index[k].expect("not a tombstone")].extra,
            }
            .forks
            .push((name, c));
        }
    }
    Ok(tree::SegmentTree { tree: t, refs, given, tombstones })
}

fn leb(d: &[u8], at: &mut usize) -> Decoded<u64> {
    varint::get(d, at).map_err(|e| Failure::Invalid(e.0.into()))
}

/// The blocks of an archive, framed: the data of leaf data blocks and non-leaf blocks as it is
/// in the archive (with whether it is Zstandard data, and checked). The blocks of every segment
/// are indexed together, those of a segment after those of the segments before it; what refers
/// to them within a segment (a formula block to module, resource and non-leaf blocks, a block
/// to its dictionary, a metadata block to its blocks) is taken within that segment's
/// ([`Segmented`]).
struct Blocks<'c> {
    leaves: Vec<LeafBlock<'c>>,
    /// Where the leaf, module and resource blocks start, in the archive.
    leaf_at: Vec<usize>,
    module_at: Vec<usize>,
    resource_at: Vec<usize>,
    dictionary_at: Vec<usize>,
    /// The dictionary (by index) of each leaf block, where it takes one.
    leaf_dictionary: Vec<Option<usize>>,
    /// The segments, in their order: an archive without a metadata block has one, of none.
    segments: Vec<Segmented>,
    /// The formula data of the formula blocks as it is in the archive (Zstandard data, checked),
    /// with its dictionary: decoded as the leaves are evaluated, only those of the leaves taken
    /// (an archive has a formula block a leaf, and opening it to take a few of them should not
    /// take all).
    formulas: Vec<(&'c [u8], Option<usize>)>,
    /// The formula data decoded already, by formula block (none, or shorter: not yet), as the
    /// formula blocks of the leaves taken are where they are read for what they refer to
    /// ([`index::Index::blocks_for`]).
    decoded_formulas: Vec<Option<Vec<u8>>>,
    /// The data of each non-leaf block, whether it is Zstandard data, and its dictionary.
    non_leaf: Vec<(&'c [u8], bool, Option<usize>)>,
    /// Where each non-leaf block starts, in the archive.
    non_leaf_at: Vec<usize>,
    modules: Vec<Vec<u8>>,
    resources: Vec<Resource>,
    /// The data of each module and resource block as stored, for ModuleBytes and ResourceBytes
    /// instructions.
    module_data: Vec<&'c [u8]>,
    resource_data: Vec<&'c [u8]>,
    dictionaries: Vec<dict::Dictionary>,
    /// The bytes each block takes in the archive, framing and padding too ([`listing`]).
    sizes: BlockSizes,
}

impl Blocks<'_> {
    /// What formula block `leaf` (a [`LeafBlock::Formula`]) is evaluated in: the module and
    /// resource blocks of its segment before it (`loaded` the modules', by module block, run by
    /// `runner`), and the non-leaf blocks after it; and the end of its segment's non-leaf blocks.
    fn env<'a, 'n>(
        &'a self,
        leaf: &LeafBlock,
        loaded: &'a [OnceLock<Decoded<Box<dyn Loaded + 'n>>>],
        runner: &'n dyn Runner,
    ) -> (Env<'a, 'n>, usize) {
        let &LeafBlock::Formula { modules, resources, non_leaf, segment, .. } = leaf else {
            unreachable!("a formula block")
        };
        let s = &self.segments[segment];
        let (m, r) = (s.first.modules..modules, s.first.resources..resources);
        let env = Env {
            first: non_leaf,
            modules: &self.modules[m.clone()],
            module_data: &self.module_data[m.clone()],
            resource_data: &self.resource_data[r.clone()],
            loaded: &loaded[m],
            resources: &self.resources[r],
            runner,
        };
        (env, s.end.non_leaf)
    }
}

/// The blocks of each kind before a point of an archive (as [`Blocks`] indexes them): those
/// before a segment are not of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    leaves: usize,
    non_leaf: usize,
    modules: usize,
    resources: usize,
    dictionaries: usize,
}

/// A segment of an archive: its blocks (of each kind, those from `first` to `end`), and its
/// metadata block (where it starts in the archive, and its decoded data) where it has one.
struct Segmented {
    first: Counts,
    end: Counts,
    metadata: Option<(usize, Vec<u8>)>,
}

/// The bytes each block takes in the archive, by its index among those of its kind.
#[derive(Default)]
struct BlockSizes {
    leaves: Vec<u64>,
    non_leaf: Vec<u64>,
    modules: Vec<u64>,
    resources: Vec<u64>,
    dictionaries: Vec<u64>,
}

enum LeafBlock<'c> {
    /// Its data, whether it is Zstandard data and whether checked, and its dictionary.
    Stored(&'c [u8], bool, bool, Option<usize>),
    /// The number of outputs and the formula data (by index), with the numbers of module and
    /// resource blocks before it and of non-leaf blocks (of all segments), and its segment.
    Formula { outputs: u64, data: usize, modules: usize, resources: usize, non_leaf: usize, segment: usize },
    /// A leaf block a file is at that is not there (salvaging, [`open_salvaging`]).
    Lost,
    /// A leaf block not read, nor to be evaluated ([`index::Index::blocks_for`]).
    Unread,
    /// A leaf block whose blocks do not read, its failure ([`index::Index::blocks_for`]).
    Bad(Failure),
}

/// The blocks `d` of an archive of `size_hint` with a metadata block or not, the payload of an
/// encrypted archive where `sealed`; `d` starting `base` bytes after the header magic (a
/// segment after the first, [`inspect`]), where the blocks are told to be.
fn parse(d: &[u8], base: usize, size_hint: u64, metadata: bool, sealed: bool) -> Decoded<Blocks<'_>> {
    let mut b = Blocks {
        leaves: Vec::new(),
        leaf_at: Vec::new(),
        module_at: Vec::new(),
        resource_at: Vec::new(),
        dictionary_at: Vec::new(),
        leaf_dictionary: Vec::new(),
        segments: Vec::new(),
        formulas: Vec::new(),
        decoded_formulas: Vec::new(),
        non_leaf: Vec::new(),
        non_leaf_at: Vec::new(),
        modules: Vec::new(),
        resources: Vec::new(),
        module_data: Vec::new(),
        resource_data: Vec::new(),
        dictionaries: Vec::new(),
        sizes: BlockSizes::default(),
    };
    // the generic structure first: a block of an undefined critical type anywhere in it makes
    // the archive unsupported, whatever else is wrong with the other blocks
    let framed = frame(d)?;
    if sealed && framed.first().is_none_or(|b| b.1 != SIZE_HINT) {
        return invalid("an encrypted archive's payload without a size hint block");
    }
    if let Some(&(_, ty, ..)) = framed.iter().find(|&&(_, ty, ..)| critical(ty) && !defined(ty)) {
        return Err(Failure::Unsupported(format!("a block of an undefined critical type {ty:#x}")));
    }
    // the blocks before the segment read
    let mut first = Counts::default();
    let counts = |b: &Blocks| Counts {
        leaves: b.leaves.len(),
        non_leaf: b.non_leaf.len(),
        modules: b.modules.len(),
        resources: b.resources.len(),
        dictionaries: b.dictionaries.len(),
    };
    for (start, ty, data, size) in framed {
        let at = base + start + HEADER_MAGIC.len();
        // (a dictionary of the segment's, by its index in it)
        let dicts = (first.dictionaries, b.dictionaries.len());
        let dictionary_of = |d, has| dictionary_of(d, has, dicts.0, dicts.1);
        match ty {
            0x00..=0x03 if sealed => return invalid("a spacer block in an encrypted archive's payload"),
            // spacer blocks, of any space: only compressors have to zero it
            0x00..=0x03 => {}
            SIZE_HINT if base + start == 0 => {
                let mut p = 0;
                if leb(data, &mut p)? != size_hint || p != data.len() {
                    return invalid("bad size hint block");
                }
            }
            // data blocks: `001k kcss` (leaf) and `0100 kkss` (non-leaf, always checked), of a
            // dictionary where kk is 2
            0x20..=0x37 => {
                let (data, dict) = dictionary_of(data, ty & 0x10 != 0)?;
                b.leaf_at.push(at);
                b.sizes.leaves.push(size);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Stored(data, ty & 0x18 != 0, ty & 0x04 != 0, dict));
            }
            0x40..=0x4b => {
                let (data, dict) = dictionary_of(data, ty & 0x08 != 0)?;
                b.non_leaf.push((data, ty & 0x0c != 0, dict));
                b.non_leaf_at.push(at);
                b.sizes.non_leaf.push(size);
            }
            // a shorthand block: the hash of a registered dictionary
            0x50 => return resource("registered dictionaries are not supported yet"),
            // dictionary blocks: `0101 0tss`
            0x51..=0x57 => {
                b.dictionary_at.push(at);
                b.sizes.dictionaries.push(size);
                b.dictionaries.push(dict::Dictionary::read(decoded(data, true, true)?, ty & 0x04 != 0)?);
            }
            // a shorthand block: the hash of a registered module
            0x60 => return resource("registered modules are not supported yet"),
            0x61..=0x63 => {
                b.module_at.push(at);
                b.sizes.modules.push(size);
                b.modules.push(decoded(data, true, true)?);
                b.module_data.push(data);
            }
            0x64..=0x67 => {
                b.resource_at.push(at);
                b.sizes.resources.push(size);
                b.resources.push(Resource::new(decoded(data, true, true)?));
                b.resource_data.push(data);
            }
            // formula blocks: `0111 0dss`
            0x70..=0x77 => {
                let (data, dict) = dictionary_of(data, ty & 0x04 != 0)?;
                let mut p = 0;
                let outputs = leb(data, &mut p)?;
                if outputs == 0 {
                    return invalid("a formula block of no outputs");
                }
                b.formulas.push((&data[p..], dict));
                b.leaf_at.push(at);
                b.sizes.leaves.push(size);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Formula {
                    outputs,
                    data: b.formulas.len() - 1,
                    modules: b.modules.len(),
                    resources: b.resources.len(),
                    non_leaf: b.non_leaf.len(),
                    segment: b.segments.len(),
                });
            }
            // mirror blocks and undefined non-critical blocks, not read
            _ if !critical(ty) => {}
            0x10..=0x17 if metadata => {
                let end = counts(&b);
                let m = Some((at, decoded(data, ty & 0x04 != 0, true)?));
                b.segments.push(Segmented { first, end, metadata: m });
                first = end;
            }
            0x10..=0x17 => return invalid("a metadata block, but none in the footer"),
            _ => return invalid(format!("bad block of type {ty:#x}")),
        }
    }
    let end = counts(&b);
    if metadata && end != first {
        return invalid("content blocks after the last metadata block");
    }
    if !metadata {
        b.segments.push(Segmented { first, end, metadata: None });
    }
    Ok(b)
}

/// A block framed: where it starts, its type, its data and the bytes it takes (with its
/// padding).
type Framed<'d> = (usize, u64, &'d [u8], u64);

/// The blocks `d` framed.
fn frame(d: &[u8]) -> Decoded<Vec<Framed<'_>>> {
    let mut framed = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let start = at;
        let ty = leb(d, &mut at)?;
        if at - start != varint::len(ty) {
            return invalid("a block type not in the shortest encoding");
        }
        let width = 1 << (ty % 4);
        let size = d.get(at..at + width).ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += width;
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        let data = usize::try_from(size)
            .ok()
            .and_then(|n| d.get(at..at.checked_add(n)?))
            .ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += data.len();
        // the padding, of any value: only compressors have to zero it
        if (at - start) % 2 == 1 {
            if at == d.len() {
                return invalid("truncated block");
            }
            at += 1;
        }
        framed.push((start, ty, data, (at - start) as u64));
    }
    Ok(framed)
}

/// The leading digit of block type `ty`: the first digit of its hexadecimal representation,
/// padded with zeros to at least 2 digits.
fn leading_digit(ty: u64) -> u64 {
    let digits = (64 - ty.leading_zeros()).div_ceil(4).max(2);
    ty >> (4 * (digits - 1))
}

/// Whether block type `ty` is critical: its leading digit is from 0 to 9.
fn critical(ty: u64) -> bool {
    leading_digit(ty) <= 9
}

/// Whether critical block type `ty` is defined by the spec, block-like structures included.
fn defined(ty: u64) -> bool {
    matches!(
        ty,
        0x00..=0x04
            | 0x10..=0x17
            | 0x20..=0x37
            | 0x40..=0x4b
            | 0x50..=0x57
            | 0x60..=0x67
            | 0x70..=0x77
            | HEADER_TYPE
            | OVERLAY_HEADER_TYPE
            | FOOTER_TYPE
            | OVERLAY_FOOTER_TYPE
            | ENCRYPTED_HEADER_TYPE
    )
}

/// The data of a block's data `d` past its dictionary index where it has one (`has`), and the
/// index: one of the dictionary blocks from `first` to `before` (those of its segment before it),
/// as an index among all.
fn dictionary_of(d: &[u8], has: bool, first: usize, before: usize) -> Decoded<(&[u8], Option<usize>)> {
    if !has {
        return Ok((d, None));
    }
    let mut at = 0;
    let i = leb(d, &mut at)?;
    match usize::try_from(i).ok().filter(|&i| i < before - first) {
        Some(i) => Ok((&d[at..], Some(first + i))),
        None => invalid("a dictionary index of no dictionary block before"),
    }
}

/// The decoded data of a block's data `d`.
fn decoded(d: &[u8], zstd: bool, checked: bool) -> Decoded<Vec<u8>> {
    decoded_with(d, zstd, checked, None)
}

/// [`decoded`], of Zstandard data compressed with dictionary `dict`.
fn decoded_with(d: &[u8], zstd: bool, checked: bool, dict: Option<&dict::Dictionary>) -> Decoded<Vec<u8>> {
    let end = d.len().checked_sub(if checked { 8 } else { 0 }).ok_or_else(|| Failure::Invalid("truncated".into()))?;
    let out = if zstd {
        let mut at = 0;
        // the original size by how much longer it is, or `0` then by how much shorter
        let delta = leb(d, &mut at)?;
        let shrink = if delta == 0 { leb(d, &mut at)? } else { 0 };
        let &window = d.get(at).filter(|_| at < end).ok_or_else(|| Failure::Invalid("truncated".into()))?;
        let blocks = &d[at + 1..end];
        let size = (blocks.len() as u64)
            .checked_add(delta)
            .and_then(|n| n.checked_sub(shrink))
            .ok_or_else(|| Failure::Invalid("bad size".into()))?;
        unzstd(window, blocks, size, dict)?
    } else {
        d[..end].to_vec()
    };
    if checked && checksum(&out).to_le_bytes() != d[end..] {
        return invalid("checksum mismatch");
    }
    Ok(out)
}

/// Zstandard `blocks` of `window` making exactly `size` bytes, with dictionary `dict`.
fn unzstd(window: u8, blocks: &[u8], size: u64, dict: Option<&dict::Dictionary>) -> Decoded<Vec<u8>> {
    let _s = aqr_common::trace::span("unzstd");
    let zstd = |e: zstd_safe::ErrorCode| Failure::Invalid(format!("zstd: {}", zstd_safe::get_error_name(e)));
    // A context of the thread's, kept for the next frame (with its input buffer) where there is
    // no dictionary to refer to: archives have a formula block a leaf, each a frame, and making
    // a context for each was much of opening a big archive.
    thread_local! {
        static DCTX: RefCell<Option<zstd_safe::DCtx<'static>>> = const { RefCell::new(None) };
    }
    match dict {
        None => DCTX.with(|c| match c.try_borrow_mut() {
            Ok(mut c) => {
                let dctx = c.get_or_insert_with(zstd_safe::DCtx::create);
                dctx.reset(ResetDirective::SessionAndParameters).map_err(zstd)?;
                unzstd_with(dctx, window, blocks, size)
            }
            Err(_) => unzstd_with(&mut zstd_safe::DCtx::create(), window, blocks, size),
        }),
        Some(d) => {
            let mut dctx = zstd_safe::DCtx::create();
            match d {
                dict::Dictionary::Raw(d) => dctx.ref_prefix(d).map(drop).map_err(zstd)?,
                dict::Dictionary::Tables(d) => dctx.ref_ddict(d).map(drop).map_err(zstd)?,
            }
            unzstd_with(&mut dctx, window, blocks, size)
        }
    }
}

/// [`unzstd`] with `dctx` (of its dictionary, if any).
fn unzstd_with(dctx: &mut zstd_safe::DCtx, window: u8, blocks: &[u8], size: u64) -> Decoded<Vec<u8>> {
    let zstd = |e: zstd_safe::ErrorCode| Failure::Invalid(format!("zstd: {}", zstd_safe::get_error_name(e)));
    // the frame: a header, then the blocks (not copied after it)
    let head = [ZSTD_MAGIC[0], ZSTD_MAGIC[1], ZSTD_MAGIC[2], ZSTD_MAGIC[3], 0, window];
    let mut frame = [&head[..], blocks].into_iter();
    let mut input = InBuffer::around(frame.next().unwrap());
    // up to one byte more than `size`, to tell a longer output: as much room as that from the
    // start where it can be had (written into as it is, without a window buffer of the
    // context's), else made as the output comes
    let limit = size.saturating_add(1);
    let mut out: Vec<u8> = Vec::new();
    if let Ok(n) = usize::try_from(limit)
        && out.try_reserve_exact(n).is_ok()
    {
        dctx.set_parameter(zstd_safe::DParameter::StableOutBuffer(true)).map_err(zstd)?;
    }
    loop {
        let Some(room) = limit.checked_sub(out.len() as u64).filter(|&r| r > 0) else {
            return invalid("decompressed data longer than declared");
        };
        out.reserve(room.min(1 << 20) as usize);
        let (pos, before) = (out.len(), input.pos());
        let mut o = OutBuffer::around_pos(&mut out, pos);
        let hint = dctx.decompress_stream(&mut o, &mut input).map_err(zstd)?;
        let written = o.pos() > pos;
        if hint == 0 {
            break;
        }
        if input.pos() == input.src.len()
            && let Some(next) = frame.next()
        {
            input = InBuffer::around(next);
        } else if !written && input.pos() == before {
            return invalid("truncated zstd data");
        }
    }
    if input.pos() != input.src.len() || frame.next().is_some() || out.len() as u64 != size {
        return invalid("zstd data of another size than declared");
    }
    Ok(out)
}

/// What a formula block refers to.
struct Env<'a, 'n> {
    /// The ordinal of the first non-leaf block after the formula block.
    first: usize,
    modules: &'a [Vec<u8>],
    /// The data of the module and resource blocks as stored.
    module_data: &'a [&'a [u8]],
    resource_data: &'a [&'a [u8]],
    loaded: &'a [OnceLock<Decoded<Box<dyn Loaded + 'n>>>],
    resources: &'a [Resource],
    runner: &'n dyn Runner,
}

enum Insn<'a> {
    Nop,
    Literal(&'a [u8]),
    Swap(usize),
    Slice { block: usize, start: u64, len: u64 },
    Apply(Apply<'a>),
    Concat(usize),
    Pack(usize),
    Split(Vec<u64>),
    Patch(u64),
    Checksum(u64),
}

struct Apply<'a> {
    module: usize,
    resources: Vec<usize>,
    params: &'a [u8],
    memory_limit: u64,
    /// The task limits, one for each class of the module.
    task_limits: Vec<u64>,
    /// Whether it is of the opcode [`APPLY_OLD`], without task limits.
    old: bool,
    num_inputs: usize,
    output_sizes: Vec<u64>,
}

/// Reads formula data, checking what does not depend on the evaluation.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn num(&mut self) -> Decoded<u64> {
        leb(self.d, &mut self.at)
    }

    /// A number less than `bound`.
    fn index(&mut self, bound: usize) -> Decoded<usize> {
        let n = self.num()?;
        usize::try_from(n)
            .ok()
            .filter(|&n| n < bound)
            .ok_or_else(|| Failure::Invalid(format!("index {n} out of range")))
    }

    /// A count of items of at least a byte each.
    fn count(&mut self) -> Decoded<usize> {
        let n = self.num()?;
        usize::try_from(n)
            .ok()
            .filter(|&n| n <= self.d.len() - self.at)
            .ok_or_else(|| Failure::Invalid(format!("count {n} out of range")))
    }

    fn nums(&mut self) -> Decoded<Vec<u64>> {
        let n = self.count()?;
        (0..n).map(|_| self.num()).collect()
    }

    fn bytes(&mut self) -> Decoded<&'a [u8]> {
        let n = self.count()?;
        self.at += n;
        Ok(&self.d[self.at - n..self.at])
    }
}

/// A formula block read: its instructions, and the non-leaf blocks, resources and modules they
/// refer to (by their ordinals among all).
struct Program<'a> {
    insns: Vec<Insn<'a>>,
    blocks: Vec<usize>,
    resources: Vec<usize>,
    modules: Vec<usize>,
}

impl Program<'_> {
    /// Whether a Checksum instruction covers each output: the item on top of the stack as it
    /// runs, the same item as Swap instructions move it (spec, "Checksum instruction").
    fn outputs_checked(&self) -> bool {
        let mut st: Vec<bool> = Vec::new();
        let replace = |st: &mut Vec<bool>, n: usize, m: usize| {
            st.truncate(st.len() - n);
            st.resize(st.len() + m, false);
        };
        for insn in &self.insns {
            match insn {
                Insn::Nop => {}
                Insn::Literal(_) | Insn::Slice { .. } => st.push(false),
                &Insn::Swap(d) => {
                    let top = st.len() - 1;
                    st.swap(top, top - d);
                }
                Insn::Apply(a) => replace(&mut st, a.num_inputs, a.output_sizes.len()),
                &Insn::Concat(n) | &Insn::Pack(n) => replace(&mut st, n, 1),
                Insn::Split(sizes) => replace(&mut st, 1, sizes.len()),
                Insn::Patch(_) => replace(&mut st, 2, 1),
                Insn::Checksum(_) => *st.last_mut().expect("checked when read") = true,
            }
        }
        st.iter().all(|&c| c)
    }

    /// The most bytes on the stack at once (with the inputs and outputs of an instruction as it
    /// runs both, or an application's memory limit where larger: the working memory of its module,
    /// which a native one takes too), as the sizes the program tells: the weight of its leaf
    /// ([`Archive::set_budget`]); and the bytes on it at the end, its outputs. Of a module of
    /// `slots[m]` task slots (by module block), the limit counts the tasks' copies of the working
    /// memory as memories of their own, which a native one does not make and Wasm ones share
    /// (copying on write): an instance's share of it is weighed, as of the instance and as many
    /// tasks. Third, the bytes of the inputs and outputs of its applications of modules of no
    /// tasks: of the work that takes a thread, its longest.
    fn weight(&self, slots: &[u64]) -> (u64, u64, u64) {
        let (mut st, mut on, mut most, mut serial) = (Vec::<u64>::new(), 0u64, 0u64, 0u64);
        let pop = |st: &mut Vec<u64>, on: &mut u64, n: usize| -> u64 {
            let taken: u64 = st.drain(st.len().saturating_sub(n)..).sum();
            *on -= taken;
            taken
        };
        for insn in &self.insns {
            let limit = match insn {
                // (the instance's share: of all of the application's where there are no task limits)
                Insn::Apply(a) if a.memory_limit != crate::NO_LIMIT && a.task_limits.is_empty() => {
                    a.memory_limit / (1 + slots[self.modules[a.module]])
                }
                Insn::Apply(a) if a.memory_limit != crate::NO_LIMIT => a.memory_limit,
                _ => 0,
            };
            let (ins, outs): (u64, Vec<u64>) = match insn {
                Insn::Literal(l) => (0, vec![l.len() as u64]),
                &Insn::Slice { len, .. } => (0, vec![len]),
                Insn::Apply(a) => (pop(&mut st, &mut on, a.num_inputs), a.output_sizes.clone()),
                &Insn::Concat(n) | &Insn::Pack(n) => {
                    let taken = pop(&mut st, &mut on, n);
                    (taken, vec![taken])
                }
                Insn::Split(sizes) => (pop(&mut st, &mut on, 1), sizes.clone()),
                &Insn::Patch(len) => (pop(&mut st, &mut on, 2), vec![len]),
                Insn::Nop | Insn::Swap(_) | Insn::Checksum(_) => continue,
            };
            let out: u64 = outs.iter().sum();
            if let Insn::Apply(a) = insn
                && slots[self.modules[a.module]] == 0
            {
                serial = serial.saturating_add(ins).saturating_add(out);
            }
            most = most.max(on.saturating_add(ins.saturating_add(out).max(limit)));
            on = on.saturating_add(out);
            st.extend(outs);
        }
        (most, on, serial)
    }
}

/// The size of the data of a stored leaf block `d` (where it is Zstandard data, by its header;
/// `d`'s own where that is not read).
fn stored_len(d: &[u8], zstd: bool, checked: bool) -> u64 {
    let raw = d.len().saturating_sub(if checked { 8 } else { 0 }) as u64;
    if !zstd {
        return raw;
    }
    let mut at = 0;
    let Ok(delta) = leb(d, &mut at) else { return raw };
    let shrink = if delta == 0 { leb(d, &mut at).unwrap_or(0) } else { 0 };
    raw.saturating_sub(at as u64 + 1).saturating_add(delta).saturating_sub(shrink)
}

/// The bytes of work on a thread from which a leaf is evaluated first ([`Program::weight`]).
const LONG_SERIAL: u64 = 64 << 20;

/// The leaves let in by [`Archive::set_budget`] ([`aqr_common::pool::dispatch_any`]): in an
/// order (of the leaves' indices), each once there is room in the budget for it (or none of
/// this evaluation is in); but while the first not let in waits, the leaves after it that fit
/// go before it, up to the limit's bytes of weights (after which it is let in as soon as it
/// fits, or alone), and past [`PASSING`] leaves that do not at most.
struct Gate<'w> {
    budget: Option<&'w Budget>,
    weights: &'w [u64],
    order: Vec<usize>,
    /// (locked within the budget's lock, never the other way)
    state: Mutex<GateState>,
    /// The leaves in (changed within the budget's lock, or as a leaf ends, which then tells the
    /// budget).
    on: AtomicUsize,
}

/// How many leaves that do not fit in the leaves going before the first waiting pass at most
/// ([`Gate`]).
const PASSING: usize = 64;

struct GateState {
    /// The leaves let in, and the first not (by their places in the order, as below).
    taken: Vec<bool>,
    first: usize,
    /// While it waits: the leaves after it looked at and not let in, the first not looked at,
    /// and the weights of those let in.
    skipped: Vec<usize>,
    scan: usize,
    passed: u64,
}

impl GateState {
    /// A leaf after the first that may go before it, of those that `fits`.
    fn passing(&mut self, fits: impl Fn(usize) -> bool) -> Option<usize> {
        let (first, taken) = (self.first, &self.taken);
        self.skipped.retain(|&i| !taken[i] && i != first);
        if let Some(k) = self.skipped.iter().position(|&i| fits(i)) {
            return Some(self.skipped.remove(k));
        }
        self.scan = self.scan.max(first + 1);
        while self.skipped.len() < PASSING && self.scan < taken.len() {
            let i = self.scan;
            self.scan += 1;
            if fits(i) {
                return Some(i);
            }
            self.skipped.push(i);
        }
        None
    }
}

/// A leaf let in, until dropped, and what it takes of the budget.
struct Admitted<'g>(&'g Gate<'g>, Option<Taken<'g>>);

impl<'w> Gate<'w> {
    fn new(budget: Option<&'w Budget>, weights: &'w [u64], order: Vec<usize>) -> Self {
        let taken = vec![false; weights.len()];
        let state = GateState { taken, first: 0, skipped: Vec::new(), scan: 0, passed: 0 };
        Gate { budget, weights, order, state: Mutex::new(state), on: AtomicUsize::new(0) }
    }

    /// Waits for a leaf to be let in, and gives its index: the first not let in yet if it fits,
    /// alone (of this evaluation) or beside what the budget holds, else one after it that does as
    /// it may pass.
    fn next(&self) -> (usize, Admitted<'_>) {
        let take = |s: &mut GateState| {
            while s.taken.get(s.first) == Some(&true) {
                s.first += 1;
            }
        };
        let Some(budget) = self.budget else {
            let mut s = self.state.lock().unwrap();
            take(&mut s);
            let first = s.first;
            s.taken[first] = true;
            self.on.fetch_add(1, Relaxed);
            return (self.order[first], Admitted(self, None));
        };
        let (i, t) = budget.wait(|room| {
            let mut s = self.state.lock().unwrap();
            take(&mut s);
            let first = s.first;
            let alone = self.on.load(Relaxed) == 0;
            let fits = |i: usize| alone || room.fits(self.weights[self.order[i]]);
            let i = if fits(first) {
                s.passed = 0;
                first
            } else if s.passed < room.limit {
                s.passing(fits)?
            } else {
                return None;
            };
            let w = self.weights[self.order[i]];
            s.taken[i] = true;
            if i != first {
                s.passed = s.passed.saturating_add(w);
            }
            self.on.fetch_add(1, Relaxed);
            Some((self.order[i], w))
        });
        (i, Admitted(self, Some(t)))
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        // (before the budget is given back, which tells those waiting)
        self.0.on.fetch_sub(1, Relaxed);
        drop(self.1.take());
    }
}

/// Formula data `d` of `outputs` outputs read, among `non_leaf` non-leaf blocks, marking those it
/// refers to in `used`: everything checked that does not depend on the evaluation.
fn read<'a>(d: &'a [u8], outputs: u64, env: &Env<'a, '_>, non_leaf: usize, used: &mut [bool]) -> Decoded<Program<'a>> {
    let mut r = Reader { d, at: 0 };
    // the non-leaf blocks referenced, by their ordinals among all
    let (mut blocks, mut next) = (Vec::new(), env.first);
    for delta in r.nums()? {
        let o = usize::try_from(delta)
            .ok()
            .and_then(|d| next.checked_add(d))
            .filter(|&o| o < non_leaf)
            .ok_or_else(|| Failure::Invalid("a referenced block that does not exist".into()))?;
        blocks.push(o);
        next = o + 1;
    }
    let n = r.count()?;
    let resources = (0..n).map(|_| r.index(env.resources.len())).collect::<Decoded<Vec<_>>>()?;
    let n = r.count()?;
    let modules = (0..n).map(|_| r.index(env.modules.len())).collect::<Decoded<Vec<_>>>()?;

    // a pass checking the stack and the lists
    let (mut insns, mut depth) = (Vec::new(), 0usize);
    let (mut b_used, mut r_used, mut m_used) =
        (vec![false; blocks.len()], vec![false; resources.len()], vec![false; modules.len()]);
    let pop = |depth: &mut usize, n: usize| match depth.checked_sub(n) {
        Some(d) => {
            *depth = d;
            Ok(())
        }
        None => invalid("stack underflow"),
    };
    while r.at < d.len() {
        let op = d[r.at];
        r.at += 1;
        let insn = match op {
            NOP => Insn::Nop,
            LITERAL => Insn::Literal(r.bytes()?),
            // the data of a block as stored, pushed as a literal is (of a module block in full:
            // shorthand blocks, which it may not take, are not supported yet)
            MODULE_BYTES => {
                let module = r.index(modules.len())?;
                m_used[module] = true;
                stored_bytes(env.module_data[modules[module]], r.num()?)?
            }
            RESOURCE_BYTES => {
                let resource = r.index(resources.len())?;
                r_used[resource] = true;
                stored_bytes(env.resource_data[resources[resource]], r.num()?)?
            }
            SWAP => Insn::Swap(r.index(depth)?),
            SLICE => {
                let block = r.index(blocks.len())?;
                b_used[block] = true;
                Insn::Slice { block, start: r.num()?, len: r.num()? }
            }
            APPLY | APPLY_OLD => {
                let module = r.index(modules.len())?;
                m_used[module] = true;
                let n = r.count()?;
                let res = (0..n).map(|_| r.index(resources.len())).collect::<Decoded<Vec<_>>>()?;
                res.iter().for_each(|&i| r_used[i] = true);
                let params = r.bytes()?;
                let memory_limit = r.num()?;
                let old = op == APPLY_OLD;
                let task_limits = if old { Vec::new() } else { r.nums()? };
                let num_inputs = usize::try_from(r.num()?).unwrap_or(usize::MAX);
                let output_sizes = r.nums()?;
                if output_sizes.is_empty() {
                    return invalid("an application without outputs");
                }
                Insn::Apply(Apply {
                    module,
                    resources: res,
                    params,
                    memory_limit,
                    task_limits,
                    old,
                    num_inputs,
                    output_sizes,
                })
            }
            CONCAT => Insn::Concat(r.index(depth + 1)?),
            PACK => Insn::Pack(r.index(depth + 1)?),
            SPLIT => {
                let sizes = r.nums()?;
                if sizes.is_empty() {
                    return invalid("a split into no items");
                }
                Insn::Split(sizes)
            }
            PATCH => Insn::Patch(r.num()?),
            CHECKSUM => {
                let v = d.get(r.at..r.at + 8).ok_or_else(|| Failure::Invalid("truncated checksum".into()))?;
                r.at += 8;
                Insn::Checksum(u64::from_le_bytes(v.try_into().unwrap()))
            }
            _ => return invalid(format!("reserved opcode {op:#04x}")),
        };
        match &insn {
            Insn::Nop | Insn::Swap(_) => {}
            Insn::Literal(_) | Insn::Slice { .. } => depth += 1,
            Insn::Apply(a) => {
                pop(&mut depth, a.num_inputs)?;
                depth += a.output_sizes.len();
            }
            &Insn::Concat(n) | &Insn::Pack(n) => {
                pop(&mut depth, n)?;
                depth += 1;
            }
            Insn::Split(sizes) => {
                pop(&mut depth, 1)?;
                depth += sizes.len();
            }
            Insn::Patch(_) => {
                pop(&mut depth, 2)?;
                depth += 1;
            }
            Insn::Checksum(_) if depth == 0 => return invalid("a checksum of an empty stack"),
            Insn::Checksum(_) => {}
        }
        insns.push(insn);
    }
    if [b_used, r_used, m_used].iter().any(|u| u.contains(&false)) {
        return invalid("a block listed but not used");
    }
    if depth as u64 != outputs {
        return invalid(format!("{depth} outputs, not {outputs}"));
    }
    blocks.iter().for_each(|&o| used[o] = true);
    Ok(Program { insns, blocks, resources, modules })
}

/// A ModuleBytes or ResourceBytes instruction pushing `data`, the data of a block as stored,
/// which its operand says is of `size` bytes.
fn stored_bytes(data: &[u8], size: u64) -> Decoded<Insn<'_>> {
    if data.len() as u64 != size {
        return invalid(format!("a block of {} bytes, not {size}", data.len()));
    }
    Ok(Insn::Literal(data))
}

/// The non-leaf blocks as formula blocks are evaluated (several at once), each with the slices
/// of it yet to be evaluated or on a stack: once there are none, it is dropped.
struct Contents<'c, 't> {
    /// Each told of its block decoded ahead ([`Contents::ahead`]).
    data: Vec<Waited<Slot<'c>>>,
    dictionaries: &'c [dict::Dictionary],
    refs: Vec<AtomicUsize>,
    /// Told of the blocks as they are decoded.
    tally: &'t Tally<'t>,
    /// Whether the pages of a block are let go of once it is read ([`Archive::let_go_read`]).
    let_go: bool,
}

/// A value under a lock with a condition variable of its own: one condition variable waited on
/// with several mutexes is an error to POSIX (and panics in std on macOS, which checks it).
struct Waited<T> {
    value: Mutex<T>,
    changed: std::sync::Condvar,
}

impl<T> Waited<T> {
    fn new(v: T) -> Self {
        Waited { value: Mutex::new(v), changed: std::sync::Condvar::new() }
    }

    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, T>> {
        self.value.lock()
    }

    fn wait<'g>(&self, g: std::sync::MutexGuard<'g, T>) -> std::sync::LockResult<std::sync::MutexGuard<'g, T>> {
        self.changed.wait(g)
    }

    fn notify_all(&self) {
        self.changed.notify_all();
    }
}

/// How far the leaves' evaluation is, told as it goes where it is to be ([`Archive::on_progress`]).
struct Tally<'t> {
    tell: Option<&'t (dyn Fn(Step) + Send + Sync)>,
    decompressed: AtomicU64,
    evaluated: AtomicU64,
    /// Of all the data blocks and of all the outputs.
    totals: (u64, u64),
}

impl Tally<'_> {
    /// So many more bytes of the data blocks decompressed and of the outputs evaluated, told.
    fn add(&self, decompressed: u64, evaluated: u64) {
        let Some(tell) = self.tell else { return };
        let d = self.decompressed.fetch_add(decompressed, Ordering::Relaxed) + decompressed;
        let e = self.evaluated.fetch_add(evaluated, Ordering::Relaxed) + evaluated;
        tell(Step::Reached(Reached { decompressed: (d, self.totals.0), evaluated: (e, self.totals.1) }));
    }

    fn step(&self, s: Step) {
        if let Some(tell) = self.tell {
            tell(s);
        }
    }
}

/// A non-leaf block: its data as it is in the archive (and whether it is Zstandard data) until
/// first sliced (or being decoded ahead), then decoded until no slice of it is left.
enum Slot<'c> {
    Raw(&'c [u8], bool, Option<usize>),
    Ahead,
    Decoded(Arc<Vec<u8>>),
    Gone,
}

impl Contents<'_, '_> {
    /// The data of block `o`, while any slice of it is to be evaluated: decoded the first time.
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        let mut s = self.data[o].lock().unwrap();
        while let Slot::Ahead = *s {
            s = self.data[o].wait(s).unwrap();
        }
        if let Slot::Raw(d, zstd, dict) = *s {
            let decoded = decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d]));
            self.tally.add(d.len() as u64, 0);
            *s = Slot::Decoded(Arc::new(decoded?));
            self.read(d);
        }
        match &*s {
            Slot::Decoded(d) => Ok(d.clone()),
            _ => unreachable!("a block with slices left"),
        }
    }

    /// Decodes block `o` ahead of its first slice, where it is not yet (and does not fail: one
    /// that does is left to be decoded, and fail, as it is sliced).
    fn ahead(&self, o: usize) {
        let mut s = self.data[o].lock().unwrap();
        let Slot::Raw(d, zstd, dict) = *s else { return };
        *s = Slot::Ahead;
        drop(s);
        let decoded = decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d]));
        let mut s = self.data[o].lock().unwrap();
        *s = match decoded {
            Ok(b) => {
                self.tally.add(d.len() as u64, 0);
                self.read(d);
                Slot::Decoded(Arc::new(b))
            }
            Err(_) => Slot::Raw(d, zstd, dict),
        };
        drop(s);
        self.data[o].notify_all();
    }

    /// Block `d` of the archive read (decoded), not to be read again: its pages let go of where
    /// they are to be ([`Archive::let_go_read`]).
    fn read(&self, d: &[u8]) {
        if self.let_go {
            // SAFETY: of the archive, a file mapped read only (as `let_go_read` was told)
            unsafe { aqr_common::pages::let_go(d.as_ptr(), d.len()) };
        }
    }

    fn release(&self, o: usize) {
        if self.refs[o].fetch_sub(1, Ordering::Relaxed) == 1 {
            *self.data[o].lock().unwrap() = Slot::Gone;
        }
    }
}

/// What takes the bytes of a file as it is decoded ([`Archive::write_file`]).
pub type Sink<'a> = dyn FnMut(&[u8]) -> Decoded<()> + Send + 'a;

/// What takes the bytes of an output as it is evaluated ([`eval::run`]): a [`Sink`], not needing to
/// be sent.
type Taker<'a> = dyn FnMut(&[u8]) -> Decoded<()> + 'a;

/// Whether Wasm module `w` is of a release (SHA-512/256 in [`crate::released::RELEASED`]), the
/// only modules that the reserved Apply instruction ([`APPLY_OLD`]) is valid for.
fn released(w: &[u8]) -> bool {
    use sha2::Digest;
    let sum = sha2::Sha512_256::digest(w);
    let hex: String = sum.iter().map(|b| format!("{b:02x}")).collect();
    crate::released::RELEASED.contains(&hex.as_str()) || cfg!(test) && tests_released(&hex)
}

/// Whether a Wasm module of SHA-512/256 `hash` is of a release ([`crate::released::RELEASED`]):
/// the modules the reserved Apply instruction is valid for, which a host runs by the rules
/// before the current ones (an `aqr_spawn` of two parameters, no task limits).
pub fn is_released(hash: &[u8; 32]) -> bool {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    crate::released::RELEASED.contains(&hex.as_str())
}

#[cfg(test)]
static TEST_RELEASED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn tests_released(hex: &str) -> bool {
    TEST_RELEASED.lock().unwrap().iter().any(|h| h == hex)
}

#[cfg(not(test))]
fn tests_released(_: &str) -> bool {
    false
}

/// Fails (a resource failure) where the minimal bound of application `a` of module block
/// `module` is over the limit of the process ([`crate::set_application_limit`], 512 MiB by
/// default), before its module is applied. Not for the earlier Apply, whose limit was an estimate.
fn within_limit(a: &Apply, module: usize) -> Decoded<()> {
    if a.old {
        return Ok(());
    }
    let (needed, limit) = (crate::minimal_bound(a.memory_limit, &a.task_limits), crate::application_limit());
    if needed > limit {
        let mib = needed.div_ceil(1 << 20);
        return Err(Failure::Resource(format!(
            "module block {module}: an application needs at least {needed} bytes of memory ({mib} MiB) \
             but is limited to {limit}; allow it with --application-bound={mib}M"
        )));
    }
    Ok(())
}

/// The outputs of application `a` of module block `module`.
fn apply(env: &Env, module: usize, a: &Apply, inputs: &[&[u8]], resources: &[Resource]) -> Decoded<Vec<Vec<u8>>> {
    let loaded = env.loaded[module].get_or_init(|| env.runner.load(&env.modules[module]));
    let loaded = loaded.as_ref().map_err(Failure::clone)?;
    let classes = task_classes(&env.modules[module]);
    if a.old {
        // (the memory limit was an estimate of all of the application's memories: not enforced)
        if !released(&env.modules[module]) {
            return invalid(format!("module block {module}: the reserved Apply instruction of a module not released"));
        }
        let outputs = loaded.apply(a.params, resources, inputs, &a.output_sizes, crate::NO_LIMIT, &[])?;
        return checked_outputs(outputs, a, module);
    }
    if a.task_limits.len() != classes {
        return invalid(format!(
            "module block {module}: {} task limits for {classes} classes of tasks",
            a.task_limits.len()
        ));
    }
    within_limit(a, module)?;
    let outputs = loaded.apply(a.params, resources, inputs, &a.output_sizes, a.memory_limit, &a.task_limits)?;
    checked_outputs(outputs, a, module)
}

/// `outputs` of application `a` of module block `module`, if they are of the sizes declared.
fn checked_outputs(outputs: Vec<Vec<u8>>, a: &Apply, module: usize) -> Decoded<Vec<Vec<u8>>> {
    if outputs.len() != a.output_sizes.len() || outputs.iter().zip(&a.output_sizes).any(|(o, &n)| o.len() as u64 != n) {
        return invalid(format!("module block {module}: outputs of other sizes than declared"));
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An archive of a single file, `quine.aqr`, that is the archive itself
    /// (`scripts/make-quine.py`): its formula takes the stored bytes of a resource block twice
    /// (ResourceBytes), splits one of them and swaps the pieces around the other.
    #[test]
    fn quine_holds_itself() {
        let c = include_bytes!("../testdata/quine.aqr");
        let Value::Tree(tree, contents) = decode_value(c, &NoModules).unwrap() else {
            panic!("a tree expected");
        };
        assert_eq!(tree.entries.len(), 1);
        assert_eq!(tree.entries[0].name, b"quine.aqr");
        let EntryNode::File(i) = tree.entries[0].node else {
            panic!("a file expected");
        };
        assert_eq!(contents[i], c);
    }

    #[test]
    fn compressed_len_counts() {
        let mut rng = aqr_common::rng::Rng::new(3);
        let random: Vec<u8> = (0..100_000).map(|_| rng.below(256) as u8).collect();
        let text: Vec<u8> = (0..300_000u32).flat_map(|i| format!("{} {} ", i % 977, i / 13).into_bytes()).collect();
        for d in [&b""[..], b"abc", &random, &text] {
            for (level, threads) in [(1, 1), (3, 4), (9, 2)] {
                let c = compressed(&mut CCtx::create(), &[d], level, threads).unwrap();
                let n = compressed_len(&mut CCtx::create(), &[d], level, threads).unwrap();
                assert_eq!(n, c.as_ref().map(Vec::len), "{} bytes, level {level}", d.len());
                assert_eq!(archived_raw_len(d, n), archived_raw(d, c.as_deref()));
            }
        }
    }
    use crate::Stream;

    thread_local! {
        /// The streams compressed ([`compressed_to`]), counted.
        pub(super) static COMPRESSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(super) const REV: Module = Module::new("rev", 1);

    pub(super) const CUT: Module = Module::new("cut", 1);

    /// `rev` reverses the concatenation of its inputs, with parameters 1, 2, 3; `cut` cuts it
    /// into outputs of the sizes declared.
    struct RevCut;

    impl crate::Modules for RevCut {
        fn apply(&self, a: &crate::Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            assert_eq!((a.module, &a.params[..], a.resources.len()), (REV, &[1, 2, 3][..], 0));
            Ok(inputs.concat().into_iter().rev().collect())
        }

        fn apply_shared(&self, s: &crate::Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
            if s.apply.module != CUT {
                return Err(Error::new("not cut"));
            }
            let (all, mut at) = (inputs.concat(), 0);
            let mut next = |n: u64| {
                let a = at.min(all.len());
                at += n as usize;
                all[a..at.min(all.len())].to_vec()
            };
            Ok(s.sizes.iter().map(|&n| next(n)).collect())
        }
    }

    pub(super) fn natives() -> Natives<'static, impl crate::Modules + Sync> {
        super::natives(&[REV, CUT], &RevCut)
    }

    /// Patches in archives: of a slice, and of an output of an application of several outputs
    /// against another; one damaged, or of another length, is invalid.
    #[test]
    fn patches() {
        let noise = aqr_common::rng::noise(10_000);
        let mut target = noise[..6000].to_vec();
        target[100..200].fill(1);
        let mut f = Formula::default();
        let a = f.push("a", noise.clone());
        let d = f.push_as("d", Kind::Compressed, crate::patch::make(&noise[..6000], &target, 3).unwrap());
        let cut = crate::Apply {
            module: CUT,
            resources: vec![],
            params: vec![],
            inputs: vec![a],
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut, vec![6000, 4000])).unwrap();
        f.root = Node::concat([o1, Node::patch(o0, d, 6000)]);
        let want = [&noise[6000..], &target[..]].concat();
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert!(decode(&c, &natives()).unwrap() == want);
        let mut bad = f.clone();
        bad.streams[1].data[3] ^= 0x40;
        assert!(matches!(decode(&encode(&bad, None, Level::Zstd(3)).unwrap(), &natives()), Err(Failure::Invalid(_))));
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        let Node::Patch(p) = &mut ns[1] else { unreachable!() };
        p.len -= 1;
        assert!(matches!(decode(&encode(&f, None, Level::Zstd(3)).unwrap(), &natives()), Err(Failure::Invalid(_))));
    }

    /// Applications of several outputs: their outputs taken anywhere, by the root or by a later
    /// one, each brought up the stack where it is taken. The archive gives what the formula
    /// does; one whose outputs are not taken once each is not made.
    #[test]
    fn shared_outputs() {
        let mut f = Formula::default();
        let a = f.push("a", b"abcdefgh".to_vec());
        let cut = |inputs| crate::Apply {
            module: CUT,
            resources: vec![],
            params: vec![],
            inputs,
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
        let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1, Node::slice(0, 0, 2)]), vec![1, 6])).unwrap();
        f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![1, 2, 3], vec![p0], 1)]);
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), b"efghabhabcd");
        // the outputs of the second taken in the other order
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.swap(0, 3);
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), b"dhabcefghab");
        let mut twice = f.clone();
        twice.root = Node::concat([twice.root.clone(), Node::Output { shared: 0, index: 0, len: 3 }]);
        assert!(encode(&twice, None, Level::Zstd(3)).is_err());
    }

    /// Streams merged by class without moving their bytes ([`crate::Formula::merged`]) archive
    /// as those merged ([`crate::Formula::merge_classes`]), byte for byte, the merged stream
    /// over several blocks too; and the encoder keeps none of the blocks it wrote.
    #[test]
    fn merged_in_pieces() {
        let mut f = Formula::default();
        let noise: Vec<u8> = (0..3 * BLOCK as u32 / 2).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        let a = f.push("a", noise[..BLOCK].to_vec());
        let b = f.push("b", b"elsewhere".to_vec());
        let a2 = f.push("a", noise[BLOCK..].to_vec());
        let a3 = f.push("a", b"third".to_vec());
        let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(2, 10, 100)], 100);
        f.root = Node::concat([a, Node::slice(2, 5, BLOCK as u64 / 2 - 5), b, rev, a3, a2]);
        let want = f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
        let sum = Some(checksum(&want));
        let mut whole = f.clone();
        whole.merge_classes();
        let enc = Encoder::new(Level::Zstd(1));
        let one = enc.encode(&whole, sum).unwrap();
        let (merged, groups) = f.merged();
        let whole = |i: usize| (i, 0, f.streams[i].data.len() as u64);
        assert_eq!(groups, [vec![whole(0), whole(2), whole(3)], vec![whole(1)]]);
        let pieces: Vec<Vec<&[u8]>> = groups
            .iter()
            .map(|g| g.iter().map(|&(i, a, b)| &f.streams[i].data[a as usize..b as usize]).collect())
            .collect();
        let enc = Encoder::new(Level::Zstd(1));
        assert!(enc.encode_pieces(&merged, &pieces, sum).unwrap() == one);
        assert!(enc.streams.borrow().is_empty());
        assert!(decode(&one, &natives()).unwrap() == want);
    }

    /// Formulas sealed into a sink one after another, then embedded in one of a stream of its
    /// own, archive and give their bytes back: the second's slice of the same bytes as one of
    /// the first's, and its slice starting with them, written once (but a slice of part of one
    /// sealed is not found); a stream of [`Kind::Compressed`] a stream of the sink of its own.
    #[test]
    fn sealed() {
        let same: Vec<u8> = (0..64u8).collect();
        let mut first = Formula::default();
        let a = first.push("a", same.clone());
        let b = first.push("b", b"of the first".to_vec());
        let c = first.push_as("c", Kind::Compressed, b"compressed".to_vec());
        first.root = Node::concat([a, b, c]);
        let mut second = Formula::default();
        second.push("a", [&b"own "[..], &same, &same, b" and more"].concat());
        second.root = Node::concat([Node::slice(0, 4, 64), Node::slice(0, 0, 4), Node::slice(0, 68, 73)]);
        let id = |_: &crate::Apply, i: &[&[u8]]| Ok(i.concat());
        let want = [first.eval(&id).unwrap(), second.eval(&id).unwrap(), b"top".to_vec()].concat();
        let enc = Encoder::new(Level::Zstd(1));
        let sink = sink::Sink::new(&enc, 16);
        sink.seal(&mut first).unwrap();
        sink.seal(&mut second).unwrap();
        assert!(first.streams.iter().chain(&second.streams).all(|s| s.data.sunk_in().is_some()));
        let mut top = Formula::default();
        let own = top.push("own", b"top".to_vec());
        let (x, y) = (top.embed(first), top.embed(second));
        top.root = Node::concat([x, y, own]);
        let sunk = sink.close().unwrap();
        // "a": the 64 bytes once, "own " (under the least shared), " and more" after them
        let lens: Vec<(&str, u64)> = sunk.iter().map(|s| (&s.class[..], s.len)).collect();
        assert_eq!(lens, [("a", 64 + 4 + 9), ("b", 12), ("c", 10)]);
        let (mut merged, groups) = top.merged();
        merged.streams.extend(sunk.iter().map(|s| crate::Stream {
            class: s.class.clone(),
            kind: s.kind,
            content: false,
            apart: false,
            data: crate::Bytes::sunk(0, s.len as usize),
        }));
        let pieces: Vec<Vec<&[u8]>> = groups
            .iter()
            .map(|g| g.iter().map(|&(i, a, b)| &top.streams[i].data[a as usize..b as usize]).collect())
            .collect();
        let mut out = Vec::new();
        enc.encode_sunk_to(&merged, &pieces, &sunk, Some(checksum(&want)), &mut out).unwrap().unwrap();
        assert!(decode(&out, &natives()).unwrap() == want);
    }

    /// Streams written to a sink, a piece at a time (over a block's end), archive as those held
    /// whole, byte for byte: all of them, or those after the first (at a level picking levels by
    /// samples too).
    #[test]
    fn sunk_as_held() {
        let mut f = Formula::default();
        let noise: Vec<u8> = (0..3 * BLOCK as u32 / 2).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        let text: Vec<u8> = b"some text, some more text. ".iter().copied().cycle().take(BLOCK + 1000).collect();
        let a = f.push("a", noise.clone());
        let b = f.push_as("b", Kind::Text, text.clone());
        let c = f.push("c", b"short".to_vec());
        f.root = Node::concat([b, Node::slice(0, BLOCK as u64 - 7, 20), c, a]);
        let want = f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat())).unwrap();
        let sum = Some(checksum(&want));
        for level in [Level::Zstd(1), Level::MID] {
            let enc = Encoder::new(level);
            let held = enc.encode(&f, sum).unwrap();
            for own in [0, 1] {
                let sink = sink::Sink::new(&enc, 1024);
                for st in &f.streams[own..] {
                    let s = sink.stream(st.class.clone(), st.kind);
                    let d = &st.data[..];
                    let (x, y) = (d.len() / 3, 2 * d.len() / 3);
                    sink.append(s, &[&d[..x], &d[x..y]]).unwrap();
                    sink.append(s, &[&d[y..]]).unwrap();
                }
                let sunk = sink.close().unwrap();
                let pieces: Vec<Vec<&[u8]>> = f.streams[..own].iter().map(|s| vec![&s.data[..]]).collect();
                let mut out = Vec::new();
                enc.encode_sunk_to(&f, &pieces, &sunk, sum, &mut out).unwrap().unwrap();
                assert!(out == held, "{level:?}, {own} held");
            }
            assert!(decode(&held, &natives()).unwrap() == want);
        }
    }

    /// A stream of more than [`BLOCK`] bytes is several non-leaf blocks: slices within one,
    /// over a boundary and of all of it come back, and the archive has a block for each.
    /// The blocks `f` slices listed, of its streams in order.
    fn ordinals_of(f: &Formula) -> Vec<Vec<Option<u64>>> {
        let mut nodes: Vec<&Node> = f.shared.iter().flat_map(|s| &s.apply.inputs).collect();
        nodes.push(&f.root);
        listed(&sliced(f.streams.len(), |s| f.streams[s].data.len(), nodes), 0..f.streams.len()).0
    }

    /// A formula slicing some blocks of a stream alone: only those are written and listed.
    #[test]
    fn some_blocks_of_a_stream() {
        let mut f = Formula::default();
        let n = 3 * BLOCK + 1000;
        let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        f.push("a", d.clone());
        let b = BLOCK as u64;
        let parts = [(5, 10), (2 * b + 10, 990), (3 * b, 7)];
        f.root = Node::concat(parts.iter().map(|&(s, l)| Node::slice(0, s, l)));
        let want: Vec<u8> = parts.iter().flat_map(|&(s, l)| d[s as usize..][..l as usize].to_vec()).collect();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        assert!(decode(&c, &natives()).unwrap() == want);
        assert_eq!(open(&c, &natives()).unwrap().parsed().unwrap().non_leaf.len(), 3);
    }

    #[test]
    fn streams_of_several_blocks() {
        let mut f = Formula::default();
        let n = 2 * BLOCK + 1000;
        let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        f.push("a", d.clone());
        let b = BLOCK as u64;
        let parts = [(5, 10), (b - 3, 7), (2 * b + 10, 990), (0, n as u64), (b, b)];
        f.root = Node::concat(parts.iter().map(|&(s, l)| Node::slice(0, s, l)));
        let want: Vec<u8> = parts.iter().flat_map(|&(s, l)| d[s as usize..][..l as usize].to_vec()).collect();
        assert!(
            description_bound(&f.root) >= {
                let ordinals = ordinals_of(&f);
                let mut out = Vec::new();
                Emit { ordinals: &ordinals, modules: &[], classes: &[], resources: &[], out: &mut out, stack: vec![] }
                    .node(&f.root);
                out.len()
            }
        );
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        assert!(decode(&c, &natives()).unwrap() == want);
        assert_eq!(open(&c, &natives()).unwrap().parsed().unwrap().non_leaf.len(), 3);
    }

    /// A file whose formula is a concatenation (of slices, applications and a concatenation in
    /// it) is written out part by part, as decoding gives it; its checksum is checked as it is
    /// written, and a wrong one fails.
    #[test]
    fn written_part_by_part() {
        let mut f = Formula::default();
        let d: Vec<u8> = (0..3 * BLOCK as u32 + 77).map(|i| (i % 253) as u8).collect();
        f.push("a", d.clone());
        let rev = |start, len| Node::apply(REV, vec![1, 2, 3], vec![Node::slice(0, start, len)], len);
        let inner = Node::concat([Node::slice(0, 5, 100), rev(BLOCK as u64 - 10, 20), Node::slice(0, 0, 3)]);
        f.root = Node::concat([Node::slice(0, 2 * BLOCK as u64, BLOCK as u64 + 77), inner, rev(7, 1000)]);
        let want = f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        let runner = natives();
        let a = open(&c, &runner).unwrap();
        let (mut out, mut parts) = (Vec::new(), 0);
        let n = a
            .write_file(2, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                parts += 1;
                Ok(())
            })
            .unwrap();
        assert!(out == want && n == want.len() as u64);
        assert!(parts >= 5, "{parts} parts");
        // a checksum of something else
        let c = encode(&f, Some(checksum(b"other")), Level::Zstd(1)).unwrap();
        let a = open(&c, &runner).unwrap();
        assert_eq!(a.write_file(2, &mut |_: &[u8]| Ok(())), invalid("output checksum mismatch"));
    }

    /// A file of many applications concatenated (some of applications, of concatenations, of
    /// splits) is written out alike whether its parts are evaluated ahead on workers or not.
    #[test]
    fn parts_made_ahead() {
        let mut f = Formula::default();
        let d: Vec<u8> = (0..2 * BLOCK as u32 + 99).map(|i| (i * 7 % 251) as u8).collect();
        let a = f.push("a", d.clone());
        let rev = |inputs: Vec<Node>, len| Node::apply(REV, vec![1, 2, 3], inputs, len);
        let split = crate::Apply {
            module: REV,
            resources: vec![],
            params: vec![1, 2, 3],
            inputs: vec![a],
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1] = <[Node; 2]>::try_from(f.split(split, vec![40, d.len() as u64 - 40])).unwrap();
        let mut parts = vec![rev(vec![o1], d.len() as u64 - 40)];
        for k in 0..40u64 {
            let at = k * 3001 % (d.len() as u64 - 2000);
            parts.push(match k % 4 {
                0 => rev(vec![Node::slice(0, at, 1000)], 1000),
                1 => rev(vec![rev(vec![Node::slice(0, at, 500)], 500), Node::slice(0, at + 7, 30)], 530),
                2 => rev(vec![Node::concat([Node::slice(0, at, 10), Node::slice(0, at + 99, 20)])], 30),
                _ => Node::slice(0, at, 77),
            });
        }
        parts.push(o0);
        f.root = Node::concat(parts);
        let want = f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        let runner = natives();
        let a = open(&c, &runner).unwrap();
        for threads in [1, 4, 16] {
            let mut out = Vec::new();
            let n = a
                .write_file(threads, &mut |p: &[u8]| {
                    out.extend_from_slice(p);
                    Ok(())
                })
                .unwrap();
            assert!(out == want && n == want.len() as u64, "{threads} threads");
        }
    }

    /// Outputs split from the one output of a module (Split instructions), taken as those of
    /// several outputs are.
    #[test]
    fn split_outputs() {
        let mut f = Formula::default();
        let a = f.push("a", b"abcdefgh".to_vec());
        let rev = crate::Apply {
            module: REV,
            resources: vec![],
            params: vec![1, 2, 3],
            inputs: vec![a],
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1, o2] = <[Node; 3]>::try_from(f.split(rev, vec![3, 0, 5])).unwrap();
        f.root = Node::concat([o2, Node::slice(0, 0, 1), o1, o0]);
        assert!(f.to_string().starts_with("shared #0 rev@1 split 3 0 5"));
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), b"edcbaahgf");
    }

    /// The outputs of a node of several (a split, an application of several outputs) pulled
    /// out of their turn, through a packed byte sequence too, are held until they are (beyond
    /// what memory holds of them, in a file): decoded and written out alike.
    #[test]
    fn pulled_out_of_turn() {
        let mut f = Formula::default();
        let d: Vec<u8> = (0..BLOCK as u32 + 5000).map(|i| (i * 13 % 247) as u8).collect();
        let n = d.len() as u64;
        let a = f.push("a", d.clone());
        let rev = |inputs| crate::Apply {
            module: REV,
            resources: vec![],
            params: vec![1, 2, 3],
            inputs,
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let [o0, o1, o2] = <[Node; 3]>::try_from(f.split(rev(vec![a]), vec![100, n - 300, 200])).unwrap();
        let cut = crate::Apply { module: CUT, params: vec![], ..rev(vec![Node::slice(0, 7, 3000)]) };
        let [s0, s1] = <[Node; 2]>::try_from(f.share(cut, vec![1000, 2000])).unwrap();
        f.root = Node::concat([o2, Node::pack([s1, o0, Node::slice(0, 1, 9)]), o1, s0]);
        let r: Vec<u8> = d.iter().rev().copied().collect();
        let (n, s) = (n as usize, &d[7..3007]);
        let packed = packed::pack(&[&s[1000..], &r[..100], &d[1..10]]);
        let want = [&r[n - 200..], &packed, &r[100..n - 200], &s[..1000]].concat();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        let runner = natives();
        let files = spool::FILES.load(Ordering::Relaxed);
        assert!(decode(&c, &runner).unwrap() == want);
        // (more held than in memory, in a file)
        assert!(spool::FILES.load(Ordering::Relaxed) > files);
        let a = open(&c, &runner).unwrap();
        for threads in [1, 4] {
            let mut out = Vec::new();
            a.write_file(threads, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
            assert!(out == want, "{threads} threads");
        }
    }

    /// Runs `cut@1` as a part module, its inputs pulled and its outputs pushed [`PIECE`] bytes
    /// at a time (failing midway where its parameters are `[0xff]`), and `rev@1` whole.
    struct PartCut;

    const PIECE: u64 = 100_000;

    /// The applications [`PartCut`] made as part modules.
    static PARTS: AtomicUsize = AtomicUsize::new(0);

    /// The memory and task limits of the applications of `rev@1` with a memory limit of 1234.
    static LIMITS: std::sync::Mutex<Vec<Vec<u64>>> = std::sync::Mutex::new(Vec::new());

    struct Stand {
        cut: bool,
    }

    impl Runner for PartCut {
        fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
            match stand_in_name(wasm) {
                Some("cut@1") => Ok(Box::new(Stand { cut: true })),
                Some("rev@1") => Ok(Box::new(Stand { cut: false })),
                _ => resource("an unknown module"),
            }
        }
    }

    impl Loaded for Stand {
        fn apply(
            &self,
            _: &[u8],
            _: &[Resource],
            inputs: &[&[u8]],
            sizes: &[u64],
            memory_limit: u64,
            task_limits: &[u64],
        ) -> Decoded<Vec<Vec<u8>>> {
            assert!(!self.cut, "a part module applied whole");
            if memory_limit == 1234 {
                LIMITS.lock().unwrap().push(task_limits.to_vec());
            }
            assert_eq!(sizes.len(), 1);
            Ok(vec![inputs.concat().into_iter().rev().collect()])
        }

        fn is_part(&self) -> bool {
            self.cut
        }

        fn apply_part(
            &self,
            params: &[u8],
            _: &[Resource],
            inputs: &mut [&mut dyn PartIn],
            outputs: &mut [&mut dyn PartOut],
            sizes: &[u64],
            _: u64,
            _: &[u64],
        ) -> Decoded<()> {
            PARTS.fetch_add(1, Ordering::Relaxed);
            if params == [0xaa] {
                // (met by the others: applied one after another, they would wait in vain)
                RENDEZVOUS.fetch_add(1, Ordering::SeqCst);
                let start = std::time::Instant::now();
                while RENDEZVOUS.load(Ordering::SeqCst) < 2 {
                    if start.elapsed().as_secs() >= 10 {
                        return invalid("not applied at the same time");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            let (mut j, mut left, mut pulled) = (0, sizes[0], 0u64);
            for i in inputs {
                let mut at = 0;
                while at < i.len() {
                    let hi = (at + PIECE).min(i.len());
                    let mut buf = Vec::new();
                    i.pull(hi, &mut |d| buf.extend_from_slice(d))?;
                    (at, pulled) = (hi, pulled + hi - at);
                    if params == [0xff] && pulled > 1 << 20 {
                        return invalid("failed midway");
                    }
                    let mut d = &buf[..];
                    while !d.is_empty() {
                        while left == 0 {
                            j += 1;
                            left = *sizes.get(j).ok_or_else(|| Failure::Invalid("inputs too long".into()))?;
                        }
                        let n = d.len().min(left as usize);
                        outputs[j].push(&d[..n])?;
                        (d, left) = (&d[n..], left - n as u64);
                    }
                }
            }
            Ok(())
        }
    }

    /// The task limits of an application go into the archive as an Apply instruction and come
    /// back to the module as they were; one of none is an Apply instruction.
    #[test]
    fn task_limits() {
        let mut f = Formula::default();
        let d: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        f.push("a", d.clone());
        let rev = |task_limits: Vec<u64>| {
            Node::Apply(Box::new(crate::Apply {
                module: REV,
                resources: vec![],
                params: vec![],
                inputs: vec![Node::slice(0, 0, 1000)],
                len: 1000,
                memory_limit: 1234,
                task_limits,
            }))
        };
        f.root = Node::concat([rev(vec![7, 9]), rev(vec![7, 9])]);
        let r: Vec<u8> = d.iter().rev().copied().collect();
        let want = [&r[..], &r].concat();
        // (a module of two classes of tasks)
        fn two_classes(m: Module) -> Option<Wasm> {
            let mut w = stand_in(m);
            let name = b"aqr_task_slots";
            w.extend_from_slice(&[0, name.len() as u8 + 3, name.len() as u8]);
            w.extend_from_slice(name);
            w.extend_from_slice(&[8, 4]);
            Some(Wasm::Bytes(Cow::Owned(w)))
        }
        let c = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&f, Some(checksum(&want))).unwrap();
        let mut wrong = f.clone();
        if let Node::Concat(ns) = &mut wrong.root {
            let Node::Apply(a) = &mut ns[0] else { unreachable!() };
            a.task_limits = vec![7, 8, 9];
        }
        let bad = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&wrong, Some(checksum(&want))).unwrap();
        assert!(matches!(decode(&bad, &PartCut), Err(Failure::Invalid(m)) if m.contains("task limits")));
        LIMITS.lock().unwrap().clear();
        assert!(decode(&c, &PartCut).unwrap() == want);
        let mut seen = std::mem::take(&mut *LIMITS.lock().unwrap());
        seen.sort();
        assert_eq!(seen, [vec![7, 9], vec![7, 9]]);
    }

    /// An application whose minimal bound (the memory limit and the sum of the task limits) is
    /// over the decoder's limit, 512 MiB by default, fails as a resource failure before its
    /// module is applied; one of exactly that is applied.
    #[test]
    fn minimal_bound_limit() {
        assert_eq!(crate::application_limit(), 512 << 20);
        let d: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        // (a module of two classes of tasks)
        fn two_classes(m: Module) -> Option<Wasm> {
            let mut w = stand_in(m);
            let name = b"aqr_task_slots";
            w.extend_from_slice(&[0, name.len() as u8 + 3, name.len() as u8]);
            w.extend_from_slice(name);
            w.extend_from_slice(&[8, 4]);
            Some(Wasm::Bytes(Cow::Owned(w)))
        }
        let at = |memory_limit: u64, task_limits: Vec<u64>| {
            let mut f = Formula::default();
            f.push("a", d.clone());
            f.root = Node::Apply(Box::new(crate::Apply {
                module: REV,
                resources: vec![],
                params: vec![],
                inputs: vec![Node::slice(0, 0, 1000)],
                len: 1000,
                memory_limit,
                task_limits,
            }));
            let want: Vec<u8> = d.iter().rev().copied().collect();
            let c = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&f, Some(checksum(&want))).unwrap();
            decode(&c, &PartCut).map(|r| r == want)
        };
        let m = 1 << 20;
        assert!(at(512 * m, vec![0, 0]).unwrap());
        assert!(at(256 * m, vec![128 * m, 128 * m]).unwrap());
        assert!(matches!(at(512 * m + 1, vec![0, 0]), Err(Failure::Resource(t)) if t.contains("--application-bound")));
        assert!(matches!(at(m, vec![512 * m, 0]), Err(Failure::Resource(_))));
    }

    /// Part modules applied as their outputs are pulled, each on a thread of its own: one of a
    /// single output in another's input (more than its queue holds, so that it waits for it to
    /// be pulled), whose outputs are pulled out of their turn; decoded and written out alike.
    /// One failing midway fails the archive, the one it pulls of let go of rather than left
    /// waiting.
    #[test]
    fn part_modules() {
        let mut f = Formula::default();
        let n = (24 << 20) + 77u64;
        let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let a = f.push("a", d.clone());
        let cut = |inputs, params| crate::Apply {
            module: CUT,
            resources: vec![],
            params,
            inputs,
            len: 0,
            memory_limit: crate::NO_LIMIT,
            task_limits: Vec::new(),
        };
        let inner = f.share(cut(vec![a.clone()], vec![]), vec![n]).pop().unwrap();
        let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![inner], vec![]), vec![n - 1000, 1000])).unwrap();
        let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(0, 5, 100)], 100);
        f.root = Node::concat([o1, rev, o0]);
        let r: Vec<u8> = d[5..105].iter().rev().copied().collect();
        let want = [&d[n as usize - 1000..], &r, &d[..n as usize - 1000]].concat();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        let (before, files) = (PARTS.load(Ordering::Relaxed), spool::FILES.load(Ordering::Relaxed));
        assert!(decode(&c, &PartCut).unwrap() == want);
        assert_eq!(PARTS.load(Ordering::Relaxed) - before, 2);
        // (more queued than in memory, in a file)
        assert!(spool::FILES.load(Ordering::Relaxed) > files);
        let archive = open(&c, &PartCut).unwrap();
        for threads in [1, 4] {
            let mut out = Vec::new();
            archive
                .write_file(threads, &mut |p: &[u8]| {
                    out.extend_from_slice(p);
                    Ok(())
                })
                .unwrap();
            assert!(out == want, "{threads} threads");
        }
        // failing midway, of the input it pulls
        let mut f = Formula::default();
        let a = f.push("a", d);
        let inner = f.share(cut(vec![a], vec![]), vec![n]).pop().unwrap();
        f.root = f.share(cut(vec![inner], vec![0xff]), vec![n]).pop().unwrap();
        let c = encode(&f, None, Level::Zstd(1)).unwrap();
        assert_eq!(decode(&c, &PartCut), invalid("failed midway"));
    }

    static RENDEZVOUS: AtomicUsize = AtomicUsize::new(0);

    /// The applications of part modules among the parts of an output run at the same time on
    /// the pool (each waits for another to have begun), their outputs written in order.
    #[test]
    fn parts_ahead() {
        let mut f = Formula::default();
        let d: Vec<u8> = (0..3000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        f.push("a", d.clone());
        let cut = |k: u64| Node::apply(CUT, vec![0xaa], vec![Node::slice(0, k * 1000, 1000)], 1000);
        f.root = Node::concat([cut(0), cut(1), cut(2)]);
        let want = d.clone();
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        let archive = open(&c, &PartCut).unwrap();
        let mut out = Vec::new();
        RENDEZVOUS.store(0, Ordering::SeqCst);
        archive
            .write_file(4, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert!(out == want);
    }

    /// Many outputs of applications taken in order with slices between them, in a concatenation
    /// in another (as aqr-wim's of a WIM file in an image): the description grows with the
    /// outputs, not with their square (a Swap for each item above an output took 3,000
    /// outputs to 40 MB), nor do the bytes concatenated (each item so far put together before
    /// each output copied them so), and the outputs come back where they are.
    #[test]
    fn many_outputs() {
        let mut f = Formula::default();
        f.push("a", (0..6000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
        let mut parts = vec![Node::slice(0, 0, 3)];
        let mut inner = Vec::new();
        for k in 0..3 {
            let rev = crate::Apply {
                module: REV,
                resources: vec![],
                params: vec![1, 2, 3],
                inputs: vec![Node::slice(0, k * 2000, 2000)],
                len: 0,
                memory_limit: crate::NO_LIMIT,
                task_limits: Vec::new(),
            };
            for (i, o) in f.split(rev, vec![2; 1000]).into_iter().enumerate() {
                inner.push(Node::slice(0, i as u64, 1));
                inner.push(o);
            }
        }
        parts.push(Node::concat(inner));
        parts.push(Node::slice(0, 5, 2));
        f.root = Node::concat(parts);
        let want: Vec<u8> = {
            let d = &f.streams[0].data;
            let mut w = d[..3].to_vec();
            for k in 0..3 {
                let r: Vec<u8> = d[k * 2000..k * 2000 + 2000].iter().rev().copied().collect();
                for i in 0..1000 {
                    w.push(d[i]);
                    w.extend_from_slice(&r[i * 2..i * 2 + 2]);
                }
            }
            w.extend_from_slice(&d[5..7]);
            w
        };
        let ordinals = ordinals_of(&f);
        let mut out = Vec::new();
        let mut e =
            Emit { ordinals: &ordinals, modules: &[REV], classes: &[], resources: &[], out: &mut out, stack: vec![] };
        for (i, s) in f.shared.iter().enumerate() {
            e.shared(i, s);
        }
        e.node(&f.root);
        assert_eq!(e.stack, [None]);
        assert!(out.len() < 3000 * 64, "{} bytes of description", out.len());
        // the bytes the Concat instructions copy: each of the outputs' a few times
        let (mut stack, mut copied) = (Vec::new(), 0usize);
        let mut r = &out[..];
        while let Some((&op, rest)) = r.split_first() {
            let mut at = 0;
            let num = |at: &mut usize| varint::get(rest, at).unwrap() as usize;
            match op {
                SLICE => {
                    let (_, _, len) = (num(&mut at), num(&mut at), num(&mut at));
                    stack.push(len);
                }
                APPLY => {
                    let (_, nr, np) = (num(&mut at), num(&mut at), num(&mut at));
                    assert_eq!((nr, np), (0, 3));
                    at += np;
                    let (_, nt) = (num(&mut at), num(&mut at));
                    assert_eq!(nt, 0);
                    let (ni, no) = (num(&mut at), num(&mut at));
                    let sizes: Vec<usize> = (0..no).map(|_| num(&mut at)).collect();
                    stack.truncate(stack.len() - ni);
                    stack.extend(sizes);
                }
                SPLIT => {
                    let n = num(&mut at);
                    let sizes: Vec<usize> = (0..n).map(|_| num(&mut at)).collect();
                    stack.pop();
                    stack.extend(sizes);
                }
                SWAP => {
                    let d = num(&mut at);
                    let top = stack.len() - 1;
                    stack.swap(top, top - d);
                }
                CONCAT => {
                    let n = num(&mut at);
                    let len: usize = stack.drain(stack.len() - n..).sum();
                    copied += len;
                    stack.push(len);
                }
                _ => panic!("{op}"),
            }
            r = &rest[at..];
        }
        assert_eq!(stack, [want.len()]);
        assert!(copied < want.len() * 8, "{copied} bytes copied of {}", want.len());
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert!(decode(&c, &natives()).unwrap() == want);
    }

    fn sample() -> Formula {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        f.root =
            Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![1, 2, 3], vec![a, b], 8), Node::slice(1, 1, 2)]);
        f
    }

    const SAMPLE: &[u8] = b"hezyxollehyz";

    /// Nodes of the bytes of a module block and of a resource block as stored push those of the
    /// blocks the archive has: the module's own (which the nodes' must be) and the resource's as
    /// given.
    #[test]
    fn block_bytes() {
        let module: std::sync::Arc<[u8]> =
            always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap().into();
        let r = Resource::stored(always_compressed(&mut CCtx::create(), &[&b"resource".repeat(40)], 3, 1).unwrap())
            .unwrap();
        assert_eq!(&r[..], b"resource".repeat(40));
        let mut f = sample();
        f.root = Node::concat([f.root, Node::ModuleBytes(REV, module.clone()), Node::ResourceBytes(r.clone())]);
        let want = [SAMPLE, &module, r.stored_data().unwrap()].concat();
        let rev = |_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect());
        assert_eq!(f.eval(&rev).unwrap(), want);
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [MODULE + 1, RESOURCE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), want);
        // the module's block of other bytes (another module's)
        let mut g = f.clone();
        let other = always_compressed(&mut CCtx::create(), &[&stand_in(CUT)], 3, 1).unwrap();
        let Node::Concat(ns) = &mut g.root else { unreachable!() };
        ns[3] = Node::ModuleBytes(REV, other.into());
        assert!(encode(&g, None, Level::Zstd(3)).is_err());
        // a module taken only so, and a resource not stored
        g.root = Node::concat([Node::slice(0, 0, 2), Node::ModuleBytes(CUT, module.clone())]);
        assert!(encode(&g, None, Level::Zstd(3)).is_err_and(|e| e.to_string().contains("not its own")));
        g.root = Node::concat([Node::slice(0, 0, 2), Node::ResourceBytes(Resource::new(b"x".to_vec()))]);
        assert!(encode(&g, None, Level::Zstd(3)).is_err_and(|e| e.to_string().contains("not stored")));
        assert!(Resource::stored(b"not zstd".to_vec()).is_err());
    }

    /// Level 0 compresses nothing: streams in uncompressed blocks, formula and module blocks of
    /// raw Zstandard blocks, of any size.
    #[test]
    fn stored_level() {
        let c = encode(&sample(), None, Level::STORED).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        let ts = types(&c);
        assert!(ts.contains(&NON_LEAF_RAW) && !ts.iter().any(|&t| [LEAF_ZSTD, NON_LEAF_ZSTD].contains(&(t & !3))));
        let noise = aqr_common::rng::noise(300_000);
        for n in [0, 1, 1000, 1025, 128 << 10, (128 << 10) + 1, 300_000] {
            let e = always_compressed(&mut CCtx::create(), &[&noise[..n]], STORED, 1).unwrap();
            assert_eq!(decoded(&e, true, true).unwrap(), &noise[..n], "{n}");
            assert!(e.len() <= n + 3 * n.div_ceil(128 << 10).max(1) + 16, "{n}");
        }
    }

    /// The archive of the blocks `add` appends.
    fn archive(add: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut blocks = Vec::new();
        add(&mut blocks);
        let mut footer = Vec::new();
        varint::put(&mut footer, blocks.len() as u64);
        varint::put(&mut footer, 0);
        let n = (footer.len() as u16).to_le_bytes();
        [&HEADER_MAGIC[..], &blocks, &FOOTER_MAGIC, &n, &footer, &n].concat()
    }

    /// An archive of a formula block of `data`, after a `rev` module block and
    /// before non-leaf blocks of `streams`.
    fn formula(data: &[u8], streams: &[&[u8]]) -> Vec<u8> {
        archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[data], 3, 1).unwrap());
            streams.iter().for_each(|s| {
                content_block(out, false, &[s], compressed(&mut CCtx::create(), &[s], 3, 1).unwrap().as_deref())
            });
        })
    }

    /// Block types of archive `c`.
    pub(super) fn types(c: &[u8]) -> Vec<u64> {
        let footer = 6 + 2 + u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize + 2;
        let d = &c[HEADER_MAGIC.len()..c.len() - footer];
        let (mut at, mut out) = (0, Vec::new());
        while at < d.len() {
            let start = at;
            let ty = varint::get(d, &mut at).unwrap();
            let w = 1 << (ty % 4);
            at += w + u64::from_le_bytes(std::array::from_fn(|i| if i < w { d[at + i] } else { 0 })) as usize;
            at += (at - start) % 2;
            out.push(ty);
        }
        out
    }

    #[test]
    fn roundtrip() {
        let f = sample();
        assert_eq!(
            f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap(),
            SAMPLE
        );
        for sum in [None, Some(checksum(SAMPLE))] {
            let c = encode(&f, sum, Level::Zstd(3)).unwrap();
            assert_eq!(c[..6], HEADER_MAGIC);
            assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
            assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        }
        // Streams without slices have no blocks.
        let mut f = sample();
        f.streams.insert(
            0,
            Stream {
                class: "unused".into(),
                kind: Kind::Unknown,
                content: false,
                apart: false,
                data: vec![1; 100].into(),
            },
        );
        f.root.slices_mut(&mut |s, _, _| *s += 1);
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        // A module named twice has one block.
        let mut f = sample();
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.push(ns[1].clone());
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), [SAMPLE, b"zyxolleh"].concat());
    }

    /// Each resource has a block, before the formula block, however many applications take it.
    #[test]
    fn resources() {
        const PRE: Module = Module::new("pre", 1);
        // `pre` puts its resources before its inputs.
        let pre = |a: &crate::Apply, i: &[&[u8]]| {
            assert_eq!(a.module, PRE);
            Ok([a.resources.iter().map(|r| &r[..]).collect::<Vec<_>>().concat(), i.concat()].concat())
        };
        let (r1, r2) = (Resource::new(vec![b'a'; 1000]), Resource::new(b"0123456789".repeat(10)));
        let mut f = Formula::default();
        let x = f.push("x", b"xyz".to_vec());
        f.root = Node::concat([
            Node::apply_with(PRE, vec![r1.clone(), r2.clone()], vec![], vec![x.clone()], 1103),
            Node::apply_with(PRE, vec![r2.clone()], vec![], vec![x.clone()], 103),
            Node::apply_with(PRE, vec![r1.clone()], vec![], vec![x.clone()], 1003),
        ]);
        let expect = [&r1[..], &r2, b"xyz", &r2, b"xyz", &r1, b"xyz"].concat();
        assert_eq!(f.eval(&pre).unwrap(), expect);
        let c = encode(&f, None, Level::MID).unwrap();
        assert_eq!(types(&c), [MODULE + 1, RESOURCE, RESOURCE, FORMULA, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), expect);
        // A resource block is always compressed, even when that makes it longer.
        let noise = Resource::new(aqr_common::rng::noise(1000));
        f.root = Node::apply_with(PRE, vec![noise.clone()], vec![], vec![x], 1003);
        let c = encode(&f, None, Level::MID).unwrap();
        assert_eq!(types(&c), [MODULE + 1, RESOURCE + 1, FORMULA, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), [&noise[..], b"xyz"].concat());
        // sized without the resources held, but archived with them
        let enc = Encoder::new(Level::MID);
        let n = enc.len(&f).unwrap();
        enc.hold_resource(&r1);
        assert_eq!(enc.len(&f).unwrap(), n);
        enc.hold_resource(&noise);
        assert_eq!(enc.len(&f).unwrap(), n - enc.resources_len(&f).unwrap());
        assert_eq!(enc.encode(&f, None).unwrap(), c);
    }

    /// A formula is sized as it is archived, its streams of a class merged, not each compressed
    /// apart (the entries of a ZIP archive alone were sized so far beyond what they took that
    /// its formula was turned down).
    #[test]
    fn sized_merged() {
        let mut f = Formula::default();
        let parts: Vec<Node> = (0..300)
            .map(|i| f.push("x", format!("entry {i}: the same few words once more\n").repeat(20).into_bytes()))
            .collect();
        f.root = Node::concat(parts);
        let mut m = f.clone();
        m.merge_classes();
        assert_eq!(m.streams.len(), 1);
        let enc = Encoder::new(Level::MID);
        assert_eq!(enc.len(&f).unwrap(), enc.len(&m).unwrap());
    }

    /// Zstandard blocks longer than their data declare by how much (`0` then the excess), and as
    /// long as their data by `0 0`.
    #[test]
    fn longer_than_data() {
        let d = aqr_common::rng::noise(50);
        // a raw zstd block, the last: 3 bytes more
        let header = ((d.len() << 3) | 1) as u32;
        let external = |size: &[u8]| [size, &[0], &header.to_le_bytes()[..3], &d, &checksum(&d).to_le_bytes()].concat();
        assert_eq!(decoded(&external(&[0, 3]), true, true).unwrap(), d);
        assert_eq!(decoded(&external(&[0, 2]), true, true), invalid("zstd data of another size than declared"));
        assert_eq!(decoded(&external(&[0, 54]), true, true), invalid("bad size"));
        // as encoded
        let c = always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap();
        assert_eq!(c[..2], [0, 3]);
        assert_eq!(decoded(&c, true, true).unwrap(), d);
        assert_eq!(compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap(), None);
        // and an empty one
        let c = always_compressed(&mut CCtx::create(), &[&[]], 3, 1).unwrap();
        assert_eq!(decoded(&c, true, true).unwrap(), b"");
    }

    /// A formula of a single slice is a leaf block, compressed if that is smaller.
    #[test]
    fn leaf() {
        for (d, ty) in [(vec![], LEAF_RAW), (b"hello".to_vec(), LEAF_RAW), (vec![7; 100_000], LEAF_ZSTD)] {
            let c = encode(&Formula::raw("a", d.clone()), None, Level::Zstd(3)).unwrap();
            assert_eq!(types(&c), [ty]);
            assert_eq!(decode(&c, &NoModules).unwrap(), d);
        }
        let mut f = Formula::raw("a", b"0123456789".to_vec());
        f.root = Node::slice(0, 3, 4);
        assert_eq!(decode(&encode(&f, None, Level::Zstd(3)).unwrap(), &NoModules).unwrap(), b"3456");
        // The leaf block's checksum is the output's: one given must be it.
        assert!(encode(&f, Some(checksum(b"3456")), Level::Zstd(3)).is_ok());
        assert!(encode(&f, Some(checksum(b"0123")), Level::Zstd(3)).is_err());
        // An empty formula is a formula block.
        let c = encode(&Formula::default(), None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [FORMULA]);
        assert_eq!(decode(&c, &NoModules).unwrap(), b"");
    }

    /// A leaf data block may go without a checksum; a non-leaf one may not. A reserved
    /// compression method is unsupported.
    #[test]
    fn data_block_types() {
        let d = vec![7; 1000];
        let c = compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap().unwrap();
        let unchecked = &c[..c.len() - 8];
        assert_eq!(decode(&archive(|out| block(out, LEAF_RAW & !4, &d)), &NoModules).unwrap(), d);
        assert_eq!(decode(&archive(|out| block(out, LEAF_ZSTD & !4, unchecked)), &NoModules).unwrap(), d);
        for ty in [0x38, 0x3c, 0x4c] {
            assert!(
                matches!(decode(&archive(|out| block(out, ty, &c)), &NoModules), Err(Failure::Unsupported(_))),
                "{ty:#x}"
            );
        }
    }

    /// Neither the padding nor the space of a spacer block is read. A block type is the shortest
    /// LEB128; an undefined one is ignored if non-critical (from A to F in its leading digit, padded to
    /// 2 hexadecimal digits), else unsupported, even with an invalid block before it.
    #[test]
    fn block_framing() {
        let decode = |blocks: &[u8]| decode(&archive(|out| out.extend_from_slice(blocks)), &NoModules);
        assert_eq!(decode(b"\x20\x05hello\x00"), Ok(b"hello".to_vec()));
        assert_eq!(decode(b"\x20\x05hello\xaa"), Ok(b"hello".to_vec()));
        assert_eq!(decode(b"\x00\x02\x01\x02\x20\x05hello\x00"), Ok(b"hello".to_vec()));
        assert!(matches!(decode(b"\xa0\x00\x05hello"), Err(Failure::Invalid(_))));
        // non-critical: 0xa0, 0xf0, 0xf00
        assert_eq!(decode(b"\x20\x05hello\x00\xa0\x01\x01\x00"), Ok(b"hello".to_vec()));
        assert_eq!(decode(b"\xf0\x01\x00\x00"), Ok(Vec::new()));
        assert_eq!(decode(b"\x80\x1e\x00\x00"), Ok(Vec::new()));
        // critical: 0x78, 0x80, and 0x0c and 0x0f, whose leading digit is 0 when padded to 2 digits
        assert!(matches!(decode(b"\x78\x00"), Err(Failure::Unsupported(_))));
        assert!(matches!(decode(b"\x80\x01\x00\x00"), Err(Failure::Unsupported(_))));
        assert!(matches!(decode(b"\x0c\x00"), Err(Failure::Unsupported(_))));
        assert!(matches!(decode(&[&[0x0f][..], &[0; 8], &[0]].concat()), Err(Failure::Unsupported(_))));
        // a block-like structure (the header) as a block
        assert!(matches!(decode(b"\xf8\x04\x00\x00"), Err(Failure::Invalid(_))));
        let bad_sum = [&b"\x24\x0dhello"[..], &[0; 8], b"\x00"].concat();
        assert!(matches!(decode(&bad_sum), Err(Failure::Invalid(_))));
        assert!(matches!(decode(&[&bad_sum[..], b"\x78\x00"].concat()), Err(Failure::Unsupported(_))));
    }

    /// The types of block-like structures, as the spec gives them, begin their magics and are
    /// invalid as blocks (not unsupported).
    #[test]
    fn block_like_types() {
        let enc = |ty: u64| {
            let mut v = Vec::new();
            varint::put(&mut v, ty);
            v
        };
        assert!(HEADER_MAGIC.starts_with(&enc(0x278)));
        assert!([0x69, 0x0d, 0x15, 0xaf, 0xf6, 0xb2].starts_with(&enc(0x69)));
        assert!(FOOTER_MAGIC.starts_with(&enc(0x90_5bcb_957d)));
        assert!([0xfc, 0x0f, 0x6e, 0x7b, 0x4b, 0xac].starts_with(&enc(0x7fc)));
        assert!(envelope::ENCRYPTED_HEADER_MAGIC.starts_with(&enc(0x155_04dc_a579)));
        for ty in [0x278, 0x69, 0x90_5bcb_957d, 0x7fc, 0x155_04dc_a579] {
            let mut b = enc(ty);
            b.resize(b.len() + (1 << (ty % 4)), 0);
            b.resize(b.len().next_multiple_of(2), 0);
            let r = decode(&archive(|out| out.extend_from_slice(&b)), &NoModules);
            assert!(matches!(r, Err(Failure::Invalid(_))), "{ty:#x}: {r:?}");
        }
    }

    /// A stream of [`Kind::Compressed`] is stored as it is where the probe level compresses
    /// all of it by less than 1/32, at any level, leaf or not; else it is compressed at the
    /// probe level, and apart from the same bytes of another kind.
    #[test]
    fn compressed_kind_light() {
        let noise = aqr_common::rng::noise(100_000);
        let same = vec![7; 100_000];
        let types = |c: &[u8]| types(c).into_iter().map(|t| t & !3).collect::<Vec<_>>();
        for level in [Level::MID, Level::Zstd(3)] {
            for (d, ty, non_leaf) in [(&noise, LEAF_RAW, NON_LEAF_RAW), (&same, LEAF_ZSTD, NON_LEAF_ZSTD)] {
                let mut f = Formula::default();
                f.root = f.push_as("a", Kind::Compressed, d.clone());
                let c = encode(&f, None, level).unwrap();
                assert_eq!(types(&c), [ty], "{level:?}");
                assert_eq!(&decode(&c, &NoModules).unwrap(), d);
                let b = f.push("b", d.clone());
                f.root = Node::concat([f.root.clone(), b]);
                let c = encode(&f, None, level).unwrap();
                assert_eq!(types(&c), [FORMULA, non_leaf, non_leaf]);
                assert_eq!(decode(&c, &NoModules).unwrap(), d.repeat(2));
            }
            // at the probe level, not the encoder's
            let mut f = Formula::default();
            f.root = f.push_as("a", Kind::Compressed, same.clone());
            let at_probe = compressed(&mut CCtx::create(), &[&same], PROBE, 1).unwrap().unwrap();
            assert!(encode(&f, None, level).unwrap().windows(at_probe.len()).any(|w| w == at_probe));
        }
        let mut f = Formula::default();
        f.root = f.push_as("a", Kind::Compressed, same.clone());
        assert_eq!(types(&encode(&f, None, Level::STORED).unwrap()), [LEAF_RAW]);
    }

    /// Data that a sample does not compress is compressed all the same where it repeats from
    /// afar, as the probe level over all of it finds (a video of a still picture, its
    /// keyframes the same): the same noise twice, 20 MiB apart, the sample's chunks not over
    /// the same bytes; of any kind.
    #[test]
    fn sample_missed() {
        let noise = aqr_common::rng::noise(20 << 20);
        let d = noise.repeat(2);
        for kind in [Kind::Unknown, Kind::Compressed] {
            let mut f = Formula::default();
            f.root = f.push_as("a", kind, d.clone());
            let c = encode(&f, None, Level::MID).unwrap();
            assert!(c.len() < noise.len() + (1 << 20), "{kind}: {}", c.len());
            assert!(decode(&c, &NoModules).unwrap() == d);
        }
    }

    /// [`Level::MID`] leaves uncompressed what the lowest level hardly shrinks, and compresses
    /// the rest at 3, or higher where that saves enough for its time.
    #[test]
    fn mid_level() {
        let types_at = |d: &[u8], level| {
            let c = encode(&Formula::raw("a", d.to_vec()), None, level).unwrap();
            assert_eq!(decode(&c, &NoModules).unwrap(), d);
            (types(&c)[0] & !3, c.len())
        };
        // shrunk at level 1, by less than 1/32 (but more than zstd keeps blocks raw below)
        let mut noise = aqr_common::rng::noise(2 << 20);
        noise.iter_mut().enumerate().filter(|(i, _)| i % 1024 < 32).for_each(|(_, b)| *b = 0);
        // a short stream, a whole one, and a sample
        for d in [&noise[..30000], &noise[..300_000], &noise] {
            assert_eq!(types_at(d, Level::Zstd(3)).0, LEAF_ZSTD, "{}", d.len());
            assert_eq!(types_at(d, Level::MID).0, LEAF_RAW, "{}", d.len());
        }
        // Text, which level 4 shrinks more than enough for its time.
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let (zstd3, mid) = (types_at(&text, Level::Zstd(3)).1, types_at(&text, Level::MID).1);
        assert!(mid < zstd3 - zstd3 / 32, "{mid} vs {zstd3}");
        // short, at level 3
        let short = Formula::raw("a", text[..30000].to_vec());
        assert_eq!(encode(&short, None, Level::MID).unwrap(), encode(&short, None, Level::Zstd(3)).unwrap());
        // and so are module and formula blocks
        assert_eq!(encode(&sample(), None, Level::MID).unwrap(), encode(&sample(), None, Level::Zstd(3)).unwrap());
    }

    /// Levels by name and number; the zstd levels picked from grow with the level, and only
    /// from 8 on are dense streams (text, PE code) at [`DENSE`] (one no longer than a sample
    /// where that is smaller).
    #[test]
    fn levels() {
        for (s, l) in [
            ("low", Level::LOW),
            ("mid", Level::Of(5)),
            ("high", Level::Of(8)),
            ("1", Level::Of(1)),
            ("0", Level::STORED),
        ] {
            assert_eq!(s.parse(), Ok(l));
        }
        assert_eq!("zstd19".parse(), Ok(Level::Zstd(19)));
        for s in ["10", "-1", "zstd", "max"] {
            assert!(s.parse::<Level>().is_err(), "{s}");
        }
        for n in 2..=Level::MAX {
            let (a, b) = (Pick::of(n - 1), Pick::of(n));
            assert!(b.higher.len() >= a.higher.len() && b.rate <= a.rate && b.top() >= a.top(), "{n}");
        }
        assert_eq!(Pick::of(1).top(), 1);

        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let mut long = Vec::new();
        // (lines shuffled, so that it is not one text repeated)
        let mut rng = aqr_common::rng::Rng::new(1);
        let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
        while long.len() <= super::SAMPLE * 2 {
            long.extend_from_slice(lines[rng.below(lines.len() as u64) as usize]);
        }
        let s = |d: &[u8], kind, class: &'static str| Stream {
            class: class.into(),
            kind,
            content: false,
            apart: false,
            data: d.to_vec().into(),
        };
        for (level, dense) in [(Level::MID, false), (Level::Of(7), false), (Level::HIGH, true), (Level::Of(9), true)] {
            let enc = Encoder::new(level);
            assert_eq!(enc.how(&s(&long, Kind::Text, "raw")) == How::Dense, dense, "{level:?}");
            assert_eq!(enc.how(&s(&long, Kind::Unknown, "x86/OP")) == How::Dense, dense, "{level:?}");
            assert_eq!(enc.how(&s(&long, Kind::Unknown, "raw")), How::Usual);
            assert_eq!(enc.how(&s(&long, Kind::Compressed, "raw")), How::Light);
            let at = |d: &[u8], how| match enc.plan(key(&[d]), &[d], how).unwrap() {
                Plan::Level(l, _) => l,
                Plan::Done(..) => 0,
                Plan::Probe => PROBE,
            };
            assert_eq!(at(&long, How::Dense) == DENSE, dense, "{level:?}");
            // (at 9, picked from as any other)
            assert_eq!(at(&long, How::Usual) == DENSE, level == Level::Of(9), "{level:?}");
            // no longer than a sample: compressed whole, at the dense level where smaller so
            let Plan::Done(Some(c), _) = enc.plan(key(&[&text]), &[&text], How::Dense).unwrap() else {
                panic!("{level:?}: not compressed whole")
            };
            let at_dense = compressed(&mut enc.cctx(), &[&text], DENSE, 1).unwrap().unwrap();
            assert!(at_dense.len() < c.len() || dense, "{level:?}: {} vs {}", at_dense.len(), c.len());
            assert_eq!(c == at_dense, dense, "{level:?}");
        }
        // archived dense, and smaller so
        let f = |kind| {
            let mut f = Formula::raw("a", long.clone());
            f.streams[0].kind = kind;
            f
        };
        let (usual, dense) =
            (encode(&f(Kind::Unknown), None, Level::HIGH).unwrap(), encode(&f(Kind::Text), None, Level::HIGH).unwrap());
        assert_eq!(decode(&dense, &NoModules).unwrap(), long);
        assert!(dense.len() < usual.len(), "{} vs {}", dense.len(), usual.len());
    }

    /// A stream larger than Zstandard's jobs is archived the same on any number of threads.
    #[test]
    fn threads() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let mut d = Vec::new();
        let mut rng = aqr_common::rng::Rng::new(1);
        while d.len() < 40 << 20 {
            let at = rng.below(text.len() as u64 - 4096) as usize;
            d.extend_from_slice(&text[at..at + 4096]);
        }
        let f = Formula::raw("a", d.clone());
        for level in [Level::MID, Level::Zstd(3)] {
            let one = Encoder::new(level).encode(&f, None).unwrap();
            assert_eq!(Encoder::with_threads(level, 4).encode(&f, None).unwrap(), one);
            assert_eq!(decode(&one, &NoModules).unwrap(), d);
        }
    }

    /// A long stream is matched from afar: the same noise twice, 20 MiB apart, takes about
    /// as much as once.
    #[test]
    fn long_distance() {
        let noise = aqr_common::rng::noise(20 << 20);
        let d = noise.repeat(2);
        let c = encode(&Formula::raw("a", d.clone()), None, Level::Zstd(3)).unwrap();
        assert!(c.len() < noise.len() + (1 << 20), "{}", c.len());
        assert!(decode(&c, &NoModules).unwrap() == d);
    }

    /// A compressed block decodes into as much memory as it declares (and a byte to tell a
    /// longer one), not what growing as it comes would take.
    #[test]
    fn decoded_at_size() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let d = text.repeat((3 << 20) / text.len() + 1);
        let c = compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap().unwrap();
        let out = decoded(&c, true, true).unwrap();
        assert_eq!(out, d);
        assert!(out.capacity() <= d.len() + 1, "{} for {}", out.capacity(), d.len());
    }

    /// A context compressed with before compresses as a new one does, whatever the levels,
    /// threads and data it had (more than a job among them).
    #[test]
    fn cctx_reused() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let big = text.repeat(ZSTD_JOB as usize / text.len() + 2);
        let runs: [(&[u8], i32, usize); 7] = [
            (&big, 9, 4),
            (&text[..1000], 1, 1),
            (&text, 19, 1),
            (&big, 3, 1),
            (&text[5000..], 6, 4),
            (&[], 3, 2),
            (&text, 3, 1),
        ];
        let mut c = CCtx::create();
        for (d, level, threads) in runs {
            let fresh = compressed(&mut CCtx::create(), &[d], level, threads).unwrap();
            assert_eq!(compressed(&mut c, &[d], level, threads).unwrap(), fresh, "level {level}, {threads} threads");
        }
    }

    /// Data of any size compresses the same where it is told of ([`Swept`]), given little room
    /// as well as plenty: on the calling thread (up to 512 KiB, where Zstandard does not take
    /// workers) the output depends on the room it has.
    #[test]
    fn swept_alike() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let mut rng = aqr_common::rng::Rng::new(1);
        let mut d = Vec::new();
        // (text and noise, in runs of either)
        while d.len() < 3 << 20 {
            let at = rng.below(text.len() as u64 - 65536) as usize;
            let run = 1 << (8 + rng.below(9));
            match rng.below(2) {
                0 => d.extend_from_slice(&text[at..at + run]),
                _ => d.extend((0..run).map(|_| rng.below(256) as u8)),
            }
        }
        let file = vec![0u8; 1000];
        let _watch = aqr_common::progress::watch(&file, |_| {});
        for n in [50_000, 410_649, 600_000, 3 << 20] {
            for (level, threads) in [(1, 1), (9, 1), (3, 4)] {
                for size in [usize::MAX, n / 8] {
                    let one = compressed_to(&mut CCtx::create(), &[&d[..n]], level, threads, size).unwrap();
                    let swept = Swept::over(aqr_common::progress::Sweep::new(&file, n as u64));
                    let two = compressed_to(&mut CCtx::create(), &[&d[..n]], level, threads, size).unwrap();
                    drop(swept);
                    assert!(one == two, "{n} bytes at level {level}, {threads} threads, room for {size}");
                }
            }
        }
    }

    /// A stream compresses the same where it is told of ([`Swept`]), a window of output at a
    /// time: all of it told, in parts.
    #[test]
    fn swept() {
        let mut rng = aqr_common::rng::Rng::new(1);
        let d: Vec<u8> =
            (0..2 * ZSTD_JOB as usize + 12345).map(|i| if i % 3 == 0 { rng.below(256) as u8 } else { b'a' }).collect();
        let job = ZSTD_JOB as usize;
        for pieces in [vec![&d[..]], vec![&d[..job], &d[job..]], vec![&d[..5], &d[5..job + 1], &d[job + 1..]]] {
            let one = compressed(&mut CCtx::create(), &pieces, 3, 4).unwrap();
            let file = vec![0u8; 1000];
            let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let t = told.clone();
            let _watch = aqr_common::progress::watch(&file, move |r| t.lock().unwrap().push(r));
            let swept = Swept::over(aqr_common::progress::Sweep::new(&file, d.len() as u64));
            assert_eq!(compressed(&mut CCtx::create(), &pieces, 3, 4).unwrap(), one);
            drop(swept);
            let told = told.lock().unwrap();
            assert!(told.len() > 2 && told.windows(2).all(|w| w[0].end == w[1].start), "{told:?}");
            assert_eq!((told[0].start, told.last().unwrap().end), (0, 1000));
        }
    }

    /// A stream in pieces compresses as it does in one, the pieces ending anywhere (at the end
    /// of a job among them), and is sized as it is archived.
    #[test]
    fn pieces() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let mut d = Vec::new();
        let mut rng = aqr_common::rng::Rng::new(1);
        while d.len() < 2 * ZSTD_JOB as usize {
            let at = rng.below(text.len() as u64 - 4096) as usize;
            d.extend_from_slice(&text[at..at + 4096]);
        }
        d.truncate(2 * ZSTD_JOB as usize);
        let job = ZSTD_JOB as usize;
        let one = compressed(&mut CCtx::create(), &[&d], 1, 4).unwrap();
        // as Zstandard compresses it in one go (long data from afar)
        assert!(d.len() > LONG);
        let mut z = zstd::bulk::Compressor::new(1).unwrap();
        for p in [
            CParameter::ContentSizeFlag(false),
            CParameter::ChecksumFlag(false),
            CParameter::DictIdFlag(false),
            CParameter::NbWorkers(4),
            CParameter::JobSize(ZSTD_JOB),
            CParameter::OverlapSizeLog(ZSTD_OVERLAP),
            CParameter::WindowLog(25),
            CParameter::EnableLongDistanceMatching(true),
        ] {
            z.set_parameter(p).unwrap();
        }
        assert!(
            one.as_ref().unwrap().ends_with(&[&z.compress(&d).unwrap()[6..], &checksum(&d).to_le_bytes()].concat())
        );
        for cuts in [vec![job], vec![1, job - 1, job + 1, d.len()], (1..40).map(|i| i * d.len() / 40).collect()] {
            let (mut pieces, mut at) = (Vec::new(), 0);
            for c in cuts {
                pieces.push(&d[at..c]);
                at = c;
            }
            pieces.push(&d[at..]);
            assert_eq!(compressed(&mut CCtx::create(), &pieces, 1, 4).unwrap(), one);
        }

        let d = &d[..3 << 20];
        let mut f = Formula::default();
        f.push("a", b"head".to_vec());
        f.push("b", Vec::new());
        f.root = Node::concat([Node::slice(1, 0, d.len() as u64), Node::slice(0, 0, 4)]);
        let whole = Formula {
            streams: vec![
                f.streams[0].clone(),
                Stream {
                    class: "b".into(),
                    kind: Kind::Unknown,
                    content: false,
                    apart: false,
                    data: d.to_vec().into(),
                },
            ],
            ..f.clone()
        };
        let pieces = [&d[..100], &d[100..2 << 20], &d[2 << 20..]];
        let raw = [b"head".as_slice(), d].concat();
        for level in [Level::MID, Level::Zstd(3)] {
            let enc = Encoder::new(level);
            assert_eq!(enc.len_with(&f, Some((1, &pieces))).unwrap(), Encoder::new(level).len(&whole).unwrap());
            assert_eq!(
                enc.smaller_with(&f, 1, &pieces, &raw).unwrap(),
                Encoder::new(level).smaller(&whole, &raw).unwrap()
            );
            // the stream compressed then is the one archived (its blocks, and the head)
            enc.forget(&raw);
            assert_eq!(enc.encode(&whole, None).unwrap(), Encoder::new(level).encode(&whole, None).unwrap());
            assert_eq!(enc.streams.borrow().len(), 1 + blocks_in(d.len()) as usize);
        }
    }

    /// Raw data that a formula was found smaller than, then forgotten, is not compressed again
    /// to be compared with another formula of it (a scan's around the file), and is compared
    /// as a new encoder would.
    #[test]
    fn smaller_after_forget() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        let mut rng = aqr_common::rng::Rng::new(2);
        // within a sample and beyond one, alike throughout (so that the sample's estimate holds)
        for n in [200 << 10, 3 << 20] {
            let mut d = Vec::new();
            while d.len() < n {
                let at = rng.below(text.len() as u64 - 64) as usize;
                d.extend_from_slice(&text[at..at + 64]);
                d.extend_from_slice(&rng.below(1 << 32).to_le_bytes()[..4]);
            }
            // a formula of most of it, then one a little larger
            let part = d[..d.len() * 9 / 10].to_vec();
            let mut f = Formula::default();
            f.push("a", part.clone());
            f.root = Node::slice(0, 0, part.len() as u64);
            let g = Formula { root: Node::concat([f.root.clone(), Node::slice(0, 0, 0)]), ..f.clone() };
            for level in [Level::MID, Level::Zstd(3)] {
                let enc = Encoder::new(level);
                assert!(enc.smaller(&f, &d).unwrap());
                enc.forget(&d);
                let before = COMPRESSED.with(|c| c.get());
                let smaller = enc.smaller(&g, &d).unwrap();
                assert_eq!(COMPRESSED.with(|c| c.get()), before, "{n} bytes at {level:?}");
                assert_eq!(smaller, Encoder::new(level).smaller(&g, &d).unwrap());
                enc.clear();
                enc.smaller(&g, &d).unwrap();
                assert!(COMPRESSED.with(|c| c.get()) > before, "cleared");
            }
        }
    }

    /// [`Level::MID`] compares a formula with raw data compressed at the base level, however
    /// higher the data would be archived at, and keeps only its length: what is compared is
    /// mostly not archived.
    #[test]
    fn smaller_at_base() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
        // alike throughout, so that the sample's estimate holds
        let mut rng = aqr_common::rng::Rng::new(3);
        let mut d = Vec::new();
        while d.len() < 3 << 20 {
            let at = rng.below(text.len() as u64 - 64) as usize;
            d.extend_from_slice(&text[at..at + 64]);
            d.extend_from_slice(&rng.below(1 << 32).to_le_bytes()[..4]);
        }
        let part = d[..d.len() * 9 / 10].to_vec();
        let mut f = Formula::default();
        f.push("a", part.clone());
        f.root = Node::slice(0, 0, part.len() as u64);
        let enc = Encoder::new(Level::MID);
        let k = key(&[&d]);
        assert!(matches!(enc.plan(k, &[&d], How::Usual).unwrap(), Plan::Level(l, _) if l > BASE.0));
        let raw = Encoder::new(Level::Zstd(BASE.0)).raw_len(&d).unwrap();
        assert_eq!(enc.smaller(&f, &d).unwrap(), enc.len(&f).unwrap() < raw);
        assert!(!enc.streams.borrow().contains_key(&k));
        assert_eq!(enc.raw_lens.borrow().get(&k), Some(&raw));
    }

    /// Above the base by the time to compress (from level 6 on, by the sample alone); below
    /// where clearly smaller, as a sample may be off by a little.
    #[test]
    fn picks() {
        let mid_pick = |n, base, higher: &[usize], lower: &[usize]| pick(&Pick::of(5), n, base, higher, lower);
        // nothing better
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[310_000]), 3);
        // level 4, 1% smaller for time worth 0.02% of the data
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 297_000, 297_000], &[310_000]), 4);
        // level 6, 1% smaller than 4 for 0.41%: not enough
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 294_000], &[310_000]), 4);
        // level 9, 12% smaller than 4 for 0.70%
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 260_000], &[310_000]), 9);
        // level 9, 3% smaller for 0.71% (most of the solid blocks of 10gb save more)
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000, 300_000, 291_000], &[310_000]), 9);
        // level 1, by 0.7% (as of a sample of 19tech_4.psd, 8.7% larger whole) and by 10%
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[298_000]), 3);
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[270_000]), 1);
        // from 6 on, the smallest
        let high = |higher: &[usize], lower: &[usize]| pick(&Pick::of(6), 1_000_000, 300_000, higher, lower);
        assert_eq!(high(&[297_000, 294_000, 294_000], &[310_000]), 6);
        assert_eq!(high(&[300_000, 300_000, 299_900], &[310_000]), 9);
        assert_eq!(high(&[300_000; 3], &[310_000]), 3);
        // low, none higher
        assert_eq!(pick(&Pick::of(2), 1_000_000, 300_000, &[], &[310_000]), 3);
    }

    #[test]
    fn stand_in_names() {
        assert_eq!(stand_in_name(&stand_in(REV)), Some("rev@1"));
        // after another custom section
        let other = [&WASM_HEADER[..], &[0, 3, 0, 9, 9], &stand_in(REV)[WASM_HEADER.len()..]].concat();
        assert_eq!(stand_in_name(&other), Some("rev@1"));
        assert_eq!(stand_in_name(&WASM_HEADER), None);
        assert_eq!(stand_in_name(&stand_in(REV)[..20]), None);
    }

    /// A dictionary or module block of a 1-byte size is a shorthand block: one in full of fewer
    /// bytes is written with a 2-byte size.
    #[test]
    fn shorthand_types() {
        let small = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap();
        assert!(small.len() < 0x100);
        for ty in [MODULE, DICTIONARY_RAW] {
            let mut out = Vec::new();
            block(&mut out, ty, &small);
            assert_eq!(out[0] as u64, ty + 1);
        }
        let shorthand = |ty: u8| archive(|out| out.extend([&[ty, 32][..], &[7; 32]].concat()));
        assert_eq!(decode(&shorthand(0x60), &natives()), resource("registered modules are not supported yet"));
        assert_eq!(decode(&shorthand(0x50), &natives()), resource("registered dictionaries are not supported yet"));
    }

    /// ModuleBytes and ResourceBytes instructions push the data of the blocks as stored, of the
    /// size they tell.
    #[test]
    fn stored_bytes() {
        let module = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap();
        let resource = always_compressed(&mut CCtx::create(), &[&[b'r'; 300]], 3, 1).unwrap();
        let of = |insns: &[u8]| {
            let d = [&[0, 1, 0, 1, 0][..], insns].concat();
            archive(|out| {
                block(out, MODULE, &module);
                block(out, RESOURCE, &resource);
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap());
            })
        };
        let (m, r) = (module.len() as u8, resource.len() as u8);
        let c = of(&[RESOURCE_BYTES, 0, r, MODULE_BYTES, 0, m, CONCAT, 2]);
        assert_eq!(decode(&c, &natives()).unwrap(), [&resource[..], &module].concat());
        let c = of(&[RESOURCE_BYTES, 0, r, MODULE_BYTES, 0, m + 1, CONCAT, 2]);
        assert_eq!(decode(&c, &natives()), invalid(format!("a block of {m} bytes, not {}", m + 1)));
        let c = of(&[RESOURCE_BYTES, 0, r - 1, MODULE_BYTES, 0, m, CONCAT, 2]);
        assert_eq!(decode(&c, &natives()), invalid(format!("a block of {r} bytes, not {}", r - 1)));
        // each listed block used
        assert_eq!(decode(&of(&[MODULE_BYTES, 0, m]), &natives()), invalid("a block listed but not used"));
        assert_eq!(decode(&of(&[RESOURCE_BYTES, 1, r]), &natives()), invalid("index 1 out of range"));
    }

    #[test]
    fn instructions_by_hand() {
        // literals swapped, and a slice of the second non-leaf block
        let d =
            [&[1, 0][..], &[0, 0], &[LITERAL, 2], b"ab", &[LITERAL, 2], b"cd", &[SWAP, 1, SLICE, 0, 1, 2, CONCAT, 3]]
                .concat();
        assert_eq!(
            decode(&formula(&d, &[b"xyz", b"0123"]), &natives()),
            invalid("a non-leaf block no formula block references")
        );
        let d =
            [&[1, 1][..], &[0, 0], &[LITERAL, 2], b"ab", &[LITERAL, 2], b"cd", &[SWAP, 1, SLICE, 0, 1, 2, CONCAT, 3]]
                .concat();
        assert_eq!(
            decode(&formula(&d, &[b"xyz", b"0123"]), &natives()),
            invalid("a non-leaf block no formula block references")
        );
        let d = [
            &[2, 0, 0][..],
            &[0, 0],
            &[LITERAL, 2],
            b"ab",
            &[LITERAL, 2],
            b"cd",
            &[SWAP, 1, SLICE, 1, 1, 2, SLICE, 0, 0, 1, CONCAT, 4],
        ]
        .concat();
        assert_eq!(decode(&formula(&d, &[b"xyz", b"0123"]), &natives()).unwrap(), b"cdab12x");
    }

    #[test]
    fn pack() {
        let mut f = sample();
        let c = f.push("c", b"0123".to_vec());
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        let rev = ns.remove(1);
        let packed = Node::pack([Node::slice(0, 1, 2), Node::slice(2, 0, 0), rev, c]);
        f.root = Node::apply(REV, vec![1, 2, 3], vec![Node::pack([Node::slice(1, 0, 1)]), packed.clone()], 71);
        let want = [&packed::pack(&[b"x"])[..], &packed::pack(&[&b"el"[..], b"", b"zyxolleh", b"0123"])]
            .concat()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();
        assert_eq!((packed.len(), want.len()), (40 + 14, 71));
        assert_eq!(f.eval(&|_: &crate::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap(), want);
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), want);
    }

    /// Split instructions by hand: of a literal and of a slice, and their sizes.
    #[test]
    fn split() {
        let split = |sizes: &[u8]| {
            let d = [
                &[1, 0, 0, 0, LITERAL, 4][..],
                b"abcd",
                &[SPLIT, 2, 1, 3, SWAP, 1, SLICE, 0, 0, 4, SPLIT, sizes.len() as u8],
                sizes,
                &[SWAP, 1, CONCAT, sizes.len() as u8 + 2],
            ]
            .concat();
            decode(&formula(&d, &[b"0123"]), &natives())
        };
        assert_eq!(split(&[2, 0, 2]).unwrap(), b"bcda0123");
        assert_eq!(split(&[3, 1]).unwrap(), b"bcda3012");
        assert_eq!(split(&[3, 0]), invalid("a split into sizes not adding up"));
        assert_eq!(split(&[3, 2]), invalid("a split into sizes not adding up"));
        assert_eq!(split(&[]), invalid("a split into no items"));
    }

    /// A formula block declares how many outputs it has, as many as its instructions leave and
    /// at least one; an archive without a metadata block is its outputs one after another.
    #[test]
    fn outputs() {
        // "abcd" split into "a" and "bcd"
        let split = [0, 0, 0, LITERAL, 4, b'a', b'b', b'c', b'd', SPLIT, 2, 1, 3];
        let of = |n: u64| {
            archive(|out| formula_block(out, n, &always_compressed(&mut CCtx::create(), &[&split], 3, 1).unwrap()))
        };
        assert_eq!(decode(&of(2), &NoModules).unwrap(), b"abcd");
        for n in [0, 1, 3] {
            assert!(matches!(decode(&of(n), &NoModules), Err(Failure::Invalid(_))), "{n} outputs");
        }
    }

    /// A non-leaf block that formula blocks share is there for each of them, as long as any is
    /// to slice it.
    #[test]
    fn shared_blocks() {
        let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
        varint::put(&mut apply, MEMORY_LIMIT);
        apply.extend_from_slice(&[0, 2, 1, 4]);
        let first =
            [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
        let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
            for d in [&first[..], &second] {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        });
        assert_eq!(decode(&c, &natives()).unwrap(), b"zyyxzxyz");
    }

    /// The evaluation tells how far it is: nothing at first, then each block decoded and each
    /// leaf evaluated, up to all the data blocks' bytes and all the outputs'.
    #[test]
    fn progress() {
        let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
        varint::put(&mut apply, MEMORY_LIMIT);
        apply.extend_from_slice(&[0, 2, 1, 4]);
        let first =
            [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
        let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
            for _ in 0..3 {
                for d in [&first[..], &second] {
                    formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
                }
                content_block(out, false, &[b"xyz"], None);
            }
            content_block(out, true, &[b"uv"], None);
        });
        let natives = natives();
        let mut a = open(&c, &natives).unwrap();
        let told = Arc::new(Mutex::new(Vec::new()));
        let t = told.clone();
        a.on_progress(Some(Arc::new(move |r| t.lock().unwrap().push(r))));
        assert_eq!(a.value(2).unwrap(), Value::File([&b"zyyxzxyz".repeat(3)[..], b"uv"].concat()));
        let told = told.lock().unwrap();
        // (the blocks' data with its checksum, and the outputs)
        let totals = (3 * (3 + 8) + 2 + 8, 3 * 8 + 2);
        // each leaf started and ended once, and told of between
        let (mut started, mut ended) = (vec![0; 7], vec![0; 7]);
        for s in told.iter() {
            match *s {
                Step::Started(i) => started[i] += 1,
                Step::Ended(i) => {
                    assert_eq!(started[i], 1, "{i} ended before it started");
                    ended[i] += 1;
                }
                _ => {}
            }
        }
        assert_eq!((started, ended), (vec![1; 7], vec![1; 7]));
        let told: Vec<Reached> =
            told.iter().filter_map(|s| if let Step::Reached(r) = s { Some(*r) } else { None }).collect();
        // (at first, each non-leaf block, each formula leaf, the stored leaf decoded and evaluated)
        assert_eq!(told.len(), 1 + 3 + 6 + 2);
        assert_eq!(told[0], Reached { decompressed: (0, totals.0), evaluated: (0, totals.1) });
        let most = told.iter().fold((0, 0), |m, r| (m.0.max(r.decompressed.0), m.1.max(r.evaluated.0)));
        assert_eq!(most, totals);
        assert!(told.iter().all(|r| (r.decompressed.1, r.evaluated.1) == totals));
    }

    /// Leaves evaluated within a budget, even of one at a time, give what they give without.
    #[test]
    fn budget() {
        let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
        varint::put(&mut apply, MEMORY_LIMIT);
        apply.extend_from_slice(&[0, 2, 1, 4]);
        let first =
            [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
        let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
            for _ in 0..20 {
                for d in [&first[..], &second] {
                    formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
                }
                content_block(out, false, &[b"xyz"], None);
            }
        });
        let natives = natives();
        let each = |budget: Option<u64>| {
            let budget = budget.map(Budget::new);
            let mut a = open(&c, &natives).unwrap();
            if let Some(b) = &budget {
                a.set_budget(b);
            }
            let got = Mutex::new(Vec::new());
            a.leaves(4, |i, o| {
                got.lock().unwrap().push((i, o));
                Ok(())
            })
            .unwrap();
            let mut got = got.into_inner().unwrap();
            got.sort();
            got
        };
        let all = each(None);
        assert_eq!(all.len(), 40);
        for b in [1, 6, 1000] {
            assert_eq!(each(Some(b)), all, "{b}");
        }
    }

    /// With the budget held elsewhere (another archive's leaves, say), the leaves go one at a
    /// time, but they go.
    #[test]
    fn budget_held() {
        let leaf = [1, 0, 0, 0, SLICE, 0, 0, 3];
        let c = archive(|out| {
            for _ in 0..20 {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&leaf], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        });
        let natives = natives();
        let budget = Budget::new(1000);
        let held = budget.take(1000);
        let mut a = open(&c, &natives).unwrap();
        a.set_budget(&budget);
        let (on, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (o, m) = (on.clone(), most.clone());
        a.on_progress(Some(Arc::new(move |s| match s {
            Step::Started(_) => _ = m.fetch_max(o.fetch_add(1, Relaxed) + 1, Relaxed),
            Step::Ended(_) => _ = o.fetch_sub(1, Relaxed),
            _ => {}
        })));
        let n = AtomicUsize::new(0);
        a.leaves(4, |_, o| {
            assert_eq!(o, vec![b"xyz".to_vec()]);
            n.fetch_add(1, Relaxed);
            Ok(())
        })
        .unwrap();
        assert_eq!((n.into_inner(), most.load(Relaxed)), (20, 1));
        drop(held);
        assert_eq!(budget.held(), 0);
    }

    /// `rev`, whose application of parameters 9 waits for the last leaf to start.
    struct WaitsForLast(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl crate::Modules for WaitsForLast {
        fn apply(&self, a: &crate::Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
            if a.params == [9] {
                let (started, changed) = &*self.0;
                let s = started.lock().unwrap();
                let (s, _) = changed.wait_timeout_while(s, std::time::Duration::from_secs(5), |s| !*s).unwrap();
                if !*s {
                    return Err(Error::new("the last leaf not let in"));
                }
            }
            Ok(inputs.concat().into_iter().rev().collect())
        }
    }

    /// Within a budget, the leaves after one waiting for room that fit go before it, and the
    /// memory limit of a module of tasks is weighed as the instance's: in each archive, the first leaf waits for the last to start.
    #[test]
    fn budget_passed() {
        // `rev` of `params`, declaring `limit`; and a leaf of no application
        let leaf = |params: u8, limit: u64, classes: u8| {
            let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2, APPLY, 0, 0, 1, params];
            varint::put(&mut d, limit);
            d.push(classes);
            (0..classes).for_each(|_| varint::put(&mut d, MEMORY_LIMIT));
            d.extend_from_slice(&[2, 1, 4, SLICE, 0, 2, 1, CONCAT, 2]);
            d
        };
        let plain = vec![1, 0, 0, 0, SLICE, 0, 0, 3];
        let run = |module: &[u8], leaves: &[Vec<u8>]| {
            let c = archive(|out| {
                block(out, MODULE, &always_compressed(&mut CCtx::create(), &[module], 3, 1).unwrap());
                for d in leaves {
                    formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
                }
                content_block(out, false, &[b"xyz"], None);
            });
            let started = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
            let waits = WaitsForLast(started.clone());
            let natives = super::natives(&[REV], &waits);
            let budget = Budget::new(2000);
            let mut a = open(&c, &natives).unwrap();
            a.set_budget(&budget);
            let last = leaves.len() - 1;
            a.on_progress(Some(Arc::new(move |s| {
                if matches!(s, Step::Started(i) if i == last) {
                    *started.0.lock().unwrap() = true;
                    started.1.notify_all();
                }
            })));
            a.leaves(4, |_, _| Ok(()))
        };
        // the second and the third wait for the first to end, and more leaves than PASSING go
        // before them
        let mut leaves = vec![leaf(9, 1500, 0), leaf(1, 1800, 0), leaf(1, 1800, 0)];
        leaves.extend(std::iter::repeat_n(plain.clone(), 2 * PASSING));
        assert_eq!(run(&stand_in(REV), &leaves), Ok(()));
        // a module of a class of 8 task slots: its instance of 1500 bytes (the tasks' have limits of
        // their own)
        let mut slotted = stand_in(REV);
        let name = b"aqr_task_slots";
        slotted.extend_from_slice(&[0, name.len() as u8 + 2, name.len() as u8]);
        slotted.extend_from_slice(name);
        slotted.push(8);
        assert_eq!(run(&slotted, &[leaf(9, 1500, 1), plain]), Ok(()));
    }

    #[test]
    fn decode_errors() {
        let c = encode(&sample(), Some(checksum(SAMPLE)), Level::Zstd(3)).unwrap();
        // A changed byte is an invalid archive, or the same file (e.g. with a larger zstd window).
        let (mut same, mut failed) = (0, 0);
        for i in 0..c.len() {
            let mut bad = c.clone();
            bad[i] ^= 0x41;
            match decode(&bad, &natives()) {
                Ok(d) if d == SAMPLE => same += 1,
                Err(Failure::Invalid(_)) => failed += 1,
                r => panic!("byte {i}: {r:?}"),
            }
        }
        assert!(same < 8 && failed + same == c.len(), "{same} the same");
        for n in 1..c.len() {
            assert!(matches!(decode(&c[..n], &natives()), Err(Failure::Invalid(_))), "{n} bytes");
        }
        assert_eq!(decode(&c, &NoModules), resource("no module runs here"));
        assert_eq!(decode(&c, &super::natives(&[CUT], &RevCut)), resource("no native implementation of rev@1"));
        // the output checksum
        let bad = encode(&sample(), Some(checksum(SAMPLE) ^ 1), Level::Zstd(3)).unwrap();
        assert_eq!(decode(&bad, &natives()), invalid("output checksum mismatch"));
    }

    /// Applications by hand: the numbers of inputs and the declared sizes.
    #[test]
    fn applications() {
        let apply = |inputs: u8, sizes: &[u8]| {
            let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY, 0, 0, 3, 1, 2, 3];
            varint::put(&mut d, MEMORY_LIMIT);
            d.extend_from_slice(&[0, inputs, sizes.len() as u8]);
            d.extend_from_slice(sizes);
            decode(&formula(&d, &[b"abc"]), &natives())
        };
        assert_eq!(apply(2, &[5]).unwrap(), b"cbcba");
        assert_eq!(apply(3, &[5]), invalid("stack underflow"));
        assert_eq!(apply(2, &[]), invalid("an application without outputs"));
        assert_eq!(apply(2, &[4]), invalid("module block 0: outputs of other sizes than declared"));
    }

    /// The earlier Apply instruction (opcode 11, no task limits, a memory limit of all the
    /// application's memories) is valid only for the modules of releases ([`released`]), and
    /// their memory limit is not enforced.
    #[test]
    fn earlier_apply_of_released_modules() {
        let old = |module: &[u8], limit: u64| {
            let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY_OLD, 0, 0, 3, 1, 2, 3];
            varint::put(&mut d, limit);
            d.extend_from_slice(&[2, 1, 5]);
            let c = archive(|out| {
                block(out, MODULE, &always_compressed(&mut CCtx::create(), &[module], 3, 1).unwrap());
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap());
                content_block(
                    out,
                    false,
                    &[b"abc"],
                    compressed(&mut CCtx::create(), &[b"abc"], 3, 1).unwrap().as_deref(),
                );
            });
            decode(&c, &natives())
        };
        // a module of the stand-in's kind, told apart by a section of its own
        let mut released_module = stand_in(REV);
        released_module.extend_from_slice(&[0, 3, 1, b'r', 0]);
        let other = stand_in(REV);
        assert_eq!(old(&other, 1), invalid("module block 0: the reserved Apply instruction of a module not released"));
        let sum = sha2::Digest::finalize(sha2::Digest::chain_update(sha2::Sha512_256::default(), &released_module));
        TEST_RELEASED.lock().unwrap().push(sum.iter().map(|b| format!("{b:02x}")).collect());
        // (the limit of 1 byte is not enforced)
        assert_eq!(old(&released_module, 1).unwrap(), b"cbcba");
        assert_eq!(old(&other, 1), invalid("module block 0: the reserved Apply instruction of a module not released"));
        // the real list: hexadecimal hashes, sorted, none repeated
        let list = crate::released::RELEASED;
        assert!(list.len() > 100);
        assert!(
            list.iter().all(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        );
        assert!(list.windows(2).all(|w| w[0] < w[1]));
    }

    /// An application of a module of classes of tasks takes a task limit for each: the bound
    /// of a formula not yet declared counts them.
    #[test]
    fn bound_counts_task_limits() {
        let mut f = Formula::raw("a", vec![0; 100]);
        for limits in [vec![], vec![1 << 28, 1 << 28]] {
            f.root = Node::Apply(Box::new(crate::Apply {
                module: REV,
                resources: vec![],
                params: vec![],
                inputs: vec![Node::slice(0, 0, 100)],
                len: 100,
                memory_limit: crate::NO_LIMIT,
                task_limits: limits,
            }));
            let ordinals = ordinals_of(&f);
            let mut out = Vec::new();
            Emit { ordinals: &ordinals, modules: &[REV], classes: &[2], resources: &[], out: &mut out, stack: vec![] }
                .node(&f.root);
            let bound = description_bound(&f.root);
            assert!(bound >= out.len(), "{bound} < {}", out.len());
        }
    }

    #[test]
    fn bound() {
        let desc = |f: &Formula| {
            let ordinals = ordinals_of(f);
            let resources: Vec<&Resource> = f.applies().into_iter().flat_map(|a| &a.resources).collect();
            let mut out = Vec::new();
            Emit {
                ordinals: &ordinals,
                modules: &[REV],
                classes: &[],
                resources: &resources,
                out: &mut out,
                stack: vec![],
            }
            .node(&f.root);
            out.len()
        };
        let mut f = sample();
        assert!(description_bound(&f.root) >= desc(&f));
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        let Node::Apply(a) = &mut ns[1] else { unreachable!() };
        a.resources = vec![Resource::new(vec![1]), Resource::new(vec![2])];
        assert!(description_bound(&f.root) >= desc(&f));
        // Of formulas put together: the parts add up.
        let mut g = Formula::raw("big", vec![0; 1 << 20]);
        let parts: Vec<_> = (0..300).map(|_| (description_bound(&sample().root), g.embed(sample()))).collect();
        let sum: usize = parts.iter().map(|p| p.0).sum();
        g.root = Node::Concat(std::iter::once(g.root.clone()).chain(parts.into_iter().map(|p| p.1)).collect());
        let b = description_bound(&g.root);
        assert_eq!(b, sum + description_bound(&Node::slice(0, 0, 1 << 20)) + 3);
        assert!(b >= desc(&g), "{b} < {}", desc(&g));
    }
}
