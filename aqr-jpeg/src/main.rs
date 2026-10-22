//! jrc: JPEG recompressor driver.
//!
//!   (every command takes -j threads; default: all cores)
//!   jrc bench [-n iters] file...     roundtrip-verify through the formula and an .aqr archive,
//!                                    ratio and throughput per file
//!   jrc fuzz [-n rounds] [-s seed] file...   mutate inputs; roundtrip must stay exact, no panics
//!   jrc info file...                 parse and decode, print per-scan information
//!   jrc mutenc [-n rounds] [-s seed] file...   re-encode inputs with valid non-canonical
//!                                    choices; roundtrip must stay exact

use aqr_formula::Apply;
use aqr_formula::aqr::{self, Level};
use aqr_jpeg::formula::{self, MODULE};
use std::time::Instant;

fn module(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    formula::decode(&a.params, inputs, a.len, 0).map_err(|e| aqr_formula::Error::new(e.0))
}

/// `d` as an archive of its formula; None if it is not coded.
fn pack(d: &[u8]) -> Option<Vec<u8>> {
    let f = formula::encode(d, 0)?;
    Some(aqr::encode(&f, Some(aqr::checksum(d)), Level::Mid).expect("archive encoding failed"))
}

fn unpack(c: &[u8]) -> Result<Vec<u8>, String> {
    aqr::decode(c, &aqr::natives(&[MODULE], &module)).map_err(|e| e.to_string())
}

fn bench(files: &[String], iters: usize) {
    println!("{:<24} {:>6} {:>6} {:>7} {:>7} | note", "file", "MB", "ratio", "enc", "dec");
    let (mut tin, mut tout) = (0usize, 0usize);
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
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        let name = &name[..name.len().min(24)];
        tin += n;
        let Some(c) = pack(&data) else {
            println!("{:<24} {:6.2} {:6.4} {:>7} {:>7} | not coded", name, n as f64 / 1e6, 1.0, "", "");
            tout += n;
            continue;
        };
        let back = unpack(&c).expect("decoding failed");
        assert!(back == data, "{path}: ROUNDTRIP MISMATCH");
        let (mut ec, mut dc) = (0f64, 0f64);
        for _ in 0..iters {
            let s = Instant::now();
            std::hint::black_box(pack(&data));
            ec += s.elapsed().as_secs_f64();
            let s = Instant::now();
            std::hint::black_box(unpack(&c).unwrap());
            dc += s.elapsed().as_secs_f64();
        }
        let k = iters as f64;
        let mbs = |x: f64| n as f64 / (x / k) / 1e6;
        println!(
            "{:<24} {:6.2} {:6.4} {:7.1} {:7.1} |",
            name,
            n as f64 / 1e6,
            c.len() as f64 / n as f64,
            mbs(ec),
            mbs(dc)
        );
        tout += c.len();
        cin += n;
        ce += ec / k;
        cd += dc / k;
    }
    println!(
        "TOTAL {:.2} MB ratio {:.4}   (coded files only: enc {:.1} dec {:.1} MB/s)",
        tin as f64 / 1e6,
        tout as f64 / tin as f64,
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
    let inputs: Vec<(String, Vec<u8>)> =
        files.iter().filter_map(|p| std::fs::read(p).ok().map(|d| (p.clone(), d))).collect();
    let (mut coded, mut total) = (0usize, 0usize);
    for round in 0..rounds {
        let (name, d) = &inputs[r.below(inputs.len())];
        let m = mutate(d, &mut r);
        let Some(c) = pack(&m) else {
            total += 1;
            continue;
        };
        let back = unpack(&c).unwrap_or_else(|e| panic!("round {round} ({name}): decoding failed: {e}"));
        if back != m {
            std::fs::write("fuzz_failure.jpg", &m).ok();
            panic!("round {round} ({name}): ROUNDTRIP MISMATCH (input saved to fuzz_failure.jpg)");
        }
        // corrupt archive: must error or produce something, never panic/hang
        let mut cc = c.clone();
        let i = r.below(cc.len());
        cc[i] ^= 1 << r.below(8);
        let _ = unpack(&cc);
        total += 1;
        coded += 1;
    }
    println!("fuzz: {total} inputs, all roundtrips exact; coded {coded}, not coded {}", total - coded);
}

