//! Containers of a disk (`scripts/gen-containers.sh`: a GPT of an ext4 and an ext2 volume, as
//! qemu-img converts it): the virtual disk as the disk is, its files found where the container
//! holds them, the container rebuilt.

use aqr_disk::container::{self, Kind};
use aqr_disk::formula::{self, Options};
use aqr_disk::{Image, Plain};

fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

fn none(_: &aqr_formula::Apply, _: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    unreachable!("no module")
}

const FS: [aqr_disk::FileSystem; 1] = [aqr_ext::FS];

/// The files of image `d`, by name, with their contents (of the image's bytes).
fn read(d: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out: Vec<_> = aqr_disk::files(d, &FS)
        .into_iter()
        .map(|f| (f.name, f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect()))
        .collect();
    out.sort();
    out
}

const CONTAINERS: [(&str, Kind); 6] = [
    ("disk.vhd", Kind::Vhd),
    ("disk.vhdx", Kind::Vhdx),
    ("disk.vmdk", Kind::Vmdk),
    ("disk.qcow2", Kind::Qcow2),
    ("disk-v2.qcow2", Kind::Qcow2),
    ("disk.vdi", Kind::Vdi),
];

#[test]
fn disks_of_containers() {
    let raw = fixture("disk.img");
    assert!(container::open(&raw).is_none());
    let want = read(&raw);
    // (the volumes' files, both: data.bin, photo.jpg and readme.txt each, big.bin of ext4's)
    assert_eq!(want.len(), 7);
    for (name, kind) in CONTAINERS {
        let d = fixture(name);
        let m = container::open(&d).unwrap_or_else(|| panic!("{name}: no container"));
        assert_eq!(m.kind, kind, "{name}");
        // (qemu-img's VHD of a size of whole cylinders: more, of zeros)
        assert!(m.len() >= raw.len(), "{name}");
        let disk = m.get(0..m.len()).unwrap();
        assert!(disk[..raw.len()] == raw[..] && disk[raw.len()..].iter().all(|&b| b == 0), "{name}: the disk");
        assert!(read(&d) == want, "{name}: the files");
        let f = formula::encode_with(&d, &Options { file_systems: &FS, ..Default::default() }).unwrap();
        assert!(f.eval(&none).unwrap() == d, "{name}: rebuilt");
    }
}

/// A qcow2 image of compressed clusters (stage 8): read as zeros, so files there are not found,
/// but whatever is found is rebuilt.
#[test]
fn compressed_clusters_as_zeros() {
    let d = fixture("disk-c.qcow2");
    let m = container::open(&d).unwrap();
    assert_eq!((m.kind, m.len()), (Kind::Qcow2, 64 << 20));
    if let Some(f) = formula::encode_with(&d, &Options { file_systems: &FS, ..Default::default() }) {
        assert!(f.eval(&none).unwrap() == d);
    }
}

/// Containers cut short and with bytes changed: no panic, and a formula, where there is one, gives
/// the container back.
#[test]
fn damaged_containers() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for (name, _) in CONTAINERS {
        let d = fixture(name);
        let opt = Options { file_systems: &FS, ..Default::default() };
        for cut in [d.len() / 2, d.len() / 5, 70_000, 4096, 600] {
            let d = &d[..cut.min(d.len())];
            if let Some(f) = formula::encode_with(d, &opt) {
                assert!(f.eval(&none).unwrap() == d, "{name} cut at {cut}");
            }
        }
        // (the headers and tables, mostly)
        let meta: Vec<usize> = (0..d.len()).filter(|&i| d[i] != 0).take(4000).collect();
        for _ in 0..100 {
            let mut e = d.clone();
            for _ in 0..1 + next() % 4 {
                let at = meta[(next() % meta.len() as u64) as usize];
                e[at] = next() as u8;
            }
            if let Some(m) = container::open(&e) {
                let _ = m.get(0..m.len().min(1 << 20));
            }
            if let Some(f) = formula::encode_with(&e, &opt) {
                assert!(f.eval(&none).unwrap() == e, "{name} changed");
            }
        }
    }
}

