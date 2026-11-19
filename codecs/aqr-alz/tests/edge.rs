//! Edge cases of the reading and the formula: damaged archives, headers lying about sizes, the
//! comment section.

use std::time::Duration;

use aqr_alz::{alz, formula};
use aqr_archive::Evaluate;
use aqr_formula::{Apply, Formula};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn eval(f: &Formula) -> Result<Vec<u8>, String> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 1).map_err(aqr_formula::Error::new))
        .map_err(|e| e.to_string())
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.alz", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Whatever `d` is, reading it and making its formula neither panic nor hang nor ask for what it
/// cannot hold, and a formula made rebuilds `d`.
fn check(d: &[u8]) {
    let _ = alz::parse(d);
    let _ = formula::extent(d);
    let _ = alz::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec));
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f).as_deref() == Ok(d), "the formula does not rebuild");
    }
    // (what a header's size reserves is within the decoding budget, nothing past it)
    let budget = formula::limit(d.len()) as usize + (1 << 20);
    assert!(aqr_testkit::most() <= budget, "an allocation of {} bytes", aqr_testkit::most());
}

/// Every fixture truncated at every byte near the headers and at a stride after.
#[test]
fn truncated() {
    for name in ["bzip2", "bzip2_dlz", "deflate_empty", "dir_tree", "enc_deflate", "enc_store", "zip20", "store_empty"]
    {
        let d = fixture(name);
        for n in (0..d.len()).filter(|&n| n < 160 || n % 61 == 0 || n + 40 > d.len()) {
            aqr_testkit::case(&format!("{name} cut at {n}"));
            check(&d[..n]);
        }
    }
}

#[test]
fn damaged_fixtures() {
    for name in ["bzip2", "bzip2_dlz", "deflate_empty", "dir_tree", "enc_deflate", "split_store"] {
        let d = fixture(name);
        aqr_testkit::damaged(&d, 120, 200, Duration::from_secs(20), check);
    }
}

/// Headers whose compressed and uncompressed sizes lie, of DEFLATE and bzip2 bodies: the huge
/// size is no allocation, and a formula made is still right.
#[test]
fn sizes_lying() {
    for name in ["bzip2", "bzip2_dlz", "deflate_empty", "normal"] {
        let d = fixture(name);
        let e = alz::parse(&d).unwrap().entries.into_iter().find(|e| matches!(e.method, Some(1 | 2))).unwrap();
        let desc = d[e.pos + 4 + 7];
        let width = [0, 1, 2, 0, 4, 0, 0, 0, 8][(desc >> 4) as usize];
        assert!(width > 0, "{name}");
        // sizes at pos + 4 + 9 + 6 (compressed, then uncompressed)
        let csize = e.pos + 4 + 9 + 6;
        let size = csize + width;
        for (what, at) in [("size", size), ("csize", csize)] {
            for v in [0u8, 1, 0x7f, 0xff] {
                let mut x = d.clone();
                x[at..at + width].fill(v);
                aqr_testkit::case(&format!("{name} {what} bytes {v:#x}"));
                check(&x);
                // the top byte alone
                let mut x = d.clone();
                x[at + width - 1] = v;
                check(&x);
            }
        }
    }
}

/// The comment section: between the entries and the central directory, however long it claims
/// to be and wherever it is cut.
#[test]
fn comment_section() {
    let mut d = b"ALZ\x01\x0a\0\0\0".to_vec();
    d.extend_from_slice(b"BLZ\x01\x03\x00\x10\0\0\0\0\x00\x00dir");
    let before = d.len();
    d.extend_from_slice(b"ELZ\x01\x00\x00\x00\x00\x02\x00hi\x01\x00\x00\x00\x00\x00");
    d.extend_from_slice(b"CLZ\x01");
    d.extend_from_slice(&[0; 8]);
    d.extend_from_slice(b"CLZ\x02");
    let a = alz::parse(&d).unwrap();
    assert_eq!((a.end, a.ended, a.entries.len()), (d.len(), true, 1));
    // a length running past the data, a section without its central directory, a chain of
    // lengths that never reaches one
    for n in before..d.len() {
        check(&d[..n]);
        let a = alz::parse(&d[..n]).unwrap();
        assert!(!a.ended || n == d.len());
    }
    let mut x = d.clone();
    x[before + 8..before + 10].copy_from_slice(&[0xff, 0xff]);
    assert_eq!(alz::parse(&x).unwrap().end, x.len());
    check(&x);
    // a comment before the first entry (past the header)
    let mut y = d[..8].to_vec();
    y.extend_from_slice(&d[before..]);
    check(&y);
}
