//! Round trips of synthetic PDF files: FlateDecode streams from every DEFLATE encoder of
//! flate-zoo through every mode, zlib wrapper variants, images with and without PNG
//! predictors, JPEG, duplicates, encrypted and malformed files, and corrupted containers.

use flate_zoo::{Encoder, backends};
use pdf_recons::container::{Options, Stats, compress_with, decompress, decompress_threads};

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

fn zlib(enc: &Encoder, data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x9c];
    z.extend_from_slice(&enc.compress(data).unwrap());
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

fn default_enc() -> Encoder {
    Encoder::new(flate_zoo::backend("zlib").unwrap_or(backends()[0]))
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

fn check(name: &str, file: &[u8]) {
    for (oname, o) in options() {
        let mut st = Stats::default();
        let c = compress_with(file, &o, &mut st);
        let back = decompress(&c).unwrap_or_else(|e| panic!("{name} ({oname}): {e}"));
        assert!(back == file, "{name} ({oname}): round trip differs");
        assert!(!st.verify_failed, "{name} ({oname}): verification failed");
        // The same bytes with threads, and the same file back.
        for t in THREADS {
            let ct = compress_with(file, &Options { threads: t, ..o.clone() }, &mut Stats::default());
            assert!(ct == c, "{name} ({oname}): {t} threads give other bytes");
            let back = decompress_threads(&c, t).unwrap_or_else(|e| panic!("{name} ({oname}, {t} threads): {e}"));
            assert!(back == file, "{name} ({oname}, {t} threads): round trip differs");
        }
    }
}

/// The coded form with everything allowed, and what it did.
fn coded(file: &[u8]) -> (Vec<u8>, Stats) {
    let mut st = Stats::default();
    let o = Options { speed: f64::INFINITY, ..Options::default() };
    let c = compress_with(file, &o, &mut st);
    assert_eq!(decompress(&c).unwrap(), file);
    (c, st)
}

#[test]
fn every_encoder() {
    let data = content(1, 3000);
    for b in backends() {
        let enc = Encoder::new(*b);
        let Ok(raw) = enc.compress(&data) else { continue };
        let mut z = vec![0x78, 0xda];
        z.extend_from_slice(&raw);
        z.extend_from_slice(&adler32(&data).to_be_bytes());
        let f = pdf(&[
            ("/Type /Catalog".into(), None),
            ("/Filter /FlateDecode".into(), Some(z)),
            ("/Filter /FlateDecode /Length1 100".into(), Some(raw.clone())),
        ]);
        check(b.name(), &f);
    }
}

#[test]
fn modes_are_used() {
    let enc = default_enc();
    let g = (64usize, 48usize);
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(2, 4000)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(2, 4000)))),
        (
            format!("/Type /XObject /Subtype /Image /Width {} /Height {} /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 3 /Columns {} >>", g.0, g.1, g.0),
            Some(zlib(&enc, &pixels(g.0, g.1, 3, true, 3))),
        ),
        (
            format!("/Type /XObject /Subtype /Image /Width {} /Height {} /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode", g.0, g.1),
            Some(zlib(&enc, &pixels(g.0, g.1, 1, false, 4))),
        ),
        (
            "/Type /XObject /Subtype /Image /Width 160 /Height 120 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode".into(),
            Some(std::fs::read("tests/data/gradient.jpg").unwrap()),
        ),
    ]);
    check("modes", &f);
    let (_, st) = coded(&f);
    use pdf_recons::container::{DUP, JPEG, PNG};
    for m in [DUP, PNG, JPEG] {
        assert!(st.modes[m as usize].0 > 0, "mode {} unused: {:?}", pdf_recons::container::MODES[m as usize], st.modes);
    }
}

#[test]
fn wrappers() {
    let enc = default_enc();
    let data = content(5, 2000);
    let raw = enc.compress(&data).unwrap();
    let z = zlib(&enc, &data);
    let mut bad_adler = z.clone();
    *bad_adler.last_mut().unwrap() ^= 1;
    let mut junk = z.clone();
    junk.extend_from_slice(b"\r\n\0junk");
    let truncated = z[..z.len() / 2].to_vec();
    let mut garbage = z.clone();
    garbage[40] ^= 0x55;
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(raw)),
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
    ]);
    check("wrappers", &f);
}

