use std::io::Write;

use recons_common::rng::noise;
use recons_dispatch::{Content, Formula, Native, Nest, Node, Options, compress, decompress, native, recompress};
use recons_formula::aqr;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Module names of `f`'s applications, parents first.
fn modules(f: &Formula) -> Vec<&'static str> {
    f.applies().iter().map(|a| a.module.name).collect()
}

/// Recompresses `d`, checking that the formula evaluates back to it, also through the
/// archive.
fn roundtrip(d: &[u8], opt: &Options) -> Formula {
    let f = recompress(d, opt);
    assert_eq!(f.len(), d.len() as u64);
    assert!(f.eval(&Native::default()).unwrap() == d, "formula does not evaluate to the input:\n{f}");
    let c = aqr::encode(&f, Some(aqr::checksum(d)), aqr::Level::Mid).unwrap();
    assert!(decompress(&c, 1).unwrap() == d, "archive does not decompress to the input:\n{f}");
    f
}

/// Options keeping every formula, however large it archives: for what is recompressed.
fn all() -> Options {
    Options { smaller: false, ..Options::default() }
}

fn text(n: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", i * 7 % 13).unwrap();
    }
    t
}

fn gzip(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename("a.txt").write(Vec::new(), flate2::Compression::new(6));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

fn zlib(d: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

#[test]
fn archive_files() {
    let opt = Options::default();
    let mut d = gzip(&text(10_000));
    let c = compress(&d, &opt);
    assert_eq!(c[..6], aqr::HEADER_MAGIC);
    assert_eq!(decompress(&c, 1).unwrap(), d);
    d.clear();
    assert_eq!(decompress(&compress(&d, &opt), 1).unwrap(), d);
    let (d, _) = pe();
    assert_eq!(decompress(&compress(&d, &opt), 1).unwrap(), d);
    // The output checksum is verified.
    let c = aqr::encode(&recompress(&d, &opt), Some(aqr::checksum(&d) ^ 1), aqr::Level::Mid).unwrap();
    assert_eq!(decompress(&c, 1), Err(aqr::Failure::Invalid("output checksum mismatch".into())));
}

#[test]
fn raw_fallback() {
    let opt = Options::default();
    for d in [&b""[..], b"hello", b"\x78\x9cnot zlib at all", b"\x1f\x8b\x08\0garbage"] {
        let f = roundtrip(d, &opt);
        assert_eq!(f.root, Node::slice(0, 0, d.len() as u64));
    }
}

/// A file whose formula would archive larger than the file itself is kept raw, alone or
/// found in data.
#[test]
fn archive_size_fallback() {
    let opt = Options::default();
    let d = gzip(&noise(200_000));
    assert!(modules(&roundtrip(&d, &opt)).is_empty());
    assert!(compress(&d, &opt).len() < d.len() + 100);
    let d = [text(10_000), gzip(&noise(200_000)), text(10_000)].concat();
    assert!(modules(&roundtrip(&d, &opt)).is_empty());
    // what does shrink is still recompressed
    let d = [gzip(&noise(200_000)), gzip(&text(100_000))].concat();
    assert_eq!(modules(&roundtrip(&d, &opt)), [native::FLATE.name]);
    // Files found that each archive smaller, but not all together.
    let d = fixture("jpeg-recons/tests/data/gradient.jpg").repeat(100);
    assert!(modules(&roundtrip(&d, &opt)).is_empty());
    assert_eq!(roundtrip(&d, &all()).applies().len(), 100);
}

#[test]
fn recons_fixtures() {
    let opt = all();
    for (path, module) in [
        ("flac-recons/tests/data/s16_f5.flac", native::FLAC.module.name),
        ("mp3-recons/tests/data/cbr128.mp3", native::MP3.module.name),
        ("mp3-recons/tests/data/tags.mp3", native::MP3.module.name),
        ("jpeg-recons/tests/data/prog_script_recode.jpg", native::JPEG.module.name),
        ("pdf-recons/tests/data/gradient.jpg", native::JPEG.module.name),
    ] {
        let f = roundtrip(&fixture(path), &opt);
        assert_eq!(modules(&f), [module], "{path}");
    }
}

#[test]
fn gzip_nested() {
    let flac = fixture("flac-recons/tests/data/s16_f5.flac");
    let f = roundtrip(&gzip(&flac), &all());
    assert_eq!(modules(&f), [native::FLATE.name, native::FLAC.module.name]);

    // Trailing bytes after the member are kept.
    let mut d = gzip(&text(100_000));
    d.extend_from_slice(b"trailing");
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [native::FLATE.name]);

    // Only the contents named are recompressed.
    let none = Options { contents: vec![], ..all() };
    assert_eq!(modules(&roundtrip(&gzip(&flac), &none)), [native::FLATE.name]);

    let d = gzip(&zlib(&text(100_000), 9));
    assert_eq!(modules(&roundtrip(&d, &all())), [native::FLATE.name; 2]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 1, ..all() })), [native::FLATE.name]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 0, ..all() })), [""; 0]);
}

