//! aqr-wim as an entry formula (see aqr-formula): the file as applications of aqr-lzx's
//! lzx-recons one after another, each giving a part of the file as it is: streams of chunks
//! rebuilt from their data (in the content streams `raw`, a slice each, a content) and records,
//! and the bytes between them (header, chunk tables, lookup table, XML data, resources not
//! taken) from `wim/rest`. A resource of no compression is a stream of `raw` between them, whole
//! (a WIM file in it is one). `FORMAT.md` tells of the streams.

use std::ops::Range;

use aqr_formula::store::Writer;
use aqr_formula::{Bytes, Formula, Kind, Node};
use aqr_lzx::formula::Stream;

use crate::wim::{self, Resource};

/// The class of the stream of the bytes between the streams of chunks, stream 1.
pub const REST: &str = "wim/rest";

aqr_common::zopt_namespace!("wim", "WIM images (aqr-wim)");

aqr_common::zopt! {
    /// Chunks of a stream of aqr-lzx at most (32 MiB of data): a resource of more is several.
    pub static STREAM_CHUNKS: Opt<usize> = Opt::value("wim:stream-chunks", 1024, "chunks of a stream of aqr-lzx at most");
    /// Bytes of data of an application of aqr-lzx's module at most (but for one stream alone), so
    /// that its memory is bounded however large the file.
    pub static APPLY_DATA: Opt<usize> = Opt::size("wim:apply-data", 32 << 20, "bytes of data of an application of aqr-lzx's module at most");
}

/// A stream of chunks of a resource (its chunks, their bytes and their data's lengths), or a
/// resource of no compression (no chunks): where it is in the file, and the resource's index.
struct Part<'a> {
    chunks: Vec<(&'a [u8], usize)>,
    span: Range<usize>,
    resource: usize,
}

/// A piece of the formula's root: an application (its streams, where its part of the file
/// starts, and its group of parts: [`groups`]), or a slice.
enum Piece {
    App(Vec<Stream>, usize, usize),
    Slice(Node),
}

/// The formula of WIM file `input`, or None where it is none (of LZX or no compression) or has
/// no resource taken apart. Chunks are split on up to `threads` threads.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_as(input, None, threads)
}

/// [`encode`] of WIM file `input`, its bytes shared: the data of each application not held but
/// made again from its chunks where it is needed ([`aqr_formula::Bytes::lazy`]), and runs of
/// resources of no compression `input`'s bytes shared rather than copied, so that whoever takes
/// the formula holds each application's data only while it works on it (aqr-compress's
/// make_later), rather than all of it from the start.
pub fn encode_shared(input: &Bytes, threads: usize) -> Option<Formula> {
    encode_as(input, Some(input), threads)
}

