use super::*;

use std::path::PathBuf;

const FIXTURES: [&str; 3] = ["ext2", "ext4", "ext4m"];

fn fixture(name: &str) -> Vec<u8> {
    let p = format!("{}/../../codecs/aqr-ext/tests/data/{name}.img.zst", env!("CARGO_MANIFEST_DIR"));
    zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap()
}

fn le32(d: &[u8], p: usize) -> usize {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize
}

fn free(d: &[u8]) -> Result<Vec<std::ops::Range<usize>>, String> {
    super::disk::free(d)
}

fn bytes(r: &[std::ops::Range<usize>]) -> usize {
    r.iter().map(|r| r.len()).sum()
}

fn rewritten(x: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(x, &parse_any("slack").unwrap(), &Options::default())
}

/// `x` with its free blocks filled with a pattern, as stale data.
fn stale(x: &[u8]) -> Vec<u8> {
    let mut y = x.to_vec();
    for r in free(x).unwrap() {
        for (i, b) in y[r.clone()].iter_mut().enumerate() {
            *b = (r.start + i) as u8 | 1;
        }
    }
    y
}

fn files(d: &[u8]) -> Vec<(Vec<u8>, Vec<Vec<u8>>)> {
    aqr_disk::files(d, super::disk::file_systems())
        .into_iter()
        .map(|f| (f.name, f.extents.iter().map(|e| d[e.clone()].to_vec()).collect()))
        .collect()
}

#[test]
fn slack_of_ext() {
    for name in FIXTURES {
        let x = fixture(name);
        let f = free(&x).unwrap();
        eprintln!("{name}: {} free bytes in {} ranges of {}", bytes(&f), f.len(), x.len());
        assert!(!f.is_empty(), "{name}");
        let s = stale(&x);
        let (w, r) = rewritten(&s).unwrap_or_else(|| panic!("{name}: nothing taken"));
        assert!(matches!(w, Rewrite::Made { .. }), "{name}: {w:?}");
        let r = r.unwrap();
        check::disk(&s, &r).unwrap();
        assert_eq!(files(&s), files(&r), "{name}");
        assert!(free(&r).unwrap() == f);
        for g in &f {
            assert!(r[g.clone()].iter().all(|&b| b == 0), "{name}");
        }
        assert!(rewritten(&r).is_none(), "{name}: R of R");
        if let Some(dir) = std::env::var_os("AQR_EXT_OUT") {
            std::fs::write(PathBuf::from(&dir).join(format!("{name}.r.img")), &r).unwrap();
            std::fs::write(PathBuf::from(&dir).join(format!("{name}.x.img")), &s).unwrap();
        }
    }
}

#[test]
fn needs_recovery_refused() {
    for name in FIXTURES {
        let mut x = fixture(name);
        x[1024 + 96] |= 4;
        let e = free(&x).unwrap_err();
        assert!(e.contains("recovery"), "{name}: {e}");
    }
}

#[test]
fn inode_table_freed_refused() {
    for name in FIXTURES {
        let mut x = fixture(name);
        let bs = 1024usize << le32(&x, 1024 + 24);
        let first = le32(&x, 1024 + 20);
        let desc = (first + 1) * bs;
        let (bb, it) = (le32(&x, desc), le32(&x, desc + 8));
        // (the first group's bitmap: the bit of the inode table's first block cleared)
        let i = it - first;
        x[bb * bs + i / 8] &= !(1 << (i % 8));
        let e = free(&x).unwrap_err();
        // (a checksum of the descriptor does not cover the bitmap: it is the structure caught)
        assert!(e.contains("structure"), "{name}: {e}");
    }
}

#[test]
fn descriptor_checksum_refused() {
    for name in FIXTURES {
        let mut x = fixture(name);
        let bs = 1024usize << le32(&x, 1024 + 24);
        let first = le32(&x, 1024 + 20);
        let ro = le32(&x, 1024 + 100);
        if ro & 0x410 == 0 {
            continue;
        }
        x[(first + 1) * bs + 0x1E] ^= 1;
        let e = free(&x).unwrap_err();
        assert!(e.contains("checksum"), "{name}: {e}");
    }
}
