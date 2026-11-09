use aqr_disk::formula::{self, Options};
use aqr_formula::{Formula, Kind};

/// Fixture `name` of tests/data (`scripts/gen-fixtures.sh`), decompressed.
fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

/// The modules of the formulas (NTFS's compressed files: lznt1-recons), run natively.
fn modules() -> aqr_decompress::Native {
    aqr_decompress::Native::default()
}

fn encode(d: &[u8]) -> Option<Formula> {
    formula::encode_with(d, &Options { file_systems: &[aqr_ntfs::FS], kind: |_| Kind::Unknown, ..Default::default() })
}

/// The files of `d` read, by name, with their contents and their extents.
fn read(d: &[u8]) -> Vec<(String, Vec<u8>, usize)> {
    let mut out: Vec<_> = aqr_ntfs::files(&aqr_disk::Plain(d))
        .into_iter()
        .map(|f| {
            let data = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
            (String::from_utf8(f.name).unwrap(), data, f.extents.len())
        })
        .collect();
    out.sort();
    out
}

/// The non-resident files `scripts/gen-fixtures.sh` wrote, by name, with their contents as read
/// (of a sparse file, its clusters of data: the hole left out).
fn expected() -> Vec<(String, Vec<u8>)> {
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut out = vec![
        ("readme.txt".to_string(), b"Aqueous NTFS fixture\r\n".repeat(40)),
        // (a hard link of bin/data.bin: one file, of one of its names)
        ("link.bin".into(), (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect()),
        ("ads.txt".into(), b"the main stream\n".repeat(100)),
        ("ads.txt:extra".into(), b"an alternate data stream\n".repeat(200)),
        ("sparse.bin".into(), [vec![0x11; 4096], vec![0x22; 4096]].concat()),
        ("photo.jpg".into(), jpeg),
        ("frag.bin".into(), (0..4096u32 * 250 + 100).map(|i| ((i * 13 + (i >> 9)) % 253) as u8).collect()),
    ];
    out.extend((0..600).step_by(2).map(|i| (format!("gap.{i:03}"), vec![(i % 251) as u8; 4096])));
    out
}

#[test]
fn files_found() {
    let d = fixture("ntfs.img");
    assert!(aqr_ntfs::starts(&aqr_disk::Plain(&d)));
    let got = read(&d);
    for (want, data) in expected() {
        let Some(g) = got.iter().find(|g| g.0 == want) else { panic!("{want} not found") };
        assert!(g.1 == data, "{want}");
    }
    // resident, system files and directories: not files with contents
    for name in ["docs/한글.txt", "docs/deeper/a rather long file name.json", "$MFT", "bin"] {
        assert!(!got.iter().any(|g| g.0 == name), "{name}");
    }
    assert!(got.iter().find(|g| g.0 == "frag.bin").unwrap().2 > 8);
    let f = encode(&d).unwrap();
    assert!(f.eval(&modules()).unwrap() == d);
    assert!(!f.shared.is_empty());
}

#[test]
fn not_ntfs() {
    assert!(!aqr_ntfs::starts(&aqr_disk::Plain(&[0; 4096])));
    let noise: Vec<u8> = (0..65536u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    assert!(!aqr_ntfs::starts(&aqr_disk::Plain(&noise)));
    let z = std::fs::read(format!("{}/../aqr-fat/tests/data/fat32.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert!(!aqr_ntfs::starts(&aqr_disk::Plain(&zstd::stream::decode_all(&z[..]).unwrap())));
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
    let d = fixture("ntfs.img");
    // (the boot sector and the MFT's records)
    let mft = 4 * 4096;
    let meta: Vec<usize> = (0..512).chain(mft..mft + 1024 * 64).filter(|&i| d[i] != 0).collect();
    for cut in [d.len() / 2, mft + 1024 * 40, mft + 3000, 4096, 600, 100] {
        let d = &d[..cut];
        if let Some(f) = encode(d) {
            assert!(f.eval(&modules()).unwrap() == d, "cut at {cut}");
        }
    }
    for _ in 0..500 {
        let mut e = d.clone();
        for _ in 0..1 + next() % 4 {
            let at = meta[(next() % meta.len() as u64) as usize];
            e[at] = next() as u8;
        }
        if let Some(f) = encode(&e) {
            assert!(f.eval(&modules()).unwrap() == e, "changed");
        }
    }
}

/// A volume Windows formatted and filled (`scripts/gen-fixtures.ps1`, a fixed VHD of an MBR):
/// its files read through aqr-disk's partitions, frag.bin's runs in an extension record.
#[test]
fn windows_volume() {
    let d = fixture("windows.vhd");
    let opt = Options { file_systems: &[aqr_ntfs::FS], kind: |_| Kind::Unknown, ..Default::default() };
    let files = aqr_disk::files(&d, opt.file_systems);
    let got = |name: &str| -> Option<(Vec<u8>, usize)> {
        let f = files.iter().find(|f| f.name == name.as_bytes())?;
        Some((f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect(), f.extents.len()))
    };
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect();
    let chunk: Vec<u8> = (0..65536u32).map(|i| ((i * 13 + 5) % 253) as u8).collect();
    let frag: Vec<u8> = (0..1536 * 1024u32).map(|i| ((i * 31 + (i >> 10)) % 249) as u8).collect();
    assert_eq!(got("readme.txt").unwrap().0, b"Aqueous NTFS fixture\r\n".repeat(40));
    // (a hard link: one file, of one of its names)
    assert!(got("link.bin").or_else(|| got("bin/data.bin")).unwrap().0 == data);
    assert!(got("sparse.bin").unwrap().0 == [&chunk[..], &chunk[..]].concat());
    assert!(got("photo.jpg").unwrap().0 == jpeg);
    let (f, pieces) = got("frag.bin").unwrap();
    assert!(f == frag && pieces > 300, "{pieces}");
    // resident: in the rest; WOF's stream not a file of its own
    for name in ["ads.txt", "ads.txt:extra", "wof.exe:WofCompressedData"] {
        assert!(got(name).is_none(), "{name}");
    }
    // WOF's: the file, its extents of WofCompressedData, of XPRESS of 4 KiB chunks
    let wof = files.iter().find(|f| f.name == b"wof.exe").unwrap();
    assert_eq!(wof.compressed, Some(aqr_disk::Compressed::Wof { algorithm: 0, len: 256 << 10 }));
    let stream: Vec<u8> = wof.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
    let (_, chunks) = aqr_disk::compressed::wof_chunks(&stream, 256 << 10, 4096).unwrap();
    assert_eq!(chunks.len(), 64);
    // (the bytes the script wrote; the volume's formula has xpress-recons, which aqr-compress's
    // tests run)
    let u = aqr_disk::compressed::unpack(&stream, wof.compressed.as_ref().unwrap()).unwrap();
    assert!(u.data == (0..256u32 << 10).map(|i| ((i * 5 + (i >> 7)) % 239) as u8).collect::<Vec<u8>>());
}

/// Files Windows compressed by WOF (`scripts/gen-wof.ps1`'s small volume): of LZX and of XPRESS of
/// 4 KiB chunks, each the file's name, its extents of WofCompressedData; both taken apart into
/// the data that plain.txt holds as it is.
#[test]
fn wof_files() {
    let d = fixture("wof-small.vhd");
    let files = aqr_disk::files(&d, &[aqr_ntfs::FS]);
    let data = |name: &[u8]| -> Vec<u8> {
        let f = files.iter().find(|f| f.name == name).unwrap();
        f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect()
    };
    let plain = data(b"plain.txt");
    assert_eq!(plain.len(), 256 << 10);
    for (name, algorithm) in [(&b"lzx/text.txt"[..], 1), (b"xpress4k/text.txt", 0)] {
        let f = files.iter().find(|f| f.name == name).unwrap();
        assert_eq!(f.compressed, Some(aqr_disk::Compressed::Wof { algorithm, len: 256 << 10 }));
    }
    for (name, algorithm) in [(&b"lzx/text.txt"[..], 1), (b"xpress4k/text.txt", 0)] {
        let how = aqr_disk::Compressed::Wof { algorithm, len: 256 << 10 };
        let u = aqr_disk::compressed::unpack(&data(name), &how).unwrap();
        assert!(u.data == plain && u.streams.len() == 1 && (u.by == aqr_disk::compressed::By::Lzx) == (algorithm == 1));
    }
}

/// Files NTFS compressed (LZNT1): of Windows' volume and of ntfs-3g's, the file, its units, taken
/// apart into the text written.
#[test]
fn lznt1_files() {
    for (name, text) in [
        ("windows.vhd", &b"The quick brown fox jumps over the lazy dog.\r\n"[..]),
        ("ntfs.img", b"compressed text, line after line\n"),
    ] {
        let d = fixture(name);
        let files = aqr_disk::files(&d, &[aqr_ntfs::FS]);
        let f = files.iter().find(|f| f.name == b"packed/text.txt").unwrap();
        let Some(how @ aqr_disk::Compressed::Lznt1 { len, unit, .. }) = &f.compressed else { panic!("{name}: {f:?}") };
        assert_eq!((*len, *unit), ((text.len() * 3000) as u64, 65536), "{name}");
        let stream: Vec<u8> = f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect();
        let u = aqr_disk::compressed::unpack(&stream, how);
        assert!(u.is_some_and(|u| u.data == text.repeat(3000)), "{name}");
    }
}
