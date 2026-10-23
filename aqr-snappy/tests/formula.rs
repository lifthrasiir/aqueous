use aqr_formula::{Apply, Formula, Node};
use aqr_snappy::formula::{self, MODULE};
use aqr_snappy::leveldb;
use aqr_snappy::snappy::{self, Hash};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The fixtures of Snappy 1.2.2 (`scripts/gen-fixtures.py`), by name, with their hash.
fn fixtures(ext: &str) -> Vec<(String, Hash, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .map(|p| {
            let name = p.file_stem().unwrap().to_str().unwrap().to_owned();
            let hash = if name.ends_with("-mul") { Hash::Mul } else { Hash::Crc };
            (name, hash, std::fs::read(&p).unwrap())
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!v.is_empty());
    v
}

fn application(f: &Formula) -> &Apply {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    a
}

#[test]
fn streams_of_snappy() {
    for (name, hash, s) in fixtures("snappy") {
        let d = snappy::decode(&s, 1 << 20).unwrap();
        assert!(snappy::compress(&d, hash).unwrap() == s, "{name}: compressed otherwise");
        let found = formula::find_hash(&d, &s).unwrap_or_else(|| panic!("{name}: no hash"));
        // a stream both hashes make is found as the first
        assert!(found == hash || snappy::compress(&d, found).unwrap() == s, "{name}");
        let mut f = Formula::default();
        let data = f.push("raw", d);
        f.root = formula::stream(found, data, s.len() as u64);
        assert!(eval(&f).unwrap() == s, "{name}");
    }
    // the hashes tell apart on most data
    let (_, _, s) = fixtures("snappy").into_iter().find(|(n, _, _)| n == "small-mul").unwrap();
    let d = snappy::decode(&s, 1 << 20).unwrap();
    assert!(snappy::compress(&d, Hash::Crc).unwrap() != s);
}

#[test]
fn tables_of_snappy() {
    for (name, hash, t) in fixtures("ldb") {
        let blocks = leveldb::blocks(&t).unwrap();
        assert!(blocks.len() >= 5, "{name}");
        let f = formula::encode(&t, 1).unwrap_or_else(|| panic!("{name}: no formula"));
        assert!(eval(&f).unwrap() == t, "{name}");
        let a = application(&f);
        assert_eq!(a.params, [1, hash as u8], "{name}");
        // the block by the other hash is kept, as it is
        let kept = &f.streams[2].data;
        assert_eq!(kept.is_empty(), !name.starts_with("odd"), "{name}");
        if !kept.is_empty() {
            assert!(blocks.iter().any(|b| b.kind == leveldb::SNAPPY && t[b.offset..b.offset + b.size] == kept[..]));
        }
    }
}

/// A table is turned down where its structure does not hold, or nothing in it is rebuilt.
#[test]
fn turned_down() {
    let (_, _, t) = fixtures("ldb").into_iter().find(|(n, _, _)| n == "table-mul").unwrap();
    assert!(leveldb::is_table(&t));
    assert!(formula::encode(&t[1..], 1).is_none());
    assert!(!leveldb::is_table(&t[..t.len() - 1]));
    let mut bad = t.clone();
    bad[10] ^= 1; // the masked CRC no longer holds
    assert!(formula::encode(&bad, 1).is_none());
    assert!(formula::encode(b"hello", 1).is_none());
    // a table of raw blocks only
    let blocks = leveldb::blocks(&t).unwrap();
    let mut layout = Vec::new();
    for _ in 0..2 {
        layout.extend_from_slice(&[4, leveldb::CODE_RAW]);
    }
    let raw = leveldb::build(b"\0\0\0\0\0\0\0\0", &layout, &[], Hash::Mul).unwrap();
    assert!(leveldb::is_table(&raw) && blocks.len() > 2);
    assert!(formula::encode(&raw, 1).is_none());
}

fn xorshift(r: &mut u64) -> u64 {
    *r ^= *r << 13;
    *r ^= *r >> 7;
    *r ^= *r << 17;
    *r
}

/// Damaged tables, streams and module inputs: errors or other output, never panics.
#[test]
fn corrupt_inputs() {
    let mut r = 99u64;
    for (name, _, t) in fixtures("ldb").into_iter().filter(|(n, _, _)| n.starts_with("table")) {
        for k in 0..60 {
            let mut t = t.clone();
            let i = xorshift(&mut r) as usize % t.len();
            if k % 4 == 0 {
                t.truncate(i);
            } else {
                t[i] ^= 1 << (xorshift(&mut r) % 8);
            }
            let _ = leveldb::is_table(&t);
            if let Some(f) = formula::encode(&t, 1) {
                assert!(eval(&f).unwrap() == t, "{name}: mutation {k}");
            }
        }
    }
    let data = b"0123456789".repeat(100);
    for params in [&[][..], &[0], &[0, 2], &[2, 0], &[0, 0, 0]] {
        assert!(formula::decode(params, &[&data], 100, 1).is_err(), "{params:?}");
    }
    assert!(formula::decode(&[0, 0], &[&data, &data], 100, 1).is_err());
    assert!(formula::decode(&[1, 0], &[&data], 100, 1).is_err());
    for layout in [&[][..], &[10, 0], &[10, 0, 10, 0], &[200, 1, 10, 0], &[4, 2], &[4, 3, 4, 0], &[0x80]] {
        assert!(formula::decode(&[1, 0], &[&data[..20], layout, b""], 100, 1).is_err(), "{layout:?}");
    }
    for _ in 0..3000 {
        let n = xorshift(&mut r) as usize % 64;
        let s: Vec<u8> = (0..n).map(|_| xorshift(&mut r) as u8).collect();
        let _ = snappy::decode(&s, 1 << 16);
        let _ = leveldb::blocks(&s);
    }
}

/// Round trips of the compressor through the decoder, over sizes around its thresholds.
#[test]
fn compressor_round_trips() {
    let mut r = 7u64;
    let mut src = Vec::new();
    while src.len() < 300_000 {
        let k = xorshift(&mut r);
        if k.is_multiple_of(5) && src.len() > 1000 {
            let from = src.len() - 1 - (k >> 8) as usize % 1000.min(src.len() - 1);
            for i in 0..(k >> 20) as usize % 300 {
                src.push(src[from + i]);
            }
        } else {
            src.push(b"aaaabbbcdef"[(k >> 30) as usize % 11]);
        }
    }
    for n in [0, 1, 14, 15, 16, 31, 32, 33, 255, 256, 257, 4096, 32768, 32769, 65535, 65536, 65537, 200_000] {
        for hash in Hash::ALL {
            let s = snappy::compress(&src[..n], hash).unwrap();
            assert!(snappy::decode(&s, n).unwrap() == src[..n], "{n} bytes, {hash:?}");
        }
    }
}

/// Speed of the compressor on blocks of 4 KiB, as in LevelDB, and on a whole stream
/// (`cargo test --release -p aqr-snappy --test formula -- --ignored --nocapture`).
#[test]
#[ignore]
fn bench_compress() {
    let (_, _, s) = fixtures("snappy").into_iter().find(|(n, _, _)| n == "fragments-mul").unwrap();
    let d = snappy::decode(&s, 1 << 20).unwrap().repeat(64);
    for hash in Hash::ALL {
        for (what, size) in [("4 KiB blocks", 4096), ("stream", d.len())] {
            let best = (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let n: usize = d.chunks(size).map(|c| snappy::compress(c, hash).unwrap().len()).sum();
                    std::hint::black_box(n);
                    t.elapsed().as_secs_f64()
                })
                .fold(f64::MAX, f64::min);
            println!("{hash:?}, {what}: {:.0} MB/s", d.len() as f64 / best / 1e6);
        }
    }
}
