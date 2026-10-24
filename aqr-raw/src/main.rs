//! Compression ratio and speed of the modules on containers or headerless samples (research
//! tool): each file's formula as an archive, and the archive evaluated back.
use aqr_formula::aqr::{self, Level};
use aqr_formula::{Apply, Formula};
use aqr_raw::audio::pcm;
use aqr_raw::container::{self, Layout};
use aqr_raw::formula::{self, raster};
use aqr_raw::image::raster::Format;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage:
  aqr-raw bench [-t N] [-n ITERS] [-v] [-r LAYOUT] FILE...
  aqr-raw info FILE...

  -r LAYOUT: take the input as headerless samples: PCM as s16le:2, s24be:6, u8:1 (sign, bits,
             endianness, ':' channels), or a raster as WIDTHxHEIGHTxCHANNELS"
    );
    std::process::exit(2);
}

fn layout(s: &str) -> Option<Layout> {
    if let Some(f) = pcm::Format::parse(s) {
        return Some(Layout::Pcm(f));
    }
    let v: Vec<usize> = s.split('x').map(|n| n.parse().ok()).collect::<Option<_>>()?;
    let &[width, height, channels] = &v[..] else { return None };
    let f = Format { width, height, channels, stride: width.checked_mul(channels)? };
    f.valid().then_some(Layout::Raster(f))
}

fn bench(args: &[String]) {
    let mut threads = 1;
    let mut iters = 1;
    let mut files = vec![];
    let mut verbose = false;
    let mut raw = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-t" => threads = it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or_else(|| usage()).max(1),
            "-n" => iters = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "-r" => raw = Some(it.next().and_then(|s| layout(s)).unwrap_or_else(|| usage())),
            "-v" => verbose = true,
            _ => files.push(a.clone()),
        }
    }
    let apply = |a: &Apply, inputs: &[&[u8]]| {
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        let r = if a.module == formula::pcm::MODULE {
            formula::pcm::decode(&a.params, &res, inputs, a.len, threads)
        } else {
            raster::decode(&a.params, inputs, a.len, threads)
        };
        r.map_err(|e| aqr_formula::Error::new(e.0))
    };
    let natives = aqr::natives(&[formula::pcm::MODULE, raster::MODULE], &apply);
    let encode = |d: &[u8]| -> Option<Formula> {
        match raw {
            Some(l) => formula::encode_samples(d, l, threads),
            None => formula::encode(d, threads),
        }
    };
    let (mut tin, mut tout, mut tf, mut tc, mut td) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let (mut skipped, mut fails) = (0, 0);
    for f in &files {
        let src = std::fs::read(f).expect("read");
        // The formula is the modules' own work; the archive is its size.
        let (mut c, mut bf, mut bc) = (None, f64::MAX, f64::MAX);
        for _ in 0..iters {
            let t = Instant::now();
            let g = encode(&src);
            bf = bf.min(t.elapsed().as_secs_f64());
            c = g.map(|g| aqr::encode(&g, Some(aqr::checksum(&src)), Level::Mid).expect("archive"));
            bc = bc.min(t.elapsed().as_secs_f64());
        }
        let Some(c) = c else {
            skipped += 1;
            if verbose {
                println!("{f}\t{}\tNO FORMULA", src.len());
            }
            continue;
        };
        let (mut bd, mut ok) = (f64::MAX, true);
        for _ in 0..iters {
            let t = Instant::now();
            let d = aqr::decode(&c, &natives);
            bd = bd.min(t.elapsed().as_secs_f64());
            ok &= d.as_deref().ok() == Some(&src[..]);
        }
        if !ok {
            fails += 1;
            println!("ROUNDTRIP FAILURE {f}");
        }
        let n = src.len() as f64;
        (tin, tout, tf, tc, td) = (tin + n, tout + c.len() as f64, tf + bf, tc + bc, td + bd);
        if verbose {
            println!(
                "{f}\t{} -> {} ({:.4})\tformula {:.1} MB/s\tc {:.1} MB/s\td {:.1} MB/s",
                src.len(),
                c.len(),
                c.len() as f64 / n,
                n / bf / 1e6,
                n / bc / 1e6,
                n / bd / 1e6,
            );
        }
    }
    println!(
        "{} files, {tin:.0} -> {tout:.0} bytes, ratio {:.4}, formula {:.1} MB/s, compress {:.1} MB/s, decompress {:.1} MB/s, no formula {skipped}, failures {fails}",
        files.len(),
        tout / tin,
        tin / tf / 1e6,
        tin / tc / 1e6,
        tin / td / 1e6
    );
}

fn info(files: &[String]) {
    for f in files {
        let d = std::fs::read(f).expect("read");
        match container::detect(&d) {
            Some(c) => {
                println!("{f}: {}, {:?}, samples at {}, {} of {} bytes", c.name, c.layout, c.start, c.len, d.len())
            }
            None => println!("{f}: not a container taken apart"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => bench(&args[1..]),
        Some("info") => info(&args[1..]),
        _ => usage(),
    }
}
