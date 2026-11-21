//! Tests of `disk::ntfs`: the free clusters of an NTFS volume, zeroed in place (`slack`).

use super::*;

fn fixture(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../codecs/aqr-ntfs/tests/data").join(name);
    zstd::decode_all(std::fs::File::open(p).unwrap()).unwrap()
}

fn le(b: &[u8], at: usize, n: usize) -> usize {
    b[at..at + n].iter().rev().fold(0, |a, &x| a << 8 | x as usize)
}

/// Where things are in a volume, read here again (by the first run of each): the cluster size,
/// the offset of the MFT's records, the record size, the bitmap's offset and the clusters.
struct Layout {
    cluster: usize,
    mft: usize,
    record: usize,
    bitmap: usize,
    clusters: usize,
}

fn layout(d: &[u8]) -> Layout {
    let cluster = le(d, 0x0B, 2) * d[0x0D] as usize;
    let record = match d[0x40] as i8 {
        n if n > 0 => n as usize * cluster,
        n => 1 << -n,
    };
    let mft = le(d, 0x30, 8) * cluster;
    // $Bitmap: record 6's unnamed $DATA, its first run.
    let r6 = mft + 6 * record;
    let mut at = r6 + le(d, r6 + 0x14, 2);
    while le(d, at, 4) != 0x80 {
        at += le(d, at + 4, 4);
    }
    assert_ne!(d[at + 8], 0, "a non-resident bitmap");
    let run = at + le(d, at + 0x20, 2);
    let (ls, os) = ((d[run] & 15) as usize, (d[run] >> 4) as usize);
    let lcn = le(d, run + 1 + ls, os);
    Layout { cluster, mft, record, bitmap: lcn * cluster, clusters: le(d, 0x28, 8) / d[0x0D] as usize }
}

fn bit(d: &[u8], l: &Layout, c: usize) -> bool {
    d[l.bitmap + c / 8] >> (c % 8) & 1 != 0
}

/// `d` with a pattern in every free cluster (by the bitmap read here).
fn dirty(d: &[u8]) -> (Vec<u8>, usize) {
    let l = layout(d);
    let mut x = d.to_vec();
    let mut n = 0;
    for c in 0..l.clusters {
        if !bit(d, &l, c) {
            n += l.cluster;
            for (i, b) in x[c * l.cluster..(c + 1) * l.cluster].iter_mut().enumerate() {
                *b = (i % 251) as u8 | 1;
            }
        }
    }
    (x, n)
}

fn slack() -> Lossy {
    parse_any("slack").unwrap()
}

#[test]
fn free_bytes_of_the_fixture() {
    let d = fixture("ntfs.img.zst");
    let l = layout(&d);
    let free = super::disk::free(&d).unwrap();
    let total: usize = free.iter().map(|r| r.len()).sum();
    let want = (0..l.clusters).filter(|&c| !bit(&d, &l, c)).count() * l.cluster;
    eprintln!("ntfs.img: {} bytes, {} free in {} ranges", d.len(), total, free.len());
    assert_eq!(total, want);
    assert!(total > 0);
    assert!(free.iter().all(|r| r.start % l.cluster == 0 && r.end <= l.clusters * l.cluster));
    assert!(free.windows(2).all(|w| w[0].end < w[1].start));
}

#[test]
fn stale_data_in_free_clusters_is_zeroed() {
    let d = fixture("ntfs.img.zst");
    let l = layout(&d);
    let (x, n) = dirty(&d);
    assert!(n > 0);
    let opt = Options::default();
    let Some((Rewrite::Made { .. }, Some(r))) = rewrite(&x, &slack(), &opt) else { panic!("not made") };
    assert_eq!(r.len(), x.len());
    check::disk(&x, &r).unwrap();
    for c in 0..l.clusters {
        let s = &r[c * l.cluster..(c + 1) * l.cluster];
        if bit(&d, &l, c) {
            assert_eq!(s, &x[c * l.cluster..(c + 1) * l.cluster], "cluster {c} changed");
        } else {
            assert!(s.iter().all(|&b| b == 0), "cluster {c} not zero");
        }
    }
    // The boot sector, the backup boot sector and the files are as they were.
    assert_eq!(r[..512], x[..512]);
    let tail = l.clusters * l.cluster;
    assert_eq!(r[tail..], x[tail..]);
    let fss = super::disk::file_systems();
    let (a, b) = (aqr_disk::files(&x, fss), aqr_disk::files(&r, fss));
    assert!(!a.is_empty());
    assert_eq!(a.len(), b.len());
    for (f, g) in a.iter().zip(&b) {
        assert_eq!((&f.name, &f.extents), (&g.name, &g.extents));
        assert!(f.extents.iter().all(|e| x[e.clone()] == r[e.clone()]));
    }
    // R again: nothing left to zero.
    assert!(rewrite(&r, &slack(), &opt).is_none());
}

#[test]
fn containers_are_turned_down() {
    for name in ["windows.vhd.zst", "wof-small.vhd.zst"] {
        let d = fixture(name);
        let r = super::disk::free(&d);
        if aqr_disk::container::open(&d).is_some() {
            let e = r.unwrap_err();
            assert!(e.starts_with("a container"), "{name}: {e}");
        } else {
            // (a fixed VHD is its disk's bytes and a footer: read as plain)
            eprintln!("{name}: plain, {} free ranges", r.as_ref().unwrap().len());
            assert!(r.unwrap().iter().all(|f| f.end <= d.len()));
        }
    }
}

#[test]
fn a_damaged_fixup_is_refused() {
    let mut d = fixture("ntfs.img.zst");
    let l = layout(&d);
    d[l.mft + 510] ^= 0xFF;
    let e = super::disk::free(&d).unwrap_err();
    assert!(e.contains("ntfs") && e.contains("fixup"), "{e}");
    // A record in the middle of the MFT.
    let mut d = fixture("ntfs.img.zst");
    d[l.mft + 5 * l.record + 510] ^= 0xFF;
    assert!(super::disk::free(&d).is_err());
}

#[test]
fn a_free_cluster_of_the_mft_is_refused() {
    let mut d = fixture("ntfs.img.zst");
    let l = layout(&d);
    let c = l.mft / l.cluster;
    assert!(bit(&d, &l, c));
    d[l.bitmap + c / 8] &= !(1 << (c % 8));
    let e = super::disk::free(&d).unwrap_err();
    assert!(e.contains("a free cluster in a file's runs"), "{e}");
}

#[test]
fn a_free_cluster_of_a_file_is_refused() {
    let mut d = fixture("ntfs.img.zst");
    let l = layout(&d);
    // The bitmap's own cluster, and the last of the used ones.
    let c = l.bitmap / l.cluster;
    d[l.bitmap + c / 8] &= !(1 << (c % 8));
    assert!(super::disk::free(&d).is_err());
}

#[test]
fn a_short_volume_is_refused() {
    let d = fixture("ntfs.img.zst");
    // More sectors than bytes.
    let mut x = d.clone();
    let n = le(&x, 0x28, 8) + 1000;
    x[0x28..0x30].copy_from_slice(&(n as u64).to_le_bytes());
    assert!(super::disk::free(&x).is_err());
    // Truncated: the volume's own bytes are fewer than its sectors.
    assert!(super::disk::free(&d[..d.len() / 2]).is_err());
}
