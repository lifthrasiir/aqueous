//! Edge cases: indexes and lengths that the inputs cannot hold, claimed sizes of data, units of
//! the sizes where a chunk ends, the bits a chunk's flags and displacements leave, damaged units.

use std::time::Duration;

use aqr_common::varint;
use aqr_lznt1::lznt1::{self, CHUNK, Tok, Unit};
use aqr_lznt1::module::{self, Part};
use aqr_lznt1::recons;
use aqr_testkit::{Refusing, case, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

/// Data of runs and repeats and bytes of every kind.
fn text(n: usize) -> Vec<u8> {
    let mut x = 777u32;
    (0..n)
        .map(|i| {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            if i / 300 % 3 == 0 { b"abcab"[i % 5] } else { (x >> 20) as u8 & 0x1f }
        })
        .collect()
}

/// A chunk of `d` (1 to 4096 bytes): literals and matches of offset 1 or 5, stored where that
/// does not shrink it.
fn chunk(d: &[u8], out: &mut Vec<u8>) {
    let mut toks = Vec::new();
    let mut pos = 0;
    while pos < d.len() {
        let cap = (d.len() - pos).min(lznt1::max_len(pos));
        let mut best = (0, 0);
        for off in [1, 5] {
            if pos >= off {
                let n = (0..cap).take_while(|&i| d[pos + i] == d[pos + i - off]).count();
                if n >= 3 && n > best.0 {
                    best = (n, off);
                }
            }
        }
        if best.0 >= 3 {
            toks.push(Tok { len: best.0 as u16, off: best.1 as u16 });
            pos += best.0;
        } else {
            toks.push(Tok::LIT);
            pos += 1;
        }
    }
    let at = out.len();
    if lznt1::write_chunk(&toks, 0, d, out).is_none() || out.len() - at >= d.len() + 2 {
        out.truncate(at);
        lznt1::write_stored(d, out).unwrap();
    }
}

/// A unit of `d`: its chunks, an end header and zeros up to `cluster`.
fn unit(d: &[u8], cluster: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for c in d.chunks(CHUNK) {
        chunk(c, &mut out);
    }
    out.extend([0, 0]);
    out.resize(out.len().next_multiple_of(cluster), 0);
    out
}

#[test]
fn units_at_the_chunks_edge() {
    for n in [1, CHUNK - 1, CHUNK, CHUNK + 1, 16 * CHUNK, 16 * CHUNK + 1, recons::MAX_UNIT, recons::MAX_UNIT + 1] {
        let d = text(n);
        let u = unit(&d, 512);
        let mut dec = Vec::new();
        let mut un = Unit::default();
        lznt1::read_unit(&u, &mut dec, &mut un).unwrap();
        assert_eq!((dec.len(), un.chunks.len(), un.end), (n, n.div_ceil(CHUNK), true), "{n}");
        assert_eq!(dec, d);
        let sp = recons::split(&[(&u[..], n)]).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert_eq!(sp.data, d, "{n}");
        assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), u, "{n}");
    }
}

#[test]
fn damaged_unit_of_two_chunks() {
    let d = text(CHUNK + 300);
    let u = unit(&d, 512);
    damaged(&u, 40, 300, LIMIT, |v| {
        if let Ok(sp) = recons::split(&[(v, d.len())]) {
            assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), v);
        }
        let (mut dec, mut un) = (Vec::new(), Unit::default());
        let _ = lznt1::read_unit(v, &mut dec, &mut un);
    });
}

