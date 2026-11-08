//! `snappy-recons` as a part module ([`module::decode_streamed`] and [`module::compress_stream`]:
//! the data pulled in pieces, the stream put in pieces) against the whole compressor, for both
//! hashes, around the fragments' size and past it, and damaged params and data.

use aqr_api::stream::{Pull, Put};
use aqr_snappy::module;
use aqr_snappy::snappy::{self, Hash};

/// Data of `n` bytes of letters and repeats of them, from `seed`.
fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        match x >> 61 {
            0 if d.len() > 64 => {
                let len = 4 + (x >> 8) as usize % 70;
                let src = (x >> 20) as usize % (d.len() - 8);
                for j in 0..len {
                    d.push(d[src + j % 8]);
                }
            }
            1 => d.extend(std::iter::repeat_n((x >> 8) as u8, 5 + (x >> 20) as usize % 200)),
            2 => d.extend((0..40).map(|i| (x >> (i % 7 * 8)) as u8)),
            _ => d.push(b'a' + (x >> 40) as u8 % 24),
        }
    }
    d.truncate(n);
    d
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

struct Capped(Vec<u8>, usize);

impl Put for Capped {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        (self.0.len() + d.len() <= self.1).then(|| self.0.extend_from_slice(d))
    }
}

#[test]
fn streams_through_pieces() {
    let sizes = [0usize, 1, 14, 15, 16, 255, 256, 4096, 65_535, 65_536, 65_537, 131_072, 200_001, 3 << 20];
    for (k, &n) in sizes.iter().enumerate() {
        for hash in Hash::ALL {
            let d = if k % 3 == 2 { (0..n).map(|i| (i * 7919 >> 3) as u8).collect() } else { data(n, k as u64) };
            let want = snappy::compress(&d, hash).unwrap();
            let p = [hash as u8];
            let whole = module::decode(&p, &[&d], want.len() as u64, 1).unwrap();
            assert!(whole == want, "{n} {hash:?}: whole");
            let part = module::decode_streamed(&p, &[&d], want.len() as u64).unwrap();
            assert!(part == want, "{n} {hash:?}: part");
            for few in [1, 13, 4093, 100_000] {
                let mut out = Capped(Vec::new(), want.len());
                module::compress_stream(&mut Few(&d, few), n as u64, hash, &mut out)
                    .unwrap_or_else(|e| panic!("{n} {hash:?} {few} at a time: {e}"));
                assert!(out.0 == want, "{n} {hash:?}: {few} at a time");
            }
            // an output of another length, data cut short
            let mut out = Capped(Vec::new(), want.len() - 1);
            assert!(module::compress_stream(&mut Few(&d, 4093), n as u64, hash, &mut out).is_err());
            assert!(module::decode_streamed(&p, &[&d], want.len() as u64 + 1).is_err());
            if n > 0 {
                let mut out = Capped(Vec::new(), usize::MAX);
                assert!(module::compress_stream(&mut Few(&d[..n - 1], 4093), n as u64, hash, &mut out).is_err());
            }
        }
    }
}

#[test]
fn bad_params() {
    let d = data(1000, 1);
    let want = snappy::compress(&d, Hash::Mul).unwrap();
    for p in [&[][..], &[2], &[0, 0], &[255]] {
        assert!(module::decode_streamed(p, &[&d], want.len() as u64).is_err(), "{p:?}");
        assert!(module::decode(p, &[&d], want.len() as u64, 1).is_err(), "{p:?}");
    }
    assert!(module::decode_streamed(&[0], &[&d, &d], want.len() as u64).is_err());
}
