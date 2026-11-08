//! Inputs damaged or crafted (the lengths, counts and sizes in them) do not make the decoders or
//! the encoder reserve memory the data cannot hold, rather than aborting the process on an
//! allocation of gigabytes or terabytes.

mod common;

use std::cell::{Cell, RefCell};

use aqr_png::{formula, module};
use common::*;

/// A global allocator that remembers the largest single request of each thread, and refuses
/// (as an exhausted heap does) those of 1 GiB and above: the aborted allocation is how a count
/// from the input reserving its room before it is checked shows. The abort tells the case.
mod big_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::{Cell, RefCell};
    use std::io::Write;

    pub struct Largest;

    thread_local! {
        static MOST: Cell<usize> = const { Cell::new(0) };
        pub static CASE: RefCell<[u8; 120]> = const { RefCell::new([0; 120]) };
    }

    const REFUSED: usize = 1 << 30;

    fn note(n: usize) -> bool {
        let _ = MOST.try_with(|m| m.set(m.get().max(n)));
        if n < REFUSED {
            return true;
        }
        let _ = CASE.try_with(|c| {
            if let Ok(c) = c.try_borrow() {
                let end = c.iter().position(|&b| b == 0).unwrap_or(c.len());
                let mut e = std::io::stderr();
                let _ = e.write_all(b"refused allocation, in case: ");
                let _ = e.write_all(&c[..end]);
                let _ = e.write_all(b"\n");
            }
        });
        false
    }

    unsafe impl GlobalAlloc for Largest {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if note(l.size()) { unsafe { System.alloc(l) } } else { std::ptr::null_mut() }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            if note(l.size()) { unsafe { System.alloc_zeroed(l) } } else { std::ptr::null_mut() }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            if note(n) { unsafe { System.realloc(p, l, n) } } else { std::ptr::null_mut() }
        }
    }

    pub fn reset() {
        MOST.with(|m| m.set(0));
    }

    pub fn most() -> usize {
        MOST.with(Cell::get)
    }

    pub fn case(s: &str) {
        CASE.with(|c| {
            let mut c = c.borrow_mut();
            let n = s.len().min(c.len() - 1);
            c[..n].copy_from_slice(&s.as_bytes()[..n]);
            c[n] = 0;
        });
    }
}

#[global_allocator]
static ALLOC: big_alloc::Largest = big_alloc::Largest;

/// What the inputs here (a few kilobytes of image) may reserve at most.
const LIMIT: usize = 48 << 20;

