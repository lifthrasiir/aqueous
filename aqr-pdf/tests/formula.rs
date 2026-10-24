//! Round trips of synthetic PDF files through entry formulas: `formula::encode`, then
//! `Formula::eval` with the module and the children's (aqr-flate, aqr-png, aqr-jpeg)
//! decoders. The DEFLATE streams come from flate2 (zlib-rs), as flate-zoo's sources are not
//! always at hand.

use aqr_formula::{Apply, Formula, Node, packed};
use aqr_pdf::encode::Options;
use aqr_pdf::formula::{self, MODULE};
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

/// Thread counts checked against one thread.
const THREADS: [usize; 3] = [2, 3, 8];

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

/// DEFLATE data of zlib at `level`, without the wrapper.
fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut z = flate2::write::DeflateEncoder::new(vec![], Compression::new(level));
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn zlib_level(data: &[u8], level: u32) -> Vec<u8> {
    let mut z = ZlibEncoder::new(vec![], Compression::new(level));
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    zlib_level(data, 6)
}

/// A PDF of the given objects (dictionary body, optional stream data), with an xref table.
fn pdf(objs: &[(String, Option<Vec<u8>>)]) -> Vec<u8> {
    let mut f = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offs = vec![];
    for (i, (d, s)) in objs.iter().enumerate() {
        offs.push(f.len());
        f.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        match s {
            Some(s) => {
                f.extend_from_slice(format!("<< {d} /Length {} >>\nstream\n", s.len()).as_bytes());
                f.extend_from_slice(s);
                f.extend_from_slice(b"\nendstream\n");
            }
            None => f.extend_from_slice(format!("{d}\n").as_bytes()),
        }
        f.extend_from_slice(b"endobj\n");
    }
    let x = f.len();
    f.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        f.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    f.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    f
}

/// Page content: text operators and numbers, somewhat repetitive.
fn content(seed: u32, n: usize) -> Vec<u8> {
    let mut s = String::from("BT /F1 12 Tf\n");
    let mut x = seed;
    for i in 0..n {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        s += &format!(
            "{} {} Td ({}) Tj\n",
            (x >> 16) % 600,
            i % 800,
            ["Hello", "PDF", "world", "recompression"][(x >> 8) as usize % 4]
        );
        if i % 7 == 0 {
            s += &format!("{}.{} {} {} re f\n", x % 97, x % 10, x % 300, (x >> 3) % 200);
        }
    }
    s += "ET\n";
    s.into_bytes()
}

/// Pixel rows (`bpp` bytes per pixel), smooth with noise; with PNG filter bytes if `png`.
fn pixels(w: usize, h: usize, bpp: usize, png: bool, seed: u32) -> Vec<u8> {
    let mut out = vec![];
    let mut x = seed;
    let mut prev = vec![0u8; w * bpp];
    for y in 0..h {
        let mut row = vec![0u8; w * bpp];
        for (i, p) in row.iter_mut().enumerate() {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            *p = ((i / bpp * 3 + y * 2 + (i % bpp) * 40) as u32 + (x >> 28)) as u8;
        }
        if png {
            // Up filter on odd rows, none on even ones.
            let f = (y % 2) as u8 * 2;
            out.push(f);
            for i in 0..row.len() {
                out.push(if f == 2 { row[i].wrapping_sub(prev[i]) } else { row[i] });
            }
        } else {
            out.extend_from_slice(&row);
        }
        prev = row;
    }
    out
}

fn jpeg() -> Vec<u8> {
    std::fs::read("tests/data/gradient.jpg").unwrap()
}

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, i: &[&[u8]]| formula::apply(a, i, threads))
}

fn options() -> Vec<(&'static str, Options)> {
    let base = Options::default();
    vec![
        ("default", base.clone()),
        ("all", Options { speed: f64::INFINITY, ..base.clone() }),
        ("tok", Options { speed: f64::INFINITY, full: false, png: false, ..base.clone() }),
        ("full", Options { speed: f64::INFINITY, tok: false, png: false, ..base.clone() }),
        ("raw", Options { tok: false, full: false, png: false, ..base.clone() }),
        ("verify", Options { speed: f64::INFINITY, verify: true, ..base }),
    ]
}

