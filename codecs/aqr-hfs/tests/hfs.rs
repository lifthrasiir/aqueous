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
    formula::encode_with(d, &Options { file_systems: &[aqr_hfs::FS], kind: |_| Kind::Unknown, ..Default::default() })
}

type Found = Vec<(String, Vec<u8>, Vec<std::ops::Range<usize>>)>;

/// The files of `d` (a disk with the volume in a partition) read, by name, with their contents and
/// their extents.
fn read(d: &[u8]) -> Found {
    let mut out: Found = aqr_disk::files(d, &[aqr_hfs::FS])
        .into_iter()
        .map(|f| {
            let data = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
            (String::from_utf8(f.name).unwrap(), data, f.extents)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn files_found(image: &str) {
    let d = fixture(image);
    let got = read(&d);
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect();
    let find = |n: &str| got.iter().find(|g| g.0 == n);
    // (HFS+ keeps names decomposed: Hangul as jamo)
    let hangul = ["docs/한글.txt", "docs/\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.txt"];
    let hangul = hangul.iter().find(|n| find(n).is_some()).expect("docs/한글.txt not found");
    assert_eq!(find(hangul).unwrap().1, "물 aqueous".as_bytes());
    let mut want = vec![
        ("readme.txt".to_string(), b"Aqueous HFS+ fixture\n".repeat(40)),
        ("docs/deeper/a rather long file name.json".into(), br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()),
        ("empty.txt".into(), Vec::new()),
        // (the resource fork stays in the rest)
        ("fork.txt".into(), b"the data fork\n".repeat(100)),
        ("photo.jpg".into(), jpeg),
        ("frag.bin".into(), (0..4096u32 * 250 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    // (a hard link: one file, under one of the names)
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
    assert_eq!(got.iter().filter(|g| g.0 == "link.bin" || g.0 == "bin/data.bin").count(), 1);
    // (the symbolic link, the directories, the private directory's inodes; decmpfs: in the rest)
    for not in ["link.sym", "bin", "docs", "many", "packed.txt"] {
        assert!(find(not).is_none(), "{not}");
    }
    assert!(got.iter().all(|g| !g.0.contains('\0') && !g.0.starts_with("iNode") && !g.0.contains("Private")));
    // 8 extents in the catalog record; the rest of the 251 in the extents overflow file
    assert!(find("frag.bin").unwrap().2.len() > 8);
    assert_eq!(got.len(), 400 + 300 + 9);
    let f = encode(&d).unwrap();
    assert!(f.eval(&none).unwrap() == d);
    assert!(!f.shared.is_empty());
}

#[test]
fn hfs_plus_files() {
    files_found("hfs.img");
}

#[test]
fn hfsx_files() {
    files_found("hfsx.img");
}

#[test]
fn not_hfs() {
    assert!(!aqr_hfs::starts(&aqr_disk::Plain(&[0; 4096])));
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(!aqr_hfs::starts(&aqr_disk::Plain(&noise)));
    for other in [
        "aqr-apfs/tests/data/apfs.img.zst",
        "aqr-fat/tests/data/fat32.img.zst",
        "aqr-ntfs/tests/data/ntfs.img.zst",
        "aqr-ext/tests/data/ext4.img.zst",
    ] {
        let z = std::fs::read(format!("{}/../{other}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(!aqr_hfs::starts(&aqr_disk::Plain(&zstd::stream::decode_all(&z[..]).unwrap())), "{other}");
    }
    // (the volumes themselves start, in their partitions: an APM and a GPT)
    for name in ["hfs.img", "hfsx.img"] {
        let d = fixture(name);
        assert!(aqr_disk::starts(&d, &[aqr_hfs::FS]), "{name}");
        let at = aqr_disk::volumes(&aqr_disk::Plain(&d))
            .into_iter()
            .find(|v| aqr_hfs::starts(&aqr_disk::Sub { of: &aqr_disk::Plain(&d), span: v.clone() }));
        assert!(at.is_some(), "{name}");
    }
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
    for image in ["hfs.img", "hfsx.img"] {
        let d = fixture(image);
        let vol = aqr_disk::volumes(&aqr_disk::Plain(&d))
            .into_iter()
            .find(|v| aqr_hfs::starts(&aqr_disk::Sub { of: &aqr_disk::Plain(&d), span: v.clone() }))
            .unwrap();
        // (the extents overflow and catalog files, from the volume header's fork data: single extents)
        let h = &d[vol.start + 1024..];
        let (bs, ext, cat) = (u32::from_be_bytes(h[40..44].try_into().unwrap()) as usize, 192 + 16, 272 + 16);
        let meta: Vec<usize> = [ext, cat]
            .into_iter()
            .flat_map(|o| {
                let (s, c) = (
                    u32::from_be_bytes(h[o..o + 4].try_into().unwrap()),
                    u32::from_be_bytes(h[o + 4..o + 8].try_into().unwrap()),
                );
                vol.start + s as usize * bs..vol.start + (s + c) as usize * bs
            })
            .filter(|&i| d[i] != 0)
            .collect();
        let head: Vec<usize> = (vol.start + 1024..vol.start + 1024 + 512).collect();
        let last = *meta.last().unwrap();
        for cut in [d.len() / 2, last, vol.start + 70_000, vol.start + 5000, vol.start + 1500, 4096] {
            let d = &d[..cut.min(d.len())];
            let _ = aqr_disk::files(d, &[aqr_hfs::FS]);
            if let Some(f) = encode(d) {
                assert!(f.eval(&none).unwrap() == d, "cut at {cut}");
            }
        }
        for k in 0..600 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let from = if k % 4 == 0 { &head } else { &meta };
                let at = from[(next() % from.len() as u64) as usize];
                e[at] = next() as u8;
            }
            let _ = aqr_disk::files(&e, &[aqr_hfs::FS]);
            if k % 3 == 0
                && let Some(f) = encode(&e)
            {
                assert!(f.eval(&none).unwrap() == e, "changed");
            }
        }
    }
}
