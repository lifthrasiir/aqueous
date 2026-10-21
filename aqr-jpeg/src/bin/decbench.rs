//! Loop the fast model decoder or the Huffman re-encoder only (for profiling).
//! usage: decbench file.jpg iters [huff|enc]   (RAYON_NUM_THREADS limits the workers)
use aqr_jpeg::{fast, jpeg};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let iters: usize = args[2].parse().unwrap();
    let j = jpeg::parse(&data).unwrap();
    let mut clean = jpeg::Clean::default();
    let mut coefs = Vec::new();
    let infos = jpeg::decode_all(&j, &data, jpeg::Order::Zigzag, &mut coefs, &mut clean).unwrap();
    let (e, _) = fast::encode(&j, &coefs);
    let mut out = Vec::new();
    if args.get(3).map(String::as_str) == Some("huff") {
        let mut scan = Vec::new();
        let t = std::time::Instant::now();
        for _ in 0..iters {
            for (sc, info) in j.scans.iter().zip(&infos) {
                jpeg::encode_scan(&j, sc, info, &coefs, &mut scan).unwrap();
            }
            std::hint::black_box(&scan);
        }
        let s = t.elapsed().as_secs_f64() / iters as f64;
        println!("huffman encode {:.1} MB/s", data.len() as f64 / s / 1e6);
        return;
    }
    if args.get(3).map(String::as_str) == Some("enc") {
        let (mut w, mut n, mut r) = (0f64, 0f64, 0f64);
        let t = std::time::Instant::now();
        for _ in 0..iters {
            let (e, ph) = fast::encode(&j, &coefs);
            std::hint::black_box(&e);
            (w, n, r) = (w + ph.walk, n + ph.norm, r + ph.rans);
        }
        let s = t.elapsed().as_secs_f64() / iters as f64;
        let k = iters as f64 * 1e-3;
        println!(
            "model encode {:.1} MB/s: walk {:.2} ms, tables {:.2} ms, rans {:.2} ms",
            data.len() as f64 / s / 1e6,
            w / k,
            n / k,
            r / k
        );
        return;
    }
    let t = std::time::Instant::now();
    for _ in 0..iters {
        fast::decode_into(&j, &e, &mut out).unwrap();
        std::hint::black_box(&out);
    }
    let s = t.elapsed().as_secs_f64() / iters as f64;
    println!("model decode {:.1} MB/s, {:.2} ns/sym", data.len() as f64 / s / 1e6, s * 1e9 / e.nsym as f64);
}
