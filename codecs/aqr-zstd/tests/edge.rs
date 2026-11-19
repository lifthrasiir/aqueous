//! Edge cases: damaged and malformed frames through `parse`/`decode`, the layout's varints near
//! `u64::MAX`, and what the emulation does not take.

#![allow(clippy::single_range_in_vec_init)]

use std::time::Duration;

use aqr_testkit::damaged;
use aqr_zstd::compress::compress;
use aqr_zstd::frames::{self, KEPT, NEW, PIECE, Params, SHIFT};
use aqr_zstd::{CONTENT_MAX, MAGIC, parse, starts};

const LIMIT: Duration = Duration::from_secs(20);

fn sample() -> Vec<u8> {
    (0..50_000u32).map(|i| (i / 7 % 251) as u8).collect()
}

fn varint(v: u64) -> Vec<u8> {
    let mut b = Vec::new();
    aqr_common::varint::put(&mut b, v);
    b
}

#[test]
fn empty_and_short() {
    assert!(parse(b"").is_none());
    assert!(parse(&MAGIC).is_none());
    assert!(parse(&MAGIC[..3]).is_none());
    assert!(!starts(b""));
    assert!(!starts(&MAGIC));
    // an empty frame (the smallest) holds
    let z = zstd::bulk::compress(b"", 3).unwrap();
    let fr = parse(&z).unwrap();
    assert_eq!(fr.len, z.len());
    assert!(frames::decode(&[], &[], 0, 1).is_err());
}

/// A frame by hand: descriptor, the rest of the header, the blocks.
fn hand(fhd: u8, header_rest: &[u8], blocks: &[u8]) -> Vec<u8> {
    let mut d = MAGIC.to_vec();
    d.push(fhd);
    d.extend_from_slice(header_rest);
    d.extend_from_slice(blocks);
    d
}

fn block(last: bool, kind: u32, size: u32) -> [u8; 3] {
    let h = last as u32 | kind << 1 | size << 3;
    [h as u8, (h >> 8) as u8, (h >> 16) as u8]
}

#[test]
fn headers() {
    // a raw block of 3 bytes
    let ok = |fhd: u8, rest: &[u8]| {
        let mut b = block(true, 0, 3).to_vec();
        b.extend_from_slice(b"abc");
        hand(fhd, rest, &b)
    };
    let content = |d: &[u8]| parse(d).unwrap().frames().next().unwrap().content;
    assert!(parse(&ok(0, &[0])).is_some());
    // the reserved block type
    let mut b = block(true, 3, 3).to_vec();
    b.extend_from_slice(b"abc");
    assert!(parse(&hand(0, &[0], &b)).is_none());
    assert!(!starts(&hand(0, &[0], &b)));
    // the reserved bit of the descriptor
    assert!(parse(&ok(0x08, &[0])).is_none());
    // the checksum flag with no checksum bytes: cut short
    assert!(parse(&ok(0x04, &[0])).is_none());
    // a dictionary ID of each width
    assert_eq!(parse(&ok(1, &[0, 7])).unwrap().frames().next().unwrap().dict, 7);
    assert_eq!(parse(&ok(3, &[0, 1, 2, 3, 4])).unwrap().frames().next().unwrap().dict, 0x04030201);
    // single segment with no content size is no frame; with one it is
    assert!(parse(&ok(0x20, &[])).is_none());
    assert_eq!(content(&ok(0x20, &[3])), Some(3));
    // the 2-byte content size is told less 256, the 8-byte one as it is
    assert_eq!(content(&ok(0x40, &[0, 0xff, 0xff])), Some(0xffff + 256));
    assert_eq!(content(&ok(0xc0, &[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])), Some(u64::MAX));
    // the largest window descriptor
    assert!(parse(&ok(0, &[0xff])).is_some());
    // blocks over the window (here 1 KiB) or 128 KiB are no frame
    let mut b = block(true, 0, 2000).to_vec();
    b.extend(std::iter::repeat_n(0, 2000));
    assert!(parse(&hand(0, &[0], &b)).is_none());
    let mut b = block(true, 0, (128 << 10) + 1).to_vec();
    b.extend(std::iter::repeat_n(0, (128 << 10) + 1));
    assert!(parse(&hand(0, &[0xff], &b)).is_none());
    // a block of 128 KiB with a window to hold it
    let mut b = block(true, 0, 128 << 10).to_vec();
    b.extend(std::iter::repeat_n(0, 128 << 10));
    assert!(parse(&hand(0, &[0xff], &b)).is_some());
    // an RLE block's size is of the content, not the bytes
    let mut b = block(true, 1, 1000).to_vec();
    b.push(9);
    let f = parse(&hand(0, &[0x10], &b)).unwrap();
    assert_eq!(f.frames().next().unwrap().bound, 1000);
    // a frame without its final block
    let mut b = block(false, 0, 3).to_vec();
    b.extend_from_slice(b"abc");
    assert!(parse(&hand(0, &[0], &b)).is_none());
}

