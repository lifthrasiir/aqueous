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
    formula::encode_with(d, &Options { file_systems: &[aqr_fat::FAT], kind: |_| Kind::Unknown, ..Default::default() })
}

/// The files the fixtures hold that `scripts/gen-fixtures.sh` wrote, by name, with their
/// contents (of a volume of clusters of `cluster` bytes).
fn expected(cluster: usize) -> Vec<(String, Vec<u8>)> {
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut out = vec![
        ("readme.txt".to_string(), b"Aqueous FAT fixture\r\n".repeat(40)),
        ("docs/deeper/a rather long file name.json".into(), br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()),
        ("docs/한글.txt".into(), "물 aqueous".as_bytes().to_vec()),
        ("bin/data.bin".into(), (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect()),
        ("empty.txt".into(), Vec::new()),
        ("photo.jpg".into(), jpeg),
        ("frag.bin".into(), (0..cluster as u32 * 5 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    out.extend((0..24).step_by(2).map(|i| (format!("gap.{i:02}"), vec![i as u8; cluster])));
    out
}

/// The files of `d` read, by name, with their contents and their extents.
fn read(d: &[u8]) -> Vec<(String, Vec<u8>, usize)> {
    let mut out: Vec<_> = aqr_fat::fat::files(&aqr_disk::Plain(d))
        .into_iter()
        .map(|f| {
            let data = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
            (String::from_utf8(f.name).unwrap(), data, f.extents.len())
        })
        .collect();
    out.sort();
    out
}

const FIXTURES: [&str; 3] = ["fat12.img", "fat16.img", "fat32.img"];

#[test]
fn files_of_each_width() {
    for name in FIXTURES {
        let d = fixture(name);
        // (the bytes of a cluster, as the script read them)
        let cluster = u16::from_le_bytes([d[11], d[12]]) as usize * d[13] as usize;
        assert!(aqr_fat::fat::starts(&aqr_disk::Plain(&d)), "{name}");
        let got = read(&d);
        for (want, data) in expected(cluster) {
            let Some(g) = got.iter().find(|g| g.0 == want) else { panic!("{name}: {want} not found in {got:?}") };
            assert!(g.1 == data, "{name}: {want}");
        }
        // (the files written into the gaps: in pieces)
        let pieces = |n: &str| got.iter().find(|g| g.0 == n).unwrap().2;
        assert!(pieces("photo.jpg") > 1 && pieces("frag.bin") > 1, "{name}");
        let f = encode(&d).unwrap();
        assert!(f.eval(&none).unwrap() == d, "{name} not rebuilt");
        assert!(!f.shared.is_empty(), "{name}");
    }
}

#[test]
fn not_fat() {
    assert!(!aqr_fat::fat::starts(&aqr_disk::Plain(&[0; 4096])));
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(!aqr_fat::fat::starts(&aqr_disk::Plain(&noise)));
    assert!(!aqr_fat::fat::starts(&aqr_disk::Plain(&fixture("exfat.img"))));
    // (an MBR's sector, of a FAT volume in a partition)
    let z = std::fs::read(format!("{}/../aqr-disk/tests/data/mbr.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mbr = zstd::stream::decode_all(&z[..]).unwrap();
    assert!(!aqr_fat::fat::starts(&aqr_disk::Plain(&mbr)) && aqr_fat::fat::starts(&aqr_disk::Plain(&mbr[512..])));
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
    for name in FIXTURES {
        let d = fixture(name);
        // (the boot sector, the FATs and the directories, mostly)
        let meta: Vec<usize> = (0..d.len()).filter(|&i| d[i] != 0).take(20000).collect();
        for cut in [d.len() / 2, *meta.last().unwrap(), 70_000, 4096, 600, 100] {
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

/// Chains that meet: every entry of FAT16's root region a file of 4 GiB from cluster 2, each
/// followed (and cut) only as far as the clusters followed in all allow, twice their count.
#[test]
fn chains_met() {
    let mut d = fixture("fat16.img");
    let le16 = |p: usize| u16::from_le_bytes([d[p], d[p + 1]]) as usize;
    let (bps, spc, reserved, fats, fat_sectors, entries) =
        (le16(11), d[13] as usize, le16(14), d[16] as usize, le16(22), le16(17));
    let root = (reserved + fats * fat_sectors) * bps;
    let cluster = bps * spc;
    let count = (le16(19) - root / bps - entries * 32 / bps) / spc;
    // (a FAT chaining each cluster to the next)
    for c in 2..count + 2 {
        let at = reserved * bps + 2 * c;
        d[at..at + 2].copy_from_slice(&(c as u16 + 1).to_le_bytes());
    }
    for e in d[root..root + entries * 32].as_chunks_mut::<32>().0 {
        e.copy_from_slice(b"BIG     BIN\x20\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x02\0\xff\xff\xff\xff");
    }
    let files = aqr_fat::fat::files(&aqr_disk::Plain(&d));
    assert_eq!(files.len(), entries);
    let bytes: usize = files.iter().flat_map(|f| &f.extents).map(|r| r.len()).sum();
    assert!(bytes > count * cluster && bytes <= (2 * count + 1024) * cluster, "{bytes}");
    let f = encode(&d).unwrap();
    assert!(f.eval(&none).unwrap() == d);
}
