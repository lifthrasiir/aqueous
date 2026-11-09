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
