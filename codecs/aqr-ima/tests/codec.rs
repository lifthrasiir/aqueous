//! Blocks through the record and back: of IMA ADPCM encoded here (mono, stereo of equal channels,
//! six channels), of bytes of noise (headers of any index and reserved byte), in the Xbox and in
//! Microsoft's layouts; and records that are not one.

use aqr_common::rng::Rng;
use aqr_ima::codec::{Layout, decode, encode};

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// IMA ADPCM blocks of `l` of the samples `pcm(c, t)` (channel, time), each block's header the
/// sample at its start and the index its encoder ends the block before at, plus one, as FMOD's.
fn ima(l: Layout, blocks: usize, pcm: &dyn Fn(usize, usize) -> i32) -> Vec<u8> {
    let n = (l.per_channel - 4) * 2;
    let mut idx = vec![0i32; l.channels];
    let mut out = Vec::new();
    for b in 0..blocks {
        let mut block = vec![0u8; l.block()];
        for c in 0..l.channels {
            let t0 = b * (n + 1);
            let mut pred = pcm(c, t0).clamp(-32768, 32767);
            let mut i = (idx[c] + if b > 0 { 1 } else { 0 }).clamp(0, 88);
            block[4 * c..4 * c + 2].copy_from_slice(&(pred as i16).to_le_bytes());
            block[4 * c + 2] = i as u8;
            for j in 0..n {
                let s = STEPS[i as usize];
                let mut diff = pcm(c, t0 + 1 + j) - pred;
                let mut v = 0u8;
                if diff < 0 {
                    v = 8;
                    diff = -diff;
                }
                let mut d = s >> 3;
                if diff >= s {
                    v |= 4;
                    diff -= s;
                    d += s;
                }
                if diff >= s >> 1 {
                    v |= 2;
                    diff -= s >> 1;
                    d += s >> 1;
                }
                if diff >= s >> 2 {
                    v |= 1;
                    d += s >> 2;
                }
                pred = if v & 8 != 0 { pred - d } else { pred + d }.clamp(-32768, 32767);
                i = (i + INDEX[(v & 7) as usize]).clamp(0, 88);
                let p = 4 * l.channels + 4 * ((j / 8) * l.channels + c) + (j % 8) / 2;
                block[p] |= v << (4 * (j % 2));
            }
            idx[c] = i;
        }
        out.extend_from_slice(&block);
    }
    out
}

fn tone(c: usize, t: usize) -> i32 {
    let x = t as f64 / 48000.0;
    (8000.0 * (x * 440.0 * std::f64::consts::TAU).sin() + 3000.0 * (x * (1000.0 + c as f64) * 6.0).sin()) as i32
}

#[test]
fn encoded_blocks_round_trip() {
    let mono = Layout::new(1, 36).unwrap();
    let d = ima(mono, 200, &tone);
    let (used, r) = encode(mono, &d);
    assert_eq!(used, d.len());
    assert_eq!(decode(mono, &r, used).unwrap(), d);
    assert!(r.len() < d.len() * 3 / 4, "{} of {}", r.len(), d.len());
    // stereo of equal channels: the right channel costs next to nothing
    let stereo = Layout::new(2, 36).unwrap();
    let e = ima(stereo, 200, &|_, t| tone(0, t));
    let (used, s) = encode(stereo, &e);
    assert_eq!(decode(stereo, &s, used).unwrap(), e);
    assert!(s.len() < r.len() + r.len() / 10, "{} against {}", s.len(), r.len());
    // six channels, Microsoft's layout (a block alignment of 1024 bytes for 6: 168 a channel)
    let six = Layout::new(6, 168).unwrap();
    let f = ima(six, 50, &tone);
    let (used, t) = encode(six, &f);
    assert_eq!(decode(six, &t, used).unwrap(), f);
}

#[test]
fn any_bytes_round_trip() {
    let mut rng = Rng::new(3);
    for (ch, per) in [(1, 36), (2, 36), (2, 8), (3, 64)] {
        let l = Layout::new(ch, per).unwrap();
        for k in 0..20 {
            let mut d = rng.bytes(l.block() * (k % 7) + (k * 5) % l.block());
            if k % 3 == 0 {
                // channels equal, now and then
                for b in 0..d.len() / l.block() {
                    let s = b * l.block();
                    let w0 = 4 * ch;
                    for c in 1..ch {
                        d.copy_within(s..s + 4, s + 4 * c);
                        for w in 0..(per - 4) / 4 {
                            let a = s + w0 + 4 * (w * ch);
                            d.copy_within(a..a + 4, a + 4 * c);
                        }
                    }
                }
            }
            let (used, r) = encode(l, &d);
            assert_eq!(used, d.len() / l.block() * l.block());
            assert_eq!(decode(l, &r, used).unwrap(), d[..used]);
        }
    }
    assert!(Layout::new(0, 36).is_none() && Layout::new(1, 6).is_none() && Layout::new(1, 4).is_none());
}

#[test]
fn bad_records_fail_or_decode_to_the_length() {
    let l = Layout::new(2, 36).unwrap();
    let d = ima(l, 100, &tone);
    let (used, r) = encode(l, &d);
    assert!(decode(l, &r, used + 1).is_err());
    let mut rng = Rng::new(9);
    for i in 0..200 {
        let mut b = r.clone();
        if i % 2 == 0 {
            b.truncate(rng.below(b.len() as u64) as usize);
        } else {
            let at = rng.below(b.len() as u64) as usize;
            b[at] ^= 1 << rng.below(8);
        }
        if let Ok(out) = decode(l, &b, used) {
            assert_eq!(out.len(), used);
        }
    }
}