#[test]
fn syntax() {
    let enc = default_enc();
    let z = zlib(&enc, &content(6, 1500));
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
    // An incremental update redefining object 3, and a dictionary nested deeply.
    f.extend_from_slice(b"3 0 obj << /A << /B << /C [1 2 [3 << /D 4 >>]] >> >> /Filter /FlateDecode >>\nstream\n");
    f.extend_from_slice(&z);
    f.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
    // A stream with no end.
    f.extend_from_slice(b"5 0 obj << /Filter /FlateDecode >>\nstream\n");
    f.extend_from_slice(&z[..z.len() - 3]);
    check("syntax", &f);
}

#[test]
fn encrypted() {
    let enc = default_enc();
    let mut f =
        pdf(&[("/Type /Catalog".into(), None), ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(7, 1000))))]);
    f.extend_from_slice(b"trailer << /Encrypt 9 0 R >>\n");
    check("encrypted", &f);
    let (_, st) = coded(&f);
    assert_eq!(st.modes[pdf_recons::container::TOK as usize].0 + st.modes[pdf_recons::container::FULL as usize].0, 0);
}

/// A PDF 2.0 file (with junk before the header, as allowed): a UTF-8 text string, an image,
/// an object stream and a cross-reference stream in place of the xref table; with `encrypt`,
/// an AES-256 (revision 6) `/Encrypt` dictionary.
fn pdf20(encrypt: bool) -> Vec<u8> {
    fn obj(f: &mut Vec<u8>, num: usize, d: &[u8], s: Option<&[u8]>) -> usize {
        let o = f.len();
        f.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        match s {
            Some(s) => {
                f.extend_from_slice(b"<< ");
                f.extend_from_slice(d);
                f.extend_from_slice(format!(" /Length {} >>\nstream\n", s.len()).as_bytes());
                f.extend_from_slice(s);
                f.extend_from_slice(b"\nendstream");
            }
            None => f.extend_from_slice(d),
        }
        f.extend_from_slice(b"\nendobj\n");
        o
    }
    let enc = default_enc();
    let mut f = b"junk before the header\n%PDF-2.0\n%\xe2\xe3\xcf\xd3\n".to_vec();
    // Cross-reference entries of W [1 4 2] by object number: type, offset or object stream,
    // generation or index.
    let mut x = vec![];
    let mut entry = |t: u8, a: usize, b: u16| {
        x.push(t);
        x.extend_from_slice(&(a as u32).to_be_bytes());
        x.extend_from_slice(&b.to_be_bytes());
    };
    entry(0, 0, 0xffff);
    let mut cat = b"<< /Type /Catalog /Pages 4 0 R /Lang (\xef\xbb\xbf".to_vec();
    cat.extend_from_slice("한국어 (\u{2192}) \\(".as_bytes());
    cat.extend_from_slice(b") >>");
    entry(1, obj(&mut f, 1, &cat, None), 0);
    entry(1, obj(&mut f, 2, b"/Filter /FlateDecode", Some(&zlib(&enc, &content(17, 3000)))), 0);
    let (pages, page) = (
        "<< /Type /Pages /Kids [5 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 4 0 R /Contents 2 0 R /Resources << /XObject << /Im1 6 0 R >> >> >>",
    );
    let head = format!("4 0 5 {} ", pages.len() + 1);
    let d = format!("/Type /ObjStm /N 2 /First {} /Filter /FlateDecode", head.len());
    entry(1, obj(&mut f, 3, d.as_bytes(), Some(&zlib(&enc, format!("{head}{pages} {page}").as_bytes()))), 0);
    entry(2, 3, 0);
    entry(2, 3, 1);
    let d = b"/Type /XObject /Subtype /Image /Width 96 /Height 64 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode";
    entry(1, obj(&mut f, 6, d, Some(&zlib(&enc, &pixels(96, 64, 1, false, 18)))), 0);
    let mut e = "";
    if encrypt {
        let d = b"<< /Filter /Standard /V 5 /R 6 /Length 256 /CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF /P -1028 /O <00> /U <00> /OE <00> /UE <00> /Perms <00> >>";
        entry(1, obj(&mut f, 7, d, None), 0);
        e = " /Encrypt 7 0 R /ID [<00> <00>]";
    }
    let (num, start) = (if encrypt { 8 } else { 7 }, f.len());
    entry(1, start, 0);
    let d = format!("/Type /XRef /Size {} /W [1 4 2] /Root 1 0 R{e} /Filter /FlateDecode", num + 1);
    obj(&mut f, num, d.as_bytes(), Some(&zlib(&enc, &x)));
    f.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
    f
}

