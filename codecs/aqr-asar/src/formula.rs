//! aqr-asar as a module of entry formulas (see aqr-formula): an asar archive rebuilt from its
//! data (the bytes after the header), the header with holes where its values follow from the
//! data, and the index of the files. `FORMAT.md` specifies it. The data is sliced into the
//! files' contents, which may be recompressed in turn.

use std::collections::HashMap;
use std::ops::Range;

use aqr_formula::{Formula, Kind, Module, Node};

use crate::json::{self, Value};
use crate::layout::{self, BLOCKS, Body, HASH, HOLE, OFFSET, PREFIX, SIZE};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("asar-recons", 1).with_memory(memory);

/// The classes of the header (with holes) and of the index, streams 1 and 2.
pub const HEADER: &str = "asar/header";
pub const INDEX: &str = "asar/index";

/// The memory of the module: the inputs and the archive, and the bodies of the index (16 bytes
/// each, of two bytes of the index at least).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, _, index] = u.inputs else { return None };
    Some(u.whole(index * 8, 1))
}

/// Bodies of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them, so that the same files elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

#[derive(Debug, Clone)]
pub struct Options {
    /// What a file's contents are by its path (`/`-separated, from the root, names as the JSON
    /// writes them): the contents of each [`Kind`] are a stream of their own (those of
    /// [`Kind::Unknown`] the stream `raw`, the others after the index), sliced in order. By
    /// default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
    /// Bodies of at least this many bytes are slices of their own; by default [`OWN_MIN`].
    pub own: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options { kind: |_| Kind::Unknown, own: OWN_MIN }
    }
}

/// A file of the header with its contents in the data.
struct Entry<'a> {
    name: Vec<u8>,
    body: Body,
    v: &'a Value,
}

/// `v`'s text as a whole number (between its quotes where `inner`), if it is one, and whether
/// it is written as [`layout`] writes it (without leading zeros).
fn whole(text: &[u8], v: &Value, inner: bool) -> Option<(u64, bool)> {
    let span = if inner { v.inner()? } else { matches!(v.kind, json::Kind::Number).then(|| v.span.clone())? };
    let t = &text[span];
    if t.is_empty() || !t.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = std::str::from_utf8(t).unwrap().parse().ok()?;
    Some((n, t == b"0" || t[0] != b'0'))
}

/// [`whole`], where it is written as [`layout`] writes it.
fn canonical(text: &[u8], v: &Value, inner: bool) -> Option<u64> {
    whole(text, v, inner).filter(|w| w.1).map(|w| w.0)
}

/// The files under directory `dir` (named `path`) with their contents in data of `n` bytes, in
/// the header's order.
fn walk<'a>(text: &[u8], dir: &'a Value, path: &[u8], n: usize, out: &mut Vec<Entry<'a>>) {
    let Some(json::Kind::Object(ms)) = dir.get(text, b"files").map(|f| &f.kind) else { return };
    for (k, v) in ms {
        let name = [path, b"/", &text[k.clone()]].concat();
        if v.get(text, b"files").is_some() {
            walk(text, v, &name, n, out);
            continue;
        }
        let unpacked = v.get(text, b"unpacked").is_some_and(|u| &text[u.span.clone()] == b"true");
        let size = v.get(text, b"size").and_then(|s| whole(text, s, false)).map(|w| w.0);
        let offset = v.get(text, b"offset").and_then(|o| whole(text, o, true)).map(|w| w.0);
        if let (false, Some(size), Some(offset)) = (unpacked, size, offset)
            && offset.checked_add(size).is_some_and(|e| e <= n as u64)
        {
            out.push(Entry { name, body: Body { offset, size }, v });
        }
    }
}

/// The files of asar archive `d` with their contents in it: their paths (as [`Options::kind`]
/// takes them) and the spans of their contents in `d`, in the header's order. Empty where it is
/// not an archive.
pub fn files(d: &[u8]) -> Vec<(Vec<u8>, Range<usize>)> {
    let Some((json, start)) = crate::header(d) else { return Vec::new() };
    let text = &d[json];
    let Some(root) = json::parse(text) else { return Vec::new() };
    let mut es = Vec::new();
    walk(text, &root, b"", d.len() - start, &mut es);
    es.into_iter()
        .map(|e| (e.name, start + e.body.offset as usize..start + (e.body.offset + e.body.size) as usize))
        .collect()
}

/// What a hole is, as the encoder finds it.
enum Hole {
    Size(Body),
    Offset,
    Hash,
    Blocks(u64),
}

/// The holes of `e`'s values that follow from its contents `d`: none unless its size is
/// canonical, and only those after the size.
fn holes(text: &[u8], e: &Entry, d: &[u8], out: &mut Vec<(Range<usize>, Hole)>) {
    let Some(size) = e.v.get(text, b"size").filter(|s| canonical(text, s, false).is_some()) else { return };
    let after = |v: &&Value| v.span.start > size.span.start;
    out.push((size.span.clone(), Hole::Size(e.body)));
    if let Some(o) = e.v.get(text, b"offset").filter(after).filter(|o| canonical(text, o, true).is_some()) {
        out.push((o.inner().unwrap(), Hole::Offset));
    }
    let Some(int) = e.v.get(text, b"integrity").filter(after) else { return };
    let mut expect = Vec::with_capacity(64);
    if let Some(h) = int.get(text, b"hash").and_then(Value::inner) {
        layout::hash(&mut expect, d);
        if text[h.clone()] == expect[..] {
            out.push((h, Hole::Hash));
        }
    }
    let size = int.get(text, b"blockSize").and_then(|s| canonical(text, s, false)).filter(|&s| s > 0);
    if let (Some(bs), Some(b)) = (size, int.get(text, b"blocks").filter(|b| matches!(b.kind, json::Kind::Array(_)))) {
        let inner = b.span.start + 1..b.span.end - 1;
        expect.clear();
        if (bs as usize) >= d.len() && !d.is_empty() {
            expect.push(b'"');
            layout::hash(&mut expect, d);
            expect.push(b'"');
        } else {
            layout::blocks(&mut expect, d, bs.min(d.len().max(1) as u64) as usize);
        }
        if text[inner.clone()] == expect[..] {
            out.push((inner, Hole::Blocks(bs)));
        }
    }
}

