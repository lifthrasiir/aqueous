//! Edge cases of the file: extreme ratios, chains of filters, dictionaries longer than the
//! look-back, and `/Length` objects an incremental update redefines.

use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};
use aqr_pdf::encode::Options;
use aqr_pdf::formula;
use aqr_testkit::Refusing;
use flate_zoo::{Encoder, backends};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let enc = Encoder::new(flate_zoo::backend("zlib").unwrap_or(backends()[0]));
    let mut z = vec![0x78, 0x9c];
    z.extend_from_slice(&enc.compress(data).unwrap());
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

fn content(seed: u32, n: usize) -> Vec<u8> {
    let mut s = String::from("BT /F1 12 Tf\n");
    let mut x = seed;
    for i in 0..n {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        s += &format!("{} {} Td ({}) Tj\n", (x >> 16) % 600, i % 800, ["Hello", "PDF", "world"][(x >> 8) as usize % 3]);
    }
    s.into_bytes()
}

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, i: &[&[u8]]| formula::apply(a, i, threads))
}

/// The file through several sets of options and threads: whatever formula is made, it gives
/// the file back.
fn check(name: &str, file: &[u8]) {
    let base = Options::default();
    for (oname, o) in [
        ("default", base.clone()),
        ("all", Options { speed: f64::INFINITY, ..base.clone() }),
        ("raw", Options { tok: false, full: false, png: false, ..base.clone() }),
    ] {
        for t in [1, 3] {
            let Some(f) = formula::encode_with(file, &Options { threads: t, ..o.clone() }) else { continue };
            let back = eval(&f, t).unwrap_or_else(|e| panic!("{name} ({oname}, {t}): {e}"));
            assert!(back == file, "{name} ({oname}, {t}): round trip differs");
        }
    }
}

/// An object with a raw dictionary body and a stream, no `/Length` added.
fn obj(f: &mut Vec<u8>, n: usize, dict: &str, stream: Option<&[u8]>) {
    f.extend_from_slice(format!("{n} 0 obj\n{dict}\n").as_bytes());
    if let Some(s) = stream {
        f.extend_from_slice(b"stream\n");
        f.extend_from_slice(s);
        f.extend_from_slice(b"\nendstream\n");
    }
    f.extend_from_slice(b"endobj\n");
}

fn trailer(f: &mut Vec<u8>) {
    f.extend_from_slice(b"trailer\n<< /Size 20 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n");
}

/// Streams inflating a thousandfold: the planner's estimates and the memory they ask for stay
/// within what the data holds.
#[test]
fn ratio_of_a_thousand() {
    for n in [4usize << 20, 16 << 20] {
        // not all zeros: a repeating short line
        let data: Vec<u8> = (0..n).map(|i| if i % 61 == 0 { b'\n' } else { b'0' }).collect();
        let z = zlib(&data);
        assert!(z.len() * 100 < data.len(), "{} -> {}", data.len(), z.len());
        let mut f = b"%PDF-1.7\n".to_vec();
        obj(&mut f, 1, "<< /Type /Catalog >>", None);
        obj(&mut f, 2, &format!("<< /Filter /FlateDecode /Length {} >>", z.len()), Some(&z));
        trailer(&mut f);
        check("1000:1", &f);
    }
    // a stream of zeros, the extreme of DEFLATE (about 1032:1)
    let z = zlib(&vec![0u8; 8 << 20]);
    let mut f = b"%PDF-1.7\n".to_vec();
    obj(&mut f, 1, &format!("<< /Filter /FlateDecode /Length {} >>", z.len()), Some(&z));
    trailer(&mut f);
    check("zeros", &f);
}

fn hex(d: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = d.iter().flat_map(|b| format!("{b:02x}").into_bytes()).collect();
    s.push(b'>');
    s
}

