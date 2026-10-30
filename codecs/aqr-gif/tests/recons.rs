use std::collections::HashMap;

use aqr_common::rng::Rng;
use aqr_gif::lzw::{self, CODES, Dict};
use aqr_gif::recons;

/// The files of `scripts/gen-fixtures.py` (and an icon of an encoder that defines codes
/// again), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "gif"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

fn roundtrip(d: &[u8]) -> recons::Split {
    let sp = recons::split(d).unwrap();
    let mut out = Vec::new();
    recons::join_onto(&sp.skeleton, &sp.record, &mut out).unwrap();
    assert!(out == d, "rebuilt otherwise");
    sp
}

#[test]
fn files_of_encoders() {
    for (name, d) in fixtures() {
        let sp = recons::split(&d).unwrap();
        assert_eq!(sp.coded, sp.images, "{name}");
        let mut out = Vec::new();
        recons::join_onto(&sp.skeleton, &sp.record, &mut out).unwrap();
        assert!(out == d, "{name}: rebuilt otherwise");
        let data = d.len() - sp.skeleton.len();
        assert!(sp.record.len() * 4 < data * 3, "{name}: record of {} bytes for {data}", sp.record.len());
        // the encoders' codes are those of the longest match (but for the one that defines codes
        // again)
        if !name.starts_with("dup") {
            assert!(sp.cost[2] < 64.0, "{name}: codes of {} bits", sp.cost[2]);
        }
    }
}

/// How a test encoder writes the codes.
#[derive(Clone, Copy, Default)]
struct Enc {
    /// A clear code first.
    first_clear: bool,
    /// Clear where the next code to define reaches this (none where 0).
    clear_at: usize,
    /// Clear at random, 1 in so many codes (none where 0).
    random_clear: u64,
    /// Codes shorter than the longest match, 1 in so many (none where 0).
    shorter: u64,
    /// Single pixels only.
    literal: bool,
    /// Codes defined again for the same pixels taken at random.
    any_code: bool,
    end: bool,
}

/// The codes of `px` written by the encoder `e`, of minimum code size `min`.
fn codes(rng: &mut Rng, px: &[u8], min: u8, e: Enc) -> Vec<u16> {
    let mut dict = Dict::new(min);
    let clear = dict.clear_code() as u16;
    let roots = || -> HashMap<Vec<u8>, Vec<u16>> { (0..clear).map(|c| (vec![c as u8], vec![c])).collect() };
    let mut strings = roots();
    let mut out = vec![];
    if e.first_clear {
        out.push(clear);
    }
    let (mut p, mut prev) = (0, None);
    let string = |dict: &Dict, c: u16| {
        let mut v = vec![];
        dict.put(c, &mut v);
        v
    };
    while p < px.len() {
        if let Some(pr) = prev
            && dict.next < CODES
        {
            let to = dict.next as u16;
            dict.add(pr, px[p]);
            strings.entry(string(&dict, to)).or_default().push(to);
        }
        let mut longest = 1;
        while p + longest < px.len() && strings.contains_key(&px[p..p + longest + 1]) {
            longest += 1;
        }
        let len = if e.literal {
            1
        } else if e.shorter > 0 && longest > 1 && rng.below(e.shorter) == 0 {
            1 + rng.below(longest as u64 - 1) as usize
        } else {
            longest
        };
        let all = &strings[&px[p..p + len]];
        let code = if e.any_code { all[rng.below(all.len() as u64) as usize] } else { all[0] };
        out.push(code);
        prev = Some(code);
        p += len;
        let full = e.clear_at > 0 && dict.next >= e.clear_at;
        // (a literal encoder clears before the codes widen)
        let narrow = e.literal && dict.next + 2 >= 1 << (min + 1);
        if full || narrow || e.random_clear > 0 && rng.below(e.random_clear) == 0 {
            out.push(clear);
            dict.clear();
            strings = roots();
            prev = None;
        }
    }
    if e.end {
        out.push(clear + 1);
    }
    out
}

/// Pixels of `colors` colors: runs, copies of the row above, noise.
fn pixels(rng: &mut Rng, n: usize, w: usize, colors: u64) -> Vec<u8> {
    let mut px = Vec::with_capacity(n);
    while px.len() < n {
        match rng.below(4) {
            0 => {
                let c = rng.below(colors) as u8;
                px.extend(std::iter::repeat_n(c, 1 + rng.below(30) as usize));
            }
            1 if px.len() >= w => {
                let from = px.len() - w;
                for i in 0..(1 + rng.below(2 * w as u64) as usize) {
                    px.push(px[from + i]);
                }
            }
            _ => px.push(rng.below(colors) as u8),
        }
    }
    px.truncate(n);
    px
}

/// A GIF file of one image of `w` x `h` whose data is `data` cut into sub-blocks of `sizes`
/// (the last one the rest).
fn file(w: u16, h: u16, min: u8, interlaced: bool, data: &[u8], sizes: &mut dyn FnMut() -> usize) -> Vec<u8> {
    let mut f = b"GIF89a".to_vec();
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&h.to_le_bytes());
    let bits = min.max(1) - 1;
    f.extend_from_slice(&[0xf0 | bits, 0, 0]);
    for i in 0..(3 << (bits + 1)) {
        f.push((i * 37) as u8);
    }
    f.extend_from_slice(b"!\xfe\x05hello\x00");
    f.push(0x2c);
    f.extend_from_slice(&[0, 0, 0, 0]);
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&h.to_le_bytes());
    f.push(if interlaced { 0x40 } else { 0 });
    f.push(min);
    let mut at = 0;
    while at < data.len() {
        let n = sizes().clamp(1, 255).min(data.len() - at);
        f.push(n as u8);
        f.extend_from_slice(&data[at..at + n]);
        at += n;
    }
    f.extend_from_slice(b"\x00;");
    f
}

