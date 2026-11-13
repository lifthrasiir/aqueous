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

/// A pak of `n` members in the format of `offsets_and_sizes`, its count in the header.
fn pak(rng: &mut Rng, n: usize) -> Vec<u8> {
    let head = 16 + 16 * n;
    let (mut data, mut table) = (Vec::new(), Vec::new());
    for _ in 0..n {
        let size = 100 + rng.below(4000) as usize;
        let off = head + data.len();
        data.extend(member(rng, size));
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
    pak
}

/// A layout learned from one pak holds in others of the format, a pak of one member too (no
/// evidence of its own); and not in one of another.
#[test]
fn template() {
    use aqr_infer_pak::template;
    let mut rng = Rng(7);
    let big = pak(&mut rng, 300);
    let files = [File { name: "a.pak".into(), data: &big }];
    let (dirs, _) = infer(&files, &Options::default());
    let t = template::learn(&files, 0, &dirs).expect("no template");
    for n in [57, 1] {
        let small = pak(&mut rng, n);
        let files = [File { name: "b.pak".into(), data: &small }];
        let dirs = template::verify(&t, &files, 0).unwrap_or_else(|| panic!("{n} members: does not hold"));
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].members.len(), n);
    }
    // the same magic, other bytes after it
    let mut other = b"PAKFILE\0".to_vec();
    other.extend(rng.bytes(64 << 10));
    let files = [File { name: "c.pak".into(), data: &other }];
    assert!(template::verify(&t, &files, 0).is_none());
}

/// The format of `offsets_and_sizes` over seeds: the members' bytes, sizes and the hashes
/// change the table's and the data's statistics (seed 7 had the table merged with its data).
#[test]
fn seeds() {
    let mut missed = Vec::new();
    for seed in 1..=16u64 {
        let mut rng = Rng(seed * 0x9e37_79b9_7f4a_7c15);
        let n = 20 + rng.below(400) as usize;
        let p = pak(&mut rng, n);
        let files = [File { name: "s.pak".into(), data: &p }];
        let (dirs, _) = infer(&files, &Options::default());
        if !(dirs.len() == 1 && dirs[0].members.len() == n) {
            missed.push((seed, n, dirs.iter().map(|d| d.members.len()).collect::<Vec<_>>()));
        }
    }
    assert!(missed.is_empty(), "{missed:?}");
}

/// `data` as an LZO1X stream of one run of literals.
fn lzo_literals(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0];
    let mut n = data.len() - 18;
    while n > 255 {
        out.push(0);
        n -= 255;
    }
    out.push(n as u8);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0x11, 0, 0]);
    out
}

/// An index and its data file of `n` LZO1X members of `base` bytes and up to 3000 more.
fn family(rng: &mut Rng, n: usize, base: usize) -> (Vec<u8>, Vec<u8>) {
    let (mut data, mut index) = (Vec::new(), b"IDXv\x01\x00\x00\x00".to_vec());
    index.extend((n as u64).to_le_bytes());
    for _ in 0..n {
        let len = base + rng.below(3000) as usize;
        let text: Vec<u8> = (0..len).map(|_| b"lorem ipsum dolor sit amet "[rng.below(27) as usize]).collect();
        let stream = lzo_literals(&text);
        index.extend(rng.next().to_le_bytes());
        index.extend((data.len() as u32).to_le_bytes());
        index.extend((stream.len() as u32).to_le_bytes());
        data.extend(stream);
    }
    (index, data)
}

/// An index and its data file: the directory in the index, of the data file, its members
/// LZO1X; its template holds for families of 5 members and of 1.
#[test]
fn index_and_data() {
    index_and_data_of(200);
}

/// The same with members of 15 KB, a data file of 5 MB (the first row was missed: the
/// template learned started a row late, past the end of the smaller families' tables).
#[test]
fn index_and_data_big() {
    index_and_data_of(15_000);
}

fn index_and_data_of(base: usize) {
    use aqr_infer_pak::template;
    let mut rng = Rng(11);
    let (index, data) = family(&mut rng, 300, base);
    let files = [File { name: "a.idx".into(), data: &index }, File { name: "a.dat".into(), data: &data }];
    let (dirs, _) = infer(&files, &Options::default());
    let what: Vec<_> = dirs.iter().map(|d| (d.owner.clone(), d.target.clone(), d.members.len(), d.codec)).collect();
    assert_eq!(dirs.len(), 1, "{what:?}");
    assert_eq!((dirs[0].target.as_str(), dirs[0].members.len()), ("a.dat", 300));
    let t = template::learn(&files, 0, &dirs).unwrap();
    for n in [5, 1] {
        let (index, data) = family(&mut rng, n, base);
        let files = [File { name: "b.idx".into(), data: &index }, File { name: "b.dat".into(), data: &data }];
        let dirs = template::verify(&t, &files, 0).unwrap_or_else(|| panic!("{n} members: does not hold"));
        assert_eq!(dirs[0].members.len(), n);
    }
}