#[test]
fn zlib_checksum() {
    let t = text(50_000);
    for level in [1, 6, 9] {
        let f = roundtrip(&zlib(&t, level), &Options::default());
        assert_eq!(modules(&f), [native::FLATE.name]);
        // The data is a raw stream of its own, between the header and the checksum.
        assert!(f.streams.iter().any(|s| s.data == t));
    }
    let mut d = zlib(&t, 6);
    *d.last_mut().unwrap() ^= 1;
    assert!(modules(&roundtrip(&d, &Options::default())).is_empty());
}

/// A PE32+ image with one executable section of x86-64 code, followed by other data.
fn pe() -> (Vec<u8>, std::ops::Range<usize>) {
    let mut code = Vec::new();
    for i in 0..2000u32 {
        let at = code.len() as i32;
        // mov [rsp+8], rbx; call f(i % 5); mov rax, [rip+d]; add rsp, 0x28; ret; int3
        code.extend_from_slice(&[0x48, 0x89, 0x5c, 0x24, 0x08, 0xe8]);
        code.extend_from_slice(&((i % 5) as i32 * 0x40 - at - 10).to_le_bytes());
        code.extend_from_slice(&[0x48, 0x8b, 0x05]);
        code.extend_from_slice(&(0x10000 - at - 17).to_le_bytes());
        code.extend_from_slice(&[0x48, 0x83, 0xc4, 0x28, 0xc3, 0xcc]);
    }
    let mut d = vec![0u8; 0x200];
    d[..2].copy_from_slice(b"MZ");
    d[0x3c] = 0x40;
    d[0x40..0x44].copy_from_slice(b"PE\0\0");
    d[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    d[0x46] = 1; // sections
    d[0x54] = 0x70; // optional header size
    let opt = 0x58;
    d[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    d[opt + 0x18..opt + 0x20].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    let sec = opt + 0x70;
    d[sec..sec + 5].copy_from_slice(b".text");
    d[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes());
    d[sec + 16..sec + 20].copy_from_slice(&(code.len() as u32).to_le_bytes());
    d[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
    d[sec + 36..sec + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes());
    let range = d.len()..d.len() + code.len();
    d.extend_from_slice(&code);
    d.extend_from_slice(&text(1000));
    (d, range)
}

#[test]
fn pe_sections() {
    let (d, code) = pe();
    // The data after the sections, scanned, is a stream of its own.
    assert_eq!(modules(&roundtrip(&d, &Options::default())), [native::X86.name]);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() });
    assert_eq!(modules(&f), [native::X86.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Node::slice(0, 0, code.start as u64));
    assert_eq!(parts[1].len(), code.len() as u64);
    assert_eq!(parts[2], Node::slice(0, code.start as u64, (d.len() - code.end) as u64));
    let classes = recons_dispatch::classes(&f);
    assert!(classes.iter().any(|&(c, n)| c == "x86/CALL_IDX" && n > 0), "{classes:?}");
}

/// A PDF of streams, by their dictionary entries and data.
fn pdf(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut f = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut objs = vec![b"<< /Type /Catalog >>".to_vec()];
    for (dict, data) in streams {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend_from_slice(data);
        o.extend_from_slice(b"\nendstream");
        objs.push(o);
    }
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(f.len());
        f.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        f.extend_from_slice(o);
        f.extend_from_slice(b"\nendobj\n");
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

#[test]
fn pdf_nested() {
    let flac = fixture("flac-recons/tests/data/s16_f5.flac");
    let d = pdf(&[
        ("/Filter /FlateDecode", zlib(&text(200_000), 6)),
        ("/Type /EmbeddedFile /Filter /FlateDecode", zlib(&flac, 6)),
        ("/Filter /LZWDecode", text(1_000_000)),
    ]);
    let f = roundtrip(&d, &all());
    assert_eq!(
        modules(&f),
        [native::PDF.module.name, native::FLATE.name, native::FLATE.name, native::FLAC.module.name],
        "{f}"
    );
    // The FLAC file, the only data of its class, is not left in a pool beside its formula.
    let classes = |f: &Formula| f.streams.iter().map(|s| s.class.to_string()).collect::<Vec<_>>();
    assert!(!classes(&f).contains(&"pdf/pool-data".into()), "{f}");
    let f = roundtrip(&d, &Options { depth: 1, ..all() });
    assert_eq!(modules(&f), [native::PDF.module.name, native::FLATE.name, native::FLATE.name]);
    assert!(classes(&f).contains(&"pdf/pool-data".into()), "{f}");
}

/// Files anywhere in the data are found, each cut to its end.
#[test]
fn scan_embedded() {
    let flac = fixture("flac-recons/tests/data/s16_f5.flac");
    let jpeg = fixture("jpeg-recons/tests/data/gradient.jpg");
    let wav = fixture("flac-recons/tests/data/s16.wav");
    let (t1, t2, t3) = (text(5000), text(3000), text(100));
    let d = [&t1[..], &jpeg, &t2, &gzip(&text(100_000)), &wav, &jpeg, &t3, &flac].concat();
    let f = roundtrip(&d, &all());
    let flac_name = native::FLAC.module.name;
    let jpeg_name = native::JPEG.module.name;
    assert_eq!(modules(&f), [jpeg_name, native::FLATE.name, flac_name, jpeg_name, flac_name], "{f}");
    // The data between the files, as one stream.
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[0], Node::slice(0, 0, t1.len() as u64));
    assert_eq!(f.streams[0].data, [t1, t2, t3].concat());

    // As one file, the data starts like none.
    let whole = Options { file: Nest::Exact, ..all() };
    assert_eq!(roundtrip(&d, &whole).root, Node::slice(0, 0, d.len() as u64));

    // In the data of a DEFLATE stream.
    let f = roundtrip(&gzip(&[&text(5000)[..], &flac, &text(5000)].concat()), &all());
    assert_eq!(modules(&f), [native::FLATE.name, flac_name], "{f}");
    let exact = Options { contents: vec![Content { nest: Nest::Exact, ..native::CONTENTS[0] }], ..all() };
    let f = roundtrip(&gzip(&[&text(5000)[..], &flac, &text(5000)].concat()), &exact);
    assert_eq!(modules(&f), [native::FLATE.name], "{f}");
}

/// A file found at the start is cut to its end too, and what follows is scanned.
#[test]
fn scan_after_file() {
    let jpeg = fixture("jpeg-recons/tests/data/gradient.jpg");
    let d = [&jpeg[..], b"appended", &jpeg].concat();
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [native::JPEG.module.name; 2], "{f}");
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.iter().map(Node::len).collect::<Vec<_>>(), [jpeg.len() as u64, 8, jpeg.len() as u64], "{f}");
    // As one file, all of it is the first JPEG's (whose recompressor codes the other).
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert!(matches!(&f.root, Node::Apply(a) if a.module == native::JPEG.module), "{f}");
}

