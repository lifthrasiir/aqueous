//! The blocks coded and given back: of an MS ADPCM encoder written here (the reference decoder's
//! inverse, greedy), of any bytes, of several layouts; records cut short fail or give blocks of
//! the length asked, never a panic.

use aqr_common::rng::Rng;
use aqr_msadpcm::codec::{Layout, decode, encode};

const COEF: [(i32, i32); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];
const ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];

/// `pcm` (samples of `channels` interleaved) as MS ADPCM blocks of `l`, predictor `p` for all,
/// each nibble the nearest the decoder makes.
fn adpcm(l: Layout, pcm: &[i16], p: usize) -> Vec<u8> {
    let ch = l.channels;
    let per = 2 + 2 * (l.block - 7 * ch) / ch;
    let mut out = Vec::new();
    for frame in pcm.chunks_exact(per * ch) {
        let s = |i: usize, c: usize| frame[i * ch + c] as i32;
        let mut block = vec![0u8; l.block];
        let mut st = Vec::new();
        for c in 0..ch {
            let delta = 16.max((s(2, c) - s(1, c)).abs() / 2) as i16;
            block[c] = p as u8;
            block[ch + 2 * c..ch + 2 * c + 2].copy_from_slice(&delta.to_le_bytes());
            block[3 * ch + 2 * c..3 * ch + 2 * c + 2].copy_from_slice(&(s(1, c) as i16).to_le_bytes());
            block[5 * ch + 2 * c..5 * ch + 2 * c + 2].copy_from_slice(&(s(0, c) as i16).to_le_bytes());
            st.push((s(1, c), s(0, c), delta as i32));
        }
        for j in 0..2 * (l.block - 7 * ch) {
            let (c, i) = (j % ch, 2 + j / ch);
            let (s1, s2, delta) = st[c];
            let pred = (s1 * COEF[p].0 + s2 * COEF[p].1) >> 8;
            let e = ((s(i, c) - pred) as f64 / delta as f64).round().clamp(-8.0, 7.0) as i32;
            let n = (e & 15) as u8;
            let v = (pred + e * delta).clamp(-32768, 32767);
            st[c] = (v, s1, ((ADAPT[n as usize] * delta) >> 8).max(16));
            let at = 7 * ch + j / 2;
            block[at] |= if j % 2 == 0 { n << 4 } else { n };
        }
        out.extend_from_slice(&block);
    }
    out
}

fn tone(n: usize, ch: usize, seed: u64) -> Vec<i16> {
    let mut r = Rng::new(seed);
    (0..n * ch)
        .map(|i| {
            let t = (i / ch) as f64;
            ((t * 0.05 + (i % ch) as f64).sin() * 9000.0 + (t * 0.31).sin() * 3000.0) as i16 + r.below(200) as i16 - 100
        })
        .collect()
}

#[test]
fn encoded_blocks_come_back_smaller() {
    for (ch, block, p) in [(1, 70, 1), (2, 140, 0), (2, 76, 5), (6, 516, 3), (1, 38, 6)] {
        let l = Layout::new(ch, block).unwrap();
        let d = adpcm(l, &tone(20_000, ch, ch as u64), p);
        let (used, rec) = encode(l, &d);
        assert_eq!(used, d.len());
        assert!(decode(l, &rec, used).unwrap() == d, "{ch} {block}");
        assert!(rec.len() < d.len() * 9 / 10, "{ch} {block}: {} of {}", rec.len(), d.len());
    }
}

#[test]
fn any_bytes_come_back() {
    let mut r = Rng::new(5);
    for (ch, block) in [(1, 8), (2, 140), (3, 24), (8, 64)] {
        let l = Layout::new(ch, block).unwrap();
        let d: Vec<u8> = (0..block * 20 + 7).map(|_| r.below(256) as u8).collect();
        let (used, rec) = encode(l, &d);
        assert_eq!(used, block * 20);
        assert!(decode(l, &rec, used).unwrap() == d[..used]);
    }
    let l = Layout::new(2, 140).unwrap();
    assert_eq!(encode(l, &[0; 100]).0, 0);
    assert!(decode(l, &[], 139).is_err());
}

#[test]
fn layouts() {
    assert!(Layout::new(0, 70).is_none());
    assert!(Layout::new(9, 700).is_none());
    assert!(Layout::new(2, 14).is_none());
    // (a sample of each channel takes as many nibbles: 3 channels, 2 bytes after the headers)
    assert!(Layout::new(3, 23).is_none() && Layout::new(3, 24).is_some());
}

#[test]
fn records_cut_short() {
    let l = Layout::new(2, 140).unwrap();
    let d = adpcm(l, &tone(5_000, 2, 9), 0);
    let (used, rec) = encode(l, &d);
    for n in (0..rec.len()).step_by(37) {
        if let Ok(b) = decode(l, &rec[..n], used) {
            assert_eq!(b.len(), used);
        }
    }
}