#[test]
fn plain_is_none() {
    assert!(container::open(&[0; 4096]).is_none());
    assert!(Plain(&[1, 2, 3]).get(1..3).is_some_and(|b| *b == [2, 3]));
}

/// Containers that Windows made (`scripts/gen-containers.ps1`, by a peer session): diskpart's
/// dynamic VHDX (a GPT, blocks of 32 MiB) and VHD (an MBR), Hyper-V's New-VHD's VHDX; of each, the
/// NTFS volume's files, the container rebuilt.
#[test]
fn windows_containers() {
    let jpeg = std::fs::read(format!("{}/../aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut want = vec![
        (b"big.bin".to_vec(), (0..8u32 << 20).map(|i| ((i * 11 + (i >> 12)) % 241) as u8).collect::<Vec<u8>>()),
        (b"data.bin".to_vec(), (0..1u32 << 20).map(|i| ((i * 7 + (i >> 8)) % 251) as u8).collect()),
        (b"photo.jpg".to_vec(), jpeg),
        (b"readme.txt".to_vec(), b"Aqueous container fixture\r\n".repeat(40)),
    ];
    want.sort();
    const NTFS: [aqr_disk::FileSystem; 1] = [aqr_ntfs::FS];
    let fs = &NTFS;
    for (name, kind) in [("win.vhdx", Kind::Vhdx), ("win.vhd", Kind::Vhd), ("win-hv.vhdx", Kind::Vhdx)] {
        let d = fixture(name);
        assert_eq!(container::open(&d).map(|m| m.kind), Some(kind), "{name}");
        let mut got: Vec<_> = aqr_disk::files(&d, fs)
            .into_iter()
            .map(|f| (f.name, f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect::<Vec<u8>>()))
            .collect();
        got.sort();
        assert!(
            got == want,
            "{name}: {:?}",
            got.iter().map(|g| (String::from_utf8_lossy(&g.0).into_owned(), g.1.len())).collect::<Vec<_>>()
        );
        let f = formula::encode_with(&d, &Options { file_systems: fs, ..Default::default() }).unwrap();
        assert!(f.eval(&none).unwrap() == d, "{name}: rebuilt");
    }
}

/// UDIF images (`scripts/gen-dmg.sh`): the chunks of each cover its disk, one after another;
/// UDRO's a container of the disk as it is, UDZO's zlib chunks inflating to their parts of it.
#[test]
fn udif_images() {
    let disk = fixture("dmg-disk.img");
    for name in ["dmg-udro.dmg", "dmg-udzo.dmg", "dmg-udbz.dmg", "dmg-ulfo.dmg"] {
        let d = fixture(name);
        let u = aqr_disk::dmg::parse(&d).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(u.len, disk.len() as u64, "{name}");
        let mut chunks = u.chunks.clone();
        chunks.sort_by_key(|c| c.disk.start);
        let mut at = 0;
        for c in &chunks {
            assert_eq!(c.disk.start, at, "{name}");
            at = c.disk.end;
            let want = &disk[c.disk.start as usize..c.disk.end as usize];
            match c.how {
                aqr_disk::dmg::How::Zeros => assert!(want.iter().all(|&b| b == 0), "{name}"),
                aqr_disk::dmg::How::Raw => assert!(&d[c.file.clone()] == want, "{name}"),
                aqr_disk::dmg::How::Zlib => {
                    let mut out = Vec::new();
                    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&d[c.file.clone()]), &mut out)
                        .unwrap();
                    assert!(out == want, "{name}");
                }
                _ => {}
            }
        }
        assert_eq!(at, u.len, "{name}");
    }
    let d = fixture("dmg-udro.dmg");
    let m = container::open(&d).unwrap();
    assert_eq!(m.kind, Kind::Dmg);
    assert!(m.get(0..m.len()).unwrap()[..] == disk[..]);
    assert!(container::open(&fixture("dmg-udzo.dmg")).is_none());
}

