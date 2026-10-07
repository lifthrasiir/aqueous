//! jrc: JPEG recompressor driver.
//!
//!   jrc bench [-n iters] file...     roundtrip-verify, ratio and throughput per file
//!   jrc fuzz [-n rounds] [-s seed] file...   mutate inputs; roundtrip must stay exact, no panics
//!   jrc c in out | jrc d in out      compress / decompress a file
//!   jrc info file...                 parse and decode, print per-scan information

use jpeg_recons::container::{self, Timing};
use std::time::Instant;

fn bench(files: &[String], iters: usize) {
    println!(
        "{:<24} {:>6} {:>6} | {:>6} {:>6} {:>6} {:>6} | {:>6} {:>6} {:>6} {:>6} | note",
        "file", "MB", "ratio", "parse", "huff", "model", "enc", "parse", "model", "huff", "dec"
    );
    let (mut tin, mut tout, mut te, mut td) = (0usize, 0usize, 0f64, 0f64);
    let (mut cin, mut ce, mut cd) = (0usize, 0f64, 0f64); // coded files only
    for path in files {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        let n = data.len();
        let (c, why) = container::compress_with(&data, &mut Timing::default());
        let back = container::decompress(&c).expect("decompress failed");
        assert!(back == data, "{path}: ROUNDTRIP MISMATCH");
        let (mut tc, mut tdd) = (Timing::default(), Timing::default());
        let (mut ec, mut dc) = (0f64, 0f64);
        for _ in 0..iters {
            let mut t = Timing::default();
            let s = Instant::now();
            std::hint::black_box(container::compress_with(&data, &mut t));
            ec += s.elapsed().as_secs_f64();
            tc.parse += t.parse;
            tc.huffman += t.huffman;
            tc.model += t.model;
            let mut t = Timing::default();
            let s = Instant::now();
            std::hint::black_box(container::decompress_with(&c, &mut t).unwrap());
            dc += s.elapsed().as_secs_f64();
            tdd.parse += t.parse;
            tdd.huffman += t.huffman;
            tdd.model += t.model;
        }
        let k = iters as f64;
        let ms = |x: f64| x / k * 1e3 / (n as f64 / 1e6);
        let mbs = |x: f64| n as f64 / (x / k) / 1e6;
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        println!(
            "{:<24} {:6.2} {:6.4} | {:6.2} {:6.2} {:6.2} {:6.1} | {:6.2} {:6.2} {:6.2} {:6.1} | {}",
            &name[..name.len().min(24)],
            n as f64 / 1e6,
            c.len() as f64 / n as f64,
            ms(tc.parse),
            ms(tc.huffman),
            ms(tc.model),
            mbs(ec),
            ms(tdd.parse),
            ms(tdd.model),
            ms(tdd.huffman),
            mbs(dc),
            why.as_deref().unwrap_or("")
        );
        tin += n;
        tout += c.len();
        te += ec / k;
        td += dc / k;
        if why.as_deref().is_none_or(|w| w.starts_with("partial")) {
            cin += n;
            ce += ec / k;
            cd += dc / k;
        }
    }
    println!(
        "TOTAL {:.2} MB ratio {:.4}  enc {:.1} MB/s  dec {:.1} MB/s   (coded files only: enc {:.1} dec {:.1} MB/s)   [phase columns: ms per MB]",
        tin as f64 / 1e6,
        tout as f64 / tin as f64,
        tin as f64 / te / 1e6,
        tin as f64 / td / 1e6,
        cin as f64 / ce / 1e6,
        cin as f64 / cd / 1e6
    );
}

/// xorshift64*
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(d: &[u8], r: &mut Rng) -> Vec<u8> {
    let mut v = d.to_vec();
    for _ in 0..1 + r.below(4) {
        if v.is_empty() {
            break;
        }
        let i = r.below(v.len());
        match r.below(6) {
            0 => v[i] ^= 1 << r.below(8),
            1 => v[i] = r.next() as u8,
            2 => v.truncate(i),
            3 => {
                let n = r.below(64).min(v.len() - i);
                v.drain(i..i + n);
            }
            4 => {
                let n = r.below(64).min(v.len() - i);
                let chunk = v[i..i + n].to_vec();
                v.splice(i..i, chunk);
            }
            _ => v[i] = [0x00, 0xFF, 0xD0, 0xD9, 0xDA, 0xC4][r.below(6)],
        }
    }
    v
}

