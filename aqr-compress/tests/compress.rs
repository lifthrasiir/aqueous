use std::io::Write;

use aqr_common::rng::noise;
use aqr_compress::{CONTENTS, Content, Formula, Nest, Node, Options, compress, recompress};
use aqr_decompress::{ARM64, FLAC, FLATE, JPEG, LZ4, MP3, Native, PDF, SNAPPY, SQLITE, X86, ZIP, decompress};
use aqr_formula::aqr;

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
    assert_eq!(modules(&roundtrip(&d, &opt)), [FLATE.name]);
    // Files found that each archive smaller, but not all together.
    let d = fixture("aqr-jpeg/tests/data/gradient.jpg").repeat(100);
    assert!(modules(&roundtrip(&d, &opt)).is_empty());
    assert_eq!(roundtrip(&d, &all()).applies().len(), 100);
}

#[test]
fn recons_fixtures() {
    let opt = all();
    for (path, module) in [
        ("aqr-flac/tests/data/s16_f5.flac", FLAC.name),
        ("aqr-mp3/tests/data/cbr128.mp3", MP3.name),
        ("aqr-mp3/tests/data/tags.mp3", MP3.name),
        ("aqr-jpeg/tests/data/prog_script_recode.jpg", JPEG.name),
        ("aqr-pdf/tests/data/gradient.jpg", JPEG.name),
    ] {
        let f = roundtrip(&fixture(path), &opt);
        assert_eq!(modules(&f), [module], "{path}");
    }
}

#[test]
fn gzip_nested() {
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let f = roundtrip(&gzip(&flac), &all());
    assert_eq!(modules(&f), [FLATE.name, FLAC.name]);

    // Trailing bytes after the member are kept.
    let mut d = gzip(&text(100_000));
    d.extend_from_slice(b"trailing");
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [FLATE.name]);

    // Only the contents named are recompressed.
    let none = Options { contents: vec![], ..all() };
    assert_eq!(modules(&roundtrip(&gzip(&flac), &none)), [FLATE.name]);

    let d = gzip(&zlib(&text(100_000), 9));
    assert_eq!(modules(&roundtrip(&d, &all())), [FLATE.name; 2]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 1, ..all() })), [FLATE.name]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 0, ..all() })), [""; 0]);
}

/// Lines of JSON like Firefox's session store, less regular than [`text`].
fn records(n: usize) -> Vec<u8> {
    let mut t = Vec::new();
    let mut r = 0x9e3779b97f4a7c15u64;
    while t.len() < n {
        r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let k = r >> 33;
        writeln!(
            t,
            r#"{{"url":"https://site{}.example/{:x}","title":"page {}","t":{}}},"#,
            k % 50,
            k,
            k % 997,
            1790000000000 + k
        )
        .unwrap();
    }
    t
}

fn mozlz4(d: &[u8]) -> Vec<u8> {
    let blk = aqr_lz4::block::Compressor::compress_fast(d, 1).unwrap();
    [&aqr_lz4::formula::MOZLZ4[..], &(d.len() as u32).to_le_bytes(), &blk].concat()
}

#[test]
fn lz4_nested() {
    let f = roundtrip(&fixture("aqr-lz4/tests/data/f64k.lz4"), &all());
    assert_eq!(modules(&f), [LZ4.name]);
    // linked blocks are not reproduced
    assert!(modules(&roundtrip(&fixture("aqr-lz4/tests/data/f64k-linked.lz4"), &all())).is_empty());

    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let f = roundtrip(&mozlz4(&gzip(&flac)), &all());
    assert_eq!(modules(&f), [LZ4.name, FLATE.name, FLAC.name]);
    // found in data, and archived smaller than raw
    let d = [text(5000), mozlz4(&records(200_000)), text(5000)].concat();
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [LZ4.name]);
}

#[test]
fn leveldb_tables() {
    for t in ["table-mul", "table-crc", "mixed-mul", "odd-crc"] {
        let d = fixture(&format!("aqr-snappy/tests/data/{t}.ldb"));
        assert_eq!(modules(&roundtrip(&d, &all())), [SNAPPY.name], "{t}");
    }
    // known by its end: not found in data, where it would be cut there
    let d = fixture("aqr-snappy/tests/data/table-mul.ldb");
    assert!(modules(&roundtrip(&[b"x", &d[..]].concat(), &all())).is_empty());
}

