//! The code of the module `clip-recons` (see `formula`, and FORMAT.md), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.
//!
//! A file is rebuilt from its skeleton (the file without the zlib streams of the tiles that are
//! modelled and without the database), the tiles (coded by [`crate::dynamic`] in batches, a tile
//! equal to an earlier one a reference to it) and the database. The skeleton is walked by the sizes it keeps ([`pieces`]): the walk is the
//! same over the file (to make the skeleton) and over the skeleton (to rebuild the file).

use crate::dynamic::{self, Escape, Model};
use crate::tile::{self, AREA, Layout};
use aqr_common::arith::ArithDecoder;
use aqr_common::pool;
use aqr_common::varint;

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid(what: &str) -> Error {
    Error(format!("invalid input: {what}"))
}

/// What the walk finds, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// Bytes the skeleton keeps.
    Keep(usize),
    /// A tile's zlib stream of this many bytes, modelled: not in the skeleton.
    Tile(Layout, usize),
    /// The database, of this many bytes: not in the skeleton.
    Database(usize),
}

fn u32be(d: &[u8], p: usize) -> Result<u32> {
    d.get(p..).and_then(|s| s.first_chunk()).map(|b| u32::from_be_bytes(*b)).ok_or_else(|| invalid("truncated"))
}

fn u64be(d: &[u8], p: usize) -> Result<usize> {
    let v =
        d.get(p..).and_then(|s| s.first_chunk()).map(|b| u64::from_be_bytes(*b)).ok_or_else(|| invalid("truncated"))?;
    usize::try_from(v).map_err(|_| invalid("size"))
}

/// Whether the string at `p` (a u32 count of UTF-16BE units, then them) is `s` (ASCII), and
/// where it ends.
fn string_is(d: &[u8], p: usize, s: &str) -> Option<usize> {
    let n = u32be(d, p).ok()? as usize;
    let units = d.get(p + 4..p + 4 + 2 * n)?;
    (n == s.len() && units.as_chunks::<2>().0.iter().zip(s.bytes()).all(|(u, c)| u[0] == 0 && u[1] == c))
        .then_some(p + 4 + 2 * n)
}

/// The end of the string at `p`.
fn string_end(d: &[u8], p: usize) -> Result<usize> {
    let n = u32be(d, p)? as usize;
    let end = n.checked_mul(2).and_then(|b| b.checked_add(p + 4)).ok_or_else(|| invalid("string"))?;
    if end > d.len() {
        return Err(invalid("truncated"));
    }
    Ok(end)
}

const BEGIN: &str = "BlockDataBeginChunk";
const END: &str = "BlockDataEndChunk";

/// The walk over `src`: the file itself where `whole`, else its skeleton. A tile is modelled
/// where its block holds a tile of a known layout (256×256) and `modelled` says so (called for
/// each such block in order, with its zlib stream where `whole`); the database is the body of
/// the `SQLi` chunk (at most one). Keeps of the skeleton follow one another merged.
pub fn pieces<'a>(
    src: &'a [u8],
    whole: bool,
    modelled: &mut dyn FnMut(Layout, &'a [u8]) -> bool,
) -> Result<Vec<Piece>> {
    let mut out = Sink::new(None);
    walk(src, whole, modelled, &mut out)?;
    Ok(out.v)
}

/// Which blocks of a skeleton are modelled, by the runs of the index (alternately modelled and
/// kept blocks, the first run modelled): [`Runs::flag`] for each block with a tile in order.
pub struct Runs<'a> {
    runs: &'a [usize],
    run: usize,
    left: usize,
    on: bool,
}

impl<'a> Runs<'a> {
    pub fn new(runs: &'a [usize]) -> Runs<'a> {
        Runs { runs, run: 0, left: runs.first().copied().unwrap_or(0), on: true }
    }

    /// Whether the next block is modelled (kept past the runs).
    pub fn flag(&mut self) -> bool {
        while self.left == 0 && self.run + 1 < self.runs.len() {
            self.run += 1;
            self.left = self.runs[self.run];
            self.on = !self.on;
        }
        if self.left == 0 {
            return false;
        }
        self.left -= 1;
        self.on
    }
}

/// The pieces of the skeleton `src` where the runs `runs` say which of its blocks are modelled,
/// in a vector of room for exactly them: the walk is made twice, the first counting them.
pub fn skeleton_pieces(src: &[u8], runs: &[usize]) -> Result<Vec<Piece>> {
    let mut count = Sink::counting();
    let mut r = Runs::new(runs);
    walk(src, false, &mut |_, _| r.flag(), &mut count)?;
    let mut out = Sink::new(Some(count.count));
    let mut r = Runs::new(runs);
    walk(src, false, &mut |_, _| r.flag(), &mut out)?;
    Ok(out.v)
}

/// Where the pieces of a walk go: counted only, or into a vector (of room reserved for them,
/// where they were counted before).
struct Sink {
    v: Vec<Piece>,
    count: usize,
    keeping: bool,
    fill: bool,
}

impl Sink {
    /// A sink filling a vector of room for `room` pieces (growing, if more come).
    fn new(room: Option<usize>) -> Sink {
        Sink { v: Vec::with_capacity(room.unwrap_or(0)), count: 0, keeping: false, fill: true }
    }

