//! The model of a chunk's holes (FORMAT.md, §5): a section's block states as indices of its
//! palette, each coded as one of its neighbours already coded (or, where none, by its bits), and
//! its light as nibbles from the top down, each of the light above it, to its west and north and
//! of the block there. One walk ([`chunk`]) both codes (an encoder of [`Coder`], given the
//! chunk's NBT) and decodes (a decoder, filling the holes), of a model kept over the chunks of a
//! file.

use aqr_common::arith::{ArithDecoder, ArithEncoder};

use crate::nbt::{self, BLOCK_LIGHT, BLOCKS, LIGHT, List, SKY_LIGHT, Section};
use crate::{Result, corrupt};

/// An adaptive bit: the probability of a 1 (of 65536), moved by 1/16 of the way to each bit.
#[derive(Clone, Copy)]
pub struct Bit(u16);

impl Default for Bit {
    fn default() -> Self {
        Bit(1 << 15)
    }
}

impl Bit {
    /// Of a probability of a 1 of `p` (of 65536).
    const fn of(p: u16) -> Bit {
        Bit(p)
    }

    #[inline]
    fn p1(self) -> u32 {
        self.0 as u32
    }
}

/// How far a bit moves to each bit (a shift).
const RATE: u32 = 4;

/// A side of the coder: the encoder (writing the bit given and returning it) or the decoder
/// (returning the bit it reads), so that one walk does both.
pub trait Coder {
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool;
    /// Whether the decoder has read past its input.
    fn failed(&self) -> bool;
}

#[inline]
fn update(b: &mut Bit, v: bool) {
    match v {
        true => b.0 += (u16::MAX - b.0) >> RATE,
        false => b.0 -= b.0 >> RATE,
    }
}

impl Coder for ArithEncoder {
    #[inline]
    fn bit(&mut self, b: &mut Bit, v: bool) -> bool {
        self.encode(v, b.p1());
        update(b, v);
        v
    }
    fn failed(&self) -> bool {
        false
    }
}

impl Coder for ArithDecoder<'_> {
    #[inline]
    fn bit(&mut self, b: &mut Bit, _: bool) -> bool {
        let v = self.decode(b.p1());
        update(b, v);
        v
    }
    fn failed(&self) -> bool {
        self.overrun()
    }
}

/// Bits of the index of a slot of the model's tables: of block states, and of light.
pub const BLOCK_BITS: u32 = 18;
pub const LITERAL_BITS: u32 = 16;

/// The probability that a bit of a guess (a neighbour, the light above) starts at: of 65536.
const HIT: u16 = 32768;
pub const TABLE_BITS: u32 = 20;

/// The box of a section's walk of block states: 2 blocks of none before it on each axis (the
/// layer below at y -1), 2 after it on x and z.
const GX: usize = 20;
const GL: usize = GX * GX;
const GRID: usize = GL * 18;

/// The cell of the box of the block at x, z, y.
#[inline]
fn cell(x: usize, z: usize, y: isize) -> usize {
    ((y + 2) as usize) * GL + (z + 2) * GX + x + 2
}

#[inline]
fn slot_of(h: u64, bits: u32) -> usize {
    (h >> (64 - bits)) as usize
}

/// The key of the decisions of a row as the one below.
const ROW: u64 = 0x2_0000_0000_0000;

/// Blocks of a section, and of a layer of it.
const BLOCKS_N: usize = 4096;
const LAYER: usize = 256;

/// The most entries of a palette (of 12 bits an index).
pub const PALETTE: usize = 4096;

/// No index (the palette's hashes have one for it, past the most entries), no light.
const NONE: u16 = PALETTE as u16;
const DARK: u8 = 16;