#[test]
fn zlib_checksum() {
    let t = text(50_000);
    for level in [1, 6, 9] {
        let f = roundtrip(&zlib(&t, level), &all());
        assert_eq!(modules(&f), [FLATE.name]);
        // The data is a raw stream of its own, between the header and the checksum.
        assert!(f.streams.iter().any(|s| s.data == t));
    }
    let mut d = zlib(&t, 6);
    *d.last_mut().unwrap() ^= 1;
    assert!(modules(&roundtrip(&d, &all())).is_empty());
}

/// x86-64 code: calls among a few functions, RIP-relative loads.
fn x86_64_code() -> Vec<u8> {
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
    code
}

/// ARM64 code: calls to a few functions, page references, loads and local branches.
fn arm64_code() -> Vec<u8> {
    let mut words = Vec::new();
    for i in 0..2000u32 {
        let at = words.len() as u32;
        words.extend_from_slice(&[
            0xa9bf7bfd,                                               // stp x29, x30, [sp, #-16]!
            0x9400_0000 | ((i % 5) * 0x10).wrapping_sub(at + 1) & 0x3ff_ffff, // bl f(i % 5)
            0x9000_0008 | ((0x100u32.wrapping_sub(at >> 10) & 3) << 29), // adrp x8, page
            0xf9400108,                                               // ldr x8, [x8]
            0xb4000048,                                               // cbz x8, +8
            0xa8c17bfd,                                               // ldp x29, x30, [sp], #16
            0xd65f03c0,                                               // ret
        ]);
    }
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// A PE32+ image with one executable section of x86-64 code, followed by other data.
fn pe() -> (Vec<u8>, std::ops::Range<usize>) {
    pe_of(0x8664, &x86_64_code())
}

/// [`pe`] for `machine`, with `code`.
fn pe_of(machine: u16, code: &[u8]) -> (Vec<u8>, std::ops::Range<usize>) {
    let mut d = vec![0u8; 0x200];
    d[..2].copy_from_slice(b"MZ");
    d[0x3c] = 0x40;
    d[0x40..0x44].copy_from_slice(b"PE\0\0");
    d[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
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
    d.extend_from_slice(code);
    d.extend_from_slice(&text(1000));
    (d, range)
}

#[test]
fn pe_sections() {
    let (d, code) = pe();
    // The data after the sections, scanned, is a stream of its own.
    assert_eq!(modules(&roundtrip(&d, &Options::default())), [X86.name]);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() });
    assert_eq!(modules(&f), [X86.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Node::slice(0, 0, code.start as u64));
    assert_eq!(parts[1].len(), code.len() as u64);
    assert_eq!(parts[2], Node::slice(0, code.start as u64, (d.len() - code.end) as u64));
    let classes = aqr_compress::classes(&f);
    assert!(classes.iter().any(|&(c, n)| c == "x86/CALL_IDX" && n > 0), "{classes:?}");
}

const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;

/// A thin 64-bit Mach-O image of `cputype` with one segment holding `code` in a section of
/// instructions, followed by other data in the segment; and where the code is.
fn macho(cputype: u32, code: &[u8]) -> (Vec<u8>, std::ops::Range<usize>) {
    let (cmds, at) = (72 + 80, 0x200);
    let mut d = vec![0u8; at];
    let put32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, p: usize, v: u64| d[p..p + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut d, 0, 0xfeed_facf);
    put32(&mut d, 4, cputype);
    put32(&mut d, 12, 2); // MH_EXECUTE
    put32(&mut d, 16, 1);
    put32(&mut d, 20, cmds as u32);
    let size = (at + code.len() + 2000) as u64;
    put32(&mut d, 32, 0x19); // LC_SEGMENT_64
    put32(&mut d, 36, cmds as u32);
    d[40..46].copy_from_slice(b"__TEXT");
    put64(&mut d, 32 + 24, 0x1_0000_0000);
    put64(&mut d, 32 + 32, size);
    put64(&mut d, 32 + 48, size);
    put32(&mut d, 32 + 64, 1);
    let s = 32 + 72;
    d[s..s + 6].copy_from_slice(b"__text");
    d[s + 16..s + 22].copy_from_slice(b"__TEXT");
    put64(&mut d, s + 32, 0x1_0000_0000 + at as u64);
    put64(&mut d, s + 40, code.len() as u64);
    put32(&mut d, s + 48, at as u32);
    put32(&mut d, s + 64, 0x8000_0400);
    d.extend_from_slice(code);
    d.extend_from_slice(&text(2000));
    (d, at..at + code.len())
}

#[test]
fn macho_sections() {
    let (d, code) = macho(CPU_TYPE_X86_64, &x86_64_code());
    assert_eq!(modules(&roundtrip(&d, &Options::default())), [X86.name]);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() });
    assert_eq!(modules(&f), [X86.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Node::slice(0, 0, code.start as u64));
    assert_eq!(parts[1].len(), code.len() as u64);
    assert_eq!(f.streams[0].class, "macho");
}

