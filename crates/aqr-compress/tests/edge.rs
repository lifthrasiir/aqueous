//! Edge cases of detection and dispatch: data cut right after its magic, a ZIP archive of an
//! empty payload or one in another cut short, and files damaged where their headers are. Whatever
//! the data, it is archived and given back, none of it a panic or a hang.

use std::time::Duration;

use aqr_compress::{Options, compress};
use aqr_decompress::decompress;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// `d` archived and given back.
fn given_back(d: &[u8]) {
    let c = compress(d, &Options::default());
    let back = decompress(&c, 1).unwrap_or_else(|e| panic!("{} bytes not decompressed: {e:?}", d.len()));
    assert!(back == d, "{} bytes not given back", d.len());
}

fn gzip(d: &[u8]) -> Vec<u8> {
    let mut z = flate2::GzBuilder::new().filename("a.txt").write(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, d).unwrap();
    z.finish().unwrap()
}

fn text(n: usize) -> Vec<u8> {
    (0..n).flat_map(|i| format!("line {} of the text, which repeats\n", i * 7 % 1000).into_bytes()).collect()
}

/// A stored ZIP archive of `files` (name, data), headers and a central directory.
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut d, mut cd) = (Vec::new(), Vec::new());
    for &(name, data) in files {
        let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        h.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&[0, 0]);
        cd.extend_from_slice(b"PK\x01\x02\x14\x03");
        cd.extend_from_slice(&h);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
        d.extend_from_slice(b"PK\x03\x04");
        d.extend_from_slice(&h);
        d.extend_from_slice(name.as_bytes());
        d.extend_from_slice(data);
    }
    let (at, n) = (d.len() as u32, files.len() as u16);
    d.extend_from_slice(&cd);
    d.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    d.extend_from_slice(&n.to_le_bytes());
    d.extend_from_slice(&n.to_le_bytes());
    d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    d.extend_from_slice(&at.to_le_bytes());
    d.extend_from_slice(&[0, 0]);
    d
}

/// Every format that a signature tells, cut at each place of its first bytes (the signature
/// alone, with its first fields) and beyond: given back.
#[test]
fn cut_after_the_magic() {
    let gz = gzip(&text(2000));
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("gzip", gz),
        ("xz", fixture("codecs/aqr-lzma/tests/data/blocks.xz")),
        ("lzma", fixture("codecs/aqr-lzma/tests/data/one-p6.lzma")),
        ("7z", fixture("codecs/aqr-7z/tests/data/copy.7z")),
        ("cab", fixture("codecs/aqr-mszip/tests/data/fixed.cab")),
        ("rar", fixture("codecs/aqr-rar/tests/data/r4_m3.rar")),
        ("alz", fixture("codecs/aqr-alz/tests/data/normal.alz")),
        ("egg", fixture("codecs/aqr-egg/tests/data/small_kor.egg")),
        ("zip", fixture("codecs/aqr-zip/tests/data/py-deflate.zip")),
    ];
    for (name, d) in &samples {
        for cut in (0..=d.len().min(80)).chain([d.len() / 2, d.len() - 1]) {
            aqr_testkit::within(Duration::from_secs(60), || given_back(&d[..cut]))
                .unwrap_or_else(|e| panic!("{name} cut at {cut}: {e}"));
        }
    }
}

/// A ZIP archive of one empty file (the signature, a header and no payload), with and without
/// its central directory; and the signature of a local header alone.
#[test]
fn zip_of_an_empty_payload() {
    let whole = zip(&[("empty", b"")]);
    given_back(&whole);
    // (the local header and its name only: no central directory)
    let local = &whole[..30 + "empty".len()];
    given_back(local);
    given_back(&whole[..whole.len() - 22]);
    given_back(b"PK\x03\x04");
    given_back(b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0");
    // (empty files among others)
    given_back(&zip(&[("a", b""), ("b", &text(300)), ("c", b""), ("d", b"")]));
    // (only the end of central directory)
    given_back(&zip(&[])[..]);
}

/// A ZIP archive in another, the inner one cut at each of several places (the entry's own
/// sizes and checksum are of the cut data).
#[test]
fn zip_in_zip_inner_cut() {
    let inner = zip(&[("a.txt", &text(400)), ("b.txt", &text(200))]);
    for cut in [4, 30, 31, 34, 60, inner.len() / 2, inner.len() - 23, inner.len() - 22, inner.len() - 1] {
        let outer = zip(&[("in.zip", &inner[..cut]), ("x.txt", b"after")]);
        given_back(&outer);
        // (the outer cut too, within the inner's bytes)
        given_back(&outer[..30 + "in.zip".len() + cut / 2]);
    }
    // (an inner archive whose central directory is that of the outer's: shared tail)
    let outer = zip(&[("in.zip", &inner)]);
    given_back(&outer);
}

/// The headers of each format damaged at every place of them, and the data at some.
#[test]
fn damaged_headers() {
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("gzip", gzip(&text(300))),
        ("xz", fixture("codecs/aqr-lzma/tests/data/blocks.xz")),
        ("7z", fixture("codecs/aqr-7z/tests/data/copy.7z")),
        ("cab", fixture("codecs/aqr-mszip/tests/data/fixed.cab")),
        ("rar", fixture("codecs/aqr-rar/tests/data/r4_m3.rar")),
        ("alz", fixture("codecs/aqr-alz/tests/data/normal.alz")),
        ("egg", fixture("codecs/aqr-egg/tests/data/small_kor.egg")),
        ("zip", zip(&[("a.txt", &text(100)), ("b.txt", b"")])),
    ];
    for (name, d) in &samples {
        let d = if d.len() > 4096 { &d[..4096] } else { &d[..] };
        std::panic::catch_unwind(|| {
            aqr_testkit::damaged(d, 8, 48, Duration::from_secs(60), given_back);
        })
        .unwrap_or_else(|_| panic!("{name} failed"));
    }
}
