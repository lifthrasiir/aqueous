//! Edge cases: indexes and lengths that the inputs cannot hold, claimed sizes of data, chunks of
//! the sizes where a block ends, damaged streams.

use std::time::Duration;

use aqr_common::varint;
use aqr_testkit::{Refusing, case, damaged, most};
use aqr_xpress::module::{self, Part};
use aqr_xpress::recons::{self, huffman};
use aqr_xpress::xpress::{self, BLOCK, BlockInfo, END, Parse, SYMS, Tok};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

/// A chunk of `d` as a simple encoder makes it: a literal, or a match of the longest run of the
/// byte before (offset 1) or of the 4 bytes before (offset 4), blocks cut at 64 KiB.
fn encode(d: &[u8], end: bool) -> Vec<u8> {
    let mut toks = Vec::new();
    let mut pos = 0;
    while pos < d.len() {
        let mut best = (0, 0);
        for off in [1, 4] {
            if pos >= off {
                let n = (0..d.len() - pos).take_while(|&i| d[pos + i] == d[pos + i - off]).count();
                if n >= 3 && n > best.0 {
                    best = (n, off);
                }
            }
        }
        // (a match stops where a block would end inside it)
        let room = BLOCK - pos % BLOCK;
        if best.0 > room {
            best.0 = room;
        }
        if best.0 >= 3 {
            toks.push(Tok { len: best.0 as u32, off: best.1 as u32 });
            pos += best.0;
        } else {
            toks.push(Tok::LIT);
            pos += 1;
        }
    }
    let mut parse = Parse { end, ..Parse::default() };
    let (mut at, mut t) = (0, 0);
    while at < d.len() {
        let (from, t0) = (at, t);
        let mut freqs = [0u32; SYMS];
        while at < d.len() && at < from + BLOCK {
            let tok = toks[t];
            match tok.len {
                0 => freqs[d[at] as usize] += 1,
                l => freqs[256 + (l as usize - 3).min(15) + 16 * tok.off.ilog2() as usize] += 1,
            }
            at += tok.size();
            t += 1;
        }
        freqs[END] += (at >= d.len() && end) as u32 + 1;
        freqs[(d[0] as usize + 1) % 256] += 1;
        let mut lens = Box::new([0u8; SYMS]);
        huffman(&freqs, 15, &mut lens[..]);
        parse.blocks.push(BlockInfo { lens, toks: t - t0 });
    }
    parse.toks = toks;
    let mut out = Vec::new();
    xpress::write(&parse, d, &mut out).unwrap();
    out
}

fn text(n: usize) -> Vec<u8> {
    let mut x = 12345u32;
    (0..n)
        .map(|i| {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            if i / 700 % 3 == 0 { b"abcab"[i % 5] } else { (x >> 20) as u8 & 0x1f }
        })
        .collect()
}

#[test]
fn chunks_at_the_blocks_edge() {
    for n in [BLOCK - 1, BLOCK, BLOCK + 1, 2 * BLOCK, 2 * BLOCK + 1] {
        let d = text(n);
        for end in [false, true] {
            let c = encode(&d, end);
            let (back, parse) = xpress::read(&c, n).unwrap_or_else(|| panic!("{n} {end}: not read"));
            assert_eq!(back, d, "{n}");
            assert_eq!(parse.blocks.len(), n.div_ceil(BLOCK), "{n}");
            let sp = recons::split(&[(&c[..], n)]).unwrap_or_else(|e| panic!("{n} {end}: {e}"));
            assert_eq!(sp.data, d);
            assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), c, "{n} {end}");
            // read with another length: where the end symbol follows, no stream of the other
            // length (without it, a code of zero bits in the padding makes one more or less
            // literal another parse that writes back the same bytes)
            if end {
                assert!(xpress::read(&c, n + 1).is_none(), "{n}");
                assert!(xpress::read(&c, n - 1).is_none(), "{n}");
            }
        }
    }
}

#[test]
fn damaged_chunk_of_two_blocks() {
    let d = text(BLOCK + 300);
    let c = encode(&d, true);
    damaged(&c, 40, 300, LIMIT, |v| {
        if let Some((data, parse)) = xpress::read(v, d.len()) {
            let mut out = Vec::new();
            xpress::write(&parse, &data, &mut out).unwrap();
            assert_eq!(out, v);
        }
        if let Ok(sp) = recons::split(&[(v, d.len())]) {
            assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), v);
        }
    });
}

