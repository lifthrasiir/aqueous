//! Edge cases of HDF5 files, built in the test (a superblock, a root object header of a
//! dataset's messages and one chunk, DEFLATE's stored blocks as its stream): the four
//! superblocks, float32 and big-endian datasets, sizes the file lies about, a fractal heap
//! whose sizes overflow; and damage to all of them. Whatever `encode_with` takes evaluates back
//! to the input, and nothing panics, hangs or allocates what the data cannot hold.

use aqr_archive::Evaluate;
use aqr_hdf5::formula::{self, Options};
use aqr_hdf5::hdf5;
use aqr_testkit::Refusing;
use std::time::Duration;

#[global_allocator]
static A: Refusing = Refusing(256 << 20);

const UNDEF: u64 = u64::MAX;

/// The formula of `d` evaluated back (it must be `d`): the chunks rebuilt and the float
/// models used.
fn roundtrip(d: &[u8], opt: &Options) -> Option<(usize, usize)> {
    let e = formula::encode_with(d, opt)?;
    let floats = e.formula.applies().iter().filter(|a| a.module == formula::FLOAT).count();
    let out = e
        .formula
        .eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| formula::apply(a, ins, 1).map_err(aqr_formula::Error::new));
    assert!(out.ok().as_deref() == Some(d), "not rebuilt");
    Some((e.chunks, floats))
}

/// A filter of a pipeline: its id and client data.
type Filter = (u16, Vec<u32>);

/// What a file of one dataset is made of.
#[derive(Clone)]
struct Spec {
    /// The superblock's version, 0 to 3.
    sb: u8,
    /// The datatype message's class (0 fixed, 1 float), size and byte order.
    class: u8,
    size: u32,
    be: bool,
    /// The filters, in order.
    filters: Vec<Filter>,
    /// The chunk's dimensions (the last is the element's bytes), as the layout message tells.
    dims: Vec<u64>,
    /// The data of the chunk before its filters (its Fletcher-32 included).
    data: Vec<u8>,
}

fn put_addr(v: &mut Vec<u8>, a: u64) {
    v.extend(a.to_le_bytes());
}

fn message(out: &mut Vec<u8>, t: u8, data: &[u8]) {
    out.push(t);
    out.extend((data.len() as u16).to_le_bytes());
    out.push(0);
    out.extend(data);
}

/// A zlib stream of stored blocks of `d`.
fn zlib_stored(d: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x9c];
    let mut chunks = d.chunks(60000).peekable();
    if chunks.peek().is_none() {
        v.extend([1, 0, 0, 0xff, 0xff]);
    }
    while let Some(c) = chunks.next() {
        v.push(chunks.peek().is_none() as u8);
        v.extend((c.len() as u16).to_le_bytes());
        v.extend((!(c.len() as u16)).to_le_bytes());
        v.extend(c);
    }
    v.extend(aqr_common::adler32::hash(d).to_be_bytes());
    v
}

fn shuffled(d: &[u8], size: usize) -> Vec<u8> {
    let n = d.len() / size;
    let mut out = vec![0; n * size];
    for i in 0..n {
        for j in 0..size {
            out[j * n + i] = d[i * size + j];
        }
    }
    out
}

fn superblock(version: u8, eof: u64, root: u64) -> Vec<u8> {
    let mut v = b"\x89HDF\r\n\x1a\n".to_vec();
    v.push(version);
    match version {
        0 | 1 => {
            v.extend([0, 0, 0, 0, 8, 8, 0]);
            v.extend(4u16.to_le_bytes());
            v.extend(16u16.to_le_bytes());
            v.extend(0u32.to_le_bytes());
            if version == 1 {
                v.extend([32, 0, 0, 0]);
            }
            put_addr(&mut v, 0);
            put_addr(&mut v, UNDEF);
            put_addr(&mut v, eof);
            put_addr(&mut v, UNDEF);
            // the root's symbol table entry
            put_addr(&mut v, 0);
            put_addr(&mut v, root);
            v.extend([0; 4 + 4 + 16]);
        }
        _ => {
            v.extend([8, 8, 0]);
            put_addr(&mut v, 0);
            put_addr(&mut v, UNDEF);
            put_addr(&mut v, eof);
            put_addr(&mut v, root);
            v.extend([0; 4]);
        }
    }
    v
}

