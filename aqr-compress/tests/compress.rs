use std::io::Write;

use aqr_common::rng::noise;
use aqr_compress::{CONTENTS, Content, Formula, Nest, Node, Options, compress, recompress};
use aqr_decompress::{
    ARM64, BUNDLE, FLAC, FLATE, JPEG, LEVELDB, LZ4, LZO, MP3, Native, PCM, PDF, PNG, PNG_SET, RASTER, SQLITE, WRT, X86,
    ZIP, decompress,
};
use aqr_formula::aqr;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Module names of `f`'s applications, parents first.
fn modules(f: &Formula) -> Vec<&'static str> {
    f.applies().iter().map(|a| a.module.name).collect()
}

/// An encoder of the modules aqr-decompress knows.
fn encoder() -> aqr::Encoder {
    aqr::Encoder::new(aqr::Level::MID).with_modules(aqr_decompress::wasm_of)
}

/// Recompresses `d`, checking that the formula evaluates back to it, also through the
/// archive.
fn roundtrip(d: &[u8], opt: &Options) -> Formula {
    let f = recompress(d, opt);
    assert_eq!(f.len(), d.len() as u64);
    assert!(f.eval(&Native::default()).unwrap() == d, "formula does not evaluate to the input:\n{f}");
    let c = encoder().encode(&f, Some(aqr::checksum(d))).unwrap();
    assert!(decompress(&c, 1).unwrap() == d, "archive does not decompress to the input:\n{f}");
    let w = aqr::decode(&c, &aqr_decompress::Wasm);
    assert!(w.as_ref().is_ok_and(|w| w == d), "archive does not decompress as WebAssembly: {:?}\n{f}", w.err());
    f
}

/// Options keeping every formula, however large it archives: for what is recompressed.
fn all() -> Options {
    Options { smaller: false, ..Options::default() }
}

/// Lines of text of no words (which aqr-wrt would take, see [`prose`]).
fn text(n: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "{i}: {} + {} = {} (% 13)", i * 7 % 13, i % 5, (i * 7 + i % 5) % 13).unwrap();
    }
    t
}

/// Lines of text of words.
fn prose(n: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", i * 7 % 13).unwrap();
    }
    t
}

/// A gzip member of `d`, of a name that tells nothing of it.
fn gzip(d: &[u8]) -> Vec<u8> {
    gzip_named(d, "a")
}

fn gzip_named(d: &[u8], name: &str) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::new(6));
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
    let c = encoder().encode(&recompress(&d, &all()), Some(aqr::checksum(&d) ^ 1)).unwrap();
    assert_eq!(decompress(&c, 1), Err(aqr::Failure::Invalid("output checksum mismatch".into())));
}

/// A gzip member's trailer is derived by aqr-flate's module where it is right, and kept as it
/// is where it is not.
#[test]
fn gzip_trailer() {
    let opt = all();
    let d = gzip(&text(100_000));
    let trailer = |f: &Formula| f.streams.iter().find(|s| s.class == "gzip").unwrap().data.len();
    let f = roundtrip(&d, &opt);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    let Node::Apply(a) = &parts[1] else { panic!("{f}") };
    assert_eq!(a.params, [2]);
    let header = trailer(&f);
    for at in [d.len() - 8, d.len() - 1] {
        let mut wrong = d.clone();
        wrong[at] ^= 1;
        let f = roundtrip(&wrong, &opt);
        assert_eq!(modules(&f), [FLATE.name]);
        assert_eq!(trailer(&f), header + 8);
    }
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
    // what does shrink is still recompressed (where its module is the archive's anyway)
    let d = [gzip(&noise(200_000)), gzip(&text(100_000))].concat();
    assert_eq!(modules(&roundtrip(&d, &Options { shared_modules: true, ..opt.clone() })), [FLATE.name]);
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
        ("aqr-raw/tests/data/s24.aiff", PCM.name),
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
    // found in data, and archived smaller than raw, with the module
    let d = [text(5000), mozlz4(&records(1_000_000)), text(5000)].concat();
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [LZ4.name]);
}

