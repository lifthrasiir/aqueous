//! `zstd-frames`: files of frames in pieces (of any order, among other bytes) made again of the
//! frames' contents, natively on one thread and on several; corrupt inputs fail.

use std::ops::Range;

use aqr_zstd::formula::frames_in;
use aqr_zstd::frames::decode;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A file of `n` frames of level 3, each cut into pieces put in places of their own among bytes
/// of no frame, as a database's overflow pages may be; and the frames' pieces, in order. Some
/// frames are not libzstd's (level 1).
pub fn file(n: usize, seed: u64) -> (Vec<u8>, Vec<Vec<Range<usize>>>) {
    let mut r = Rng(seed);
    // the pieces: (frame, piece, bytes), shuffled
    let mut pieces = Vec::new();
    for i in 0..n {
        let len = [0, 10, 300, 5000, 40_000, 150_000][r.below(6)] + r.below(100);
        let content: Vec<u8> =
            (0..len).map(|k| ((k / (1 + r.below(7))) as u8).wrapping_mul(r.below(3) as u8 + 1)).collect();
        let level = if r.below(10) == 0 { 1 } else { 3 };
        let z = zstd::bulk::compress(&content, level).unwrap();
        let mut at = 0;
        let mut k = 0;
        while at < z.len() {
            let m = (1 + r.below(4092)).min(z.len() - at);
            pieces.push((i, k, z[at..at + m].to_vec()));
            at += m;
            k += 1;
        }
    }
    for i in (1..pieces.len()).rev() {
        // (shuffled near: frames open at once a few dozen)
        let j = i.saturating_sub(r.below(40));
        pieces.swap(i, j);
    }
    let mut d = Vec::new();
    let mut ranges: Vec<Vec<(usize, Range<usize>)>> = vec![Vec::new(); n];
    for (i, k, b) in pieces {
        for _ in 0..r.below(20) {
            d.push(r.next() as u8);
        }
        ranges[i].push((k, d.len()..d.len() + b.len()));
        d.extend_from_slice(&b);
    }
    d.extend_from_slice(b"the end");
    let frames = ranges
        .into_iter()
        .map(|mut v| {
            v.sort_by_key(|p| p.0);
            v.into_iter().map(|p| p.1).collect()
        })
        .collect();
    (d, frames)
}

fn inputs(f: &aqr_formula::Formula) -> (&[u8], Vec<&[u8]>) {
    let aqr_formula::Node::Apply(a) = &f.root else { panic!("an application") };
    (&a.params, f.streams[1..].iter().map(|s| &s.data[..]).collect())
}

#[test]
fn frames_made_again() {
    for (n, seed) in [(1, 1), (30, 2), (400, 3)] {
        let (d, frames) = file(n, seed);
        let f = frames_in(&d, &frames, 3, aqr_formula::Kind::Table, 4).expect("taken");
        let (params, ins) = inputs(&f);
        let p = aqr_zstd::frames::Params::parse(params).unwrap();
        assert!(ins[3].len() > ins[0].len(), "{n}: the contents stored");
        for threads in [1, 3] {
            assert!(decode(params, &ins, d.len() as u64, threads).unwrap() == d, "{n} {threads}");
        }
        // (the pool and the frames open no more than the params tell)
        let mut tight = p;
        tight.pool -= 1;
        if n > 1 {
            assert!(decode(&tight.bytes(), &ins, d.len() as u64, 1).is_err(), "{n}: pool");
        }
        // corrupt inputs fail, never panic
        let mut r = Rng(seed);
        for _ in 0..200 {
            let mut bad: Vec<Vec<u8>> = ins.iter().map(|i| i.to_vec()).collect();
            let s = r.below(4);
            if bad[s].is_empty() {
                continue;
            }
            match r.below(3) {
                0 => {
                    let at = r.below(bad[s].len());
                    bad[s][at] ^= 1 << r.below(8);
                }
                1 => {
                    let at = r.below(bad[s].len());
                    bad[s].truncate(at);
                }
                _ => bad[s].push(r.next() as u8),
            }
            let bad: Vec<&[u8]> = bad.iter().map(|b| &b[..]).collect();
            let _ = decode(params, &bad, d.len() as u64, 1 + r.below(2));
        }
    }
}

/// Frames overlapping another's pieces, or of no libzstd level emulated, are kept as bytes.
#[test]
fn kept_as_they_are() {
    let (d, mut frames) = file(20, 7);
    frames.push(vec![frames[0][0].clone()]);
    let f = frames_in(&d, &frames, 3, aqr_formula::Kind::Table, 2).expect("taken");
    let (params, ins) = inputs(&f);
    assert!(decode(params, &ins, d.len() as u64, 2).unwrap() == d);
    assert!(frames_in(&d, &frames, 19, aqr_formula::Kind::Table, 2).is_none());
    assert!(frames_in(&d, &[], 3, aqr_formula::Kind::Table, 2).is_none());
}