    /// A sink only counting.
    fn counting() -> Sink {
        Sink { v: Vec::new(), count: 0, keeping: false, fill: false }
    }

    /// `n` bytes the skeleton keeps, merged with those kept just before.
    fn keep(&mut self, n: usize) {
        if !self.keeping {
            self.piece(Piece::Keep(n));
            self.keeping = true;
        } else if let Some(Piece::Keep(m)) = self.v.last_mut() {
            *m += n;
        }
    }

    fn piece(&mut self, p: Piece) {
        self.count += 1;
        self.keeping = false;
        if self.fill {
            self.v.push(p);
        }
    }
}

fn walk<'a>(
    src: &'a [u8],
    whole: bool,
    modelled: &mut dyn FnMut(Layout, &'a [u8]) -> bool,
    out: &mut Sink,
) -> Result<()> {
    if !src.starts_with(b"CSFCHUNK") {
        return Err(invalid("not a .clip file"));
    }
    let first = u64be(src, 16)?;
    if first < 24 || first > src.len() {
        return Err(invalid("header"));
    }
    out.keep(first);
    // p: in src; the walk ends where src does
    let (mut p, mut database) = (first, false);
    while p < src.len() {
        if src.get(p..p + 4) != Some(b"CHNK") {
            return Err(invalid("chunk"));
        }
        let kind = src.get(p + 4..p + 8).ok_or_else(|| invalid("truncated"))?;
        let n = u64be(src, p + 8)?;
        out.keep(16);
        p += 16;
        let body_end =
            |p: usize, n: usize| p.checked_add(n).filter(|&e| e <= src.len()).ok_or_else(|| invalid("truncated"));
        if kind == b"SQLi" {
            if database {
                return Err(invalid("two databases"));
            }
            database = true;
            out.piece(Piece::Database(n));
            if whole {
                p = body_end(p, n)?;
            }
            continue;
        }
        if kind != b"Exta" {
            out.keep(n);
            p = body_end(p, n)?;
            continue;
        }
        // external data: its ID and the size of its data, then block data or anything
        let idl = u64be(src, p)?;
        let head = idl.checked_add(16).ok_or_else(|| invalid("size"))?;
        let size = u64be(src, (p + 8).checked_add(idl).ok_or_else(|| invalid("size"))?)?;
        if head.checked_add(size) != Some(n) {
            return Err(invalid("external data"));
        }
        let data = p + head;
        if string_is(src, data + 4, BEGIN).is_none() {
            out.keep(n);
            p = body_end(p, n)?;
            continue;
        }
        out.keep(head);
        // o: bytes of the data so far, in the file
        let (mut q, mut o) = (data, 0usize);
        while o < size {
            if let Some(r) = string_is(src, q + 4, BEGIN) {
                let rec = u32be(src, q)? as usize;
                let mut f = [0u32; 5];
                for (i, v) in f.iter_mut().enumerate() {
                    *v = u32be(src, r + 4 * i)?;
                }
                let (mut r, mut from, mut absent) = (r + 20, q, 0);
                if f[4] != 0 {
                    let zl = u32::from_le_bytes(
                        *src.get(r + 4..).and_then(|s| s.first_chunk()).ok_or_else(|| invalid("truncated"))?,
                    ) as usize;
                    if u32be(src, r)? as usize != zl + 4 {
                        return Err(invalid("block"));
                    }
                    let layout = Layout::of(f[1] as usize).filter(|_| f[2] == 256 && f[3] == 256);
                    let z = if whole { src.get(r + 8..r + 8 + zl).ok_or_else(|| invalid("truncated"))? } else { &[] };
                    match layout {
                        Some(l) if modelled(l, z) => {
                            out.keep(r + 8 - q);
                            out.piece(Piece::Tile(l, zl));
                            r += 8 + if whole { zl } else { 0 };
                            (from, absent) = (r, zl);
                        }
                        _ => r += 8 + zl,
                    }
                }
                let e = string_is(src, r, END).ok_or_else(|| invalid("block end"))?;
                out.keep(e - from);
                // the record's size counts all of it, the stream included
                if e - q + if whole { 0 } else { absent } != rec {
                    return Err(invalid("block size"));
                }
                q = e;
                o += rec;
            } else {
                let r = string_end(src, q)?;
                let (hs, cnt, es) = (u32be(src, r)? as usize, u32be(src, r + 4)? as usize, u32be(src, r + 8)? as usize);
                let t = cnt
                    .checked_mul(es)
                    .and_then(|b| b.checked_add(hs))
                    .and_then(|b| b.checked_add(r - q))
                    .filter(|&t| q + t <= src.len())
                    .ok_or_else(|| invalid("table"))?;
                out.keep(t);
                q += t;
                o += t;
            }
        }
        if o != size {
            return Err(invalid("block data"));
        }
        p = q;
    }
    Ok(())
}

/// A batch of the modelled tiles: its layout and the tiles it holds, `n` places of [`Parsed::order`]
/// from `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    pub layout: Layout,
    pub from: usize,
    pub n: usize,
}