/// UDIF images cut short or with bytes changed (in the trailer and the property list's block
/// tables): none, or chunks within the file and the disk; the container of one as well.
#[test]
fn udif_images_damaged() {
    for name in ["dmg-udro.dmg", "dmg-udzo.dmg"] {
        let d = fixture(name);
        let u = aqr_disk::dmg::parse(&d).unwrap();
        let check = |e: &[u8], what: &str| {
            if let Some(u) = aqr_disk::dmg::parse(e) {
                for c in &u.chunks {
                    assert!(c.file.end <= u.koly.start && c.disk.end <= u.len, "{name}, {what}: {c:?}");
                }
            }
            if let Some(m) = container::open(e) {
                assert_eq!(m.kind, Kind::Dmg, "{name}, {what}");
            }
        };
        for n in [0, 1, 511, 512, 513, d.len() / 2, d.len() - 513, d.len() - 512, d.len() - 1] {
            check(&d[..n], &format!("cut at {n}"));
        }
        let at: Vec<usize> = (u.koly.clone()).chain(u.xml.clone().step_by(7)).collect();
        for &p in &at {
            for v in [0x00, 0xff, d[p] ^ 0x01] {
                let mut e = d.clone();
                e[p] = v;
                check(&e, &format!("byte {p} = {v:#x}"));
            }
        }
    }
}

/// A qcow2 image of compressed clusters (`scripts/gen-containers.sh`, `qemu-img convert -c`): its
/// chunks cover the disk one after another, its raw DEFLATE ones inflating to their parts of it;
/// the same image cut short or with bytes changed (its header, its tables) gives none, or chunks
/// within the file and the disk; an image of no compressed cluster gives none.
#[test]
fn qcow2_compressed_chunks() {
    let (disk, d) = (fixture("disk.img"), fixture("disk-c.qcow2"));
    let u = aqr_disk::container::qcow2_chunks(&d).unwrap();
    assert_eq!(u.len, disk.len() as u64);
    let mut chunks = u.chunks.clone();
    chunks.sort_by_key(|c| c.disk.start);
    let (mut at, mut deflated) = (0, 0);
    for c in &chunks {
        assert_eq!(c.disk.start, at);
        at = c.disk.end;
        let want = &disk[c.disk.start as usize..c.disk.end as usize];
        match c.how {
            aqr_disk::dmg::How::Zeros => assert!(want.iter().all(|&b| b == 0)),
            aqr_disk::dmg::How::Raw => assert!(&d[c.file.clone()] == want),
            aqr_disk::dmg::How::Deflate => {
                let mut out = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::DeflateDecoder::new(&d[c.file.clone()]), &mut out)
                    .unwrap();
                assert!(out == want);
                deflated += 1;
            }
            how => panic!("{how:?}"),
        }
    }
    assert_eq!(at, u.len);
    assert!(deflated > 100, "{deflated}");
    assert!(aqr_disk::container::qcow2_chunks(&fixture("disk.qcow2")).is_none());
    for n in [0, 103, 104, 4096, d.len() / 2, d.len() - 1] {
        let e = &d[..n];
        if let Some(u) = aqr_disk::container::qcow2_chunks(e) {
            assert!(u.chunks.iter().all(|c| c.file.end <= e.len()), "cut at {n}");
        }
    }
    for p in (0..104).chain((0x30000..0x30400).step_by(3)) {
        for v in [0x00, 0xff, d[p] ^ 0x40] {
            let mut e = d.clone();
            e[p] = v;
            if let Some(u) = aqr_disk::container::qcow2_chunks(&e) {
                assert!(u.chunks.iter().all(|c| c.file.end <= e.len()), "byte {p} = {v:#x}");
            }
        }
    }
}
