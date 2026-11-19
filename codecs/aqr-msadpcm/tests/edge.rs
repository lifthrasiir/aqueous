//! The extremes of Microsoft ADPCM blocks (8 channels, every predictor index, deltas and
//! samples at their limits, nibbles that saturate the samples and grow the delta past 16 bits),
//! records that lie about the length they give, and damaged records.

use std::time::Duration;

use aqr_common::rng::Rng;
use aqr_msadpcm::codec::{Layout, decode, encode};
use aqr_testkit::{Refusing, damaged, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 28);

fn round_trip(l: Layout, d: &[u8]) -> Vec<u8> {
    let (used, r) = encode(l, d);
    assert_eq!(used, d.len() / l.block * l.block);
    assert_eq!(decode(l, &r, used).unwrap(), d[..used]);
    r
}

#[test]
fn layout_limits() {
    assert!(Layout::new(8, 60).is_some() && Layout::new(8, 56).is_none() && Layout::new(8, 57).is_none());
    assert!(Layout::new(9, 1000).is_none() && Layout::new(0, 1000).is_none());
    assert!(Layout::new(1, 1 << 20).is_some() && Layout::new(1, (1 << 20) + 1).is_none());
    // (blocks of 8 channels, a sample of each takes 8 nibbles: 4 bytes a sample)
    assert!(Layout::new(8, 56 + 4).is_some() && Layout::new(8, 56 + 3).is_none());
}

/// Eight channels of noise, and of channels that are all alike.
#[test]
fn most_channels() {
    let mut rng = Rng::new(4);
    for block in [60, 256, 1024] {
        let l = Layout::new(8, block).unwrap();
        round_trip(l, &rng.bytes(block * 9 + 3));
    }
}

/// Every header field at its limits, with nibbles of every kind: the delta past 16 bits grows
/// (each nibble 6 and 7 multiplies it by 2.4), samples saturate both ways, predictor indices
/// past the 7 coefficient pairs.
#[test]
fn saturation() {
    for (ch, block) in [(1, 38), (2, 76), (8, 120)] {
        let l = Layout::new(ch, block).unwrap();
        let mut d = Vec::new();
        for predictor in [0u8, 6, 7, 128, 255] {
            for (delta, s1, s2) in [
                (0i16, 0i16, 0i16),
                (i16::MAX, i16::MAX, i16::MAX),
                (i16::MIN, i16::MIN, i16::MIN),
                (-1, i16::MAX, i16::MIN),
                (16, i16::MIN, i16::MAX),
                (i16::MAX, 0, i16::MIN),
            ] {
                for nibble in [0x00u8, 0x77, 0x88, 0xff, 0x76, 0x89, 0x7f, 0xf7] {
                    let mut b = vec![nibble; block];
                    for c in 0..ch {
                        b[c] = predictor;
                        b[ch + 2 * c..ch + 2 * c + 2].copy_from_slice(&delta.to_le_bytes());
                        b[3 * ch + 2 * c..3 * ch + 2 * c + 2].copy_from_slice(&s1.to_le_bytes());
                        b[5 * ch + 2 * c..5 * ch + 2 * c + 2].copy_from_slice(&s2.to_le_bytes());
                    }
                    d.extend_from_slice(&b);
                }
            }
        }
        within(Duration::from_secs(60), || round_trip(l, &d)).unwrap();
        let rev: Vec<u8> = d.chunks(block).rev().flatten().copied().collect();
        within(Duration::from_secs(60), || round_trip(l, &rev)).unwrap();
    }
}

#[test]
fn lying_lengths() {
    let l = Layout::new(2, 140).unwrap();
    let d = Rng::new(5).bytes(l.block * 20);
    let (used, r) = encode(l, &d);
    for len in [used + l.block, used * 1000, 1 << 32, 1 << 40, 1 << 62] {
        let len = len / l.block * l.block;
        assert!(within(Duration::from_secs(30), || decode(l, &r, len)).unwrap().is_err(), "{len}");
        assert!(within(Duration::from_secs(30), || decode(l, &[], len)).unwrap().is_err(), "{len}");
    }
    assert_eq!(decode(l, &[], 0).unwrap(), []);
    assert_eq!(encode(l, &[]).0, 0);
}

#[test]
fn damaged_records() {
    let l = Layout::new(2, 140).unwrap();
    let d = Rng::new(6).bytes(l.block * 6);
    let (used, r) = encode(l, &d);
    damaged(&r, 60, 24, Duration::from_secs(30), |b| {
        if let Ok(out) = decode(l, b, used) {
            assert_eq!(out.len(), used);
        }
    });
}
