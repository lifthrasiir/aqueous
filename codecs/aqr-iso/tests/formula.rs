use aqr_formula::{Formula, Kind, Node};
use aqr_iso::formula::{self, Options};

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
    let files = aqr_iso::files(d);
    let mut out: Vec<_> = aqr_iso::pieces(d, &files)
        .into_iter()
        .filter_map(|p| Some((String::from_utf8(files[p.file?].name.clone()).unwrap(), d[p.span].to_vec())))
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
        let f = formula::encode(&d, 1).expect(name);
        assert!(eval(&f) == d, "{name} not rebuilt");
    }
}

#[test]
fn trees_found() {
    let d = fixture("iso-joliet-udf102.iso");
    let files = aqr_iso::files(&d);
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
    assert!(eval(&formula::encode(&d, 1).unwrap()) == d);
}

#[test]
fn contents_by_kind() {
    let d = fixture("udf201.iso");
    let kind = |name: &[u8]| match name.ends_with(b".txt") {
        true => Kind::Text,
        false => Kind::Unknown,
    };
    let f = formula::encode_with(&d, &Options { kind }).unwrap();
    assert!(eval(&f) == d);
    let classes: Vec<_> = f.streams.iter().map(|s| (&s.class[..], s.content, s.data.len())).collect();
    let text = 840 + "물 aqueous".len();
    assert_eq!(
        classes,
        [("raw", true, 20026), (formula::REST, false, d.len() - 20026 - text), ("iso/text", true, text)]
    );
    // a file a slice of its own
    let Node::Concat(parts) = &f.root else { unreachable!() };
    assert_eq!(parts.iter().filter(|n| matches!(n, Node::Slice { stream: 0 | 2, .. })).count(), 5);
}

/// UDF 2.50 keeps its files in a metadata partition, which is not read: no file is found.
#[test]
fn metadata_partition_not_read() {
    let d = fixture("udf250.iso");
    assert!(aqr_iso::starts(&d));
    assert!(formula::encode(&d, 1).is_none());
}

#[test]
fn not_an_image() {
    assert!(formula::encode(&[0; 40000], 1).is_none());
    assert!(formula::encode(b"CD001", 1).is_none());
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
            if let Some(f) = formula::encode(d, 1) {
                assert!(eval(&f) == d, "{name} cut at {cut}");
            }
        }
        for _ in 0..300 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let at = meta[(next() % meta.len() as u64) as usize];
                e[at] = next() as u8;
            }
            if let Some(f) = formula::encode(&e, 1) {
                assert!(eval(&f) == e, "{name} changed");
            }
        }
    }
}

#[test]
// (one range is an extent of one range, not a range of indices)
#[allow(clippy::single_range_in_vec_init)]
fn pieces_overlapping_and_contiguous() {
    use aqr_iso::File;
    let d = vec![0u8; 100];
    let file = |name: &str, extents: &[std::ops::Range<usize>], rank| File {
        name: name.into(),
        extents: extents.to_vec(),
        rank,
    };
    let files = [
        // two contiguous extents: one piece
        file("a", &[10..20, 20..30], 2),
        // the same extent, of a lower rank: it names it
        file("b", &[10..30], 0),
        // overlapping the one before: left out
        file("c", &[25..40], 0),
        // past the end: cut
        file("d", &[90..120], 0),
        file("e", &[50..50], 0),
    ];
    let p: Vec<_> = aqr_iso::pieces(&d, &files).into_iter().map(|p| (p.span, p.file)).collect();
    assert_eq!(p, [(0..10, None), (10..30, Some(1)), (30..90, None), (90..100, Some(3))]);
}