/// In a universal file, the code of each image is filtered by its architecture, that of
/// others (PowerPC) kept with the rest; found in other data too.
#[test]
fn macho_universal() {
    let (x86, code) = macho(CPU_TYPE_X86_64, &x86_64_code());
    let (arm, _) = macho(CPU_TYPE_ARM64, &arm64_code());
    let mut d = Vec::new();
    d.extend_from_slice(&0xcafe_babeu32.to_be_bytes());
    d.extend_from_slice(&2u32.to_be_bytes());
    let at = [0x1000, (0x1000 + x86.len()).next_multiple_of(0x1000)];
    for ((cputype, image), at) in [(CPU_TYPE_X86_64, &x86), (CPU_TYPE_ARM64, &arm)].into_iter().zip(at) {
        for v in [cputype, 3, at as u32, image.len() as u32, 12] {
            d.extend_from_slice(&v.to_be_bytes());
        }
    }
    d.resize(at[0], 0);
    d.extend_from_slice(&x86);
    d.resize(at[1], 0);
    d.extend_from_slice(&arm);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() });
    assert_eq!(modules(&f), [X86.name, ARM64.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[0], Node::slice(0, 0, (at[0] + code.start) as u64));
    let mut wrapped = text(3000);
    wrapped.extend_from_slice(&d);
    wrapped.extend_from_slice(&text(3000));
    assert_eq!(modules(&roundtrip(&wrapped, &Options::default())), [X86.name, ARM64.name]);
    // a PowerPC image in place of the ARM64 one
    let mut ppc = d.clone();
    ppc[8 + 20..8 + 24].copy_from_slice(&18u32.to_be_bytes());
    ppc[at[1]..at[1] + 4].copy_from_slice(&[0xfe, 0xed, 0xfa, 0xcf]);
    assert_eq!(modules(&roundtrip(&ppc, &Options { file: Nest::Exact, ..Options::default() })), [X86.name]);
}

/// An ARM64 image is filtered as ARM64 code, and one of another machine (ARM) not at all.
#[test]
fn pe_machines() {
    let (d, code) = pe_of(0xaa64, &arm64_code());
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() });
    assert_eq!(modules(&f), [ARM64.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[1].len(), code.len() as u64);
    let (d, _) = pe_of(0x1c4, &arm64_code());
    assert!(modules(&roundtrip(&d, &Options { file: Nest::Exact, ..Options::default() })).is_empty());
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
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let d = pdf(&[
        ("/Filter /FlateDecode", zlib(&text(200_000), 6)),
        ("/Type /EmbeddedFile /Filter /FlateDecode", zlib(&flac, 6)),
        ("/Filter /LZWDecode", text(1_000_000)),
    ]);
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [PDF.name, FLATE.name, FLATE.name, FLAC.name], "{f}");
    // The FLAC file, the only data of its class, is not left in a pool beside its formula.
    let classes = |f: &Formula| f.streams.iter().map(|s| s.class.to_string()).collect::<Vec<_>>();
    assert!(!classes(&f).contains(&"pdf/pool-data".into()), "{f}");
    let f = roundtrip(&d, &Options { depth: 1, ..all() });
    assert_eq!(modules(&f), [PDF.name, FLATE.name, FLATE.name]);
    assert!(classes(&f).contains(&"pdf/pool-data".into()), "{f}");
}

