//! usage: aqr-tex-bench [-j FILES] [-t THREADS] [-z LEVEL] FILE... | @LIST
//!
//! For each DDS file (or each path of LIST): its blocks coded and decoded back, checked, by
//! format: the ratio (the coded blocks, and the rest of the file as zstd -19 takes it), the
//! bits of each part, and the speeds a thread (of the blocks' bytes). `-z` adds zstd at that
//! level of the whole file, for reference. Files are taken several at once (`-j`, by default as
//! many as the machine has threads), each coded and decoded on up to `-t` threads (1); the
//! speeds are of a file's bytes by the time it takes.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aqr_tex::{dds, texture};

#[derive(Default)]
struct Total {
    files: usize,
    bytes: usize,
    blocks: usize,
    coded: usize,
    rest: usize,
    zstd: usize,
    enc: f64,
    dec: f64,
    cost: [u64; aqr_tex::coder::CATS],
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let (mut threads, mut level, mut within) = (std::thread::available_parallelism().map_or(1, |n| n.get()), 0, 1);
    let mut paths = vec![];
    while let Some(a) = args.next() {
        match a.as_str() {
            "-j" => threads = args.next().unwrap().parse().unwrap(),
            "-t" => within = args.next().unwrap().parse().unwrap(),
            "-z" => level = args.next().unwrap().parse().unwrap(),
            _ => match a.strip_prefix('@') {
                Some(list) => paths.extend(std::fs::read_to_string(list).unwrap().lines().map(str::to_owned)),
                None => paths.push(a),
            },
        }
    }
    paths.retain(|p| !p.is_empty());
    let next = AtomicUsize::new(0);
    let totals: Mutex<BTreeMap<&'static str, Total>> = Mutex::default();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(path) = paths.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let d = std::fs::read(path).unwrap();
                    let Some(found) = dds::detect(&d) else {
                        eprintln!("{path}: not taken");
                        continue;
                    };
                    let dds::Data::Blocks(l) = found.data else {
                        eprintln!("{path}: not of blocks");
                        continue;
                    };
                    let blocks = &d[found.start..found.start + l.len() as usize];
                    let t = Instant::now();
                    let (main, apart, cost) = texture::encode(&l, blocks, within);
                    let enc = t.elapsed().as_secs_f64();
                    let t = Instant::now();
                    let back = texture::decode(&l, &main, &apart, within).unwrap_or_else(|e| panic!("{path}: {e}"));
                    let dec = t.elapsed().as_secs_f64();
                    assert!(back == blocks, "{path}: not given back");
                    let mut rest = d[..found.start].to_vec();
                    rest.extend_from_slice(&d[found.start + blocks.len()..]);
                    let rest = zstd::bulk::compress(&rest, 19).unwrap().len();
                    let z = if level > 0 { zstd::bulk::compress(&d, level).unwrap().len() } else { 0 };
                    let mut t = totals.lock().unwrap();
                    let e = t.entry(l.format.name()).or_default();
                    e.files += 1;
                    e.bytes += d.len();
                    e.blocks += blocks.len();
                    e.coded += main.len() + apart.len();
                    e.rest += rest;
                    e.zstd += z;
                    e.enc += enc;
                    e.dec += dec;
                    for (a, c) in e.cost.iter_mut().zip(cost) {
                        *a += c;
                    }
                }
            });
        }
    });
    let mut all = Total::default();
    let show = |name: &str, t: &Total| {
        let parts: Vec<String> = ["choice", "cache", "endpoints", "indices", "alpha"]
            .iter()
            .zip(t.cost)
            .filter(|(_, c)| *c > 0)
            .map(|(n, c)| format!("{n} {:.4}", c as f64 / 65536.0 / 8.0 / t.bytes as f64))
            .collect();
        let mb = t.blocks as f64 / 1e6;
        println!(
            "{name}: {} files, {:.1} MB -> {:.4}{} [{}], encode {:.1} MB/s, decode {:.1} MB/s",
            t.files,
            t.bytes as f64 / 1e6,
            (t.coded + t.rest) as f64 / t.bytes as f64,
            if t.zstd > 0 { format!(" (zstd {:.4})", t.zstd as f64 / t.bytes as f64) } else { String::new() },
            parts.join(", "),
            mb / t.enc,
            mb / t.dec,
        );
    };
    for (name, t) in totals.into_inner().unwrap() {
        show(name, &t);
        all.files += t.files;
        all.bytes += t.bytes;
        all.blocks += t.blocks;
        all.coded += t.coded;
        all.rest += t.rest;
        all.zstd += t.zstd;
        all.enc += t.enc;
        all.dec += t.dec;
        for (a, c) in all.cost.iter_mut().zip(t.cost) {
            *a += c;
        }
    }
    show("all", &all);
}