#[test]
fn pdf_2_0() {
    use pdf_recons::container::{FULL, TOK};
    let f = pdf20(false);
    check("PDF 2.0", &f);
    let (_, st) = coded(&f);
    assert!(st.modes[TOK as usize].0 + st.modes[FULL as usize].0 >= 2, "{:?}", st.modes);
    let f = pdf20(true);
    check("PDF 2.0, AES-256", &f);
    let (_, st) = coded(&f);
    assert_eq!(st.modes[TOK as usize].0 + st.modes[FULL as usize].0, 0);
}

#[test]
fn not_pdf() {
    for f in [
        vec![],
        b"%PDF-".to_vec(),
        b"hello".repeat(1000),
        (0..100_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect(),
    ] {
        check("not a pdf", &f);
    }
}

#[test]
fn cut_files() {
    let enc = default_enc();
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(8, 3000)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(9, 3000)))),
    ]);
    for cut in [f.len() / 3, f.len() / 2, f.len() - 30] {
        check("cut", &f[..cut]);
    }
}

#[test]
fn corrupted_containers() {
    let enc = default_enc();
    let g = (32usize, 32usize);
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(10, 2000)))),
        ("/Filter /FlateDecode".into(), Some(zlib(&enc, &content(10, 2000)))),
        (
            format!(
                "/Subtype /Image /Width {} /Height {} /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode /DecodeParms << /Predictor 15 /Columns {} >>",
                g.0, g.1, g.0
            ),
            Some(zlib(&enc, &pixels(g.0, g.1, 1, true, 11))),
        ),
        ("/Subtype /Image /Filter /DCTDecode".into(), Some(std::fs::read("tests/data/gradient.jpg").unwrap())),
    ]);
    for (_, o) in options() {
        let c = compress_with(&f, &o, &mut Stats::default());
        let mut x = 99u32;
        for i in 0..400 {
            let mut m = c.clone();
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            match i % 4 {
                0 => {
                    let k = x as usize % m.len();
                    m[k] ^= 1 << (x >> 29);
                }
                1 => m.truncate(x as usize % m.len()),
                2 => {
                    let k = x as usize % m.len();
                    m.insert(k, (x >> 8) as u8);
                }
                _ => {
                    let k = x as usize % m.len();
                    m[k] = (x >> 16) as u8;
                }
            }
            let d = decompress(&m);
            if let Ok(d) = &d {
                assert_eq!(*d, f, "a corrupt container decoded to something else");
            }
            let t = THREADS[i % THREADS.len()];
            assert_eq!(decompress_threads(&m, t), d, "{t} threads: another outcome");
        }
    }
}

#[test]
fn pixel_geometry_mismatch() {
    // Rows longer than the data (a second filter after FlateDecode, or a wrong width), and
    // data longer than the image.
    let enc = default_enc();
    let small = pixels(16, 4, 1, false, 12);
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Subtype /Image /Width 4000 /Height 3000 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter [/FlateDecode /DCTDecode]".into(), Some(zlib(&enc, &small))),
        ("/Subtype /Image /Width 9000 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode".into(), Some(zlib(&enc, &small))),
        ("/Subtype /Image /Width 3 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /Filter /FlateDecode".into(), Some(zlib(&enc, &pixels(64, 64, 4, false, 13)))),
    ]);
    check("pixel geometry", &f);
}

