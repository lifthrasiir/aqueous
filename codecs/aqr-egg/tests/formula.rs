use aqr_egg::{egg, formula};
use aqr_formula::{Apply, Formula, Kind, Node};

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "egg"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert_eq!(v.len(), 10);
    v
}

fn fixture(name: &str) -> Vec<u8> {
    fixtures().into_iter().find(|f| f.0 == name).unwrap().1
}

/// Bytes of the blocks of `f` rebuilt by each module (aqr-flate's, aqr-bz2's, aqr-lzma's,
/// aqr-azo's).
fn rebuilt(f: &Formula) -> [u64; 4] {
    let mut by = [0; 4];
    let mut count = |a: &Apply| match a.module {
        m if m == aqr_flate::formula::MODULE => by[0] += a.len,
        m if m == aqr_bz2::formula::MODULE => by[1] += a.len,
        m if m == aqr_lzma::formula::MODULE => by[2] += a.len,
        m if m == aqr_azo::formula::MODULE => by[3] += a.len,
        _ => panic!("{:?}", a.module),
    };
    f.shared.iter().for_each(|s| count(&s.apply));
    f.root.visit(&mut |n| {
        if let Node::Apply(a) = n {
            count(a);
        }
    });
    by
}

/// A bzip2 stream of `data` at `level`, as libbzip2 writes it.
fn bzip2(data: &[u8], level: u8) -> Vec<u8> {
    aqr_bz2::compress::stream(level, &aqr_bz2::compress::blocks(data, level, 1))
}

#[test]
fn fixtures_rebuilt() {
    for (name, d) in fixtures() {
        let a = egg::parse(&d).unwrap();
        assert_eq!(formula::extent(&d), Some(d.len()), "{name}");
        let blocks = || a.entries.iter().flat_map(|e| e.blocks.iter().map(move |b| (b, e.encrypted)));
        let Some(f) = formula::encode(&d, 2) else {
            // (nothing to rebuild: every block encrypted)
            assert!(blocks().all(|(_, enc)| enc), "{name}: no formula");
            continue;
        };
        assert!(eval(&f) == d, "{name}");
        // every block not encrypted rebuilt, but tiny DEFLATE blocks, whose records are no
        // shorter
        let body = |m: u8, min: usize| -> u64 {
            let b = blocks().filter(|(b, enc)| b.method == m && !enc && b.body.len() >= min);
            b.map(|(b, _)| b.body.len() as u64).sum()
        };
        let [flate, bz2, lzma, azo] = rebuilt(&f);
        assert_eq!(azo, body(egg::AZO, 0), "{name}");
        assert!((body(egg::DEFLATE, 32)..=body(egg::DEFLATE, 0)).contains(&flate), "{name}: {flate}");
        assert_eq!(bz2, 0, "{name}");
        assert_eq!(lzma, body(egg::LZMA, 0) - 9 * blocks().filter(|b| b.0.method == egg::LZMA).count() as u64);
        let kept = f.streams[0].data.len();
        eprintln!("{name}: {} B, {} files, {kept} B kept", d.len(), a.entries.len());
    }
    // the name after its locale
    let d = fixture("small_kor");
    let a = egg::parse(&d).unwrap();
    assert_eq!(&d[a.entries[0].name.clone()], b"\xc0\xdb\xc0\xba.txt");
}

/// A solid archive's block holds the files before it: their contents are sliced by their
/// sizes, each of the kind its name tells.
#[test]
fn solid_contents_by_kind() {
    let d = fixture("solid");
    let a = egg::parse(&d).unwrap();
    assert!(a.solid && a.entries.len() == 2 && a.entries[0].blocks.is_empty());
    let opt = formula::Options {
        kind: |n| if n.ends_with(b"2.txt") { Kind::Compressed } else { Kind::Unknown },
        ..Default::default()
    };
    let f = formula::encode_with(&d, &opt, &mut |_, _, _| true).unwrap().formula;
    assert!(eval(&f) == d);
    let size = |c: &str| f.streams.iter().filter(|s| s.class == c).map(|s| s.data.len() as u64).sum::<u64>();
    assert_eq!(size("egg/compressed"), a.entries[1].size);
    assert_eq!(size("raw") + size("egg/unknown"), a.entries[0].size);
}

/// bzip2 blocks (standard streams) by `bz2-recons`, stored blocks as they are, among blocks of
/// other methods kept.
#[test]
fn bzip2_and_stored() {
    let text: Vec<u8> = (0..20_000u32).flat_map(|i| format!("{} ", i * i % 1013).into_bytes()).collect();
    let (b9, b1) = (bzip2(&text, 9), bzip2(&text[..5000], 1));
    let files: [(&str, u8, &[u8], u32); 4] = [
        ("a.txt", egg::BZIP2, &b9, text.len() as u32),
        ("b.bin", egg::STORED, b"stored", 6),
        ("c.azo", egg::AZO, b"not taken", 99),
        ("d.txt", egg::BZIP2, &b1, 5000),
    ];
    let d = egg::write(false, &files);
    let f = formula::encode(&d, 2).unwrap();
    assert!(eval(&f) == d);
    assert_eq!(rebuilt(&f), [0, (b9.len() + b1.len()) as u64, 0, 0]);
    // turned down where it would not pay; refused where the size is not the data's
    let e = formula::encode_with(&d, &formula::Options::default(), &mut |_, _, _| false).unwrap();
    assert_eq!(e.rebuilt, [false, true, false, false]);
    let bad = egg::write(false, &[("a", egg::BZIP2, &b1, 4999)]);
    assert!(formula::encode(&bad, 1).is_none());
}

#[test]
fn limits_and_refusals() {
    let d = fixture("lzma_simple");
    // nothing decoded past the limit: the large block kept, the rest rebuilt
    let a = egg::parse(&d).unwrap();
    let sizes: Vec<u64> = a.entries.iter().flat_map(|e| &e.blocks).map(|b| b.size as u64).collect();
    let small = *sizes.iter().min().unwrap();
    assert!(sizes.iter().any(|&s| s > small));
    let opt = formula::Options { limit: small, ..Default::default() };
    let e = formula::encode_with(&d, &opt, &mut |_, _, _| true).unwrap();
    assert_eq!(e.inflated, small);
    assert!(eval(&e.formula) == d);
    // only encrypted blocks: nothing to rebuild
    assert!(formula::encode(&fixture("lorem_long_zipEnc"), 1).is_none());
    // cut short, anywhere: rebuilt or refused, never wrong
    for n in (0..d.len()).step_by(97) {
        let d = &d[..n];
        if let Some(f) = formula::encode(d, 1) {
            assert!(eval(&f) == d, "{n}");
        }
    }
    // not an archive
    assert!(formula::encode(&d[..14], 1).is_none());
    assert!(formula::encode(&d[4..], 1).is_none());
}

/// Found in other data: the archive ends with its final end marker.
#[test]
fn extent_in_data() {
    let d = fixture("extattr");
    let mut x = d.clone();
    x.extend_from_slice(b"PK\x03\x04 and the rest");
    assert_eq!(formula::extent(&x), Some(d.len()));
    let f = formula::encode(&x, 1).unwrap();
    assert!(eval(&f) == x);
}
