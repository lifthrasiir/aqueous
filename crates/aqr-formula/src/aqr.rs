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
//! takes); streams of [`Kind::Compressed`] are stored as they are.
//!
//! [`decode`] evaluates such archives, and any other without a metadata block or an
//! encryption.
//!
//! An archive of several files is one segment (`segment`): [`Encoder::encode_segment`] lays
//! out the formulas of the files over streams they may share, with the metadata block of their
//! [`Tree`], and [`decode_value`] decodes any archive of a segment at most (without
//! registered dictionaries, imports or encryption) to its files. The blocks alike of a batch
//! of its leaves may take a dictionary trained for them ([`dict`]).

mod dict;
mod segment;
mod tree;

pub use segment::{Batch, ContentAt, Layout, Leaf, Placed, Segment, SegmentWriter};
pub use tree::{Entry, EntryNode, Extra, MAC_FLAGS_ALLOWED, Time, Tree, WIN_ATTRS_ALLOWED, name_bytes, name_from};

use std::borrow::Cow;
use std::cell::{RefCell, RefMut};
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use aqr_common::varint;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh64::{Xxh64, xxh64};
use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;
use zstd::zstd_safe::{self, CCtx, CParameter, InBuffer, OutBuffer, ResetDirective};

use crate::{Error, Formula, Kind, Module, Node, Resource, Result, packed};

pub const HEADER_MAGIC: [u8; 6] = [0xf8, 0x04, 0x03, 0x6a, 0xd6, 0xbe];
pub const FOOTER_MAGIC: [u8; 6] = [0xfd, 0xaa, 0xae, 0xde, 0x85, 0x12];
const ENCRYPTED_HEADER_MAGIC: [u8; 8] = [0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a, 0x01, 0x00];
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
const SWAP: u8 = 0x0f;
const SLICE: u8 = 0x10;
const APPLY: u8 = 0x11;
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

/// How hard an [`Encoder`] compresses: a level of the archive, which picks the zstd level of
/// each stream (and how hard aqr-compress recompresses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A level from 0 (fastest) to 9 (smallest), [`Level::LOW`], [`Level::MID`] and
    /// [`Level::HIGH`] being 2, 5 and 8. At 0 ([`Level::STORED`]) nothing is compressed: every
    /// block is uncompressed, but for those that are always Zstandard (formula, module and
    /// resource blocks), which hold raw Zstandard blocks. Otherwise each stream is uncompressed
    /// if incompressible (`PROBE`), else at the base zstd level of the level's `Pick`, or:
    ///
    /// - at a higher level of it where that saves enough for its time, at its rate of bytes
    ///   per second of compression;
    /// - at a lower level of it where that is smaller by 1/32 at least (beyond what a sample
    ///   may be off by), as it is faster too;
    /// - at its dense level, where it has one, if of PE code or text (`Encoder::how`): if
    ///   longer than a sample, always; else where that is smaller.
    ///
    /// Module and formula blocks, and streams shorter than `SMALL`, are at the base.
    Of(u8),
    /// Every stream at this zstd level.
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
    /// Stored as it is, of [`Kind::Compressed`].
    Stored,
    Usual,
    /// PE code or text, at the dense level of the [`Pick`] (which has one).
    Dense,
}

/// What an [`Encoder`] makes of a stream: compressed already ([`compressed`]), or to be
/// compressed at a zstd level, to about so many bytes (by a sample).
enum Plan {
    Done(Option<Vec<u8>>, i32),
    Level(i32, usize),
}

/// Encodes formulas as archives at a [`Level`], keeping the compressed streams and resources
/// by their contents: [`Encoder::len`] sizes a formula, and the same streams and resources in a
/// later formula are not compressed again.
pub struct Encoder {
    level: Level,
    /// Threads compressing a stream (the archive does not depend on them).
    threads: usize,
    /// Compressed streams (as [`compressed`] gives) by their [`StreamKey`].
    streams: RefCell<HashMap<StreamKey, Option<Vec<u8>>>>,
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
    decoded(d, true, true).ok()
}

/// A stream's (or resource's) hash and length.
type StreamKey = (u128, usize);

/// What [`compressed`] gives: the compressed stream, or `None` where it does not compress.
type Compressed = Option<Vec<u8>>;

impl Encoder {
    pub fn new(level: Level) -> Encoder {
        Encoder::with_threads(level, 1)
    }

    /// Drops what is kept of stream `d` compressed (by [`Encoder::smaller`], say), where it will
    /// not be archived; only its archived length is kept, for [`Encoder::smaller`].
    pub fn forget(&self, d: &[u8]) {
        let k = key(&[d]);
        self.trials.borrow_mut().retain(|t, _| t.0 != k);
        if self.streams.borrow().contains_key(&k) {
            // from what is kept, compressing nothing
            let n = self.raw_len(d).expect("a stream compressed already");
            self.raw_lens.borrow_mut().insert(k, n);
            self.streams.borrow_mut().remove(&k);
        }
    }

    /// Drops everything kept of the streams and resources compressed.
    pub fn clear(&self) {
        self.streams.borrow_mut().clear();
        self.levels.borrow_mut().clear();
        self.resources.borrow_mut().clear();
        self.raw_lens.borrow_mut().clear();
        self.trials.borrow_mut().clear();
    }