#[test]
fn skippables() {
    let mut z = zstd::bulk::compress(&sample(), 3).unwrap();
    let n = z.len();
    // a skippable frame claiming u32::MAX is not one (and cannot overflow)
    z.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 0xff, 0xff, 0xff, 0xff, 1, 2]);
    let fr = parse(&z).unwrap();
    assert_eq!((fr.pieces.len(), fr.len), (1, n));
    // each of the 16 magics; a skippable frame cut in its size
    for m in 0..16u8 {
        let mut d = z[..n].to_vec();
        d.extend_from_slice(&[0x50 | m, 0x2a, 0x4d, 0x18, 0, 0, 0, 0]);
        assert_eq!(parse(&d).unwrap().pieces.len(), 2);
        d.truncate(n + 6);
        assert_eq!(parse(&d).unwrap().pieces.len(), 1);
    }
    // leading skippable frames are no start
    let mut d = vec![0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0];
    d.extend_from_slice(&z[..n]);
    assert!(parse(&d).is_none());
    // a frame telling more content than CONTENT_MAX is parsed but not decoded
    let mut b = block(true, 1, 10).to_vec();
    b.push(0);
    let big = hand(0xe0, &(CONTENT_MAX + 1).to_le_bytes(), &b);
    let fr = parse(&big).unwrap();
    assert!(aqr_zstd::decode(&big, &fr, u64::MAX).is_none());
}

#[test]
fn damaged_frames() {
    let mut d = zstd::bulk::compress(&sample(), 3).unwrap();
    d.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 2, 0, 0, 0, 1, 2]);
    let mut c = zstd::bulk::Compressor::new(3).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
    d.extend_from_slice(&c.compress(b"checked content, checked content").unwrap());
    damaged(&d, 200, 64, LIMIT, |v| {
        let _ = starts(v);
        if let Some(fr) = parse(v) {
            assert!(fr.len <= v.len());
            let _ = aqr_zstd::decode(v, &fr, 1 << 20);
        }
    });
}

#[test]
fn decode_limits() {
    let data = sample();
    let z = zstd::bulk::compress(&data, 3).unwrap();
    let fr = parse(&z).unwrap();
    assert!(aqr_zstd::decode(&z, &fr, u64::MAX).is_some());
    assert!(aqr_zstd::decode(&z, &fr, data.len() as u64 - 1).is_none());
    assert!(aqr_zstd::decode(&z, &fr, 0).is_none());
    // a frame with a dictionary is not taken
    let mut b = block(true, 0, 3).to_vec();
    b.extend_from_slice(b"abc");
    let d = hand(1, &[0, 7], &b);
    let fr = parse(&d).unwrap();
    assert!(aqr_zstd::decode(&d, &fr, u64::MAX).is_none());
    // a content size that lies
    let mut b = block(true, 0, 3).to_vec();
    b.extend_from_slice(b"abc");
    let d = hand(0x20, &[4], &b);
    let fr = parse(&d).unwrap();
    assert!(aqr_zstd::decode(&d, &fr, u64::MAX).is_none());
}

#[test]
fn params_extremes() {
    let ok = Params { level: 3, content: 1000, pool: 1 << 20, open: 8 };
    assert_eq!(Params::parse(&ok.bytes()).unwrap(), ok);
    for p in [
        Params { content: aqr_zstd::compress::CONTENT_MAX as u64 + 1, ..ok },
        Params { content: CONTENT_MAX, ..ok },
        Params { content: u64::MAX, ..ok },
        Params { pool: (1 << 40) + 1, ..ok },
        Params { pool: u64::MAX, ..ok },
        Params { open: (1 << 32) + 1, ..ok },
        Params { open: u64::MAX, ..ok },
        Params { level: 2, ..ok },
        Params { level: 4, ..ok },
        Params { level: 0, ..ok },
    ] {
        assert!(Params::parse(&p.bytes()).is_err(), "{p:?}");
    }
    let mut b = ok.bytes();
    b.push(0);
    assert!(Params::parse(&b).is_err());
    assert!(Params::parse(&[]).is_err());
    assert!(Params::parse(&[3, 1, 1]).is_err());
    assert!(Params::parse(&[3, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 1]).is_err());
}

#[test]
fn pool_extremes() {
    let mut p = frames::Pool::new(u64::MAX);
    assert!(p.take(u64::MAX).is_none());
    assert!(p.take(u64::MAX - 10).is_none());
    let mut p = frames::Pool::new(u64::MAX);
    assert!(p.take(1 << 63).is_some());
    assert!(p.take(1 << 63).is_none());
    let mut p = frames::Pool::new(1 << 20);
    assert!(p.take(0).is_some());
    assert!(p.take((1 << 20) + 1).is_none());
}

fn run(layout: &[u8], kept: &[u8], lengths: &[u8], contents: &[u8], size: u64) -> Result<Vec<u8>, frames::Error> {
    let p = Params { level: 3, content: 1000, pool: 4096, open: 4 };
    frames::decode(&p.bytes(), &[layout, kept, lengths, contents], size, 1)
}

