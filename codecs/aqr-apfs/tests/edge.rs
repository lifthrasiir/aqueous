//! The APFS fixture with its objects damaged and their checksums made right again, and its
//! B-trees made to loop: no panic, no hang, and no allocation the data cannot hold.

use std::time::Duration;

use aqr_testkit::within;

const BLOCK: usize = 4096;
/// Where the container starts in the fixture (a partition).
const BASE: usize = 20480;
const LIMIT: Duration = Duration::from_secs(60);

fn fixture(name: &str) -> Vec<u8> {
    let z = std::fs::read(format!("{}/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    zstd::stream::decode_all(&z[..]).unwrap()
}

fn le16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize
}

fn le32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

/// The Fletcher-64 of an object block, as the format says.
fn fletcher(b: &[u8]) -> u64 {
    const M: u64 = 0xffff_ffff;
    let (mut s1, mut s2) = (0u64, 0u64);
    for w in b[8..].as_chunks::<4>().0 {
        s1 += u32::from_le_bytes(*w) as u64;
        s2 += s1;
    }
    let (s1, s2) = (s1 % M, s2 % M);
    let c1 = M - (s1 + s2) % M;
    let c2 = M - (s1 + c1) % M;
    c2 << 32 | c1
}

fn set_checksum(b: &mut [u8]) {
    let c = fletcher(b);
    b[..8].copy_from_slice(&c.to_le_bytes());
}

/// The offsets of the blocks of `d` whose checksums are right.
fn objects(d: &[u8]) -> Vec<usize> {
    (BASE..d.len() - BLOCK + 1)
        .step_by(BLOCK)
        .filter(|&at| {
            d[at..at + BLOCK].iter().any(|&b| b != 0)
                && fletcher(&d[at..at + BLOCK]) == u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
        })
        .collect()
}

fn run(d: &[u8]) -> usize {
    within(LIMIT, || aqr_disk::files(d, &[aqr_apfs::FS]).len()).unwrap()
}

/// Bytes of the objects (the superblocks, object maps, trees) changed and the checksum of their
/// block made right again: the files of the container are found or not, never a panic.
#[test]
fn damaged_with_checksums() {
    let d = fixture("apfs.img");
    assert!(run(&d) > 100);
    let blocks = objects(&d);
    // (the blocks that are objects: there are many, the trees' nodes among them)
    assert!(blocks.len() > 10);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut found = 0;
    for _ in 0..800 {
        let mut e = d.clone();
        let mut touched = Vec::new();
        for _ in 0..1 + next() % 3 {
            let at = blocks[(next() % blocks.len() as u64) as usize];
            // the header and the node's fields more often than the records
            let i = if next() % 2 == 0 { 8 + (next() % 64) as usize } else { (next() % BLOCK as u64) as usize };
            e[at + i] = next() as u8;
            touched.push(at);
        }
        for at in touched {
            set_checksum(&mut e[at..at + BLOCK]);
        }
        found += (run(&e) > 0) as usize;
    }
    // (some of the damages are in what is not read)
    assert!(found > 0);
}

/// Every B-tree node of the container whose children are the node itself (or the next index
/// node, in a ring): the walk ends, with the files of the other trees or none.
#[test]
fn tree_loops() {
    let d = fixture("apfs.img");
    let blocks = objects(&d);
    // (btree root and node objects: types 2 and 3 of the header at 24)
    let nodes: Vec<usize> = blocks.iter().copied().filter(|&at| matches!(le32(&d, at + 24) & 0xffff, 2 | 3)).collect();
    let index: Vec<usize> = nodes.iter().copied().filter(|&at| le16(&d, at + 34) > 0).collect();
    assert!(!nodes.is_empty());
    assert!(!index.is_empty());
    for ring in [false, true] {
        for only_fs in [false, true] {
            let mut e = d.clone();
            for (n, &at) in index.iter().enumerate() {
                let target = if ring { index[(n + 1) % index.len()] } else { at };
                let oid = u64::from_le_bytes(e[target + 8..target + 16].try_into().unwrap());
                let b = &mut e[at..at + BLOCK];
                let (flags, nkeys) = (le16(b, 32), le32(b, 36));
                let (toff, tlen) = (le16(b, 40), le16(b, 42));
                let end = if flags & 1 != 0 { BLOCK - 40 } else { BLOCK };
                let (fixed, width) = (flags & 4 != 0, if flags & 4 != 0 { 4 } else { 8 });
                if only_fs && fixed {
                    continue;
                }
                for i in 0..nkeys.min(tlen / width) {
                    let t = 56 + toff + i * width;
                    let voff = if fixed { le16(b, t + 2) } else { le16(b, t + 4) };
                    if voff != 0xffff && voff >= 8 && voff <= end {
                        b[end - voff..end - voff + 8].copy_from_slice(&oid.to_le_bytes());
                    }
                }
                set_checksum(b);
            }
            run(&e);
        }
    }
}

/// The container's object map and every tree emptied or of the greatest counts.
#[test]
fn node_counts() {
    let d = fixture("apfs.img");
    let blocks = objects(&d);
    let nodes: Vec<usize> = blocks.iter().copied().filter(|&at| matches!(le32(&d, at + 24) & 0xffff, 2 | 3)).collect();
    for (at_field, v) in [(36usize, u32::MAX), (36, 0), (40, 0xffff), (42, 0xffff), (34, 0xffff)] {
        let mut e = d.clone();
        for &at in &nodes {
            let b = &mut e[at..at + BLOCK];
            match at_field {
                36 => b[36..40].copy_from_slice(&v.to_le_bytes()),
                _ => b[at_field..at_field + 2].copy_from_slice(&(v as u16).to_le_bytes()),
            }
            set_checksum(b);
        }
        run(&e);
    }
}
