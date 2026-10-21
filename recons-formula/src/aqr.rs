//! A formula as an Aqueous archive (`spec/index.bs`) of a single file, without a metadata
//! block.
//!
//! [`encode`] lays the formula out as blocks: a module block for each module, the formula
//! block, and a non-leaf block for each stream it slices, in the order of the streams. A
//! formula of a single slice is a leaf block of its own.
//!
//! Until modules are WebAssembly (the spec's "Module execution" is tentative), a module block
//! holds a Wasm module of custom sections only, one of them ([`NATIVE_SECTION`]) naming the
//! module (`name@version`), and a [`NativeModule`] stands in for it. Its applications declare
//! what a module reading its inputs whole and committing its output at the end needs: windows
//! of the whole inputs and output, and no memory limit ([`MEMORY_LIMIT`]).
//!
//! [`decode`] evaluates such archives, and any other without a metadata block, a dictionary
//! or an encryption.

use std::fmt;

use recons_common::varint;
use xxhash_rust::xxh64::xxh64;
use zstd::zstd_safe::{self, CParameter, InBuffer, OutBuffer};

use crate::{Error, Formula, Module, Node, Result};

pub const HEADER_MAGIC: [u8; 6] = [0xf8, 0x04, 0x03, 0x6a, 0xd6, 0xbe];
pub const FOOTER_MAGIC: [u8; 6] = [0xfd, 0xaa, 0xae, 0xde, 0x85, 0x12];
const ENCRYPTED_HEADER_MAGIC: [u8; 8] = [0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a, 0x01, 0x00];

/// The name of the custom section naming the native implementation of a module block.
pub const NATIVE_SECTION: &str = "aqueous-native";
/// The memory limit of every application, as no module declares one yet.
pub const MEMORY_LIMIT: u64 = u64::MAX;

// Block types with a u8 size field, all with checksums; `+ 0..=3` widens the size field.
const LEAF_ZSTD: u64 = 0x24;
const NON_LEAF_ZSTD: u64 = 0x34;
const LEAF_RAW: u64 = 0x44;
const NON_LEAF_RAW: u64 = 0x4c;
const MODULE: u64 = 0x60;
const FORMULA: u64 = 0x70;

const NOP: u8 = 0x00;
const LITERAL: u8 = 0x01;
const SWAP: u8 = 0x0f;
const SLICE: u8 = 0x10;
const APPLY: u8 = 0x11;
const CONCAT: u8 = 0x12;
const CHECKSUM: u8 = 0x20;

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const WASM_HEADER: [u8; 8] = *b"\0asm\x01\0\0\0";

/// The checksum of the output, as [`encode`] takes it.
pub fn checksum(d: &[u8]) -> u64 {
    xxh64(d, 0)
}

