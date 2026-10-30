//! aqr-wim as an entry formula (see aqr-formula): the file as the concatenation of its pieces,
//! in order: the rest (header, chunk tables, lookup table, XML data, what no resource is) a
//! slice of `wim/rest`; each resource of no compression a slice of the content stream `raw`; the
//! chunks of each LZX resource outputs of aqr-lzx's module, of its data in `raw` (each stream's
//! data a slice, a content); chunks aqr-lzx does not take a slice of `wim/kept`. `FORMAT.md`
//! tells of the streams.

use std::ops::Range;
use std::sync::Mutex;

use aqr_formula::{Formula, Kind, Node};

use crate::wim::{self, Resource};

/// The class of the stream of the rest of the file, stream 1.
pub const REST: &str = "wim/rest";

/// Chunks of a stream of aqr-lzx at most (32 MiB of data): a resource of more is several.
pub const STREAM_CHUNKS: usize = 1024;
/// Bytes of data of an application of aqr-lzx's module at most (but for one stream alone), so
/// that its memory is bounded however large the file: its streams are one after another.
pub const APPLY_DATA: usize = 32 << 20;

/// The formula of WIM file `input`, or None where it is none (of LZX or no compression) or has
/// no resource taken apart. Chunks are split on up to `threads` threads.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let w = wim::parse(input)?;
    if w.resources.is_empty() {
        return None;
    }
    // each resource of chunks as streams of at most [`STREAM_CHUNKS`], and where the data of
    // each stream and each resource of no compression goes in `raw`
    let mut streams: Vec<Vec<(&[u8], usize)>> = Vec::new();
    let mut of: Vec<Range<usize>> = Vec::new();
    let mut total = 0;
    for r in &w.resources {
        let first = streams.len();
        match r {
            Resource::Chunks { chunks, .. } => {
                for part in chunks.chunks(STREAM_CHUNKS) {
                    let s: Vec<(&[u8], usize)> = part.iter().map(|(c, n)| (&input[c.clone()], *n)).collect();
                    total += aqr_lzx::recons::data_len(&s);
                    streams.push(s);
                }
            }
            Resource::Data(d) => total += d.len(),
        }
        of.push(first..streams.len());
    }
    // (the data written where it goes, all of it held once: writing each stream's data
    // somewhere of its own first made the threads wait on each other for the memory)
    let mut raw = vec![0u8; total];
    let mut outs: Vec<Mutex<Option<&mut [u8]>>> = Vec::with_capacity(streams.len());
    let mut left = &mut raw[..];
    for (r, o) in w.resources.iter().zip(&of) {
        if let Resource::Data(d) = r {
            let (a, b) = std::mem::take(&mut left).split_at_mut(d.len());
            a.copy_from_slice(&input[d.clone()]);
            left = b;
        }
        for s in &streams[o.clone()] {
            let (a, b) = std::mem::take(&mut left).split_at_mut(aqr_lzx::recons::data_len(s));
            outs.push(Mutex::new(Some(a)));
            left = b;
        }
    }
    let splits = aqr_common::pool::par_map(streams.len(), threads, |i| {
        let out = outs[i].lock().unwrap().take().expect("a stream's data");
        aqr_lzx::recons::split_into(&streams[i], out).ok().map(|(record, _)| record)
    });
    drop(outs);

    let mut f = Formula::default();
    f.push_content("raw", Kind::Unknown, Vec::new());
    f.push(REST, Vec::new());
    f.push_as("wim/kept", Kind::Compressed, Vec::new());
    let (mut rest, mut kept) = (Vec::new(), Vec::new());
    // the pieces: a slice of a stream (0, 1, 2), or the output of the `i`-th stream of chunks
    enum Piece {
        Slice(usize, usize, usize),
        Lzx(usize),
    }
    let mut pieces = Vec::new();
    let mut lzx: Vec<(Node, Vec<u8>, u64)> = Vec::new();
    let slice = |pieces: &mut Vec<Piece>, s: usize, buf: &mut Vec<u8>, r: Range<usize>| {
        if !r.is_empty() {
            pieces.push(Piece::Slice(s, buf.len(), r.len()));
            buf.extend_from_slice(&input[r]);
        }
    };
    let mut splits: Vec<Option<Vec<u8>>> = splits;
    // where the data read goes in `raw`, and where it is kept (the data of a resource not
    // taken left out)
    let (mut read, mut to) = (0, 0);
    let mut at = 0;
    for (r, of) in w.resources.iter().zip(of) {
        let span = r.span();
        slice(&mut pieces, 1, &mut rest, at..span.start);
        match r {
            Resource::Data(d) => {
                raw.copy_within(read..read + d.len(), to);
                pieces.push(Piece::Slice(0, to, d.len()));
                (read, to) = (read + d.len(), to + d.len());
            }
            Resource::Chunks { table, chunks } => {
                slice(&mut pieces, 1, &mut rest, table.clone());
                let n: usize = streams[of.clone()].iter().map(|s| aqr_lzx::recons::data_len(s)).sum();
                // (a resource of a stream not split kept whole)
                if splits[of.clone()].iter().all(Option::is_some) {
                    raw.copy_within(read..read + n, to);
                    let mut t = to;
                    for (i, part) in of.clone().zip(chunks.chunks(STREAM_CHUNKS)) {
                        let len: usize = part.iter().map(|c| c.0.len()).sum();
                        let m = aqr_lzx::recons::data_len(&streams[i]);
                        pieces.push(Piece::Lzx(lzx.len()));
                        lzx.push((Node::slice(0, t as u64, m as u64), splits[i].take().expect("split"), len as u64));
                        t += m;
                    }
                    to += n;
                } else {
                    slice(&mut pieces, 2, &mut kept, table.end..span.end);
                }
                read += n;
            }
        }
        at = span.end;
    }
    slice(&mut pieces, 1, &mut rest, at..input.len());
    raw.truncate(to);
    if lzx.is_empty() && !pieces.iter().any(|p| matches!(p, Piece::Slice(0, ..))) {
        return None;
    }
    f.streams[0].data = raw.into();
    f.streams[1].data = rest.into();
    f.streams[2].data = kept.into();
    // the applications, each of streams of at most [`APPLY_DATA`] bytes of data in all
    let mut outputs: Vec<Option<Node>> = Vec::with_capacity(lzx.len());
    let mut group: Vec<(Node, Vec<u8>, u64)> = Vec::new();
    let mut bytes = 0;
    for s in lzx {
        let n = s.0.len() as usize;
        if !group.is_empty() && bytes + n > APPLY_DATA {
            outputs.extend(aqr_lzx::formula::apply(&mut f, std::mem::take(&mut group)).into_iter().map(Some));
            bytes = 0;
        }
        bytes += n;
        group.push(s);
    }
    if !group.is_empty() {
        outputs.extend(aqr_lzx::formula::apply(&mut f, group).into_iter().map(Some));
    }
    f.root = Node::concat(pieces.into_iter().map(|p| match p {
        Piece::Slice(s, a, n) => Node::slice(s, a as u64, n as u64),
        Piece::Lzx(i) => outputs[i].take().expect("an output each"),
    }));
    debug_assert_eq!(f.root.len(), input.len() as u64);
    Some(f)
}
