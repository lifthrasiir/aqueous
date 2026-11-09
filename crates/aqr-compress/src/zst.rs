//! Zstandard files whose content other files of a batch are (the whole, or a member of the tar
//! archive it is): those files taken from the frames as they are, decompressed by the archive's
//! Patch instructions (aqr-zstd), rather than stored again. The frames are not rebuilt from
//! their content: they cost about what the content would.

use std::collections::HashMap;
use std::ops::Range;

use aqr_formula::{Formula, Node};

/// The most bytes of content decompressed of a file of frames, which is held as it is looked at.
const CONTENT_MAX: u64 = 2 << 30;

/// The most bytes of content of a frame taken from ([`share`]): the bound of an application
/// ([`crate::Options::application_bound`]), as a decoder makes a patch's output whole.
pub(crate) fn bound(opt: &crate::Options) -> u64 {
    opt.application_bound.unwrap_or(crate::memory::DEFAULT_BOUND)
}

/// The frames of `d`, a Zstandard file, unless each tells a content of more than `bound` bytes
/// (none of them would be taken, [`share`]).
fn frames(d: &[u8], bound: u64) -> Option<aqr_zstd::Frames> {
    aqr_zstd::parse(d).filter(|fr| !fr.frames().all(|f| f.content.is_some_and(|n| n > bound)))
}

/// The content of the frames of `d`, a Zstandard file, where they decompress ([`CONTENT_MAX`]
/// bytes at most) and some may be taken from ([`frames`]).
pub(crate) fn content(d: &[u8], bound: u64) -> Option<Vec<u8>> {
    frames(d, bound).and_then(|fr| aqr_zstd::decode(d, &fr, CONTENT_MAX)).map(|(c, _)| c)
}

/// Takes the files of `f` (its root's parts, each all of a stream of its own; of `files`' sizes
/// and XXH3-128) that are the content of another's frames, or one of its members
/// ([`crate::archive::member_spans`]), from those frames: from frames of `bound` bytes of
/// content at most, as a decoder makes a patch's output whole. A file of frames gives to none,
/// and takes from none; a file is taken once, by the longest piece it is (of those apart).
pub(crate) fn share(f: &mut Formula, files: &[(usize, u128)], bound: u64) {
    let Node::Concat(parts) = &f.root else { return };
    // (of each part, its stream where it is all of it)
    let streams: Vec<Option<usize>> = parts
        .iter()
        .map(|p| match *p {
            Node::Slice { stream, start: 0, len } if f.streams[stream].data.len() as u64 == len => Some(stream),
            _ => None,
        })
        .collect();
    let is_source = |k: usize| streams[k].is_some_and(|s| f.streams[s].data.starts_with(&aqr_zstd::MAGIC));
    let sources: Vec<usize> = (0..parts.len()).filter(|&k| is_source(k)).collect();
    let mut targets: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, &(len, _)) in files.iter().enumerate() {
        if len > 0 && streams[k].is_some() && !is_source(k) {
            targets.entry(len).or_default().push(k);
        }
    }
    if sources.is_empty() || targets.is_empty() {
        return;
    }
    let _s = aqr_common::trace::span("zstd");
    let mut taken = vec![false; parts.len()];
    // (one at a time: a content may be large)
    for k in sources {
        let s = streams[k].expect("a stream");
        let d = &f.streams[s].data[..];
        let Some(fr) = frames(d, bound) else { continue };
        let Some((content, lens)) = aqr_zstd::decode(d, &fr, CONTENT_MAX) else { continue };
        // the pieces of the content that files are: the whole, or a member; within frames of
        // the bound
        let ends: Vec<u64> = lens
            .iter()
            .scan(0, |at, &n| {
                *at += n;
                Some(*at)
            })
            .collect();
        let within = |r: &Range<usize>| {
            let (first, last) =
                (ends.partition_point(|&e| e <= r.start as u64), ends.partition_point(|&e| e < r.end as u64));
            lens[first..=last.min(lens.len() - 1)].iter().all(|&n| n <= bound)
        };
        let spans = std::iter::once(0..content.len()).chain(crate::archive::member_spans(&content));
        let mut pieces: Vec<(Range<u64>, usize)> = Vec::new();
        for r in spans {
            let Some(ks) = targets.get(&r.len()).filter(|_| within(&r)) else { continue };
            let h = xxhash_rust::xxh3::xxh3_128(&content[r.clone()]);
            pieces.extend(ks.iter().filter(|&&t| files[t].1 == h).map(|&t| (r.start as u64..r.end as u64, t)));
        }
        drop(content);
        // each file taken once, the longest pieces first, apart from each other
        pieces.sort_by_key(|(r, t)| (std::cmp::Reverse(r.end - r.start), r.start, *t));
        let mut kept: Vec<(Range<u64>, usize)> = Vec::new();
        for (r, t) in pieces {
            if !taken[t] && kept.iter().all(|(q, _)| q.end <= r.start || r.end <= q.start) {
                taken[t] = true;
                kept.push((r, t));
            }
        }
        if kept.is_empty() {
            continue;
        }
        kept.sort_by_key(|(r, _)| r.start);
        let blocks: Vec<(Range<u64>, u64)> =
            fr.frames().map(|f| f.blocks.start as u64..f.blocks.end as u64).zip(lens).collect();
        let ranges: Vec<Range<u64>> = kept.iter().map(|(r, _)| r.clone()).collect();
        let nodes = aqr_zstd::formula::take(f, s, &blocks, &ranges);
        let Node::Concat(parts) = &mut f.root else { unreachable!("parts") };
        for ((_, t), n) in kept.into_iter().zip(nodes) {
            parts[t] = n;
            // (its stream, which no slice is of now, emptied: streams are merged by their class
            // whether sliced or not, and the files' contents are told by their streams' index)
            f.streams[streams[t].expect("a stream")].data = Vec::new().into();
        }
    }
}
