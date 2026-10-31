//! aqr-mkv as an entry formula (see aqr-formula): the file as slices of two streams,
//! `mkv/keys`, the keyframes of its video tracks, and `mkv/data`, the rest of its bytes, but
//! for the frames seen before, each a slice of the stream where it was first. `FORMAT.md`
//! tells of the streams.

use std::collections::HashMap;

use aqr_formula::{Formula, Kind, Node, Region};

/// The class of the stream of the rest of the file, stream 0.
pub const DATA: &str = "mkv/data";

/// The class of the stream of the video tracks' keyframes, stream 1.
pub const KEYS: &str = "mkv/keys";

/// The least bytes of a frame taken from where it was before, or into [`KEYS`]: a smaller one
/// costs more as a slice than as its bytes.
pub const FRAME_MIN: usize = 256;

/// The least bytes of frames seen before for any file to be taken ([`taken_min`]).
pub const TAKEN_MIN: usize = 64 << 10;

/// The least bytes of frames seen before for a file of `len` bytes to be taken: a hundredth of
/// it, and [`TAKEN_MIN`]. Fewer gain too little for a pass of zstd over all of the file, which
/// would otherwise be stored as it is (aqr-compress takes Matroska files as compressed).
pub const fn taken_min(len: usize) -> usize {
    let n = len / 100;
    if n > TAKEN_MIN { n } else { TAKEN_MIN }
}

/// The formula of Matroska or WebM file `input`, or None where it is none ([`crate::starts`])
/// or the frames seen before come to less than [`taken_min`] of it and the bytes the formula's
/// description takes at most. The same whatever `threads` is (none is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    if !crate::starts(input) {
        return None;
    }
    let walk = crate::walk(input);
    let (mut data, mut keys) = (Vec::with_capacity(input.len()), Vec::new());
    let mut nodes = Vec::new();
    // the frames of FRAME_MIN bytes or more, by their hash: the stream, where they are in it,
    // and where in the file
    let mut seen: HashMap<u64, Vec<(usize, usize, usize)>> = HashMap::new();
    let (mut p, mut repeated) = (0, 0);
    for r in walk.frames.iter().filter(|f| f.span.len() >= FRAME_MIN) {
        let (span, frame) = (r.span.clone(), &input[r.span.clone()]);
        let key = r.flags & 0x80 != 0 && walk.video.contains(&r.track);
        let kept = seen.entry(xxhash_rust::xxh3::xxh3_64(frame)).or_default();
        let before = kept.iter().find(|&&(_, _, q)| input[q..].starts_with(frame));
        if before.is_none() && !key {
            // (left in the data, to be copied with what is around it)
            kept.push((0, data.len() + span.start - p, span.start));
            continue;
        }
        nodes.push(Node::slice(0, data.len() as u64, (span.start - p) as u64));
        data.extend_from_slice(&input[p..span.start]);
        match before {
            Some(&(s, at, _)) => {
                nodes.push(Node::slice(s, at as u64, frame.len() as u64));
                repeated += frame.len();
            }
            None => {
                kept.push((1, keys.len(), span.start));
                nodes.push(Node::slice(1, keys.len() as u64, frame.len() as u64));
                keys.extend_from_slice(frame);
            }
        }
        p = span.end;
    }
    nodes.push(Node::slice(0, data.len() as u64, (input.len() - p) as u64));
    data.extend_from_slice(&input[p..]);
    let root = Node::concat(nodes);
    if repeated < taken_min(input.len()) + aqr_formula::aqr::description_bound(&root) {
        return None;
    }
    let mut f = Formula::default();
    f.push(DATA, data);
    f.push(KEYS, keys);
    f.root = root;
    Some(f)
}

/// What the parts of file `input` that [`encode`] turned down are: all of it compressed, not to
/// be looked into (its frames' bytes would be taken for files of other formats here and there).
pub fn regions(input: &[u8]) -> Option<Vec<Region>> {
    crate::starts(input).then(|| vec![Region { span: 0..input.len(), kind: Kind::Compressed, contents: Kind::Unknown }])
}

/// Whether formula `f` is one of [`encode`]'s, or holds one (a scan's of a file, say).
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == DATA)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::file;

    /// The frames seen before are held once, the video's keyframes apart; the file given back.
    #[test]
    fn repeated_frames() {
        let key = [7u8; 70 << 10];
        let (other, sound): (Vec<u8>, Vec<u8>) =
            ((0..1000u32).map(|i| ((i * 37) >> 3) as u8).collect(), (0..300u32).map(|i| (i * 11) as u8).collect());
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
        for open in [false, true] {
            let d = file(
                &[
                    &[(1, true, &key), (2, true, &sound), (1, false, &other)],
                    &[(1, false, &other), (1, true, &key), (2, true, &sound), (1, false, b"p2")],
                    &[(1, true, &key)],
                ],
                open,
            );
            let f = encode(&d, 1).unwrap();
            assert!(is(&f));
            assert_eq!(f.streams[1].data.len(), key.len());
            assert_eq!(f.streams[0].data.len(), d.len() - 3 * key.len() - other.len() - sound.len(), "{open}");
            assert!(f.eval(&none).unwrap() == d, "{open}");
        }
        // (frames seen once only, or too few bytes of them, are not taken)
        assert!(encode(&file(&[&[(1, true, &key), (1, false, &other)]], false), 1).is_none());
        assert!(encode(&file(&[&[(1, false, &other), (1, false, &other)]], false), 1).is_none());
    }
}
