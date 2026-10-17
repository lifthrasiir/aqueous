use std::io::Write;

use recons_dispatch::{Formula, Native, Node, Options, native, recompress};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Module names of `f`'s applications, parents first.
fn modules(f: &Formula) -> Vec<&'static str> {
    f.applies().iter().map(|a| a.module.name).collect()
}

/// Recompresses `d`, checking that the formula evaluates back to it.
fn roundtrip(d: &[u8], opt: &Options) -> Formula {
    let f = recompress(d, opt);
    assert_eq!(f.len(), d.len() as u64);
    assert!(f.eval(&Native::default()).unwrap() == d, "formula does not evaluate to the input:\n{f}");
    f
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
fn raw_fallback() {
    let opt = Options::default();
    for d in [&b""[..], b"hello", b"\x78\x9cnot zlib at all", b"\x1f\x8b\x08\0garbage"] {
        let f = roundtrip(d, &opt);
        assert_eq!(f.root, Node::slice(0, 0, d.len() as u64));
    }
}

#[test]
fn whole_fixtures() {
    let opt = Options::default();
    for (path, module) in [
        ("flac-recons/tests/data/s16_f5.flac", native::FLAC.name),
        ("mp3-recons/tests/data/cbr128.mp3", native::MP3.name),
        ("mp3-recons/tests/data/tags.mp3", native::MP3.name),
        ("jpeg-recons/tests/data/prog_script_recode.jpg", native::JPEG.name),
        ("pdf-recons/tests/data/gradient.jpg", native::JPEG.name),
    ] {
        let f = roundtrip(&fixture(path), &opt);
        assert_eq!(modules(&f), [module], "{path}");
    }
}

#[test]
fn gzip_nested() {
    let flac = fixture("flac-recons/tests/data/s16_f5.flac");
    let f = roundtrip(&gzip(&flac), &Options::default());
    assert_eq!(modules(&f), [native::FLATE.name, native::FLAC.name]);

    // Trailing bytes after the member are kept.
    let mut d = gzip(&text(100_000));
    d.extend_from_slice(b"trailing");
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [native::FLATE.name]);

    let d = gzip(&zlib(&text(100_000), 9));
    assert_eq!(modules(&roundtrip(&d, &Options::default())), [native::FLATE.name; 2]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 1, ..Options::default() })), [native::FLATE.name]);
    assert_eq!(modules(&roundtrip(&d, &Options { depth: 0, ..Options::default() })), [""; 0]);
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
    let f = roundtrip(&d, &Options::default());
    assert_eq!(modules(&f), [native::X86.name]);
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], Node::slice(0, 0, code.start as u64));
    assert_eq!(parts[1].len(), code.len() as u64);
    assert_eq!(parts[2], Node::slice(0, code.start as u64, (d.len() - code.end) as u64));
    let classes = recons_dispatch::classes(&f);
    assert!(classes.iter().any(|&(c, n)| c == "x86/CALL_IDX" && n > 0), "{classes:?}");
}