#[test]
fn layout_varints() {
    let tag = |n: u64, k: u64| varint(n << SHIFT | k);
    let content = [7u8; 100];
    let z = compress(&content, 3).unwrap();
    let zl = z.len() as u64;
    // the honest layout, to be sure of the harness
    let mut l = tag(zl, NEW);
    l.extend(varint(0));
    assert!(run(&l, b"", &varint(100), &content, zl).unwrap() == z);
    // lengths and sizes near u64::MAX
    for n in [u64::MAX >> SHIFT, (u64::MAX >> SHIFT) - 1, 1 << 40, 1 << 31, 5000, zl + 1] {
        for k in [KEPT, NEW, PIECE] {
            let mut l = tag(n, k);
            if k == NEW {
                l.extend(varint(0));
            }
            if k == PIECE {
                l.extend(varint(0));
                l.extend(varint(0));
            }
            assert!(run(&l, b"abc", &varint(100), &content, n.wrapping_add(3)).is_err());
            assert!(run(&l, b"abc", &varint(100), &content, zl).is_err());
        }
    }
    // zigzag deltas of the extremes, and frame indexes of u64::MAX
    for delta in [u64::MAX, u64::MAX - 1, 1, 2, 1 << 63] {
        let mut l = tag(zl, NEW);
        l.extend(varint(delta));
        assert!(run(&l, b"", &varint(100), &content, zl).is_err());
    }
    for j in [u64::MAX, 1, 1 << 63] {
        let mut l = tag(zl / 2, NEW);
        l.extend(varint(0));
        l.extend(tag(zl - zl / 2, PIECE));
        l.extend(varint(j));
        l.extend(varint(0));
        assert!(run(&l, b"", &varint(100), &content, zl).is_err(), "{j}");
    }
    // a content length past the params' bound, or past the contents
    let mut l = tag(zl, NEW);
    l.extend(varint(0));
    assert!(run(&l, b"", &varint(1001), &content, zl).is_err());
    assert!(run(&l, b"", &varint(u64::MAX), &content, zl).is_err());
    assert!(run(&l, b"", &varint(101), &content, zl).is_err());
    // the size told too large
    assert!(run(&l, b"", &varint(100), &content, u64::MAX).is_err());
    // tag 3 is none
    assert!(run(&tag(1, 3), b"a", &varint(100), &content, 1).is_err());
    // truncated varints
    assert!(run(&[0x80], b"", &[], &[], 0).is_err());
    assert!(run(&l[..1], b"", &varint(100), &content, zl).is_err());
}

#[test]
fn emulated_blocks() {
    // RLE blocks, across 128 KiB
    for n in [1, 2, 127, 128, 1000, 131_072, 131_073, 200_000, 262_143] {
        let z = compress(&vec![0u8; n], 3).unwrap();
        assert!(z == zstd::bulk::compress(&vec![0u8; n], 3).unwrap(), "zeros {n}");
    }
    // raw blocks across 128 KiB
    let mut s = 0x1234_5678_9abc_def1u64;
    let noise: Vec<u8> = (0..262_143)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect();
    for n in [100, 131_071, 131_072, 131_073, 200_000, 262_143] {
        let z = compress(&noise[..n], 3).unwrap();
        assert!(z == zstd::bulk::compress(&noise[..n], 3).unwrap(), "noise {n}");
    }
    // levels other than 3, and other strategies
    for level in [-5, 0, 1, 2, 4, 5, 9, 19, 22, i32::MIN, i32::MAX] {
        assert!(compress(&noise[..1000], level).is_none(), "{level}");
    }
    assert!(compress(&[noise.as_slice(), &[1]].concat(), 3).is_none());
}

/// A frame with a checksum is not made again (libzstd's default writes none): it is kept as
/// bytes, or the formula is not made.
#[test]
fn checksummed_frames() {
    let content = sample();
    let mut c = zstd::bulk::Compressor::new(3).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true)).unwrap();
    let z = c.compress(&content).unwrap();
    assert!(z != compress(&content, 3).unwrap());
    let f = aqr_zstd::formula::frames_in(&z, &[vec![0..z.len()]], 3, aqr_formula::Kind::Table, 1);
    assert!(f.is_none());
    // beside a plain frame: whatever is made again, the file is as it was
    let plain = compress(&content, 3).unwrap();
    let d = [&z[..], &plain[..]].concat();
    let fr = [vec![0..z.len()], vec![z.len()..d.len()]];
    if let Some(f) = aqr_zstd::formula::frames_in(&d, &fr, 3, aqr_formula::Kind::Table, 1) {
        let aqr_formula::Node::Apply(a) = &f.root else { panic!() };
        let ins: Vec<&[u8]> = f.streams[1..].iter().map(|s| &s.data[..]).collect();
        assert!(frames::decode(&a.params, &ins, d.len() as u64, 1).unwrap() == d);
    }
}
