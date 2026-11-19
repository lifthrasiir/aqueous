//! The extremes of IMA ADPCM blocks (255 channels, saturated predictors and step indices,
//! every nibble), records that lie about the length they give, and damaged records.

use std::time::Duration;

use aqr_common::rng::Rng;
use aqr_ima::codec::{Layout, decode, encode};
use aqr_testkit::{Refusing, damaged, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 28);

fn round_trip(l: Layout, d: &[u8]) -> Vec<u8> {
    let (used, r) = encode(l, d);
    assert_eq!(used, d.len() / l.block() * l.block());
    assert_eq!(decode(l, &r, used).unwrap(), d[..used]);
    r
}

#[test]
fn most_channels() {
    let mut rng = Rng::new(1);
    for (ch, per) in [(255, 8), (255, 36), (254, 12)] {
        let l = Layout::new(ch, per).unwrap();
        let d = rng.bytes(l.block() * 3 + 5);
        within(Duration::from_secs(60), || round_trip(l, &d)).unwrap();
        // every channel as the first (the "same as the channel before" path, 254 times a block)
        let mut e = d.clone();
        for b in 0..3 {
            let s = b * l.block();
            for c in 1..ch {
                e.copy_within(s..s + 4, s + 4 * c);
                for w in 0..(per - 4) / 4 {
                    let a = s + 4 * ch + 4 * (w * ch);
                    e.copy_within(a..a + 4, a + 4 * c);
                }
            }
        }
        let r = within(Duration::from_secs(60), || round_trip(l, &e)).unwrap();
        assert!(r.len() < e.len() / 20, "{} of {}", r.len(), e.len());
    }
    assert!(Layout::new(256, 36).is_none() && Layout::new(0, 36).is_none());
    assert!(
        Layout::new(1, 8).is_some() && Layout::new(1, 1 << 20).is_some() && Layout::new(1, (1 << 20) + 4).is_none()
    );
}

/// Headers of the extreme predictors and step indices (against the ends of the block before,
/// which are as extreme), and nibbles that saturate the predictor and the index both ways.
#[test]
fn saturation() {
    for ch in [1, 2] {
        let l = Layout::new(ch, 36).unwrap();
        let mut d = Vec::new();
        // (the predictor, the step index, the reserved byte, the nibble of the whole block)
        let headers: [(i16, u8, u8, u8); 10] = [
            (32767, 88, 0, 0x77),
            (-32768, 88, 0xff, 0xff),
            (32767, 88, 0, 0xff),
            (-32768, 0, 0, 0x77),
            (0, 89, 0, 0x00),
            (0, 255, 0, 0x88),
            (-1, 0, 0, 0x00),
            (32767, 255, 255, 0x7f),
            (-32768, 88, 1, 0xf7),
            (0, 0, 0, 0x33),
        ];
        for (p, i, r, n) in headers {
            let mut block = vec![n; l.block()];
            for c in 0..ch {
                block[4 * c..4 * c + 2].copy_from_slice(&p.to_le_bytes());
                block[4 * c + 2] = i;
                block[4 * c + 3] = r;
            }
            d.extend_from_slice(&block);
        }
        round_trip(l, &d);
        // the same blocks, in the opposite order
        let rev: Vec<u8> = d.chunks(l.block()).rev().flatten().copied().collect();
        round_trip(l, &rev);
    }
}

#[test]
fn lying_lengths() {
    let l = Layout::new(2, 36).unwrap();
    let mut rng = Rng::new(2);
    let d = rng.bytes(l.block() * 20);
    let (used, r) = encode(l, &d);
    for len in [used + l.block(), used * 1000, 1 << 32, 1 << 40, 1 << 62] {
        let len = len / l.block() * l.block();
        let res = within(Duration::from_secs(30), || decode(l, &r, len)).unwrap();
        assert!(res.is_err(), "{len}");
        let res = within(Duration::from_secs(30), || decode(l, &[], len)).unwrap();
        assert!(res.is_err(), "{len}");
    }
    // no length, no blocks
    assert_eq!(decode(l, &[], 0).unwrap(), []);
    assert_eq!(encode(l, &[]).0, 0);
}

#[test]
fn damaged_records() {
    let l = Layout::new(2, 36).unwrap();
    let mut rng = Rng::new(3);
    let d = rng.bytes(l.block() * 6);
    let (used, r) = encode(l, &d);
    damaged(&r, 60, 24, Duration::from_secs(30), |b| {
        if let Ok(out) = decode(l, b, used) {
            assert_eq!(out.len(), used);
        }
    });
}