/// Round trips of `file` with every set of options and thread counts; how many applications
/// of each module (this one, aqr-flate, aqr-png, aqr-jpeg) the formulas had in all.
fn check(name: &str, file: &[u8]) -> [usize; 4] {
    let mut count = [0; 4];
    for (oname, o) in options() {
        let Some(f) = formula::encode_with(file, &o) else { continue };
        assert_eq!(f.len(), file.len() as u64, "{name} ({oname})");
        let back = eval(&f, 1).unwrap_or_else(|e| panic!("{name} ({oname}): {e}"));
        assert!(back == file, "{name} ({oname}): round trip differs");
        for t in THREADS {
            let ft = formula::encode_with(file, &Options { threads: t, ..o.clone() });
            assert!(ft.as_ref() == Some(&f), "{name} ({oname}): {t} threads give another formula");
            let back = eval(&f, t).unwrap_or_else(|e| panic!("{name} ({oname}, {t} threads): {e}"));
            assert!(back == file, "{name} ({oname}, {t} threads): round trip differs");
        }
        let modules = [MODULE, aqr_flate::formula::MODULE, aqr_png::formula::MODULE, aqr_jpeg::formula::MODULE];
        for a in f.applies() {
            count[modules.iter().position(|m| *m == a.module).unwrap()] += 1;
        }
    }
    count
}

fn image(w: usize, h: usize, cs: &str, pred: Option<(u32, usize)>) -> String {
    let parms = match pred {
        Some((p, colors)) => format!(" /DecodeParms << /Predictor {p} /Colors {colors} /Columns {w} >>"),
        None => String::new(),
    };
    format!(
        "/Type /XObject /Subtype /Image /Width {w} /Height {h} /BitsPerComponent 8 /ColorSpace /{cs} /Filter /FlateDecode{parms}"
    )
}

#[test]
fn modes_are_used() {
    let g = (64usize, 48usize);
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(2, 4000)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(2, 4000)))),
        (image(g.0, g.1, "DeviceRGB", Some((15, 3))), Some(zlib(&pixels(g.0, g.1, 3, true, 3)))),
        (image(g.0, g.1, "DeviceGray", None), Some(zlib(&pixels(g.0, g.1, 1, false, 4)))),
        (
            "/Type /XObject /Subtype /Image /Width 160 /Height 120 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode".into(),
            Some(jpeg()),
        ),
    ]);
    // This module, aqr-flate (full), aqr-png and aqr-jpeg all apply.
    let n = check("modes", &f);
    assert!(n.iter().all(|&c| c > 0), "a module is unused: {n:?}");
    let o = Options { speed: f64::INFINITY, ..Options::default() };
    let fm = formula::encode_with(&f, &o).unwrap();
    let Node::Apply(root) = &fm.root else { panic!("root is {:?}", fm.root) };
    assert_eq!(root.module, MODULE);
    assert_eq!(root.inputs.len(), 11);
    assert!(root.params.is_empty());
}

#[test]
fn levels_and_wrappers() {
    let data = content(5, 2000);
    let z = zlib(&data);
    let mut bad_adler = z.clone();
    *bad_adler.last_mut().unwrap() ^= 1;
    let mut junk = z.clone();
    junk.extend_from_slice(b"\r\n\0junk");
    let truncated = z[..z.len() / 2].to_vec();
    let mut garbage = z.clone();
    garbage[40] ^= 0x55;
    let mut objs = vec![
        ("/Type /Catalog".to_string(), None),
        ("/Filter /FlateDecode".into(), Some(deflate(&data, 6))),
        ("/Filter /FlateDecode".into(), Some(bad_adler)),
        ("/Filter /FlateDecode".into(), Some(junk)),
        ("/Filter /FlateDecode".into(), Some(truncated)),
        ("/Filter /FlateDecode".into(), Some(garbage)),
        ("/Filter [/FlateDecode]".into(), Some(z.clone())),
        ("/Filter /Fl".into(), Some(z.clone())),
        ("/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >>".into(), Some(z)),
        ("/Filter /FlateDecode".into(), Some(vec![])),
        (
            "/Filter /DCTDecode".into(),
            Some(b"not a jpeg at all, but long enough to be an item of its own........".to_vec()),
        ),
    ];
    for level in [0, 1, 3, 9] {
        objs.push(("/Filter /FlateDecode".into(), Some(zlib_level(&data, level))));
    }
    check("wrappers", &pdf(&objs));
}