/// The neighbours that a block's index is coded as one of, in order: (x, z, y) offsets.
const NEIGHBOURS: [(i8, i8, i8); 16] = [
    (-1, 0, 0),
    (0, -1, 0),
    (0, 0, -1),
    (-1, -1, 0),
    (1, -1, 0),
    (-1, 0, -1),
    (0, -1, -1),
    (1, 0, -1),
    (0, 1, -1),
    (-2, 0, 0),
    (0, -2, 0),
    (0, 0, -2),
    (-1, 1, -1),
    (1, 1, -1),
    (-1, -1, -1),
    (1, -1, -1),
];

/// The adaptive bits of the model, kept over a file's chunks.
pub struct Model {
    flags: [Bit; 3],
    blocks: Vec<Bit>,
    literals: Vec<Bit>,
    light: Vec<Bit>,
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

impl Model {
    pub fn new() -> Model {
        Model {
            flags: [Bit::default(); 3],
            blocks: vec![Bit::of(HIT); 1 << BLOCK_BITS],
            literals: vec![Bit::default(); 1 << LITERAL_BITS],
            // (of each line, the first bit is of the light as guessed, the others of a tree)
            light: (0..1 << TABLE_BITS).map(|i| if i % 16 == 0 { Bit::of(HIT) } else { Bit::default() }).collect(),
        }
    }

    /// The bit of whether a candidate of `kind` is a hole.
    pub fn flag(&mut self, kind: usize) -> &mut Bit {
        &mut self.flags[kind]
    }
}

/// Bytes that a [`Model`] and a [`Work`] take, on WebAssembly as natively: their blocks.
pub fn memory() -> [u64; 3] {
    let bit = core::mem::size_of::<Bit>() as u64;
    [bit << BLOCK_BITS, bit << LITERAL_BITS, bit << TABLE_BITS]
}

/// Buffers of a section's walk.
pub struct Work {
    /// The indices of the section, and of another (the one below, or of the light's blocks).
    idx: Vec<u16>,
    other: Vec<u16>,
    /// The hashes of the palette's entries, and of the other's.
    hashes: Vec<u32>,
    other_hashes: Vec<u32>,
    /// The index (of the section's palette) of each block of the layer below, or [`NONE`].
    below: Vec<u16>,
    /// Light, of the section and of the layer above ([`DARK`] where none).
    light: Vec<u8>,
    above: Vec<u8>,
    /// The box of the walk of block states.
    grid: Vec<u16>,
}

impl Default for Work {
    fn default() -> Self {
        Work::new()
    }
}

impl Work {
    pub fn new() -> Work {
        Work {
            idx: vec![0; BLOCKS_N],
            other: vec![0; BLOCKS_N],
            hashes: Vec::with_capacity(PALETTE + 1),
            other_hashes: Vec::with_capacity(PALETTE),
            below: vec![NONE; LAYER],
            light: vec![0; BLOCKS_N],
            above: vec![DARK; LAYER],
            grid: vec![NONE; GRID],
        }
    }
}

/// Bytes of the blocks of a [`Work`].
pub fn work_memory() -> [u64; 8] {
    let n = BLOCKS_N as u64;
    let p = 4 * (PALETTE as u64 + 1);
    [2 * n, 2 * n, p, 4 * PALETTE as u64, 2 * LAYER as u64, n, LAYER as u64, 2 * GRID as u64]
}

#[inline]
fn mix(x: u64) -> u64 {
    (x ^ x >> 29).wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

#[inline]
fn slot(h: u64) -> usize {
    (h >> (64 - TABLE_BITS)) as usize
}

/// The hash of a palette's entry: of its payload's bytes (FNV-1a, its high 32 bits).
fn hash(b: &[u8]) -> u32 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h >> 32) as u32
}

/// The hashes of the entries of `list` in `d` into `out`.
fn palette(d: &[u8], list: List, out: &mut Vec<u32>) -> Result<()> {
    out.clear();
    if list.count as usize > PALETTE {
        return Err(corrupt());
    }
    let mut at = list.at as usize;
    for _ in 0..list.count {
        let end = nbt::skip(d, at, list.ty, nbt::DEPTH)?;
        out.push(hash(&d[at..end]));
        at = end;
    }
    Ok(())
}