thread_local! {
    static WORST: Cell<usize> = const { Cell::new(0) };
    static BAD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` as the case `name`, noting the largest single allocation.
fn case<R>(name: &str, f: impl FnOnce() -> R) -> R {
    big_alloc::case(name);
    big_alloc::reset();
    let r = f();
    let most = big_alloc::most();
    WORST.with(|w| w.set(w.get().max(most)));
    if most > LIMIT {
        BAD.with(|b| b.borrow_mut().push(format!("{name}: {most} bytes")));
    }
    r
}

fn verdict() {
    let bad = BAD.with(|b| b.take());
    assert!(bad.is_empty(), "reserved more than the data holds: {bad:?}");
}

/// `i` damaged in each of its streams, at about `per` places each, several ways.
fn damaged(i: &[Vec<u8>], per: usize, mut f: impl FnMut(&[Vec<u8>], &str)) {
    for s in 0..i.len() {
        let step = (i[s].len() / per).max(1);
        for at in (0..i[s].len()).step_by(step) {
            for (how, v) in [("ff", 0xffu8), ("80", 0x80), ("7f", 0x7f), ("00", 0), ("01", 1)] {
                let mut j = i.to_vec();
                if j[s][at] == v {
                    continue;
                }
                j[s][at] = v;
                f(&j, &format!("stream {s} at {at} to {how}"));
            }
            let mut j = i.to_vec();
            j[s][at] ^= 0x5a;
            f(&j, &format!("stream {s} at {at} xor"));
        }
    }
}

fn files() -> Vec<(&'static str, Vec<u8>)> {
    let single = |w: u32, h: u32, depth: u8, color: u8, il: u8| {
        let data = image_data(w as usize, h as usize, depth, color, il == 1, 3);
        let z = zlib(&zlib6(), &data);
        let mut cs = vec![(b"IHDR", ihdr(w, h, depth, color, il))];
        if color == 3 {
            cs.push((b"PLTE", (0..48).map(|i| (i * 7) as u8).collect()));
        }
        cs.extend(idats(&z, 700));
        cs.push((b"IEND", vec![]));
        png(&cs)
    };
    vec![
        ("rgb", single(60, 40, 8, 2, 0)),
        ("rgba interlaced", single(37, 29, 8, 6, 1)),
        ("palette 4", single(50, 20, 4, 3, 0)),
        ("gray16", single(33, 21, 16, 0, 0)),
        ("apng", apng(&[(24, 16), (20, 10), (24, 16)], 2)),
    ]
}

/// The head, rANS stream and raw bits of each record of a formula's inputs joined.
fn joined(i: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
    let (index, heads, sts, raws) = (&i[1], &i[2], &i[3], &i[4]);
    let (mut at, mut h, mut s, mut r) = (0, 0, 0, 0);
    let (mut ix, mut recs) = (vec![], vec![]);
    while at < index.len() {
        let n = aqr_common::varint::get(index, &mut at).unwrap() as usize;
        let head = &heads[h..h + n];
        h += n;
        let mut lens = [0usize; 3];
        if head[0] != 0 {
            let mut k = 1;
            for _ in 0..2 {
                let n = aqr_common::varint::get(head, &mut k).unwrap() as usize;
                k += n;
            }
            for l in &mut lens {
                *l = aqr_common::varint::get(head, &mut k).unwrap() as usize;
            }
        }
        recs.extend_from_slice(head);
        recs.extend_from_slice(&sts[s..s + lens[1]]);
        recs.extend_from_slice(&raws[r..r + lens[2]]);
        s += lens[1];
        r += lens[2];
        aqr_common::varint::put(&mut ix, (n + lens[1] + lens[2]) as u64);
    }
    (ix, recs)
}

#[test]
fn damaged_records_reserve_what_the_data_holds() {
    for (name, file) in files() {
        let Some(f) = formula::encode(&file, 1) else { continue };
        let i: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
        let len = file.len() as u64;
        damaged(&i, 150, |j, what| {
            let r: Vec<&[u8]> = j.iter().map(|v| &v[..]).collect();
            case(&format!("{name} whole {what}"), || {
                let _ = module::decode(&[], &r, len, 1);
            });
            case(&format!("{name} part {what}"), || {
                let _ = aqr_png::part::decode_streamed(&[], &r, len);
            });
        });
    }
    verdict();
}

#[test]
fn damaged_sets_reserve_what_the_data_holds() {
    for (name, file) in files() {
        let Some(f) = formula::encode(&file, 1) else { continue };
        let i: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
        let (ix, recs) = joined(&i);
        let len = file.len() as u64;
        let mut params = vec![];
        for v in [1u64, 0, len] {
            aqr_common::varint::put(&mut params, v);
        }
        let set = vec![aqr_common::packed::pack(&[&i[0]]), aqr_common::packed::pack(&[&ix]), aqr_common::packed::pack(&[&recs])];
        case(&format!("{name} set"), || assert_eq!(module::decode_set(&params, &[&set[0], &set[1], &set[2]], len, 1).unwrap(), file));
        // the same file twice, the second against the first
        let other = apng(&[(24, 16), (20, 10), (24, 16)], 9);
        let _ = other;
        damaged(&set, 100, |j, what| {
            case(&format!("{name} set {what}"), || {
                let _ = module::decode_set(&params, &[&j[0], &j[1], &j[2]], len, 1);
            });
        });
        // the parameters damaged
        for at in 0..params.len() {
            for v in [0xffu8, 0x7f, 0x80, 0] {
                let mut p = params.clone();
                p[at] = v;
                case(&format!("{name} set params {at} to {v}"), || {
                    let _ = module::decode_set(&p, &[&set[0], &set[1], &set[2]], len, 1);
                });
            }
        }
        // a varint of 2^63 and over in the sizes
        for big in [1u64 << 40, 1 << 62, 1 << 63, u64::MAX] {
            let mut p = vec![];
            for v in [1u64, 0, big] {
                aqr_common::varint::put(&mut p, v);
            }
            case(&format!("{name} set size {big}"), || {
                let _ = module::decode_set(&p, &[&set[0], &set[1], &set[2]], big, 1);
            });
        }
    }
    verdict();
}

#[test]
fn damaged_references_reserve_what_the_data_holds() {
    let a = apng(&[(24, 16), (20, 10), (24, 16)], 2);
    let b = apng(&[(24, 16), (20, 10), (24, 16)], 3);
    let Some(f) = formula::encode_against(&b, &a, 1) else { return };
    let i: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    if i.len() != 4 {
        return;
    }
    let len = b.len() as u64;
    damaged(&i, 150, |j, what| {
        case(&format!("against {what}"), || {
            let _ = module::decode_against(&[], &[&j[0], &j[1], &j[2], &j[3]], len, 1);
        });
    });
    verdict();
}

/// A PNG file of `w` x `h` at `depth` bits and `color`, its data a few bytes of a zlib stream.
fn crafted(w: u32, h: u32, depth: u8, color: u8, il: u8, data: &[u8]) -> Vec<u8> {
    let z = zlib(&zlib6(), data);
    png(&[(b"IHDR", ihdr(w, h, depth, color, il)), (b"IDAT", z), (b"IEND", vec![])])
}

#[test]
fn crafted_files_reserve_what_the_data_holds() {
    for (w, h) in [(u32::MAX, 1), (1, u32::MAX), (1 << 20, 1 << 20), (1 << 30, 3), (60000, 60000), (1 << 31, 1 << 31)] {
        for (depth, color) in [(8, 0), (16, 6), (1, 0), (8, 3)] {
            for il in [0, 1] {
                for data in [&[0u8; 0][..], &[0u8; 64][..], &[0u8; 5000][..]] {
                    let file = crafted(w, h, depth, color, il, data);
                    case(&format!("encode {w}x{h} {depth}/{color} il {il} data {}", data.len()), || {
                        let _ = formula::encode(&file, 1);
                    });
                    case(&format!("references {w}x{h} {depth}/{color} il {il} data {}", data.len()), || {
                        let _ = formula::encode_against(&file, &file, 1);
                    });
                }
            }
        }
    }
    verdict();
}

/// The inputs of `file`'s formula with its IDAT chunk declared `idat` bytes long (the file the
/// skeleton makes of that length), and the layout of the one image's record written as
/// `layout` writes it: a stream of one fixed block of `count` tokens, `n` bytes of data.
fn declared(file: &[u8], idat: u64, n: u32, count: u32) -> (Vec<Vec<u8>>, u64) {
    let f = formula::encode(file, 1).expect("coded");
    let mut i: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    // the skeleton's IDAT chunk: its kind, then its length as a varint
    let p = i[0].windows(4).position(|w| w == b"IDAT").expect("an IDAT chunk") + 4;
    let mut at = p;
    let old = aqr_common::varint::get(&i[0], &mut at).unwrap();
    let mut sk = i[0][..p].to_vec();
    aqr_common::varint::put(&mut sk, idat);
    sk.extend_from_slice(&i[0][at..]);
    i[0] = sk;
    // the head: the mode, the layout, the rest
    let head = &i[2];
    let mut at = 1;
    let llen = aqr_common::varint::get(head, &mut at).unwrap() as usize;
    let rest = &head[at + llen..];
    let mut lw = aqr_png::rans::BitW::default();
    lw.bits(1, 0);
    lw.gamma(n);
    lw.gamma(0);
    lw.bits(8, 0x78);
    lw.bits(8, 0x9c);
    lw.gamma(1);
    lw.bits(1, 1);
    lw.bits(2, 1);
    lw.bits(1, 0);
    lw.gamma(count);
    lw.bits(8, 0);
    lw.gamma(0);
    lw.gamma(0);
    let layout = lw.finish();
    let mut h = vec![head[0]];
    aqr_common::varint::put(&mut h, layout.len() as u64);
    h.extend_from_slice(&layout);
    h.extend_from_slice(rest);
    let mut index = vec![];
    aqr_common::varint::put(&mut index, h.len() as u64);
    (i[1], i[2]) = (index, h);
    (i, file.len() as u64 - old + idat)
}

#[test]
fn token_counts_beyond_the_payload() {
    let data = image_data(60, 40, 8, 2, false, 5);
    let file = crafted(60, 40, 8, 2, 0, &data);
    // an image of a payload of 1 MiB (as the skeleton declares it), of a billion tokens
    let (i, len) = declared(&file, 1 << 20, 1 << 30, 1 << 30);
    let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
    let (res, most) = {
        big_alloc::reset();
        let res = module::decode(&[], &r, len, 1);
        (res, big_alloc::most())
    };
    assert!(res.is_err());
    assert!(most < (200 << 20), "{most} bytes reserved");
    big_alloc::reset();
    let res = aqr_png::part::decode_streamed(&[], &r, len);
    assert!(res.is_err());
    assert!(big_alloc::most() < (200 << 20), "{} bytes reserved", big_alloc::most());
}

#[test]
fn plan_of_a_set_of_a_count_beyond_the_params() {
    let mut params = vec![];
    aqr_common::varint::put(&mut params, 1 << 40);
    big_alloc::reset();
    let packed = aqr_common::packed::pack::<&[u8]>(&[]);
    assert!(aqr_png::plan::set(0, &params, &[], (&packed, &packed, &packed)).is_none());
    assert!(big_alloc::most() < (1 << 20), "{} bytes reserved", big_alloc::most());
}

#[test]
fn rows_beyond_the_payload() {
    let data = image_data(60, 40, 8, 2, false, 5);
    let file = crafted(60, 40, 8, 2, 0, &data);
    let f = formula::encode(&file, 1).expect("coded");
    let i: Vec<Vec<u8>> = f.streams.iter().map(|s| s.data.to_vec()).collect();
    // the skeleton's IHDR: the kind, its length, the flags, the CRC if given, then the data
    let mut sk = i[0].clone();
    let mut at = sk.windows(4).position(|w| w == b"IHDR").expect("an IHDR chunk") + 4;
    aqr_common::varint::get(&sk, &mut at).unwrap();
    at += 1 + 4 * (sk[at] >> 7) as usize;
    // a row of 12 GiB
    sk[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut j = i.clone();
    j[0] = sk;
    let r: Vec<&[u8]> = j.iter().map(|v| &v[..]).collect();
    big_alloc::reset();
    assert!(module::decode(&[], &r, file.len() as u64, 1).is_err());
    assert!(aqr_png::part::decode_streamed(&[], &r, file.len() as u64).is_err());
    assert!(big_alloc::most() < (200 << 20), "{} bytes reserved", big_alloc::most());
}
