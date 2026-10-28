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
