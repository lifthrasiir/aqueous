//! A formula as an Aqueous archive (`spec/index.bs`) of a single file, without a metadata
//! block.
//!
//! [`encode`] lays the formula out as blocks: a module block for each module, a resource block
//! for each resource, the formula block, and a non-leaf block for each stream it slices, in the
//! order of the streams. A formula of a single slice is a leaf block of its own.
//!
//! Until modules are WebAssembly (the spec's "Module execution" is tentative), a module block
//! holds a Wasm module of custom sections only, one of them ([`NATIVE_SECTION`]) naming the
//! module (`name@version`), and a [`NativeModule`] stands in for it as a whole module. Its
//! applications declare no memory limit ([`MEMORY_LIMIT`]).
//!
//! [`Encoder`] encodes several formulas, compressing each stream and resource once however
//! many hold it, at the zstd level that its [`Level`] picks for it; streams of
//! [`Kind::Compressed`] are stored as they are.
//!
//! [`decode`] evaluates such archives, and any other without a metadata block, a dictionary
//! or an encryption.

use std::borrow::Cow;
use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::ops::Range;

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

/// The name of the custom section naming the native implementation of a module block.
pub const NATIVE_SECTION: &str = "aqueous-native";
/// The memory limit of every application, as no module declares one yet.
pub const MEMORY_LIMIT: u64 = u64::MAX;

// Block types with a u8 size field, all with checksums; `+ 0..=3` widens the size field.
const LEAF_RAW: u64 = 0x24;
const LEAF_ZSTD: u64 = 0x2c;
const NON_LEAF_RAW: u64 = 0x40;
const NON_LEAF_ZSTD: u64 = 0x44;
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
/// each stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Each stream uncompressed if incompressible (`PROBE`), else at zstd level
    /// `MID_BASE`, or:
    ///
    /// - at a level of `MID_HIGHER` where that saves enough for its time, at `MID_RATE`
    ///   bytes per second of compression;
    /// - at a level of `MID_LOWER` where that is smaller by 1/32 at least (beyond what a
    ///   sample may be off by), as it is faster too.
    ///
    /// Module and formula blocks, and streams shorter than `MID_SMALL`, are at the base.
    Mid,
    /// Every stream at this zstd level.
    Zstd(i32),
}

/// The zstd level at which a stream is incompressible if it saves less than 1/32 (as a
/// recompressor's entropy-coded output does), whatever level would compress it otherwise.
const PROBE: i32 = 1;
/// The zstd level of [`Level::Mid`], with the bytes per second it compresses (as measured on
/// text and PSD skeletons, as are the others).
const MID_BASE: (i32, f64) = (3, 350e6);
/// The zstd levels below [`MID_BASE`] that [`Level::Mid`] takes where clearly smaller, fastest
/// first: some data compresses better at lower levels.
const MID_LOWER: [i32; 1] = [1];
/// The zstd levels above [`MID_BASE`] that [`Level::Mid`] picks from, with their speed.
const MID_HIGHER: [(i32, f64); 3] = [(4, 330e6), (6, 140e6), (9, 100e6)];
/// The bytes that [`Level::Mid`] would save for a second of compression above [`MID_BASE`].
const MID_RATE: f64 = 4e6;
/// Streams shorter than this are compressed at [`MID_BASE`] by [`Level::Mid`], as picking
/// would take longer than any level.
const MID_SMALL: usize = 64 << 10;
/// [`Level::Mid`] picks the level by this many bytes of the stream, in [`SAMPLE_CHUNKS`]
/// chunks spread evenly over it (or by the whole stream if no longer).
const SAMPLE: usize = 1 << 20;
const SAMPLE_CHUNKS: usize = 16;