#[test]
fn encoders_of_all_kinds() {
    let mut rng = Rng::new(7);
    let usual = Enc { first_clear: true, clear_at: CODES, end: true, ..Enc::default() };
    let kinds = [
        usual,
        Enc { clear_at: 0, ..usual },
        Enc { clear_at: 4093, first_clear: false, ..usual },
        Enc { end: false, ..usual },
        Enc { random_clear: 50, ..usual },
        Enc { shorter: 5, ..usual },
        Enc { shorter: 3, any_code: true, clear_at: 0, ..usual },
        Enc { literal: true, ..usual },
        Enc { literal: true, any_code: true, ..usual },
    ];
    for (k, &e) in kinds.iter().enumerate() {
        for min in [2, 3, 4, 8] {
            for case in 0..4 {
                let (w, h) = (1 + rng.below(200) as u16, 1 + rng.below(150) as u16);
                let area = w as usize * h as usize;
                // fewer pixels than the image's, or more, in some
                let n = match case {
                    1 => area / 2 + 1,
                    2 => area + 77,
                    _ => area,
                };
                let colors = (1u64 << min).min(2 + rng.below(1 << min));
                let px = pixels(&mut rng, n, w as usize, colors);
                let c = codes(&mut rng, &px, min, e);
                // padding bits, bytes after, sub-blocks of other sizes
                let pad = if case == 3 { rng.below(256) as u8 } else { 0 };
                let mut data = recons::write(&c, min).unwrap().finish(pad);
                if case == 3 {
                    let n = 1 + rng.below(9) as usize;
                    data.extend(rng.bytes(n));
                }
                let block = [255, 254, 100][rng.below(3) as usize];
                let mut sizes = || if case == 3 { 1 + rng.below(255) as usize } else { block };
                let f = file(w, h, min, case == 0, &data, &mut sizes);
                let dec = lzw::decode(&data, min, 1 << 24).unwrap();
                // (without an end code, the bytes after are codes too)
                assert!(
                    if e.end { dec.pixels == px } else { dec.pixels.starts_with(&px) },
                    "kind {k}, {min} bits, case {case}: the test encoder's"
                );
                let sp = roundtrip(&f);
                assert_eq!(sp.coded, 1, "kind {k}, {min} bits, case {case}");
            }
        }
    }
}

#[test]
fn cut_files() {
    let (_, d) = fixtures().into_iter().find(|f| f.0 == "im-photo").unwrap();
    // cut at every few bytes into the image data, and after the last sub-block
    let start = d.len() - 1000;
    for end in (start..d.len()).step_by(37).chain([d.len() - 2]) {
        let sp = roundtrip(&d[..end]);
        assert_eq!(sp.coded, 1, "cut at {end}");
    }
}

#[test]
fn not_coded() {
    assert!(recons::split(b"not a GIF file").is_err());
    let (_, d) = fixtures().into_iter().find(|f| f.0 == "im16-photo").unwrap();
    let desc = aqr_gif::gif::parse(&d).unwrap().images[0].at;
    // a header and no image
    assert!(recons::split(&[&d[..desc], b";"].concat()).is_err());
    // an image whose first code is not defined: nothing to code
    let undefined = [&d[..desc + 10], b"\x02\x01\xff\x00;"].concat();
    assert!(recons::split(&undefined).is_err());
    // a second image of a minimum code size beyond 8, kept as it is
    let wide = [&d[..d.len() - 1], &d[desc..desc + 10], b"\x09\x02\xff\xff\x00;"].concat();
    let sp = roundtrip(&wide);
    assert_eq!((sp.coded, sp.images), (1, 2));
}

#[test]
fn given_up() {
    // pixels as lossy encoders make them, strings of LZW's dictionary (here taken at random),
    // which the model's contexts do not tell
    let mut rng = Rng::new(3);
    let mut dict = Dict::new(8);
    let clear = dict.clear_code() as u16;
    let (w, h) = (1024u16, 1024u16);
    let (mut px, mut c, mut prev) = (vec![], vec![clear], None);
    while px.len() < w as usize * h as usize {
        let first = clear as usize + 2;
        let code = if dict.next > first && rng.below(8) != 0 {
            (first + rng.below((dict.next - first) as u64) as usize) as u16
        } else {
            rng.below(clear as u64) as u16
        };
        let at = px.len();
        dict.put(code, &mut px);
        if let Some(pr) = prev {
            dict.add(pr, px[at]);
        }
        c.push(code);
        prev = Some(code);
        if dict.next == CODES {
            c.push(clear);
            dict.clear();
            prev = None;
        }
    }
    c.push(clear + 1);
    let data = recons::write(&c, 8).unwrap().finish(0);
    let f = file(w, h, 8, false, &data, &mut || 255);
    assert_eq!(recons::split(&f).err().map(|e| e.0), Some("the pixels no smaller than their LZW data"));
}

#[test]
fn damaged_records() {
    for (name, d) in fixtures() {
        let sp = recons::split(&d).unwrap();
        for k in 0..16 {
            let mut r = sp.record.clone();
            let at = r.len() * k / 16;
            if k == 15 {
                r.truncate(at);
            } else {
                r[at] ^= 1 << (k % 8);
            }
            // an error or other bytes, never a panic
            let mut out = Vec::new();
            let _ = recons::join_onto(&sp.skeleton, &r, &mut out);
        }
        let mut out = Vec::new();
        assert!(recons::join_onto(&sp.skeleton[..sp.skeleton.len() / 2], &sp.record, &mut out).is_err(), "{name}");
    }
}