/// The formula rebuilding asar archive `input`, or None where it is not one ([`crate::header`],
/// of JSON telling a file). The same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    encode_with(input, &Options::default())
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let (span, start) = crate::header(input)?;
    let text = &input[span.clone()];
    if text.contains(&HOLE) {
        return None;
    }
    let root = json::parse(text)?;
    let data = &input[start..];
    let mut es = Vec::new();
    walk(text, &root, b"", data.len(), &mut es);
    if es.is_empty() {
        return None;
    }
    let mut hs = Vec::new();
    for e in &es {
        holes(text, e, &data[e.body.offset as usize..][..e.body.size as usize], &mut hs);
    }
    hs.sort_by_key(|h| h.0.start);
    let mut header = Vec::with_capacity(text.len());
    let (mut at, mut bodies) = (0, Vec::new());
    for (r, h) in hs {
        header.extend_from_slice(&text[at..r.start]);
        header.push(HOLE);
        match h {
            Hole::Size(b) => {
                header.push(SIZE);
                bodies.push(b);
            }
            Hole::Offset => header.push(OFFSET),
            Hole::Hash => header.push(HASH),
            Hole::Blocks(bs) => {
                header.push(BLOCKS);
                aqr_common::varint::put(&mut header, bs);
            }
        }
        at = r.end;
    }
    header.extend_from_slice(&text[at..]);
    let head: &[u8; PREFIX] = input.first_chunk()?;
    let index = layout::index(&bodies, head, text.len(), &input[span.end..start]);
    // The header must give the archive back: a check of everything, cheap but for the hashes.
    if layout::build(data, &header, &index, input.len() as u64).ok().as_deref() != Some(input) {
        return None;
    }

    // the data by kind: the files' contents in order, what lies between them unknown; contents
    // the same as a file's before (of at least `own` bytes) taken from its slices
    let mut by_offset: Vec<&Entry> = es.iter().collect();
    by_offset.sort_by_key(|e| e.body.offset);
    // (each piece of the data, its kind, and the piece it is a copy of)
    let mut pieces: Vec<(Range<usize>, Kind, Option<usize>)> = Vec::with_capacity(es.len() + 1);
    let mut seen: HashMap<&[u8], usize> = HashMap::new();
    let mut end = 0;
    for e in by_offset {
        let r = e.body.offset as usize..(e.body.offset + e.body.size) as usize;
        if r.start < end {
            continue;
        }
        if r.start > end {
            pieces.push((end..r.start, Kind::Unknown, None));
        }
        end = r.end;
        let first = match r.len() >= opt.own {
            true => *seen.entry(&data[r.clone()]).or_insert(pieces.len()),
            false => pieces.len(),
        };
        pieces.push((r, (opt.kind)(&e.name), (first < pieces.len()).then_some(first)));
    }
    if end < data.len() {
        pieces.push((end..data.len(), Kind::Unknown, None));
    }
    // (the pieces kept, one after another, and where each starts in them)
    let mut kept = Vec::with_capacity(data.len());
    let (mut ends, mut at) = (Vec::with_capacity(pieces.len()), vec![0; pieces.len()]);
    for (k, (r, kind, copy)) in pieces.iter().enumerate() {
        if copy.is_none() {
            at[k] = kept.len();
            kept.extend_from_slice(&data[r.clone()]);
            ends.push((*kind, kept.len()));
        }
    }
    let mut f = Formula::default();
    let mut inputs = vec![f.push("raw", Vec::new()), f.push(HEADER, header), f.push(INDEX, index)];
    let copies = ends.len() < pieces.len();
    inputs[0] = f.push_by_kind(0, kept, &ends, "asar", opt.own);
    if copies {
        inputs[0] = with_copies(&inputs[0], &pieces, &at);
    }
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The data of `pieces` from `node`, the slices of the pieces kept (each starting at `at` in
/// them), one after another: a copy of a piece the slices of that piece.
fn with_copies(node: &Node, pieces: &[(Range<usize>, Kind, Option<usize>)], at: &[usize]) -> Node {
    // (each slice of the pieces kept, by where it starts in them)
    let mut slices = Vec::new();
    let mut start = 0;
    let mut add = |n: &Node| {
        let &Node::Slice { stream, start: a, len } = n else { unreachable!("slices") };
        slices.push((start, stream, a, len));
        start += len;
    };
    match node {
        Node::Concat(ns) => ns.iter().for_each(&mut add),
        n => add(n),
    }
    let take = |mut a: u64, mut l: u64, out: &mut Vec<Node>| {
        let mut k = slices.partition_point(|s| s.0 <= a).saturating_sub(1);
        while l > 0 {
            let (s0, stream, start, len) = slices[k];
            let m = l.min(s0 + len - a);
            out.push(Node::slice(stream, start + a - s0, m));
            (a, l, k) = (a + m, l - m, k + 1);
        }
    };
    let mut out = Vec::with_capacity(slices.len() + pieces.len());
    for (k, (r, _, copy)) in pieces.iter().enumerate() {
        take(at[copy.unwrap_or(k)] as u64, r.len() as u64, &mut out);
    }
    Node::concat(out)
}
