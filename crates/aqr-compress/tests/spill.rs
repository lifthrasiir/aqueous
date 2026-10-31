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

/// With a spill, large buffers of streams are kept in it rather than on the heap: the same
/// archive, less of the heap at most, and the spill gone once done.
#[test]
fn spilled() {
    let t = text(24 << 20, 4);
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&t).unwrap();
    let d = z.finish().unwrap();
    let opt = Options { threads: 4, ..Options::default() };
    let (heap, heap_used) = peak(|| aqr_compress::compress(&d, &opt));
    let path = std::env::temp_dir().join(format!("aqr-compress-spilled-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let spilling = Options { spill: Some(path.clone()), ..opt };
    let (spilled, spilled_used) = peak(|| aqr_compress::compress(&d, &spilling));
    println!("peak {:.1} MB, {:.1} MB spilling", heap_used as f64 / 1e6, spilled_used as f64 / 1e6);
    assert!(spilled == heap);
    assert!(aqr_decompress::decompress(&spilled, 1).unwrap() == d);
    assert!(spilled_used + (16 << 20) < heap_used, "{spilled_used} spilling, {heap_used} not");
    assert!(std::fs::read_dir(&path).unwrap().next().is_none());
    std::fs::remove_dir(&path).unwrap();
}
