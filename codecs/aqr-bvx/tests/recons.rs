use aqr_bvx::bvx::{self, D_MAX};
use aqr_bvx::plan::{self, LMD_LOW, Match, Part, Plan, Seg, Side};
use aqr_bvx::recons;
use aqr_common::rng::Rng;

/// The streams of libcompression (`scripts/gen-fixtures.py`), by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lzfse"))
        .map(|p| (p.file_stem().unwrap().to_str().unwrap().to_owned(), std::fs::read(&p).unwrap()))
        .collect();
    v.sort();
    assert!(!v.is_empty());
    v
}

#[test]
fn streams_of_libcompression() {
    for (name, s) in fixtures() {
        let st = bvx::decode(&s, 1 << 24).unwrap();
        assert_eq!(st.used, s.len(), "{name}");
        let mut out = Vec::new();
        bvx::write(&st.data, &st.blocks, &mut out).unwrap();
        assert!(out == s, "{name}: written otherwise");
        let sp = recons::split(&s, 1 << 24).unwrap();
        assert!(sp.data == st.data, "{name}");
        // the compressor's choices, all usual
        let p = recons::plan(&sp.data, &sp.record).unwrap();
        assert!(p.parts.iter().all(|p| !matches!(p, Part::Lzfse(_, s) if !s.usual())), "{name}");
        let mut out = Vec::new();
        recons::join_onto(&sp.data, &sp.record, &mut out).unwrap();
        assert!(out == s, "{name}: rebuilt otherwise");
        assert!(sp.record.len() <= s.len() / 25 + 32, "{name}: record of {} bytes", sp.record.len());
    }
}

/// Data with repeats near and far, runs and noise.
fn data(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut d = Vec::with_capacity(n);
    while d.len() < n {
        match rng.below(4) {
            0 => {
                let n = 1 + rng.below(40) as usize;
                d.extend(rng.bytes(n));
            }
            1 => d.extend(std::iter::repeat_n(rng.below(3) as u8, 1 + rng.below(5000) as usize)),
            _ if d.len() > 8 => {
                let from = rng.below(d.len() as u64 - 4) as usize;
                let len = (4 + rng.below(300) as usize).min(d.len() - from);
                d.extend_from_within(from..from + len);
            }
            _ => d.push(rng.below(256) as u8),
        }
    }
    d.truncate(n);
    d
}

/// A random parse of `d[start..end]` (`lzvn`: as LZVN's instructions can write it).
fn parse(rng: &mut Rng, d: &[u8], start: usize, end: usize, lzvn: bool) -> Seg {
    let window = if lzvn { 65535 } else { D_MAX as usize };
    let (mut matches, mut run, mut pos, mut prev) = (Vec::new(), 0, start, 0usize);
    while pos < end {
        if rng.below(3) == 0 && pos > 0 {
            // a match at a random distance among a few, of a random length up to what holds
            let mut best = None;
            for _ in 0..8 {
                let dist =
                    if rng.below(4) == 0 && prev > 0 { prev } else { 1 + rng.below(pos.min(window) as u64) as usize };
                // (one at the distance before right after a match is that match continued)
                if dist > pos || dist == prev && run == 0 {
                    continue;
                }
                let l = (0..end - pos).take_while(|&i| d[pos - dist + i] == d[pos + i]).count();
                if l >= 3 && best.is_none_or(|(bl, _)| l > bl) {
                    best = Some((l, dist));
                }
            }
            if let Some((l, dist)) = best {
                let len = if rng.below(2) == 0 { l } else { 3 + rng.below((l - 2) as u64) as usize };
                matches.push(Match { lits: run, len: len as u32, dist: dist as u32 });
                pos += len;
                run = 0;
                prev = dist;
                continue;
            }
        }
        pos += 1;
        run += 1;
    }
    Seg { matches, tail: run }
}

