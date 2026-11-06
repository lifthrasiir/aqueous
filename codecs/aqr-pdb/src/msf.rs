//! The MSF container of a PDB file (FORMAT.md §2): blocks of a fixed size, a directory listing
//! the blocks of each stream. The file is rebuilt from its streams one after another, the
//! layout (the superblock's fields, the streams' sizes and their blocks as deltas from the block
//! after the previous) and the rest of the blocks' bytes in their order (the free page maps,
//! free blocks, what follows the streams' data in their last blocks).

use aqr_common::varint;

use crate::{Result, corrupt};

/// The magic of an MSF 7.00 file.
pub const MAGIC: &[u8; 32] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0";

/// The inputs.
pub const INPUT_NAMES: [&str; 3] = ["streams", "layout", "rest"];

/// Whether `d` starts as an MSF 7.00 file.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC)
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

/// The bytes of the file as the superblock's fields tell: its blocks.
pub fn extent(d: &[u8]) -> Option<usize> {
    if !starts(d) {
        return None;
    }
    let (bs, nb) = (u32le(d, 32)? as usize, u32le(d, 40)? as usize);
    bs.checked_mul(nb)
}

/// The layout of a file: its superblock's fields and its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layout {
    block_size: u32,
    fpm: u32,
    blocks: u32,
    unknown: u32,
    /// The block holding the list of the directory's blocks.
    map: u32,
    /// The directory's blocks.
    dir: Vec<u32>,
    /// Each stream's size (`u32::MAX` for a stream of none).
    sizes: Vec<u32>,
    /// Where each stream's blocks start in `lists`, and where the last's end.
    starts: Vec<usize>,
    /// The streams' blocks, one after another.
    lists: Vec<u32>,
}

impl Layout {
    fn dir_bytes(&self) -> u64 {
        4 + 4 * self.sizes.len() as u64 + 4 * self.lists.len() as u64
    }

    /// The size and the blocks of stream `k`.
    fn stream(&self, k: usize) -> (u32, &[u32]) {
        (self.sizes[k], &self.lists[self.starts[k]..self.starts[k + 1]])
    }

    /// Of each block, the bytes at its start that the layout and the streams fill (the rest
    /// comes from the rest); `None` where a block is used twice or not within the file.
    fn covered(&self) -> Option<Vec<u32>> {
        let (bs, nb) = (self.block_size, self.blocks as usize);
        let mut cov = vec![0u32; nb];
        let mut set = |b: u32, n: u32| -> Option<()> {
            let c = cov.get_mut(b as usize)?;
            if *c != 0 || n == 0 {
                return None;
            }
            *c = n;
            Some(())
        };
        set(0, 56)?;
        set(self.map, 4 * self.dir.len() as u32)?;
        let dir = self.dir_bytes();
        for (k, &b) in self.dir.iter().enumerate() {
            set(b, (dir - k as u64 * bs as u64).min(bs as u64) as u32)?;
        }
        for s in 0..self.sizes.len() {
            let (size, blocks) = self.stream(s);
            let size = if size == u32::MAX { 0 } else { size as u64 };
            for (k, &b) in blocks.iter().enumerate() {
                set(b, (size - k as u64 * bs as u64).min(bs as u64) as u32)?;
            }
        }
        Some(cov)
    }
}

impl Layout {
    /// The streams in the order their data goes in the first input: by their first blocks (in
    /// the file's order), streams of none last, by index. (No two streams share a first block
    /// where the blocks are covered once: [`Layout::covered`].)
    fn order(&self) -> Vec<usize> {
        let mut first = vec![u32::MAX; self.blocks as usize];
        // (each a stream's index, of which there are no more)
        let mut none = Vec::with_capacity(self.sizes.len());
        for k in 0..self.sizes.len() {
            match self.stream(k).1.first().and_then(|&b| first.get_mut(b as usize)) {
                Some(f) => *f = k as u32,
                None => none.push(k),
            }
        }
        let mut out = Vec::with_capacity(self.sizes.len());
        out.extend(first.iter().filter(|&&k| k != u32::MAX).map(|&k| k as usize));
        out.extend_from_slice(&none);
        out
    }
}

/// The blocks of a stream of `size` bytes.
fn blocks_of(size: u32, bs: u32) -> usize {
    if size == u32::MAX { 0 } else { size.div_ceil(bs) as usize }
}