/// `f` as an archive, its blocks compressed at zstd `level`, with `checksum` of the output
/// ([`checksum`]) if given (a leaf block checksums itself). Fails on slices out of their
/// stream.
pub fn encode(f: &Formula, checksum: Option<u64>, level: i32) -> Result<Vec<u8>> {
    let mut slices = Ok(());
    f.root.visit(&mut |n| {
        if let &Node::Slice { stream, start, len } = n
            && f.streams.get(stream).is_none_or(|s| start.checked_add(len).is_none_or(|e| e > s.data.len() as u64))
        {
            slices = Err(Error::new(format!("slice [{start}, +{len}) out of stream #{stream}")));
        }
    });
    slices?;

    let mut blocks = Vec::new();
    if let &Node::Slice { stream, start, len } = &f.root {
        content_block(&mut blocks, true, &f.streams[stream].data[start as usize..][..len as usize], level)?;
    } else {
        // the non-leaf block of each stream sliced, by its ordinal
        let mut ordinals = vec![None; f.streams.len()];
        f.root.visit(&mut |n| {
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
        let mut modules: Vec<Module> = Vec::new();
        for a in f.applies() {
            if !modules.contains(&a.module) {
                modules.push(a.module);
            }
        }

        let mut data = Vec::new();
        varint::put(&mut data, sliced.len() as u64);
        sliced.iter().for_each(|_| varint::put(&mut data, 0));
        varint::put(&mut data, 0);
        varint::put(&mut data, modules.len() as u64);
        (0..modules.len()).for_each(|i| varint::put(&mut data, i as u64));
        instructions(&f.root, &ordinals, &modules, &mut data);
        if let Some(c) = checksum {
            data.push(CHECKSUM);
            data.extend_from_slice(&c.to_le_bytes());
        }

        for &m in &modules {
            block(&mut blocks, MODULE, &padded(|pad| wasm(m, pad), level)?);
        }
        block(
            &mut blocks,
            FORMULA,
            &padded(
                |pad| {
                    let mut d = data.clone();
                    d.resize(d.len() + pad, NOP);
                    d
                },
                level,
            )?,
        );
        for s in sliced {
            content_block(&mut blocks, false, &f.streams[s].data, level)?;
        }
    }

    let mut out = Vec::with_capacity(blocks.len() + 32);
    out.extend_from_slice(&HEADER_MAGIC);
    out.extend_from_slice(&blocks);
    let mut footer = Vec::new();
    varint::put(&mut footer, blocks.len() as u64);
    varint::put(&mut footer, 0);
    out.extend_from_slice(&FOOTER_MAGIC);
    out.extend_from_slice(&(footer.len() as u16).to_le_bytes());
    out.extend_from_slice(&footer);
    out.extend_from_slice(&(footer.len() as u16).to_le_bytes());
    Ok(out)
}

/// The instructions pushing node `n`, its streams' blocks at `ordinals` and its modules at
/// their indices in `modules`.
fn instructions(n: &Node, ordinals: &[Option<u64>], modules: &[Module], out: &mut Vec<u8>) {
    match n {
        &Node::Slice { stream, start, len } => {
            out.push(SLICE);
            varint::put(out, ordinals[stream].unwrap());
            varint::put(out, start);
            varint::put(out, len);
        }
        Node::Concat(ns) => {
            ns.iter().for_each(|n| instructions(n, ordinals, modules, out));
            out.push(CONCAT);
            varint::put(out, ns.len() as u64);
        }
        Node::Apply(a) => {
            a.inputs.iter().for_each(|n| instructions(n, ordinals, modules, out));
            out.push(APPLY);
            varint::put(out, modules.iter().position(|&m| m == a.module).unwrap() as u64);
            varint::put(out, 0);
            varint::put(out, a.params.len() as u64);
            out.extend_from_slice(&a.params);
            varint::put(out, MEMORY_LIMIT);
            varint::put(out, a.inputs.len() as u64);
            a.inputs.iter().for_each(|n| varint::put(out, n.len()));
            varint::put(out, 1);
            varint::put(out, a.len);
            varint::put(out, a.len);
        }
    }
}

/// Bytes of the instructions pushing node `n` at most, wherever the streams of its formula are
/// among others (up to 2^21 of them, and 2^14 modules): the bounds of formulas put together
/// ([`Formula::embed`]) add up, with the nodes around them.
pub fn description_bound(n: &Node) -> usize {
    match n {
        &Node::Slice { start, len, .. } => 1 + 3 + varint::len(start) + varint::len(len),
        Node::Concat(ns) => ns.iter().map(description_bound).sum::<usize>() + 1 + varint::len(ns.len() as u64),
        Node::Apply(a) => {
            let inputs: usize = a.inputs.iter().map(|n| description_bound(n) + varint::len(n.len())).sum();
            inputs
                + 1
                + 2
                + 1
                + varint::len(a.params.len() as u64)
                + a.params.len()
                + varint::len(MEMORY_LIMIT)
                + varint::len(a.inputs.len() as u64)
                + 1
                + 2 * varint::len(a.len)
        }
    }
}

/// A Wasm module of custom sections only, naming `m` in [`NATIVE_SECTION`], with `pad` more
/// bytes in another section.
fn wasm(m: Module, pad: usize) -> Vec<u8> {
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
    if pad > 0 {
        custom(&mut w, "", &vec![0; pad]);
    }
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

/// Appends a block of `ty` (with a u8 size field) and `data`, widening the size field as
/// needed and padding the block to an even length.
fn block(out: &mut Vec<u8>, ty: u64, data: &[u8]) {
    let start = out.len();
    let ss = match data.len() as u64 {
        0..0x100 => 0,
        0x100..0x1_0000 => 1,
        0x1_0000..0x1_0000_0000 => 2,
        _ => 3,
    };
    varint::put(out, ty + ss);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes()[..1 << ss]);
    out.extend_from_slice(data);
    if (out.len() - start) % 2 == 1 {
        out.push(0);
    }
}

/// Appends a compressed or uncompressed (leaf or non-leaf) block of `data`, whichever the spec
/// allows.
fn content_block(out: &mut Vec<u8>, leaf: bool, data: &[u8], level: i32) -> Result<()> {
    match compressed(data, level)? {
        Some(c) => block(out, if leaf { LEAF_ZSTD } else { NON_LEAF_ZSTD }, &c),
        None => {
            let mut u = data.to_vec();
            u.extend_from_slice(&checksum(data).to_le_bytes());
            block(out, if leaf { LEAF_RAW } else { NON_LEAF_RAW }, &u);
        }
    }
    Ok(())
}

/// The compressed external structure of `make(pad)` with the least `pad` tried (0, 16, 32,
/// ...) that compresses, for blocks that are always compressed.
fn padded(make: impl Fn(usize) -> Vec<u8>, level: i32) -> Result<Vec<u8>> {
    let mut pad = 0;
    loop {
        if let Some(c) = compressed(&make(pad), level)? {
            return Ok(c);
        }
        pad = (pad * 2).max(16);
    }
}

/// The compressed external structure of `data` (Zstandard, XXH64), unless the Zstandard
/// blocks would be longer than `data`.
fn compressed(data: &[u8], level: i32) -> Result<Option<Vec<u8>>> {
    let io = |e: std::io::Error| Error::new(format!("zstd: {e}"));
    let mut c = zstd::bulk::Compressor::new(level).map_err(io)?;
    for p in [CParameter::ContentSizeFlag(false), CParameter::ChecksumFlag(false), CParameter::DictIdFlag(false)] {
        c.set_parameter(p).map_err(io)?;
    }
    let frame = c.compress(data).map_err(io)?;
    // a frame header of no flags, then the window descriptor
    let [m0, m1, m2, m3, 0, window, blocks @ ..] = &frame[..] else {
        return Err(Error::new("zstd: unexpected frame header"));
    };
    assert_eq!([*m0, *m1, *m2, *m3], ZSTD_MAGIC);
    if blocks.len() > data.len() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(blocks.len() + 20);
    varint::put(&mut out, (data.len() - blocks.len()) as u64);
    out.push(*window);
    out.extend_from_slice(blocks);
    out.extend_from_slice(&checksum(data).to_le_bytes());
    Ok(Some(out))
}

/// Why an archive does not decode, as the spec classifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The archive violates the spec (or a module fails on its inputs).
    Invalid(String),
    /// The archive exceeds what this decoder supports: a limit, or a feature not implemented
    /// yet (a module without a native implementation among them).
    Resource(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Invalid(m) => write!(f, "invalid archive: {m}"),
            Failure::Resource(m) => write!(f, "resource failure: {m}"),
        }
    }
}

