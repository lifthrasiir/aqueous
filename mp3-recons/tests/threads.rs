//! Threads: the compressed bytes are the same for every number of threads, and decompression
//! with threads round-trips and fails as with one thread. The files of tests/data are
//! repeated so that the inputs are large enough for the threaded paths (several thousand
//! frames, so that the output is also assembled in pieces).

use mp3_recons::{compress, compress_threads, decompress, decompress_threads};

const THREADS: [usize; 3] = [2, 3, 8];

fn files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

/// `d` repeated up to at least 600 KB.
fn big(d: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    while v.len() < 600 << 10 {
        v.extend_from_slice(d);
    }
    v
}

fn check(name: &str, d: &[u8]) {
    let c = compress(d);
    assert_eq!(decompress(&c).unwrap(), d, "{name}: one thread");
    for t in THREADS {
        assert!(compress_threads(d, t) == c, "{name}: compressed with {t} threads differs");
        assert!(decompress_threads(&c, t).unwrap() == d, "{name}: decompressed with {t} threads");
    }
}

#[test]
fn same_output() {
    let all = files();
    for (name, d) in &all {
        let b = big(d);
        check(name, &b);
        let c = compress(&b);
        assert!(c.len() < b.len(), "{name}: not coded");
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&b);
        j.extend_from_slice(b"TAG and after");
        check(name, &j);
        for cut in [1, 417, b.len() / 3 + 5] {
            check(name, &b[..b.len() - cut]);
            check(name, &b[cut..]);
        }
        // Streams concatenated (sample rate and mode change).
        let mut two = b.clone();
        for (_, e) in &all {
            two.extend_from_slice(e);
        }
        check(name, &two);
    }
}

#[test]
fn small_inputs() {
    for (name, d) in files() {
        check(&name, &d);
    }
}

/// The result of a decompression, comparable.
fn outcome(r: Result<Vec<u8>, mp3_recons::container::Error>) -> Result<u64, String> {
    r.map(|v| xxhash_rust::xxh3::xxh3_64(&v)).map_err(|e| e.0)
}

#[test]
fn corrupt_containers() {
    for (name, d) in files().into_iter().take(4) {
        let c = compress(&big(&d));
        let mut cases = Vec::new();
        for i in (9..c.len()).step_by(c.len() / 150) {
            let mut m = c.clone();
            m[i] ^= 0x5a;
            cases.push(m);
        }
        // The first bytes (lengths of the streams) and the end.
        for i in 9..68 {
            let mut m = c.clone();
            m[i] ^= 0x21;
            cases.push(m);
        }
        // Inside each stream, alone and with the side stream (last) not decompressing.
        // The side stream is stored plain (range coded); zstd it to have one that fails.
        let st = streams(&c);
        let (hdr_end, sp, sn) = st[7];
        let mut zc = c[..hdr_end].to_vec();
        let z = zstd::bulk::compress(&c[sp..sp + sn], 1).unwrap();
        put_varint(&mut zc, (z.len() as u64) << 1 | 1);
        put_varint(&mut zc, sn as u64);
        let zside = zc.len();
        zc.extend_from_slice(&z);
        zc.extend_from_slice(&c[sp + sn..]);
        assert!(decompress(&zc).is_ok() && decompress_threads(&zc, 2).is_ok(), "{name}: zstd side stream");
        for &(_, p, n) in &st {
            for q in [p, p + n / 2, p + n - 1] {
                if n == 0 {
                    continue;
                }
                let mut m = c.clone();
                m[q] ^= 0x5a;
                cases.push(m);
                if q < sp {
                    let mut m = zc.clone();
                    m[q] ^= 0x5a;
                    m[zside] ^= 0x5a; // the zstd magic number
                    cases.push(m);
                }
            }
        }
        for cut in [6, 20, 1000, c.len() / 2] {
            cases.push(c[..c.len() - cut].to_vec());
        }
        for m in &cases {
            let one = outcome(decompress(m));
            for t in THREADS {
                assert_eq!(outcome(decompress_threads(m, t)), one, "{name}: {t} threads");
            }
        }
    }
}

/// Offsets (of the length, of the data) and lengths of the stored byte streams of a coded
/// container.
fn streams(c: &[u8]) -> Vec<(usize, usize, usize)> {
    assert_eq!(c[8], 1);
    let mut p = 25;
    let varint = |p: &mut usize| {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let b = c[*p];
            *p += 1;
            v |= ((b & 0x7f) as u64) << s;
            s += 7;
            if b < 0x80 {
                return v;
            }
        }
    };
    let mut out = Vec::new();
    for _ in 0..8 {
        let start = p;
        let v = varint(&mut p);
        if v & 1 != 0 {
            varint(&mut p);
        }
        let n = (v >> 1) as usize;
        out.push((start, p, n));
        p += n;
    }
    out
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A zstd stream that decompresses to fewer bytes than its declared size is rejected.
#[test]
fn short_zstd_stream() {
    let (name, d) = &files()[0];
    let c = compress(&big(d));
    let mut done = 0;
    for (start, p, _) in streams(&c) {
        if c[start] & 1 == 0 {
            continue;
        }
        // Declared size one larger.
        let mut q = start;
        let skip = |q: &mut usize| while c[std::mem::replace(q, *q + 1)] >= 0x80 {};
        skip(&mut q);
        let raw_at = q;
        skip(&mut q);
        assert_eq!(q, p);
        let (mut raw, mut s) = (0u64, 0);
        for &b in &c[raw_at..p] {
            raw |= ((b & 0x7f) as u64) << s;
            s += 7;
        }
        let mut m = c[..raw_at].to_vec();
        put_varint(&mut m, raw + 1);
        m.extend_from_slice(&c[p..]);
        let one = outcome(decompress(&m));
        assert_eq!(one, Err("stream size".into()), "{name}: stream at {start}");
        for t in THREADS {
            assert_eq!(outcome(decompress_threads(&m, t)), one, "{name}: {t} threads");
        }
        done += 1;
    }
    assert!(done > 0);
}
