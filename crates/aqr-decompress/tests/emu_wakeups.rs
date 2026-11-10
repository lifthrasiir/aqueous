//! Timings of the native emulation of gangs and channels (`aqr_api::emu`): many tasks exchanging
//! small messages, and several applications at once. Ignored; run each case in a process of its
//! own for the system time, e.g.
//!
//! ```sh
//! AQR_CASE=prores AQR_STRIPES=16 /usr/bin/time -l cargo rt -p aqr-decompress --test emu_wakeups -- --ignored --nocapture
//! ```
//!
//! `AQR_CASE`: `prores` (the fixture repeated `AQR_REPEAT` times, in `AQR_STRIPES` stripes),
//! `small` (synthetic frames of `AQR_W` x `AQR_H`, `AQR_FRAMES` of them), `flac` (every flac fixture, `AQR_REPEAT` times, `AQR_JOBS` archives decoded at once).

#![cfg(feature = "wasm")]

use aqr_decompress::{Native, wasm_of};
use aqr_formula::aqr::{self, Encoder, Level};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Decodes each of `archives` `reps` times, `jobs` of them at once.
fn run(what: &str, archives: &[(Vec<u8>, Vec<u8>)], reps: usize, jobs: usize) {
    let work: Vec<_> = (0..reps).flat_map(|_| archives.iter()).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let bytes: usize = work.iter().map(|w| w.1.len()).sum();
    let t = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((c, d)) = work.get(i) else { break };
                    assert!(aqr::decode(c, &Native::default()).unwrap() == *d);
                }
            });
        }
    });
    let s = t.elapsed().as_secs_f64();
    eprintln!("{what}: {} archives, {bytes} bytes in {s:.2} s wall = {:.1} MB/s", work.len(), bytes as f64 / s / 1e6);
}

#[test]
#[ignore]
fn emu_wakeups() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    match std::env::var("AQR_CASE").as_deref().unwrap_or("prores") {
        "prores" => {
            let one = fixture("codecs/aqr-prores/tests/data/aw-422hq.mov");
            let stripes = env("AQR_STRIPES", 16);
            let mut d = Vec::new();
            let frames = aqr_prores::find(&one);
            eprintln!("{} frames in the fixture, {} bytes", frames.len(), one.len());
            for _ in 0..env("AQR_REPEAT", 40) {
                d.extend_from_slice(&one);
            }
            let f = aqr_prores::formula::encode_with(&d, 2, aqr_prores::formula::CHUNK, Some(stripes)).unwrap();
            let c = enc.encode(&f, None).unwrap();
            eprintln!("{} bytes in {stripes} stripes -> {} archived", d.len(), c.len());
            run(&format!("prores x{stripes}"), &[(c, d)], 1, 1);
        }
        "small" => {
            // (frames of AQR_W x AQR_H pixels: small ones, as a proxy's)
            let (w, h) = (env("AQR_W", 128), env("AQR_H", 64));
            let mut d = b"\0\0\0\x08wide".to_vec();
            for i in 0..env("AQR_FRAMES", 4000) {
                d.extend_from_slice(&aqr_prores::synth::synthetic(w as u16, h as u16, 2, 0, i as u64));
            }
            let frames = aqr_prores::find(&d);
            let stripes = env("AQR_STRIPES", 16);
            let f = aqr_prores::formula::encode_with(&d, 2, aqr_prores::formula::CHUNK, Some(stripes)).unwrap();
            let c = enc.encode(&f, None).unwrap();
            eprintln!("{} frames, {} bytes in {stripes} stripes -> {} archived", frames.len(), d.len(), c.len());
            run(&format!("small x{stripes}"), &[(c, d)], 1, 1);
        }
        "flac" => {
            let dir = "codecs/aqr-flac/tests/data";
            let mut archives = Vec::new();
            for t in ["c6", "s16_f0", "s16_junk", "s24_f8", "s32", "s16_ffmpeg"] {
                let d = fixture(&format!("{dir}/{t}.flac"));
                let f = aqr_flac::formula::encode(&d, 1).unwrap();
                archives.push((enc.encode(&f, None).unwrap(), d));
            }
            run("flac", &archives, env("AQR_REPEAT", 20), env("AQR_JOBS", 8));
        }
        c => panic!("case {c}"),
    }
}