    /// An encoder compressing each stream on `threads` threads, into the same archive as on one.
    pub fn with_threads(level: Level, threads: usize) -> Encoder {
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

    /// The encoder compressing a batch's blocks alike with dictionaries trained for them where
    /// they pay ([`SegmentWriter::batch`]; [`dict`]), as by default, or not.
    pub fn with_dictionaries(mut self, on: bool) -> Encoder {
        self.trains = on;
        self
    }

    /// The data of the module block of `m`, compressed at `level`, once.
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
    fn modules_len(&self, f: &Formula) -> Result<usize> {
        let mut modules: Vec<Module> = Vec::new();
        for a in f.applies() {
            if !modules.contains(&a.module) {
                modules.push(a.module);
            }
        }
        let mut n = Count(0);
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
        let mut resources: Vec<&Resource> = Vec::new();
        for a in f.applies() {
            for r in &a.resources {
                if !resources.contains(&r) && which(r) {
                    resources.push(r);
                }
            }
        }
        let mut n = Count(0);
        resources.into_iter().try_for_each(|r| self.resource_block(&mut n, r))?;
        Ok(n.0)
    }

    /// The Zstandard context of the encoder.
    fn cctx(&self) -> RefMut<'_, CCtx<'static>> {
        self.cctx.borrow_mut()
    }

    /// The length of the archive of `f` without a checksum.
    pub fn len(&self, f: &Formula) -> Result<usize> {
        self.len_with(f, None)
    }

    /// [`Encoder::len`], with the stream of `stand_in` (by index) its bytes rather than its data.
    fn len_with(&self, f: &Formula, stand_in: Option<(usize, &[&[u8]])>) -> Result<usize> {
        let mut n = Count(0);
        self.write(f, stand_in, None, &mut n)?;
        let held = self.held.borrow();
        let modules = if self.shared_modules { self.modules_len(f)? } else { 0 };
        Ok(match self.shared {
            true => n.0 - self.resources_len(f)?,
            false if held.is_empty() => n.0,
            false => n.0 - self.resources_len_of(f, |r| held.contains(&key(&[&r[..]])))?,
        } - modules)
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

    /// How stream `s` is compressed: stored if of [`Kind::Compressed`]; dense where the level
    /// has a dense level ([`Pick`]), if PE code (an x86 or ARM64 filter's streams, or the rest
    /// of a PE file) or text.
    fn how(&self, s: &crate::Stream) -> How {
        let dense = matches!(self.level, Level::Of(n) if Pick::of(n).dense.is_some())
            && (s.kind == Kind::Text
                || s.class == "pe"
                || s.class.starts_with("x86/")
                || s.class.starts_with("arm64/"));
        match s.kind {
            Kind::Compressed => How::Stored,
            _ if dense => How::Dense,
            _ => How::Usual,
        }
    }

    /// [`encode`] at the level of the encoder. Fails on slices out of their stream.
    pub fn encode(&self, f: &Formula, checksum: Option<u64>) -> Result<Vec<u8>> {
        // sized first (the streams compressed then are kept), so as to be written at its size
        let mut out = Vec::with_capacity(self.len(f)? + 16);
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
        let r = self.write_streams(f, &streams, checksum, &mut w);
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
        self.write_streams(f, &streams, checksum, out)
    }

    /// [`Encoder::write`] of `f` whose streams' bytes are `streams` (each in pieces).
    fn write_streams(
        &self,
        f: &Formula,
        streams: &[&[&[u8]]],
        checksum: Option<u64>,
        out: &mut impl Out,
    ) -> Result<()> {
        // of the module and formula blocks
        let level = self.base_level();
        f.check_outputs()?;
        let mut slices = Ok(());
        f.visit(&mut |n| {
            if let &Node::Slice { stream, start, len } = n
                && streams.get(stream).is_none_or(|&s| start.checked_add(len).is_none_or(|e| e > total(s) as u64))
            {
                slices = Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
            }
        });
        slices?;

        out.put(&HEADER_MAGIC);
        let blocks = out.len();
        if let (&Node::Slice { stream, start, len }, []) = (&f.root, &f.shared[..]) {
            let leaf = cut(streams[stream], start as usize, len as usize);
            if checksum.is_some_and(|c| c != checksum_of(&leaf)) {
                return Err(Error::new("output checksum not that of the leaf"));
            }
            self.content_block(out, true, &leaf, self.how(&f.streams[stream]), None)?;
        } else {
            // the non-leaf block of each stream sliced, by its ordinal
            let mut ordinals = vec![None; f.streams.len()];
            f.visit(&mut |n| {
                if let &Node::Slice { stream, len: 1.., .. } = n {
                    ordinals[stream] = Some(0);
                }
            });
            let (mut sliced, mut next) = (Vec::new(), 0);
            for (s, o) in ordinals.iter_mut().enumerate() {
                if o.is_some() {
                    *o = Some(next);
                    next += blocks_in(total(streams[s]));
                    sliced.push(s);
                }
            }
            let (mut modules, mut resources): (Vec<Module>, Vec<&Resource>) = (Vec::new(), Vec::new());
            for a in f.applies() {
                if !modules.contains(&a.module) {
                    modules.push(a.module);
                }
                for r in &a.resources {
                    if !resources.contains(&r) {
                        resources.push(r);
                    }
                }
            }

            let mut data = Vec::new();
            varint::put(&mut data, next);
            (0..next).for_each(|_| varint::put(&mut data, 0));
            for n in [resources.len(), modules.len()] {
                varint::put(&mut data, n as u64);
                (0..n).for_each(|i| varint::put(&mut data, i as u64));
            }
            let mut e =
                Emit { ordinals: &ordinals, modules: &modules, resources: &resources, out: &mut data, stack: vec![] };
            for (i, s) in f.shared.iter().enumerate() {
                e.shared(i, s);
            }
            e.node(&f.root);
            debug_assert_eq!(e.stack, [None]);
            if let Some(c) = checksum {
                data.push(CHECKSUM);
                data.extend_from_slice(&c.to_le_bytes());
            }

            for &m in &modules {
                block(out, MODULE, &self.module_block(m, level)?);
            }
            for r in resources {
                self.resource_block(out, r)?;
            }
            formula_block(out, 1, &always_compressed(&mut self.cctx(), &[&data], level, self.threads)?);
            for s in sliced {
                self.stream_blocks(out, streams[s], self.how(&f.streams[s]), None)?;
            }
        }

        footer(out, out.len() - blocks);
        Ok(())
    }
}

/// The instructions pushing node `n`, its streams' blocks at `ordinals` and its modules and
/// resources at their indices in `modules` and `resources`.
/// Instructions as nodes push their values, with the stack they leave: the outputs of
/// applications of several outputs not taken yet by (application, output), the rest `None`.
struct Emit<'a> {
    ordinals: &'a [Option<u64>],
    modules: &'a [Module],
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
                let first = self.ordinals[stream].unwrap();
                let pieces = slice_blocks(start, len);
                let n = pieces.len();
                for (k, at, n) in pieces {
                    self.out.push(SLICE);
                    varint::put(self.out, first + k);
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
        let out = &mut *self.out;
        out.push(APPLY);
        varint::put(out, self.modules.iter().position(|&m| m == a.module).unwrap() as u64);
        varint::put(out, a.resources.len() as u64);
        for r in &a.resources {
            varint::put(out, self.resources.iter().position(|&s| s == r).unwrap() as u64);
        }
        varint::put(out, a.params.len() as u64);
        out.extend_from_slice(&a.params);
        varint::put(out, a.memory_limit);
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
        Node::Slice { .. } => false,
        Node::Concat(ns) | Node::Pack(ns) => ns.iter().any(takes_output),
        Node::Apply(a) => a.inputs.iter().any(takes_output),
        Node::Patch(p) => takes_output(&p.source) || takes_output(&p.patch),
    }
}

/// Bytes of the instructions pushing node `n` at most, wherever the streams of its formula are
/// among others (up to 2^21 of them, and 2^14 modules and resources each): the bounds of
/// formulas put together ([`Formula::embed`]) add up, with the nodes around them. An output of
/// an application of several outputs ([`Node::Output`]) is counted as one Swap: the Swaps
/// bringing it up to the top of the stack depend on the formula around it.
pub fn description_bound(n: &Node) -> usize {
    match n {
        Node::Output { .. } => 1 + 3,
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
            if s.get(p..p.checked_add(n)?)? == STAND_IN_SECTION.as_bytes() {
                return std::str::from_utf8(&s[p + n..]).ok();
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

/// A block of the bytes of `parts` one after the other.
fn block_of(out: &mut impl Out, ty: u64, parts: &[&[u8]]) {
    let start = out.len();
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let ss = match len as u64 {
        0..0x100 => 0,
        0x100..0x1_0000 => 1,
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
                    match match how {
                        How::Stored => Plan::Done(None, STORED),
                        _ => self.plan(key(data), data, how)?,
                    } {
                        Plan::Done(c, _) => c,
                        Plan::Level(level, size) => compressed_to(&mut self.cctx(), data, level, self.threads, size)?,
                    }
                }
            };
            content_block(out, leaf, data, c.as_deref());
            return Ok(());
        }
        let c = match streams.entry(key_how(data, how)) {
            MapEntry::Occupied(e) => e.into_mut(),
            MapEntry::Vacant(e) => {
                let plan = match how {
                    How::Stored => Plan::Done(None, STORED),
                    _ => self.plan(key(data), data, how)?,
                };
                e.insert(match plan {
                    Plan::Done(c, _) => c,
                    Plan::Level(level, size) => compressed_to(&mut self.cctx(), data, level, self.threads, size)?,
                })
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
    fn resource_block(&self, out: &mut impl Out, r: &[u8]) -> Result<()> {
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
        let c = match self.plan(key(&[r]), &[r], How::Usual)? {
            Plan::Done(c, _) => c,
            Plan::Level(level, size) => compressed_to(&mut self.cctx(), &[r], level, self.threads, size)?,
        };
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
        let saves = c.as_ref().is_some_and(|c| c.len() < sample.len() - sample.len() / 32);
        self.trials.borrow_mut().insert((k, PROBE), c);
        Ok(saves)
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

    /// Compresses `streams` (but those of [`Kind::Compressed`], which are stored as they are)
    /// as [`Encoder::encode`] would, each on a thread of its own (on up to the encoder's
    /// threads, the calling one and workers of [`aqr_common::pool`]), but those of more than
    /// two of Zstandard's jobs, each on up to `LARGE_THREADS` beforehand: for the blocks of
    /// them written next, the same bytes as one after another.
    pub fn precompress(&self, streams: &[&crate::Stream]) -> Result<()> {
        let todo: Vec<(&[u8], How)> = {
            let kept = self.streams.borrow();
            let mut seen = std::collections::HashSet::new();
            streams
                .iter()
                .flat_map(|s| block_ranges(s.data.len()).map(move |r| (&s.data[r], self.how(s))))
                .filter(|&(d, how)| {
                    let k = key_how(&[d], how);
                    how != How::Stored && !kept.contains_key(&k) && seen.insert(k)
                })
                .collect()
        };
        let level = self.level;
        let total = todo.iter().map(|t| t.0.len() as u64).sum();
        let (done, progress) = (std::sync::atomic::AtomicU64::new(0), self.progress.as_deref());
        let one = |(d, how): (&[u8], How), threads: usize| -> Result<(StreamKey, Compressed, i32)> {
            let made = THREAD_CCTX.with(|c| {
                let c = &mut c.borrow_mut();
                let (c, l) =
                    match plan_by(level, how, &[d], &mut |sample, l| compressed(c, &[sample.unwrap_or(d)], l, 1))? {
                        Plan::Done(c, l) => (c, l),
                        Plan::Level(l, size) => (compressed_to(c, &[d], l, threads, size)?, l),
                    };
                Ok((key_how(&[d], how), c, l))
            });
            if let Some(p) = progress {
                p(done.fetch_add(d.len() as u64, std::sync::atomic::Ordering::Relaxed) + d.len() as u64, total);
            }
            made
        };
        let (large, small): (Vec<_>, Vec<_>) = todo.iter().partition(|t| t.0.len() > 2 * ZSTD_JOB as usize);
        let threads = self.threads.min(LARGE_THREADS);
        let mut made: Vec<_> = large.into_iter().map(|&t| one(t, threads)).collect();
        made.extend(aqr_common::pool::par_map(small.len(), self.threads, |i| one(*small[i], 1)));
        let (mut kept, mut levels) = (self.streams.borrow_mut(), self.levels.borrow_mut());
        for m in made {
            let (k, c, l) = m?;
            kept.insert(k, c);
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
        let streams =
            f.streams.iter().enumerate().filter(|(_, s)| self.how(s) != How::Stored).map(|(i, s)| match stand_in {
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
                    if matches!(self.level, Level::Of(_)) && level > self.base_level() {
                        let c = compressed_to(&mut self.cctx(), &[d], self.base_level(), self.threads, estimate)?;
                        let raw = archived_raw(d, c.as_deref());
                        self.raw_lens.borrow_mut().insert(key(&[d]), raw);
                        return Ok(n < raw);
                    }
                    compressed_to(&mut self.cctx(), &[d], level, self.threads, estimate)?
                }
            };
            self.streams.borrow_mut().insert(key(&[d]), c);
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
            let len = |c: &Option<Vec<u8>>| c.as_ref().map_or(sample.len(), Vec::len);
            let probe = trial(&sample, PROBE)?;
            if len(&probe) >= sample.len() - sample.len() / 32 {
                return Ok(Plan::Done(None, STORED));
            }
            if n < SMALL {
                return Ok(Plan::Done(compress(None, p.base.0)?, p.base.0));
            }
            let mut tried = Vec::new();
            for level in [p.base.0].into_iter().chain(p.higher.iter().map(|h| h.0)).chain(p.lower.iter().copied()) {
                tried.push((level, if level == PROBE { probe.clone() } else { trial(&sample, level)? }));
            }
            let lens: Vec<usize> = tried.iter().map(|(_, c)| len(c)).collect();
            let (higher, lower) = lens[1..].split_at(p.higher.len());
            let level = pick(&p, sample.len(), lens[0], higher, lower);
            let best = tried.into_iter().find(|t| t.0 == level).expect("a level tried");
            let size = scaled(len(&best.1), sample.len(), n);
            // (dense data at the dense level above any picked: where larger than a sample, as
            // a sample does not tell; else where it is smaller, as the sample is all of it)
            match p.dense {
                Some(dense) if how == How::Dense && sample.len() < n && level >= p.base.0 => {
                    Ok(Plan::Level(dense, size))
                }
                Some(dense) if how == How::Dense && sample.len() == n && level >= p.base.0 => {
                    let c = trial(&sample, dense)?;
                    Ok(if len(&c) < len(&best.1) { Plan::Done(c, dense) } else { Plan::Done(best.1, best.0) })
                }
                _ if sample.len() == n => Ok(Plan::Done(best.1, best.0)),
                _ => Ok(Plan::Level(best.0, size)),
            }
        }
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

/// [`key`] of `data` compressed as `how` tells: a dense stream is kept apart from the same
/// bytes compressed otherwise (as a file compared with its formula, say).
fn key_how(data: &[&[u8]], how: How) -> StreamKey {
    let (h, n) = key(data);
    (if how == How::Dense { !h } else { h }, n)
}

/// [`checksum`] of `data`.
fn checksum_of(data: &[&[u8]]) -> u64 {
    let mut h = Xxh64::new(0);
    data.iter().for_each(|p| h.update(p));
    h.digest()
}

/// The length of the archive of `d` as it is ([`Encoder::raw_len`]), compressed as `c`
/// ([`compressed`]) if given.
fn archived_raw(d: &[u8], c: Option<&[u8]>) -> usize {
    let mut n = Count(0);
    n.put(&HEADER_MAGIC);
    content_block(&mut n, true, &[d], c);
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
/// all of it up to [`WINDOW_LOG_MAX`].
pub(crate) const LONG: usize = 8 << 20;

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
    #[cfg(test)]
    tests::COMPRESSED.with(|n| n.set(n.get() + 1));
    let zstd = |e: zstd_safe::ErrorCode| Error::new(format!("zstd: {}", zstd_safe::get_error_name(e)));
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
    /// blocks given in order and `params`, with the memories of at most `memory_limit` bytes.
    /// The decoder checks the outputs against `sizes`.
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
    ) -> Decoded<Vec<Vec<u8>>>;
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
            ) -> Decoded<Vec<Vec<u8>>> {
                let a = crate::Apply {
                    module: self.module,
                    resources: resources.to_vec(),
                    params: params.to_vec(),
                    inputs: Vec::new(),
                    len: sizes.iter().sum(),
                    memory_limit,
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

/// An archive opened ([`open`]): its blocks framed, and its tree read where it has one. The
/// data of its blocks is decoded as the leaves are evaluated, a non-leaf block when first sliced
/// (and dropped once no slice of it is left), so that evaluating them takes about the memory of
/// those evaluated at once.
pub struct Archive<'c, 'n> {
    blocks: Blocks<'c>,
    runner: &'n dyn Runner,
    /// The modules loaded, by module block, as they are first applied.
    loaded: Vec<OnceLock<Decoded<Box<dyn Loaded + 'n>>>>,
    /// The tree, and its contents (each a leaf, an output of it and its size).
    tree: Option<(Tree, Vec<ContentRef>)>,
    /// The bytes of the leaves evaluated at once, by their weights ([`Archive::set_budget`]).
    budget: u64,
    /// Told how far the evaluation is ([`Archive::on_progress`]).
    tell: Option<Telling>,
}

/// Archive `c` opened, its modules run by `runner`.
pub fn open<'c, 'n>(c: &'c [u8], runner: &'n dyn Runner) -> Decoded<Archive<'c, 'n>> {
    if c.starts_with(&ENCRYPTED_HEADER_MAGIC) {
        return resource("encrypted archives are not supported yet");
    }
    if !c.starts_with(&HEADER_MAGIC) {
        return invalid("bad header magic");
    }
    let u16_at = |at: usize| u16::from_le_bytes([c[at], c[at + 1]]) as usize;
    let (fields_start, archive_end) = c
        .len()
        .checked_sub(2)
        .and_then(|copy| {
            let fields = copy.checked_sub(u16_at(copy))?;
            Some((fields, fields.checked_sub(8).filter(|&end| end >= HEADER_MAGIC.len())?))
        })
        .ok_or_else(|| Failure::Invalid("truncated footer".into()))?;
    if c[archive_end..archive_end + 6] != FOOTER_MAGIC || u16_at(archive_end + 6) != u16_at(c.len() - 2) {
        return invalid("bad footer");
    }
    let fields = &c[fields_start..c.len() - 2];
    let mut at = 0;
    let (size_hint, metadata) = (leb(fields, &mut at)?, leb(fields, &mut at)?);
    if at != fields.len() || size_hint != (archive_end - HEADER_MAGIC.len()) as u64 {
        return invalid("bad footer");
    }
    let mut blocks = parse(&c[HEADER_MAGIC.len()..archive_end], size_hint, metadata != 0)?;
    let meta = match metadata {
        0 => None,
        back => {
            let at = usize::try_from(back).ok().and_then(|b| fields_start.checked_sub(b));
            match blocks.metadata.pop() {
                Some((m, d)) if Some(m) == at && blocks.metadata.is_empty() => Some((m, tree::read(&d)?)),
                Some(_) if !blocks.metadata.is_empty() => return resource("several segments are not supported yet"),
                _ => return invalid("no metadata block where the footer points"),
            }
        }
    };
    let outputs: Vec<u64> =
        blocks.leaves.iter().map(|l| if let LeafBlock::Formula { outputs, .. } = l { *outputs } else { 1 }).collect();
    let tree = meta.map(|(at, m)| tree_of(m, at, &blocks, &outputs)).transpose()?;
    let loaded = blocks.modules.iter().map(|_| OnceLock::new()).collect();
    Ok(Archive { blocks, runner, loaded, tree, budget: u64::MAX, tell: None })
}

impl Archive<'_, '_> {
    /// The tree, where the archive has a metadata block.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref().map(|t| &t.0)
    }

    /// Has leaves evaluated at once only while their weights add up to at most `bytes` (or one
    /// alone), taken in their order: the weight of a leaf is the most bytes its evaluation holds
    /// at once, as its formula tells (its literals and slices, and the inputs and outputs of an
    /// application as it is made), short of what modules take within. None by default.
    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes;
    }

    /// Tells `tell` how far the evaluation of the leaves is as it goes (none: tells nothing).
    pub fn on_progress(&mut self, tell: Option<Telling>) {
        self.tell = tell;
    }

    /// The number of contents of the tree's files.
    pub fn contents_len(&self) -> usize {
        self.tree.as_ref().map_or(0, |t| t.1.len())
    }

    /// The number of leaf blocks.
    pub fn leaves_len(&self) -> usize {
        self.blocks.leaves.len()
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
        self.each_leaf_into(threads, None, f)
    }

    /// The file of an archive of a single file, written to `sink` as it is evaluated: of a
    /// single leaf, a formula's output that is a concatenation of parts ([`run`]) is not held
    /// whole, but written as each part is made. Returns its length.
    pub fn write_file(&self, threads: usize, sink: &mut Sink) -> Decoded<u64> {
        if self.tree.is_some() {
            return invalid("an archive of a tree");
        }
        let mut n = 0u64;
        if self.blocks.leaves.len() != 1 {
            let Value::File(d) = self.value(threads)? else { unreachable!("an archive of a single file") };
            sink(&d)?;
            return Ok(d.len() as u64);
        }
        let mut counted = |d: &[u8]| {
            n += d.len() as u64;
            sink(d)
        };
        let streaming: Mutex<&mut Sink> = Mutex::new(&mut counted);
        self.each_leaf_into(threads, Some(&streaming), |_, outputs| {
            let mut sink = streaming.lock().unwrap();
            outputs?.iter().try_for_each(|d| sink(d))
        })?;
        Ok(n)
    }

    /// [`Archive::each_leaf`], a leaf of a formula written to `sink` as it is evaluated where
    /// its output may be ([`run`]): `f` given no output of it then.
    fn each_leaf_into(
        &self,
        threads: usize,
        sink: Option<&Mutex<&mut Sink>>,
        f: impl Fn(usize, Decoded<Vec<Vec<u8>>>) -> Decoded<()> + Sync,
    ) -> Decoded<()> {
        let blocks = &self.blocks;
        // every formula block read first: a non-leaf block is dropped once nothing is to slice it
        let mut used = vec![false; blocks.non_leaf.len()];
        let env = |modules: usize, resources: usize, first| Env {
            first,
            modules: &blocks.modules[..modules],
            loaded: &self.loaded[..modules],
            resources: &blocks.resources[..resources],
            runner: self.runner,
        };
        let programs = blocks
            .leaves
            .iter()
            .map(|leaf| match *leaf {
                LeafBlock::Stored(..) => Ok(None),
                LeafBlock::Formula { outputs, data, modules, resources, non_leaf } => {
                    let env = env(modules, resources, non_leaf);
                    read(&blocks.formulas[data], outputs, &env, blocks.non_leaf.len(), &mut used).map(Some)
                }
            })
            .collect::<Decoded<Vec<_>>>()?;
        if used.contains(&false) {
            return invalid("a non-leaf block no formula block references");
        }
        let mut refs = vec![0; blocks.non_leaf.len()];
        for p in programs.iter().flatten() {
            for insn in &p.insns {
                if let &Insn::Slice { block, .. } = insn {
                    refs[p.blocks[block]] += 1;
                }
            }
        }
        // by leaf, its weight and the bytes of its outputs
        let (weights, lens): (Vec<u64>, Vec<u64>) = (blocks.leaves.iter().zip(&programs))
            .map(|(leaf, p)| match (leaf, p) {
                (&LeafBlock::Stored(d, zstd, checked, _), _) => {
                    let n = stored_len(d, zstd, checked);
                    (n, n)
                }
                (_, Some(p)) => p.weight(),
                _ => (0, 0),
            })
            .unzip();
        let stored = blocks.leaves.iter().map(|l| if let &LeafBlock::Stored(d, ..) = l { d.len() as u64 } else { 0 });
        let data_len = stored.sum::<u64>() + blocks.non_leaf.iter().map(|(d, ..)| d.len() as u64).sum::<u64>();
        let tally = Tally {
            tell: self.tell.as_deref(),
            decompressed: AtomicU64::new(0),
            evaluated: AtomicU64::new(0),
            totals: (data_len, lens.iter().sum()),
        };
        tally.add(0, 0);
        let contents = Contents {
            refs: refs.into_iter().map(AtomicUsize::new).collect(),
            data: blocks.non_leaf.iter().map(|&(d, zstd, dict)| Mutex::new(Slot::Raw(d, zstd, dict))).collect(),
            dictionaries: &blocks.dictionaries,
            tally: &tally,
        };
        let gate = Gate::new(self.budget);
        let programs: Vec<Mutex<Option<Program>>> = programs.into_iter().map(Mutex::new).collect();
        // each leaf on a worker of its own, which is given back as it is done, for those still
        // running to take for their own work (a stream that takes long alone)
        let done = aqr_common::pool::dispatch(
            blocks.leaves.len(),
            threads,
            |i| gate.admit(weights[i]),
            |i, _admitted| {
                tally.step(Step::Started(i));
                let outputs = match blocks.leaves[i] {
                    LeafBlock::Stored(raw, zstd, checked, dict) => {
                        let d = decoded_with(raw, zstd, checked, dict.map(|d| &blocks.dictionaries[d]));
                        tally.add(raw.len() as u64, 0);
                        d.map(|d| vec![d])
                    }
                    LeafBlock::Formula { modules, resources, non_leaf, .. } => {
                        let p = programs[i].lock().unwrap().take().expect("each leaf once");
                        let env = env(modules, resources, non_leaf);
                        match sink {
                            Some(sink) => run(p, &env, &contents, Some(&mut **sink.lock().unwrap())),
                            None => run(p, &env, &contents, None),
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
                    Mutex::new((0..self.blocks.leaves.len()).map(|_| None).collect());
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
        let Some((_, refs)) = &self.tree else { return invalid("an archive of a single file") };
        // by leaf, its outputs taken: output, content, size
        let mut of: Vec<Vec<(usize, usize, u64)>> = vec![Vec::new(); self.blocks.leaves.len()];
        refs.iter().enumerate().for_each(|(c, &(l, o, n))| of[l].push((o, c, n)));
        self.leaves(threads, |i, mut outputs| {
            for &(o, c, n) in &of[i] {
                let d = std::mem::take(&mut outputs[o]);
                if d.len() as u64 != n {
                    return invalid("a file of another size than its entry's");
                }
                f(c, d)?;
            }
            Ok(())
        })
    }
}

/// The tree of metadata block `m` at `at`, among `blocks` whose leaves have `outputs` each;
/// and its contents, each a leaf and an output of it, and the size of the files of it.
#[allow(clippy::type_complexity)]
fn tree_of(
    m: tree::Metadata,
    at: usize,
    blocks: &Blocks,
    outputs: &[u64],
) -> Decoded<(Tree, Vec<(usize, usize, u64)>)> {
    if m.prev != (at - HEADER_MAGIC.len() + 1) as u64 {
        return invalid("a first metadata block not pointing to the header");
    }
    let to = |offsets: &[u64], at_: &[usize]| {
        offsets.len() == at_.len() && offsets.iter().zip(at_).all(|(&o, &b)| at.checked_sub(o as usize) == Some(b))
    };
    if !to(&m.modules, &blocks.module_at)
        || !to(&m.resources, &blocks.resource_at)
        || !to(&m.dictionaries, &blocks.dictionary_at)
    {
        return invalid("module, resource or dictionary offsets not those of the blocks");
    }
    let mut refs: Vec<(usize, usize, u64)> = Vec::new();
    let mut ref_of: HashMap<(usize, usize), usize> = HashMap::new();
    // the content of output `output` of the leaf block at relative offset `offset`, of `size`
    // bytes; and the leaf
    let mut content = |offset: u64, output: u64, size: u64| -> Decoded<(usize, usize)> {
        let leaf = at
            .checked_sub(offset as usize)
            .and_then(|b| blocks.leaf_at.binary_search(&b).ok())
            .ok_or_else(|| Failure::Invalid("a file or a fork not at a leaf block".into()))?;
        if output >= outputs[leaf] {
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
    // the tree's index of each entry by number, tombstones (of nothing to remove) left out
    let mut index = vec![None; m.entries.len() + 1];
    let mut names = std::collections::HashSet::new();
    for (k, (parent, name, read)) in m.entries.into_iter().enumerate() {
        let k = k + 1;
        let parent_index = match parent {
            0 => None,
            p => Some(index[p].expect("a directory")),
        };
        if !names.insert((parent, name.clone())) {
            return invalid("two entries of a name in a directory");
        }
        let node = match read {
            tree::Read::Dir => EntryNode::Dir,
            tree::Read::File { offset, output, size, dictionary } => {
                let (c, leaf) = content(offset, output, size)?;
                if blocks.leaf_dictionary[leaf].map_or(0, |d| d as u64 + 1) != dictionary {
                    return invalid("a file's dictionary not that of its leaf block");
                }
                EntryNode::File(c)
            }
            tree::Read::Link(target) => EntryNode::Link(target),
            tree::Read::Fifo => EntryNode::Fifo,
            tree::Read::Device { block, major, minor } => EntryNode::Device { block, major, minor },
            tree::Read::Special(0) => {
                if !extras[k].is_empty() || !m.forks[k].is_empty() {
                    return invalid("a tombstone of an extra");
                }
                continue;
            }
            tree::Read::Special(kind) => EntryNode::Special(kind),
        };
        index[k] = Some(t.entries.len());
        t.entries.push(Entry { parent: parent_index, name, node, extra: std::mem::take(&mut extras[k]) });
    }
    // (after the files, so that their contents are numbered as they are)
    for (k, forks) in m.forks.into_iter().enumerate() {
        for (name, (offset, output, size)) in forks {
            let c = content(offset, output, size)?.0;
            match k {
                0 => &mut t.root,
                k => &mut t.entries[index[k].expect("not a tombstone")].extra,
            }
            .forks
            .push((name, c));
        }
    }
    Ok((t, refs))
}

fn leb(d: &[u8], at: &mut usize) -> Decoded<u64> {
    varint::get(d, at).map_err(|e| Failure::Invalid(e.0.into()))
}

/// The blocks of an archive of a segment at most, framed: the data of leaf data blocks and
/// non-leaf blocks as it is in the archive (with whether it is Zstandard data, and checked).
struct Blocks<'c> {
    leaves: Vec<LeafBlock<'c>>,
    /// Where the leaf, module and resource blocks start, in the archive.
    leaf_at: Vec<usize>,
    module_at: Vec<usize>,
    resource_at: Vec<usize>,
    dictionary_at: Vec<usize>,
    /// The dictionary (by index) of each leaf block, where it takes one.
    leaf_dictionary: Vec<Option<usize>>,
    /// The metadata blocks, where each starts and its decoded data.
    metadata: Vec<(usize, Vec<u8>)>,
    /// The formula data of the formula blocks.
    formulas: Vec<Vec<u8>>,
    /// The data of each non-leaf block, whether it is Zstandard data, and its dictionary.
    non_leaf: Vec<(&'c [u8], bool, Option<usize>)>,
    modules: Vec<Vec<u8>>,
    resources: Vec<Resource>,
    dictionaries: Vec<dict::Dictionary>,
}

enum LeafBlock<'c> {
    /// Its data, whether it is Zstandard data and whether checked, and its dictionary.
    Stored(&'c [u8], bool, bool, Option<usize>),
    /// The number of outputs and the formula data (by index), with the numbers of module and
    /// resource blocks before it and of non-leaf blocks.
    Formula { outputs: u64, data: usize, modules: usize, resources: usize, non_leaf: usize },
}

/// The blocks in `d`; `metadata` tells whether the footer points to a metadata block.
fn parse(d: &[u8], size_hint: u64, metadata: bool) -> Decoded<Blocks<'_>> {
    let mut b = Blocks {
        leaves: Vec::new(),
        leaf_at: Vec::new(),
        module_at: Vec::new(),
        resource_at: Vec::new(),
        dictionary_at: Vec::new(),
        leaf_dictionary: Vec::new(),
        metadata: Vec::new(),
        formulas: Vec::new(),
        non_leaf: Vec::new(),
        modules: Vec::new(),
        resources: Vec::new(),
        dictionaries: Vec::new(),
    };
    // the generic structure first: a block of an undefined critical type anywhere in it makes
    // the archive unsupported, whatever else is wrong with the other blocks
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
        framed.push((start, ty, data));
    }
    if let Some(&(_, ty, _)) = framed.iter().find(|&&(_, ty, _)| critical(ty) && !defined(ty)) {
        return Err(Failure::Unsupported(format!("a block of an undefined critical type {ty:#x}")));
    }
    for (start, ty, data) in framed {
        let at = start + HEADER_MAGIC.len();
        // (content blocks after a metadata block are of another segment)
        if !b.metadata.is_empty() && matches!(ty, 0x20..=0x37 | 0x40..=0x4b | 0x50..=0x57 | 0x60..=0x67 | 0x70..=0x77) {
            return resource("several segments are not supported yet");
        }
        match ty {
            // spacer blocks, of any space: only compressors have to zero it
            0x00..=0x03 => {}
            0x04 if start == 0 => {
                let mut p = 0;
                if leb(data, &mut p)? != size_hint || p != data.len() {
                    return invalid("bad size hint block");
                }
            }
            // data blocks: `001k kcss` (leaf) and `0100 kkss` (non-leaf, always checked), of a
            // dictionary where kk is 2
            0x20..=0x37 => {
                let (data, dict) = dictionary_of(data, ty & 0x10 != 0, b.dictionaries.len())?;
                b.leaf_at.push(at);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Stored(data, ty & 0x18 != 0, ty & 0x04 != 0, dict));
            }
            0x40..=0x4b => {
                let (data, dict) = dictionary_of(data, ty & 0x08 != 0, b.dictionaries.len())?;
                b.non_leaf.push((data, ty & 0x0c != 0, dict));
            }
            // a shorthand block: the hash of a registered dictionary
            0x50 => return resource("registered dictionaries are not supported yet"),
            // dictionary blocks: `0101 0tss`
            0x51..=0x57 => {
                b.dictionary_at.push(at);
                b.dictionaries.push(dict::Dictionary::read(decoded(data, true, true)?, ty & 0x04 != 0)?);
            }
            0x60..=0x63 => {
                b.module_at.push(at);
                b.modules.push(decoded(data, true, true)?);
            }
            0x64..=0x67 => {
                b.resource_at.push(at);
                b.resources.push(Resource::new(decoded(data, true, true)?));
            }
            // formula blocks: `0111 0dss`
            0x70..=0x77 => {
                let (data, dict) = dictionary_of(data, ty & 0x04 != 0, b.dictionaries.len())?;
                let mut p = 0;
                let outputs = leb(data, &mut p)?;
                if outputs == 0 {
                    return invalid("a formula block of no outputs");
                }
                b.formulas.push(decoded_with(&data[p..], true, true, dict.map(|d| &b.dictionaries[d]))?);
                b.leaf_at.push(at);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Formula {
                    outputs,
                    data: b.formulas.len() - 1,
                    modules: b.modules.len(),
                    resources: b.resources.len(),
                    non_leaf: b.non_leaf.len(),
                });
            }
            // mirror blocks and undefined non-critical blocks, not read
            _ if !critical(ty) => {}
            0x10..=0x17 if metadata => b.metadata.push((at, decoded(data, ty & 0x04 != 0, true)?)),
            0x10..=0x17 => return invalid("a metadata block, but none in the footer"),
            _ => return invalid(format!("bad block of type {ty:#x}")),
        }
    }
    Ok(b)
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
/// index: one of the `before` dictionary blocks before it.
fn dictionary_of(d: &[u8], has: bool, before: usize) -> Decoded<(&[u8], Option<usize>)> {
    if !has {
        return Ok((d, None));
    }
    let mut at = 0;
    let i = leb(d, &mut at)?;
    match usize::try_from(i).ok().filter(|&i| i < before) {
        Some(i) => Ok((&d[at..], Some(i))),
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
    let zstd = |e: zstd_safe::ErrorCode| Failure::Invalid(format!("zstd: {}", zstd_safe::get_error_name(e)));
    // the frame: a header, then the blocks (not copied after it)
    let head = [ZSTD_MAGIC[0], ZSTD_MAGIC[1], ZSTD_MAGIC[2], ZSTD_MAGIC[3], 0, window];
    let mut frame = [&head[..], blocks].into_iter();
    let mut dctx = zstd_safe::DCtx::create();
    match dict {
        None => {}
        Some(dict::Dictionary::Raw(d)) => dctx.ref_prefix(d).map(drop).map_err(zstd)?,
        Some(dict::Dictionary::Tables(d)) => dctx.ref_ddict(d).map(drop).map_err(zstd)?,
    }
    let mut input = InBuffer::around(frame.next().unwrap());
    // up to one byte more than `size`, to tell a longer output: as much room as that from the
    // start where it can be had, else made as the output comes
    let limit = size.saturating_add(1);
    let mut out: Vec<u8> = Vec::new();
    if let Ok(n) = usize::try_from(limit) {
        let _ = out.try_reserve_exact(n);
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
    /// The most bytes on the stack at once (with the inputs and outputs of an instruction as it
    /// runs both, or an application's memory limit where larger: the working memory of its module,
    /// which a native one takes too), as the sizes the program tells: the weight of its leaf
    /// ([`Archive::set_budget`]); and the bytes on it at the end, its outputs.
    fn weight(&self) -> (u64, u64) {
        let (mut st, mut on, mut most) = (Vec::<u64>::new(), 0u64, 0u64);
        let pop = |st: &mut Vec<u64>, on: &mut u64, n: usize| -> u64 {
            let taken: u64 = st.drain(st.len().saturating_sub(n)..).sum();
            *on -= taken;
            taken
        };
        for insn in &self.insns {
            let limit = match insn {
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
            most = most.max(on.saturating_add(ins.saturating_add(out).max(limit)));
            on = on.saturating_add(out);
            st.extend(outs);
        }
        (most, on)
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

/// The leaves let in by [`Archive::set_budget`] (in their order, [`aqr_common::pool::dispatch`]),
/// each once those evaluated leave room for it.
struct Gate {
    budget: u64,
    /// The weights of the leaves in.
    state: Mutex<u64>,
    changed: std::sync::Condvar,
}

/// A leaf let in, until dropped.
struct Admitted<'g>(&'g Gate, u64);

impl Gate {
    fn new(budget: u64) -> Self {
        Gate { budget, state: Mutex::new(0), changed: std::sync::Condvar::new() }
    }

    /// Waits for a leaf of weight `w` to be let in: alone, or with those in within the budget.
    fn admit(&self, w: u64) -> Admitted<'_> {
        if self.budget == u64::MAX {
            return Admitted(self, 0);
        }
        let mut s = self.state.lock().unwrap();
        while *s != 0 && s.saturating_add(w) > self.budget {
            s = self.changed.wait(s).unwrap();
        }
        *s += w;
        Admitted(self, w)
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        if self.0.budget != u64::MAX {
            *self.0.state.lock().unwrap() -= self.1;
            self.0.changed.notify_all();
        }
    }
}

/// Formula data `d` of `outputs` outputs read, among `non_leaf` non-leaf blocks, marking those it
/// refers to in `used`: everything checked that does not depend on the evaluation.
fn read<'a>(d: &'a [u8], outputs: u64, env: &Env, non_leaf: usize, used: &mut [bool]) -> Decoded<Program<'a>> {
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
            SWAP => Insn::Swap(r.index(depth)?),
            SLICE => {
                let block = r.index(blocks.len())?;
                b_used[block] = true;
                Insn::Slice { block, start: r.num()?, len: r.num()? }
            }
            APPLY => {
                let module = r.index(modules.len())?;
                m_used[module] = true;
                let n = r.count()?;
                let res = (0..n).map(|_| r.index(resources.len())).collect::<Decoded<Vec<_>>>()?;
                res.iter().for_each(|&i| r_used[i] = true);
                let params = r.bytes()?;
                let memory_limit = r.num()?;
                let num_inputs = usize::try_from(r.num()?).unwrap_or(usize::MAX);
                let output_sizes = r.nums()?;
                if output_sizes.is_empty() {
                    return invalid("an application without outputs");
                }
                Insn::Apply(Apply { module, resources: res, params, memory_limit, num_inputs, output_sizes })
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

/// A byte sequence on the stack: a slice of a non-leaf block (by its ordinal among all, with
/// its data), a literal, or bytes of its own.
enum Item<'a> {
    Slice(usize, Arc<Vec<u8>>, Range<usize>),
    Literal(&'a [u8]),
    Bytes(Vec<u8>),
    /// Of so many bytes written out already ([`Streamed`]).
    Written(u64),
}

/// The non-leaf blocks as formula blocks are evaluated (several at once), each with the slices
/// of it yet to be evaluated or on a stack: once there are none, it is dropped.
struct Contents<'c, 't> {
    data: Vec<Mutex<Slot<'c>>>,
    dictionaries: &'c [dict::Dictionary],
    refs: Vec<AtomicUsize>,
    /// Told of the blocks as they are decoded.
    tally: &'t Tally<'t>,
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
/// first sliced, then decoded until no slice of it is left.
enum Slot<'c> {
    Raw(&'c [u8], bool, Option<usize>),
    Decoded(Arc<Vec<u8>>),
    Gone,
}

impl Contents<'_, '_> {
    /// The data of block `o`, while any slice of it is to be evaluated: decoded the first time.
    fn block(&self, o: usize) -> Decoded<Arc<Vec<u8>>> {
        let mut s = self.data[o].lock().unwrap();
        if let Slot::Raw(d, zstd, dict) = *s {
            let decoded = decoded_with(d, zstd, true, dict.map(|d| &self.dictionaries[d]));
            self.tally.add(d.len() as u64, 0);
            *s = Slot::Decoded(Arc::new(decoded?));
        }
        match &*s {
            Slot::Decoded(d) => Ok(d.clone()),
            _ => unreachable!("a block with slices left"),
        }
    }

    fn get<'v>(&self, v: &'v Item) -> &'v [u8] {
        match v {
            Item::Slice(_, d, r) => &d[r.clone()],
            Item::Literal(l) => l,
            Item::Bytes(b) => b,
            Item::Written(_) => unreachable!("a value written out is only concatenated"),
        }
    }

    /// `v` used up: its bytes, of its own.
    fn take(&self, v: Item) -> Vec<u8> {
        match v {
            Item::Slice(o, d, r) => {
                let out = d[r].to_vec();
                self.release(o);
                out
            }
            Item::Literal(l) => l.to_vec(),
            Item::Bytes(b) => b,
            Item::Written(_) => unreachable!("a value written out is only concatenated"),
        }
    }

    /// `vs` used up, once their bytes are no longer needed.
    fn drop_all(&self, vs: Vec<Item>) {
        for v in vs {
            if let Item::Slice(o, ..) = v {
                self.release(o);
            }
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

/// What takes the bytes of an output as it is evaluated ([`run`]): a [`Sink`], not needing to
/// be sent.
type Taker<'a> = dyn FnMut(&[u8]) -> Decoded<()> + 'a;

/// Where the output of a formula block of one output may be written out as it is evaluated:
/// the values it is the concatenation of (through Concat instructions, at any depth), each by
/// its number (values numbered as they are pushed), in the order of the output.
struct Streamed {
    order: HashMap<usize, usize>,
}

/// [`Streamed`] of program `p`, if it has one output of more than one part, no part read but by
/// a concatenation (a Checksum instruction but the last one, of the output, say).
fn streamed(p: &Program) -> Option<Streamed> {
    enum V {
        Part,
        Concat(Vec<usize>),
    }
    let (mut vals, mut stack, mut read) = (Vec::new(), Vec::<usize>::new(), HashSet::new());
    let last = p.insns.len().checked_sub(1)?;
    let push = |vals: &mut Vec<V>, stack: &mut Vec<usize>, v| {
        stack.push(vals.len());
        vals.push(v);
    };
    for (i, insn) in p.insns.iter().enumerate() {
        match insn {
            Insn::Nop | Insn::Concat(1) => {}
            Insn::Literal(_) | Insn::Slice { .. } => push(&mut vals, &mut stack, V::Part),
            &Insn::Swap(d) => {
                let top = stack.len() - 1;
                stack.swap(top, top - d);
            }
            Insn::Apply(a) => {
                stack.truncate(stack.len() - a.num_inputs);
                (0..a.output_sizes.len()).for_each(|_| push(&mut vals, &mut stack, V::Part));
            }
            &Insn::Concat(n) => {
                let parts = stack.split_off(stack.len() - n);
                push(&mut vals, &mut stack, V::Concat(parts));
            }
            &Insn::Pack(n) => {
                stack.truncate(stack.len() - n);
                push(&mut vals, &mut stack, V::Part);
            }
            Insn::Split(sizes) => {
                stack.pop();
                (0..sizes.len()).for_each(|_| push(&mut vals, &mut stack, V::Part));
            }
            Insn::Patch(_) => {
                stack.truncate(stack.len() - 2);
                push(&mut vals, &mut stack, V::Part);
            }
            Insn::Checksum(_) if i == last && stack.len() == 1 => {}
            Insn::Checksum(_) => {
                read.insert(*stack.last()?);
            }
        }
    }
    let &[root] = &stack[..] else { return None };
    // the parts in order, from the root through its concatenations
    let (mut order, mut todo) = (HashMap::new(), vec![root]);
    while let Some(v) = todo.pop() {
        if read.contains(&v) {
            return None;
        }
        match &vals[v] {
            V::Concat(parts) => todo.extend(parts.iter().rev()),
            V::Part => {
                order.insert(v, order.len());
            }
        }
    }
    (order.len() > 1).then_some(Streamed { order })
}

/// Writes the parts of a streamed output ([`Streamed`]) as they come, in order: those that
/// come before their turn held until it.
struct Writer<'s, 'w> {
    plan: &'s Streamed,
    sink: &'w mut Taker<'w>,
    next: usize,
    held: HashMap<usize, Vec<u8>>,
    hash: Xxh64,
}

impl Writer<'_, '_> {
    fn write(&mut self, d: &[u8]) -> Decoded<()> {
        self.hash.update(d);
        (self.sink)(d)
    }

    /// Part `v` (its number) of bytes `d`, written or held.
    fn part(&mut self, v: usize, d: &[u8]) -> Decoded<()> {
        let at = self.plan.order[&v];
        if at != self.next {
            self.held.insert(at, d.to_vec());
            return Ok(());
        }
        self.write(d)?;
        self.next += 1;
        while let Some(h) = self.held.remove(&self.next) {
            self.write(&h)?;
            self.next += 1;
        }
        Ok(())
    }
}

/// The outputs of formula block `p`, with the non-leaf blocks in `contents`. Where `sink` is
/// given and the output may be streamed ([`streamed`]), it is written to `sink` as its parts
/// are evaluated, rather than held whole, and no output is returned.
fn run<'a>(p: Program<'a>, env: &Env, contents: &Contents, sink: Option<&mut Taker<'_>>) -> Decoded<Vec<Vec<u8>>> {
    let plan = sink.is_some().then(|| streamed(&p)).flatten();
    let mut w = match (plan.as_ref(), sink) {
        (Some(plan), Some(sink)) => Some(Writer { plan, sink, next: 0, held: HashMap::new(), hash: Xxh64::new(0) }),
        _ => None,
    };
    // literals and slices where they are, each value numbered as [`streamed`] numbers it
    let mut stack: Vec<Item<'a>> = Vec::new();
    let mut ids: Vec<usize> = Vec::new();
    let mut next = 0;
    // pushes a value: a part of the output streamed is written out
    let mut push =
        |stack: &mut Vec<Item<'a>>, ids: &mut Vec<usize>, w: &mut Option<Writer<'_, '_>>, v: Item<'a>| -> Decoded<()> {
            let id = next;
            next += 1;
            let v = match w {
                Some(w) if w.plan.order.contains_key(&id) => {
                    let n = contents.get(&v).len() as u64;
                    w.part(id, contents.get(&v))?;
                    contents.drop_all(vec![v]);
                    Item::Written(n)
                }
                _ => v,
            };
            stack.push(v);
            ids.push(id);
            Ok(())
        };
    for insn in p.insns {
        match insn {
            Insn::Nop => {}
            Insn::Literal(l) => push(&mut stack, &mut ids, &mut w, Item::Literal(l))?,
            Insn::Swap(depth) => {
                let top = stack.len() - 1;
                stack.swap(top, top - depth);
                ids.swap(top, top - depth);
            }
            Insn::Slice { block, start, len } => {
                let o = p.blocks[block];
                let d = contents.block(o)?;
                let r = usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(s, l)| Some(s..s.checked_add(l)?))
                    .filter(|r| r.end <= d.len())
                    .ok_or_else(|| Failure::Invalid("slice out of its block".into()))?;
                push(&mut stack, &mut ids, &mut w, Item::Slice(o, d, r))?;
            }
            Insn::Apply(a) => {
                let inputs = stack.split_off(stack.len() - a.num_inputs);
                ids.truncate(stack.len());
                let res: Vec<Resource> = a.resources.iter().map(|&i| env.resources[p.resources[i]].clone()).collect();
                let bytes: Vec<&[u8]> = inputs.iter().map(|v| contents.get(v)).collect();
                let outputs = apply(env, p.modules[a.module], &a, &bytes, &res)?;
                contents.drop_all(inputs);
                for o in outputs {
                    push(&mut stack, &mut ids, &mut w, Item::Bytes(o))?;
                }
            }
            Insn::Concat(1) => {}
            Insn::Concat(n) => {
                let parts = stack.split_off(stack.len() - n);
                ids.truncate(stack.len());
                // (parts of the output streamed are all written out, others none)
                let item = match parts.iter().filter(|v| matches!(v, Item::Written(_))).count() {
                    0 => {
                        let bytes = parts.iter().map(|v| contents.get(v)).collect::<Vec<_>>().concat();
                        contents.drop_all(parts);
                        Item::Bytes(bytes)
                    }
                    k if k == n => {
                        Item::Written(parts.iter().map(|v| if let Item::Written(m) = v { *m } else { 0 }).sum())
                    }
                    _ => unreachable!("a concatenation of parts streamed and not"),
                };
                push(&mut stack, &mut ids, &mut w, item)?;
            }
            Insn::Pack(n) => {
                let parts = stack.split_off(stack.len() - n);
                ids.truncate(stack.len());
                let bytes = packed::pack(&parts.iter().map(|v| contents.get(v)).collect::<Vec<_>>());
                contents.drop_all(parts);
                push(&mut stack, &mut ids, &mut w, Item::Bytes(bytes))?;
            }
            Insn::Split(sizes) => {
                let x = stack.pop().unwrap();
                ids.pop();
                let bytes = contents.get(&x);
                if sizes.iter().try_fold(0u64, |s, &n| s.checked_add(n)) != Some(bytes.len() as u64) {
                    return invalid("a split into sizes not adding up");
                }
                // slices and literals split where they are (a block's slices counted first, as
                // any written out is let go)
                if let Item::Slice(o, ..) = x {
                    contents.refs[o].fetch_add(sizes.len() - 1, Ordering::Relaxed);
                }
                let mut at = 0;
                for &n in &sizes {
                    let r = at..at + n as usize;
                    let item = match &x {
                        Item::Slice(o, d, s) => Item::Slice(*o, d.clone(), s.start + r.start..s.start + r.end),
                        &Item::Literal(l) => Item::Literal(&l[r]),
                        Item::Bytes(b) => Item::Bytes(b[r].to_vec()),
                        Item::Written(_) => unreachable!("a value written out is only concatenated"),
                    };
                    push(&mut stack, &mut ids, &mut w, item)?;
                    at += n as usize;
                }
            }
            Insn::Patch(len) => {
                let items = stack.split_off(stack.len() - 2);
                ids.truncate(stack.len());
                let out = crate::patch::apply(contents.get(&items[0]), contents.get(&items[1]), len)
                    .map_err(|e| Failure::Invalid(e.0))?;
                contents.drop_all(items);
                push(&mut stack, &mut ids, &mut w, Item::Bytes(out))?;
            }
            Insn::Checksum(v) => {
                let sum = match (stack.last().unwrap(), &w) {
                    (Item::Written(_), Some(w)) => w.hash.digest(),
                    (top, _) => checksum(contents.get(top)),
                };
                if sum != v {
                    return invalid("output checksum mismatch");
                }
            }
        }
    }
    match w {
        Some(w) => {
            debug_assert!(w.next == w.plan.order.len() && w.held.is_empty());
            Ok(Vec::new())
        }
        None => Ok(stack.into_iter().map(|v| contents.take(v)).collect()),
    }
}

/// The outputs of application `a` of module block `module`.
fn apply(env: &Env, module: usize, a: &Apply, inputs: &[&[u8]], resources: &[Resource]) -> Decoded<Vec<Vec<u8>>> {
    let loaded = env.loaded[module].get_or_init(|| env.runner.load(&env.modules[module]));
    let loaded = loaded.as_ref().map_err(Failure::clone)?;
    let outputs = loaded.apply(a.params, resources, inputs, &a.output_sizes, a.memory_limit)?;
    if outputs.len() != a.output_sizes.len() || outputs.iter().zip(&a.output_sizes).any(|(o, &n)| o.len() as u64 != n) {
        return invalid(format!("module block {module}: outputs of other sizes than declared"));
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A stream of more than [`BLOCK`] bytes is several non-leaf blocks: slices within one,
    /// over a boundary and of all of it come back, and the archive has a block for each.
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
                let ordinals = [Some(0)];
                let mut out = Vec::new();
                Emit { ordinals: &ordinals, modules: &[], resources: &[], out: &mut out, stack: vec![] }.node(&f.root);
                out.len()
            }
        );
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        assert!(decode(&c, &natives()).unwrap() == want);
        assert_eq!(open(&c, &natives()).unwrap().blocks.non_leaf.len(), 3);
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
        };
        let [o0, o1, o2] = <[Node; 3]>::try_from(f.split(rev, vec![3, 0, 5])).unwrap();
        f.root = Node::concat([o2, Node::slice(0, 0, 1), o1, o0]);
        assert!(f.to_string().starts_with("shared #0 rev@1 split 3 0 5"));
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), b"edcbaahgf");
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
        let ordinals: Vec<_> = (0..f.streams.len() as u64).map(Some).collect();
        let mut out = Vec::new();
        let mut e = Emit { ordinals: &ordinals, modules: &[REV], resources: &[], out: &mut out, stack: vec![] };
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
                    let (_, ni, no) = (num(&mut at), num(&mut at), num(&mut at));
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
            assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
            assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        }
        // Streams without slices have no blocks.
        let mut f = sample();
        f.streams.insert(
            0,
            Stream { class: "unused".into(), kind: Kind::Unknown, content: false, data: vec![1; 100].into() },
        );
        f.root.slices_mut(&mut |s, _, _| *s += 1);
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        // A module named twice has one block.
        let mut f = sample();
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.push(ns[1].clone());
        let c = encode(&f, None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
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
        assert_eq!(types(&c), [MODULE, RESOURCE, RESOURCE, FORMULA, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), expect);
        // A resource block is always compressed, even when that makes it longer.
        let noise = Resource::new(aqr_common::rng::noise(1000));
        f.root = Node::apply_with(PRE, vec![noise.clone()], vec![], vec![x], 1003);
        let c = encode(&f, None, Level::MID).unwrap();
        assert_eq!(types(&c), [MODULE, RESOURCE + 1, FORMULA, NON_LEAF_RAW]);
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
        assert!(ENCRYPTED_HEADER_MAGIC.starts_with(&enc(0x155_04dc_a579)));
        for ty in [0x278, 0x69, 0x90_5bcb_957d, 0x7fc, 0x155_04dc_a579] {
            let mut b = enc(ty);
            b.resize(b.len() + (1 << (ty % 4)), 0);
            b.resize(b.len().next_multiple_of(2), 0);
            let r = decode(&archive(|out| out.extend_from_slice(&b)), &NoModules);
            assert!(matches!(r, Err(Failure::Invalid(_))), "{ty:#x}: {r:?}");
        }
    }

    /// A stream of [`Kind::Compressed`] is stored as it is, at any level, leaf or not.
    #[test]
    fn compressed_kind_stored() {
        let d = vec![7; 100_000];
        for level in [Level::MID, Level::Zstd(3)] {
            let mut f = Formula::default();
            f.root = f.push_as("a", Kind::Compressed, d.clone());
            let types = |c: &[u8]| types(c).into_iter().map(|t| t & !3).collect::<Vec<_>>();
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [LEAF_RAW]);
            assert_eq!(decode(&c, &NoModules).unwrap(), d);
            let b = f.push("b", d.clone());
            f.root = Node::concat([f.root.clone(), b]);
            // (the bytes of "b" would be kept stored too: they are those of "a")
            f.streams[1].data[0] = 0;
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [FORMULA, NON_LEAF_RAW, NON_LEAF_ZSTD]);
            assert_eq!(decode(&c, &NoModules).unwrap(), [&d[..], &f.streams[1].data].concat());
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
            data: d.to_vec().into(),
        };
        for (level, dense) in [(Level::MID, false), (Level::Of(7), false), (Level::HIGH, true), (Level::Of(9), true)] {
            let enc = Encoder::new(level);
            assert_eq!(enc.how(&s(&long, Kind::Text, "raw")) == How::Dense, dense, "{level:?}");
            assert_eq!(enc.how(&s(&long, Kind::Unknown, "x86/OP")) == How::Dense, dense, "{level:?}");
            assert_eq!(enc.how(&s(&long, Kind::Unknown, "raw")), How::Usual);
            assert_eq!(enc.how(&s(&long, Kind::Compressed, "raw")), How::Stored);
            let at = |d: &[u8], how| match enc.plan(key(&[d]), &[d], how).unwrap() {
                Plan::Level(l, _) => l,
                Plan::Done(..) => 0,
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
                Stream { class: "b".into(), kind: Kind::Unknown, content: false, data: d.to_vec().into() },
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
        apply.extend_from_slice(&[2, 1, 4]);
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
        apply.extend_from_slice(&[2, 1, 4]);
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
        apply.extend_from_slice(&[2, 1, 4]);
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
            let mut a = open(&c, &natives).unwrap();
            if let Some(b) = budget {
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
            d.extend_from_slice(&[inputs, sizes.len() as u8]);
            d.extend_from_slice(sizes);
            decode(&formula(&d, &[b"abc"]), &natives())
        };
        assert_eq!(apply(2, &[5]).unwrap(), b"cbcba");
        assert_eq!(apply(3, &[5]), invalid("stack underflow"));
        assert_eq!(apply(2, &[]), invalid("an application without outputs"));
        assert_eq!(apply(2, &[4]), invalid("module block 0: outputs of other sizes than declared"));
    }

    #[test]
    fn bound() {
        let desc = |f: &Formula| {
            let ordinals: Vec<_> = (0..f.streams.len() as u64).map(Some).collect();
            let resources: Vec<&Resource> = f.applies().into_iter().flat_map(|a| &a.resources).collect();
            let mut out = Vec::new();
            Emit { ordinals: &ordinals, modules: &[REV], resources: &resources, out: &mut out, stack: vec![] }
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
