use aqr_disk::formula::{self, Options};
use aqr_formula::{Formula, Kind};

/// Fixture `name` of tests/data (`scripts/gen-fixtures.sh`), decompressed.
fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

fn encode(d: &[u8]) -> Option<Formula> {
    formula::encode_with(d, &Options { file_systems: &[aqr_apfs::FS], kind: |_| Kind::Unknown, ..Default::default() })
}

/// The files of `d` (a disk with the container in a partition) read, by name, with their
/// contents and their extents.
fn read(d: &[u8]) -> Vec<(String, Vec<u8>, Vec<std::ops::Range<usize>>)> {
    let mut out: Vec<_> = aqr_disk::files(d, &[aqr_apfs::FS])
        .into_iter()
        .map(|f| {
            let data = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
            (String::from_utf8(f.name).unwrap(), data, f.extents)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn files_found() {
    let d = fixture("apfs.img");
    let got = read(&d);
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect();
    let find = |n: &str| got.iter().find(|g| g.0 == n);
    let mut want = vec![
        ("readme.txt".to_string(), b"Aqueous APFS fixture\n".repeat(40)),
        ("docs/deeper/a rather long file name.json".into(), br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()),
        ("docs/한글.txt".into(), "물 aqueous".as_bytes().to_vec()),
        ("empty.txt".into(), Vec::new()),
        ("clone.bin".into(), data.clone()),
        // (APFS allocates 16 KiB for the first write: three blocks of zeros follow the data; the hole
        // between that extent and the second, at 256 KiB, is left out)
        ("sparse.bin".into(), [vec![0x11; 4096], vec![0; 12288], vec![0x22; 4096]].concat()),
        ("photo.jpg".into(), jpeg),
        ("frag.bin".into(), (0..4096u32 * 250 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    // (a hard link: one file, of the name found first)
    want.push((if find("link.bin").is_some() { "link.bin" } else { "bin/data.bin" }.into(), data));
    want.extend(
        (0..400)
            .map(|i| (format!("many/entry with a longer name {i:03}.txt"), format!("line {i}\n").repeat(40).into())),
    );
    want.extend((0..600).step_by(2).map(|i| (format!("gap.{i:03}"), vec![(i % 251) as u8; 4096])));
    for (name, data) in want {
        let Some(g) = find(&name) else { panic!("{name} not found") };
        assert!(g.1 == data, "{name}");
    }
    // the hard link is one file; the symbolic link and the directories are no files with contents
    assert_eq!(got.iter().filter(|g| g.0 == "link.bin" || g.0 == "bin/data.bin").count(), 1);
    for not in ["link.sym", "bin", "docs", "many"] {
        assert!(find(not).is_none(), "{not}");
    }
    // (the clone shares the original's blocks)
    let (clone, orig) = (find("clone.bin").unwrap(), find("link.bin").or(find("bin/data.bin")).unwrap());
    assert_eq!(clone.2, orig.2);
    // (the hole is left out)
    assert_eq!(find("sparse.bin").unwrap().1.len(), 16384 + 4096);
    // (the two extents lie next to each other in the container: one range)
    assert_eq!(find("sparse.bin").unwrap().2.len(), 1);
    // (decmpfs, UF_COMPRESSED: its data in the rest)
    assert!(find("packed.txt").is_none());
    let f = encode(&d).unwrap();
    assert!(f.eval(&none).unwrap() == d);
    assert!(!f.shared.is_empty());
}

/// Two volumes of a container: the files of both.
#[test]
fn two_volumes() {
    let d = fixture("apfs2.img");
    let got = read(&d);
    let find = |n: &str| got.iter().find(|g| g.0 == n);
    assert!(find("readme.txt").unwrap().1 == b"Aqueous APFS fixture\n");
    assert!(find("other.txt").unwrap().1 == b"the second volume\n");
    let f = encode(&d).unwrap();
    assert!(f.eval(&none).unwrap() == d);
}

#[test]
fn not_apfs() {
    assert!(!aqr_apfs::starts(&aqr_disk::Plain(&[0; 4096])));
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(!aqr_apfs::starts(&aqr_disk::Plain(&noise)));
    for other in
        ["aqr-fat/tests/data/fat32.img.zst", "aqr-ntfs/tests/data/ntfs.img.zst", "aqr-ext/tests/data/ext4.img.zst"]
    {
        let z = std::fs::read(format!("{}/../{other}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(!aqr_apfs::starts(&aqr_disk::Plain(&zstd::stream::decode_all(&z[..]).unwrap())), "{other}");
    }
    // (the container itself starts, in its partition)
    let d = fixture("apfs.img");
    assert!(aqr_apfs::starts(&aqr_disk::Sub { of: &aqr_disk::Plain(&d), span: 20480..d.len() }));
    assert!(aqr_disk::starts(&d, &[aqr_apfs::FS]));
}

/// Images cut short or with bytes changed: no panic, and a formula, where there is one, gives the
/// image back.
#[test]
fn damaged() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let d = fixture("apfs.img");
    // (the superblocks, checkpoints, object maps and trees, mostly)
    let meta: Vec<usize> = (20480..d.len().min(8 << 20)).filter(|&i| d[i] != 0).take(30000).collect();
    for cut in [d.len() / 2, *meta.last().unwrap(), 70_000, 24_576, 4096, 1500] {
        let d = &d[..cut.min(d.len())];
        let _ = aqr_disk::files(d, &[aqr_apfs::FS]);
        if let Some(f) = encode(d) {
            assert!(f.eval(&none).unwrap() == d, "cut at {cut}");
        }
    }
    // (also with the checksums made right again: the trees themselves damaged)
    for k in 0..600 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 4 {
            let at = meta[(next() % meta.len() as u64) as usize];
            e[at] = next() as u8;
        }
        let _ = aqr_disk::files(&e, &[aqr_apfs::FS]);
        // (the encoder is slower: some of the images)
        if k % 3 == 0
            && let Some(f) = encode(&e)
        {
            assert!(f.eval(&none).unwrap() == e, "changed");
        }
    }
}
