//! The LZX chunks of a WIM file: whether each is read and written back bit for bit, the
//! resources' SHA-1, and what the chunks are made of. The WIM may be a range of a file (a WIM
//! in a disc image).
//!
//! ```sh
//! cargo run --release -p aqr-lzx --example aqr-lzx-check -- FILE [OFFSET LEN]
//! ```

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use aqr_lzx::lzx;
use sha1::{Digest, Sha1};

const CHUNK: usize = 32768;

struct Res {
    size: u64,
    flags: u8,
    offset: u64,
    orig: u64,
    hash: [u8; 20],
}

fn reshdr(b: &[u8]) -> (u64, u8, u64, u64) {
    let mut s = [0u8; 8];
    s[..7].copy_from_slice(&b[..7]);
    let q = |p: usize| u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
    (u64::from_le_bytes(s), b[7], q(8), q(16))
}

/// The chunks of a compressed resource: its bytes and its data's length.
fn chunks<'a>(w: &'a [u8], r: &Res) -> Vec<(&'a [u8], usize)> {
    let n = (r.orig as usize).div_ceil(CHUNK);
    let es = if r.orig > u32::MAX as u64 { 8 } else { 4 };
    let base = r.offset as usize;
    let data = base + (n - 1) * es;
    let mut offs = vec![0usize];
    for i in 0..n - 1 {
        let e = &w[base + i * es..base + i * es + es];
        let mut v = [0u8; 8];
        v[..es].copy_from_slice(e);
        offs.push(u64::from_le_bytes(v) as usize);
    }
    offs.push(base + r.size as usize - data);
    (0..n).map(|i| (&w[data + offs[i]..data + offs[i + 1]], CHUNK.min(r.orig as usize - i * CHUNK))).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let file = std::fs::File::open(&a[0]).unwrap();
    let m = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let (off, len) = match a.len() {
        3 => (a[1].parse().unwrap(), a[2].parse().unwrap()),
        _ => (0, m.len()),
    };
    let w = &m[off..off + len];
    assert_eq!(&w[..8], b"MSWIM\0\0\0", "not a WIM");
    let (lsize, _, loff, _) = reshdr(&w[48..72]);
    let res: Vec<Res> = w[loff as usize..(loff + lsize) as usize]
        .as_chunks::<50>()
        .0
        .iter()
        .map(|e| {
            let (size, flags, offset, orig) = reshdr(&e[..24]);
            Res { size, flags, offset, orig, hash: e[30..50].try_into().unwrap() }
        })
        .filter(|r| r.flags & 4 != 0 && r.orig > 0)
        .collect();
    let (chunks_n, stored, unread, unwritten, bad_hash) =
        (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let (blocks, uncompressed, trailing, tail) =
        (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let start = std::time::Instant::now();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Relaxed);
                    let Some(r) = res.get(i) else { break };
                    let mut h = Sha1::new();
                    for (c, n) in chunks(w, r) {
                        chunks_n.fetch_add(1, Relaxed);
                        if c.len() == n {
                            stored.fetch_add(1, Relaxed);
                            h.update(c);
                            continue;
                        }
                        let Some((mut data, parse)) = lzx::read(c, n) else {
                            if unread.fetch_add(1, Relaxed) == 0
                                && let Ok(path) = std::env::var("AQR_LZX_DUMP")
                            {
                                std::fs::write(path, [&(n as u32).to_le_bytes()[..], c].concat()).unwrap();
                            }
                            continue;
                        };
                        blocks.fetch_add(parse.blocks.len(), Relaxed);
                        let u = parse.blocks.iter().filter(|b| b.kind == lzx::UNCOMPRESSED).count();
                        uncompressed.fetch_add(u, Relaxed);
                        trailing.fetch_add(!parse.trailing.is_empty() as usize, Relaxed);
                        tail.fetch_add((parse.tail_bits != 0) as usize, Relaxed);
                        if lzx::write(&data, &parse).as_deref() != Some(c) {
                            unwritten.fetch_add(1, Relaxed);
                        }
                        lzx::e8_decode(&mut data);
                        h.update(&data);
                    }
                    if h.finalize()[..] != r.hash {
                        bad_hash.fetch_add(1, Relaxed);
                    }
                }
            });
        }
    });
    println!(
        "{} resources, {} chunks ({} stored) in {:.1} s: not read {}, not written back {}, SHA-1 wrong {}",
        res.len(),
        chunks_n.into_inner(),
        stored.into_inner(),
        start.elapsed().as_secs_f64(),
        unread.into_inner(),
        unwritten.into_inner(),
        bad_hash.into_inner()
    );
    println!(
        "blocks {} ({} uncompressed); chunks with trailing bytes {}, with padding bits not zero {}",
        blocks.into_inner(),
        uncompressed.into_inner(),
        trailing.into_inner(),
        tail.into_inner()
    );
}