/// A random plan over `d`: segments of LZFSE (blocks cut anywhere, sides unusual too), LZVN and
/// stored blocks.
fn random_plan(rng: &mut Rng, d: &[u8]) -> Plan {
    let mut plan = Plan::default();
    let mut at = 0;
    while at < d.len() {
        let n = (1 + rng.below(60000) as usize).min(d.len() - at);
        // (LZFSE blocks one after another are one segment)
        let lzfse_last = matches!(plan.parts.last(), Some(Part::Lzfse(..)));
        match rng.below(3 - lzfse_last as u64) {
            0 => plan.parts.push(Part::Stored(n as u32)),
            1 => {
                plan.parts.push(Part::Lzvn(n as u32));
                plan.segs.push(parse(rng, d, at, at + n, true));
            }
            _ => {
                let seg = parse(rng, d, at, at + n, false);
                let mut left = plan::triples(&seg).count() as u32;
                while left > 0 {
                    let k =
                        if rng.below(2) == 0 { left.min(recons::BLOCK_LMD) } else { 1 + rng.below(left as u64) as u32 };
                    let unusual = rng.below(4) == 0;
                    let side = Side {
                        freqs: None,
                        lit_start: if unusual { [1, 2, 3, 1023] } else { [0; 4] },
                        lmd_start: if unusual { [5, 63, 200] } else { [0; 3] },
                        lit_low: if unusual { 3 } else { 0 },
                        lmd_low: if unusual { 9 } else { LMD_LOW },
                        pad: vec![],
                        zd: vec![],
                    };
                    plan.parts.push(Part::Lzfse(k, side));
                    left -= k;
                }
                plan.segs.push(seg);
            }
        }
        at += n;
    }
    // the pads and distances of no match, now that the blocks are known
    let shapes = plan::shapes(&plan).unwrap();
    for (p, (pad, zd)) in plan.parts.iter_mut().filter(|p| matches!(p, Part::Lzfse(..))).zip(shapes) {
        let Part::Lzfse(_, side) = p else { unreachable!() };
        side.pad = (0..pad).map(|_| if rng.below(2) == 0 { 0 } else { rng.below(256) as u8 }).collect();
        side.zd = (0..zd).map(|_| if rng.below(2) == 0 { 0 } else { rng.below(1000) as u32 }).collect();
    }
    plan
}

/// Streams of random parses and blocks, which no compressor writes, split and rebuilt.
#[test]
fn random_parses() {
    let mut rng = Rng::new(7);
    let mut taken = 0;
    for round in 0..40 {
        let n = 1 + rng.below(200_000) as usize;
        let d = data(&mut rng, n);
        let plan = random_plan(&mut rng, &d);
        let mut s = Vec::new();
        // (an LZFSE block of too many literals for its header is not written)
        if plan::join(&d, &plan, &mut s).is_err() {
            continue;
        }
        let sp = recons::split(&s, d.len()).unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert!(sp.data == d, "round {round}");
        assert_eq!(recons::plan(&d, &sp.record).unwrap(), plan, "round {round}");
        let mut out = Vec::new();
        recons::join_onto(&d, &sp.record, &mut out).unwrap();
        assert!(out == s, "round {round}");
        taken += 1;
    }
    assert!(taken >= 30, "{taken}");
}

/// Damaged records and data fail or give some stream, but never panic.
#[test]
fn damaged() {
    let mut rng = Rng::new(3);
    for (name, s) in fixtures() {
        let sp = recons::split(&s, 1 << 24).unwrap();
        for _ in 0..30 {
            let mut r = sp.record.clone();
            let mut d = sp.data.clone();
            match rng.below(3) {
                0 if !r.is_empty() => {
                    let i = rng.below(r.len() as u64) as usize;
                    r[i] ^= 1 << rng.below(8);
                }
                1 => r.truncate(rng.below(r.len() as u64 + 1) as usize),
                _ => d.truncate(rng.below(d.len() as u64 + 1) as usize),
            }
            let mut out = Vec::new();
            let _ = recons::join_onto(&d, &r, &mut out);
            let _ = name;
        }
    }
}