#[test]
fn syntax() {
    let z = zlib(&content(6, 1500));
    let mut f = b"%PDF-1.4\n".to_vec();
    // An indirect length, CR line ends, a dictionary with strings holding keywords.
    f.extend_from_slice(b"1 0 obj\r<< /Length 2 0 R /Filter /FlateDecode /T (obj stream) >>\rstream\r\n");
    f.extend_from_slice(&z);
    f.extend_from_slice(b"\rendstream\rendobj\r2 0 obj\r");
    f.extend_from_slice(z.len().to_string().as_bytes());
    f.extend_from_slice(b"\rendobj\r");
    // A wrong direct length, and a stream keyword in a string.
    f.extend_from_slice(b"3 0 obj << /Length 7 /Filter /FlateDecode >> stream\n");
    f.extend_from_slice(&z);
    f.extend_from_slice(b"\nendstream endobj 4 0 obj (stream\n) endobj\n");
    // A stream with no end.
    f.extend_from_slice(b"5 0 obj << /Filter /FlateDecode >>\nstream\n");
    f.extend_from_slice(&z[..z.len() - 3]);
    check("syntax", &f);
}

#[test]
fn encrypted() {
    let mut f = pdf(&[("/Type /Catalog".into(), None), ("/Filter /FlateDecode".into(), Some(zlib(&content(7, 1000))))]);
    f.extend_from_slice(b"trailer << /Encrypt 9 0 R >>\n");
    // Nothing to code but repeats: no formula.
    assert!(formula::encode(&f, 1).is_none());
}

/// Many items of every mode, more than the threads keep in flight.
#[test]
fn many_items() {
    let jpeg = jpeg();
    let mut objs = vec![("/Type /Catalog".to_string(), None)];
    for i in 0..60u32 {
        let (d, s) = match i % 6 {
            0 | 1 => ("/Filter /FlateDecode".into(), zlib(&content(100 + i, 200 + 37 * i as usize))),
            2 => {
                let (w, h) = (24 + i as usize, 16 + i as usize % 7);
                (image(w, h, "DeviceRGB", Some((15, 3))), zlib(&pixels(w, h, 3, true, i)))
            }
            3 => (
                "/Subtype /Image /Width 160 /Height 120 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode"
                    .into(),
                jpeg.clone(),
            ),
            4 => ("/Filter /FlateDecode".into(), zlib(&content(100 + i - 4, 200 + 37 * (i - 4) as usize))),
            _ => ("/Filter /LZWDecode".into(), content(200 + i, 50)),
        };
        objs.push((d, Some(s)));
    }
    let n = check("many items", &pdf(&objs));
    assert!(n.iter().all(|&c| c > 0), "a module is unused: {n:?}");
}

/// More DEFLATE blocks than inflated bytes (empty stored blocks, as from flushes).
#[test]
fn empty_blocks() {
    let mut z = vec![0x78, 0x01];
    for _ in 0..20 {
        z.extend_from_slice(&[0x00, 0x00, 0x00, 0xff, 0xff]);
    }
    // A final fixed block holding `a`, then the Adler-32.
    z.extend_from_slice(&[0x4b, 0x04, 0x00]);
    z.extend_from_slice(&adler32(b"a").to_be_bytes());
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(z)),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(14, 3000)))),
    ]);
    check("empty blocks", &f);
}

/// Items that a child does not code stay in the module.
#[test]
fn children_fail() {
    let (w, h) = (40usize, 30usize);
    let noise: Vec<u8> = (0..w * h * 3 + h).map(|i| (i * 7919 % 251) as u8).collect();
    let j = jpeg();
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        (image(w, h, "DeviceRGB", Some((15, 3))), Some(zlib(&noise))),
        ("/Filter /DCTDecode".into(), Some(j[..j.len() / 2].to_vec())),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(9, 3000)))),
    ]);
    check("children fail", &f);
}

