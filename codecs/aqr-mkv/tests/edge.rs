//! Edge cases of Matroska files: lacing, blocks in groups, unknown sizes, tracks numbered by
//! 2 or more bytes, several Segments, bytes after the last; damaged files.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_mkv::{formula, starts, walk};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);

/// An element of ID `id` and `data`, its size in `w` bytes (0 for unknown).
fn element(id: &[u8], data: &[u8], w: usize) -> Vec<u8> {
    let size = match w {
        0 => vec![0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        _ => (data.len() as u64 | 1 << (7 * w)).to_be_bytes()[8 - w..].to_vec(),
    };
    [id, &size, data].concat()
}

fn head() -> Vec<u8> {
    element(&[0x1a, 0x45, 0xdf, 0xa3], &element(&[0x42, 0x82], b"webm", 1), 1)
}

/// A Tracks element of video track `n` (its number in 2 bytes at most) and an audio track 2.
fn tracks(n: u64) -> Vec<u8> {
    let num = if n < 256 { vec![n as u8] } else { n.to_be_bytes()[6..].to_vec() };
    let entry =
        |num: &[u8], kind: u8| element(&[0xae], &[element(&[0xd7], num, 1), element(&[0x83], &[kind], 1)].concat(), 1);
    element(&[0x16, 0x54, 0xae, 0x6b], &[entry(&num, 1), entry(&[2], 2)].concat(), 2)
}

/// The vint of track number `n` in `w` bytes.
fn vint(n: u64, w: usize) -> Vec<u8> {
    (n | 1 << (7 * w)).to_be_bytes()[8 - w..].to_vec()
}

/// A SimpleBlock of `track` (a vint of `w` bytes), flags and the payload.
fn simple(track: u64, w: usize, flags: u8, payload: &[u8]) -> Vec<u8> {
    element(&[0xa3], &[&vint(track, w)[..], &[0, 0, flags], payload].concat(), 4)
}

fn block_group(track: u64, w: usize, payload: &[u8]) -> Vec<u8> {
    let b = element(&[0xa1], &[&vint(track, w)[..], &[0, 0, 0], payload].concat(), 4);
    element(&[0xa0], &[b, element(&[0x9b], &[1], 1)].concat(), 4)
}

fn cluster(body: &[u8], w: usize) -> Vec<u8> {
    element(&[0x1f, 0x43, 0xb6, 0x75], &[element(&[0xe7], &[0], 1), body.to_vec()].concat(), w)
}

fn segment(body: &[u8], w: usize) -> Vec<u8> {
    element(&[0x18, 0x53, 0x80, 0x67], body, w)
}

fn payload(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 11) as u8
        })
        .collect()
}

fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

/// Whether `d` has a formula; what it gives back is `d`.
fn round_trip(d: &[u8]) -> bool {
    match formula::encode(d, 1) {
        Some(f) => {
            assert!(f.eval(&none).unwrap() == d);
            true
        }
        None => false,
    }
}

#[test]
fn block_groups_are_frames() {
    let big = payload(2, 70 << 10);
    let c = [block_group(1, 1, &big), block_group(1, 1, &big), block_group(2, 1, b"audio")].concat();
    let d = [head(), segment(&[tracks(1), cluster(&c, 4)].concat(), 8)].concat();
    assert!(starts(&d));
    let w = walk(&d);
    assert_eq!(w.frames.len(), 3);
    assert_eq!(&d[w.frames[0].span.clone()], &big[..]);
    assert_eq!(w.frames[0].flags, 0);
    // a repeated frame of a BlockGroup is taken from where it was first
    let f = formula::encode(&d, 1).expect("repeats taken");
    assert!(f.eval(&none).unwrap() == d);
    // (a Block has no flags: none is a keyframe, the first stays in the data)
    assert_eq!(f.streams[0].data.len(), d.len() - big.len());
}

#[test]
fn laced_blocks() {
    // lacing flags (Xiph 0x02, fixed 0x04, EBML 0x06): the payload, lace header and all, is
    // the frame; the bytes come back whatever the lacing says
    let big = payload(3, 70 << 10);
    for lacing in [0x02u8, 0x04, 0x06] {
        let pl = [&[2u8, 0xff, 0xff, 3, 0x81][..], &big].concat();
        let c = [simple(1, 1, 0x80 | lacing, &pl), simple(1, 1, 0x80 | lacing, &pl)].concat();
        let d = [head(), segment(&[tracks(1), cluster(&c, 4)].concat(), 8)].concat();
        let w = walk(&d);
        assert_eq!(w.frames.len(), 2, "{lacing}");
        assert_eq!(w.frames[1].flags & 0x06, lacing);
        assert!(round_trip(&d), "{lacing}");
        // a lace header cut short or lying about sizes only changes what is a frame
        let cut = [simple(1, 1, 0x82, &[255]), simple(1, 1, 0x86, &[0x7f, 1])].concat();
        let d = [head(), segment(&[tracks(1), cluster(&cut, 4)].concat(), 8)].concat();
        assert_eq!(walk(&d).frames.len(), 2);
        assert!(!round_trip(&d));
    }
}

