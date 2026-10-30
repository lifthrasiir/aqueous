//! aqr-wim as an entry formula (see aqr-formula): the file as applications of aqr-lzx's
//! lzx-recons one after another, each giving a part of the file as it is: streams of chunks
//! rebuilt from their data (in the content stream `raw`, a slice each, a content) and records,
//! and the bytes between them (header, chunk tables, lookup table, XML data, resources not
//! taken) from `wim/rest`. A resource of no compression is a slice of `raw` between them, whole
//! (a WIM file in it is one). `FORMAT.md` tells of the streams.

use std::ops::Range;
use std::sync::Mutex;

use aqr_formula::{Formula, Kind, Node};
use aqr_lzx::formula::Stream;

use crate::wim::{self, Resource};

/// The class of the stream of the bytes between the streams of chunks, stream 1.
pub const REST: &str = "wim/rest";

/// Chunks of a stream of aqr-lzx at most (32 MiB of data): a resource of more is several.
pub const STREAM_CHUNKS: usize = 1024;
/// Bytes of data of an application of aqr-lzx's module at most (but for one stream alone), so
/// that its memory is bounded however large the file.
pub const APPLY_DATA: usize = 32 << 20;

/// A stream of chunks of a resource (its chunks, their bytes and their data's lengths), or a
/// resource of no compression (no chunks): where it is in the file, and the resource's index.
struct Part<'a> {
    chunks: Vec<(&'a [u8], usize)>,
    span: Range<usize>,
    resource: usize,
}

/// A piece of the formula's root: an application (its streams, where its part of the file
/// starts), or a slice.
enum Piece {
    App(Vec<Stream>, usize),
    Slice(Node),
}

/// The formula of WIM file `input`, or None where it is none (of LZX or no compression) or has
/// no resource taken apart. Chunks are split on up to `threads` threads.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let w = wim::parse(input)?;
    // the streams of chunks of each resource, of at most [`STREAM_CHUNKS`], and the resources of
    // no compression
    let mut parts: Vec<Part> = Vec::new();
    for (i, r) in w.resources.iter().enumerate() {
        match r {
            Resource::Chunks { chunks, .. } => {
                for c in chunks.chunks(STREAM_CHUNKS) {
                    let span = c[0].0.start..c[c.len() - 1].0.end;
                    let chunks = c.iter().map(|(r, n)| (&input[r.clone()], *n)).collect();
                    parts.push(Part { chunks, span, resource: i });
                }
            }
            Resource::Data(d) => parts.push(Part { chunks: Vec::new(), span: d.clone(), resource: i }),
        }
    }
    if parts.is_empty() {
        return None;
    }
    // (the data written where it goes, all of it held once: writing each stream's data
    // somewhere of its own first made the threads wait on each other for the memory)
    let stored = |p: &Part| p.chunks.is_empty();
    let lens: Vec<usize> =
        parts.iter().map(|p| if stored(p) { p.span.len() } else { aqr_lzx::recons::data_len(&p.chunks) }).collect();
    let mut raw = vec![0u8; lens.iter().sum()];
    let mut outs: Vec<Mutex<Option<&mut [u8]>>> = Vec::with_capacity(parts.len());
    let mut left = &mut raw[..];
    for &n in &lens {
        let (a, b) = std::mem::take(&mut left).split_at_mut(n);
        outs.push(Mutex::new(Some(a)));
        left = b;
    }
    let records = aqr_common::pool::par_map(parts.len(), threads, |i| {
        let out = outs[i].lock().unwrap().take().expect("a stream's data");
        if stored(&parts[i]) {
            out.copy_from_slice(&input[parts[i].span.clone()]);
            return Some(Vec::new());
        }
        aqr_lzx::recons::split_into(&parts[i].chunks, out).ok().map(|(record, _)| record)
    });
    drop(outs);
    // a resource of a stream not split kept whole, with the bytes around it
    let mut taken = vec![true; w.resources.len()];
    for (p, r) in parts.iter().zip(&records) {
        taken[p.resource] &= r.is_some();
    }
    if !taken.iter().any(|&t| t) {
        return None;
    }

    let mut f = Formula::default();
    f.push_content("raw", Kind::Unknown, Vec::new());
    f.push(REST, Vec::new());
    let mut rest = Vec::new();
    let kept = |rest: &mut Vec<u8>, r: Range<usize>| {
        let n = Node::slice(1, rest.len() as u64, r.len() as u64);
        rest.extend_from_slice(&input[r]);
        n
    };
    // the streams taken, their data where it is kept in `raw`, by application of at most
    // [`APPLY_DATA`] bytes of data; a resource of no compression a slice between applications
    let (mut read, mut to, mut at) = (0, 0, 0);
    let mut pieces: Vec<Piece> = Vec::new();
    let mut bytes = 0;
    // (the application open ended at `end`, the bytes kept after its last stream up to there)
    let close = |pieces: &mut Vec<Piece>, rest: &mut Vec<u8>, f: &mut Formula, at: usize, end: usize| match pieces.pop()
    {
        Some(Piece::App(streams, start)) => {
            let tail = kept(rest, at..end);
            let app = aqr_lzx::formula::apply(f, streams, tail, (end - start) as u64);
            pieces.push(Piece::Slice(app));
        }
        other => {
            pieces.extend(other);
            if at < end {
                pieces.push(Piece::Slice(kept(rest, at..end)));
            }
        }
    };
    for ((p, record), &n) in parts.iter().zip(records).zip(&lens) {
        if taken[p.resource] {
            raw.copy_within(read..read + n, to);
            let data = Node::slice(0, to as u64, n as u64);
            if stored(p) {
                close(&mut pieces, &mut rest, &mut f, at, p.span.start);
                pieces.push(Piece::Slice(data));
            } else {
                let open = matches!(pieces.last(), Some(Piece::App(..)));
                if !open || bytes > 0 && bytes + n > APPLY_DATA {
                    close(&mut pieces, &mut rest, &mut f, at, at);
                    pieces.push(Piece::App(Vec::new(), at));
                    bytes = 0;
                }
                let s = Stream { kept: kept(&mut rest, at..p.span.start), data, record: record.expect("taken") };
                let Some(Piece::App(streams, _)) = pieces.last_mut() else { unreachable!("an application open") };
                streams.push(s);
                bytes += n;
            }
            (to, at) = (to + n, p.span.end);
        }
        read += n;
    }
    close(&mut pieces, &mut rest, &mut f, at, input.len());
    raw.truncate(to);
    f.streams[0].data = raw.into();
    let nodes: Vec<Node> = pieces
        .into_iter()
        .map(|p| match p {
            Piece::Slice(n) => n,
            Piece::App(..) => unreachable!("applications closed"),
        })
        .collect();
    f.streams[1].data = rest.into();
    f.root = Node::concat(nodes);
    debug_assert_eq!(f.root.len(), input.len() as u64);
    Some(f)
}