fn encode_as(input: &[u8], shared: Option<&Bytes>, threads: usize) -> Option<Formula> {
    // (the bytes shared, or a copy of them: the data is made again of them)
    let owned;
    let (whole, input) = match shared {
        Some(b) => (b, input),
        None => {
            owned = Bytes::from(input.to_vec());
            (&owned, &owned[..])
        }
    };
    let w = wim::parse(input)?;
    // the streams of chunks of each resource, of at most [`STREAM_CHUNKS`], and the resources of
    // no compression
    let mut parts: Vec<Part> = Vec::new();
    for (i, r) in w.resources.iter().enumerate() {
        match r {
            Resource::Chunks { chunks, .. } => {
                for c in chunks.chunks(STREAM_CHUNKS.get()) {
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
    let stored = |i: usize| parts[i].chunks.is_empty();
    let lens: Vec<usize> = (0..parts.len())
        .map(|i| if stored(i) { parts[i].span.len() } else { aqr_lzx::recons::data_len(&parts[i].chunks) })
        .collect();
    // the data of each application, and of each resource of no compression, a stream of `raw`:
    // not held but made again from its chunks where it is needed, so that only the parts being
    // split are held (a file claiming 2048 times its size is not planned as buffers at once)
    let spans: Vec<Range<usize>> = parts.iter().map(|p| p.span.clone()).collect();
    let records = aqr_common::pool::par_map(parts.len(), threads, |i| {
        if stored(i) {
            return Some(Vec::new());
        }
        let mut own = vec![0; lens[i]];
        aqr_lzx::recons::split_into(&parts[i].chunks, &mut own).ok()
    });
    // (the chunks read through: of a file mapped, its pages let go of, read again as the data
    // is made again of them)
    parts.iter().flat_map(|p| &p.chunks).for_each(|&(c, _)| aqr_formula::store::let_go(c));
    // a resource of a stream not split kept whole, with the bytes around it
    let mut taken = vec![true; w.resources.len()];
    for (p, r) in parts.iter().zip(&records) {
        taken[p.resource] &= r.is_some();
    }
    if !taken.iter().any(|&t| t) {
        return None;
    }

    // laid out of the parts taken
    let kept: Vec<usize> = (0..parts.len()).filter(|&i| taken[parts[i].resource]).collect();
    let laid = groups(&kept, stored, &lens, &spans);
    let (mut group_of, mut offset) = (vec![usize::MAX; parts.len()], vec![0; parts.len()]);
    let mut data: Vec<Option<Bytes>> = Vec::with_capacity(laid.len());
    for (k, g) in laid.iter().enumerate() {
        let mut at = 0;
        for &i in g {
            (group_of[i], offset[i]) = (k, at);
            at += lens[i];
        }
        data.push(Some(match stored(g[0]) {
            // (a run of them, one right after another in the file)
            true => whole.slice(spans[g[0]].start..spans[g[g.len() - 1]].end),
            false => made_again(whole, input, g.iter().map(|&i| &parts[i].chunks[..]), at),
        }));
    }

    // the first stream of `raw` stream 0 (an empty one where none is taken), the others after
    // the rest, stream 1, as the archive puts the streams of a class together in the order of
    // their indices
    let mut f = Formula::default();
    f.push_content("raw", Kind::Unknown, Vec::new());
    f.push(REST, Vec::new());
    let mut first = true;
    let mut next = |f: &mut Formula, d: Bytes| match std::mem::take(&mut first) {
        true => {
            f.streams[0].data = d;
            0
        }
        false => {
            f.push_content("raw", Kind::Unknown, d);
            f.streams.len() - 1
        }
    };
    let mut rest = Vec::new();
    let kept_rest = |rest: &mut Vec<u8>, r: Range<usize>| {
        let n = Node::slice(1, rest.len() as u64, r.len() as u64);
        rest.extend_from_slice(&input[r]);
        n
    };
    // the streams taken by application ([`groups`]); a resource of no compression a slice
    // between applications
    let mut at = 0;
    let mut pieces: Vec<Piece> = Vec::new();
    // (the application open ended at `end`, the bytes kept after its last stream up to there)
    let close = |pieces: &mut Vec<Piece>, rest: &mut Vec<u8>, f: &mut Formula, at: usize, end: usize| match pieces.pop()
    {
        Some(Piece::App(streams, start, _)) => {
            let tail = kept_rest(rest, at..end);
            let app = aqr_lzx::formula::apply(f, streams, tail, (end - start) as u64);
            pieces.push(Piece::Slice(app));
        }
        other => {
            pieces.extend(other);
            if at < end {
                pieces.push(Piece::Slice(kept_rest(rest, at..end)));
            }
        }
    };
    // (the stream of the group open, of resources of no compression)
    let (mut stream, mut stored_open) = (0, None);
    for (i, (p, record)) in parts.iter().zip(records).enumerate() {
        if !taken[p.resource] {
            continue;
        }
        let (g, n) = (group_of[i], lens[i]);
        if stored(i) {
            close(&mut pieces, &mut rest, &mut f, at, p.span.start);
            let s = match stored_open {
                Some((open, s)) if open == g => s,
                _ => next(&mut f, data[g].take().expect("its data")),
            };
            stored_open = Some((g, s));
            pieces.push(Piece::Slice(Node::slice(s, offset[i] as u64, n as u64)));
        } else {
            stored_open = None;
            if !matches!(pieces.last(), Some(&Piece::App(_, _, open)) if open == g) {
                close(&mut pieces, &mut rest, &mut f, at, at);
                stream = next(&mut f, data[g].take().expect("its data"));
                pieces.push(Piece::App(Vec::new(), at, g));
            }
            let kept = kept_rest(&mut rest, at..p.span.start);
            let data = Node::slice(stream, offset[i] as u64, n as u64);
            let s = Stream { kept, data, record: record.expect("taken") };
            let Some(Piece::App(streams, ..)) = pieces.last_mut() else { unreachable!("an application open") };
            streams.push(s);
        }
        at = p.span.end;
    }
    close(&mut pieces, &mut rest, &mut f, at, input.len());
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
    if shared.is_none() {
        // (the data held, of the parts that were split: the whole of what they gave)
        f.streams.iter_mut().for_each(|s| s.data = s.data.made());
    }
    Some(f)
}

/// The data of the streams of chunks `parts` (of `input`, whose bytes `whole` shares), `len`
/// bytes, made again from the chunks where it is needed ([`encode_shared`]).
fn made_again<'a>(
    whole: &Bytes,
    input: &[u8],
    parts: impl Iterator<Item = &'a [(&'a [u8], usize)]>,
    len: usize,
) -> Bytes {
    let at = |c: &[u8]| c.as_ptr() as usize - input.as_ptr() as usize;
    let parts: Vec<Vec<(Range<usize>, usize)>> =
        parts.map(|p| p.iter().map(|&(c, n)| (at(c)..at(c) + c.len(), n)).collect()).collect();
    let whole = whole.clone();
    Bytes::lazy(len, move || {
        let mut b = Writer::new(Some(len));
        let mut piece = Vec::new();
        for p in &parts {
            let chunks: Vec<(&[u8], usize)> = p.iter().map(|(r, n)| (&whole[r.clone()], *n)).collect();
            piece.resize(aqr_lzx::recons::data_len(&chunks), 0);
            aqr_lzx::recons::data_into(&chunks, &mut piece).expect("chunks split before");
            b.write(&piece);
            // (of a file mapped, let go of again: see `encode_as`)
            chunks.iter().for_each(|&(c, _)| aqr_formula::store::let_go(c));
        }
        b.into_bytes()
    })
}

/// Parts `order` (by index) as streams of `raw`: the others than resources of no compression
/// (`stored`) one after another, an application of at most [`APPLY_DATA`] bytes of data
/// (`lens`; but for one alone) each; resources of no compression between them, those one right
/// after another in the file (`spans`) together, as their slices are one slice of the root (a
/// content).
fn groups(order: &[usize], stored: impl Fn(usize) -> bool, lens: &[usize], spans: &[Range<usize>]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    // (the group open: of an application and its bytes of data, or of resources of no
    // compression and where the last ends)
    let mut open: Option<Result<usize, usize>> = None;
    let apply_data = APPLY_DATA.get();
    for &i in order {
        let join = match open {
            Some(Ok(bytes)) => !stored(i) && (bytes == 0 || bytes + lens[i] <= apply_data),
            Some(Err(end)) => stored(i) && end == spans[i].start,
            None => false,
        };
        if !join {
            groups.push(Vec::new());
        }
        groups.last_mut().expect("a group open").push(i);
        open = Some(match (stored(i), open) {
            (true, _) => Err(spans[i].end),
            (false, Some(Ok(bytes))) if join => Ok(bytes + lens[i]),
            (false, _) => Ok(lens[i]),
        });
    }
    groups
}
