use aqr_7z::formula::{self, Options};
use aqr_formula::{Apply, Formula, Kind, Node};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        if let Some(r) = aqr_bcj::formula::decode(a.module, &a.params, inputs, a.len) {
            return r.map_err(|e| aqr_formula::Error::new(e.to_string()));
        }
        let decode = match a.module.name {
            "lzma-recons" => aqr_lzma::formula::decode,
            "lzma2-recons" => aqr_lzma::formula::decode2,
            m => panic!("{m}"),
        };
        decode(&a.params, inputs, a.len, 2).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The archives of 7-Zip 26 (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "7z"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

fn kind(name: &[u8]) -> Kind {
    if name.ends_with(b".txt") { Kind::Text } else { Kind::Unknown }
}

/// The bytes of every slice of the formula of at least `min` bytes.
fn slices(f: &Formula, min: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    f.visit(&mut |n| {
        if let &Node::Slice { stream, start, len } = n
            && len >= min
        {
            out.push(f.streams[stream].data[start as usize..][..len as usize].to_vec());
        }
    });
    out
}

#[test]
fn archives_rebuilt() {
    for (name, d) in fixtures().into_iter().filter(|f| !f.0.starts_with("filter-")) {
        assert!(formula::starts(&d) && formula::extent(&d) == Some(d.len()), "{name}");
        let opt = Options { threads: 2, kind, ..Options::default() };
        let f = formula::encode_with(&d, &opt).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == d, "{name}");
        // every file of 1 KiB or more a slice of its own (the PE file's in a folder of BCJ too)
        let sums = formula::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec));
        assert_eq!(sums.len(), 7, "{name}");
        let own: Vec<u32> = slices(&f, 1024).iter().map(|s| crc32fast::hash(s)).collect();
        let found = sums.iter().filter(|&&(c, n)| n >= 1024 && own.contains(&c)).count();
        let large = sums.iter().filter(|s| s.1 >= 1024).count();
        assert_eq!(found, large, "{name}");
        // text by its name
        let text: usize = f.streams.iter().filter(|s| s.kind == Kind::Text).map(|s| s.data.len()).sum();
        assert!(text >= 2 * 32768, "{name}");
        if name == "copy" {
            // (but the encoded header's)
            assert!(f.applies().len() == 1 && f.streams.iter().any(|s| s.content), "{name}");
        } else {
            assert!(!f.applies().is_empty(), "{name}");
        }
    }
}

/// Archives of a PE file and a text through 7-Zip's other filters: rebuilt from the files.
#[test]
fn filters_rebuilt() {
    let fixtures: Vec<_> = fixtures().into_iter().filter(|f| f.0.starts_with("filter-")).collect();
    assert_eq!(fixtures.len(), 6);
    for (name, d) in fixtures {
        let f = formula::encode(&d, 2).unwrap_or_else(|| panic!("{name}"));
        assert!(eval(&f).unwrap() == d, "{name}");
        let sums = formula::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec));
        let own: Vec<u32> = slices(&f, 1024).iter().map(|s| crc32fast::hash(s)).collect();
        assert!(sums.len() == 2 && sums.iter().all(|s| own.contains(&s.0)), "{name}");
        let module = if name == "filter-bcj2" { "bcj2" } else { "bcj-" };
        assert!(f.applies().iter().any(|a| a.module.name.starts_with(module)), "{name}");
    }
}

#[test]
fn encoded_header_rebuilt() {
    let (_, d) = fixtures().into_iter().find(|f| f.0 == "lzma2").unwrap();
    let f = formula::encode(&d, 1).unwrap();
    assert!(f.streams.iter().any(|s| s.class == "7z/header" && s.kind == Kind::Table));
    let (_, d) = fixtures().into_iter().find(|f| f.0 == "lzma2-plain").unwrap();
    let f = formula::encode(&d, 1).unwrap();
    assert!(!f.streams.iter().any(|s| s.class == "7z/header"));
}

#[test]
fn segments_of_blocks() {
    let (_, d) = fixtures().into_iter().find(|f| f.0 == "blocks").unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let index = f.streams.iter().find(|s| s.class == "lzma2/index").unwrap();
    assert!(!index.data.is_empty(), "several segments");
}

#[test]
fn not_7z() {
    assert!(!formula::starts(b"7z\xbc\xaf\x27\x1c\0\x04 but nothing more"));
    let (_, mut d) = fixtures().remove(0);
    // a header of another CRC-32
    let n = d.len();
    d[n - 3] ^= 1;
    assert!(formula::encode(&d, 1).is_none());
    // trailing bytes kept
    let (_, d) = fixtures().remove(0);
    let e = [&d[..], b"after"].concat();
    assert_eq!(formula::extent(&e), Some(d.len()));
    assert!(eval(&formula::encode(&e, 1).unwrap()).unwrap() == e);
}

#[test]
fn written_rebuilt() {
    let a: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 27) as u8 + b'a').collect();
    let files: [(&str, &[u8]); 4] = [("d/a.txt", &a), ("e", b""), ("d/b", b"hello"), ("d/c.txt", &a[1000..5000])];
    let d = aqr_7z::write::solid(&files);
    let f = formula::encode_with(&d, &Options { kind, ..Options::default() }).unwrap();
    assert!(eval(&f).unwrap() == d);
    let sums = formula::entry_sums(&mut |r| d.get(r.start as usize..r.end as usize).map(<[u8]>::to_vec));
    let want: Vec<(u32, u64)> =
        files.iter().filter(|f| !f.1.is_empty()).map(|f| (crc32fast::hash(f.1), f.1.len() as u64)).collect();
    assert_eq!(sums, want);
    // (all files but the empty one and the one of 5 bytes, apart)
    let own = slices(&f, 1024);
    assert!(own.contains(&a) && own.contains(&a[1000..5000].to_vec()));
    // an archive of an empty file alone: no folder to take
    assert!(formula::encode(&aqr_7z::write::solid(&[("e", b"")]), 1).is_none());
}