fn fuzz(files: &[String], rounds: usize, seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let inputs: Vec<(String, Vec<u8>)> = files.iter().filter_map(|p| std::fs::read(p).ok().map(|d| (p.clone(), d))).collect();
    let (mut coded, mut total) = (0usize, 0usize);
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for round in 0..rounds {
        let (name, d) = &inputs[r.below(inputs.len())];
        let m = mutate(d, &mut r);
        let (c, why) = container::compress_with(&m, &mut Timing::default());
        let back = container::decompress(&c).unwrap_or_else(|e| panic!("round {round} ({name}): decompress failed: {e}"));
        if back != m {
            std::fs::write("fuzz_failure.jpg", &m).ok();
            panic!("round {round} ({name}): ROUNDTRIP MISMATCH (input saved to fuzz_failure.jpg)");
        }
        // corrupt container: must error or produce something, never panic/hang
        let mut cc = c.clone();
        if cc.len() > 8 {
            let i = 5 + r.below(cc.len() - 5);
            cc[i] ^= 1 << r.below(8);
            let _ = container::decompress(&cc);
        }
        total += 1;
        match why {
            None => coded += 1,
            Some(w) if w.starts_with("partial") => {
                coded += 1;
                *reasons.entry("(coded, some raw scans)".into()).or_default() += 1
            }
            Some(w) => *reasons.entry(w.split(':').next().unwrap().to_string()).or_default() += 1,
        }
    }
    println!("fuzz: {total} inputs, all roundtrips exact; coded {coded}, stored {}", total - coded);
    for (w, n) in reasons {
        println!("  stored {n:6}: {w}");
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: jrc bench|fuzz [-n N] [-s seed] files... | jrc c|d in out");
        std::process::exit(2);
    }
    let cmd = args.remove(0);
    let (mut n, mut seed) = (None, 1u64);
    while args.len() >= 2 && args[0].starts_with('-') {
        match args[0].as_str() {
            "-n" => n = Some(args[1].parse().unwrap()),
            "-s" => seed = args[1].parse().unwrap(),
            o => panic!("unknown option {o}"),
        }
        args.drain(..2);
    }
    match cmd.as_str() {
        "bench" => bench(&args, n.unwrap_or(5)),
        "fuzz" => fuzz(&args, n.unwrap_or(1000), seed),
        "info" => {
            for path in &args {
                let d = std::fs::read(path).unwrap();
                println!("{path}:");
                let j = match jpeg_recons::jpeg::parse(&d) {
                    Ok(j) => j,
                    Err(e) => {
                        println!("  parse error: {e}");
                        continue;
                    }
                };
                println!("  {}x{} progressive={} comps={} scans={}", j.width, j.height, j.progressive, j.comps.len(), j.scans.len());
                let (mut coefs, mut clean) = (Vec::new(), jpeg_recons::jpeg::Clean::default());
                match jpeg_recons::jpeg::decode_all(&j, &d, jpeg_recons::jpeg::Order::Zigzag, &mut coefs, &mut clean) {
                    Ok(infos) => {
                        if !j.progressive {
                            let (e, _) = jpeg_recons::fast::encode(&j, &coefs);
                            println!("  model: tables {}B rans {}B raw {}B", e.tables.len(), e.rans.len() * 4, e.raw.len());
                        }
                        for (s, i) in j.scans.iter().zip(infos) {
                            let t = i.truncated.map(|t| format!("truncated at MCU {} bit {} tail {}B boundary={}", t.mcus, t.bits, t.tail.len(), t.boundary));
                            println!("  scan comps={} ri={} data={}B standard={} trailing_rst={} pad_zero={} {}", s.comps.len(), s.ri, s.end - s.start, i.standard, i.trailing_rst, i.pad_zero, t.unwrap_or_default());
                        }
                    }
                    Err(e) => println!("  decode error: {e}"),
                }
            }
        }
        "pscan" => {
            for path in &args {
                let d = std::fs::read(path).unwrap();
                let j = jpeg_recons::jpeg::parse(&d).unwrap();
                println!("{path}:");
                for (desc, td, te, bytes) in jpeg_recons::progressive::scan_times(&j, &d, n.unwrap_or(5)).unwrap() {
                    println!("  {desc:<34} {bytes:9} B  decode {:7.2} ms  encode {:7.2} ms", td * 1e3, te * 1e3);
                }
            }
        }
        "c" | "d" => {
            let d = std::fs::read(&args[0]).unwrap();
            let out = if cmd == "c" { container::compress(&d) } else { container::decompress(&d).unwrap() };
            std::fs::write(&args[1], out).unwrap();
        }
        _ => panic!("unknown command {cmd}"),
    }
}
