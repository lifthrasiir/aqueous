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
    let mut out: Vec<Piece> = Vec::new();
    let keep = |out: &mut Vec<Piece>, n: usize| match out.last_mut() {
        Some(Piece::Keep(m)) => *m += n,
        _ => out.push(Piece::Keep(n)),
    };
    if !src.starts_with(b"CSFCHUNK") {
        return Err(invalid("not a .clip file"));
    }
    let first = u64be(src, 16)?;
    if first < 24 || first > src.len() {
        return Err(invalid("header"));
    }
    keep(&mut out, first);
    // p: in src; the walk ends where src does
    let (mut p, mut database) = (first, false);
    while p < src.len() {
        if src.get(p..p + 4) != Some(b"CHNK") {
            return Err(invalid("chunk"));
        }
        let kind = src.get(p + 4..p + 8).ok_or_else(|| invalid("truncated"))?;
        let n = u64be(src, p + 8)?;
        keep(&mut out, 16);
        p += 16;
        let body_end =
            |p: usize, n: usize| p.checked_add(n).filter(|&e| e <= src.len()).ok_or_else(|| invalid("truncated"));
        if kind == b"SQLi" {
            if database {
                return Err(invalid("two databases"));
            }
            database = true;
            out.push(Piece::Database(n));
            if whole {
                p = body_end(p, n)?;
            }
            continue;
        }
        if kind != b"Exta" {
            keep(&mut out, n);
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
            keep(&mut out, n);
            p = body_end(p, n)?;
            continue;
        }
        keep(&mut out, head);
        // o: bytes of the data so far, in the file
        let (mut q, mut o) = (data, 0usize);
        while o < size {
            if let Some(r) = string_is(src, q + 4, BEGIN) {
                let rec = u32be(src, q)? as usize;
                let f: Vec<u32> = (0..5).map(|i| u32be(src, r + 4 * i)).collect::<Result<_>>()?;
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
                            keep(&mut out, r + 8 - q);
                            out.push(Piece::Tile(l, zl));
                            r += 8 + if whole { zl } else { 0 };
                            (from, absent) = (r, zl);
                        }
                        _ => r += 8 + zl,
                    }
                }
                let e = string_is(src, r, END).ok_or_else(|| invalid("block end"))?;
                keep(&mut out, e - from);
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
                keep(&mut out, t);
                q += t;
                o += t;
            }
        }
        if o != size {
            return Err(invalid("block data"));
        }
        p = q;
    }
    Ok(out)
}

/// The batches of the modelled tiles (of layouts `tiles`, in file order): the tiles of each
/// layout in file order, `per` at a time, the layouts in the order colour, one plane, two planes.
/// Each batch is its layout and the tiles' places in `tiles`.
pub fn batches(tiles: &[Layout], per: usize) -> Vec<(Layout, Vec<usize>)> {
    let mut out = Vec::new();
    for l in [Layout::Color, Layout::One, Layout::Two] {
        let of: Vec<usize> = (0..tiles.len()).filter(|&i| tiles[i] == l).collect();
        out.extend(of.chunks(per.max(1)).map(|c| (l, c.to_vec())));
    }
    out
}

/// The layouts of the modelled tiles of `pieces`.
pub fn layouts(pieces: &[Piece]) -> Vec<Layout> {
    pieces.iter().filter_map(|p| if let Piece::Tile(l, _) = p { Some(*l) } else { None }).collect()
}

