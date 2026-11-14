//! aqr-minecraft's tool: region files encoded into their formulas and made again.
//!
//! `aqr-minecraft check [-t N] [-z LEVEL] FILE_OR_DIR...`: each `.mca` file (of a directory, all
//! of them) encoded and decoded back (natively, on N threads) and compared; the streams' bytes
//! and, with `-z`, each kind of stream of all the files compressed by zstd at LEVEL, files of a
//! directory together in batches of 64 MiB.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::{Formula, Node};

fn files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p).unwrap().flatten().map(|e| e.path()).collect();
        v.sort();
        for e in v {
            files(&e, out);
        }
    } else if p.extension().is_some_and(|e| e == "mca") {
        out.push(p.to_path_buf());
    }
}

/// The streams of the application at the root of `f`, its inputs in order.
fn inputs(f: &Formula) -> Vec<&[u8]> {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    a.inputs
        .iter()
        .map(|n| match n {
            Node::Slice { stream, .. } => &f.streams[*stream].data[..],
            _ => panic!("not a stream"),
        })
        .collect()
}

const NAMES: [&str; 4] = ["header", "layout", "data", "kept"];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut level = None;
    let mut paths = Vec::new();
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("check") => {}
        _ => {
            eprintln!("usage: aqr-minecraft check [-t N] [-z LEVEL] FILE_OR_DIR...");
            std::process::exit(2);
        }
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().unwrap().parse().unwrap(),
            "-z" => level = Some(it.next().unwrap().parse::<i32>().unwrap()),
            p => files(Path::new(p), &mut paths),
        }
    }
    // (files at once, each on a thread)
    let (mut enc, mut dec) = (0f64, 0f64);
    let mut input = 0u64;
    let mut streams = [0u64; 4];
    let mut batches: [Vec<Vec<u8>>; 4] = Default::default();
    let mut current: [Vec<u8>; 4] = Default::default();
    let (mut taken, mut failed) = (0, 0);
    let t0 = Instant::now();
    for group in paths.chunks(threads.max(1)) {
        let res = aqr_common::pool::par_map(group.len(), threads, |i| {
            let d = std::fs::read(&group[i]).unwrap();
            let t = Instant::now();
            let f = aqr_minecraft::formula::encode(&d, 1);
            let e = t.elapsed().as_secs_f64();
            let Some(f) = f else { return (d.len(), None) };
            let t = Instant::now();
            let ins = inputs(&f);
            let Node::Apply(a) = &f.root else { unreachable!() };
            let out = aqr_minecraft::formula::apply(a, &ins, 1);
            let dt = t.elapsed().as_secs_f64();
            let ok = out.as_deref() == Ok(&d[..]);
            let ins: Vec<Vec<u8>> = ins.iter().map(|s| s.to_vec()).collect();
            (d.len(), Some((ok, e, dt, ins)))
        });
        for (len, r) in res {
            input += len as u64;
            let Some((ok, e, dt, ins)) = r else { continue };
            if !ok {
                failed += 1;
                continue;
            }
            taken += 1;
            enc += e;
            dec += dt;
            for (k, s) in ins.into_iter().enumerate() {
                streams[k] += s.len() as u64;
                if level.is_some() {
                    current[k].extend_from_slice(&s);
                    if current[k].len() >= 64 << 20 {
                        batches[k].push(std::mem::take(&mut current[k]));
                    }
                }
            }
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    println!(
        "{} files, {:.1} MB: {taken} taken, {failed} not made again, {} not taken; {wall:.1} s",
        paths.len(),
        input as f64 / 1e6,
        paths.len() - taken - failed
    );
    println!(
        "encode {:.1} MB/s, decode {:.1} MB/s of the files, a thread",
        input as f64 / 1e6 / enc,
        input as f64 / 1e6 / dec
    );
    for k in 0..4 {
        print!("{:>7}: {:>10.3} MB", NAMES[k], streams[k] as f64 / 1e6);
        if let Some(l) = level {
            let mut b = std::mem::take(&mut batches[k]);
            b.push(std::mem::take(&mut current[k]));
            let z: usize =
                aqr_common::pool::par_map(b.len(), threads, |i| zstd::bulk::compress(&b[i], l).unwrap().len())
                    .into_iter()
                    .sum();
            print!("  zstd -{l}: {:>10.3} MB", z as f64 / 1e6);
        }
        println!();
    }
}