/// The batches of the modelled tiles (of layouts `tiles`, in file order): the tiles of each
/// layout in file order, `per` at a time, the layouts in the order colour, one plane, two planes.
/// Each batch is its layout and the places in `tiles` of its tiles, which the other vector holds
/// batch after batch.
pub fn batches(tiles: &[Layout], per: usize) -> (Vec<Batch>, Vec<usize>) {
    let per = per.max(1);
    let layouts = [Layout::Color, Layout::One, Layout::Two];
    let count = |l: Layout| tiles.iter().filter(|&&t| t == l).count();
    let mut out = Vec::with_capacity(layouts.iter().map(|&l| count(l).div_ceil(per)).sum());
    let mut order = Vec::with_capacity(tiles.len());
    for l in layouts {
        let from = order.len();
        order.extend((0..tiles.len()).filter(|&i| tiles[i] == l));
        let n = order.len() - from;
        out.extend((0..n.div_ceil(per)).map(|k| Batch { layout: l, from: from + k * per, n: per.min(n - k * per) }));
    }
    (out, order)
}

/// The layouts of the modelled tiles of `pieces`.
pub fn layouts(pieces: &[Piece]) -> Vec<Layout> {
    let tile = |p: &Piece| if let Piece::Tile(l, _) = p { Some(*l) } else { None };
    let mut out = Vec::with_capacity(pieces.iter().filter(|p| tile(p).is_some()).count());
    out.extend(pieces.iter().filter_map(tile));
    out
}

/// The distinct tiles among modelled tiles of layouts `all` (file order), given the references
/// `refs` (in order: a tile's place, and how many distinct tiles back the one it repeats is):
/// their layouts, and for every tile the distinct tile it is or repeats.
pub fn sources(all: &[Layout], refs: &[(usize, usize)]) -> Result<(Vec<Layout>, Vec<usize>)> {
    let mut distinct = Vec::with_capacity(all.len().saturating_sub(refs.len()));
    let mut srcs = Vec::with_capacity(all.len());
    let mut refs = refs.iter().peekable();
    for (i, &l) in all.iter().enumerate() {
        match refs.next_if(|r| r.0 == i) {
            Some(&(_, back)) => {
                let d = distinct.len().checked_sub(back).filter(|_| back > 0).ok_or_else(|| invalid("reference"))?;
                if distinct[d] != l {
                    return Err(invalid("reference"));
                }
                srcs.push(d);
            }
            None => {
                srcs.push(distinct.len());
                distinct.push(l);
            }
        }
    }
    Ok((distinct, srcs))
}

/// Largest batch.
pub const MAX_BATCH: usize = 1 << 16;

/// Bytes of what the module holds on WebAssembly (32-bit pointers), checked there by
/// assertions: a piece of the skeleton, a batch, a distinct tile's place.
pub mod wasm {
    pub const PIECE: u64 = 8;
    pub const BATCH: u64 = 12;
    pub const PLACE: u64 = 8;
    /// A `usize`.
    pub const WORD: u64 = 4;
}

