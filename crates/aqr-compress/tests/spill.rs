//! Peak heap use of archiving with a spill ([`aqr_compress::Options::spill`]). (A test binary
//! of its own: the allocator counts every thread.)

use std::io::Write;

use aqr_common::mem::{Counting, peak};
use aqr_compress::Options;

#[global_allocator]
static A: Counting = Counting;

fn text(n: usize, seed: usize) -> Vec<u8> {
    let mut t = Vec::with_capacity(n + 100);
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", (i + seed) * 7 % 13).unwrap();
    }
    t
}

fn gzip(t: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(t).unwrap();
    z.finish().unwrap()
}

/// A tar archive of `files` (ustar, named by their index).
fn tar(files: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let mut h = [0u8; 512];
        let name = format!("f{i}.gz");
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[124..135].copy_from_slice(format!("{:011o}", f.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[148..156].fill(b' ');
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(f);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

/// With a spill, large buffers of streams are kept in it rather than on the heap: the same
/// archive, less of the heap at most (the data of the files of a tar archive, inflated one
/// after another, not all held), and the spill gone once done.
#[test]
fn spilled() {
    let d = tar(&(0..4).map(|i| gzip(&text(8 << 20, i))).collect::<Vec<_>>());
    let opt = Options { threads: 4, ..Options::default() };
    let (heap, heap_used) = peak(|| aqr_compress::compress(&d, &opt));
    let path = std::env::temp_dir().join(format!("aqr-compress-spilled-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let spilling = Options { spill: Some(path.join(".out")), ..opt };
    let (spilled, spilled_used) = peak(|| aqr_compress::compress(&d, &spilling));
    println!("peak {:.1} MB, {:.1} MB spilling", heap_used as f64 / 1e6, spilled_used as f64 / 1e6);
    assert!(spilled == heap);
    assert!(aqr_decompress::decompress(&spilled, 1).unwrap() == d);
    // (52–53 MB, and 97–110 MB without: the data of the files before held as the next is made)
    assert!(spilled_used + (8 << 20) < heap_used, "{spilled_used} spilling, {heap_used} not");
    assert!(std::fs::read_dir(&path).unwrap().next().is_none());
    std::fs::remove_dir(&path).unwrap();
}