/// What an [`Encoder`] makes of a stream: compressed already ([`compressed`]), or to be
/// compressed at a zstd level, to about so many bytes (by a sample).
enum Plan {
    Done(Option<Vec<u8>>),
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
    /// Compressed resources by their [`StreamKey`].
    resources: RefCell<HashMap<StreamKey, Vec<u8>>>,
    /// The archived lengths ([`Encoder::raw_len`]) of streams forgotten compressed
    /// ([`Encoder::forget`]), by their [`StreamKey`]: a stream is compared with formulas of it
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
            resources: RefCell::default(),
            raw_lens: RefCell::default(),
            trials: RefCell::default(),
            cctx: RefCell::new(CCtx::create()),
            shared: false,
        }
    }

    /// Sizes formulas ([`Encoder::len`], [`Encoder::smaller`]) without their resource blocks,
    /// as if the archive held them already: as in an archive of many files that share them,
    /// where they come to nothing per file. For measuring; the archives are the same.
    pub fn share_resources(&mut self) {
        self.shared = true;
    }

    /// The bytes of the resource blocks in the archive of `f`.
    pub fn resources_len(&self, f: &Formula) -> Result<usize> {
        let mut resources: Vec<&Resource> = Vec::new();
        for a in f.applies() {
            for r in &a.resources {
                if !resources.contains(&r) {
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
        Ok(if self.shared { n.0 - self.resources_len(f)? } else { n.0 })
    }

    /// The length of the archive of `d` as it is, without a checksum (`d` itself not copied).
    fn raw_len(&self, d: &[u8]) -> Result<usize> {
        let mut n = Count(0);
        n.put(&HEADER_MAGIC);
        self.content_block(&mut n, true, &[d], false)?;
        let blocks = n.0 - HEADER_MAGIC.len();
        footer(&mut n, blocks);
        Ok(n.0)
    }

    /// [`encode`] at the level of the encoder. Fails on slices out of their stream.
    pub fn encode(&self, f: &Formula, checksum: Option<u64>) -> Result<Vec<u8>> {
        // sized first (the streams compressed then are kept), so as to be written at its size
        let mut out = Vec::with_capacity(self.len(f)? + 16);
        self.write(f, None, checksum, &mut out)?;
        Ok(out)
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
        // of the module and formula blocks
        let level = match self.level {
            Level::Mid => MID_BASE.0,
            Level::Zstd(l) => l,
        };
        let data: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        let streams: Vec<&[&[u8]]> = (0..data.len())
            .map(|s| match stand_in {
                Some((t, bytes)) if t == s => bytes,
                _ => std::slice::from_ref(&data[s]),
            })
            .collect();
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
            self.content_block(out, true, &leaf, f.streams[stream].kind == Kind::Compressed)?;
        } else {
            // the non-leaf block of each stream sliced, by its ordinal
            let mut ordinals = vec![None; f.streams.len()];
            f.visit(&mut |n| {
                if let &Node::Slice { stream, .. } = n {
                    ordinals[stream] = Some(0);
                }
            });
            let mut sliced = Vec::new();
            for (s, o) in ordinals.iter_mut().enumerate() {
                if o.is_some() {
                    *o = Some(sliced.len() as u64);
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
            varint::put(&mut data, sliced.len() as u64);
            sliced.iter().for_each(|_| varint::put(&mut data, 0));
            for n in [resources.len(), modules.len()] {
                varint::put(&mut data, n as u64);
                (0..n).for_each(|i| varint::put(&mut data, i as u64));
            }
            let mut e =
                Emit { ordinals: &ordinals, modules: &modules, resources: &resources, out: &mut data, stack: vec![] };
            for (i, s) in f.shared.iter().enumerate() {
                s.apply.inputs.iter().for_each(|n| e.node(n));
                e.apply(&s.apply, &s.sizes);
                let k = e.stack.len() - s.sizes.len();
                (0..s.sizes.len()).for_each(|j| e.stack[k + j] = Some((i, j)));
            }
            e.node(&f.root);
            debug_assert_eq!(e.stack, [None]);
            if let Some(c) = checksum {
                data.push(CHECKSUM);
                data.extend_from_slice(&c.to_le_bytes());
            }

            for &m in &modules {
                block(out, MODULE, &always_compressed(&mut self.cctx(), &[&wasm(m)], level, self.threads)?);
            }
            for r in resources {
                self.resource_block(out, r)?;
            }
            formula_block(out, 1, &always_compressed(&mut self.cctx(), &[&data], level, self.threads)?);
            for s in sliced {
                self.content_block(out, false, streams[s], f.streams[s].kind == Kind::Compressed)?;
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
            &Node::Slice { stream, start, len } => {
                self.out.push(SLICE);
                varint::put(self.out, self.ordinals[stream].unwrap());
                varint::put(self.out, start);
                varint::put(self.out, len);
                self.stack.push(None);
            }
            Node::Concat(ns) | Node::Pack(ns) => {
                ns.iter().for_each(|n| self.node(n));
                self.out.push(if let Node::Pack(_) = n { PACK } else { CONCAT });
                varint::put(self.out, ns.len() as u64);
                self.stack.truncate(self.stack.len() - ns.len());
                self.stack.push(None);
            }
            Node::Apply(a) => {
                a.inputs.iter().for_each(|n| self.node(n));
                self.apply(a, &[a.len]);
            }
            &Node::Output { shared, index, .. } => {
                // up from where it is, the others keeping their order: swapped with the item
                // above it, then with the next one up and so on
                let at = self.stack.iter().rposition(|&o| o == Some((shared, index))).unwrap();
                let depth = self.stack.len() - 1 - at;
                for d in 1..=depth {
                    self.out.push(SWAP);
                    varint::put(self.out, d as u64);
                }
                self.stack.remove(at);
                self.stack.push(None);
            }
        }
    }

    /// The Apply instruction of `a` (its inputs pushed), of outputs of `sizes`.
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
        varint::put(out, MEMORY_LIMIT);
        varint::put(out, a.inputs.len() as u64);
        varint::put(out, sizes.len() as u64);
        sizes.iter().for_each(|&n| varint::put(out, n));
        self.stack.truncate(self.stack.len() - a.inputs.len());
        self.stack.extend(sizes.iter().map(|_| None));
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
        &Node::Slice { start, len, .. } => 1 + 3 + varint::len(start) + varint::len(len),
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

/// A Wasm module of a custom section only, naming `m` in [`NATIVE_SECTION`].
fn wasm(m: Module) -> Vec<u8> {
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
    custom(&mut w, NATIVE_SECTION, m.to_string().as_bytes());
    w
}

/// The name in [`NATIVE_SECTION`] of Wasm module `w`, if any.
fn native_name(w: &[u8]) -> Option<&str> {
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
            if s.get(p..p.checked_add(n)?)? == NATIVE_SECTION.as_bytes() {
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

/// The end of an archive whose blocks take `blocks` bytes.
fn footer(out: &mut impl Out, blocks: usize) {
    let mut f = Vec::new();
    varint::put(&mut f, blocks as u64);
    varint::put(&mut f, 0);
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

impl Encoder {
    /// Appends a compressed or uncompressed (leaf or non-leaf) block of `data`, whichever the
    /// spec allows; uncompressed where `stored` (and not kept compressed already).
    fn content_block(&self, out: &mut impl Out, leaf: bool, data: &[&[u8]], stored: bool) -> Result<()> {
        let mut streams = self.streams.borrow_mut();
        let c = match streams.entry(key(data)) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let plan = if stored { Plan::Done(None) } else { self.plan(*e.key(), data)? };
                e.insert(match plan {
                    Plan::Done(c) => c,
                    Plan::Level(level, size) => compressed_to(&mut self.cctx(), data, level, self.threads, size)?,
                })
            }
        };
        content_block(out, leaf, data, c.as_deref());
        Ok(())
    }

    /// Appends a resource block of `r`, compressed at the level the encoder picks for it, or at
    /// [`MID_BASE`] where it does not compress: the block is always compressed.
    fn resource_block(&self, out: &mut impl Out, r: &[u8]) -> Result<()> {
        let mut resources = self.resources.borrow_mut();
        let c = match resources.entry(key(&[r])) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let c = match self.plan(key(&[r]), &[r])? {
                    Plan::Done(Some(c)) => Some(c),
                    Plan::Done(None) => None,
                    Plan::Level(level, size) => compressed_to(&mut self.cctx(), &[r], level, self.threads, size)?,
                };
                e.insert(match c {
                    Some(c) => c,
                    None => always_compressed(&mut self.cctx(), &[r], MID_BASE.0, self.threads)?,
                })
            }
        };
        block(out, RESOURCE, c);
        Ok(())
    }

    /// About the fewest bytes stream `d` archives to at the level of the encoder, by a sample,
    /// or as soon as it is known to be at most `enough`: for [`Level::Mid`], at the lowest and
    /// the highest zstd levels it picks from (`MID_LOWER`, the last of `MID_HIGHER`: some
    /// data, and data of a few hundred KB, which zstd compresses with parameters of its own,
    /// compress better at lower levels), or as it is where they do not compress it. It
    /// compresses the sample at those levels only, the faster first, where picking the level
    /// compresses it at every level: a bound, for telling whether a formula of `d` may archive
    /// smaller.
    pub fn least(&self, d: &[u8], enough: usize) -> Result<usize> {
        let levels = match self.level {
            Level::Mid => [&MID_LOWER[..], &[MID_HIGHER[MID_HIGHER.len() - 1].0]].concat(),
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

    /// `sample` of the stream whose key is `k` compressed at `level`, as [`Encoder::least`]
    /// compressed it if it did.
    fn trial(&self, k: StreamKey, sample: &[u8], level: i32) -> Result<Option<Vec<u8>>> {
        match self.trials.borrow_mut().remove(&(k, level)) {
            Some(c) => Ok(c),
            None => compressed(&mut self.cctx(), &[sample], level, self.threads),
        }
    }

    /// What to make of `data`, whose key is `k`, at the level of the encoder.
    fn plan(&self, k: StreamKey, data: &[&[u8]]) -> Result<Plan> {
        let n = total(data);
        match self.level {
            Level::Zstd(l) if n <= SAMPLE => Ok(Plan::Done(match self.trials.borrow_mut().remove(&(k, l)) {
                Some(c) => c,
                None => compressed(&mut self.cctx(), data, l, self.threads)?,
            })),
            Level::Zstd(l) => {
                let sample = sampled(data);
                let c = self.trial(k, &sample, l)?.map_or(sample.len(), |c| c.len());
                Ok(Plan::Level(l, scaled(c, sample.len(), n)))
            }
            Level::Mid => {
                let sample = sampled(data);
                let len = |c: &Option<Vec<u8>>| c.as_ref().map_or(sample.len(), Vec::len);
                let probe = self.trial(k, &sample, PROBE)?;
                if len(&probe) >= sample.len() - sample.len() / 32 {
                    return Ok(Plan::Done(None));
                }
                if n < MID_SMALL {
                    return Ok(Plan::Done(compressed(&mut self.cctx(), data, MID_BASE.0, self.threads)?));
                }
                let mut tried = vec![(MID_BASE.0, self.trial(k, &sample, MID_BASE.0)?)];
                for (level, _) in MID_HIGHER {
                    tried.push((level, self.trial(k, &sample, level)?));
                }
                for level in MID_LOWER {
                    tried.push((level, if level == PROBE { probe.clone() } else { self.trial(k, &sample, level)? }));
                }
                let lens: Vec<usize> = tried.iter().map(|(_, c)| len(c)).collect();
                let (higher, lower) = lens[1..].split_at(MID_HIGHER.len());
                let level = mid_pick(sample.len(), lens[0], higher, lower);
                let best = tried.into_iter().find(|t| t.0 == level).expect("a level tried");
                let size = scaled(len(&best.1), sample.len(), n);
                Ok(if sample.len() == n { Plan::Done(best.1) } else { Plan::Level(best.0, size) })
            }
        }
    }

    /// Whether `f` archives smaller than `d` raw. A formula of at most half of the bytes
    /// that `d` is estimated to compress to (by a sample) is taken as smaller without
    /// compressing `d`.
    pub fn smaller(&self, f: &Formula, d: &[u8]) -> Result<bool> {
        self.smaller_of(f, None, d)
    }

    /// [`Encoder::smaller`], with stream `s` of `f` the bytes of `pieces` one after the other
    /// rather than its data (which is not read): a stream yet to be put together, sized as it
    /// will be archived.
    pub fn smaller_with(&self, f: &Formula, s: usize, pieces: &[&[u8]], d: &[u8]) -> Result<bool> {
        self.smaller_of(f, Some((s, pieces)), d)
    }

    fn smaller_of(&self, f: &Formula, stand_in: Option<(usize, &[&[u8]])>, d: &[u8]) -> Result<bool> {
        let n = self.len_with(f, stand_in)?;
        // (`d` was compressed, so a formula of it of at most half of its estimate was not one)
        if let Some(&raw) = self.raw_lens.borrow().get(&key(&[d])) {
            return Ok(n < raw);
        }
        if !self.streams.borrow().contains_key(&key(&[d])) {
            let c = match self.plan(key(&[d]), &[d])? {
                Plan::Done(c) => c,
                Plan::Level(level, estimate) => {
                    if n <= estimate / 2 {
                        return Ok(true);
                    }
                    compressed_to(&mut self.cctx(), &[d], level, self.threads, estimate)?
                }
            };
            self.streams.borrow_mut().insert(key(&[d]), c);
        }
        Ok(n < self.raw_len(d)?)
    }
}

/// The zstd level of [`Level::Mid`] for a sample of `n` bytes, by the bytes it compresses to
/// at [`MID_BASE`], at [`MID_HIGHER`] and at [`MID_LOWER`] (in their order): above the base by
/// the time to compress, else below where smaller by 1/32 at least.
fn mid_pick(n: usize, base: usize, higher: &[usize], lower: &[usize]) -> i32 {
    let cost = |len: usize, speed: f64| len as f64 + MID_RATE * n as f64 / speed;
    let (mut best, mut least) = (MID_BASE.0, cost(base, MID_BASE.1));
    for (&(level, speed), &len) in MID_HIGHER.iter().zip(higher) {
        if cost(len, speed) < least {
            (best, least) = (level, cost(len, speed));
        }
    }
    if best == MID_BASE.0
        && let Some((&level, _)) = MID_LOWER.iter().zip(lower).find(|&(_, &len)| len <= base - base / 32)
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

/// [`checksum`] of `data`.
fn checksum_of(data: &[&[u8]]) -> u64 {
    let mut h = Xxh64::new(0);
    data.iter().for_each(|p| h.update(p));
    h.digest()
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
/// blocks are longer than `data`: for blocks that are always compressed.
fn always_compressed(c: &mut CCtx, data: &[&[u8]], level: i32, threads: usize) -> Result<Vec<u8>> {
    Ok(external(c, data, level, threads, usize::MAX)?.0)
}

/// Zstandard's jobs of multithreaded compression, and how much of the window each job takes
/// over from the data before it (9: all of it). Data is cut in jobs however many the threads,
/// so that it compresses the same on any; this loses 0.1-0.2% against compressing it in one
/// piece, at levels 3 and 4.
const ZSTD_JOB: u32 = 16 << 20;
const ZSTD_OVERLAP: u32 = 9;

/// The compressed external structure of `data` (Zstandard, XXH64), unless the Zstandard
/// blocks would be longer than `data`. It is compressed on `threads` threads, always in
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
    #[cfg(test)]
    tests::COMPRESSED.with(|n| n.set(n.get() + 1));
    let zstd = |e: zstd_safe::ErrorCode| Error::new(format!("zstd: {}", zstd_safe::get_error_name(e)));
    // A context compressed with before keeps its workers and buffers, not its parameters.
    c.reset(ResetDirective::SessionAndParameters).map_err(zstd)?;
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
    c.set_pledged_src_size(Some(len as u64)).map_err(zstd)?;
    let pieces: Vec<&[u8]> = data.iter().copied().filter(|p| !p.is_empty()).collect();
    // the frame, then its checksum
    let bound = zstd_safe::compress_bound(len).saturating_add(8);
    let room = size.saturating_add(size / 8).saturating_add(4096).saturating_add(8);
    let mut frame = Vec::with_capacity(room.min(bound));
    for i in 0..pieces.len().max(1) {
        let (piece, last) = (pieces.get(i).copied().unwrap_or_default(), i + 1 >= pieces.len());
        let end = if last { ZSTD_EndDirective::ZSTD_e_end } else { ZSTD_EndDirective::ZSTD_e_continue };
        let mut input = InBuffer::around(piece);
        loop {
            if frame.len() == frame.capacity() {
                frame.reserve_exact(bound.saturating_sub(frame.len()).max(4096));
            }
            let pos = frame.len();
            let left =
                c.compress_stream2(&mut OutBuffer::around_pos(&mut frame, pos), &mut input, end).map_err(zstd)?;
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

type Decoded<T> = std::result::Result<T, Failure>;

fn invalid<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Invalid(msg.into()))
}

fn resource<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Resource(msg.into()))
}

/// A native implementation standing in for the module blocks naming [`NativeModule::module`].
pub trait NativeModule {
    fn module(&self) -> Module;

    /// The outputs of an application, of `sizes` bytes each: from `inputs`, the resource
    /// blocks given in order and `params`. The decoder checks the outputs against `sizes`.
    fn apply(&self, params: &[u8], resources: &[Resource], inputs: &[&[u8]], sizes: &[u64]) -> Decoded<Vec<Vec<u8>>>;
}

/// `modules` run by `m`, as archives apply them: [`crate::Modules::apply`] for one output, else
/// [`crate::Modules::apply_shared`]. The [`crate::Apply`] that `m` is given has no inputs of its
/// own (they are the evaluated ones).
pub fn natives<'a>(modules: &[Module], m: &'a (impl crate::Modules + 'a)) -> Vec<Box<dyn NativeModule + 'a>> {
    struct Native<'a, M> {
        module: Module,
        m: &'a M,
    }

    impl<M: crate::Modules> NativeModule for Native<'_, M> {
        fn module(&self) -> Module {
            self.module
        }

        fn apply(
            &self,
            params: &[u8],
            resources: &[Resource],
            inputs: &[&[u8]],
            sizes: &[u64],
        ) -> Decoded<Vec<Vec<u8>>> {
            let a = crate::Apply {
                module: self.module,
                resources: resources.to_vec(),
                params: params.to_vec(),
                inputs: Vec::new(),
                len: sizes.iter().sum(),
            };
            match sizes {
                [_] => self.m.apply(&a, inputs).map(|out| vec![out]),
                _ => self.m.apply_shared(&crate::Shared { apply: a, sizes: sizes.to_vec() }, inputs),
            }
            .map_err(|e| Failure::Invalid(e.0))
        }
    }

    modules.iter().map(|&module| Box::new(Native { module, m }) as Box<dyn NativeModule + 'a>).collect()
}

/// The file in archive `c`, with the modules implemented by `modules`.
pub fn decode(c: &[u8], natives: &[Box<dyn NativeModule + '_>]) -> Decoded<Vec<u8>> {
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
    if metadata != 0 {
        return invalid("no metadata block where the footer points");
    }

    // every formula block read first: a non-leaf block is dropped once nothing is to slice it
    let mut used = vec![false; blocks.non_leaf.len()];
    let env = |modules: usize, resources: usize, first| Env {
        first,
        modules: &blocks.modules[..modules],
        resources: &blocks.resources[..resources],
        natives,
    };
    let programs = blocks
        .leaves
        .iter()
        .map(|leaf| match *leaf {
            Leaf::Stored(_) => Ok(None),
            Leaf::Formula { outputs, data, modules, resources, non_leaf } => {
                let env = env(modules, resources, non_leaf);
                read(&blocks.formulas[data], outputs, &env, blocks.non_leaf.len(), &mut used).map(Some)
            }
        })
        .collect::<Decoded<Vec<_>>>()?;
    if used.contains(&false) {
        return invalid("a non-leaf block no formula block references");
    }
    let mut contents = Contents { refs: vec![0; blocks.non_leaf.len()], data: std::mem::take(&mut blocks.non_leaf) };
    for p in programs.iter().flatten() {
        for insn in &p.insns {
            if let &Insn::Slice { block, .. } = insn {
                contents.refs[p.blocks[block]] += 1;
            }
        }
    }

    let mut out = Vec::new();
    for (leaf, program) in std::mem::take(&mut blocks.leaves).into_iter().zip(programs) {
        let leaf = match (leaf, program) {
            (Leaf::Stored(d), _) => d,
            (Leaf::Formula { modules, resources, non_leaf, .. }, Some(p)) => {
                run(p, &env(modules, resources, non_leaf), &mut contents)?
            }
            (Leaf::Formula { .. }, None) => unreachable!(),
        };
        // the first leaf as it is
        if out.is_empty() {
            out = leaf;
        } else {
            out.extend_from_slice(&leaf);
        }
    }
    Ok(out)
}

fn leb(d: &[u8], at: &mut usize) -> Decoded<u64> {
    varint::get(d, at).map_err(|e| Failure::Invalid(e.0.into()))
}

/// The blocks of an archive without a metadata block, decoded.
struct Blocks {
    leaves: Vec<Leaf>,
    /// The formula data of the formula blocks.
    formulas: Vec<Vec<u8>>,
    non_leaf: Vec<Vec<u8>>,
    modules: Vec<Vec<u8>>,
    resources: Vec<Resource>,
}

enum Leaf {
    Stored(Vec<u8>),
    /// The number of outputs and the formula data (by index), with the numbers of module and
    /// resource blocks before it and of non-leaf blocks.
    Formula {
        outputs: u64,
        data: usize,
        modules: usize,
        resources: usize,
        non_leaf: usize,
    },
}

/// The blocks in `d`; `metadata` tells whether the footer points to a metadata block.
fn parse(d: &[u8], size_hint: u64, metadata: bool) -> Decoded<Blocks> {
    let mut b = Blocks {
        leaves: Vec::new(),
        formulas: Vec::new(),
        non_leaf: Vec::new(),
        modules: Vec::new(),
        resources: Vec::new(),
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
        match ty {
            // spacer blocks, of any space: only compressors have to zero it
            0x00..=0x03 => {}
            0x04 if start == 0 => {
                let mut p = 0;
                if leb(data, &mut p)? != size_hint || p != data.len() {
                    return invalid("bad size hint block");
                }
            }
            // data blocks: `001k kcss` (leaf) and `0100 kkss` (non-leaf, always checked)
            0x30..=0x37 | 0x48..=0x4b | 0x50..=0x57 | 0x74..=0x77 => {
                return resource("dictionaries are not supported yet");
            }
            0x20..=0x2f => b.leaves.push(Leaf::Stored(decoded(data, ty & 0x08 != 0, ty & 0x04 != 0)?)),
            0x40..=0x47 => b.non_leaf.push(decoded(data, ty & 0x04 != 0, true)?),
            0x60..=0x63 => b.modules.push(decoded(data, true, true)?),
            0x64..=0x67 => b.resources.push(Resource::new(decoded(data, true, true)?)),
            0x70..=0x73 => {
                let mut p = 0;
                let outputs = leb(data, &mut p)?;
                if outputs == 0 {
                    return invalid("a formula block of no outputs");
                }
                b.formulas.push(decoded(&data[p..], true, true)?);
                b.leaves.push(Leaf::Formula {
                    outputs,
                    data: b.formulas.len() - 1,
                    modules: b.modules.len(),
                    resources: b.resources.len(),
                    non_leaf: b.non_leaf.len(),
                });
            }
            // mirror blocks and undefined non-critical blocks, not read
            _ if !critical(ty) => {}
            0x10..=0x17 if metadata => return resource("metadata blocks are not supported yet"),
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

/// The decoded data of a block's data `d`.
fn decoded(d: &[u8], zstd: bool, checked: bool) -> Decoded<Vec<u8>> {
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
        unzstd(window, blocks, size)?
    } else {
        d[..end].to_vec()
    };
    if checked && checksum(&out).to_le_bytes() != d[end..] {
        return invalid("checksum mismatch");
    }
    Ok(out)
}

/// Zstandard `blocks` of `window` making exactly `size` bytes.
fn unzstd(window: u8, blocks: &[u8], size: u64) -> Decoded<Vec<u8>> {
    let zstd = |e: zstd_safe::ErrorCode| Failure::Invalid(format!("zstd: {}", zstd_safe::get_error_name(e)));
    // the frame: a header, then the blocks (not copied after it)
    let head = [ZSTD_MAGIC[0], ZSTD_MAGIC[1], ZSTD_MAGIC[2], ZSTD_MAGIC[3], 0, window];
    let mut frame = [&head[..], blocks].into_iter();
    let mut dctx = zstd_safe::DCtx::create();
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
    resources: &'a [Resource],
    natives: &'a [Box<dyn NativeModule + 'n>],
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
    Checksum(u64),
}

struct Apply<'a> {
    module: usize,
    resources: Vec<usize>,
    params: &'a [u8],
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
                let _memory_limit = r.num()?;
                let num_inputs = usize::try_from(r.num()?).unwrap_or(usize::MAX);
                let output_sizes = r.nums()?;
                if output_sizes.is_empty() {
                    return invalid("an application without outputs");
                }
                Insn::Apply(Apply { module, resources: res, params, num_inputs, output_sizes })
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

/// A byte sequence on the stack: a slice of a non-leaf block (by its ordinal among all), a
/// literal, or bytes of its own.
enum Value<'a> {
    Slice(usize, Range<usize>),
    Literal(&'a [u8]),
    Bytes(Vec<u8>),
}

/// The non-leaf blocks as formula blocks are evaluated, each with the slices of it yet to be
/// evaluated or on the stack: once there are none, it is dropped.
struct Contents {
    data: Vec<Vec<u8>>,
    refs: Vec<usize>,
}

impl Contents {
    fn get<'v>(&'v self, v: &'v Value) -> &'v [u8] {
        match v {
            Value::Slice(o, r) => &self.data[*o][r.clone()],
            Value::Literal(l) => l,
            Value::Bytes(b) => b,
        }
    }

    /// `v` used up: its bytes, of its own.
    fn take(&mut self, v: Value) -> Vec<u8> {
        match v {
            Value::Slice(o, r) => {
                let out = self.data[o][r].to_vec();
                self.release(o);
                out
            }
            Value::Literal(l) => l.to_vec(),
            Value::Bytes(b) => b,
        }
    }

    /// `vs` used up, once their bytes are no longer needed.
    fn drop_all(&mut self, vs: Vec<Value>) {
        for v in vs {
            if let Value::Slice(o, _) = v {
                self.release(o);
            }
        }
    }

    fn release(&mut self, o: usize) {
        self.refs[o] -= 1;
        if self.refs[o] == 0 {
            self.data[o] = Vec::new();
        }
    }
}

/// The outputs of formula block `p` one after another, with the non-leaf blocks in `contents`.
fn run(p: Program, env: &Env, contents: &mut Contents) -> Decoded<Vec<u8>> {
    // literals and slices where they are
    let mut stack: Vec<Value> = Vec::new();
    for insn in p.insns {
        match insn {
            Insn::Nop => {}
            Insn::Literal(l) => stack.push(Value::Literal(l)),
            Insn::Swap(depth) => {
                let top = stack.len() - 1;
                stack.swap(top, top - depth);
            }
            Insn::Slice { block, start, len } => {
                let o = p.blocks[block];
                let r = usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(s, l)| Some(s..s.checked_add(l)?))
                    .filter(|r| r.end <= contents.data[o].len())
                    .ok_or_else(|| Failure::Invalid("slice out of its block".into()))?;
                stack.push(Value::Slice(o, r));
            }
            Insn::Apply(a) => {
                let inputs = stack.split_off(stack.len() - a.num_inputs);
                let res: Vec<Resource> = a.resources.iter().map(|&i| env.resources[p.resources[i]].clone()).collect();
                let bytes: Vec<&[u8]> = inputs.iter().map(|v| contents.get(v)).collect();
                let outputs = apply(env, p.modules[a.module], &a, &bytes, &res)?;
                contents.drop_all(inputs);
                stack.extend(outputs.into_iter().map(Value::Bytes));
            }
            Insn::Concat(1) => {}
            Insn::Concat(n) => {
                let parts = stack.split_off(stack.len() - n);
                let bytes = parts.iter().map(|v| contents.get(v)).collect::<Vec<_>>().concat();
                contents.drop_all(parts);
                stack.push(Value::Bytes(bytes));
            }
            Insn::Pack(n) => {
                let parts = stack.split_off(stack.len() - n);
                let bytes = packed::pack(&parts.iter().map(|v| contents.get(v)).collect::<Vec<_>>());
                contents.drop_all(parts);
                stack.push(Value::Bytes(bytes));
            }
            Insn::Split(sizes) => {
                let x = stack.pop().unwrap();
                let bytes = contents.get(&x);
                if sizes.iter().try_fold(0u64, |s, &n| s.checked_add(n)) != Some(bytes.len() as u64) {
                    return invalid("a split into sizes not adding up");
                }
                // slices and literals split where they are
                let mut at = 0;
                for &n in &sizes {
                    let r = at..at + n as usize;
                    stack.push(match &x {
                        Value::Slice(o, s) => Value::Slice(*o, s.start + r.start..s.start + r.end),
                        &Value::Literal(l) => Value::Literal(&l[r]),
                        Value::Bytes(b) => Value::Bytes(b[r].to_vec()),
                    });
                    at += n as usize;
                }
                if let Value::Slice(o, _) = x {
                    contents.refs[o] += sizes.len() - 1;
                }
            }
            Insn::Checksum(v) => {
                if checksum(contents.get(stack.last().unwrap())) != v {
                    return invalid("output checksum mismatch");
                }
            }
        }
    }
    if stack.len() == 1 {
        return Ok(contents.take(stack.pop().unwrap()));
    }
    Ok(stack.into_iter().map(|v| contents.take(v)).collect::<Vec<_>>().concat())
}

/// The outputs of application `a` of module block `module`.
fn apply(env: &Env, module: usize, a: &Apply, inputs: &[&[u8]], resources: &[Resource]) -> Decoded<Vec<Vec<u8>>> {
    let w = &env.modules[module];
    let name = native_name(w).ok_or_else(|| Failure::Resource("Wasm modules are not supported yet".into()))?;
    let native = env
        .natives
        .iter()
        .find(|n| n.module().to_string() == name)
        .ok_or_else(|| Failure::Resource(format!("no native implementation of {name}")))?;
    let outputs = native.apply(a.params, resources, inputs, &a.output_sizes)?;
    if outputs.len() != a.output_sizes.len() || outputs.iter().zip(&a.output_sizes).any(|(o, &n)| o.len() as u64 != n) {
        return invalid(format!("{name}: outputs of other sizes than declared"));
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

    const REV: Module = Module { name: "rev", version: 1 };

    /// `rev` reverses the concatenation of its inputs, with parameters 1, 2, 3.
    struct Rev;

    impl NativeModule for Rev {
        fn module(&self) -> Module {
            REV
        }

        fn apply(&self, params: &[u8], resources: &[Resource], inputs: &[&[u8]], _: &[u64]) -> Decoded<Vec<Vec<u8>>> {
            assert_eq!((params, resources.len()), (&[1, 2, 3][..], 0));
            Ok(vec![inputs.concat().into_iter().rev().collect()])
        }
    }

    const CUT: Module = Module { name: "cut", version: 1 };

    /// `cut` cuts the concatenation of its inputs into outputs of the sizes declared.
    struct Cut;

    impl NativeModule for Cut {
        fn module(&self) -> Module {
            CUT
        }

        fn apply(&self, _: &[u8], _: &[Resource], inputs: &[&[u8]], sizes: &[u64]) -> Decoded<Vec<Vec<u8>>> {
            let (all, mut at) = (inputs.concat(), 0);
            let mut next = |n: u64| {
                let a = at.min(all.len());
                at += n as usize;
                all[a..at.min(all.len())].to_vec()
            };
            Ok(sizes.iter().map(|&n| next(n)).collect())
        }
    }

    fn natives() -> Vec<Box<dyn NativeModule>> {
        vec![Box::new(Rev), Box::new(Cut)]
    }

    /// Applications of several outputs: their outputs taken anywhere, by the root or by a later
    /// one, each brought up the stack where it is taken. The archive gives what the formula
    /// does; one whose outputs are not taken once each is not made.
    #[test]
    fn shared_outputs() {
        let mut f = Formula::default();
        let a = f.push("a", b"abcdefgh".to_vec());
        let cut = |inputs| crate::Apply { module: CUT, resources: vec![], params: vec![], inputs, len: 0 };
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

    fn sample() -> Formula {
        let mut f = Formula::default();
        let a = f.push("a", b"hello".to_vec());
        let b = f.push("b", b"xyz".to_vec());
        f.root =
            Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![1, 2, 3], vec![a, b], 8), Node::slice(1, 1, 2)]);
        f
    }

    const SAMPLE: &[u8] = b"hezyxollehyz";

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
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&wasm(REV)], 3, 1).unwrap());
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[data], 3, 1).unwrap());
            streams.iter().for_each(|s| {
                content_block(out, false, &[s], compressed(&mut CCtx::create(), &[s], 3, 1).unwrap().as_deref())
            });
        })
    }

    /// Block types of archive `c`.
    fn types(c: &[u8]) -> Vec<u64> {
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
        f.streams.insert(0, Stream { class: "unused".into(), kind: Kind::Unknown, content: false, data: vec![1; 100] });
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
        const PRE: Module = Module { name: "pre", version: 1 };
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
        let c = encode(&f, None, Level::Mid).unwrap();
        assert_eq!(types(&c), [MODULE, RESOURCE, RESOURCE, FORMULA, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), expect);
        // A resource block is always compressed, even when that makes it longer.
        let noise = Resource::new(aqr_common::rng::noise(1000));
        f.root = Node::apply_with(PRE, vec![noise.clone()], vec![], vec![x], 1003);
        let c = encode(&f, None, Level::Mid).unwrap();
        assert_eq!(types(&c), [MODULE, RESOURCE + 1, FORMULA, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), [&noise[..], b"xyz"].concat());
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
            assert_eq!(decode(&c, &[]).unwrap(), d);
        }
        let mut f = Formula::raw("a", b"0123456789".to_vec());
        f.root = Node::slice(0, 3, 4);
        assert_eq!(decode(&encode(&f, None, Level::Zstd(3)).unwrap(), &[]).unwrap(), b"3456");
        // The leaf block's checksum is the output's: one given must be it.
        assert!(encode(&f, Some(checksum(b"3456")), Level::Zstd(3)).is_ok());
        assert!(encode(&f, Some(checksum(b"0123")), Level::Zstd(3)).is_err());
        // An empty formula is a formula block.
        let c = encode(&Formula::default(), None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [FORMULA]);
        assert_eq!(decode(&c, &[]).unwrap(), b"");
    }

    /// A leaf data block may go without a checksum; a non-leaf one may not. A reserved
    /// compression method is unsupported.
    #[test]
    fn data_block_types() {
        let d = vec![7; 1000];
        let c = compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap().unwrap();
        let unchecked = &c[..c.len() - 8];
        assert_eq!(decode(&archive(|out| block(out, LEAF_RAW & !4, &d)), &[]).unwrap(), d);
        assert_eq!(decode(&archive(|out| block(out, LEAF_ZSTD & !4, unchecked)), &[]).unwrap(), d);
        for ty in [0x38, 0x3c, 0x4c] {
            assert!(matches!(decode(&archive(|out| block(out, ty, &c)), &[]), Err(Failure::Unsupported(_))), "{ty:#x}");
        }
    }

    /// Neither the padding nor the space of a spacer block is read. A block type is the shortest
    /// LEB128; an undefined one is ignored if non-critical (from A to F in its leading digit, padded to
    /// 2 hexadecimal digits), else unsupported, even with an invalid block before it.
    #[test]
    fn block_framing() {
        let decode = |blocks: &[u8]| decode(&archive(|out| out.extend_from_slice(blocks)), &[]);
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
            let r = decode(&archive(|out| out.extend_from_slice(&b)), &[]);
            assert!(matches!(r, Err(Failure::Invalid(_))), "{ty:#x}: {r:?}");
        }
    }

    /// A stream of [`Kind::Compressed`] is stored as it is, at any level, leaf or not.
    #[test]
    fn compressed_kind_stored() {
        let d = vec![7; 100_000];
        for level in [Level::Mid, Level::Zstd(3)] {
            let mut f = Formula::default();
            f.root = f.push_as("a", Kind::Compressed, d.clone());
            let types = |c: &[u8]| types(c).into_iter().map(|t| t & !3).collect::<Vec<_>>();
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [LEAF_RAW]);
            assert_eq!(decode(&c, &[]).unwrap(), d);
            let b = f.push("b", d.clone());
            f.root = Node::concat([f.root.clone(), b]);
            // (the bytes of "b" would be kept stored too: they are those of "a")
            f.streams[1].data[0] = 0;
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [FORMULA, NON_LEAF_RAW, NON_LEAF_ZSTD]);
            assert_eq!(decode(&c, &[]).unwrap(), [&d[..], &f.streams[1].data].concat());
        }
    }

    /// [`Level::Mid`] leaves uncompressed what the lowest level hardly shrinks, and compresses
    /// the rest at 3, or higher where that saves enough for its time.
    #[test]
    fn mid_level() {
        let types_at = |d: &[u8], level| {
            let c = encode(&Formula::raw("a", d.to_vec()), None, level).unwrap();
            assert_eq!(decode(&c, &[]).unwrap(), d);
            (types(&c)[0] & !3, c.len())
        };
        // shrunk at level 1, by less than 1/32 (but more than zstd keeps blocks raw below)
        let mut noise = aqr_common::rng::noise(2 << 20);
        noise.iter_mut().enumerate().filter(|(i, _)| i % 1024 < 32).for_each(|(_, b)| *b = 0);
        // a short stream, a whole one, and a sample
        for d in [&noise[..30000], &noise[..300_000], &noise] {
            assert_eq!(types_at(d, Level::Zstd(3)).0, LEAF_ZSTD, "{}", d.len());
            assert_eq!(types_at(d, Level::Mid).0, LEAF_RAW, "{}", d.len());
        }
        // Text, which level 4 shrinks more than enough for its time.
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
        let (zstd3, mid) = (types_at(&text, Level::Zstd(3)).1, types_at(&text, Level::Mid).1);
        assert!(mid < zstd3 - zstd3 / 32, "{mid} vs {zstd3}");
        // short, at level 3
        let short = Formula::raw("a", text[..30000].to_vec());
        assert_eq!(encode(&short, None, Level::Mid).unwrap(), encode(&short, None, Level::Zstd(3)).unwrap());
        // and so are module and formula blocks
        assert_eq!(encode(&sample(), None, Level::Mid).unwrap(), encode(&sample(), None, Level::Zstd(3)).unwrap());
    }

    /// A stream larger than Zstandard's jobs is archived the same on any number of threads.
    #[test]
    fn threads() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
        let mut d = Vec::new();
        let mut rng = aqr_common::rng::Rng::new(1);
        while d.len() < 40 << 20 {
            let at = rng.below(text.len() as u64 - 4096) as usize;
            d.extend_from_slice(&text[at..at + 4096]);
        }
        let f = Formula::raw("a", d.clone());
        for level in [Level::Mid, Level::Zstd(3)] {
            let one = Encoder::new(level).encode(&f, None).unwrap();
            assert_eq!(Encoder::with_threads(level, 4).encode(&f, None).unwrap(), one);
            assert_eq!(decode(&one, &[]).unwrap(), d);
        }
    }

    /// A compressed block decodes into as much memory as it declares (and a byte to tell a
    /// longer one), not what growing as it comes would take.
    #[test]
    fn decoded_at_size() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
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
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
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

    /// A stream in pieces compresses as it does in one, the pieces ending anywhere (at the end
    /// of a job among them), and is sized as it is archived.
    #[test]
    fn pieces() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
        let mut d = Vec::new();
        let mut rng = aqr_common::rng::Rng::new(1);
        while d.len() < 2 * ZSTD_JOB as usize {
            let at = rng.below(text.len() as u64 - 4096) as usize;
            d.extend_from_slice(&text[at..at + 4096]);
        }
        d.truncate(2 * ZSTD_JOB as usize);
        let job = ZSTD_JOB as usize;
        let one = compressed(&mut CCtx::create(), &[&d], 1, 4).unwrap();
        // as Zstandard compresses it in one go
        let mut z = zstd::bulk::Compressor::new(1).unwrap();
        for p in [
            CParameter::ContentSizeFlag(false),
            CParameter::ChecksumFlag(false),
            CParameter::DictIdFlag(false),
            CParameter::NbWorkers(4),
            CParameter::JobSize(ZSTD_JOB),
            CParameter::OverlapSizeLog(ZSTD_OVERLAP),
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
                Stream { class: "b".into(), kind: Kind::Unknown, content: false, data: d.to_vec() },
            ],
            ..f.clone()
        };
        let pieces = [&d[..100], &d[100..2 << 20], &d[2 << 20..]];
        let raw = [b"head".as_slice(), d].concat();
        for level in [Level::Mid, Level::Zstd(3)] {
            let enc = Encoder::new(level);
            assert_eq!(enc.len_with(&f, Some((1, &pieces))).unwrap(), Encoder::new(level).len(&whole).unwrap());
            assert_eq!(
                enc.smaller_with(&f, 1, &pieces, &raw).unwrap(),
                Encoder::new(level).smaller(&whole, &raw).unwrap()
            );
            // the stream compressed then is the one archived
            enc.forget(&raw);
            assert_eq!(enc.encode(&whole, None).unwrap(), Encoder::new(level).encode(&whole, None).unwrap());
            assert_eq!(enc.streams.borrow().len(), 2);
        }
    }

    /// Raw data that a formula was found smaller than, then forgotten, is not compressed again
    /// to be compared with another formula of it (a scan's around the file), and is compared
    /// as a new encoder would.
    #[test]
    fn smaller_after_forget() {
        let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/index.bs")).unwrap();
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
            for level in [Level::Mid, Level::Zstd(3)] {
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

    /// Above the base by the time to compress; below where clearly smaller, as a sample may
    /// be off by a little.
    #[test]
    fn mid_picks() {
        // nothing better
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[310_000]), 3);
        // level 4, 1% smaller for 0.07% of time
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 297_000, 297_000], &[310_000]), 4);
        // level 6, 1% smaller than 4 for 1.6% of time: not enough
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 294_000], &[310_000]), 4);
        // level 9, 12% smaller than 4 for 2.8% of time
        assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 260_000], &[310_000]), 9);
        // level 1, by 0.7% (as of a sample of 19tech_4.psd, 8.7% larger whole) and by 10%
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[298_000]), 3);
        assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[270_000]), 1);
    }

    #[test]
    fn native_names() {
        assert_eq!(native_name(&wasm(REV)), Some("rev@1"));
        // after another custom section
        let other = [&WASM_HEADER[..], &[0, 3, 0, 9, 9], &wasm(REV)[WASM_HEADER.len()..]].concat();
        assert_eq!(native_name(&other), Some("rev@1"));
        assert_eq!(native_name(&WASM_HEADER), None);
        assert_eq!(native_name(&wasm(REV)[..20]), None);
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
        assert_eq!(decode(&of(2), &[]).unwrap(), b"abcd");
        for n in [0, 1, 3] {
            assert!(matches!(decode(&of(n), &[]), Err(Failure::Invalid(_))), "{n} outputs");
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
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&wasm(REV)], 3, 1).unwrap());
            for d in [&first[..], &second] {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        });
        assert_eq!(decode(&c, &natives()).unwrap(), b"zyyxzxyz");
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
                // (blocks framed anew by a changed size, one of them using a dictionary)
                Err(Failure::Resource(m)) if m == "dictionaries are not supported yet" => failed += 1,
                r => panic!("byte {i}: {r:?}"),
            }
        }
        assert!(same < 8 && failed + same == c.len(), "{same} the same");
        for n in 1..c.len() {
            assert!(matches!(decode(&c[..n], &natives()), Err(Failure::Invalid(_))), "{n} bytes");
        }
        assert_eq!(decode(&c, &[]), resource("no native implementation of rev@1"));
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
        assert_eq!(apply(2, &[4]), invalid("rev@1: outputs of other sizes than declared"));
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