/// The bits of the displacement and of the length: where the split moves, at the greatest
/// offset and length that the data done allows.
#[test]
fn displacement_widths() {
    let expect = |done: usize| match done {
        0..=16 => 4,
        17..=32 => 5,
        33..=64 => 6,
        65..=128 => 7,
        129..=256 => 8,
        257..=512 => 9,
        513..=1024 => 10,
        1025..=2048 => 11,
        _ => 12,
    };
    // (no match is written with nothing done: from 1)
    for done in 1..=CHUNK {
        assert_eq!(lznt1::disp_bits(done), expect(done), "{done}");
        assert_eq!(lznt1::max_len(done), (1 << (16 - expect(done))) + 2, "{done}");
    }
    // a chunk of `done` bytes of data and one match from the farthest offset the width holds, as
    // long as it can be
    for done in [1, 2, 15, 16, 17, 31, 32, 33, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023, 1024, 1025]
        .into_iter()
        .chain([2047, 2048, 2049, 3000, 3600])
    {
        let mut d = text(done);
        let off = done.min(1 << lznt1::disp_bits(done));
        let len = (CHUNK - done).min(lznt1::max_len(done));
        if len < 3 {
            continue;
        }
        for i in 0..len {
            d.push(d[d.len() - off]);
            let _ = i;
        }
        let mut toks = vec![Tok::LIT; done];
        toks.push(Tok { len: len as u16, off: off as u16 });
        let mut c = Vec::new();
        lznt1::write_chunk(&toks, 0, &d, &mut c).unwrap_or_else(|| panic!("{done}: not written"));
        let (mut dec, mut t) = (Vec::new(), Vec::new());
        let (size, info) = lznt1::read_chunk(&c, &mut dec, &mut t).unwrap_or_else(|| panic!("{done}: not read"));
        assert_eq!((size, dec, t), (c.len(), d.clone(), toks.clone()), "{done}");
        assert!(!info.stored);
        // one more of the offset or the length is no token of this width
        for (l, o) in [(len + 1, off), (len, off + 1), (len, 0)] {
            if l == len && o == off + 1 && off < done && o <= 1 << lznt1::disp_bits(done) {
                continue;
            }
            let mut toks = toks.clone();
            *toks.last_mut().unwrap() = Tok { len: l as u16, off: o as u16 };
            let mut dd = d.clone();
            dd.resize(done, 0);
            let mut c = Vec::new();
            assert!(lznt1::write_chunk(&toks, 0, &dd, &mut c).is_none(), "{done}: {l} {o}");
        }
    }
}

#[test]
fn slack_bits_of_the_last_group() {
    // 3 literals: the flag byte has 5 unused bits above the 3 used
    let d = b"xyz";
    let toks = [Tok::LIT; 3];
    for slack in [0u8, 1, 0b10101, 0b11111] {
        let mut c = Vec::new();
        lznt1::write_chunk(&toks, slack, d, &mut c).unwrap();
        assert_eq!(c[2], slack << 3);
        let (mut dec, mut t) = (Vec::new(), Vec::new());
        let (_, info) = lznt1::read_chunk(&c, &mut dec, &mut t).unwrap();
        assert_eq!((info.slack, &dec[..]), (slack, &d[..]));
    }
    // a bit past the unused ones
    assert!(lznt1::write_chunk(&toks, 32, d, &mut Vec::new()).is_none());
    assert!(lznt1::write_chunk(&toks, 0xff, d, &mut Vec::new()).is_none());
    // 8 tokens fill the flags: no slack, none to write
    let d8 = b"abcdefgh";
    let mut c = Vec::new();
    lznt1::write_chunk(&[Tok::LIT; 8], 0, d8, &mut c).unwrap();
    assert!(lznt1::write_chunk(&[Tok::LIT; 8], 1, d8, &mut Vec::new()).is_none());
    let (mut dec, mut t) = (Vec::new(), Vec::new());
    assert_eq!(lznt1::read_chunk(&c, &mut dec, &mut t).unwrap().1.slack, 0);
    // 9 tokens: the second group has one used bit
    let d9 = b"abcdefghi";
    for slack in [0, 1, 127] {
        let mut c = Vec::new();
        lznt1::write_chunk(&[Tok::LIT; 9], slack, d9, &mut c).unwrap();
        let (mut dec, mut t) = (Vec::new(), Vec::new());
        assert_eq!(lznt1::read_chunk(&c, &mut dec, &mut t).unwrap().1.slack, slack);
    }
    assert!(lznt1::write_chunk(&[Tok::LIT; 9], 128, d9, &mut Vec::new()).is_none());
    // the slack bits through the record
    let mut u = Vec::new();
    lznt1::write_chunk(&toks, 0b10110, d, &mut u).unwrap();
    u.extend([0, 0]);
    u.resize(512, 0);
    let sp = recons::split(&[(&u[..], 3)]).unwrap();
    assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), u);
}

