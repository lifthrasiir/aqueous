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
    formula::encode_with(d, &Options { file_systems: &[aqr_ext::FS], kind: |_| Kind::Unknown, ..Default::default() })
}

/// The files of `d` read, by name, with their contents and their extents.
fn read(d: &[u8]) -> Vec<(String, Vec<u8>, usize)> {
    let mut out: Vec<_> = aqr_ext::files(&aqr_disk::Plain(d))
        .into_iter()
        .map(|f| {
            let data = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
            (String::from_utf8(f.name).unwrap(), data, f.extents.len())
        })
        .collect();
    out.sort();
    out
}

/// The files `scripts/gen-fixtures.sh` wrote that are not inline, by name, with their contents as
/// read (of a sparse file, its blocks of data; of one preallocated, the block written), of a
/// volume of blocks of `block` bytes.
fn expected(name: &str, block: usize) -> Vec<(String, Vec<u8>)> {
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect();
    let mut out = vec![
        ("readme.txt".to_string(), b"Aqueous ext fixture\n".repeat(40)),
        ("empty.txt".into(), Vec::new()),
        ("sparse.bin".into(), [vec![0x11; block], vec![0x22; block]].concat()),
        ("photo.jpg".into(), jpeg),
        ("frag.bin".into(), (0..block as u32 * 250 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    // (a hard link: one file, of the name found first)
    let linked = read(&fixture(name)).iter().any(|g| g.0 == "link.bin");
    out.push((if linked { "link.bin" } else { "bin/data.bin" }.into(), data));
    out.extend((0..600).step_by(2).map(|i| (format!("gap.{i:03}"), vec![(i % 251) as u8; block])));
    match name {
        "ext4.img" => {
            out.push(("prealloc.bin".into(), vec![0x33; block]));
            out.extend((0..400).map(|i| {
                (format!("many/entry with a longer name {i:03}.txt"), format!("line {i}\n").repeat(40).into())
            }));
        }
        _ => {
            if name == "ext4m.img" {
                out.extend((0..40).map(|k| (format!("top{k:02}/f.bin"), vec![k as u8; 3000])));
            }
            out.push(("big.bin".into(), (0..block as u32 * 300).map(|i| ((i * 3 + (i >> 10)) % 241) as u8).collect()));
            out.push(("docs/한글.txt".into(), "물 aqueous".as_bytes().to_vec()));
            out.push(("docs/deeper/a rather long file name.json".into(), br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()));
        }
    }
    out
}

const FIXTURES: [(&str, usize); 3] = [("ext4.img", 4096), ("ext2.img", 1024), ("ext4m.img", 1024)];

#[test]
fn files_found() {
    for (name, block) in FIXTURES {
        let d = fixture(name);
        assert!(aqr_ext::starts(&aqr_disk::Plain(&d)), "{name}");
        let got = read(&d);
        for (want, data) in expected(name, block) {
            let Some(g) = got.iter().find(|g| g.0 == want) else { panic!("{name}: {want} not found") };
            assert!(g.1 == data, "{name}: {want}");
        }
        // inline files, symbolic links, directories: not files with contents
        for not in ["link.sym", "bin", "docs"] {
            assert!(!got.iter().any(|g| g.0 == not), "{name}: {not}");
        }
        if name == "ext4.img" {
            assert!(!got.iter().any(|g| g.0.starts_with("docs/")), "{name}: inline");
        }
        // (ext4 allocating on a large volume: frag.bin not in the gaps)
        if name != "ext4m.img" {
            assert!(got.iter().find(|g| g.0 == "frag.bin").unwrap().2 > 100, "{name}");
        }
        let f = encode(&d).unwrap();
        assert!(f.eval(&none).unwrap() == d, "{name}");
        assert!(!f.shared.is_empty(), "{name}");
    }
}

#[test]
fn not_ext() {
    assert!(!aqr_ext::starts(&aqr_disk::Plain(&[0; 4096])));
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(!aqr_ext::starts(&aqr_disk::Plain(&noise)));
    for other in ["aqr-fat/tests/data/fat32.img.zst", "aqr-ntfs/tests/data/ntfs.img.zst"] {
        let z = std::fs::read(format!("{}/../{other}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(!aqr_ext::starts(&aqr_disk::Plain(&zstd::stream::decode_all(&z[..]).unwrap())), "{other}");
    }
}

/// Volumes cut short or with bytes changed: no panic, and a formula, where there is one, gives the
/// volume back.
#[test]
fn damaged() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (name, _) in FIXTURES {
        let d = fixture(name);
        // (the superblock, the descriptors, the inode tables and directories, mostly)
        let meta: Vec<usize> = (1024..d.len()).filter(|&i| d[i] != 0).take(30000).collect();
        for cut in [d.len() / 2, *meta.last().unwrap(), 70_000, 4096, 1500] {
            let d = &d[..cut.min(d.len())];
            if let Some(f) = encode(d) {
                assert!(f.eval(&none).unwrap() == d, "{name} cut at {cut}");
            }
        }
        for _ in 0..300 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let at = meta[(next() % meta.len() as u64) as usize];
                e[at] = next() as u8;
            }
            if let Some(f) = encode(&e) {
                assert!(f.eval(&none).unwrap() == e, "{name} changed");
            }
        }
    }
}

/// meta_bg: the descriptors of groups 16 on in group 16 (ext4m.img), where debugfs tells their
/// inode tables are; the files whose inodes are there (to 703, of 40 a group) found by
/// [`files_found`].
#[test]
fn meta_groups() {
    let tables = aqr_ext::inode_tables(&aqr_disk::Plain(&fixture("ext4m.img")));
    assert_eq!(tables.len(), 20);
    assert_eq!(tables[15..], [185, 131082, 131092, 131102, 131112]);
}