/// Bits of an index of a palette of `p` entries (at least 4), and the longs of 4096 of them.
pub fn index_bits(p: usize) -> (u32, usize) {
    let bits = (usize::BITS - (p - 1).leading_zeros()).max(4);
    (bits, BLOCKS_N.div_ceil(64 / bits as usize))
}

/// The 4096 indices of `bits` bits packed in the longs at `at` of `d` into `out`.
pub fn unpack(d: &[u8], at: usize, bits: u32, out: &mut [u16]) {
    let per = 64 / bits as usize;
    let mask = (1u64 << bits) - 1;
    for (l, o) in d[at..].as_chunks::<8>().0.iter().zip(out.chunks_mut(per)) {
        let w = u64::from_be_bytes(*l);
        for (j, v) in o.iter_mut().enumerate() {
            *v = (w >> (j as u32 * bits) & mask) as u16;
        }
    }
}

/// `idx` packed into longs of `bits` bits at `at` of `d`.
pub fn pack(idx: &[u16], bits: u32, d: &mut [u8], at: usize) {
    let per = 64 / bits as usize;
    for (l, o) in d[at..].as_chunks_mut::<8>().0.iter_mut().zip(idx.chunks(per)) {
        let w = o.iter().enumerate().fold(0u64, |w, (j, &v)| w | (v as u64) << (j as u32 * bits));
        *l = w.to_be_bytes();
    }
}

/// Whether the block states `data` of the section `s` are a hole that the model makes again: of
/// a palette of 2 to 4096 entries, as many longs as 4096 indices take, each index in the palette
/// and the bits of the longs past the indices zero.
pub fn blocks_hole(d: &[u8], s: &Section) -> bool {
    let (Some(p), Some(a)) = (s.palette, s.data) else { return false };
    let n = p.count as usize;
    if s.twice || !(2..=PALETTE).contains(&n) {
        return false;
    }
    let (bits, longs) = index_bits(n);
    if a.count as usize != longs {
        return false;
    }
    let mut idx = vec![0; longs * (64 / bits as usize)];
    unpack(d, a.at as usize, bits, &mut idx);
    if idx[..BLOCKS_N].iter().any(|&i| i as usize >= n) {
        return false;
    }
    let mut again = vec![0; longs * 8];
    pack(&idx, bits, &mut again, 0);
    again == d[a.at as usize..][..longs * 8]
}