/// A root object header (version 2) of `msgs`.
fn ohdr(msgs: &[u8]) -> Vec<u8> {
    let mut v = b"OHDR".to_vec();
    v.extend([2, 0, msgs.len() as u8]);
    v.extend(msgs);
    v.extend([0; 4]);
    v
}

impl Spec {
    fn new(sb: u8, class: u8, size: u32, be: bool, data: Vec<u8>) -> Spec {
        let n = data.len() as u64 / size as u64;
        Spec { sb, class, size, be, filters: vec![(1, vec![6])], dims: vec![n, size as u64], data }
    }

    /// The file: its superblock, root object header (the dataset's) and its one chunk.
    fn file(&self) -> Vec<u8> {
        let mut chunk = self.data.clone();
        for (id, client) in &self.filters {
            match id {
                2 => chunk = shuffled(&chunk, client.first().map_or(1, |&c| c.max(1) as usize)),
                1 => chunk = zlib_stored(&chunk),
                _ => {}
            }
        }
        let sb_len = superblock(self.sb, 0, 0).len();
        let msgs = |chunk_at: u64, chunk_len: u64| {
            let mut m = Vec::new();
            let mut dt = vec![self.class | 1 << 4, self.be as u8, 0, 0];
            dt.extend(self.size.to_le_bytes());
            dt.extend([0; 12]);
            message(&mut m, 0x03, &dt);
            let mut f = vec![2, self.filters.len() as u8];
            for (id, client) in &self.filters {
                f.extend(id.to_le_bytes());
                f.extend(0u16.to_le_bytes());
                f.extend((client.len() as u16).to_le_bytes());
                client.iter().for_each(|c| f.extend(c.to_le_bytes()));
            }
            message(&mut m, 0x0b, &f);
            let mut l = vec![4, 2, 2, self.dims.len() as u8, 8];
            self.dims.iter().for_each(|d| l.extend(d.to_le_bytes()));
            l.push(1);
            put_addr(&mut l, chunk_len);
            l.extend(0u32.to_le_bytes());
            put_addr(&mut l, chunk_at);
            message(&mut m, 0x08, &l);
            ohdr(&m)
        };
        let root = msgs(0, 0);
        let at = (sb_len + root.len()) as u64;
        let root = msgs(at, chunk.len() as u64);
        let mut d = superblock(self.sb, at + chunk.len() as u64, sb_len as u64);
        d.extend(root);
        d.extend(chunk);
        d
    }
}

/// Smooth floats of `n` samples: a random walk.
fn walk(n: usize, wide: bool, be: bool) -> Vec<u8> {
    let mut x = 0.0f64;
    let mut s = 12345u32;
    let mut d = Vec::new();
    for _ in 0..n {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        x += (s >> 16) as f64 / 65536.0 - 0.5;
        match (wide, be) {
            (true, true) => d.extend(x.to_be_bytes()),
            (true, false) => d.extend(x.to_le_bytes()),
            (false, true) => d.extend((x as f32).to_be_bytes()),
            (false, false) => d.extend((x as f32).to_le_bytes()),
        }
    }
    d
}

fn ints(n: usize) -> Vec<u8> {
    (0..n as u32).flat_map(|i| ((i * 7 + i * i / 50) as u16).to_le_bytes()).collect()
}

/// A dataset of integers is rebuilt through each of the superblocks (the object header is the
/// same: a v1 superblock only tells where the root is).
#[test]
fn superblocks() {
    for sb in 0..=3 {
        let s = Spec::new(sb, 0, 2, false, ints(4000));
        let d = s.file();
        assert_eq!(hdf5::superblock(&d), Some(0));
        assert!(formula::starts(&d), "sb {sb}");
        assert_eq!(formula::extent(&d), Some(d.len()), "sb {sb}");
        let f = hdf5::parse(&d).unwrap();
        assert_eq!((f.datasets.len(), f.chunks.len()), (1, 1), "sb {sb}");
        assert_eq!(roundtrip(&d, &Options::default()), Some((1, 0)), "sb {sb}");
    }
    // a superblock of a version that does not exist, or of sizes of offsets that are not
    for (at, v) in [(8, 4), (8, 255), (13, 3), (13, 16), (14, 0), (14, 1)] {
        let mut d = Spec::new(0, 0, 2, false, ints(4000)).file();
        d[at] = v;
        let _ = roundtrip(&d, &Options::default());
        let _ = formula::extent(&d);
    }
}