#[test]
fn lzop_files() {
    for t in ["big.1", "big.9", "mixed.9"] {
        let d = fixture(&format!("aqr-lzo/tests/data/{t}.lzo"));
        assert_eq!(modules(&roundtrip(&d, &all())), [LZO.name], "{t}");
    }
    // found in data
    let d = [text(5000), fixture("aqr-lzo/tests/data/big.9.lzo"), text(5000)].concat();
    assert_eq!(modules(&roundtrip(&d, &all())), [LZO.name]);
    // nothing to rebuild
    assert!(modules(&roundtrip(&fixture("aqr-lzo/tests/data/empty.3.lzo"), &all())).is_empty());
}

#[test]
fn leveldb_tables() {
    for t in ["table-mul", "table-crc", "mixed-mul", "odd-crc"] {
        let d = fixture(&format!("aqr-snappy/tests/data/{t}.ldb"));
        assert_eq!(modules(&roundtrip(&d, &all())), [LEVELDB.name], "{t}");
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
            0xa9bf7bfd,                                                       // stp x29, x30, [sp, #-16]!
            0x9400_0000 | ((i % 5) * 0x10).wrapping_sub(at + 1) & 0x3ff_ffff, // bl f(i % 5)
            0x9000_0008 | ((0x100u32.wrapping_sub(at >> 10) & 3) << 29),      // adrp x8, page
            0xf9400108,                                                       // ldr x8, [x8]
            0xb4000048,                                                       // cbz x8, +8
            0xa8c17bfd,                                                       // ldp x29, x30, [sp], #16
            0xd65f03c0,                                                       // ret
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
    assert_eq!(modules(&roundtrip(&d, &all())), [X86.name]);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert_eq!(modules(&f), [X86.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Node::slice(0, 0, code.start as u64));
    assert_eq!(parts[1].len(), code.len() as u64);
    assert_eq!(parts[2], Node::slice(0, code.start as u64, (d.len() - code.end) as u64));
    let classes = aqr_compress::classes(&f);
    assert!(classes.iter().any(|&(c, n)| c == "x86/CALL_IDX" && n > 0), "{classes:?}");
}

/// The rest of a PE image coded is scanned: a zlib stream in it is found.
#[test]
fn pe_rest_scanned() {
    let (mut d, _) = pe();
    d.extend_from_slice(&zlib(&text(20_000), 6));
    d.extend_from_slice(&text(500));
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert_eq!(modules(&f), [X86.name, FLATE.name], "{f}");
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
    assert_eq!(modules(&roundtrip(&d, &all())), [X86.name]);
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
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
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert_eq!(modules(&f), [X86.name, ARM64.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[0], Node::slice(0, 0, (at[0] + code.start) as u64));
    let mut wrapped = text(3000);
    wrapped.extend_from_slice(&d);
    wrapped.extend_from_slice(&text(3000));
    assert_eq!(modules(&roundtrip(&wrapped, &all())), [X86.name, ARM64.name]);
    // a PowerPC image in place of the ARM64 one
    let mut ppc = d.clone();
    ppc[8 + 20..8 + 24].copy_from_slice(&18u32.to_be_bytes());
    ppc[at[1]..at[1] + 4].copy_from_slice(&[0xfe, 0xed, 0xfa, 0xcf]);
    assert_eq!(modules(&roundtrip(&ppc, &Options { file: Nest::Exact, ..all() })), [X86.name]);
}

/// An ARM64 image is filtered as ARM64 code, and one of another machine (ARM) not at all.
#[test]
fn pe_machines() {
    let (d, code) = pe_of(0xaa64, &arm64_code());
    let f = roundtrip(&d, &Options { file: Nest::Exact, ..all() });
    assert_eq!(modules(&f), [ARM64.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts[1].len(), code.len() as u64);
    let (d, _) = pe_of(0x1c4, &arm64_code());
    assert!(modules(&roundtrip(&d, &Options { file: Nest::Exact, ..all() })).is_empty());
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
    // (the two FlateDecode streams one application, the FLAC file found in its data)
    assert_eq!(modules(&f), [PDF.name, FLAC.name, FLATE.name], "{f}");
    // The FLAC file, the only data of its class, is not left in a pool beside its formula.
    let classes = |f: &Formula| f.streams.iter().map(|s| s.class.to_string()).collect::<Vec<_>>();
    assert!(!classes(&f).contains(&"pdf/pool-data".into()), "{f}");
    let f = roundtrip(&d, &Options { depth: 1, ..all() });
    assert_eq!(modules(&f), [PDF.name, FLATE.name]);
    assert!(classes(&f).contains(&"pdf/pool-data".into()), "{f}");
}

/// An image in a trivial container, its header and trailer kept around aqr-raw's module; as two
/// bytes are too weak a signature, not looked for in other data.
#[test]
fn raw_images() {
    let mut pgm = b"P5\n# a gradient\n320 200\n255\n".to_vec();
    pgm.extend((0..200u32).flat_map(|y| (0..320u32).map(move |x| ((x + y) / 3 + (x * y) % 7) as u8)));
    pgm.extend_from_slice(b"trailer");
    let f = roundtrip(&pgm, &all());
    assert_eq!(modules(&f), [RASTER.name], "{f}");
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3, "{f}");
    let f = roundtrip(&[&text(5000)[..], &pgm].concat(), &all());
    assert!(modules(&f).is_empty(), "{f}");
}

/// Files anywhere in the data are found, each cut to its end.
#[test]
fn scan_embedded() {
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let wav = fixture("aqr-raw/tests/data/s16.wav");
    let (t1, t2, t3) = (text(5000), text(3000), text(100));
    let d = [&t1[..], &jpeg, &t2, &gzip(&text(100_000)), &wav, &jpeg, &t3, &flac].concat();
    let f = roundtrip(&d, &all());
    let flac_name = FLAC.name;
    let jpeg_name = JPEG.name;
    assert_eq!(modules(&f), [jpeg_name, FLATE.name, PCM.name, jpeg_name, flac_name], "{f}");
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
    let t = prose(100_000);
    let d = [&text(5000)[..], &zlib(&t, 6), &text(3000), &mp3, &text(100)].concat();
    // (The MP3 alone archives no smaller than raw, with the tables of its module.)
    let f = roundtrip(&d, &Options { smaller: false, ..Options::default() });
    assert_eq!(modules(&f), [FLATE.name, MP3.name], "{f}");
    let f = roundtrip(&d, &Options { shared_modules: true, ..Options::default() });
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
    // (the deflated bodies, the FLAC file among them, given the archive by aqr-flate's module)
    assert_eq!(modules(&f), [ZIP.name, JPEG.name, FLATE.name, FLATE.name, FLAC.name]);
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
        assert_eq!(modules(&f), [ZIP.name, FLATE.name], "{name}");
    }
}

/// A ZIP archive cut short in its last entry (a download cut short): its entries are still
/// recompressed, as a file and found in data.
#[test]
fn zip_cut() {
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let t = text(50_000);
    let d = zip(&[("a.txt", &t, true), ("b.jpg", &jpeg, false), ("c.txt", &t, true)]);
    let cut = &d[..d.windows(4).rposition(|w| w == b"PK\x03\x04").unwrap() + 200];
    let f = roundtrip(cut, &all());
    assert_eq!(modules(&f), [ZIP.name, JPEG.name, FLATE.name]);
    let f = roundtrip(&[&b"some data before "[..], cut].concat(), &all());
    assert_eq!(modules(&f), [ZIP.name, JPEG.name, FLATE.name]);
}

/// An RGBA PNG file of a picture of `w` x `h` (noise, rows filtered with Sub), with the pixels
/// of `rect` (x, y, width, height) changed: a layer shown over it.
fn picture(w: usize, h: usize, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
    let mut data = Vec::new();
    for y in 0..h {
        data.push(1);
        let mut left = [0u8; 4];
        for x in 0..w {
            let inside = rect.is_some_and(|(rx, ry, rw, rh)| (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y));
            for (c, l) in left.iter_mut().enumerate() {
                let v = if inside {
                    ((x * 31) ^ (y * 17)) as u8
                } else {
                    (((x as u32).wrapping_mul(2654435761) ^ (y as u32 * 40503) ^ (c as u32 * 977)) >> 11) as u8
                };
                data.push(v.wrapping_sub(*l));
                *l = v;
            }
        }
    }
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, d) in [(b"IHDR", ihdr), (b"IDAT", zlib(&data, 6)), (b"IEND", vec![])] {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(&d);
        out.extend_from_slice(&crc32fast::hash(&out[start..]).to_be_bytes());
    }
    out
}

/// PNG files of a ZIP archive that are variants of a picture are one set, coded against each
/// other, and smaller than coded alone; one of another size is coded alone.
#[test]
fn zip_png_set() {
    let files = [
        picture(240, 160, None),
        picture(240, 160, Some((20, 30, 50, 40))),
        picture(240, 160, Some((100, 10, 30, 120))),
        picture(240, 160, Some((20, 30, 50, 40))).into_iter().rev().collect::<Vec<u8>>(),
        picture(100, 90, None),
    ];
    let names = ["a.png", "b.png", "c.png", "d.bin", "e.png"];
    let entries: Vec<(&str, &[u8], bool)> = names.iter().zip(&files).map(|(n, d)| (*n, &d[..], false)).collect();
    let d = zip(&entries);
    let f = roundtrip(&d, &all());
    assert_eq!(f.shared.len(), 1);
    assert_eq!(f.shared[0].sizes.len(), 3);
    let m = modules(&f);
    assert_eq!(
        (m.iter().filter(|&&m| m == PNG_SET.name).count(), m.iter().filter(|&&m| m == PNG.name).count()),
        (1, 1)
    );
    // smaller than without the set
    let records =
        |f: &Formula| -> usize { f.streams.iter().filter(|s| s.class == "png/records").map(|s| s.data.len()).sum() };
    let alone: usize = files[..3].iter().map(|d| records(&recompress(d, &all()))).sum();
    let set = records(&f) - records(&recompress(&files[4], &all()));
    assert!(set * 2 < alone, "{set} against {alone}");
    // archived and verified (the files alone evaluated beforehand but those of the set), and
    // not kept as they are (as verification failing would)
    let c = compress(&d, &all());
    assert!(decompress(&c, 1).unwrap() == d);
    let raw = aqr::encode(&Formula::raw("raw", d.clone()), None, aqr::Level::MID).unwrap().len();
    // (without the module blocks, which an archive of more files has once)
    let mut ms: Vec<_> = f.applies().iter().map(|a| a.module).collect();
    ms.dedup();
    let modules: usize = ms.iter().map(|&m| aqr_decompress::built(m).unwrap().block.len()).sum();
    assert!((c.len() - modules) * 10 < raw * 9, "{} against {raw}", c.len() - modules);
    // the same where the files coded alone would not pay (as this noise does not), the
    // modules the archive's anyway
    let f = roundtrip(&d, &Options { shared_modules: true, ..Options::default() });
    assert_eq!(f.shared.len(), 1);
    assert_eq!(f.shared[0].sizes.len(), 3);
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
    // five levels of archives (each in the deflated body of the one before, the innermost of
    // stored entries), and nothing inflated as much as the zeros
    let levels = [ZIP.name, FLATE.name].repeat(5);
    assert_eq!(modules(&f), [&levels[..], &[ZIP.name]].concat());
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

/// What recompressors know of their data refines what is made of it: the data of a gzip member
/// by its name, the contents of a ZIP archive's entries by theirs, a PDF file's streams by
/// what they are.
#[test]
fn kinds() {
    let flac = fixture("aqr-flac/tests/data/s16_f5.flac");
    // text and compressed data is not looked into
    for (name, found) in [("a", true), ("a.flac", true), ("a.txt", false), ("a.mp4", false)] {
        let f = roundtrip(&gzip_named(&flac, name), &all());
        let want: &[&str] = if found { &[FLATE.name, FLAC.name] } else { &[FLATE.name] };
        assert_eq!(modules(&f), want, "{name}");
    }
    // A file named so is one of its format, whatever it starts like, and not looked into where
    // its recompressor turns it down; what follows it is.
    // (no body the same as another, which would be taken again)
    let gz = |n| gzip(&text(n));
    let junk = |j: &[u8]| [j, &flac].concat();
    let d = zip(&[
        ("a.png", &gz(100_000), false),
        ("b.png", &junk(b"junk"), true),
        ("c.bin", &junk(b"other junk"), true),
        ("d.gz", &[gz(50_000), gz(60_000)].concat(), false),
        ("e.bin", &gz(70_000), false),
    ]);
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), [ZIP.name, FLATE.name, FLATE.name, FLATE.name, FLATE.name, FLAC.name], "{f}");
    // Pixels are not looked into, embedded files are, and what an image's filter after
    // FlateDecode decodes is taken as such: a JPEG file for DCTDecode.
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    for (dict, data, found) in [
        ("/Subtype /Image /Filter /FlateDecode", &flac, None),
        ("/Type /EmbeddedFile /Filter /FlateDecode", &flac, Some(FLAC.name)),
        ("/Subtype /Image /Filter [/FlateDecode /DCTDecode]", &jpeg, Some(JPEG.name)),
        ("/Subtype /Image /Filter [/FlateDecode /JPXDecode]", &[b"junk", &jpeg[..]].concat(), None),
    ] {
        let d = pdf(&[
            ("/Filter /FlateDecode", zlib(&text(200_000), 6)),
            (dict, zlib(data, 6)),
            ("/Filter /LZWDecode", text(1_000_000)),
        ]);
        let f = roundtrip(&d, &all());
        let want: Vec<&str> = [PDF.name].into_iter().chain(found).chain([FLATE.name]).collect();
        assert_eq!(modules(&f), want, "{dict}: {f}");
    }
}