/// The hashes of the blocks of the section `s` in `d`, into `w.other` (as indices) and
/// `w.other_hashes`, or false where it has none: a palette of one entry, or of more and its
/// data of the longs of 4096 indices (an index past the palette taken as no block).
fn blocks_of(d: &[u8], s: &Section, w: &mut Work) -> Result<bool> {
    let Some(p) = s.palette else { return Ok(false) };
    if p.count == 0 || p.count as usize > PALETTE {
        return Ok(false);
    }
    palette(d, p, &mut w.other_hashes)?;
    if p.count == 1 {
        w.other.fill(0);
        return Ok(true);
    }
    let (bits, longs) = index_bits(p.count as usize);
    match s.data {
        Some(a) if a.count as usize == longs => {
            unpack(d, a.at as usize, bits, &mut w.other);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The hash of `w.other[i]`, or none.
fn other_hash(w: &Work, i: usize) -> Option<u32> {
    w.other_hashes.get(w.other[i] as usize).copied()
}

/// The section's block states (`d` at its data) coded: of `orig` (encoding) or decoded.
#[inline(never)]
fn blocks<C: Coder>(
    c: &mut C,
    m: &mut Model,
    w: &mut Work,
    d: &mut [u8],
    secs: &[Section],
    s: usize,
    orig: Option<&[u8]>,
) -> Result<()> {
    let sec = &secs[s];
    let (Some(p), Some(a)) = (sec.palette, sec.data) else { return Err(corrupt()) };
    let n = p.count as usize;
    if sec.twice || !(2..=PALETTE).contains(&n) {
        return Err(corrupt());
    }
    let (bits, longs) = index_bits(n);
    if a.count as usize != longs {
        return Err(corrupt());
    }
    let lit_bits = usize::BITS - (n - 1).leading_zeros();
    palette(d, p, &mut w.hashes)?;
    // the layer below: the top of the section before, of the Y below
    w.below.fill(NONE);
    let under = sec.y.and_then(|y| {
        let t = secs[..s].iter().rposition(|t| t.palette.is_some())?;
        (secs[t].y? as i16 == y as i16 - 1).then_some(t)
    });
    if let Some(t) = under
        && blocks_of(d, &secs[t], w)?
    {
        let mut last = (u32::MAX, NONE);
        for i in 0..LAYER {
            let Some(h) = other_hash(w, BLOCKS_N - LAYER + i) else { continue };
            if h != last.0 {
                last = (h, w.hashes.iter().position(|&x| x == h).map_or(NONE, |j| j as u16));
            }
            w.below[i] = last.1;
        }
    }
    if let Some(o) = orig {
        unpack(o, a.at as usize, bits, &mut w.idx);
    }
    // the section in a box of none around it, the layer below in place: a neighbour is one load
    let g = &mut w.grid;
    g.fill(NONE);
    for (z, row) in w.below.chunks(16).enumerate() {
        g[cell(0, z, -1)..][..16].copy_from_slice(row);
    }
    w.hashes.resize(PALETTE + 1, 0);
    w.hashes[NONE as usize] = 0x5555_5555;
    let hashes = &w.hashes;
    let offsets = NEIGHBOURS.map(|(dx, dz, dy)| dx as isize + dz as isize * GX as isize + dy as isize * GL as isize);
    // each row: as the row below (where there is one), else block by block
    let mut copied = [0u8; 16];
    let mut last = 0u64;
    for r in 0..BLOCKS_N / 16 {
        let (z, y) = (r & 15, (r >> 4) as isize);
        let at = cell(0, z, y);
        let below = at - GL;
        if g[below] != NONE {
            let same = w.idx[r * 16..r * 16 + 16] == g[below..below + 16];
            let flat = g[below..below + 16].iter().all(|&v| v == g[below]) as u64;
            let key = ROW ^ last ^ (copied[z] as u64) << 1 ^ flat << 2 ^ (hashes[g[below] as usize] as u64) << 8;
            if c.bit(&mut m.blocks[slot_of(mix(key), BLOCK_BITS)], same) {
                g.copy_within(below..below + 16, at);
                copied[z] = 1;
                last = 1;
                continue;
            }
        }
        copied[z] = 0;
        last = 0;
        for x in 0..16 {
            g[at + x] = block(c, m, g, at + x, &offsets, hashes, w.idx[r * 16 + x], lit_bits, n)?;
        }
    }
    for (i, row) in w.idx.chunks_mut(16).enumerate() {
        row.copy_from_slice(&g[cell(0, i & 15, (i >> 4) as isize)..][..16]);
    }
    if c.failed() {
        return Err(corrupt());
    }
    pack(&w.idx, bits, d, a.at as usize);
    Ok(())
}

/// A block's index coded (`truth`, encoding) or decoded, its neighbours in the box `g` around
/// its cell `at`: as the first of them it is, else by its `lit_bits` bits (of a palette of `n`).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn block<C: Coder>(
    c: &mut C,
    m: &mut Model,
    g: &[u16],
    at: usize,
    offsets: &[isize; 16],
    hashes: &[u32],
    truth: u16,
    lit_bits: u32,
    n: usize,
) -> Result<u16> {
    let nb = |ci: usize| g[(at as isize + offsets[ci]) as usize];
    let (wv, nv, dv) = (nb(0), nb(1), nb(2));
    let pat = (wv == nv) as u64 | ((wv == dv) as u64) << 1 | ((nv == dv) as u64) << 2;
    let key_of = |ci: usize, k: usize, v: u16| {
        let other = if ci == 0 { dv } else { wv };
        ((ci as u64 * 8 + pat) * 16 + k as u64)
            ^ (hashes[v as usize] as u64) << 16
            ^ (hashes[other as usize] as u64) << 40
    };
    // the block to the west first, the others then (each once)
    if wv != NONE && c.bit(&mut m.blocks[slot_of(mix(key_of(0, 1, wv)), BLOCK_BITS)], truth == wv) {
        return Ok(wv);
    }
    let mut seen = [NONE; 16];
    let mut k = 0;
    if wv != NONE {
        seen[0] = wv;
        k = 1;
    }
    for ci in 1..NEIGHBOURS.len() {
        let v = nb(ci);
        if v == NONE || seen[..k].contains(&v) {
            continue;
        }
        seen[k] = v;
        k += 1;
        if c.bit(&mut m.blocks[slot_of(mix(key_of(ci, k, v)), BLOCK_BITS)], truth == v) {
            return Ok(v);
        }
    }
    // its bits, of the block to its west
    let base = mix(0x1_0000_0000_0000 ^ (hashes[wv as usize] as u64) << 8);
    let mut node = 1u64;
    for b in (0..lit_bits).rev() {
        let bit = c.bit(&mut m.literals[slot_of(base ^ mix(node), LITERAL_BITS)], truth >> b & 1 != 0);
        node = node << 1 | bit as u64;
    }
    let v = (node - (1 << lit_bits)) as u16;
    match (v as usize) < n {
        true => Ok(v),
        false => Err(corrupt()),
    }
}

/// The section's light of `kind` (`d` at its array) coded: of `orig` (encoding) or decoded.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn light<C: Coder>(
    c: &mut C,
    m: &mut Model,
    w: &mut Work,
    d: &mut [u8],
    secs: &[Section],
    s: usize,
    kind: usize,
    orig: Option<&[u8]>,
) -> Result<()> {
    let sec = &secs[s];
    let l = kind - BLOCK_LIGHT;
    let Some(a) = sec.light[l] else { return Err(corrupt()) };
    if sec.twice || a.count as usize != LIGHT {
        return Err(corrupt());
    }
    // the layer above: the bottom of the section after, of the Y above
    w.above.fill(DARK);
    let over = sec.y.and_then(|y| {
        let t = s + 1 + secs[s + 1..].iter().position(|t| t.light[l].is_some())?;
        (secs[t].y? as i16 == y as i16 + 1).then_some(t)
    });
    let mut state = 17u64;
    if let Some(t) = over {
        let at = secs[t].light[l].unwrap().at as usize;
        if secs[t].light[l].unwrap().count as usize == LIGHT {
            for i in 0..LAYER {
                w.above[i] = d[at + i / 2] >> (i % 2 * 4) & 15;
            }
            state = match w.above.iter().all(|&v| v == w.above[0]) {
                true => w.above[0] as u64,
                false => 16,
            };
        }
    }
    let has_blocks = blocks_of(d, sec, w)?;
    let at = a.at as usize;
    if let Some(o) = orig {
        for i in 0..BLOCKS_N {
            w.light[i] = o[at + i / 2] >> (i % 2 * 4) & 15;
        }
    }
    let tag = (kind as u64) << 56;
    let uniform = w.light.iter().all(|&v| v == w.light[0]);
    if c.bit(&mut m.light[slot(mix(tag ^ 0x100 ^ state))], uniform) {
        let base = mix(tag ^ 0x200 ^ state);
        let v = tree(c, &mut m.light, base, w.light[0]);
        w.light.fill(v);
    } else {
        let has_above = over.is_some();
        let mut last = 0;
        for y in (0..16).rev() {
            for z in 0..16 {
                light_row(c, m, w, tag, y, z, z == 0, has_above, has_blocks, &mut last);
            }
        }
    }
    if c.failed() {
        return Err(corrupt());
    }
    for (i, b) in d[at..at + LIGHT].iter_mut().enumerate() {
        *b = w.light[2 * i] | w.light[2 * i + 1] << 4;
    }
    Ok(())
}

/// A row of light coded or decoded (into `w.light`): as the row above, else each nibble (`first`:
/// of no row to its north).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn light_row<C: Coder>(
    c: &mut C,
    m: &mut Model,
    w: &mut Work,
    tag: u64,
    y: usize,
    z: usize,
    first: bool,
    has_above: bool,
    has_blocks: bool,
    last: &mut u64,
) {
    let r = y * LAYER + z * 16;
    if y < 15 || has_above {
        let up: [u8; 16] = match y {
            15 => w.above[z * 16..][..16].try_into().unwrap(),
            _ => w.light[r + LAYER..][..16].try_into().unwrap(),
        };
        let flat = up.iter().all(|&v| v == up[0]) as u64;
        let b0 = if has_blocks { other_hash(w, r).unwrap_or(0) } else { 0 } as u64;
        let key = tag ^ 0x300 ^ *last ^ flat << 1 ^ (up[0] as u64) << 2 ^ b0 << 24;
        let same = w.light[r..r + 16] == up;
        *last = c.bit(&mut m.light[slot(mix(key))], same) as u64;
        if *last != 0 {
            w.light[r..r + 16].copy_from_slice(&up);
            return;
        }
    }
    for x in 0..16 {
        let i = r + x;
        let u = if y < 15 { w.light[i + LAYER] } else { w.above[i & 255] };
        let wv = if x > 0 { w.light[i - 1] } else { DARK };
        let nv = if !first { w.light[i - 16] } else { DARK };
        let b = if has_blocks { other_hash(w, i).unwrap_or(0) } else { 0 };
        let key = tag ^ (u as u64 | (wv as u64) << 5 | (nv as u64) << 10) ^ (b as u64) << 24;
        // as the light above (or west, or north, or none), else its bits
        let guess = [u, wv, nv, 0].into_iter().find(|&v| v != DARK).unwrap();
        let at = slot(mix(key)) & !15;
        w.light[i] = match c.bit(&mut m.light[at], w.light[i] == guess) {
            true => guess,
            false => tree(c, &mut m.light, mix(key), w.light[i]),
        };
    }
}