/// The layout of file `d`, `None` where it is not one this module rebuilds.
fn parse(d: &[u8]) -> Option<Layout> {
    if !starts(d) {
        return None;
    }
    let f = |k: usize| u32le(d, 32 + 4 * k);
    let (block_size, fpm, blocks, dir_bytes, unknown, map) = (f(0)?, f(1)?, f(2)?, f(3)?, f(4)?, f(5)?);
    if !block_size.is_power_of_two() || !(512..=1 << 16).contains(&block_size) {
        return None;
    }
    let bs = block_size as usize;
    if bs.checked_mul(blocks as usize)? != d.len() || dir_bytes < 4 {
        return None;
    }
    let block = |b: u32| d.get(b as usize * bs..(b as usize + 1) * bs);
    let n = dir_bytes.div_ceil(block_size) as usize;
    let map_block = block(map)?;
    if 4 * n > bs {
        return None;
    }
    let dir: Vec<u32> = (0..n).map(|k| u32le(map_block, 4 * k)).collect::<Option<_>>()?;
    let mut bytes = Vec::with_capacity(n * bs);
    for &b in &dir {
        bytes.extend_from_slice(block(b)?);
    }
    bytes.truncate(dir_bytes as usize);
    let ns = u32le(&bytes, 0)? as usize;
    let mut at = 4 + 4 * ns;
    // (each stream's size and block 4 bytes of the directory)
    let room = bytes.len() / 4;
    let (mut sizes, mut starts) = (Vec::with_capacity(ns.min(room)), Vec::with_capacity(ns.min(room) + 1));
    let mut lists = Vec::with_capacity(room);
    starts.push(0);
    for k in 0..ns {
        let size = u32le(&bytes, 4 + 4 * k)?;
        for _ in 0..blocks_of(size, block_size) {
            lists.push(u32le(&bytes, at)?);
            at += 4;
        }
        sizes.push(size);
        starts.push(lists.len());
    }
    let l = Layout { block_size, fpm, blocks, unknown, map, dir, sizes, starts, lists };
    (at == bytes.len() && l.dir_bytes() == dir_bytes as u64).then_some(l)
}

/// The inputs ([`INPUT_NAMES`]) of file `d`, or `None` where it is not rebuilt as it is.
pub fn encode(d: &[u8]) -> Option<[Vec<u8>; 3]> {
    encode_streams(d).map(|(inputs, _)| inputs)
}

/// Where each stream is in the first input (none for a stream of none).
pub type Spans = Vec<Option<std::ops::Range<usize>>>;

/// [`encode`], with where each stream is in the first input.
pub fn encode_streams(d: &[u8]) -> Option<([Vec<u8>; 3], Spans)> {
    let l = parse(d)?;
    let cov = l.covered()?;
    let bs = l.block_size as usize;
    let mut streams = Vec::new();
    let mut spans = vec![None; l.sizes.len()];
    for k in l.order() {
        let (size, blocks) = l.stream(k);
        spans[k] = (size != u32::MAX).then(|| streams.len()..streams.len() + size as usize);
        let size = if size == u32::MAX { 0 } else { size as usize };
        for (k, &b) in blocks.iter().enumerate() {
            let n = (size - k * bs).min(bs);
            streams.extend_from_slice(&d[b as usize * bs..][..n]);
        }
    }
    let mut layout = Vec::new();
    let put = |o: &mut Vec<u8>, v: u64| varint::put(o, v);
    put(&mut layout, l.block_size.trailing_zeros() as u64);
    put(&mut layout, l.fpm as u64);
    put(&mut layout, l.blocks as u64);
    put(&mut layout, l.unknown as u64);
    put(&mut layout, l.map as u64);
    put(&mut layout, l.sizes.len() as u64);
    for size in &l.sizes {
        put(&mut layout, size.wrapping_add(1) as u64);
    }
    // the blocks, as deltas from the block after the previous: the directory's, then the streams'
    let mut prev = l.map;
    for &b in l.dir.iter().chain(&l.lists) {
        varint::put_signed(&mut layout, b as i64 - prev as i64 - 1);
        prev = b;
    }
    let mut rest = Vec::new();
    for (b, &c) in cov.iter().enumerate() {
        rest.extend_from_slice(&d[b * bs + c as usize..(b + 1) * bs]);
    }
    let inputs = [streams, layout, rest];
    let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
    (decode(&refs, d.len() as u64).ok()? == d).then_some((inputs, spans))
}

/// A reader of a stream of varints.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn get(&mut self) -> Result<u64> {
        varint::get(self.d, &mut self.at).map_err(|_| corrupt())
    }

    fn u32(&mut self) -> Result<u32> {
        u32::try_from(self.get()?).map_err(|_| corrupt())
    }
}

/// What [`decode`] takes of an [`aqr_api::stack`] at most besides what `aqr_api::whole` copies,
/// of `layout` and a file of `len` bytes: the streams' sizes, the directory's blocks, where the
/// streams' blocks start and their blocks, the blocks' coverage, the file, the directory, and the
/// streams' order (each block's first stream, the streams of none, the order). `None` where
/// the layout does not start as one.
pub fn decode_plan(layout: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::block;
    let mut r = Reader { d: layout, at: 0 };
    let block_size = 1u32 << r.get().ok().filter(|l| (9..=16).contains(l))?;
    // (the free page map, the blocks, a field unknown, the directory's map)
    let (_, blocks, _, _) = (r.u32().ok()?, r.u32().ok()? as u64, r.u32().ok()?, r.u32().ok()?);
    let ns = r.get().ok()?;
    let mut total = 0;
    for _ in 0..ns {
        total += blocks_of(r.u32().ok()?.wrapping_sub(1), block_size) as u64;
    }
    let dir_bytes = 4 + 4 * ns + 4 * total;
    let dir_blocks = dir_bytes.div_ceil(block_size as u64);
    // (where the streams' blocks start a `usize` each, 4 bytes on WebAssembly)
    let layout = block(4 * ns) + block(4 * dir_blocks) + block(4 * (ns + 1)) + block(4 * total);
    let order = block(4 * blocks) + 2 * block(4 * ns);
    Some(layout + block(4 * blocks) + block(len) + block(dir_bytes) + order)
}

