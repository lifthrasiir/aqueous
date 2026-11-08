//! The module as a part module ([`part::run`]: the data pulled in pieces, the stream put in
//! pieces) against the whole compressor, on blocks and frames of every shape: past the view of a
//! block (matches reaching back, backward extension of long literal runs, matches of megabytes),
//! with the acceleration skipping, and with the literal runs the view cannot hold refused.

use aqr_api::stream::{Pull, Put};
use aqr_lz4::block::Compressor;
use aqr_lz4::frame;
use aqr_lz4::module::{self};
use aqr_lz4::part::{self, LIT_MAX};

/// Data of `n` bytes of letters and repeats of them (at any distance up to `reach`), from `seed`.
fn data(n: usize, seed: u64, reach: usize) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if d.len() > 64 && x >> 60 < 6 {
            let len = 4 + (x >> 8) as usize % 60;
            let back = 1 + (x >> 20) as usize % d.len().min(reach);
            let src = d.len() - back.max(len.min(d.len()));
            for j in 0..len {
                d.push(d[src + j]);
            }
        } else {
            d.push((x >> 40) as u8 % 24);
        }
    }
    d.truncate(n);
    d
}

/// Noise, which LZ4 leaves as one run of literals.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

fn params(kind: u8, accel: u32, descriptor: Option<(u8, u8)>) -> Vec<u8> {
    let mut p = vec![kind];
    aqr_common::varint::put(&mut p, accel as u64);
    if let Some((flg, bd)) = descriptor {
        p.extend([flg, bd]);
    }
    p
}

/// A [`Pull`] giving at most `k` bytes at a time.
struct Few<'a>(&'a [u8], usize);

impl Pull for Few<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len()).min(self.1);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

struct Vecs(Vec<u8>);

impl Put for Vecs {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        self.0.extend_from_slice(d);
        Some(())
    }
}

/// The part module and the whole one on `d` with params `p`: the same, and `want`; and whatever
/// the pieces the data comes in.
fn same(what: &str, p: &[u8], d: &[u8], want: &[u8]) {
    let whole = module::decode(p, &[d], want.len() as u64, 1);
    assert!(whole.as_deref().ok() == Some(want), "{what}: whole");
    let part = part::decode_streamed(p, &[d], want.len() as u64);
    assert!(part.as_deref().ok() == Some(want), "{what}: part");
    for k in [1, 13, 4093, 100_000] {
        let mut out = Vecs(Vec::new());
        part::run(p, &mut Few(d, k), d.len() as u64, &mut out).unwrap_or_else(|e| panic!("{what}: {k} at a time: {e}"));
        assert!(out.0 == want, "{what}: {k} at a time");
    }
    assert!(part::reproduces(p, d, want), "{what}: reproduces");
    let mut cut = want.to_vec();
    if let Some(l) = cut.last_mut() {
        *l ^= 1;
        assert!(!part::reproduces(p, d, &cut), "{what}: reproduces another");
    }
}

#[test]
fn blocks_through_the_view() {
    // (sizes around the narrow table's end, the view's size and well past it)
    for (k, &(n, reach, accel)) in [
        (0usize, 1usize << 20, 1u32),
        (1, 1 << 20, 1),
        (12, 1 << 20, 1),
        (13, 1 << 20, 1),
        (200, 100, 1),
        (65_546, 65_000, 1),
        (65_547, 65_000, 1),
        (65_548, 1 << 20, 1),
        (300_000, 70_000, 1),
        (300_000, 1 << 20, 3),
        (3 << 20, 65_535, 1),
        (3 << 20, 65_536, 1),
        (3 << 20, 1 << 20, 17),
        (700_000, 1 << 30, 5),
    ]
    .iter()
    .enumerate()
    {
        let d = data(n, k as u64, reach);
        let want = Compressor::compress_fast(&d, accel).unwrap();
        same(&format!("{n} bytes, reach {reach}, accel {accel}"), &params(0, accel, None), &d, &want);
    }
}