/// Inflated data in the pools is larger than the file, but compresses well: the formula is
/// made where the streams, zstd-compressed, are smaller than the file.
#[test]
fn inflated_pools() {
    let f = pdf(&[("/Type /Catalog".into(), None), ("/Filter /FlateDecode".into(), Some(zlib(&content(3, 4000))))]);
    let o = Options { speed: f64::INFINITY, tok: false, png: false, ..Options::default() };
    let fm = formula::encode_with(&f, &o).expect("coded");
    assert!(fm.streams.iter().map(|s| s.data.len()).sum::<usize>() > f.len());
    check("inflated pools", &f);
}

#[test]
fn not_coded() {
    assert!(formula::encode(b"", 1).is_none());
    assert!(formula::encode(b"not a PDF at all", 1).is_none());
    assert!(formula::encode(&content(1, 500), 1).is_none());
    let f = pdf(&[("/Type /Catalog".into(), None)]);
    assert!(formula::encode(&f, 1).is_none());
}

#[test]
fn cut_files() {
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(3, 2500)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(4, 2500)))),
    ]);
    for cut in (0..f.len()).step_by(61).chain([f.len() - 1]) {
        check("cut", &f[..cut]);
    }
}

/// Corrupted slots make errors or other output, never panics.
#[test]
fn corrupted_slots() {
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(3, 2500)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&content(4, 2500)))),
        (image(32, 32, "DeviceRGB", Some((15, 3))), Some(zlib(&pixels(32, 32, 3, true, 5)))),
        ("/Filter /DCTDecode".into(), Some(jpeg())),
    ]);
    let o = Options { speed: f64::INFINITY, ..Options::default() };
    let fm = formula::encode_with(&f, &o).unwrap();
    let Node::Apply(root) = &fm.root else { panic!() };
    let inputs: Vec<Vec<u8>> = root
        .inputs
        .iter()
        .map(|n| eval(&Formula { streams: fm.streams.clone(), root: n.clone() }, 1).unwrap())
        .collect();
    let run = |ins: &[Vec<u8>], len: u64| {
        let r: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
        formula::decode(&[], &r, len, 1)
    };
    assert_eq!(run(&inputs, root.len).unwrap(), f);
    assert!(run(&inputs, root.len - 1).is_err());
    assert!(run(&inputs, root.len + 1).is_err());
    assert!(run(&inputs[1..], root.len).is_err());
    let r: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
    assert!(formula::decode(&[0], &r, root.len, 1).is_err());
    // The table, skeleton, raw, DEFLATE, PNG, JPEG slots and the token symbols are used up
    // exactly: a byte more or less is an error. The other token parts are only read as far
    // as needed.
    for k in (0..6).chain([8]) {
        let mut more = inputs.clone();
        more[k].push(0);
        assert!(run(&more, root.len).is_err(), "slot {k}: a byte more");
        let mut less = inputs.clone();
        if less[k].pop().is_some() {
            assert!(run(&less, root.len).is_err(), "slot {k}: a byte less");
        }
    }
    // The DEFLATE, PNG and JPEG slots are packed, an item for every item of the table taking one.
    for k in 3..6 {
        let items = packed::unpack(&inputs[k]).unwrap();
        assert!(!items.is_empty(), "slot {k}");
        let mut more = inputs.clone();
        more[k] = packed::pack(&[&items[..], &[b""]].concat());
        assert!(run(&more, root.len).is_err(), "slot {k}: an item more");
        let mut less = inputs.clone();
        less[k] = packed::pack(&items[1..]);
        assert!(run(&less, root.len).is_err(), "slot {k}: an item less");
        let mut plain = inputs.clone();
        plain[k] = items.concat();
        assert!(run(&plain, root.len).is_err(), "slot {k}: not packed");
    }
    for k in 0..inputs.len() {
        for at in (0..inputs[k].len()).step_by(1 + inputs[k].len() / 40) {
            let mut flip = inputs.clone();
            flip[k][at] ^= 0x5a;
            let _ = run(&flip, root.len);
        }
    }
}