/// The file of `len` bytes from `inputs` ([`INPUT_NAMES`]).
pub fn decode(inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[streams, layout, rest] = inputs else { return Err(corrupt()) };
    let mut r = Reader { d: layout, at: 0 };
    let log = r.get()?;
    if !(9..=16).contains(&log) {
        return Err(corrupt());
    }
    let block_size = 1u32 << log;
    let (fpm, blocks, unknown, map) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    let bs = block_size as usize;
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    if (blocks as u64) * bs as u64 != len as u64 {
        return Err(corrupt());
    }
    let ns = r.get()?;
    // (each stream's size is a byte of the layout at least)
    if ns > (layout.len() - r.at) as u64 {
        return Err(corrupt());
    }
    let mut sizes: Vec<u32> = Vec::with_capacity(ns as usize);
    for _ in 0..ns {
        sizes.push(r.u32()?.wrapping_sub(1));
    }
    let total: u64 = sizes.iter().map(|&s| blocks_of(s, block_size) as u64).sum();
    let dir_bytes = 4 + 4 * ns + 4 * total;
    let dir_blocks = dir_bytes.div_ceil(bs as u64);
    // (each block is a byte of the layout at least)
    if dir_blocks + total > (layout.len() - r.at) as u64 || 4 * dir_blocks > bs as u64 {
        return Err(corrupt());
    }
    let mut prev = map;
    let mut next = || -> Result<u32> {
        let b = (prev as i64).wrapping_add(varint::get_signed(layout, &mut r.at).map_err(|_| corrupt())?);
        prev = u32::try_from(b.wrapping_add(1)).map_err(|_| corrupt())?;
        Ok(prev)
    };
    let mut dir = Vec::with_capacity(dir_blocks as usize);
    for _ in 0..dir_blocks {
        dir.push(next()?);
    }
    let (mut starts, mut lists) = (Vec::with_capacity(sizes.len() + 1), Vec::with_capacity(total as usize));
    starts.push(0);
    for &s in &sizes {
        for _ in 0..blocks_of(s, block_size) {
            lists.push(next()?);
        }
        starts.push(lists.len());
    }
    if r.at != layout.len() {
        return Err(corrupt());
    }
    let l = Layout { block_size, fpm, blocks, unknown, map, dir, sizes, starts, lists };
    let cov = l.covered().ok_or_else(corrupt)?;
    let mut out = vec![0u8; len];
    // the superblock
    out[..32].copy_from_slice(MAGIC);
    let dir_bytes = u32::try_from(dir_bytes).map_err(|_| corrupt())?;
    for (k, v) in [block_size, fpm, blocks, dir_bytes, unknown, map].into_iter().enumerate() {
        out[32 + 4 * k..36 + 4 * k].copy_from_slice(&v.to_le_bytes());
    }
    // the directory's blocks, and the directory
    for (k, &b) in l.dir.iter().enumerate() {
        out[map as usize * bs + 4 * k..][..4].copy_from_slice(&b.to_le_bytes());
    }
    let mut dir = Vec::with_capacity(dir_bytes as usize);
    dir.extend_from_slice(&(ns as u32).to_le_bytes());
    for &s in &l.sizes {
        dir.extend_from_slice(&s.to_le_bytes());
    }
    for b in &l.lists {
        dir.extend_from_slice(&b.to_le_bytes());
    }
    for (chunk, &b) in dir.chunks(bs).zip(&l.dir) {
        out[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
    }
    // the streams
    let mut at = 0;
    for k in l.order() {
        let (size, blocks) = l.stream(k);
        let size = if size == u32::MAX { 0 } else { size as usize };
        for (k, &b) in blocks.iter().enumerate() {
            let n = (size - k * bs).min(bs);
            let s = streams.get(at..at + n).ok_or_else(corrupt)?;
            out[b as usize * bs..][..n].copy_from_slice(s);
            at += n;
        }
    }
    if at != streams.len() {
        return Err(corrupt());
    }
    // the rest
    let mut at = 0;
    for (b, &c) in cov.iter().enumerate() {
        let n = bs - c as usize;
        let s = rest.get(at..at + n).ok_or_else(corrupt)?;
        out[b * bs + c as usize..(b + 1) * bs].copy_from_slice(s);
        at += n;
    }
    if at != rest.len() {
        return Err(corrupt());
    }
    Ok(out)
}