/// A FASTA file of nucleotides goes to aqr-fasta, by its start or its name.
#[test]
fn fasta() {
    let mut d = b">chr test\n".to_vec();
    for i in 0..4000 {
        let line: Vec<u8> =
            (0..60).map(|j| if i % 500 < 5 { b'N' } else { b"ACGTacgtTTAG"[(i * 13 + j * 7 + j / 5) % 12] }).collect();
        d.extend_from_slice(&line);
        d.push(b'\n');
    }
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), ["fasta-recons"]);
    assert_eq!(aqr_compress::kind_by_name(b"x/chr1.fa"), aqr_formula::Kind::Format("fasta"));
}

/// A DDS file of BC1 blocks of a smooth picture, `w` × `h` with `mips` mipmaps.
fn dds_file(w: u32, h: u32, mips: u32) -> Vec<u8> {
    let mut d = b"DDS ".to_vec();
    for v in [124, 0x2_1007, h, w, 0, 0, mips] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.resize(76, 0);
    for v in [32u32, 4] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(b"DXT1");
    d.resize(108, 0);
    d.extend_from_slice(&0x40_1008u32.to_le_bytes());
    d.resize(128, 0);
    for m in 0..mips {
        let (bw, bh) = ((w >> m).max(1).div_ceil(4), (h >> m).max(1).div_ceil(4));
        for by in 0..bh {
            for bx in 0..bw {
                let c1 = ((bx * 31 / bw) << 11 | (by * 63 / bh) << 5 | (m * 4)) as u16;
                d.extend_from_slice(&(c1 + 0x0821).to_le_bytes());
                d.extend_from_slice(&c1.to_le_bytes());
                let i = (bx * 7 + by * 13 + bx * by) as usize;
                d.extend_from_slice(&[0x00, 0x55, 0xaa, 0xff, 0x1b, 0xe4][i % 6..][..1].repeat(4));
            }
        }
    }
    d
}