/// zlib streams and MPEG audio frames without a tag are found in data too, by their checksum
/// and by frames one after another.
#[test]
fn scan_weak() {
    let mp3 = fixture("mp3-recons/tests/data/vbr.mp3");
    let t = text(100_000);
    let d = [&text(5000)[..], &zlib(&t, 6), &text(3000), &mp3, &text(100)].concat();
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [native::FLATE.name, native::MP3.module.name], "{f}");
    // A wrong checksum is no zlib stream.
    let mut z = zlib(&t, 6);
    *z.last_mut().unwrap() ^= 1;
    let f = roundtrip(&[&text(5000)[..], &z].concat(), &Options::default());
    assert_eq!(modules(&f), [""; 0], "{f}");
}

/// Where the files found would take more description than allowed, those with the fewest bytes
/// per byte of description are kept raw.
#[test]
fn description_limit() {
    let jpeg = fixture("jpeg-recons/tests/data/gradient.jpg");
    let flac = fixture("flac-recons/tests/data/s16_f5.flac");
    let d = [jpeg.repeat(20), flac.clone(), jpeg.repeat(20)].concat();
    let bound = |f: &Formula| aqr::description_bound(&f.root);
    let f = roundtrip(&d, &all());
    assert_eq!(f.applies().len(), 41);
    let limit = bound(&f) / 4;
    let f = roundtrip(&d, &Options { description: limit, ..all() });
    assert!(bound(&f) <= limit, "{} > {limit}", bound(&f));
    let found = modules(&f);
    // The FLAC file, the largest, is kept.
    assert!(found.contains(&native::FLAC.module.name) && found.len() < 41 && found.len() > 5, "{found:?}");
    // In a DEFLATE stream, as its contents.
    let z = gzip(&d);
    let opt = Options { description: limit, ..all() };
    let f = roundtrip(&z, &opt);
    assert!(bound(&f) <= limit, "{} > {limit}", bound(&f));
    assert!(modules(&f).len() > 2, "{f}");
    // and when none of its contents fits, the stream alone (with room for the data around
    // it, but for no file).
    let flate = bound(&roundtrip(&z, &Options { depth: 1, ..all() }));
    let f = roundtrip(&z, &Options { description: flate + 64, ..all() });
    assert_eq!(modules(&f), [native::FLATE.name]);
}

/// A file of many files is recompressed whole.
#[test]
fn archive_many_files() {
    let jpeg = fixture("jpeg-recons/tests/data/gradient.jpg");
    let d = jpeg.repeat(3000);
    let f = roundtrip(&d, &all());
    assert_eq!(f.applies().len(), 3000);
}
