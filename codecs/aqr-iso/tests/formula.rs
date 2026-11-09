use aqr_disk::formula::{self, Options, REST};
use aqr_formula::{Formula, Kind, Node};

/// The formula of image `d` (aqr-disk's, of aqr-iso's file system), its files of kinds by `kind`.
fn encode_with(d: &[u8], kind: fn(&[u8]) -> Kind) -> Option<Formula> {
    formula::encode_with(d, &Options { file_systems: &[aqr_iso::FS], kind })
}

fn encode(d: &[u8]) -> Option<Formula> {
    encode_with(d, |_| Kind::Unknown)
}

/// Fixture `name` of tests/data (`scripts/gen-fixtures.ps1`), decompressed.
fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

/// The output of `f`, of slices only.
fn eval(f: &Formula) -> Vec<u8> {
    let Node::Concat(parts) = &f.root else { panic!("not a concatenation") };
    let mut out = Vec::new();
    for n in parts {
        let &Node::Slice { stream, start, len } = n else { panic!("{n:?}") };
        out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize]);
    }
    out
}

/// What the fixtures hold (by `scripts/gen-fixtures.ps1`), by name.
fn expected() -> Vec<(&'static str, Vec<u8>)> {
    let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect();
    vec![
        ("bin/copy.bin", data.clone()),
        ("bin/data.bin", data),
        ("docs/deeper/a rather long file name.json", br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()),
        ("docs/한글.txt", "물 aqueous".as_bytes().to_vec()),
        ("readme.txt", b"Aqueous ISO fixture\r\n".repeat(40)),
    ]
}

/// The files of `d` taken, by name, with their contents.
fn taken(d: &[u8]) -> Vec<(String, Vec<u8>)> {
    let files = aqr_disk::files(d, &[aqr_iso::FS]);
    let mut out: Vec<_> = aqr_disk::pieces(d, &files)
        .into_iter()
        .filter_map(|p| Some((String::from_utf8(files[p.file?.0].name.clone()).unwrap(), d[p.span].to_vec())))
        .collect();
    out.sort();
    out
}

#[test]
fn files_of_each_tree() {
    let want = expected();
    for name in ["iso-joliet-udf102.iso", "joliet.iso", "udf201.iso"] {
        let d = fixture(name);
        let got = taken(&d);
        assert_eq!(got.len(), want.len(), "{name}");
        for ((gn, gd), (wn, wd)) in got.iter().zip(&want) {
            assert_eq!((gn.as_str(), gd), (*wn, wd), "{name}");
        }
        let f = encode(&d).expect(name);
        assert!(eval(&f) == d, "{name} not rebuilt");
    }
}

#[test]
fn trees_found() {
    let d = fixture("iso-joliet-udf102.iso");
    let files = aqr_iso::files(&aqr_disk::Plain(&d));
    // the empty file too, in each of the three trees
    for rank in 0..3 {
        assert_eq!(files.iter().filter(|f| f.rank == rank).count(), 6, "rank {rank}");
    }
    // ISO 9660's own names: level 1, upper case, versions dropped
    let d = fixture("iso.iso");
    let mut names: Vec<_> = taken(&d).into_iter().map(|t| t.0).collect();
    names.sort();
    assert_eq!(names[..2], ["BIN/COPY.BIN", "BIN/DATA.BIN"]);
    assert!(names.contains(&"README.TXT".to_string()));
    assert!(eval(&encode(&d).unwrap()) == d);
}

#[test]
fn contents_by_kind() {
    let d = fixture("udf201.iso");
    let kind = |name: &[u8]| match name.ends_with(b".txt") {
        true => Kind::Text,
        false => Kind::Unknown,
    };
    let f = encode_with(&d, kind).unwrap();
    assert!(eval(&f) == d);
    // (a stream of each file, those of a class merged in the order of their first)
    let mut classes: Vec<(&str, bool, usize)> = Vec::new();
    for s in &f.streams {
        match classes.iter_mut().find(|c| c.0 == s.class) {
            Some(c) => c.2 += s.data.len(),
            None => classes.push((&s.class, s.content, s.data.len())),
        }
    }
    let text = 840 + "물 aqueous".len();
    assert_eq!(classes, [("raw", true, 20026), (REST, false, d.len() - 20026 - text), ("disk/text", true, text)]);
    // a file a slice of its own, the whole of its stream
    let Node::Concat(parts) = &f.root else { unreachable!() };
    let files: Vec<_> = parts.iter().filter(|n| !matches!(n, Node::Slice { stream: 1, .. })).collect();
    assert_eq!(files.len(), 5);
    for n in files {
        let &Node::Slice { stream, start, len } = n else { panic!("a slice") };
        assert!(stream != 1 && start == 0 && len == f.streams[stream].data.len() as u64);
    }
    assert_eq!(f.streams.len(), 1 + 5);
}

/// An image without files of kind `Unknown`: stream 0, of `raw`, is empty, the rest stream 1.
#[test]
fn no_raw_contents() {
    let d = fixture("udf201.iso");
    let f = encode_with(&d, |_| Kind::Text).unwrap();
    assert!(eval(&f) == d);
    assert!(f.streams[0].class == "raw" && f.streams[0].data.is_empty());
    assert_eq!(f.streams[1].class, REST);
    assert!(f.streams[2..].iter().all(|s| s.class == "disk/text"));
}

/// UDF 2.50 keeps its files in a metadata partition, which is not read: no file is found.
#[test]
fn metadata_partition_not_read() {
    let d = fixture("udf250.iso");
    assert!(aqr_iso::starts(&aqr_disk::Plain(&d)));
    assert!(encode(&d).is_none());
}

#[test]
fn not_an_image() {
    assert!(encode(&[0; 40000]).is_none());
    assert!(encode(b"CD001").is_none());
}

/// Images cut short or with bytes changed: no panic, and a formula, where there is one, gives
/// the input back.
#[test]
fn damaged() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for name in ["iso-joliet-udf102.iso", "iso.iso", "joliet.iso", "udf201.iso", "udf250.iso"] {
        let d = fixture(name);
        // (where the descriptors and directories are, mostly)
        let meta: Vec<usize> = (0..d.len()).filter(|&i| d[i] != 0).take(20000).collect();
        for cut in [d.len() / 2, d.len() * 9 / 10, 16 * 2048 + 100, 257 * 2048 + 17] {
            let d = &d[..cut.min(d.len())];
            if let Some(f) = encode(d) {
                assert!(eval(&f) == d, "{name} cut at {cut}");
            }
        }
        for _ in 0..300 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let at = meta[(next() % meta.len() as u64) as usize];
                e[at] = next() as u8;
            }
            if let Some(f) = encode(&e) {
                assert!(eval(&f) == e, "{name} changed");
            }
        }
    }
}

/// An image on a partition (of a GPT, aqr-disk's fixture of hdiutil's): its files found there.
#[test]
fn on_a_partition() {
    let z = std::fs::read(format!("{}/../aqr-disk/tests/data/gpt.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut d = zstd::stream::decode_all(&z[..]).unwrap();
    let iso = fixture("udf201.iso");
    let at = 0x800 * 512;
    d[at..at + iso.len()].copy_from_slice(&iso);
    assert!(taken(&d) == taken(&iso) && taken(&d).len() == expected().len());
    assert!(eval(&encode(&d).unwrap()) == d);
}
