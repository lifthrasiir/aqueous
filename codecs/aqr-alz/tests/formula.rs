use aqr_alz::{alz, formula};
use aqr_formula::{Apply, Formula, Node};

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "alz"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert_eq!(v.len(), 11);
    v
}

/// Bytes of the bodies of `f` rebuilt by each module (aqr-flate's, aqr-bz2's).
fn rebuilt(f: &Formula) -> (u64, u64) {
    let (mut flate, mut bz2) = (0, 0);
    let mut count = |a: &Apply| match a.module {
        m if m == aqr_flate::formula::MODULE => flate += a.len,
        m if m == aqr_bz2::formula::MODULE_ALZ => bz2 += a.len,
        _ => panic!("{:?}", a.module),
    };
    f.shared.iter().for_each(|s| count(&s.apply));
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            count(a);
        }
    });
    (flate, bz2)
}

#[test]
fn fixtures_rebuilt() {
    for (name, d) in fixtures() {
        let a = alz::parse(&d).unwrap();
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let Some(f) = formula::encode(&d, 2) else {
            // (nothing to rebuild: every body encrypted)
            assert!(a.entries.iter().all(|e| e.encrypted || e.body.is_empty()), "{name}: no formula");
            continue;
        };
        assert!(eval(&f) == d, "{name}");
        // every body not encrypted rebuilt, DEFLATE and bzip2 by their modules: but tiny DEFLATE
        // bodies, whose records are no shorter
        let body = |m: u8, min: usize| -> u64 {
            let e = a.entries.iter().filter(|e| e.method == Some(m) && !e.encrypted && e.body.len() >= min);
            e.map(|e| e.body.len() as u64).sum()
        };
        let (flate, bz2) = rebuilt(&f);
        assert!((body(alz::DEFLATE, 32)..=body(alz::DEFLATE, 0)).contains(&flate), "{name}: {flate}");
        assert_eq!(bz2, body(alz::BZIP2, 0), "{name}");
        let kept = f.streams[0].data.len();
        let encrypted: usize = a.entries.iter().filter(|e| e.encrypted).map(|e| e.body.len()).sum();
        eprintln!("{name}: {} B, {} entries, {kept} B kept ({encrypted} encrypted)", d.len(), a.entries.len());
    }
}

/// The contents by their kinds: a stream of their own for a kind told by the name.
#[test]
fn contents_by_kind() {
    let d = &fixtures().into_iter().find(|f| f.0 == "deflate_empty").unwrap().1;
    let opt = formula::Options {
        kind: |n| if n.ends_with(b".gz") { aqr_formula::Kind::Compressed } else { aqr_formula::Kind::Unknown },
        ..Default::default()
    };
    let f = formula::encode_with(d, &opt, &mut |_, _, _| true).unwrap().formula;
    assert!(eval(&f) == *d);
    assert!(
        f.streams.iter().any(|s| s.class == "alz/compressed" && s.data.len() == 95),
        "{:?}",
        f.streams.iter().map(|s| (&s.class, s.data.len())).collect::<Vec<_>>()
    );
}

#[test]
fn limits_and_refusals() {
    let fx = fixtures();
    let get = |n: &str| &fx.iter().find(|f| f.0 == n).unwrap().1;
    let normal = get("normal");
    // nothing decoded past the limit: the large entry kept, the rest rebuilt
    let opt = formula::Options { limit: 10_000, ..Default::default() };
    let e = formula::encode_with(normal, &opt, &mut |_, _, _| true).unwrap();
    assert!(e.inflated <= 10_000 && e.inflated > 0);
    assert!(eval(&e.formula) == *normal);
    // bzip2 turned down where it would not pay
    let bz = get("bzip2");
    let f = formula::encode_with(bz, &formula::Options::default(), &mut |_, _, _| false);
    assert!(f.is_none());
    // only encrypted bodies: nothing to rebuild
    assert!(formula::encode(get("enc_store"), 1).is_none());
    // cut short, anywhere: rebuilt or refused, never wrong
    for n in (0..normal.len()).step_by(997) {
        let d = &normal[..n];
        if let Some(f) = formula::encode(d, 1) {
            assert!(eval(&f) == d, "{n}");
        }
    }
    // not an archive
    assert!(formula::encode(b"ALZ\x01\x0a\0\0\0", 1).is_none());
    assert!(formula::encode(&normal[8..], 1).is_none());
}

/// Found in other data: the archive ends with its end record.
#[test]
fn extent_in_data() {
    let d = &fixtures().into_iter().find(|f| f.0 == "bzip2").unwrap().1;
    let mut x = d.clone();
    x.extend_from_slice(b"PK\x03\x04 and the rest");
    assert_eq!(formula::extent(&x), Some(d.len()));
    let f = formula::encode(&x, 1).unwrap();
    assert!(eval(&f) == x);
}
