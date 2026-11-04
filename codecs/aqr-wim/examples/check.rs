//! What aqr-wim makes of WIM files: the resources taken apart, the formula's streams, whether
//! it gives each file back (natively), and how fast. The WIM may be a range of a file (a WIM in
//! a disc image).
//!
//! ```sh
//! cargo run --release -p aqr-wim --example aqr-wim-check -- FILE [OFFSET LEN]
//! ```

use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let file = std::fs::File::open(&a[0]).unwrap();
    let m = unsafe { memmap2::Mmap::map(&file).unwrap() };
    let (off, len) = match a.len() {
        3 => (a[1].parse().unwrap(), a[2].parse().unwrap()),
        _ => (0, m.len()),
    };
    let d = &m[off..off + len];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let w = aqr_wim::wim::parse(d).expect("a WIM taken");
    let (mut chunked, mut data) = (0, 0);
    for r in &w.resources {
        match r {
            aqr_wim::wim::Resource::Chunks { .. } => chunked += 1,
            aqr_wim::wim::Resource::Data(_) => data += 1,
        }
    }
    println!("flags {:#x}: {} resources taken ({chunked} of chunks, {data} as they are)", w.flags, w.resources.len());
    let t = Instant::now();
    let f = aqr_wim::formula::encode(d, threads).expect("a formula");
    let enc = t.elapsed().as_secs_f64();
    // (a digest of the streams, records among them: the same of every build that splits alike)
    let mut digest = std::collections::hash_map::DefaultHasher::new();
    for s in &f.streams {
        std::hash::Hash::hash(&s.class, &mut digest);
        std::hash::Hash::hash(&s.data[..], &mut digest);
    }
    let classes: std::collections::BTreeMap<&str, (usize, usize)> =
        f.streams.iter().fold(Default::default(), |mut m, s| {
            let e = m.entry(&s.class[..]).or_default();
            *e = (e.0 + 1, e.1 + s.data.len());
            m
        });
    for (class, (n, len)) in classes {
        println!("  {class:<12} {n:>6} streams {len:>14} bytes");
    }
    println!("  digest {:016x}", std::hash::Hasher::finish(&digest));
    let t = Instant::now();
    let out = f.eval(&aqr_decompress::Native { threads }).expect("evaluated");
    let dec = t.elapsed().as_secs_f64();
    println!(
        "rebuilt: {}; formula in {enc:.1} s ({:.0} MB/s), evaluated in {dec:.1} s ({:.0} MB/s)",
        out == d,
        d.len() as f64 / enc / 1e6,
        d.len() as f64 / dec / 1e6
    );
}
