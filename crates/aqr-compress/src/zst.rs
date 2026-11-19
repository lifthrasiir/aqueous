//! Zstandard files whose content other files of a batch are (the whole, or a member of the tar
//! archive it is): those files taken from the frames as they are, decompressed by the archive's
//! Patch instructions (aqr-zstd), rather than stored again. The frames are not rebuilt from
//! their content: they cost about what the content would.

use std::collections::HashMap;
use std::ops::Range;

use aqr_formula::{Formula, Node};

aqr_common::zopt! {
    /// The most bytes of content decompressed of a file of frames, which is held as it is looked at.
    static CONTENT_MAX: Opt<usize> = Opt::size(
        "zst:content-max",
        2 << 30,
        "most bytes of content decompressed of a zstd file, to share with the files of its batch",
    );
}

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
    frames(d, bound).and_then(|fr| aqr_zstd::decode(d, &fr, CONTENT_MAX.get() as u64)).map(|(c, _)| c)
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
        let Some((content, lens)) = aqr_zstd::decode(d, &fr, CONTENT_MAX.get() as u64) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_archive::Evaluate;

    fn text(n: usize, seed: usize) -> Vec<u8> {
        (0..n).flat_map(|i| format!("line {} of the text {seed}\n", (i + seed) * 7 % 1000).into_bytes()).collect()
    }

    /// A batch's formula of `files`: each all of a stream of its own, their lengths and hashes.
    fn batch(files: &[&[u8]]) -> (Formula, Vec<(usize, u128)>) {
        let mut f = Formula::default();
        let parts: Vec<Node> = files.iter().map(|d| f.push("raw", d.to_vec())).collect();
        f.root = Node::Concat(parts);
        (f, files.iter().map(|d| (d.len(), xxhash_rust::xxh3::xxh3_128(d))).collect())
    }

    /// Whether the files of the formula are taken (no longer a slice of their stream).
    fn taken(f: &Formula) -> Vec<bool> {
        let Node::Concat(parts) = &f.root else { panic!("parts") };
        parts.iter().map(|p| !matches!(p, Node::Slice { start: 0, .. })).collect()
    }

    fn frames(parts: &[&[u8]]) -> Vec<u8> {
        parts.iter().flat_map(|p| zstd::bulk::compress(p, 3).unwrap()).collect()
    }

    /// The files that are the content of frames are taken from them; one of the same content
    /// as another is taken once; the frames themselves and files of other content are not.
    #[test]
    fn content_taken_once() {
        let content = text(20_000, 1);
        let z = frames(&[&content]);
        let other = text(20_000, 2);
        let (mut f, files) = batch(&[&content, &z, &other, &content]);
        share(&mut f, &files, u64::MAX);
        // (the first of the two equal files is taken, the other is its copy)
        assert_eq!(taken(&f), [true, false, false, false]);
        // without frames, or of data that is not
        let (mut f, files) = batch(&[&content, &other]);
        share(&mut f, &files, u64::MAX);
        assert_eq!(taken(&f), [false, false]);
        // an empty file, and a frame file of no file's content
        let (mut f, files) = batch(&[b"", &z]);
        share(&mut f, &files, u64::MAX);
        assert_eq!(taken(&f), [false, false]);
    }

    /// Frames of more content than the bound give nothing; of frames some within it, a file is
    /// taken only where all the frames it spans are.
    #[test]
    fn content_over_the_bound() {
        let (a, b) = (text(5_000, 3), text(30_000, 4));
        let whole = [a.clone(), b.clone()].concat();
        let z = frames(&[&a, &b]);
        let run = |bound| {
            let (mut f, files) = batch(&[&whole, &z, &a, &b]);
            share(&mut f, &files, bound);
            taken(&f)
        };
        // (the whole is the content of the frames; a and b are pieces of it, taken only as members
        // of a tar archive, which they are not)
        assert_eq!(run(u64::MAX), [true, false, false, false]);
        let ends = a.len() as u64;
        assert_eq!(run(ends), [false; 4], "the second frame is over the bound: the whole is not within");
        assert_eq!(run(a.len() as u64 - 1), [false; 4], "all the frames are over it");
        assert_eq!(run(b.len() as u64), run(u64::MAX), "frames of exactly the bound's content");
        assert_eq!(run(b.len() as u64 - 1), [false; 4]);
        assert!(frames_of(&z, a.len() as u64 - 1).is_none());
        assert!(frames_of(&z, a.len() as u64).is_some());
    }

    fn frames_of(d: &[u8], bound: u64) -> Option<aqr_zstd::Frames> {
        super::frames(d, bound)
    }

    /// `content` decodes the frames within [`CONTENT_MAX`] bytes, exactly.
    #[test]
    fn content_within_the_most() {
        let c = text(10_000, 5);
        let z = frames(&[&c[..c.len() / 2], &c[c.len() / 2..]]);
        assert!(content(&z, u64::MAX).is_some_and(|d| d == c));
        {
            let _m = CONTENT_MAX.set_here(c.len());
            assert!(content(&z, u64::MAX).is_some_and(|d| d == c));
        }
        let _m = CONTENT_MAX.set_here(c.len() - 1);
        assert!(content(&z, u64::MAX).is_none());
        // (a bound under each frame's content: none taken)
        assert!(content(&z, 1).is_none());
    }

    /// Frames that do not decode (cut, damaged in a block, a header of no content) give nothing
    /// and cost no panic; the files stay as they are.
    #[test]
    fn frames_that_do_not_decode() {
        let content = text(20_000, 6);
        let z = frames(&[&content]);
        aqr_testkit::damaged(&z, 64, 40, std::time::Duration::from_secs(30), |z| {
            let (mut f, files) = batch(&[&content, z]);
            share(&mut f, &files, u64::MAX);
            // (taken or not, the files evaluate to themselves)
            let back = f.eval(&aqr_decompress::Native::default()).unwrap();
            assert!(back == [&content[..], z].concat());
        });
    }
}