#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<Piece>() as u64 == wasm::PIECE);
    assert!(size_of::<Batch>() as u64 == wasm::BATCH);
    assert!(size_of::<(usize, usize)>() as u64 == wasm::PLACE);
};

/// What the module reads of its inputs before it decodes, laid out in the order it is taken (the
/// module's memory plan follows it, see `crate::plan`).
pub struct Parsed {
    /// The runs of modelled and kept blocks of the skeleton.
    pub runs: Vec<usize>,
    /// The skeleton's walk.
    pub pieces: Vec<Piece>,
    /// The layouts of the modelled tiles, in file order.
    pub all: Vec<Layout>,
    /// The references: a tile's place, and how many distinct tiles back the one it repeats is.
    pub refs: Vec<(usize, usize)>,
    /// The distinct tiles' layouts, and for each tile of the file the distinct tile it is.
    pub distinct: Vec<Layout>,
    pub srcs: Vec<usize>,
    /// The batches, and the distinct tiles they hold ([`batches`]).
    pub batches: Vec<Batch>,
    pub order: Vec<usize>,
    /// The bytes of each batch's stream, and where they start.
    pub lens: Vec<usize>,
    pub starts: Vec<usize>,
    /// Each distinct tile's place in the file, and the length of its zlib stream.
    pub places: Vec<(usize, usize)>,
}

/// The varint at `at` of the index.
fn num(index: &[u8], at: &mut usize) -> Result<usize> {
    varint::get(index, at).ok().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| invalid("index"))
}

/// The module's parameter: the tiles a batch holds.
pub fn per_batch(params: &[u8]) -> Result<usize> {
    let mut at = 0;
    let per = varint::get(params, &mut at).map_err(|_| invalid("parameters"))? as usize;
    if at != params.len() || per == 0 || per > MAX_BATCH {
        return Err(invalid("parameters"));
    }
    Ok(per)
}

/// The index and the skeleton read, for a file of `len` bytes whose database holds `database`
/// bytes and whose tiles' streams `tiles`: every vector reserved at the size it ends at.
pub fn parse(per: usize, skeleton: &[u8], index: &[u8], database: usize, tiles: usize, len: u64) -> Result<Parsed> {
    let mut at = 0;
    let nruns = num(index, &mut at)?;
    if nruns > index.len() {
        return Err(invalid("index"));
    }
    let mut runs = Vec::with_capacity(nruns);
    for _ in 0..nruns {
        runs.push(num(index, &mut at)?);
    }
    let pieces = skeleton_pieces(skeleton, &runs)?;
    let all = layouts(&pieces);
    // the references: where (the tiles skipped since the last) and to which distinct tile back
    let nrefs = num(index, &mut at)?;
    if nrefs > index.len() {
        return Err(invalid("index"));
    }
    let mut refs = Vec::with_capacity(nrefs);
    let mut next = 0usize;
    for _ in 0..nrefs {
        let p = next.checked_add(num(index, &mut at)?).filter(|&p| p < all.len()).ok_or_else(|| invalid("index"))?;
        refs.push((p, num(index, &mut at)?));
        next = p + 1;
    }
    let (distinct, srcs) = sources(&all, &refs)?;
    let (batches, order) = batches(&distinct, per);
    let mut lens = Vec::with_capacity(batches.len());
    for _ in 0..batches.len() {
        lens.push(num(index, &mut at)?);
    }
    if at != index.len() {
        return Err(invalid("index"));
    }
    let mut starts = Vec::with_capacity(lens.len());
    let mut s = 0usize;
    for &n in &lens {
        starts.push(s);
        s = s.checked_add(n).ok_or_else(|| invalid("index"))?;
    }
    if s != tiles {
        return Err(invalid("tiles"));
    }
    // where each distinct tile goes in the file, as the pieces lay it out
    let len = usize::try_from(len).map_err(|_| invalid("length"))?;
    let mut places = vec![(0, 0); distinct.len()];
    let (mut pos, mut kept, mut t, mut seen) = (0usize, 0usize, 0usize, 0usize);
    for p in &pieces {
        match *p {
            Piece::Keep(n) => {
                kept += n;
                pos += n;
            }
            Piece::Tile(_, n) => {
                let d = srcs[t];
                if d == seen {
                    places[d] = (pos, n);
                    seen += 1;
                } else if places[d].1 != n {
                    return Err(invalid("a tile's stream"));
                }
                t += 1;
                pos += n;
            }
            Piece::Database(n) => {
                if database != n {
                    return Err(invalid("database"));
                }
                pos += n;
            }
        }
    }
    if kept != skeleton.len() || pos != len {
        return Err(invalid("length"));
    }
    Ok(Parsed { runs, pieces, all, refs, distinct, srcs, batches, order, lens, starts, places })
}