/// A nibble by a binary tree of the bits of `t` at the 16 slots from `base`'s.
#[inline]
fn tree<C: Coder>(c: &mut C, t: &mut [Bit], base: u64, v: u8) -> u8 {
    let at = slot(base) & !15;
    let mut node = 1usize;
    for b in (0..4).rev() {
        let bit = c.bit(&mut t[at + node], v >> b & 1 != 0);
        node = node << 1 | bit as usize;
    }
    (node - 16) as u8
}

/// The holes of a chunk (its NBT `d`, parsed into `secs`) coded: the block states of each
/// section in order, then the block light, then the sky light, of each section from the last;
/// encoding, of the chunk's NBT `orig` (its holes zero in `d`, and filled as they are coded).
pub fn chunk<C: Coder>(
    c: &mut C,
    m: &mut Model,
    w: &mut Work,
    d: &mut [u8],
    secs: &[Section],
    orig: Option<&[u8]>,
) -> Result<()> {
    for s in 0..secs.len() {
        if secs[s].data.is_some_and(|a| a.hole) {
            blocks(c, m, w, d, secs, s, orig)?;
        }
    }
    for kind in [BLOCK_LIGHT, SKY_LIGHT] {
        for s in (0..secs.len()).rev() {
            if secs[s].light[kind - BLOCK_LIGHT].is_some_and(|a| a.hole) {
                light(c, m, w, d, secs, s, kind, orig)?;
            }
        }
    }
    Ok(())
}

/// The kinds of holes, for [`crate::nbt::Candidate::kind`].
pub const KINDS: [usize; 3] = [BLOCKS, BLOCK_LIGHT, SKY_LIGHT];
