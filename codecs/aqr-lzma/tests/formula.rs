use aqr_formula::{Apply, Formula};
use aqr_lzma::formula::{self, MODULE};
use aqr_lzma::{alone, recons};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 2).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The fixtures of XZ Utils (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzma"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

#[test]
fn files_rebuilt() {
    for (name, d) in fixtures() {
        assert!(formula::starts(&d), "{name}");
        let f = formula::encode(&d, 1).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == d, "{name}");
        // with bytes after the stream, kept as they are
        let e = [&d[..], b"trailing"].concat();
        let f = formula::encode(&e, 1).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == e, "{name}");
    }
}

#[test]
fn not_lzma() {
    assert!(!formula::starts(b"hello, world, this is not an lzma file"));
    // other files of 10gb, by chance of a valid header: benchmarks/silesia/sao (properties 0, a
    // dictionary of 16 MiB, 4.3 TB of data; then what is no stream), and OpenOffice's biblio.dbt
    // (a dictionary of 4 bytes, which no encoder writes)
    let mut x = 1u64;
    let rest: Vec<u8> = (0..4096)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect();
    let sao = [&[0u8, 0, 0, 0, 1, 0, 0, 0, 0xb5, 0xf3, 3, 0, 0, 0][..], &rest].concat();
    let dbt = [&[0x4fu8, 4, 0, 0, 0][..], &[0; 16][..]].concat();
    assert!(!formula::starts(&sao) && !formula::starts(&dbt));
    let (_, mut d) = fixtures().remove(0);
    d[alone::HEADER] = 1;
    assert!(!formula::starts(&d) && formula::encode(&d, 1).is_none());
}

#[test]
fn streams_by_index() {
    let (mut data, mut records, mut lens, mut out) = (vec![], vec![], vec![], vec![]);
    for (_, d) in fixtures() {
        let h = alone::parse(&d).unwrap();
        let sp = recons::split(&d[alone::HEADER..], h.props, h.dict, h.size, 1 << 24).unwrap();
        lens.push((sp.data.len(), sp.record.len()));
        data.extend(&sp.data);
        records.extend(&sp.record);
        out.extend(&d[alone::HEADER..][..sp.used]);
    }
    let index = formula::index(&lens);
    assert!(formula::decode(&[], &[&data, &records, &index], out.len() as u64, 4).unwrap() == out);
    // an index that does not cut the inputs exactly, parameters, another length
    assert!(formula::decode(&[], &[&data, &records, &index[..index.len() - 1]], out.len() as u64, 1).is_err());
    assert!(formula::decode(&[], &[&data[1..], &records, &index], out.len() as u64, 1).is_err());
    assert!(formula::decode(&[0], &[&data, &records, &index], out.len() as u64, 1).is_err());
    assert!(formula::decode(&[], &[&data, &records, &index], out.len() as u64 - 1, 1).is_err());
    assert!(formula::decode(&[], &[&data, &records], out.len() as u64, 1).is_err());
}

fn eval_any(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        let out = match aqr_bcj::formula::decode(a.module, &a.params, inputs, a.len) {
            Some(r) => r.map_err(|e| e.to_string()),
            None if a.module == formula::LZMA2 => {
                formula::decode2(&a.params, inputs, a.len, 2).map_err(|e| e.to_string())
            }
            None => formula::decode(&a.params, inputs, a.len, 2).map_err(|e| e.to_string()),
        };
        out.map_err(aqr_formula::Error::new)
    })
}

/// The `.xz` fixtures (`scripts/gen-fixtures.py`), by name.
fn xz_fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "xz"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

#[test]
fn xz_files_rebuilt() {
    for (name, d) in xz_fixtures() {
        assert!(formula::starts_xz(&d), "{name}");
        assert_eq!(formula::extent_xz(&d), Some(d.len()), "{name}");
        // (no block: nothing to take)
        if name == "empty" {
            assert!(formula::encode_xz(&d, 1).is_none() && formula::data_xz(&d).is_none());
            continue;
        }
        let f = formula::encode_xz(&d, 2).unwrap_or_else(|| panic!("{name}"));
        assert!(eval_any(&f).unwrap() == d, "{name}");
        // the data, all of it in the formula
        let data = formula::data_xz(&d).unwrap_or_default();
        assert_eq!(f.streams[1].data.len(), data.len(), "{name}");
        if name == "blocks" {
            assert!(f.shared.len() == 1 && f.shared[0].sizes.len() > 2, "{name}");
        }
        // with bytes after it, kept as they are (and not taken as padding: zeros before them
        // are not the streams')
        let e = [&d[..], b"\0\0\0\0trailing"].concat();
        let x = formula::extent_xz(&e).unwrap();
        assert!(x <= d.len() && d[x..].iter().all(|&b| b == 0), "{name}");
        let f = formula::encode_xz(&e, 1).unwrap_or_else(|| panic!("{name}"));
        assert!(eval_any(&f).unwrap() == e, "{name}");
    }
}

#[test]
fn not_xz() {
    let (_, mut d) = xz_fixtures().into_iter().find(|f| f.0 == "crc64").unwrap();
    assert!(!formula::starts_xz(b"\xfd7zXZ\0\0\x04 not a header"));
    // a damaged block header: no block taken
    d[13] ^= 1;
    assert!(formula::starts_xz(&d) && formula::encode_xz(&d, 1).is_none());
}

#[test]
fn greedy_xz() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 27) as u8 + b'a').collect();
    for d in [&data[..], b"", b"x"] {
        let x = aqr_lzma::greedy::xz(d);
        assert_eq!(formula::extent_xz(&x), Some(x.len()));
        assert!(formula::data_xz(&x).unwrap_or_default() == d);
        let f = formula::encode_xz(&x, 1).unwrap();
        assert!(eval_any(&f).unwrap() == x);
    }
}

/// `.xz` files of every filter before LZMA2 (XZ Utils'): the data in the formula is that before
/// the filters, which their modules filter again.
#[test]
fn xz_filters() {
    let code = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/filters.bin")).unwrap();
    let fixtures: Vec<_> = xz_fixtures().into_iter().filter(|f| f.0.starts_with("filter-")).collect();
    assert_eq!(fixtures.len(), 11);
    for (name, d) in fixtures {
        let f = formula::encode_xz(&d, 1).unwrap_or_else(|| panic!("{name}"));
        assert!(eval_any(&f).unwrap() == d, "{name}");
        assert!(f.streams[1].data == code, "{name}: other data");
        assert!(formula::data_xz(&d).unwrap() == code, "{name}");
        let filters = f.applies().iter().filter(|a| a.module.name.starts_with("bcj")).count();
        assert_eq!(filters, if name == "filter-chain" { 2 } else { 1 }, "{name}");
    }
}
