//! Edge cases of the container and of damaged input: dimensions of 0 and of 65535, other
//! headers, tables and endings, and the skeleton and record damaged.

use std::time::Duration;

use aqr_gif::lzw::Dict;
use aqr_gif::recons;
use aqr_testkit::{Refusing, damaged, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(60);

/// LZW data of the pixels `px` as single-pixel codes of minimum code size `min`, a clear code
/// first and before the codes widen, and an end code.
fn lzw(px: &[u8], min: u8, end: bool) -> Vec<u8> {
    let mut dict = Dict::new(min);
    let clear = dict.clear_code() as u16;
    let mut c = vec![clear];
    let mut prev = None;
    for &p in px {
        if let Some(pr) = prev {
            dict.add(pr, p);
        }
        c.push(p as u16);
        prev = Some(p as u16);
        if dict.next + 2 >= 1 << (min + 1) {
            c.push(clear);
            dict.clear();
            prev = None;
        }
    }
    if end {
        c.push(clear + 1);
    }
    recons::write(&c, min).unwrap().finish(0)
}

fn chain(data: &[u8]) -> Vec<u8> {
    let mut v = vec![];
    for b in data.chunks(255) {
        v.push(b.len() as u8);
        v.extend_from_slice(b);
    }
    v.push(0);
    v
}

/// One image's descriptor, its table, and the data's chain.
fn image(w: u16, h: u16, flags: u8, table: &[u8], min: u8, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x2c, 0, 0, 0, 0];
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&h.to_le_bytes());
    f.push(flags);
    f.extend_from_slice(table);
    f.push(min);
    f.extend(chain(data));
    f
}

fn header(version: &[u8; 6], w: u16, h: u16, global_bits: Option<u8>) -> Vec<u8> {
    let mut f = version.to_vec();
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&h.to_le_bytes());
    match global_bits {
        Some(b) => {
            f.extend_from_slice(&[0xf0 | b, 0, 0]);
            f.extend((0..3 << (b + 1)).map(|i| (i * 37) as u8));
        }
        None => f.extend_from_slice(&[0x70, 0, 0]),
    }
    f
}

fn roundtrip(d: &[u8]) -> Option<recons::Split> {
    let sp = recons::split(d).ok()?;
    let mut out = Vec::with_capacity(d.len());
    recons::join_onto(&sp.skeleton, &sp.record, sp.held, &mut out).unwrap();
    assert!(out == d, "rebuilt otherwise");
    Some(sp)
}

fn pix(n: usize, colors: u8) -> Vec<u8> {
    (0..n).map(|i| ((i * 7 + i / 5) % colors as usize) as u8).collect()
}

#[test]
fn zero_dimensions() {
    let px = pix(40, 4);
    for (w, h) in [(0, 0), (0, 5), (5, 0), (0, 65535), (65535, 0)] {
        for flags in [0, 0x40] {
            let mut f = header(b"GIF89a", w, h, Some(1));
            f.extend(image(w, h, flags, &[], 2, &lzw(&px, 2, true)));
            f.push(b';');
            let r = within(LIMIT, || roundtrip(&f)).unwrap();
            // coded as the pixels the stream has, whatever the descriptor says
            assert!(r.is_some(), "{w}x{h} flags {flags:#x}");
        }
    }
}

#[test]
fn huge_dimensions_tiny_stream() {
    let px = pix(40, 4);
    for flags in [0, 0x40] {
        let mut f = header(b"GIF89a", 65535, 65535, Some(1));
        f.extend(image(65535, 65535, flags, &[], 2, &lzw(&px, 2, true)));
        f.push(b';');
        // (the global heap refuses what the 40 pixels do not need)
        let r = within(LIMIT, || roundtrip(&f)).unwrap();
        assert!(r.is_some());
        // and the same of a screen of that size with a small image
        let mut g = header(b"GIF89a", 65535, 65535, Some(1));
        g.extend(image(8, 5, flags, &[], 2, &lzw(&px, 2, true)));
        g.push(b';');
        within(LIMIT, || roundtrip(&g)).unwrap();
    }
}