/// Float32 and big-endian datasets, shuffled or not, go through the float model (or are
/// kept, never rebuilt wrong).
#[test]
fn floats() {
    for (wide, be, shuffle) in
        [(false, false, false), (false, true, true), (true, true, false), (true, false, true), (false, false, true)]
    {
        let size = if wide { 8 } else { 4 };
        let mut s = Spec::new(3, 1, size, be, walk(3000, wide, be));
        if shuffle {
            s.filters.insert(0, (2, vec![size]));
        }
        let d = s.file();
        let (chunks, floats) = roundtrip(&d, &Options::default()).unwrap_or_else(|| panic!("{wide} {be} {shuffle}"));
        assert_eq!(chunks, 1);
        assert_eq!(floats, 1, "wide {wide}, big endian {be}, shuffle {shuffle}");
        assert_eq!(roundtrip(&d, &Options { floats: false, ..Options::default() }), Some((1, 0)));
    }
    // special values, a NaN with a payload, an element size that is not the type's, a
    // shuffle of another size
    let mut data = walk(3000, false, false);
    for (i, v) in [f32::NAN.to_bits(), f32::INFINITY.to_bits(), 0x8000_0000, 1, 0x7fc0_0001].into_iter().enumerate() {
        data[i * 400..i * 400 + 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut s = Spec::new(3, 1, 4, false, data.clone());
    roundtrip(&s.file(), &Options::default()).unwrap();
    s.filters.insert(0, (2, vec![8]));
    roundtrip(&s.file(), &Options::default());
    s.filters[0].1.clear();
    let _ = roundtrip(&s.file(), &Options::default());
    // a chunk of a size that no sample fits (6 bytes of 4-byte floats)
    let mut s = Spec::new(3, 1, 4, false, data[..2000 * 4 + 2].to_vec());
    s.dims = vec![2000 * 4 + 2, 1];
    let _ = roundtrip(&s.file(), &Options::default());
    // a size of 0, and one past the type's
    for size in [0, 3, 16, u32::MAX] {
        let mut s = Spec::new(3, 1, 4, false, data.clone());
        s.size = size;
        let _ = roundtrip(&s.file(), &Options::default());
    }
}

/// Dimensions and sizes the file lies about: the data is not the chunk's size, the product of
/// the dimensions wraps or overflows (with the 4 bytes of a Fletcher-32 added to it), the
/// chunk is of no bytes.
#[test]
fn lying_dimensions() {
    let data = ints(4000);
    let opt = Options::default();
    let base = Spec::new(3, 0, 2, false, data.clone());
    // (the truth first: the lie is the difference)
    assert!(roundtrip(&base.file(), &opt).is_some());
    for dims in [
        vec![4001, 2],
        vec![3999, 2],
        vec![0, 2],
        vec![4000, 0],
        vec![0],
        vec![],
        vec![u64::MAX],
        vec![u64::MAX - 3],
        vec![u64::MAX - 4],
        vec![1 << 63, 2],
        vec![1 << 32, 1 << 32],
        vec![4000, 2, u64::MAX],
        vec![u64::MAX, u64::MAX],
    ] {
        for filters in [
            vec![(1, vec![6])],
            vec![(3, vec![]), (1, vec![6])],
            vec![(2, vec![2]), (3, vec![]), (1, vec![6]), (3, vec![])],
            vec![(1, vec![6]), (3, vec![])],
        ] {
            let mut s = base.clone();
            s.dims = dims.clone();
            s.filters = filters;
            let name = format!("{dims:?}");
            let r = aqr_testkit::within(Duration::from_secs(10), || roundtrip(&s.file(), &opt));
            assert!(r.is_ok(), "{name}: {r:?}");
        }
    }
    // a chunk longer than the file, and one that is the file
    let mut d = base.file();
    let at = d.len() - 100;
    d.truncate(at);
    let _ = roundtrip(&d, &opt);
}

/// A fractal heap of a group whose sizes overflow when they are counted: the rows of its
/// indirect block start at 2^63 bytes.
#[test]
fn heap_sizes_overflow() {
    for (start, max_direct, width, rows) in [
        (1u64 << 63, 1u64 << 63, 2u64, 3u16),
        (1 << 63, 1 << 63, 65535, 3),
        (1 << 62, 1 << 62, 4, 4),
        (1 << 32, 1 << 63, 1 << 16, 64),
        (1, 1 << 63, 2, 64),
        (1 << 63, 1 << 63, 1, 65),
    ] {
        let mut d = superblock(3, 0, 0);
        let sb_len = d.len();
        // the root: a link info message to the heap
        let mut link = vec![0, 0];
        let (heap, btree) = (0u64, UNDEF);
        put_addr(&mut link, heap);
        put_addr(&mut link, btree);
        let mut m = Vec::new();
        message(&mut m, 0x02, &link);
        let root_len = ohdr(&m).len();
        let heap_at = (sb_len + root_len) as u64;
        link.splice(2..10, heap_at.to_le_bytes());
        let mut m = Vec::new();
        message(&mut m, 0x02, &link);
        let root = ohdr(&m);
        // the heap's header
        let mut h = b"FRHP".to_vec();
        h.extend([0, 8, 0]);
        h.extend(0u16.to_le_bytes());
        h.push(0);
        h.extend([0; 4]);
        h.extend([0; 8]);
        put_addr(&mut h, UNDEF);
        h.extend([0; 8]);
        put_addr(&mut h, UNDEF);
        h.extend([0; 8 * 8]);
        h.extend((width as u16).to_le_bytes());
        h.extend(start.to_le_bytes());
        h.extend(max_direct.to_le_bytes());
        h.extend(64u16.to_le_bytes());
        h.extend([0; 2]);
        let block_at = heap_at + h.len() as u64 + 8 + 2 + 8;
        put_addr(&mut h, block_at);
        h.extend(rows.to_le_bytes());
        h.extend([0; 8]);
        // its root indirect block: every child at the same place
        let mut b = b"FHIB".to_vec();
        b.push(0);
        put_addr(&mut b, heap_at);
        b.extend([0; 8]);
        for _ in 0..(rows as u64 * width).min(1 << 17) {
            put_addr(&mut b, block_at);
        }
        d.extend(root);
        d.extend(h);
        d.extend(b);
        let eof = d.len() as u64;
        let name = format!("start {start}, max {max_direct}, width {width}, rows {rows}");
        d[8 + 3 + 1 + 8 + 8..8 + 3 + 1 + 8 + 8 + 8].copy_from_slice(&eof.to_le_bytes());
        d[8 + 3 + 1 + 8 + 8 + 8..8 + 3 + 1 + 8 + 8 + 16].copy_from_slice(&(sb_len as u64).to_le_bytes());
        let r = aqr_testkit::within(Duration::from_secs(10), || {
            let _ = hdf5::parse(&d);
            roundtrip(&d, &Options::default())
        });
        assert!(r.is_ok(), "{name}: {r:?}");
    }
}

/// Every kind of file above damaged: what is taken evaluates to the input.
#[test]
fn damaged_files() {
    let mut files = Vec::new();
    for sb in [0, 1, 2, 3] {
        files.push(Spec::new(sb, 0, 2, false, ints(1500)).file());
    }
    let mut s = Spec::new(3, 1, 4, true, walk(1200, false, true));
    s.filters.insert(0, (2, vec![4]));
    s.filters.insert(1, (3, vec![]));
    files.push(s.file());
    for (i, d) in files.iter().enumerate() {
        aqr_testkit::damaged(d, 40, 400, Duration::from_secs(20), |v| {
            let _ = roundtrip(v, &Options::default());
            let _ = formula::extent(v);
            let _ = formula::after_user_block(v);
        });
        let _ = i;
    }
}