/// The module: the file of `len` bytes from the skeleton, the index (the runs of modelled and
/// kept blocks, the repeated tiles, then the bytes of each batch's stream), the tiles' streams
/// and the database;
/// the parameter is the tiles a batch holds. `threads`: the batches decoded at once (the same
/// bytes whatever the number).
///
/// The memory is laid out as a stack, in the order of the code (`crate::plan` follows it): the
/// vectors of [`parse`], then the file, which the batches (as tasks, given each one's zlib
/// streams as bytes) fill in the places of their tiles, the repeated tiles last.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[skeleton, index, tiles, database] = inputs else {
        return Err(invalid("inputs"));
    };
    let per = per_batch(params)?;
    let p = parse(per, skeleton, index, database.len(), tiles.len(), len)?;
    let out = std::cell::RefCell::new(Vec::new());
    pool::par_each_apart(
        p.batches.len(),
        threads,
        2,
        |b| decode_batch(&p, b, &tiles[p.starts[b]..p.starts[b] + p.lens[b]]).unwrap_or_default(),
        // the file but its tiles (after the tasks are spawned: they do not copy it)
        || {
            let mut o = vec![0u8; len as usize];
            let (mut pos, mut s) = (0usize, 0usize);
            for piece in &p.pieces {
                match *piece {
                    Piece::Keep(n) => {
                        o[pos..pos + n].copy_from_slice(&skeleton[s..s + n]);
                        s += n;
                        pos += n;
                    }
                    Piece::Tile(_, n) => pos += n,
                    Piece::Database(n) => {
                        o[pos..pos + n].copy_from_slice(database);
                        pos += n;
                    }
                }
            }
            *out.borrow_mut() = o;
        },
        |b, bytes| {
            let Batch { from, n, .. } = p.batches[b];
            let places = &p.order[from..from + n];
            if bytes.is_empty() || bytes.len() != places.iter().map(|&i| p.places[i].1).sum::<usize>() {
                return Err(invalid("tiles"));
            }
            let (mut o, mut at) = (out.borrow_mut(), 0);
            for &i in places {
                let (pos, n) = p.places[i];
                o[pos..pos + n].copy_from_slice(&bytes[at..at + n]);
                at += n;
            }
            Ok(())
        },
    )?;
    let mut out = out.into_inner();
    // the repeated tiles, after the tile they repeat
    let (mut pos, mut t, mut seen) = (0usize, 0usize, 0usize);
    for piece in &p.pieces {
        match *piece {
            Piece::Keep(n) | Piece::Database(n) => pos += n,
            Piece::Tile(_, n) => {
                let d = p.srcs[t];
                if d == seen {
                    seen += 1;
                } else {
                    let from = p.places[d].0;
                    out.copy_within(from..from + n, pos);
                }
                t += 1;
                pos += n;
            }
        }
    }
    Ok(out)
}

/// The zlib streams of batch `b`, one after the other, from its stream of arithmetic code
/// `stream`; `None` if it is corrupt or a stream is not the length its tile has in the file. The
/// bytes are reserved with the room the writer asks past them.
pub fn decode_batch(p: &Parsed, b: usize, stream: &[u8]) -> Option<Vec<u8>> {
    let Batch { layout: l, from, n } = p.batches[b];
    let places = &p.order[from..from + n];
    let mut m = Model::new(Escape::Med, true);
    let mut d = dynamic::Dec(ArithDecoder::new(stream));
    let (mut px, mut t) = (vec![0u64; AREA], Vec::with_capacity(l.bytes()));
    let sum: usize = places.iter().map(|&i| p.places[i].1).sum();
    let mut out = Vec::with_capacity(sum + crate::zlib::ROOM);
    for &i in places {
        m.tile(&mut d, l, &mut px);
        tile::unpixels(l, &px, &mut t);
        let before = out.len();
        out = crate::zlib::deflate_onto(&t, out)?;
        if out.len() - before != p.places[i].1 {
            return None;
        }
    }
    (!d.0.overrun()).then_some(out)
}
