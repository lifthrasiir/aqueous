//! Edge cases: CAB streams of a real compressor (windows of 2^18 to 2^20 bytes), the e8
//! translation at its edges, damaged and foreign streams, claimed sizes the data cannot hold.

use std::time::Duration;

use aqr_common::varint;
use aqr_lzx::lzx::{self, Variant};
use aqr_lzx::module::{self, Part};
use aqr_lzx::recons;
use aqr_testkit::{Refusing, case, damaged};
use sha1::{Digest, Sha1};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

/// SHA-1 of the file that `makecab` (Windows, `CompressionType=LZX`, `CompressionMemory=N`)
/// compressed into `lzx{18,19,20}.cab`: 66,000 bytes of text, x86-like calls and noise.
const SHA1: &str = "5070a345ca8ddaecee0fc5e79cdc4571e2500265";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A one-folder, one-file cabinet's LZX stream: the window's bits, the file's size, the stream,
/// and each frame's bytes.
fn cab_stream(cab: &[u8]) -> (u8, usize, Vec<u8>, Vec<usize>) {
    let u16_at = |p: usize| u16::from_le_bytes(cab[p..p + 2].try_into().unwrap()) as usize;
    let u32_at = |p: usize| u32::from_le_bytes(cab[p..p + 4].try_into().unwrap()) as usize;
    assert_eq!(&cab[..4], b"MSCF");
    let (files, folder) = (u32_at(16), 36);
    assert_eq!((u16_at(26), u16_at(28)), (1, 1), "one folder, one file");
    let (start, frames, kind) = (u32_at(folder), u16_at(folder + 4), u16_at(folder + 6));
    assert_eq!(kind & 0xf, 3, "LZX");
    let (mut at, mut stream, mut sizes) = (start, Vec::new(), Vec::new());
    for _ in 0..frames {
        let (cb, cu) = (u16_at(at + 4), u16_at(at + 6));
        assert!(cu <= lzx::FRAME);
        stream.extend_from_slice(&cab[at + 8..at + 8 + cb]);
        sizes.push(cb);
        at += 8 + cb;
    }
    (((kind >> 8) & 0x1f) as u8, u32_at(files), stream, sizes)
}

fn sha1(d: &[u8]) -> String {
    Sha1::digest(d).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn streams_of_a_real_compressor() {
    for name in ["lzx18.cab", "lzx19.cab", "lzx20.cab"] {
        let (bits, n, src, sizes) = cab_stream(&fixture(name));
        assert_eq!(n, 66000, "{name}");
        assert_eq!(sizes.len(), 3);
        let v = Variant::cab(bits).unwrap();
        let (mut d, parse) = lzx::read_as(&src, n, v).unwrap_or_else(|| panic!("{name}: not read"));
        assert_eq!(lzx::write_as(&d, &parse, v).as_deref(), Some(&src[..]), "{name}: written back");
        if let Some(size) = parse.e8 {
            lzx::e8_decode_frames(&mut d, size);
        }
        assert_eq!(sha1(&d), SHA1, "{name}: data");
        // the frames' ends are the cabinet's data blocks'
        let ends = lzx::frame_ends(&src, n, v).unwrap();
        let want: Vec<usize> = sizes
            .iter()
            .scan(0, |a, s| {
                *a += s;
                Some(*a)
            })
            .take(2)
            .collect();
        assert_eq!(ends, want, "{name}: frames");
        let sp = recons::split_stream(&src, n, v).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(sha1(&sp.data), SHA1, "{name}");
        let mut out = Vec::new();
        recons::join_stream_onto(&sp.data, &sp.record, v, &mut out).unwrap();
        assert_eq!(out, src, "{name}: rebuilt");
        // (the same stream under another window: another parse or none, never a panic)
        for other in 15..=21 {
            if other != bits {
                let w = Variant::cab(other).unwrap();
                if let Some((d, p)) = lzx::read_as(&src, n, w) {
                    assert_eq!(lzx::write_as(&d, &p, w).as_deref(), Some(&src[..]), "{name} as {other}");
                }
                let _ = recons::join_stream_onto(&sp.data, &sp.record, w, &mut Vec::new());
            }
        }
    }
}

#[test]
fn damaged_streams_of_a_real_compressor() {
    let (bits, n, src, _) = cab_stream(&fixture("lzx19.cab"));
    let v = Variant::cab(bits).unwrap();
    damaged(&src, 60, 200, LIMIT, |s| {
        if let Some((d, p)) = lzx::read_as(s, n, v) {
            // (read only what is written back as it is)
            assert_eq!(lzx::write_as(&d, &p, v).as_deref(), Some(s));
        }
        let _ = lzx::frame_ends(s, n, v);
        if let Ok(sp) = recons::split_stream(s, n, v) {
            let mut out = Vec::new();
            recons::join_stream_onto(&sp.data, &sp.record, v, &mut out).unwrap();
            assert_eq!(out, s);
        }
    });
}

#[test]
fn damaged_chunks() {
    let d = fixture("greedy.chunks");
    let (n, cl) = (
        u32::from_le_bytes(d[..4].try_into().unwrap()) as usize,
        u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize,
    );
    let chunk = d[8..8 + cl].to_vec();
    assert!(lzx::read(&chunk, n).is_some());
    damaged(&chunk, 60, 200, LIMIT, |s| {
        if let Some((data, p)) = lzx::read(s, n) {
            assert_eq!(lzx::write(&data, &p).as_deref(), Some(s));
        }
        if let Ok(sp) = recons::split(&[(s, n)]) {
            assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), s);
        }
    });
}

