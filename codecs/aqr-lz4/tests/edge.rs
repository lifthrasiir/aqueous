//! Edge cases of the containers: claimed sizes that the data cannot hold, damaged inputs,
//! frames of other kinds.

use std::time::Duration;

use aqr_lz4::block::{self, Compressor};
use aqr_lz4::formula::{self, MOZLZ4};
use aqr_lz4::frame;
use aqr_testkit::{Refusing, case, damaged, most};
use xxhash_rust::xxh32::xxh32;

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A frame header of FLG and BD, with the content size where FLG says so.
fn header(flg: u8, bd: u8, size: u64) -> Vec<u8> {
    let mut h = frame::MAGIC.to_vec();
    h.extend_from_slice(&[flg, bd]);
    if flg & 0x08 != 0 {
        h.extend_from_slice(&size.to_le_bytes());
    }
    h.push((xxh32(&h[4..], 0) >> 8) as u8);
    h
}

#[test]
fn mozlz4_claiming_more_than_its_block_holds() {
    let data = b"session window tab ".repeat(100);
    let blk = Compressor::compress_fast(&data, 1).unwrap();
    for claim in [u32::MAX, 1 << 31, 1 << 30, data.len() as u32 + 1, data.len() as u32 - 1] {
        let moz = [&MOZLZ4[..], &claim.to_le_bytes(), &blk].concat();
        case("mozlz4 claim");
        assert!(formula::encode(&moz, 1).is_none(), "{claim}");
        assert!(formula::extent(&moz).is_none(), "{claim}");
        assert!(most() < 1 << 20, "{claim}: {} bytes asked", most());
    }
}

/// A block of a high ratio: a literal, matches of 19 + 255 * 100 bytes (the last one shorter),
/// and a literal; it holds `n` bytes of 'a'.
fn runs(n: usize) -> Vec<u8> {
    let mut b = Vec::new();
    let mut left = n - 2;
    let mut first = true;
    while left >= 4 {
        let m = left.min(19 + 255 * 100);
        left -= m;
        if first {
            b.extend_from_slice(&[0x1f, b'a', 1, 0]);
            first = false;
        } else {
            b.extend_from_slice(&[0x0f, 1, 0]);
        }
        let extra = m - 4 - 15;
        b.extend(std::iter::repeat_n(255, extra / 255));
        b.push((extra % 255) as u8);
    }
    assert_eq!(left, 0);
    b.extend_from_slice(&[0x10, b'a']);
    b
}

#[test]
fn high_ratio_blocks_bounded_by_what_they_say() {
    // a block holding 8 MiB of 'a's in about 34 KB
    let n = 8 << 20;
    let blk = runs(n);
    assert!(blk.len() < 40_000);
    assert_eq!(block::end(&blk, n), Some(blk.len()));
    let mut out = Vec::new();
    block::decode(&blk, &mut out, 0, n).unwrap();
    assert!(out.len() == n && out.iter().all(|&b| b == b'a'));
    // a limit on the output stops it
    let mut out = Vec::new();
    assert!(block::decode(&blk, &mut out, 0, 1 << 20).is_err());
    assert!(out.len() <= 1 << 20);
    // as mozlz4 with a wrong length, nothing is made
    let moz = [&MOZLZ4[..], &((n + 1) as u32).to_le_bytes(), &blk].concat();
    case("high ratio");
    assert!(formula::encode(&moz, 1).is_none());
    assert!(most() < 1 << 20);
}

#[test]
fn frame_with_a_lying_content_size() {
    let data = b"abcabcabd".repeat(1000);
    let blk = Compressor::compress_fast(&data, 1).unwrap();
    for size in [0, 1, u64::MAX, 1 << 60, data.len() as u64] {
        let mut f = header(0x68, 0x40, size);
        f.extend_from_slice(&(blk.len() as u32).to_le_bytes());
        f.extend_from_slice(&blk);
        f.extend_from_slice(&[0; 4]);
        case("lying size");
        let r = frame::decode(&f).unwrap();
        assert_eq!(r.data, data);
        assert!(most() < 1 << 20, "{size}: {} bytes asked", most());
        let _ = formula::encode(&f, 1);
        assert!(most() < 1 << 20, "{size}: {} bytes asked", most());
    }
}