#[test]
fn filter_chains() {
    let data = content(3, 800);
    let z = zlib(&data);
    let zz = zlib(&z);
    let mut f = b"%PDF-1.7\n".to_vec();
    obj(&mut f, 1, "<< /Type /Catalog >>", None);
    let mut n = 2;
    let mut add = |f: &mut Vec<u8>, filter: &str, extra: &str, s: &[u8]| {
        obj(f, n, &format!("<< /Filter {filter} {extra} /Length {} >>", s.len()), Some(s));
        n += 1;
    };
    add(&mut f, "[/FlateDecode]", "", &z);
    add(&mut f, "[/ASCIIHexDecode /FlateDecode]", "", &hex(&z));
    add(&mut f, "[/FlateDecode /FlateDecode]", "", &zz);
    add(&mut f, "[/FlateDecode /ASCIIHexDecode]", "", &hex(&z));
    add(&mut f, "[/FlateDecode /FlateDecode /FlateDecode]", "", &zlib(&zz));
    add(&mut f, "[]", "", &z);
    add(&mut f, "[/Fl]", "", &z);
    add(&mut f, "/Fl", "", &z);
    add(&mut f, "[/FlateDecode /DCTDecode]", "", &z);
    add(&mut f, "[/FlateDecode]", "/DecodeParms [null]", &z);
    add(&mut f, "[/FlateDecode /FlateDecode]", "/DecodeParms [null << /Predictor 12 /Columns 4 >>]", &zz);
    add(&mut f, "[/FlateDecode]", "/DecodeParms [<< /Predictor 15 /Columns 100000000 >>]", &z);
    add(&mut f, "[/FlateDecode]", "/DecodeParms [<< /Predictor 2 /Colors 9999 /BitsPerComponent 16 >>]", &z);
    add(&mut f, "[/FlateDecode]", "/DecodeParms 7", &z);
    add(&mut f, "[/FlateDecode /FlateDecode]", "/DecodeParms [<< >>]", &zz);
    add(&mut f, "[/FlateDecode [/FlateDecode]]", "", &zz);
    add(&mut f, "[/Crypt /FlateDecode]", "/DecodeParms [<< /Name /Identity >> null]", &z);
    trailer(&mut f);
    check("chains", &f);
}

/// A dictionary longer than the look-back from `stream` (64 KiB), by a long string, a long
/// array, and many keys; the stream is no stream then, but the file comes back.
#[test]
fn dictionaries_past_the_look_back() {
    let z = zlib(&content(4, 500));
    for len in [60_000usize, 65_400, 65_536, 65_700, 200_000] {
        let pad = "7 ".repeat(len / 2);
        for dict in [
            format!("<< /Filter /FlateDecode /Pad [{pad}] /Length {} >>", z.len()),
            format!("<< /Pad [{pad}] /Filter /FlateDecode /Length {} >>", z.len()),
            format!("<< /Filter /FlateDecode /S ({}) /Length {} >>", "x".repeat(len), z.len()),
        ] {
            let mut f = b"%PDF-1.7\n".to_vec();
            obj(&mut f, 1, "<< /Type /Catalog >>", None);
            obj(&mut f, 2, &dict, Some(&z));
            obj(&mut f, 3, &format!("<< /Filter /FlateDecode /Length {} >>", z.len()), Some(&z));
            trailer(&mut f);
            check(&format!("dict {len}"), &f);
        }
    }
    // thousands of nested dictionaries (and arrays) must not recurse without bound
    let nest =
        format!("<< /Filter /FlateDecode /A {}{} /Length {} >>", "<< /A ".repeat(5000), ">>".repeat(5000), z.len());
    let mut f = b"%PDF-1.7\n".to_vec();
    obj(&mut f, 1, &nest, Some(&z));
    obj(&mut f, 2, &format!("<< /Filter /FlateDecode /A {}{} >>", "[".repeat(20000), "]".repeat(20000)), Some(&z));
    trailer(&mut f);
    check("nesting", &f);
}

/// `/Length N 0 R` where an incremental update gives N another value than the first: the
/// stream's end by the direct values or the keyword must not mix them.
#[test]
fn indirect_length_redefined_by_an_update() {
    let z = zlib(&content(5, 600));
    let zl = z.len();
    for (first, second) in [(zl, zl + 5), (zl + 5, zl), (zl + 1000, zl - 3), (0, zl), (zl, 0), (zl, 99_999_999)] {
        let mut f = b"%PDF-1.7\n".to_vec();
        obj(&mut f, 1, "<< /Type /Catalog >>", None);
        obj(&mut f, 2, "<< /Filter /FlateDecode /Length 4 0 R >>", Some(&z));
        obj(&mut f, 4, &format!("{first}"), None);
        f.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        trailer(&mut f);
        // the update: the length object again, and streams whose length is the new one
        obj(&mut f, 4, &format!("{second}"), None);
        obj(&mut f, 5, "<< /Filter /FlateDecode /Length 4 0 R >>", Some(&z));
        obj(&mut f, 6, "<< /Filter /FlateDecode /Length 6 0 R >>", Some(&z));
        obj(&mut f, 7, "<< /Filter /FlateDecode /Length 7 0 R /Prev 0 >>", Some(&z));
        f.extend_from_slice(b"xref\n4 1\n0000000000 00000 n \n");
        trailer(&mut f);
        check(&format!("length {first}/{second}"), &f);
    }
    // a length that is negative, huge, fractional, or no number
    for l in ["-1", "99999999999999999999", "1.5", "1e3", "(12)", "/Name", "[1]", "<< >>", "true", "null"] {
        let mut f = b"%PDF-1.7\n".to_vec();
        obj(&mut f, 1, &format!("<< /Filter /FlateDecode /Length {l} >>"), Some(&z));
        trailer(&mut f);
        check(&format!("length {l}"), &f);
    }
}