#[test]
fn long_matches_and_literals() {
    // a run of megabytes is one match; noise after it, then more of the run's byte: a match
    // found past a run of literals, extended backwards over them
    let mut d = vec![7u8; 5 << 20];
    d.extend(noise(100_000, 3));
    d.extend(vec![7u8; 1 << 20]);
    let want = Compressor::compress_fast(&d, 1).unwrap();
    same("long runs", &params(0, 1, None), &d, &want);
    // noise within the view's literals, then a match that reaches back across them
    let n = noise(LIT_MAX - 20_000, 4);
    let d = [n.clone(), data(200_000, 5, 1 << 20), n].concat();
    for accel in [1, 4, 17] {
        let want = Compressor::compress_fast(&d, accel).unwrap();
        same(&format!("literals, accel {accel}"), &params(0, accel, None), &d, &want);
    }
}

#[test]
fn literal_runs_past_the_view_are_refused() {
    // (noise is one run of literals: the whole block of it cannot be held)
    let d = noise(LIT_MAX + 70_000, 1);
    let p = params(0, 1, None);
    let want = Compressor::compress_fast(&d, 1).unwrap();
    assert!(part::decode_streamed(&p, &[&d], want.len() as u64).is_err());
    assert!(!part::reproduces(&p, &d, &want));
    // a block of noise short of it is a run of literals in the view
    let d = noise(LIT_MAX - 20_000, 2);
    let d = [data(300_000, 6, 1 << 20), d, data(300_000, 7, 1 << 20)].concat();
    let want = Compressor::compress_fast(&d, 1).unwrap();
    same("literals up to the limit", &p, &d, &want);
}

#[test]
fn frames_through_pieces() {
    // FLG: version 1, independent blocks; BD: the block size ID in the high nibble
    for (k, &(n, flg, bd, accel)) in [
        (0usize, 0x60u8, 0x40u8, 1u32),
        (1, 0x60, 0x40, 1),
        (65_536, 0x60 | 0x08, 0x40, 1),
        (65_537, 0x60 | 0x10, 0x40, 1),
        (1 << 20, 0x60 | 0x04, 0x40, 1),
        (1 << 20, 0x60 | 0x1c, 0x50, 3),
        (5 << 20, 0x64, 0x60, 1),
        (9 << 20, 0x64, 0x70, 2),
    ]
    .iter()
    .enumerate()
    {
        assert!(frame::encodable(flg, bd), "{n} {flg:x} {bd:x}");
        // (a part of it noise: stored blocks)
        let d = [data(n * 3 / 4, k as u64, 1 << 16), noise(n - n * 3 / 4, k as u64)].concat();
        let want = frame::encode(&d, flg, bd, accel, None).unwrap().unwrap();
        same(&format!("frame of {n}, {flg:x} {bd:x}"), &params(1, accel, Some((flg, bd))), &d, &want);
    }
}

#[test]
fn bad_params_and_data() {
    let d = data(100_000, 8, 1 << 16);
    let want = Compressor::compress_fast(&d, 1).unwrap();
    let p = params(0, 1, None);
    for bad in [vec![], vec![2, 1], vec![0, 0], vec![0, 1, 0], vec![1, 1], vec![1, 1, 0x60, 0x40]] {
        assert!(part::decode_streamed(&bad, &[&d], want.len() as u64).is_err(), "{bad:?}");
    }
    // an output of another length, data cut short or with no end to its input
    assert!(part::decode_streamed(&p, &[&d], want.len() as u64 + 1).is_err());
    assert!(part::decode_streamed(&p, &[&d], want.len() as u64 - 1).is_err());
    let mut out = Vecs(Vec::new());
    assert!(part::run(&p, &mut Few(&d[..d.len() - 1], 4093), d.len() as u64, &mut out).is_err());
}

#[test]
fn skipping_far_is_refused() {
    // (an acceleration of 65537 probes every 65537 bytes: a run of literals past the view)
    let d = data(9 << 20, 9, 1 << 20);
    let want = Compressor::compress_fast(&d, 65537).unwrap();
    let p = params(0, 65537, None);
    assert!(part::decode_streamed(&p, &[&d], want.len() as u64).is_err());
    // but a small block is taken from the data whole
    let d = &d[..60_000];
    let want = Compressor::compress_fast(d, 65537).unwrap();
    same("a small block", &p, d, &want);
}
