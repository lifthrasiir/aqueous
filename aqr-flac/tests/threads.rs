//! The threaded paths: the same formula whatever the number of threads, and evaluating
//! back to the input on any. Over the files in tests/data, with junk and cuts, as raw PCM,
//! and over inputs large enough to fill the pipelines (many batches).

use aqr_flac::formula::{self, MODULE, Options};
use aqr_flac::pcm::Format;
use aqr_formula::{Apply, Formula};

const THREADS: [usize; 3] = [2, 3, 8];

fn files() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

fn file(name: &str) -> Vec<u8> {
    files().into_iter().find(|f| f.0 == name).unwrap().1
}

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// Same formula for all thread counts, and every evaluation round-trips (the formula, if
/// the input is coded at all).
fn check(name: &str, d: &[u8], raw: Option<Format>) -> Option<Formula> {
    let f1 = formula::encode_with(d, &Options { raw, threads: 1 });
    for t in THREADS {
        let f = formula::encode_with(d, &Options { raw, threads: t });
        assert!(f == f1, "{name}: {t} threads, other formula");
        if let Some(f) = &f {
            assert!(eval(f, t).as_deref() == Ok(d), "{name}: {t} threads, evaluate");
        }
    }
    f1
}

#[test]
fn data_files() {
    for (name, d) in files() {
        check(&name, &d, None).expect(&name);
        let mut j = b"some junk before".to_vec();
        j.extend_from_slice(&d);
        j.extend_from_slice(b"TAG and after");
        check(&format!("{name} with junk"), &j, None);
        for cut in [1, 417, d.len() / 3] {
            check(&format!("{name} cut {cut}"), &d[..d.len() - cut.min(d.len())], None);
            check(&format!("{name} from {cut}"), &d[cut.min(d.len())..], None);
        }
    }
}

#[test]
fn raw_pcm() {
    for (name, d) in files() {
        for f in ["s16le:2", "s24be:2", "u8:1", "s32le:6"] {
            let fmt = Format::parse(f).unwrap();
            check(&format!("{name} as {f}"), &d, Some(fmt));
            check(&format!("{name} as {f}, cut"), &d[..d.len() * 2 / 3 + 1], Some(fmt));
        }
    }
}

#[test]
fn small_and_empty() {
    for d in [&b""[..], b"x", b"fLaC", b"RIFF\0\0\0\0WAVE"] {
        check(&format!("{d:?}"), d, None);
    }
}

/// FLAC files one after the other (the later headers become junk), with junk and damaged
/// frames: hundreds of frames, many batches.
#[test]
fn long_flac() {
    let mut d = Vec::new();
    for (i, name) in ["s16_f5.flac", "s16_junk.flac", "s16_lax.flac", "s24_f8.flac"].iter().cycle().take(40).enumerate()
    {
        let mut f = file(name);
        if i % 7 == 3 {
            // A damaged frame (kept as junk).
            let p = f.len() / 2;
            f[p] ^= 0x40;
        }
        d.extend_from_slice(&f);
        if i % 5 == 0 {
            d.extend_from_slice(b"junk between");
        }
    }
    check("long FLAC", &d, None).unwrap();
    // Only one kind of stream (the same sample size): no junk but the headers.
    let one = file("s16_f5.flac").repeat(30);
    check("s16_f5 x30", &one, None);
}

/// A synthetic signal: tones, noise, silence, steps.
fn signal(n: usize, channels: usize, bits: u32) -> Vec<i64> {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let amp = (1i64 << (bits - 1)) - 1;
    let mut v = Vec::with_capacity(n * channels);
    for i in 0..n {
        for c in 0..channels {
            let t = i as f64 / 44100.0;
            let x = match (i / 30000) % 4 {
                0 => (t * 440.0 * (c + 1) as f64 * std::f64::consts::TAU).sin() * 0.6,
                1 => (t * 97.0 * std::f64::consts::TAU).sin() * 0.3 + (rng() % 2001) as f64 / 1e4 - 0.1,
                2 => 0.0,
                _ => ((i / 500) % 3) as f64 * 0.2 - 0.2,
            };
            v.push(((x * amp as f64) as i64).clamp(-amp, amp));
        }
    }
    v
}

fn wav(samples: &[i64], channels: usize, bytes: usize) -> Vec<u8> {
    let data = samples.len() * bytes;
    let mut d = Vec::new();
    d.extend_from_slice(b"RIFF");
    d.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    d.extend_from_slice(b"WAVEfmt ");
    d.extend_from_slice(&16u32.to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&(channels as u16).to_le_bytes());
    d.extend_from_slice(&44100u32.to_le_bytes());
    d.extend_from_slice(&((44100 * channels * bytes) as u32).to_le_bytes());
    d.extend_from_slice(&((channels * bytes) as u16).to_le_bytes());
    d.extend_from_slice(&(8 * bytes as u16).to_le_bytes());
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(data as u32).to_le_bytes());
    for &s in samples {
        let v = if bytes == 1 { (s + 128) as u64 } else { s as u64 };
        d.extend_from_slice(&v.to_le_bytes()[..bytes]);
    }
    d.extend_from_slice(b"LIST trailing chunk");
    d
}

#[test]
fn long_pcm() {
    for (ch, bytes, n) in [(2, 2, 700_000), (1, 3, 400_000), (6, 2, 120_000)] {
        let d = wav(&signal(n, ch, 8 * bytes as u32), ch, bytes);
        let name = format!("WAV {ch} ch, {bytes} bytes");
        check(&name, &d, None).expect(&name);
        // As raw PCM, with a partial sample at the end.
        let fmt = Format::parse(&format!("s{}le:{ch}", 8 * bytes)).unwrap();
        check(&format!("{name}, raw"), &d[44..d.len() - 1], Some(fmt));
    }
}
