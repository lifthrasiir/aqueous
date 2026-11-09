//! LZNT1 streams split into their data and record and rebuilt: the record's share of the
//! compressed bytes, by what it codes, and the speeds of splitting and joining (one thread, the
//! data's MB/s).
//!
//! A file is the clusters of a compressed NTFS file in order, named `lznt1-<n>-<data length>-<unit
//! length>.bin` with `lznt1-<n>-<data length>-<unit length>.alloc` beside it (a line for each
//! unit: the bytes it holds, 0 for a hole), or a buffer of `RtlCompressBuffer` (`NAME.lznt1`,
//! whose data length is that of `NAME.raw` beside it). With `-n K` each is split and joined K
//! times for the speeds.
//!
//! ```sh
//! cargo run --release -p aqr-lznt1 --example aqr-lznt1-check -- [-n K] FILE...
//! ```

use std::time::Instant;

use aqr_lznt1::recons::{self, CATS};

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
    let names = ["kind", "length", "offset", "rest"];
    let mut total = ([0f64; CATS], 0usize, 0usize);
    for f in &a {
        let d = std::fs::read(f).unwrap();
        let base = f.rsplit('/').next().unwrap();
        let units: Vec<(&[u8], usize)> = if let Some(stem) = base.strip_suffix(".bin") {
            let p: Vec<usize> = stem.split('-').skip(2).map(|x| x.parse().unwrap()).collect();
            let alloc = std::fs::read_to_string(f.replace(".bin", ".alloc")).unwrap();
            let (mut at, mut left) = (0, p[0]);
            alloc
                .lines()
                .map(|l| {
                    let n: usize = l.trim().parse().unwrap();
                    let len = p[1].min(left);
                    left -= len;
                    at += n;
                    (&d[at - n..at], len)
                })
                .collect()
        } else {
            let raw = f.rsplit_once('/').map_or(String::new(), |(a, _)| format!("{a}/"))
                + base.split('.').next().unwrap()
                + ".raw";
            vec![(&d[..], std::fs::metadata(raw).unwrap().len() as usize)]
        };
        let (holes, stored) =
            (units.iter().filter(|c| c.0.is_empty()).count(), units.iter().filter(|c| c.0.len() == c.1).count());
        let t = Instant::now();
        let mut s = None;
        for _ in 0..times {
            s = Some(recons::split(&units));
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
        let want: Vec<u8> = units.iter().flat_map(|c| c.0.iter().copied()).collect();
        assert!(j == want, "{f}: not rebuilt");
        let comp = want.len();
        let mb = s.data.len() as f64 / 1e6;
        let cost: f64 = s.cost.iter().sum();
        print!(
            "{base}: {} units ({holes} holes, {stored} stored), {} -> {} compressed, record {} ({:.2}%), split {:.1} MB/s, join {:.1} MB/s;",
            units.len(),
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
