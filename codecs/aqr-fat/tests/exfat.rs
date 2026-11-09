use aqr_formula::{Apply, Formula, Kind};

fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

fn none(_: &Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!()
}

fn encode(d: &[u8]) -> Option<Formula> {
    aqr_disk::formula::encode_with(
        d,
        &aqr_disk::formula::Options { file_systems: &[aqr_fat::EXFAT], kind: |_| Kind::Unknown },
    )
}

/// What scripts/gen-fixtures.sh writes, by name.
fn expected() -> Vec<(String, Vec<u8>)> {
    let jpeg = format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"));
    let mut v = vec![
        ("readme.txt".to_string(), b"Aqueous FAT fixture\r\n".repeat(40)),
        ("docs/deeper/a rather long file name.json".to_string(), br#"{"a": [1, 2, 3], "b": "c"}"#.to_vec()),
        ("docs/한글.txt".to_string(), "물 aqueous".as_bytes().to_vec()),
        ("bin/data.bin".to_string(), (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect()),
        ("empty.txt".to_string(), Vec::new()),
        ("photo.jpg".to_string(), std::fs::read(jpeg).unwrap()),
        ("frag.bin".to_string(), (0..4096u32 * 5 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    for i in (0..24).step_by(2) {
        v.push((format!("gap.{i:02}"), vec![i as u8; 4096]));
    }
    v
}

fn contents(d: &[u8], f: &aqr_fat::File) -> Vec<u8> {
    f.extents.iter().flat_map(|r| d[r.clone()].iter().copied()).collect()
}

#[test]
fn files_found() {
    let d = fixture("exfat");
    assert!(aqr_fat::exfat::starts(&aqr_disk::Plain(&d)));
    let files = aqr_fat::exfat::files(&aqr_disk::Plain(&d));
    for (name, data) in expected() {
        let f = files.iter().find(|f| f.name == name.as_bytes()).unwrap_or_else(|| panic!("{name} not found"));
        assert!(contents(&d, f) == data, "{name}");
        // photo.jpg and frag.bin were written after deleting every other gap file, but on this fixture
        // macOS's allocator did not put them into the gaps: they are not fragmented (see the output)
        if name == "photo.jpg" || name == "frag.bin" {
            eprintln!("{name}: {:?}", f.extents);
        }
    }
}

#[test]
fn rebuilt() {
    let d = fixture("exfat");
    let f = encode(&d).unwrap();
    assert!(f.eval(&none).unwrap() == d);
}

#[test]
fn damaged() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let d = fixture("exfat");
    // (the directory clusters: where the files' entries are, from the files' names found)
    let heap = u32::from_le_bytes(d[88..92].try_into().unwrap()) as usize * 512;
    let meta: Vec<usize> =
        (0..d.len().min(65536)).chain(heap..heap + 4 * 4096).filter(|&i| i < d.len() && d[i] != 0).collect();
    let check = |d: &[u8], what: &str| {
        if let Some(f) = encode(d) {
            assert!(f.eval(&none).unwrap() == d, "{what}");
        }
    };
    for cut in [d.len() / 2, 12 * 512 + 100, 24 * 512, heap + 100, heap + 4096 + 17, heap + 3 * 4096] {
        check(&d[..cut.min(d.len())], &format!("cut at {cut}"));
    }
    for n in 0..300 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 4 {
            let at = meta[(next() % meta.len() as u64) as usize];
            e[at] = next() as u8;
        }
        check(&e, &format!("changed {n}"));
    }
}

#[test]
fn starts_only_exfat() {
    assert!(!aqr_fat::exfat::starts(&aqr_disk::Plain(&[0; 4096])));
    let mut seed = 1u64;
    let noise: Vec<u8> = (0..4096)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    assert!(!aqr_fat::exfat::starts(&aqr_disk::Plain(&noise)));
    for n in ["fat12", "fat16", "fat32"] {
        assert!(!aqr_fat::exfat::starts(&aqr_disk::Plain(&fixture(n))), "{n}");
    }
}
