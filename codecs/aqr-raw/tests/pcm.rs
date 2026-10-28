//! PCM through the formula: WAV and AIFF taken apart and headerless samples, evaluated back
//! through the modules on one thread and several (the same formula whatever the threads),
//! with junk, cuts and corrupted inputs.

use aqr_formula::{Apply, Formula, aqr};
use aqr_raw::audio::pcm::Format;
use aqr_raw::container::{self, Layout};
use aqr_raw::formula::{self, pcm};

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

fn eval(f: &Formula, threads: usize) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, pcm::MODULE);
        let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        pcm::decode(&a.params, &res, inputs, a.len, threads).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// The formula of `d` (as a container, or as samples of `raw`), the same on every number of
/// threads, evaluated back on one and several.
fn check(name: &str, d: &[u8], raw: Option<Format>) -> Option<Formula> {
    let enc = |t| match raw {
        Some(fmt) => formula::encode_samples(d, Layout::Pcm(fmt), t),
        None => formula::encode(d, t),
    };
    let f = enc(1);
    for t in [2, 3, 8] {
        assert!(enc(t) == f, "{name}: {t} threads, other formula");
    }
    let f = f?;
    assert_eq!(f.len(), d.len() as u64, "{name}");
    for t in [1, 3] {
        assert!(eval(&f, t).unwrap() == d, "{name}: {t} threads");
    }
    Some(f)
}

#[test]
fn data_files() {
    for (name, d) in files() {
        let c = container::detect(&d).expect(&name);
        assert_eq!(c.name, if name.contains(".aif") { "aiff" } else { "wav" }, "{name}");
        let f = check(&name, &d, None).expect(&name);
        let [a] = f.applies()[..] else { panic!("{name}") };
        let Layout::Pcm(fmt) = c.layout else { panic!("{name}") };
        assert_eq!(a.params, fmt.to_bytes(), "{name}");
        assert_eq!(a.inputs.len(), 6);
        assert_eq!(a.len, c.len as u64);
        // Smaller than the input as archives; an archive also holds the tables (about 2.4 KB
        // compressed, once).
        let n = aqr::encode(&f, Some(aqr::checksum(&d)), aqr::Level::MID).unwrap().len();
        assert!(n < d.len() + 2400, "{name}: {n} >= {} + 2400", d.len());
    }
}

#[test]
fn junk_and_cuts() {
    for (name, d) in files() {
        let mut j = d.clone();
        j.extend_from_slice(b"TAG and after");
        check(&name, &j, None).expect(&name);
        for cut in [1, 5, 417, d.len() / 3, d.len() / 2 + 7] {
            check(&name, &d[..d.len() - cut.min(d.len())], None);
            check(&name, &d[cut.min(d.len())..], None);
        }
    }
}

#[test]
fn raw_pcm() {
    for (name, d) in files() {
        for f in ["s16le:2", "s24be:2", "u8:1", "s32le:6", "s16be:3"] {
            let fmt = Format::parse(f);
            check(&format!("{name} as {f}"), &d, fmt).expect(&name);
            check(&format!("{name} as {f}, cut"), &d[..d.len() * 2 / 3 + 1], fmt);
        }
    }
}

#[test]
fn not_coded() {
    for d in [&b""[..], b"x", b"not audio at all", b"RIFF\0\0\0\0WAVE", b"RIFF\0\0\0\0WAVEdata\0\0\0\0"] {
        assert!(check(&format!("{d:?}"), d, None).is_none());
    }
    // Less than a sample frame.
    assert!(check("two bytes", b"ab", Format::parse("s16le:2")).is_none());
    check("two bytes", b"ab", Format::parse("s16le:1")).unwrap();
}

#[test]
fn corrupt_inputs() {
    let (_, d) = files().into_iter().next().unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    let ins: Vec<Vec<u8>> = a
        .inputs
        .iter()
        .map(|n| match n {
            aqr_formula::Node::Slice { stream, .. } => f.streams[*stream].data.clone(),
            _ => panic!(),
        })
        .collect();
    let run = |p: &[u8], i: &[Vec<u8>], len: u64| {
        let r: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
        let _ = pcm::decode(p, &[&a.resources[0][..]], &r, len, 1);
    };
    run(&[], &ins, a.len);
    run(&[2, 2, 4], &ins, a.len);
    run(&[0, 2, 0], &ins, a.len);
    run(&a.params, &ins[..5], a.len);
    run(&a.params, &ins, a.len + 1);
    run(&a.params, &ins, a.len + 4);
    run(&a.params, &ins, u64::MAX);
    for k in 0..ins.len() {
        let mut m = ins.clone();
        let n = m[k].len();
        m[k].truncate(n / 2);
        run(&a.params, &m, a.len);
        for i in (0..n).step_by(53) {
            let mut m = ins.clone();
            m[k][i] ^= 0x5a;
            run(&a.params, &m, a.len);
        }
    }
}

/// The decoder codes with the resource it is given, the same that aqr-flac's module takes.
#[test]
fn resource() {
    let (_, d) = files().into_iter().next().unwrap();
    let f = formula::encode(&d, 1).unwrap();
    let a = f.applies()[0];
    assert_eq!(a.resources.len(), 1);
    assert!(a.resources[0][..] == pcm::resource()[..]);
    let ins: Vec<&[u8]> = a
        .inputs
        .iter()
        .map(|n| match n {
            aqr_formula::Node::Slice { stream, .. } => &f.streams[*stream].data[..],
            _ => panic!(),
        })
        .collect();
    let run = |res: &[&[u8]]| pcm::decode(&a.params, res, &ins, a.len, 1).map_err(|e| e.0);
    let good = &a.resources[0][..];
    let c = container::detect(&d).unwrap();
    assert!(run(&[good]).unwrap() == d[c.start..c.start + c.len]);
    assert!(run(&[]).is_err());
    assert!(run(&[good, good]).is_err());
    let mut bad = good.to_vec();
    bad[0] ^= 1; // layout id
    assert!(run(&[&bad]).is_err());
    let mut bad = good.to_vec();
    bad[4] ^= 1;
    assert!(run(&[&bad]).is_err());
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

/// Long enough to fill the pipelines of the threaded paths (many batches).
#[test]
fn long_pcm() {
    for (ch, bytes, n) in [(2, 2, 700_000), (1, 3, 400_000), (6, 2, 120_000)] {
        let d = wav(&signal(n, ch, 8 * bytes as u32), ch, bytes);
        let name = format!("WAV {ch} ch, {bytes} bytes");
        check(&name, &d, None).expect(&name);
        // As raw PCM, with a partial sample at the end.
        let fmt = Format::parse(&format!("s{}le:{ch}", 8 * bytes)).unwrap();
        check(&format!("{name}, raw"), &d[44..d.len() - 1], Some(fmt)).expect(&name);
    }
}