#[test]
fn other_kinds_of_frame() {
    // legacy (0x184C2102) and skippable (0x184D2A5x) frames are not taken
    let mut legacy = vec![0x02, 0x21, 0x4c, 0x18];
    legacy.extend_from_slice(&[4, 0, 0, 0, 0x40, 1, 2, 3]);
    let mut skip = vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
    skip.extend_from_slice(&fixture("f64k.lz4"));
    for d in [&legacy, &skip] {
        assert!(!formula::starts(d));
        assert!(formula::encode(d, 1).is_none());
        assert!(formula::extent(d).is_none());
        assert!(frame::decode(d).is_err());
        assert!(frame::end(d).is_none());
    }
    // a dictionary
    let h = header(0x61, 0x40, 0);
    let mut f = h[..h.len() - 1].to_vec();
    f.truncate(6);
    f.extend_from_slice(&[1, 2, 3, 4]);
    f.push((xxh32(&f[4..], 0) >> 8) as u8);
    f.extend_from_slice(&[0; 4]);
    assert!(frame::decode(&f).is_err());
    assert!(formula::encode(&f, 1).is_none());
    // a bad header checksum, reserved bits, bad block sizes
    let mut f = header(0x60, 0x40, 0);
    f.extend_from_slice(&[0; 4]);
    assert_eq!(frame::decode(&f).unwrap().data, b"");
    let n = f.len();
    f[n - 5] ^= 1;
    assert!(frame::decode(&f).is_err());
    for (flg, bd) in [(0x62, 0x40), (0x60, 0x41), (0x60, 0x30), (0x60, 0x80), (0xa0, 0x40), (0x20, 0x40)] {
        let mut f = header(flg, bd, 0);
        f.extend_from_slice(&[0; 4]);
        assert!(frame::decode(&f).is_err(), "{flg:#x} {bd:#x}");
        assert!(frame::end(&f).is_none(), "{flg:#x} {bd:#x}");
        assert!(formula::encode(&f, 1).is_none());
    }
    // a block larger than the frame says, and the end cut off
    let mut f = header(0x60, 0x40, 0);
    f.extend_from_slice(&(70_000u32).to_le_bytes());
    f.extend_from_slice(&vec![0; 70_000]);
    f.extend_from_slice(&[0; 4]);
    assert!(frame::decode(&f).is_err());
    assert!(frame::end(&f).is_none());
    let good = fixture("f64k.lz4");
    for cut in [1, 3, 4, 5] {
        assert!(frame::decode(&good[..good.len() - cut]).is_err(), "{cut}");
        assert!(frame::end(&good[..good.len() - cut]).is_none(), "{cut}");
        assert!(formula::encode(&good[..good.len() - cut], 1).is_none(), "{cut}");
    }
}

#[test]
fn damaged_inputs() {
    for name in ["f64k.lz4", "f1m-runs.lz4", "f64k-linked.lz4", "fempty.lz4", "tiny12-a1.mozlz4", "runs-a1.mozlz4"] {
        let d = fixture(name);
        damaged(&d, 150, 40, LIMIT, |v| {
            let _ = formula::extent(v);
            let _ = formula::starts(v);
            if let Ok(f) = frame::decode(v) {
                assert!(f.len <= v.len());
            }
            let _ = frame::end(v);
            if let Some(f) = formula::encode(v, 1) {
                // (an Apply of the module: running it gives what was read)
                drop(f);
            }
            assert!(most() < 128 << 20, "{} bytes asked", most());
        });
    }
}