#[test]
fn claimed_lengths_beyond_the_data() {
    let (bits, n, src, _) = cab_stream(&fixture("lzx18.cab"));
    let v = Variant::cab(bits).unwrap();
    for n in [usize::MAX, usize::MAX / 2, 1 << 40, 1 << 33, 1 << 31, n + 1, n - 1] {
        case("read, huge n");
        assert!(lzx::read_as(&src, n, v).is_none(), "{n}");
        assert!(lzx::frame_ends(&src, n, v).is_none(), "{n}");
        case("split_stream, huge n");
        assert!(recons::split_stream(&src, n, v).is_err(), "{n}");
        case("split, huge n");
        assert!(recons::split(&[(&src[..], n)]).is_err(), "{n}");
    }
    assert!(recons::split(&[(&src[..], usize::MAX), (&src[..], 2)]).is_err());
    let mut buf = vec![0; 10];
    assert!(recons::split_into(&[(&src[..], usize::MAX), (&src[..], 2)], &mut buf).is_err());
    assert!(recons::data_into(&[(&src[..], usize::MAX), (&src[..], 2)], &mut buf).is_err());
}

#[test]
fn e8_at_its_edges() {
    // frames of 10 and 11 bytes: only the second is translated
    for len in [9usize, 10, 11, 12, 15, 16] {
        let mut d = vec![0u8; len];
        d[0] = 0xe8;
        d[1..5].copy_from_slice(&5i32.to_le_bytes());
        // (a frame of the stream's second: its start makes the translation show)
        let mut e = d.clone();
        lzx::e8_encode_frame(&mut e, lzx::FRAME, 1000);
        assert_eq!(e != d, len > 10, "{len}");
        lzx::e8_decode_frame(&mut e, lzx::FRAME, 1000);
        assert_eq!(e, d, "{len}");
        // (whole streams of such frames, for the filter of frames longer than 10)
        let mut s = [d.clone(), d.clone()].concat();
        let o = s.clone();
        lzx::e8_encode_frames(&mut s, 1000);
        lzx::e8_decode_frames(&mut s, 1000);
        assert_eq!(s, o, "{len}");
    }
    // the operands at the ends of the range: relative -at and size - 1 and the ones outside
    for size in [1i32, 2, 1000, 12_000_000, i32::MAX] {
        for pos in [0usize, 1, 5, 100, lzx::FRAME - 11, lzx::FRAME - 10] {
            for rel in [-(pos as i64) - 1, -(pos as i64), 0, size as i64 - pos as i64 - 1, size as i64 - pos as i64]
                .into_iter()
                .chain([size as i64 - 1, size as i64, size as i64 + 1, i32::MIN as i64, i32::MAX as i64])
            {
                let Ok(rel) = i32::try_from(rel) else { continue };
                let mut d = vec![0x90u8; lzx::FRAME];
                d[pos] = 0xe8;
                d[pos + 1..pos + 5].copy_from_slice(&rel.to_le_bytes());
                let mut e = d.clone();
                lzx::e8_encode_at(&mut e, 0, size);
                let at = pos as i64;
                // (the last 10 bytes of a frame are not scanned)
                let translated = pos < lzx::FRAME - 10 && rel as i64 >= -at && (rel as i64) < size as i64;
                // (at the start the near operands are their own translation)
                let same = at == 0 && (rel as i64) < size as i64;
                assert_eq!(e != d, translated && !same, "{size} {pos} {rel}");
                lzx::e8_decode_at(&mut e, 0, size);
                assert_eq!(e, d, "{size} {pos} {rel}");
            }
        }
    }
    // a frame far into a stream (a multiple of the frame size below 2^30), and past 2^30 it is not translated
    let far = (1usize << 30) - lzx::FRAME;
    for size in [12_000_000u32, u32::MAX, 1 << 31, (1 << 31) - 1] {
        let mut d = vec![0xe8u8; 100];
        let o = d.clone();
        lzx::e8_encode_frame(&mut d, far, size);
        lzx::e8_decode_frame(&mut d, far, size);
        assert_eq!(d, o, "{size}");
        let mut p = o.clone();
        lzx::e8_encode_frame(&mut p, 1 << 30, size);
        assert_eq!(p, o);
    }
    // runs of e8 bytes and operands of e8 bytes: the scan skips an operand
    let mut d = vec![0xe8u8; 64];
    d.extend([0xe8, 0xe8, 0xe8, 0xe8, 0xe8, 0, 0, 0, 0, 0, 0, 0]);
    for size in [1u32, 100, 12_000_000] {
        let mut e = d.clone();
        lzx::e8_encode_frames(&mut e, size);
        lzx::e8_decode_frames(&mut e, size);
        assert_eq!(e, d, "{size}");
    }
}