#[test]
fn long_track_numbers() {
    // video track numbers of 2, 3 and 4 bytes in the blocks (the entry's own in 2)
    for w in [2usize, 3, 4] {
        let big = payload(4, 70 << 10);
        let n = 300u64;
        let c = [simple(n, w, 0x80, &big), simple(n, w, 0x80, &big), simple(2, 1, 0x80, b"snd")].concat();
        let d = [head(), segment(&[tracks(n), cluster(&c, 4)].concat(), 8)].concat();
        let wk = walk(&d);
        assert_eq!(wk.video, [n], "{w}");
        assert_eq!(wk.frames.len(), 3);
        assert!(wk.frames.iter().take(2).all(|f| f.track == n));
        let f = formula::encode(&d, 1).expect("formula");
        // (the keyframe of the video track goes to its stream)
        assert_eq!(f.streams[1].data.len(), big.len(), "{w}");
        assert!(f.eval(&none).unwrap() == d);
    }
    // a track number of 8 bytes, or of an all-zero first byte: no frame, no panic
    for lead in [0x01u8, 0x00] {
        let blk = element(&[0xa3], &[lead, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0x80, 1, 2, 3], 1);
        let d = [head(), segment(&cluster(&blk, 4), 8)].concat();
        let _ = walk(&d);
    }
}

#[test]
fn unknown_sizes() {
    let big = payload(5, 70 << 10);
    let blk = simple(1, 1, 0x80, &big);
    // an unknown Segment, and clusters of unknown size ended by each other and by Cues
    let c1 = cluster(&[blk.clone(), blk.clone()].concat(), 0);
    let c2 = cluster(&blk, 0);
    let cues = element(&[0x1c, 0x53, 0xbb, 0x6b], &[0xbb, 0x80], 1);
    let d = [head(), segment(&[tracks(1), c1, c2, cues].concat(), 0)].concat();
    assert!(starts(&d));
    assert_eq!(walk(&d).frames.len(), 3);
    assert!(round_trip(&d));
    // the same cut anywhere
    for n in (0..d.len()).step_by(997) {
        let _ = walk(&d[..n]);
        round_trip(&d[..n]);
    }
    // an unknown size where the block itself is: not walked past
    let open = element(&[0xa3], &[0x81, 0, 0, 0x80, 1, 2, 3], 0);
    let d = [head(), segment(&cluster(&[blk.clone(), open, blk].concat(), 4), 8)].concat();
    assert_eq!(walk(&d).frames.len(), 1);
    round_trip(&d);
}

#[test]
fn several_segments_and_trailing_bytes() {
    let big = payload(6, 70 << 10);
    let one = |w: usize| segment(&[tracks(1), cluster(&simple(1, 1, 0x80, &big), 4)].concat(), w);
    let d = [head(), one(8), one(4)].concat();
    // the second Segment's frames are walked too, and the repeat taken across them
    assert_eq!(walk(&d).frames.len(), 2);
    assert!(round_trip(&d));
    // a second file (head and all) after the first
    assert!(round_trip(&[d.clone(), d].concat()));
    // bytes after the last element
    let tails: [&[u8]; 5] = [b"x", &[0; 100], &[0x1f, 0x43, 0xb6], &[0xff; 9], &[0x1f, 0x43, 0xb6, 0x75, 0x84]];
    for tail in tails {
        round_trip(&[head(), one(8), one(8), tail.to_vec()].concat());
    }
}

#[test]
fn damaged_files() {
    let big = payload(7, 4 << 10);
    let c = [simple(1, 1, 0x80, &big), block_group(1, 1, &big), simple(1, 1, 0x82, &big), simple(1, 1, 0x80, &big)]
        .concat();
    let d = [head(), segment(&[tracks(1), cluster(&c, 4), cluster(&c, 0)].concat(), 8)].concat();
    aqr_testkit::damaged(&d, 400, 128, Duration::from_secs(20), |m| {
        for f in &walk(m).frames {
            assert!(f.span.start < f.span.end && f.span.end <= m.len());
        }
        let _ = starts(m);
        round_trip(m);
    });
    // enough repeated bytes for a formula, damaged
    let big = payload(8, 70 << 10);
    let c = [simple(1, 1, 0x80, &big), simple(1, 1, 0x80, &big)].concat();
    let d = [head(), segment(&[tracks(1), cluster(&c, 4)].concat(), 8)].concat();
    assert!(round_trip(&d));
    aqr_testkit::damaged(&d, 60, 128, Duration::from_secs(60), |m| {
        round_trip(m);
    });
}