#[test]
fn chunks_that_do_not_read() {
    let (mut dec, mut t) = (Vec::new(), Vec::new());
    let mut rd = |b: &[u8]| lznt1::read_chunk(b, &mut dec, &mut t).is_some();
    // a signature other than 3, a header alone, a payload cut short, a flag byte alone
    assert!(!rd(&[0x00, 0xb0]));
    assert!(!rd(&[0x00, 0xa0, 0]));
    assert!(!rd(&[0x00]));
    assert!(!rd(&[0x05, 0xb0, 0]));
    assert!(!rd(&[0x00, 0xb0, 0x00]));
    // a match with no data before it, one past the chunk's end
    assert!(!rd(&[0x02, 0xb0, 0x01, 0x00, 0x00]));
    assert!(rd(&[0x01, 0xb0, 0x00, b'a']));
    // a stored chunk of the most bytes
    let mut s = vec![0xff, 0x3f];
    s.extend(vec![9u8; CHUNK]);
    assert!(rd(&s));
    s.pop();
    assert!(!rd(&s));
}

#[test]
fn claimed_lengths_beyond_the_data() {
    let d = text(5000);
    let u = unit(&d, 512);
    for n in [usize::MAX, usize::MAX / 2, 1 << 40, 1 << 33, 1 << 31] {
        case("split, huge n");
        assert!(recons::split(&[(&u[..], n)]).is_err(), "{n}");
    }
    assert!(recons::split(&[(&u[..], usize::MAX), (&u[..], 2)]).is_err());
    let mut buf = vec![0; 10];
    assert!(recons::split_into(&[(&u[..], usize::MAX), (&u[..], 2)], &mut buf).is_err());
    assert!(recons::data_into(&[(&u[..], usize::MAX), (&u[..], 2)], &mut buf).is_err());
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
    let u = unit(&d, 512);
    let sp = recons::split(&[(&u[..], d.len())]).unwrap();
    let (data, rec) = (&sp.data[..], &sp.record[..]);
    let len = 2 + u.len() as u64 + 3;
    let good = index_of(&[(2, data.len() as u64, rec.len() as u64)]);
    let inputs = |i: &[u8]| module::decode(&[], &[data, rec, b"abxyz", i], len, 1);
    let out = inputs(&good).unwrap();
    assert_eq!(&out[..2], b"ab");
    assert_eq!(out[2..2 + u.len()], u[..]);
    assert_eq!(&out[2 + u.len()..], b"xyz");
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
    let u = unit(b"a", 512);
    let sp = recons::split(&[(&u[..], 1)]).unwrap();
    let idx = index_of(&[(0, 1, sp.record.len() as u64)]);
    for len in [u64::MAX, 1 << 40] {
        case("module, huge len with data");
        assert!(module::decode(&[], &[&sp.data, &sp.record, &[], &idx], len, 1).is_err(), "{len}");
        assert!(module::decode(&[], &[&sp.data, &sp.record, &[], &idx], len, 4).is_err(), "{len}");
    }
    assert!(most() < usize::MAX);
}

#[test]
fn many_streams_on_threads() {
    let d = text(2000);
    let u = unit(&d, 512);
    let sp = recons::split(&[(&u[..], d.len())]).unwrap();
    let k = 5;
    let (data, rec) = (sp.data.repeat(k), sp.record.repeat(k));
    let part = Part { kept: 1, data: sp.data.len(), record: sp.record.len() };
    let idx = module::index(&vec![part; k]);
    let kept = vec![b'k'; k];
    let len = (k * (1 + u.len())) as u64;
    let a = module::decode(&[], &[&data, &rec, &kept, &idx], len, 1).unwrap();
    let b = module::decode(&[], &[&data, &rec, &kept, &idx], len, 4).unwrap();
    assert_eq!(a, b);
    assert!(module::decode(&[], &[&data, &rec, &kept, &idx], len + 1, 1).is_err());
    assert!(module::decode(&[], &[&data, &rec, &kept, &idx], len + 1, 4).is_err());
}
