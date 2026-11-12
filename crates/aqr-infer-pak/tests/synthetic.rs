//! Directories of made-up formats: nothing about them is told the inference.

use aqr_infer_pak::tiling::Layout;
use aqr_infer_pak::{File, Options, infer};

/// xorshift64*: deterministic bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Members that look like files: a 4-byte magic, then text-like bytes.
fn member(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut v = b"MEMB".to_vec();
    v.extend((4..n).map(|_| b"abcdefgh  \n"[rng.below(11) as usize]));
    v
}

/// A directory at the head: records of a 64-bit hash, a 32-bit offset and a 32-bit size; the
/// members after it, each aligned to 16.
#[test]
fn offsets_and_sizes() {
    let mut rng = Rng(0x1234_5678);
    let n = 300;
    let head = 16 + 16 * n;
    let mut data = Vec::new();
    let mut table = Vec::new();
    for _ in 0..n {
        let size = 100 + rng.below(4000) as usize;
        let off = head + data.len();
        data.extend(member(&mut rng, size));
        while data.len() % 16 != 0 {
            data.push(0);
        }
        table.extend(rng.next().to_le_bytes());
        table.extend((off as u32).to_le_bytes());
        table.extend((size as u32).to_le_bytes());
    }
    let mut pak = b"PAKFILE\0".to_vec();
    pak.extend((n as u64).to_le_bytes());
    pak.extend(table);
    pak.extend(data);
    let files = [File { name: "x.pak".into(), data: &pak }];
    let (dirs, _) = infer(&files, &Options::default());
    assert_eq!(dirs.len(), 1, "{:?}", dirs.iter().map(|d| (&d.target, d.members.len())).collect::<Vec<_>>());
    let d = &dirs[0];
    assert!(matches!(d.cand.layout, Layout::Explicit { .. }));
    assert_eq!(d.members.len(), n);
    assert!(d.cover > 0.98, "{}", d.cover); // the header and the padding are no members
}

/// Sizes alone, in a table of 8-byte records, the members zstd-compressed and laid end to end
/// after it.
#[test]
fn sizes_end_to_end() {
    let mut rng = Rng(0x9abc_def0);
    let n = 200;
    let mut sizes = Vec::new();
    let mut data = Vec::new();
    for _ in 0..n {
        let len = 2000 + rng.below(6000) as usize;
        let m = member(&mut rng, len);
        let c = zstd::bulk::compress(&m, 3).unwrap();
        sizes.push(c.len() as u64);
        data.extend(c);
    }
    let mut pak = b"CHUNKS\0\0".to_vec();
    pak.extend((n as u64).to_le_bytes());
    for s in &sizes {
        pak.extend(s.to_le_bytes());
    }
    pak.extend(data);
    let files = [File { name: "y.pak".into(), data: &pak }];
    let (dirs, _) = infer(&files, &Options::default());
    let d = dirs.iter().find(|d| d.target == "y.pak").expect("no directory");
    assert!(matches!(d.cand.layout, Layout::Implicit { .. }));
    assert_eq!(d.members.len(), n);
    assert_eq!(d.codecs, vec![("zstd", 24)]);
}

/// Random bytes have no directory.
#[test]
fn random_bytes() {
    let mut rng = Rng(42);
    let data = rng.bytes(4 << 20);
    let files = [File { name: "z.bin".into(), data: &data }];
    let (dirs, _) = infer(&files, &Options::default());
    assert!(dirs.is_empty());
}