/// A DDS texture goes to aqr-tex, by its start or its name, and is found in other data. A large
/// one is coded in stripes, decoded apart (as tasks of the Wasm module).
#[test]
fn dds() {
    let d = dds_file(64, 64, 7);
    let f = roundtrip(&d, &all());
    assert_eq!(modules(&f), ["bcn-recons"]);
    let mut other = vec![0x5a; 1000];
    other.extend_from_slice(&d);
    other.extend_from_slice(&[0x5a; 1000]);
    let f = roundtrip(&other, &all());
    assert_eq!(modules(&f), ["bcn-recons"]);
    assert_eq!(aqr_compress::kind_by_name(b"x/tex.DDS"), aqr_formula::Kind::Format("dds"));
    let f = roundtrip(&dds_file(1024, 1024, 2), &all());
    assert_eq!(modules(&f), ["bcn-recons"]);
}

/// Text goes to aqr-wrt where that pays: a whole file of text bytes, and data of a name telling
/// text (a gzip member's); not text that is short, or has other bytes, or of no words.
#[test]
fn text_words() {
    let d = prose(400_000);
    assert_eq!(modules(&roundtrip(&d, &all())), [WRT.name]);
    assert_eq!(modules(&roundtrip(&gzip_named(&d, "a.txt"), &all())), [FLATE.name, WRT.name]);
    assert_eq!(modules(&roundtrip(&gzip(&d), &all())), [FLATE.name, WRT.name]);
    // as aqr-wrt tells, whatever the archive would make of the file without it
    assert_eq!(modules(&roundtrip(&d, &Options::default())), [WRT.name]);
    assert!(modules(&roundtrip(&d[..200_000], &all())).is_empty());
    assert!(modules(&roundtrip(&[&d[..], b"\0"].concat(), &all())).is_empty());
    assert!(modules(&roundtrip(&text(400_000), &all())).is_empty());
}