#[test]
fn headers_and_tables() {
    let px = pix(300, 4);
    let data = lzw(&px, 2, true);
    let local = [9u8, 8, 7, 6, 5, 4, 3, 2, 1, 0, 1, 2];
    let mut cases: Vec<(&str, Vec<u8>)> = vec![];
    // GIF87a
    let mut f = header(b"GIF87a", 20, 15, Some(1));
    f.extend(image(20, 15, 0, &[], 2, &data));
    f.push(b';');
    cases.push(("87a", f));
    // no global table, a local one
    let mut f = header(b"GIF89a", 20, 15, None);
    f.extend(image(20, 15, 0x81, &local, 2, &data));
    f.push(b';');
    cases.push(("local only", f));
    // no table at all
    let mut f = header(b"GIF89a", 20, 15, None);
    f.extend(image(20, 15, 0, &[], 2, &data));
    f.push(b';');
    cases.push(("no table", f));
    // global and local, two images
    let mut f = header(b"GIF89a", 20, 15, Some(1));
    f.extend(image(20, 15, 0x81, &local, 2, &data));
    f.extend(image(10, 30, 0, &[], 2, &data));
    f.push(b';');
    cases.push(("two", f));
    // bytes after the trailer
    let mut f = header(b"GIF89a", 20, 15, Some(1));
    f.extend(image(20, 15, 0, &[], 2, &data));
    f.extend_from_slice(b";trailing bytes \x2c\x00\x00");
    cases.push(("after trailer", f));
    // no trailer
    let mut f = header(b"GIF89a", 20, 15, Some(1));
    f.extend(image(20, 15, 0, &[], 2, &data));
    cases.push(("no trailer", f));
    for (name, f) in cases {
        let sp = within(LIMIT, || roundtrip(&f)).unwrap().unwrap_or_else(|| panic!("{name}: not coded"));
        assert_eq!(sp.coded, sp.images, "{name}");
    }
}

#[test]
fn cut_chains() {
    let px = pix(2000, 4);
    let data = lzw(&px, 2, true);
    let mut f = header(b"GIF89a", 50, 40, Some(1));
    f.extend(image(50, 40, 0, &[], 2, &data));
    f.push(b';');
    for cut in 1..60.min(f.len()) {
        let d = &f[..f.len() - cut];
        within(LIMIT, || roundtrip(d)).unwrap();
    }
    // a cut inside the header or the table
    for cut in 0..40 {
        within(LIMIT, || roundtrip(&f[..cut])).unwrap();
    }
}

#[test]
fn interlaced_sizes() {
    for min in [2u8, 5] {
        for h in [1u16, 2, 3, 4, 5, 7, 8, 9, 12, 13, 17] {
            for w in [1u16, 3] {
                let n = w as usize * h as usize;
                for n in [n, n.saturating_sub(1), n + 1, 0] {
                    let px = pix(n, 1 << min);
                    let mut f = header(b"GIF89a", w, h, Some(min - 1));
                    f.extend(image(w, h, 0x40, &[], min, &lzw(&px, min, true)));
                    f.push(b';');
                    within(LIMIT, || roundtrip(&f)).unwrap();
                }
            }
        }
    }
}

#[test]
fn minimum_code_sizes() {
    for min in [0u8, 1, 2, 8, 9, 10, 11, 12, 13, 255] {
        let mut f = header(b"GIF89a", 10, 10, Some(1));
        // data that is some stream whatever the size says
        f.extend(image(10, 10, 0, &[], min, &[0x84, 0x1d, 0x81, 0x7a, 0x50, 0x00, 0xff, 0x01, 0x02]));
        f.push(b';');
        within(LIMIT, || roundtrip(&f)).unwrap();
        // proper data of the size where it is one (LZW codes of 2 to 8 bits' minimum)
        if (2..=8).contains(&min) {
            let mut f = header(b"GIF89a", 10, 10, Some(1));
            f.extend(image(10, 10, 0, &[], min, &lzw(&pix(100, 4), min, true)));
            f.push(b';');
            assert!(within(LIMIT, || roundtrip(&f)).unwrap().is_some(), "min {min}");
        }
    }
}

fn sample() -> Vec<u8> {
    let px = pix(600, 4);
    let mut f = header(b"GIF89a", 30, 20, Some(1));
    f.extend_from_slice(b"!\xfe\x03abc\x00");
    f.extend(image(30, 20, 0x40, &[], 2, &lzw(&px, 2, true)));
    f.extend(image(5, 6, 0x81, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], 2, &lzw(&pix(30, 4), 2, true)));
    f.push(b';');
    f
}

#[test]
fn damaged_skeleton() {
    let f = sample();
    let sp = roundtrip(&f).unwrap();
    assert_eq!(sp.coded, 2);
    damaged(&sp.skeleton, 3, 64, LIMIT, |sk| {
        // an error or other bytes, never a panic
        let mut out = Vec::with_capacity(f.len());
        let _ = recons::join_onto(sk, &sp.record, sp.held, &mut out);
        let _ = recons::plan(sk, &sp.record, sp.held);
    });
}

#[test]
fn damaged_record() {
    let f = sample();
    let sp = recons::split(&f).unwrap();
    damaged(&sp.record, 3, 64, LIMIT, |r| {
        let mut out = Vec::with_capacity(f.len());
        let _ = recons::join_onto(&sp.skeleton, r, sp.held, &mut out);
        let _ = recons::plan(&sp.skeleton, r, sp.held);
    });
}

#[test]
fn damaged_files() {
    let f = sample();
    damaged(&f, 3, 64, LIMIT, |d| {
        // whatever splits joins back
        if let Ok(sp) = recons::split(d) {
            let mut out = Vec::with_capacity(d.len());
            recons::join_onto(&sp.skeleton, &sp.record, sp.held, &mut out).unwrap();
            assert!(out == d, "rebuilt otherwise");
        }
    });
}