/// Files anywhere in the data are found, each cut to its end.
#[test]
fn scan_embedded() {
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let wav = fixture("aqr-flac/tests/data/s16.wav");
    let (t1, t2, t3) = (text(5000), text(3000), text(100));
    let d = [&t1[..], &jpeg, &t2, &gzip(&text(100_000)), &wav, &jpeg, &t3, &flac].concat();
    let f = roundtrip(&d, &all());
    let flac_name = FLAC.name;
    let jpeg_name = JPEG.name;
    assert_eq!(modules(&f), [jpeg_name, FLATE.name, flac_name, jpeg_name, flac_name], "{f}");
    // The data between the files, as one stream.
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[0], Node::slice(0, 0, t1.len() as u64));
    assert_eq!(f.streams[0].data, [t1, t2, t3].concat());

    // As one file, the data starts like none.
    let whole = Options { file: Nest::Exact, ..all() };
    assert_eq!(roundtrip(&d, &whole).root, Node::slice(0, 0, d.len() as u64));

    // In the data of a DEFLATE stream.
    let f = roundtrip(&gzip(&[&text(5000)[..], &flac, &text(5000)].concat()), &all());
    assert_eq!(modules(&f), [FLATE.name, flac_name], "{f}");
    let exact = Options { contents: vec![Content { nest: Nest::Exact, ..CONTENTS[0] }], ..all() };
    let f = roundtrip(&gzip(&[&text(5000)[..], &flac, &text(5000)].concat()), &exact);
    assert_eq!(modules(&f), [FLATE.name], "{f}");
}

/// A file found at the start is cut to its end too, and what follows is scanned.
#[test]
fn scan_after_file() {
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let d = [&jpeg[..], b"appended", &jpeg].concat();
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [JPEG.name; 2], "{f}");
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.iter().map(Node::len).collect::<Vec<_>>(), [jpeg.len() as u64, 8, jpeg.len() as u64], "{f}");
    // As one file, all of it is the first JPEG's (whose recompressor codes the other).
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert!(matches!(&f.root, Node::Apply(a) if a.module == JPEG), "{f}");
}

/// zlib streams and MPEG audio frames without a tag are found in data too, by their checksum
/// and by frames one after another.
#[test]
fn scan_weak() {
    let mp3 = fixture("aqr-mp3/tests/data/vbr.mp3");
    let t = text(100_000);
    let d = [&text(5000)[..], &zlib(&t, 6), &text(3000), &mp3, &text(100)].concat();
    // (The MP3 alone archives no smaller than raw, with the tables of its module.)
    let f = roundtrip(&d, &Options { smaller: false, ..Options::default() });
    assert_eq!(modules(&f), [FLATE.name, MP3.name], "{f}");
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [FLATE.name], "{f}");
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
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let d = [jpeg.repeat(20), flac.clone(), jpeg.repeat(20)].concat();
    let bound = |f: &Formula| aqr::description_bound(&f.root);
    let f = roundtrip(&d, &all());
    assert_eq!(f.applies().len(), 41);
    let limit = bound(&f) / 4;
    let f = roundtrip(&d, &Options { description: limit, ..all() });
    assert!(bound(&f) <= limit, "{} > {limit}", bound(&f));
    let found = modules(&f);
    // The FLAC file, the largest, is kept.
    assert!(found.contains(&FLAC.name) && found.len() < 41 && found.len() > 5, "{found:?}");
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
    assert_eq!(modules(&f), [FLATE.name]);
}

/// A file of many files is recompressed whole.
#[test]
fn archive_many_files() {
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let d = jpeg.repeat(3000);
    let f = roundtrip(&d, &all());
    assert_eq!(f.applies().len(), 3000);
}