fn index_of(parts: &[(u64, u64, u64)]) -> Vec<u8> {
    let mut v = Vec::new();
    for &(a, b, c) in parts {
        varint::put(&mut v, a);
        varint::put(&mut v, b);
        varint::put(&mut v, c);
    }
    v
}

#[test]
fn module_with_inconsistent_indexes() {
    let (bits, n, src, _) = cab_stream(&fixture("lzx18.cab"));
    let v = Variant::cab(bits).unwrap();
    let sp = recons::split_stream(&src, n, v).unwrap();
    let (data, rec) = (&sp.data[..], &sp.record[..]);
    let len = 2 + src.len() as u64 + 3;
    let params = [bits];
    let good = index_of(&[(2, data.len() as u64, rec.len() as u64)]);
    let run = |i: &[u8], l: u64| module::decode(&params, &[data, rec, b"abxyz", i], l, 1);
    let out = run(&good, len).unwrap();
    assert_eq!(&out[2..2 + src.len()], &src[..]);
    assert_eq!(module::index(&[Part { kept: 2, data: data.len(), record: rec.len() }]), good);
    let streamed = module::decode_streamed(&params, &[data, rec, b"abxyz", &good], len).unwrap();
    assert_eq!(streamed, out);
    let (dl, rl) = (data.len() as u64, rec.len() as u64);
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("data short", index_of(&[(2, dl - 1, rl)])),
        ("data long", index_of(&[(2, dl + 1, rl)])),
        ("record short", index_of(&[(2, dl, rl - 1)])),
        ("record long", index_of(&[(2, dl, rl + 1)])),
        ("kept long", index_of(&[(6, dl, rl)])),
        ("huge", index_of(&[(u64::MAX, u64::MAX, u64::MAX)])),
        ("wrapping", index_of(&[(2, u64::MAX - dl + 1, rl), (0, dl, 0)])),
        ("none", Vec::new()),
        ("truncated", good[..good.len() - 1].to_vec()),
        ("empty varint", vec![0x80]),
        ("more streams", index_of(&[(2, dl, rl), (0, 0, 0), (0, 0, 1)])),
    ];
    for (name, i) in bad {
        if let Ok(o) = run(&i, len) {
            assert!(o != out, "{name}: the same output from a wrong index");
        }
        if let Ok(o) = module::decode_streamed(&params, &[data, rec, b"abxyz", &i], len) {
            assert!(o != out, "{name}: the same output from a wrong index (streamed)");
        }
    }
    assert!(module::decode(&[1], &[data, rec, b"abxyz", &good], len, 1).is_err());
    assert!(module::decode(&[bits, 0], &[data, rec, b"abxyz", &good], len, 1).is_err());
    assert!(module::decode(&[22], &[data, rec, b"abxyz", &good], len, 1).is_err());
    assert!(module::decode(&params, &[data, rec, b"abxyz"], len, 1).is_err());
    for l in [len - 1, len + 1, 0, u64::MAX] {
        assert!(run(&good, l).is_err(), "{l}");
        assert!(module::decode_streamed(&params, &[data, rec, b"abxyz", &good], l).is_err(), "{l}");
    }
}
