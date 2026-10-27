use aqr_formula::{Apply, Formula};
use aqr_lzo::formula::{self, MODULE};
use aqr_lzo::{lzop, recons};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, 2).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The files of lzop (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzo"))
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
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let lz = lzop::parse(&d).unwrap();
        if lz.blocks.iter().all(|b| b.stored()) {
            // nothing to rebuild (an empty file)
            assert!(formula::encode(&d, 1).is_none(), "{name}");
            continue;
        }
        let f = formula::encode(&d, 2).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == d, "{name}");
        // the data of every block, stored ones too, is one stream, as the file holds it
        let raw = f.streams.iter().find(|s| s.class == "raw").unwrap();
        assert_eq!(raw.data.len(), lz.blocks.iter().map(|b| b.data).sum::<usize>(), "{name}");
        // with bytes after the file, kept as they are
        let e = [&d[..], b"trailing"].concat();
        let f = formula::encode(&e, 1).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == e, "{name}");
    }
}

#[test]
fn blocks_kept() {
    // a block whose stream does not split stays in the container: the first of two copies of
    // a block, damaged so that its stream ends otherwise
    let (_, d) = fixtures().into_iter().find(|(n, _)| n == "big.1").unwrap();
    let lz = lzop::parse(&d).unwrap();
    let (b0, b1) = (lz.blocks[0], lz.blocks[1]);
    assert!(!b0.stored() && b1.stored());
    let entry = &d[lz.header..b0.at + b0.stream];
    let mut bad = entry.to_vec();
    let n = bad.len();
    bad[n - 3] = 0x12;
    let e = [&d[..lz.header], &bad, entry, &d[lz.header + entry.len()..]].concat();
    assert_eq!(lzop::parse(&e).unwrap().blocks.len(), 3);
    let f = formula::encode(&e, 1).unwrap();
    assert!(eval(&f).unwrap() == e);
    let kept = lz.header + bad.len() + (entry.len() - b0.stream) + (b1.at - b0.at - b0.stream) + 4;
    assert_eq!(f.streams.iter().find(|s| s.class == "lzo").unwrap().data.len(), kept);
    // with that block alone, nothing
    let e = [&d[..lz.header], &bad, &d[lz.header + entry.len()..]].concat();
    assert!(formula::encode(&e, 1).is_none());
}

#[test]
fn not_lzop() {
    assert!(!formula::starts(b"hello, world, this is not an lzop file"));
    let (_, d) = fixtures().into_iter().find(|(n, _)| n == "mixed.9").unwrap();
    assert!(!formula::starts(&d[..d.len() - 1]) && formula::encode(&d[..d.len() - 1], 1).is_none());
    let mut e = d.clone();
    e[0] = 0x88;
    assert!(!formula::starts(&e));
    // a method other than LZO1X
    let mut e = d.clone();
    e[15] = 4;
    assert!(!formula::starts(&e));
}

#[test]
fn streams_by_index() {
    let (mut data, mut records, mut lens, mut out) = (vec![], vec![], vec![], vec![]);
    for (_, d) in fixtures() {
        for b in lzop::parse(&d).unwrap().blocks.iter().filter(|b| !b.stored()) {
            let sp = recons::split(&d[b.at..b.at + b.stream], b.data).unwrap();
            lens.push((sp.data.len(), sp.record.len()));
            data.extend(&sp.data);
            records.extend(&sp.record);
            out.extend(&d[b.at..b.at + b.stream]);
        }
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