/// A ZIP archive of `entries` (name, data, deflated or stored), as most writers make it.
fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let (mut o, mut cd) = (Vec::new(), Vec::new());
    for &(name, data, deflate) in entries {
        let body = if deflate {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let mut common = Vec::new();
        common.extend_from_slice(&20u16.to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        common.extend_from_slice(&(if deflate { 8u16 } else { 0 }).to_le_bytes());
        common.extend_from_slice(&0x5021_6c4au32.to_le_bytes());
        common.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
        common.extend_from_slice(&(body.len() as u32).to_le_bytes());
        common.extend_from_slice(&(data.len() as u32).to_le_bytes());
        common.extend_from_slice(&(name.len() as u16).to_le_bytes());
        common.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(b"PK\x01\x02\x14\x03");
        cd.extend_from_slice(&common);
        cd.extend_from_slice(&[0; 6]);
        cd.extend_from_slice(&0o100644u32.wrapping_shl(16).to_le_bytes());
        cd.extend_from_slice(&(o.len() as u32).to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
        o.extend_from_slice(b"PK\x03\x04");
        o.extend_from_slice(&common);
        o.extend_from_slice(name.as_bytes());
        o.extend_from_slice(&body);
    }
    let (at, n) = (o.len() as u32, entries.len() as u16);
    o.extend_from_slice(&cd);
    o.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    o.extend_from_slice(&n.to_le_bytes());
    o.extend_from_slice(&n.to_le_bytes());
    o.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    o.extend_from_slice(&at.to_le_bytes());
    o.extend_from_slice(&[0, 0]);
    o
}

#[test]
fn zip_nested() {
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let t = text(50_000);
    let gz = gzip(&t);
    let d = zip(&[("a.txt", &t, true), ("b.flac", &flac, true), ("c.jpg", &jpeg, false), ("d.gz", &gz, false)]);
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [ZIP.name, FLAC.name, JPEG.name, FLATE.name]);
    // found in data, and in data inflated from a gzip member
    let f = roundtrip(&[&b"some data before "[..], &d, b" and after"].concat(), &all());
    assert_eq!(modules(&f)[0], ZIP.name);
    assert_eq!(modules(&roundtrip(&gzip(&d), &all()))[..2], [FLATE.name, ZIP.name]);
    // with bytes between its records, still found in data
    let mut odd = d.clone();
    let cd = odd.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    odd.splice(cd..cd, *b"padding");
    let at = odd.len() - 6;
    let off = u32::from_le_bytes(odd[at..at + 4].try_into().unwrap()) + 7;
    odd[at..at + 4].copy_from_slice(&off.to_le_bytes());
    let f = roundtrip(&[&b"some data before "[..], &odd, b" and after"].concat(), &all());
    assert_eq!(modules(&f)[0], ZIP.name);
    // archives written by tools
    for name in ["py-stream.zip", "infozip.zip", "go.zip", "ditto.zip", "bsdtar.zip"] {
        let f = roundtrip(&fixture(&format!("aqr-zip/tests/data/{name}")), &all());
        assert_eq!(modules(&f), [ZIP.name], "{name}");
    }
}

/// Archives of archives, each level the same archive 16 times like 42.zip, down to a large run
/// of zeros: each level inflates one of them (the rest are taken again), and the zeros, past
/// what may be inflated, stay deflated.
#[test]
fn zip_nested_like_42() {
    let mut level = zip(&[("0.dll", &vec![0; 96 << 20], true)]);
    for name in ["page", "doc", "chapter", "book", "lib"] {
        let names: Vec<String> = (0..16).map(|i| format!("{name} {i:x}.zip")).collect();
        let es: Vec<(&str, &[u8], bool)> = names.iter().map(|n| (&n[..], &level[..], true)).collect();
        level = zip(&es);
    }
    let opt = Options { depth: 10, ..all() };
    let t = std::time::Instant::now();
    let f = roundtrip(&level, &opt);
    assert!(t.elapsed().as_secs() < 60);
    // five levels of archives, and nothing inflated as much as the zeros
    assert_eq!(modules(&f), [ZIP.name; 6]);
    let inflated: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
    assert!(inflated < 64 << 20, "{inflated}");
    // with room for it, the zeros are inflated too
    let f = roundtrip(&level, &Options { expansion: 1 << 20, ..opt });
    let inflated: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
    assert!(inflated >= 96 << 20, "{inflated}");
}

/// A database made by the sqlite3 tool from `sql` (None without it).
fn sqlite_db(sql: &str) -> Option<Vec<u8>> {
    let path = std::env::temp_dir().join(format!("aqr-compress-test-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let out = std::process::Command::new("sqlite3").arg(&path).arg(sql).output().ok()?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let d = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    Some(d)
}

#[test]
fn sqlite_whole() {
    let sql = "create table t(a integer primary key, b text);
        with recursive n(i) as (select 1 union all select i + 1 from n where i < 3000)
        insert into t select i, 'row ' || i || ' of the table, with some text' from n;";
    let Some(db) = sqlite_db(sql) else { return };
    assert_eq!(modules(&roundtrip(&db, &all())), [SQLITE.name]);
    // a database is a whole file: not looked for in the middle of data
    assert!(modules(&roundtrip(&[&b"some data before "[..], &db].concat(), &all())).is_empty());
}
