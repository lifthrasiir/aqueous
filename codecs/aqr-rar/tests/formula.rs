use aqr_formula::{Apply, Formula, Node};
use aqr_rar::{formula, rar4, rar5};

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "rar"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    v
}

/// Bytes rebuilt by the modules in `f`.
fn rebuilt(f: &Formula) -> u64 {
    let mut n = 0;
    let mut count = |a: &Apply| {
        assert!(a.module == formula::MODULE5 || a.module == formula::MODULE3);
        n += a.len;
    };
    f.shared.iter().for_each(|s| count(&s.apply));
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            count(a);
        }
    });
    n
}

/// The packed bytes of the compressed files of the archive `d`.
fn compressed(d: &[u8]) -> u64 {
    match rar5::parse(d) {
        Some(a) => a.entries.iter().filter(|e| !e.service && e.method != 0).map(|e| e.data.len() as u64).sum(),
        None => rar4::parse(d)
            .unwrap()
            .entries
            .iter()
            .filter(|e| !e.service && e.method != 0x30)
            .map(|e| e.data.len() as u64)
            .sum(),
    }
}

/// Every fixture rebuilt: each compressed stream by its module, stored files as contents.
#[test]
fn fixtures_rebuilt() {
    for (name, d) in fixtures() {
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let f = formula::encode(&d, 2).unwrap_or_else(|| panic!("{name}: no formula"));
        assert!(eval(&f) == d, "{name}");
        assert_eq!(rebuilt(&f), compressed(&d), "{name}");
        let module = if name.starts_with("r5") { formula::MODULE5 } else { formula::MODULE3 };
        let mut ok = true;
        f.root.visit(&mut |n| {
            if let Node::Apply(a) = n {
                ok &= a.module == module;
            }
        });
        assert!(ok, "{name}: another module");
    }
}

/// Archives cut short: a formula of what is there, or none; never another archive.
#[test]
fn cut_short() {
    for (name, d) in fixtures() {
        for n in (0..d.len()).step_by(997) {
            let x = &d[..n];
            if let Some(f) = formula::encode(x, 1) {
                assert!(eval(&f) == x, "{name} cut at {n}");
            }
        }
    }
}

/// Data after the archive is kept.
#[test]
fn data_after() {
    for (name, d) in fixtures() {
        let x = [&d[..], b"some data after"].concat();
        let f = formula::encode(&x, 1).unwrap();
        assert!(eval(&f) == x, "{name}");
    }
}

/// Over the limit, streams are kept as they are.
#[test]
fn budget() {
    for (name, d) in fixtures() {
        let o = formula::Options { threads: 1, limit: 0, ..Default::default() };
        if let Some(e) = formula::encode_with(&d, &o) {
            assert_eq!(e.inflated, 0, "{name}");
            assert_eq!(rebuilt(&e.formula), 0, "{name}");
            assert!(eval(&e.formula) == d, "{name}");
        }
    }
}
