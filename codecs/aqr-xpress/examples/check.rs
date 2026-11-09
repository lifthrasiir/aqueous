//! XPRESS Huffman streams split into their data and record and rebuilt: the record's share of
//! the compressed bytes, by what it codes, and the speeds of splitting and joining (one thread,
//! the data's MB/s).
//!
//! A file is a WOF stream as NTFS holds it, named `<algorithm>-<n>-<data length>.wof` (algorithm
//! 0, 2, 3: chunks of 4, 8, 16 KiB), or a buffer of `RtlCompressBuffer` (`NAME.xph`,
//! `NAME.max.xph`, whose data length is that of `NAME.raw` beside it). With `-n K` each is
//! split and joined K times for the speeds.
//!
//! ```sh
//! cargo run --release -p aqr-xpress --example aqr-xpress-check -- [-n K] FILE...
//! ```

use std::time::Instant;

use aqr_xpress::recons::{self, CATS};

/// The chunks of a WOF stream of `n` bytes of data in chunks of `cs`.
fn wof(d: &[u8], n: usize, cs: usize) -> Vec<(&[u8], usize)> {
    let count = n.div_ceil(cs);
    let base = 4 * (count - 1);
    let mut offs = vec![0usize];
    for i in 0..count - 1 {
        offs.push(u32::from_le_bytes(d[4 * i..4 * i + 4].try_into().unwrap()) as usize);
    }
    offs.push(d.len() - base);
    (0..count).map(|i| (&d[base + offs[i]..base + offs[i + 1]], cs.min(n - i * cs))).collect()
}

fn main() {
    let mut a: Vec<String> = std::env::args().skip(1).collect();
    let times = match a.iter().position(|x| x == "-n") {
        Some(i) => {
            let k = a[i + 1].parse().unwrap();
            a.drain(i..i + 2);
            k
        }
        None => 1,
    };
    let names = ["kind", "length", "offset", "table", "rest"];
    let mut total = ([0f64; CATS], 0usize, 0usize);
    for f in &a {
        let d = std::fs::read(f).unwrap();
        let base = f.rsplit('/').next().unwrap();
        let chunks = if let Some(stem) = base.strip_suffix(".wof") {
            let p: Vec<usize> = stem.split('-').map(|x| x.parse().unwrap()).collect();
            let cs = match p[0] {
                0 => 4096,
                2 => 8192,
                3 => 16384,
                _ => panic!("not XPRESS"),
            };
            wof(&d, p[2], cs)
        } else {
            let raw = f.rsplit_once('/').map_or(String::new(), |(a, _)| format!("{a}/"))
                + base.split('.').next().unwrap()
                + ".raw";
            vec![(&d[..], std::fs::metadata(raw).unwrap().len() as usize)]
        };
        let stored = chunks.iter().filter(|c| c.0.len() == c.1).count();
        let t = Instant::now();
        let mut s = None;
        for _ in 0..times {
            s = Some(recons::split(&chunks));
        }
        let split = t.elapsed().as_secs_f64() / times as f64;
        let s = match s.unwrap() {
            Ok(s) => s,
            Err(e) => {
                println!("{f}: {e}");
                continue;
            }
        };
        let t = Instant::now();
        let mut j = Vec::new();
        for _ in 0..times {
            j = recons::join(&s.data, &s.record).unwrap();
        }
        let join = t.elapsed().as_secs_f64() / times as f64;
        let want: Vec<u8> = if chunks.len() == 1 {
            chunks[0].0.to_vec()
        } else {
            // (the chunks one after another: a WOF stream without its table)
            chunks.iter().flat_map(|c| c.0.iter().copied()).collect()
        };
        assert!(j == want, "{f}: not rebuilt");
        let comp: usize = chunks.iter().map(|c| c.0.len()).sum();
        let mb = s.data.len() as f64 / 1e6;
        let cost: f64 = s.cost.iter().sum();
        print!(
            "{base}: {} chunks ({stored} stored), {} -> {} compressed, record {} ({:.2}%), split {:.1} MB/s, join {:.1} MB/s;",
            chunks.len(),
            s.data.len(),
            comp,
            s.record.len(),
            100.0 * s.record.len() as f64 / comp as f64,
            mb / split,
            mb / join
        );
        for (n, c) in names.iter().zip(s.cost) {
            print!(" {n} {:.2}%", 100.0 * c / 8.0 / comp as f64);
        }
        println!(" (of {:.0} cost bytes)", cost / 8.0);
        for i in 0..CATS {
            total.0[i] += s.cost[i];
        }
        total.1 += comp;
        total.2 += s.record.len();
    }
    println!("all: record {} of {} ({:.2}%)", total.2, total.1, 100.0 * total.2 as f64 / total.1.max(1) as f64);
}