fn mutenc(files: &[String], rounds: usize, seed: u64) {
    use aqr_jpeg::variant;
    let mut r = variant::Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let inputs: Vec<(String, Vec<u8>, Vec<aqr_jpeg::jpeg::Coefs>)> = files
        .iter()
        .filter_map(|p| {
            let d = std::fs::read(p).ok()?;
            let c = variant::coefficients(&d)?;
            Some((p.clone(), d, c))
        })
        .collect();
    assert!(!inputs.is_empty(), "no decodable inputs");
    let (mut identical, mut coded, mut not_coded, mut ungenerated) = (0, 0, 0, 0);
    for round in 0..rounds {
        let (name, d, c0) = &inputs[r.below(inputs.len() as u64) as usize];
        let Some((v, desc)) = variant::reencode(d, &mut r) else {
            ungenerated += 1;
            continue;
        };
        // the generator must only change the coding, never the coefficients
        let c1 = variant::coefficients(&v)
            .unwrap_or_else(|| panic!("round {round} ({name}): variant does not decode: {desc}"));
        assert!(
            c0.iter().zip(&c1).all(|(a, b)| a.blocks == b.blocks),
            "round {round} ({name}): variant changed coefficients: {desc}"
        );
        if v == *d {
            identical += 1;
        }
        let Some(cmp) = pack(&v) else {
            not_coded += 1;
            continue;
        };
        let back = unpack(&cmp).unwrap_or_else(|e| {
            std::fs::write("mutenc_failure.jpg", &v).ok();
            panic!("round {round} ({name}): decoding failed: {e} (variant saved to mutenc_failure.jpg): {desc}")
        });
        if back != v {
            std::fs::write("mutenc_failure.jpg", &v).ok();
            panic!("round {round} ({name}): ROUNDTRIP MISMATCH (variant saved to mutenc_failure.jpg): {desc}");
        }
        coded += 1;
    }
    println!(
        "mutenc: {rounds} rounds, all roundtrips exact; not generated {ungenerated}, identical to original {identical}, coded {coded}, not coded {not_coded}"
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: jrc bench|fuzz|mutenc|info|pscan [-n N] [-s seed] files...");
        std::process::exit(2);
    }
    let cmd = args.remove(0);
    let (mut n, mut seed, mut threads) = (None, 1u64, 0usize);
    while args.len() >= 2 && args[0].starts_with('-') {
        match args[0].as_str() {
            "-n" => n = Some(args[1].parse().unwrap()),
            "-s" => seed = args[1].parse().unwrap(),
            "-j" => threads = args[1].parse().unwrap(),
            o => panic!("unknown option {o}"),
        }
        args.drain(..2);
    }
    // 0 = one worker per core
    rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().unwrap();
    match cmd.as_str() {
        "bench" => bench(&args, n.unwrap_or(5)),
        "fuzz" => fuzz(&args, n.unwrap_or(1000), seed),
        "mutenc" => mutenc(&args, n.unwrap_or(200), seed),
        "info" => {
            for path in &args {
                let d = std::fs::read(path).unwrap();
                println!("{path}:");
                let j = match aqr_jpeg::jpeg::parse(&d) {
                    Ok(j) => j,
                    Err(e) => {
                        println!("  parse error: {e}");
                        continue;
                    }
                };
                println!(
                    "  {}x{} progressive={} comps={} scans={}",
                    j.width,
                    j.height,
                    j.progressive,
                    j.comps.len(),
                    j.scans.len()
                );
                let (mut coefs, mut clean) = (Vec::new(), aqr_jpeg::jpeg::Clean::default());
                match aqr_jpeg::jpeg::decode_all(&j, &d, aqr_jpeg::jpeg::Order::Zigzag, &mut coefs, &mut clean) {
                    Ok(infos) => {
                        if !j.progressive {
                            let (e, _) = aqr_jpeg::fast::encode(&j, &coefs);
                            let rans: usize = e.stripes.iter().map(|s| s.rans.len() * 4).sum();
                            let raw: usize = e.stripes.iter().map(|s| s.raw.len()).sum();
                            println!(
                                "  model: tables {}B rans {}B raw {}B stripes {}",
                                e.tables.len(),
                                rans,
                                raw,
                                e.stripes.len()
                            );
                        }
                        for (s, i) in j.scans.iter().zip(infos) {
                            let t = i.truncated.map(|t| {
                                format!(
                                    "truncated at MCU {} bit {} tail {}B boundary={}",
                                    t.mcus,
                                    t.bits,
                                    t.tail.len(),
                                    t.boundary
                                )
                            });
                            println!(
                                "  scan comps={} ri={} data={}B standard={} trailing_rst={} pad_zero={} {}",
                                s.comps.len(),
                                s.ri,
                                s.end - s.start,
                                i.standard,
                                i.trailing_rst,
                                i.pad_zero,
                                t.unwrap_or_default()
                            );
                        }
                    }
                    Err(e) => println!("  decode error: {e}"),
                }
            }
        }
        "pscan" => {
            for path in &args {
                let d = std::fs::read(path).unwrap();
                let j = aqr_jpeg::jpeg::parse(&d).unwrap();
                println!("{path}:");
                for (desc, td, te, bytes) in aqr_jpeg::progressive::scan_times(&j, &d, n.unwrap_or(5)).unwrap() {
                    println!("  {desc:<34} {bytes:9} B  decode {:7.2} ms  encode {:7.2} ms", td * 1e3, te * 1e3);
                }
            }
        }
        _ => panic!("unknown command {cmd}"),
    }
}