impl std::error::Error for Failure {}

type Decoded<T> = std::result::Result<T, Failure>;

fn invalid<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Invalid(msg.into()))
}

fn unsupported<T>(msg: impl Into<String>) -> Decoded<T> {
    Err(Failure::Resource(msg.into()))
}

/// A native implementation standing in for the module blocks naming [`NativeModule::module`].
pub trait NativeModule {
    fn module(&self) -> Module;

    /// The outputs of an application, of `sizes` bytes each: from `inputs`, the resource
    /// blocks given in order and `params`. The decoder checks the outputs against `sizes`.
    fn apply(&self, params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], sizes: &[u64]) -> Decoded<Vec<Vec<u8>>>;
}

/// The file in archive `c`, with the modules implemented by `modules`.
pub fn decode(c: &[u8], modules: &[Box<dyn NativeModule + '_>]) -> Decoded<Vec<u8>> {
    if c.starts_with(&ENCRYPTED_HEADER_MAGIC) {
        return unsupported("encrypted archives are not supported yet");
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
    let blocks = parse(&c[HEADER_MAGIC.len()..archive_end], size_hint, metadata != 0)?;
    if metadata != 0 {
        return invalid("no metadata block where the footer points");
    }

    let mut used = vec![false; blocks.non_leaf.len()];
    let mut out = Vec::new();
    for leaf in &blocks.leaves {
        match leaf {
            Leaf::Stored(d) => out.extend_from_slice(d),
            Leaf::Formula { data, modules: m, resources: r, non_leaf } => {
                let env = Env {
                    non_leaf: &blocks.non_leaf,
                    first: *non_leaf,
                    modules: &blocks.modules[..*m],
                    resources: &blocks.resources[..*r],
                    natives: modules,
                };
                out.extend_from_slice(&evaluate(data, &env, &mut used)?);
            }
        }
    }
    if used.contains(&false) {
        return invalid("a non-leaf block no formula block references");
    }
    Ok(out)
}

fn leb(d: &[u8], at: &mut usize) -> Decoded<u64> {
    varint::get(d, at).map_err(|e| Failure::Invalid(e.0.into()))
}

/// The blocks of an archive without a metadata block, decoded.
struct Blocks {
    leaves: Vec<Leaf>,
    non_leaf: Vec<Vec<u8>>,
    modules: Vec<Vec<u8>>,
    resources: Vec<Vec<u8>>,
}

enum Leaf {
    Stored(Vec<u8>),
    /// The formula data, with the numbers of module and resource blocks before it and of
    /// non-leaf blocks.
    Formula {
        data: Vec<u8>,
        modules: usize,
        resources: usize,
        non_leaf: usize,
    },
}

/// The blocks in `d`; `metadata` tells whether the footer points to a metadata block.
fn parse(d: &[u8], size_hint: u64, metadata: bool) -> Decoded<Blocks> {
    let mut b = Blocks { leaves: Vec::new(), non_leaf: Vec::new(), modules: Vec::new(), resources: Vec::new() };
    let mut at = 0;
    while at < d.len() {
        let start = at;
        let ty = leb(d, &mut at)?;
        let width = 1 << (ty % 4);
        let size = d.get(at..at + width).ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += width;
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        let data = usize::try_from(size)
            .ok()
            .and_then(|n| d.get(at..at.checked_add(n)?))
            .ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += data.len();
        if (at - start) % 2 == 1 {
            if d.get(at) != Some(&0) {
                return invalid("bad block padding");
            }
            at += 1;
        }
        match ty {
            0x00..=0x03 if data.iter().all(|&b| b == 0) => {}
            0x04 if start == 0 => {
                let mut p = 0;
                if leb(data, &mut p)? != size_hint || p != data.len() {
                    return invalid("bad size hint block");
                }
            }
            0x20..=0x3f if ty & 0x08 != 0 => return unsupported("dictionaries are not supported yet"),
            0x50..=0x57 | 0x74..=0x77 => return unsupported("dictionaries are not supported yet"),
            0x20..=0x3f => {
                let content = decoded(data, true, ty & 0x04 != 0)?;
                if ty & 0x10 == 0 {
                    b.leaves.push(Leaf::Stored(content));
                } else {
                    b.non_leaf.push(content);
                }
            }
            0x40..=0x4f => {
                let content = decoded(data, false, ty & 0x04 != 0)?;
                if ty & 0x08 == 0 {
                    b.leaves.push(Leaf::Stored(content));
                } else {
                    b.non_leaf.push(content);
                }
            }
            0x60..=0x63 => b.modules.push(decoded(data, true, true)?),
            0x64..=0x67 => b.resources.push(decoded(data, true, true)?),
            0x70..=0x73 => b.leaves.push(Leaf::Formula {
                data: decoded(data, true, true)?,
                modules: b.modules.len(),
                resources: b.resources.len(),
                non_leaf: b.non_leaf.len(),
            }),
            // mirror blocks, not read
            0xc10..=0xc17 | 0xc50..=0xc57 | 0xc60..=0xc67 | 0xc70..=0xc77 => {}
            0x10..=0x17 if metadata => return unsupported("metadata blocks are not supported yet"),
            0x10..=0x17 => return invalid("a metadata block, but none in the footer"),
            _ => return invalid(format!("bad block of type {ty:#x}")),
        }
    }
    Ok(b)
}

/// The decoded data of a block's data `d`.
fn decoded(d: &[u8], zstd: bool, checked: bool) -> Decoded<Vec<u8>> {
    let end = d.len().checked_sub(if checked { 8 } else { 0 }).ok_or_else(|| Failure::Invalid("truncated".into()))?;
    let out = if zstd {
        let mut at = 0;
        let delta = leb(d, &mut at)?;
        let &window = d.get(at).filter(|_| at < end).ok_or_else(|| Failure::Invalid("truncated".into()))?;
        let blocks = &d[at + 1..end];
        let size = (blocks.len() as u64).checked_add(delta).ok_or_else(|| Failure::Invalid("bad size".into()))?;
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
    let mut frame = Vec::with_capacity(blocks.len() + 6);
    frame.extend_from_slice(&ZSTD_MAGIC);
    frame.extend_from_slice(&[0, window]);
    frame.extend_from_slice(blocks);
    let mut dctx = zstd_safe::DCtx::create();
    let mut input = InBuffer::around(&frame);
    let mut out: Vec<u8> = Vec::new();
    // up to one byte more than `size`, to tell a longer output
    let limit = size.saturating_add(1);
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
        if !written && input.pos() == before {
            return invalid("truncated zstd data");
        }
    }
    if input.pos() != frame.len() || out.len() as u64 != size {
        return invalid("zstd data of another size than declared");
    }
    Ok(out)
}

/// What a formula block refers to.
struct Env<'a, 'n> {
    non_leaf: &'a [Vec<u8>],
    /// The ordinal of the first non-leaf block after the formula block.
    first: usize,
    modules: &'a [Vec<u8>],
    resources: &'a [Vec<u8>],
    natives: &'a [Box<dyn NativeModule + 'n>],
}

enum Insn<'a> {
    Nop,
    Literal(&'a [u8]),
    Swap(usize),
    Slice { block: usize, start: u64, len: u64 },
    Apply(Apply<'a>),
    Concat(usize),
    Checksum(u64),
}

struct Apply<'a> {
    module: usize,
    resources: Vec<usize>,
    params: &'a [u8],
    input_windows: Vec<u64>,
    output_sizes: Vec<u64>,
    output_windows: Vec<u64>,
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

/// The output of a formula block of formula data `d`, marking the non-leaf blocks it refers to
/// in `used`.
fn evaluate(d: &[u8], env: &Env, used: &mut [bool]) -> Decoded<Vec<u8>> {
    let mut r = Reader { d, at: 0 };
    // the non-leaf blocks referenced, by their ordinals among all
    let (mut blocks, mut next) = (Vec::new(), env.first);
    for delta in r.nums()? {
        let o = usize::try_from(delta)
            .ok()
            .and_then(|d| next.checked_add(d))
            .filter(|&o| o < env.non_leaf.len())
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
                let input_windows = r.nums()?;
                let output_sizes = r.nums()?;
                let output_windows = (0..output_sizes.len()).map(|_| r.num()).collect::<Decoded<Vec<_>>>()?;
                if output_sizes.is_empty() {
                    return invalid("an application without outputs");
                }
                Insn::Apply(Apply { module, resources: res, params, input_windows, output_sizes, output_windows })
            }
            CONCAT => Insn::Concat(r.index(depth + 1)?),
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
                pop(&mut depth, a.input_windows.len())?;
                depth += a.output_sizes.len();
            }
            &Insn::Concat(n) => {
                pop(&mut depth, n)?;
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
    if depth != 1 {
        return invalid(format!("{depth} outputs, not one"));
    }
    blocks.iter().for_each(|&o| used[o] = true);

    let mut stack: Vec<Vec<u8>> = Vec::new();
    for insn in insns {
        match insn {
            Insn::Nop => {}
            Insn::Literal(l) => stack.push(l.to_vec()),
            Insn::Swap(depth) => {
                let top = stack.len() - 1;
                stack.swap(top, top - depth);
            }
            Insn::Slice { block, start, len } => {
                let c = &env.non_leaf[blocks[block]];
                let s = usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(len).ok())
                    .and_then(|(s, l)| c.get(s..s.checked_add(l)?))
                    .ok_or_else(|| Failure::Invalid("slice out of its block".into()))?;
                stack.push(s.to_vec());
            }
            Insn::Apply(a) => {
                let inputs = stack.split_off(stack.len() - a.input_windows.len());
                let res: Vec<&[u8]> = a.resources.iter().map(|&i| &env.resources[resources[i]][..]).collect();
                stack.extend(apply(env, modules[a.module], &a, &inputs, &res)?);
            }
            Insn::Concat(n) => {
                let parts = stack.split_off(stack.len() - n);
                stack.push(parts.concat());
            }
            Insn::Checksum(v) => {
                if checksum(stack.last().unwrap()) != v {
                    return invalid("output checksum mismatch");
                }
            }
        }
    }
    Ok(stack.pop().unwrap())
}

/// The outputs of application `a` of module block `module`.
fn apply(env: &Env, module: usize, a: &Apply, inputs: &[Vec<u8>], resources: &[&[u8]]) -> Decoded<Vec<Vec<u8>>> {
    let w = &env.modules[module];
    let name = native_name(w).ok_or_else(|| Failure::Resource("Wasm modules are not supported yet".into()))?;
    let native = env
        .natives
        .iter()
        .find(|n| n.module().to_string() == name)
        .ok_or_else(|| Failure::Resource(format!("no native implementation of {name}")))?;
    // only one output for now, which a native module commits at the end
    let &[size] = &a.output_sizes[..] else {
        return invalid(format!("{name}: {} outputs declared", a.output_sizes.len()));
    };
    // a native module reads its inputs whole, which the windows have to hold
    if inputs.iter().zip(&a.input_windows).any(|(i, &w)| (i.len() as u64) > w) || a.output_windows[0] < size {
        return invalid(format!("{name}: a window smaller than its data"));
    }
    let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
    let outputs = native.apply(a.params, resources, &inputs, &a.output_sizes)?;
    if outputs.len() != 1 || outputs[0].len() as u64 != size {
        return invalid(format!("{name}: outputs of other sizes than declared"));
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stream;

    const REV: Module = Module { name: "rev", version: 1 };

    /// `rev` reverses the concatenation of its inputs, with parameters 1, 2, 3.
    struct Rev;

    impl NativeModule for Rev {
        fn module(&self) -> Module {
            REV
        }

        fn apply(&self, params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], _: &[u64]) -> Decoded<Vec<Vec<u8>>> {
            assert_eq!((params, resources.len()), (&[1, 2, 3][..], 0));
            Ok(vec![inputs.concat().into_iter().rev().collect()])
        }
    }

    fn natives() -> Vec<Box<dyn NativeModule>> {
        vec![Box::new(Rev)]
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

    /// An archive of a formula block of `data` (padded to compress), after a `rev` module block and
    /// before non-leaf blocks of `streams`.
    fn formula(data: &[u8], streams: &[&[u8]]) -> Vec<u8> {
        archive(|out| {
            block(out, MODULE, &padded(|pad| wasm(REV, pad), 3).unwrap());
            block(out, FORMULA, &padded(|pad| [data, &vec![NOP; pad]].concat(), 3).unwrap());
            streams.iter().for_each(|s| content_block(out, false, s, 3).unwrap());
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
            let c = encode(&f, sum, 3).unwrap();
            assert_eq!(c[..6], HEADER_MAGIC);
            assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
            assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        }
        // Streams without slices have no blocks.
        let mut f = sample();
        f.streams.insert(0, Stream { class: "unused".into(), data: vec![1; 100] });
        f.root.slices_mut(&mut |s, _, _| *s += 1);
        let c = encode(&f, None, 3).unwrap();
        assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
        // A module named twice has one block.
        let mut f = sample();
        let Node::Concat(ns) = &mut f.root else { unreachable!() };
        ns.push(ns[1].clone());
        let c = encode(&f, None, 3).unwrap();
        assert_eq!(types(&c), [MODULE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), [SAMPLE, b"zyxolleh"].concat());
    }

    /// A formula of a single slice is a leaf block, compressed if that is smaller.
    #[test]
    fn leaf() {
        for (d, ty) in [(vec![], LEAF_RAW), (b"hello".to_vec(), LEAF_RAW), (vec![7; 100_000], LEAF_ZSTD)] {
            let c = encode(&Formula::raw("a", d.clone()), None, 3).unwrap();
            assert_eq!(types(&c), [ty]);
            assert_eq!(decode(&c, &[]).unwrap(), d);
        }
        let mut f = Formula::raw("a", b"0123456789".to_vec());
        f.root = Node::slice(0, 3, 4);
        assert_eq!(decode(&encode(&f, None, 3).unwrap(), &[]).unwrap(), b"3456");
        // An empty formula is a formula block.
        let c = encode(&Formula::default(), None, 3).unwrap();
        assert_eq!(types(&c), [FORMULA]);
        assert_eq!(decode(&c, &[]).unwrap(), b"");
    }

    #[test]
    fn native_names() {
        assert_eq!(native_name(&wasm(REV, 0)), Some("rev@1"));
        assert_eq!(native_name(&wasm(REV, 100)), Some("rev@1"));
        assert_eq!(native_name(&WASM_HEADER), None);
        assert_eq!(native_name(&wasm(REV, 0)[..20]), None);
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
    fn decode_errors() {
        let c = encode(&sample(), Some(checksum(SAMPLE)), 3).unwrap();
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
        assert_eq!(decode(&c, &[]), unsupported("no native implementation of rev@1"));
        // the output checksum
        let bad = encode(&sample(), Some(checksum(SAMPLE) ^ 1), 3).unwrap();
        assert_eq!(decode(&bad, &natives()), invalid("output checksum mismatch"));
    }

    /// Applications by hand: the declared sizes and windows.
    #[test]
    fn applications() {
        let apply = |windows: [u64; 2], size: u64, window: u64| {
            let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY, 0, 0, 3, 1, 2, 3];
            varint::put(&mut d, MEMORY_LIMIT);
            d.extend_from_slice(&[2, windows[0] as u8, windows[1] as u8, 1, size as u8, window as u8]);
            decode(&formula(&d, &[b"abc"]), &natives())
        };
        assert_eq!(apply([3, 2], 5, 5).unwrap(), b"cbcba");
        assert_eq!(apply([8, 8], 5, 8).unwrap(), b"cbcba");
        assert_eq!(apply([2, 2], 5, 5), invalid("rev@1: a window smaller than its data"));
        assert_eq!(apply([3, 2], 5, 4), invalid("rev@1: a window smaller than its data"));
        assert_eq!(apply([3, 2], 4, 4), invalid("rev@1: outputs of other sizes than declared"));
    }

    #[test]
    fn bound() {
        let desc = |f: &Formula| {
            let ordinals: Vec<_> = (0..f.streams.len() as u64).map(Some).collect();
            let mut out = Vec::new();
            instructions(&f.root, &ordinals, &[REV], &mut out);
            out.len()
        };
        let f = sample();
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