#[test]
fn claimed_lengths_beyond_the_data() {
    let d = text(5000);
    let c = encode(&d, true);
    for n in [usize::MAX, usize::MAX / 2, 1 << 40, 1 << 33, 1 << 31] {
        case("read, huge n");
        assert!(xpress::read(&c, n).is_none(), "{n}");
        // (a split may ask for the length claimed, fallibly: the allocator refuses it)
        assert!(most() < 1 << 24, "{n}: {} bytes asked", most());
        case("split, huge n");
        assert!(recons::split(&[(&c[..], n)]).is_err(), "{n}");
    }
    // lengths that add up past usize
    assert!(recons::split(&[(&c[..], usize::MAX), (&c[..], 2)]).is_err());
    let mut buf = vec![0; 10];
    assert!(recons::split_into(&[(&c[..], usize::MAX), (&c[..], 2)], &mut buf).is_err());
    assert!(recons::data_into(&[(&c[..], usize::MAX), (&c[..], 2)], &mut buf).is_err());
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
    let d = text(3000);
    let c = encode(&d, true);
    let sp = recons::split(&[(&c[..], d.len())]).unwrap();
    let (data, rec) = (&sp.data[..], &sp.record[..]);
    let len = 2 + c.len() as u64 + 3;
    let good = index_of(&[(2, data.len() as u64, rec.len() as u64)]);
    let inputs = |i: &[u8]| module::decode(&[], &[data, rec, b"abxyz", i], len, 1);
    // (the tail of 3 bytes after the stream)
    let out = inputs(&good).unwrap();
    assert_eq!(&out[..2], b"ab");
    assert_eq!(out[2..2 + c.len()], c[..]);
    assert_eq!(&out[2 + c.len()..], b"xyz");
    assert_eq!(module::index(&[Part { kept: 2, data: data.len(), record: rec.len() }]), good);
    let (dl, rl) = (data.len() as u64, rec.len() as u64);
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("data short", index_of(&[(2, dl - 1, rl)])),
        ("data long", index_of(&[(2, dl + 1, rl)])),
        ("record short", index_of(&[(2, dl, rl - 1)])),
        ("record long", index_of(&[(2, dl, rl + 1)])),
        ("kept long", index_of(&[(6, dl, rl)])),
        ("huge", index_of(&[(u64::MAX, u64::MAX, u64::MAX)])),
        ("huge kept", index_of(&[(u64::MAX, dl, rl)])),
        ("wrapping", index_of(&[(2, u64::MAX - dl + 1, rl), (0, dl, 0)])),
        ("split in two", index_of(&[(2, dl / 2, rl / 2), (0, dl - dl / 2, rl - rl / 2)])),
        ("none", Vec::new()),
        ("truncated", good[..good.len() - 1].to_vec()),
        ("empty varint", vec![0x80]),
        ("more streams", index_of(&[(2, dl, rl), (0, 0, 0), (0, 0, 1)])),
    ];
    for (name, i) in bad {
        if let Ok(o) = inputs(&i) {
            assert!(o != out, "{name}: the same output from a wrong index");
        }
    }
    // other shapes of the inputs
    assert!(module::decode(&[1], &[data, rec, b"abxyz", &good], len, 1).is_err());
    assert!(module::decode(&[], &[data, rec, b"abxyz"], len, 1).is_err());
    assert!(module::decode(&[], &[data, rec, b"abxyz", &good, b""], len, 1).is_err());
    for l in [len - 1, len + 1, 0] {
        assert!(module::decode(&[], &[data, rec, b"abxyz", &good], l, 1).is_err(), "{l}");
    }
}

#[test]
fn module_with_lengths_beyond_memory() {
    let empty: [&[u8]; 4] = [&[], &[], &[], &[]];
    for len in [u64::MAX, u64::MAX - 10, 1 << 62, 1 << 40, 1 << 33, 1 << 31] {
        case("module, huge len");
        // (reserved fallibly: the allocator refuses what is asked)
        assert!(module::decode(&[], &empty, len, 1).is_err(), "{len}");
    }
    // a stream of one literal claiming a length it cannot reach
    let c = encode(b"a", true);
    let sp = recons::split(&[(&c[..], 1)]).unwrap();
    let idx = index_of(&[(0, 1, sp.record.len() as u64)]);
    for len in [u64::MAX, 1 << 40] {
        case("module, huge len with data");
        assert!(module::decode(&[], &[&sp.data, &sp.record, &[], &idx], len, 1).is_err(), "{len}");
        assert!(module::decode(&[], &[&sp.data, &sp.record, &[], &idx], len, 4).is_err(), "{len}");
    }
}

#[test]
fn many_streams_on_threads() {
    let d = text(2000);
    let c = encode(&d, true);
    let sp = recons::split(&[(&c[..], d.len())]).unwrap();
    let k = 5;
    let (data, rec) = (sp.data.repeat(k), sp.record.repeat(k));
    let part = Part { kept: 1, data: sp.data.len(), record: sp.record.len() };
    let idx = module::index(&vec![part; k]);
    let kept = vec![b'k'; k];
    let len = (k * (1 + c.len())) as u64;
    let a = module::decode(&[], &[&data, &rec, &kept, &idx], len, 1).unwrap();
    let b = module::decode(&[], &[&data, &rec, &kept, &idx], len, 4).unwrap();
    assert_eq!(a, b);
    // a wrong length is the same error on either path
    assert!(module::decode(&[], &[&data, &rec, &kept, &idx], len + 1, 1).is_err());
    assert!(module::decode(&[], &[&data, &rec, &kept, &idx], len + 1, 4).is_err());
}