/// A ustar header of `name` for a body of `n` bytes.
fn tar_header(name: &str, n: usize) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[124..136].copy_from_slice(format!("{n:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"14123456701\0");
    h[156] = b'0';
    h[257..265].copy_from_slice(b"ustar\x0000");
    let sum: u32 = h.iter().map(|&b| b as u32).sum::<u32>() + 8 * b' ' as u32;
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut d = Vec::new();
    for (name, body) in files {
        d.extend(tar_header(name, body.len()));
        d.extend_from_slice(body);
        d.resize(d.len().next_multiple_of(512), 0);
    }
    d.resize(d.len() + 10240 - d.len() % 10240, 0);
    d
}

/// ar, tar and cpio archives go to aqr-bundle, and their members to their recompressors (by
/// their names, or found in them); a tar archive in a gzip file too.
#[test]
fn bundles() {
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let png = picture(64, 48, None);
    let t = tar(&[
        ("a.txt", &text(5000)),
        ("b.jpg", &jpeg),
        ("c.bin", &[&noise(100)[..], &zlib(&text(30_000), 6)].concat()),
        ("d.png", &png),
    ]);
    let f = roundtrip(&t, &all());
    assert_eq!(modules(&f), [BUNDLE.name, JPEG.name, FLATE.name, PNG.name], "{f}");
    let f = roundtrip(&gzip_named(&t, "x.tar"), &all());
    assert_eq!(modules(&f), [FLATE.name, BUNDLE.name, JPEG.name, FLATE.name, PNG.name], "{f}");
    // found where a file starts: ar and cpio of ASCII headers, not tar
    let ar = fixture("aqr-bundle/tests/data/llvm-gnu.a");
    let cpio = fixture("aqr-bundle/tests/data/bsdtar-newc.cpio");
    let d = [&noise(1000)[..], &ar, &noise(1000), &cpio, &t].concat();
    let f = roundtrip(&d, &all());
    // (the Mach-O objects of the ar archive by aqr-exe)
    assert_eq!(modules(&f).iter().filter(|&&m| m == BUNDLE.name).count(), 2, "{f}");
    assert_eq!(aqr_compress::kind_by_name(b"x/y.tar"), aqr_formula::Kind::Format("tar"));
}

/// A bundle does not count as a format deeper: a JPEG file in a ZIP archive in a tar archive in
/// gzip is recompressed as one in a ZIP archive in gzip is.
#[test]
fn bundles_not_deeper() {
    let jpeg = fixture("aqr-jpeg/tests/data/gradient.jpg");
    let z = zip(&[("a.jpg", &jpeg, false), ("b.txt", &text(3000), false)]);
    let d = gzip_named(&tar(&[("x.zip", &z)]), "x.tar");
    let f = roundtrip(&d, &Options { smaller: false, ..Options::default() });
    assert_eq!(modules(&f), [FLATE.name, BUNDLE.name, ZIP.name, JPEG.name], "{f}");
}