/// The distinct tiles among modelled tiles of layouts `all` (file order), given the references
/// `refs` (in order: a tile's place, and how many distinct tiles back the one it repeats is):
/// their layouts, and for every tile the distinct tile it is or repeats.
pub fn sources(all: &[Layout], refs: &[(usize, usize)]) -> Result<(Vec<Layout>, Vec<usize>)> {
    let (mut distinct, mut srcs) = (Vec::new(), Vec::with_capacity(all.len()));
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

/// The module: the file of `len` bytes from the skeleton, the index (the runs of modelled and
/// kept blocks, the repeated tiles, then the bytes of each batch's stream), the tiles' streams
/// and the database;
/// the parameter is the tiles a batch holds. `threads`: the batches decoded at once (the same
/// bytes whatever the number).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    let &[skeleton, index, tiles, database] = inputs else {
        return Err(invalid("inputs"));
    };
    let mut at = 0;
    let per = varint::get(params, &mut at).map_err(|_| invalid("parameters"))? as usize;
    if at != params.len() || per == 0 || per > MAX_BATCH {
        return Err(invalid("parameters"));
    }
    // the index
    let mut at = 0;
    let nruns = num(index, &mut at)?;
    if nruns > index.len() {
        return Err(invalid("index"));
    }
    let runs: Vec<usize> = (0..nruns).map(|_| num(index, &mut at)).collect::<Result<_>>()?;
    let (mut run, mut left, mut on) = (0, runs.first().copied().unwrap_or(0), true);
    let mut flags = |_: Layout, _: &[u8]| -> bool {
        while left == 0 && run + 1 < runs.len() {
            run += 1;
            left = runs[run];
            on = !on;
        }
        if left == 0 {
            // past the runs: kept
            return false;
        }
        left -= 1;
        on
    };
    let pieces = pieces(skeleton, false, &mut flags)?;
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
    let bs = batches(&distinct, per);
    let lens: Vec<usize> = (0..bs.len()).map(|_| num(index, &mut at)).collect::<Result<_>>()?;
    if at != index.len() {
        return Err(invalid("index"));
    }
    let mut starts = Vec::with_capacity(lens.len() + 1);
    let mut s = 0usize;
    for &n in &lens {
        starts.push(s);
        s = s.checked_add(n).ok_or_else(|| invalid("index"))?;
    }
    if s != tiles.len() {
        return Err(invalid("tiles"));
    }
    let streams = pool::par_map_apart(bs.len(), threads, |b| {
        decode_batch(bs[b].0, bs[b].1.len(), &tiles[starts[b]..starts[b] + lens[b]])
    });
    // each distinct tile's stream, by its place among them
    let mut zs: Vec<&[u8]> = vec![&[]; distinct.len()];
    for ((_, places), st) in bs.iter().zip(&streams) {
        let st = st.as_ref().ok_or_else(|| invalid("tiles"))?;
        for (&i, z) in places.iter().zip(st) {
            zs[i] = z;
        }
    }
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| invalid("length"))?);
    let (mut s, mut t) = (0usize, 0usize);
    for p in &pieces {
        match *p {
            Piece::Keep(n) => {
                out.extend_from_slice(skeleton.get(s..s + n).ok_or_else(|| invalid("skeleton"))?);
                s += n;
            }
            Piece::Tile(_, n) => {
                let z = zs[srcs[t]];
                if z.len() != n {
                    return Err(invalid("a tile's stream"));
                }
                out.extend_from_slice(z);
                t += 1;
            }
            Piece::Database(n) => {
                if database.len() != n {
                    return Err(invalid("database"));
                }
                out.extend_from_slice(database);
            }
        }
    }
    if s != skeleton.len() || out.len() as u64 != len {
        return Err(invalid("length"));
    }
    Ok(out)
}

/// The varint at `at` of the index.
fn num(index: &[u8], at: &mut usize) -> Result<usize> {
    varint::get(index, at).ok().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| invalid("index"))
}

/// The zlib streams of a batch of `n` tiles of layout `l`; `None` if its stream is corrupt.
pub fn decode_batch(l: Layout, n: usize, stream: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut m = Model::new(Escape::Med, true);
    let mut d = dynamic::Dec(ArithDecoder::new(stream));
    let (mut px, mut t, mut out) = (vec![0u64; AREA], Vec::new(), Vec::with_capacity(n));
    for _ in 0..n {
        m.tile(&mut d, l, &mut px);
        tile::unpixels(l, &px, &mut t);
        out.push(crate::zlib::deflate(&t)?);
    }
    (!d.0.overrun()).then_some(out)
}