/// The planner's estimates: inflated sizes from the first block's codes, JPEG blocks from the
/// frame header.
#[test]
fn estimates() {
    // Close for zlib's codes; other encoders' need not fit their data (fdeflate's are
    // fixed), but must parse or be refused.
    let data = content(1, 20_000);
    assert!(flate_zoo::backend("zlib").is_some());
    for b in backends() {
        let Ok(z) = Encoder::new(*b).compress(&data) else { continue };
        let e = pdf_recons::flate::expansion(&z);
        if b.name() == "zlib" {
            let (e, actual) = (e.unwrap(), data.len() as f64 / z.len() as f64);
            assert!((e / actual).log2().abs() < 0.5, "estimate {e:.2}, actual {actual:.2}");
        }
    }
    // Cut and damaged headers.
    let z = zlib(&default_enc(), &data);
    for k in 0..400.min(z.len()) {
        let _ = pdf_recons::flate::expansion(&z[..k]);
        let mut m = z[..400].to_vec();
        m[k] ^= 1 << (k % 8);
        let _ = pdf_recons::flate::expansion(&m);
    }
    assert_eq!(pdf_recons::flate::expansion(&[0x78, 0x9c, 0x03, 0x00]), None);
    assert_eq!(pdf_recons::flate::expansion(&[]), None);
    let jpeg = std::fs::read("tests/data/gradient.jpg").unwrap();
    // 160×120 in 4:2:0: 20×15 luma blocks, 10×8 of each chroma (whole MCUs).
    assert_eq!(pdf_recons::image::jpeg_frame(&jpeg), Some((false, 300 + 2 * 80)));
}

/// Many items of every mode, more than the threads keep in flight, so that their results
/// come back out of order.
#[test]
fn many_items() {
    let enc = default_enc();
    let jpeg = std::fs::read("tests/data/gradient.jpg").unwrap();
    let mut objs = vec![("/Type /Catalog".to_string(), None)];
    for i in 0..60u32 {
        let (d, s) = match i % 6 {
            0 | 1 => ("/Filter /FlateDecode".into(), zlib(&enc, &content(100 + i, 200 + 37 * i as usize))),
            2 => {
                let (w, h) = (24 + i as usize, 16 + i as usize % 7);
                (
                    format!(
                        "/Subtype /Image /Width {w} /Height {h} /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 3 /Columns {w} >>"
                    ),
                    zlib(&enc, &pixels(w, h, 3, true, i)),
                )
            }
            3 => (
                "/Subtype /Image /Width 160 /Height 120 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode"
                    .into(),
                jpeg.clone(),
            ),
            4 => ("/Filter /FlateDecode".into(), zlib(&enc, &content(100 + i - 4, 200 + 37 * (i - 4) as usize))),
            _ => ("/Filter /LZWDecode".into(), content(200 + i, 50)),
        };
        objs.push((d, Some(s)));
    }
    let f = pdf(&objs);
    check("many items", &f);
    let (_, st) = coded(&f);
    assert!(st.modes.iter().filter(|m| m.0 > 0).count() >= 5, "{:?}", st.modes);
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
    // With a stream that gains, so that the file is coded.
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /FlateDecode".into(), Some(z)),
        ("/Filter /FlateDecode".into(), Some(zlib(&default_enc(), &content(14, 3000)))),
    ]);
    check("empty blocks", &f);
}

/// The streams of the items' data are used up exactly.
#[test]
fn streams_used_up() {
    let f = pdf(&[
        ("/Type /Catalog".into(), None),
        ("/Filter /LZWDecode".into(), Some(content(15, 200))),
        ("/Filter /FlateDecode".into(), Some(zlib(&default_enc(), &content(16, 20000)))),
    ]);
    let (c, _) = coded(&f);
    assert_eq!(c[6..8], [0, 0], "coded");
    let mut r = pdf_recons::varint::Reader::new(&c[8..]);
    let mut streams = vec![];
    while !r.at_end() {
        let n = r.usize(c.len()).unwrap();
        streams.push(r.bytes(n).unwrap().to_vec());
    }
    assert_eq!(streams.len(), 18);
    // A byte more in the raw stream (stored).
    assert!(!streams[3].is_empty());
    streams[3].push(0);
    let mut m = c[..8].to_vec();
    for s in &streams {
        pdf_recons::varint::put(&mut m, s.len() as u64);
        m.extend_from_slice(s);
    }
    assert!(decompress(&m).is_err(), "a byte left in the raw stream");
}
