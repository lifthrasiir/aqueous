//! usage: aqr-wasm-bench [-l LEVEL] FILE...
//!
//! For each Wasm module: that its streams join back, and zstd at LEVEL (22) of the module
//! against that of its streams one after another (after their lengths) in one frame, and of
//! each stream alone; then the totals, the speeds of splitting and joining, and each stream's
//! share of the totals.

use aqr_wasm::wasm::{self, STREAM_NAMES, STREAMS};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut level = 22;
    if args.first().is_some_and(|a| a == "-l") {
        level = args[1].parse().unwrap();
        args.drain(..2);
    }
    let z = |d: &[u8]| zstd::bulk::compress(d, level).unwrap().len();
    let (mut raw, mut plain, mut joined, mut apart) = (0, 0, 0, 0);
    let (mut te, mut td) = (0f64, 0f64);
    let (mut by, mut zby) = ([0usize; STREAMS], [0usize; STREAMS]);
    for path in &args {
        let d = std::fs::read(path).unwrap();
        let t = std::time::Instant::now();
        let st = wasm::split(&d);
        te += t.elapsed().as_secs_f64();
        let Some(st) = st else {
            println!("{path}: not taken");
            continue;
        };
        let t = std::time::Instant::now();
        let back = wasm::join(std::array::from_fn(|i| &st[i][..]), d.len() as u64).unwrap();
        td += t.elapsed().as_secs_f64();
        assert!(back == d, "{path}: not given back");
        let mut one = Vec::new();
        for s in &st {
            aqr_common::varint::put(&mut one, s.len() as u64);
        }
        for s in &st {
            one.extend_from_slice(s);
        }
        if let Ok(dir) = std::env::var("DUMP") {
            let name = std::path::Path::new(path).file_name().unwrap();
            std::fs::write(std::path::Path::new(&dir).join(name), &one).unwrap();
        }
        let (p, j) = (z(&d), z(&one));
        let mut a = 0;
        for (i, s) in st.iter().enumerate() {
            by[i] += s.len();
            let n = if s.is_empty() { 0 } else { z(s) };
            zby[i] += n;
            a += n;
        }
        println!("{path}: {} -> {p} plain, {j} joined, {a} apart", d.len());
        (raw, plain, joined, apart) = (raw + d.len(), plain + p, joined + j, apart + a);
    }
    println!("total: {raw} -> {plain} plain, {joined} joined ({:.4}), {apart} apart", joined as f64 / plain as f64);
    println!("split {:.0} MB/s, join {:.0} MB/s (one thread)", raw as f64 / 1e6 / te, raw as f64 / 1e6 / td);
    for i in 0..STREAMS {
        println!("  {:10} {:8} -> {:7}", STREAM_NAMES[i], by[i], zby[i]);
    }
}
