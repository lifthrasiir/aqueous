use aqr_formula::{Apply, Formula, Kind, Node};
use aqr_lha::{formula, lha};

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzh"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    v
}

/// Bytes rebuilt by the module in `f`.
fn rebuilt(f: &Formula) -> u64 {
    let mut n = 0;
    let mut count = |a: &Apply| {
        assert_eq!(a.module, formula::MODULE);
        n += a.len;
    };
    f.shared.iter().for_each(|s| count(s.apply().unwrap()));
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            count(a);
        }
    });
    n
}

#[test]
fn fixtures_rebuilt() {
    for (name, d) in fixtures() {
        let a = lha::parse(&d).unwrap();
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let Some(f) = formula::encode(&d, 2) else {
            // (nothing to rebuild: directories only)
            assert!(a.entries.iter().all(|e| e.data.is_empty()), "{name}: no formula");
            continue;
        };
        assert!(eval(&f) == d, "{name}");
        // every stream rebuilt by the module, the stored files their contents
        let streams: u64 = a
            .entries
            .iter()
            .filter(|e| e.method.starts_with(b"-lh") && e.method != *b"-lh0-")
            .map(|e| e.data.len() as u64)
            .sum();
        assert_eq!(rebuilt(&f), streams, "{name}");
        let kept = f.streams[0].data.len();
        let headers: usize = a.entries.iter().map(|e| e.header.len()).sum();
        assert_eq!(kept, headers + d.len() - a.entries.last().unwrap().data.end, "{name}");
    }
}

#[test]
fn contents_by_kind() {
    let d = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/unix_h0_lh5.lzh")).unwrap();
    let opt = formula::Options { threads: 2, kind: |_| Kind::Text, ..Default::default() };
    let e = formula::encode_with(&d, &opt).unwrap();
    assert_eq!(e.inflated, 18092);
    assert!(e.formula.streams.iter().any(|s| s.kind == Kind::Text && s.data.len() == 18092));
    assert!(eval(&e.formula) == d);
}

#[test]
fn limit_and_cut_archives() {
    let d = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/unix_lh6_long.lzh")).unwrap();
    // over the limit: kept as it is
    let opt = formula::Options { threads: 2, limit: 1000, ..Default::default() };
    assert!(formula::encode_with(&d, &opt).is_none());
    // cut anywhere: no formula, or one rebuilding what is there
    for n in (0..d.len()).step_by(997) {
        if let Some(f) = formula::encode(&d[..n], 2) {
            assert!(eval(&f) == d[..n], "{n}");
        }
    }
    // an archive with data after it
    let x = [&d[..], b"trailing bytes"].concat();
    let f = formula::encode(&x, 2).unwrap();
    assert!(eval(&f) == x);
    // (the end byte 0 is the archive's last)
    assert_eq!(d.last(), Some(&0));
    assert_eq!(formula::extent(&x), Some(d.len()));
}
