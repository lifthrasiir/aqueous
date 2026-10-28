//! zlib's emulation and detection against real zlib output from flate-zoo.

use flate_zoo::{Sweep, presets_of};

/// zlib's Huffman headers are what [`aqr_flate::header::zlib_header`] builds for each block.
#[test]
fn zlib_headers_are_predicted() {
    use aqr_flate::deflate::Body;
    let text = include_bytes!("../src/zlib.rs").repeat(3);
    for level in 1..=9 {
        let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap())
            .set("level", level)
            .unwrap();
        let raw = enc.compress(&text).unwrap();
        let (stream, _) = aqr_flate::deflate::parse(&raw).unwrap();
        for (i, b) in stream.blocks.iter().enumerate() {
            if let Body::Dynamic { header, tokens } = &b.body {
                assert!(
                    aqr_flate::header::zlib_header(tokens) == *header,
                    "level {level}, block {i}"
                );
            }
        }
    }
}

fn zlib_params(enc: &flate_zoo::Encoder) -> Option<aqr_flate::zlib::Params> {
    use aqr_flate::zlib::{Params, Strategy};
    let level = enc.get("level")? as u8;
    let strategy = Strategy::ALL[enc.get("strategy")? as usize];
    let wbits = (enc.get("wbits")? as u8).max(9);
    let mem = enc.get("mem")? as u8;
    (level > 0).then_some(Params {
        level,
        strategy,
        wbits,
        mem,
    })
}

fn zlib_inputs() -> Vec<Vec<u8>> {
    let mut lcg = 7u32;
    let mut mixed = include_bytes!("../src/zlib.rs").repeat(6);
    // Incompressible stretches make zlib emit stored blocks in between.
    mixed.extend((0..40_000).map(|_| {
        lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (lcg >> 24) as u8
    }));
    mixed.extend(include_bytes!("../src/deflate.rs").repeat(4));
    vec![
        b"a".to_vec(),
        b"abcabcabd".repeat(20_000),
        vec![0; 100_000],
        mixed,
    ]
}

/// [`aqr_flate::zlib::compress`] writes what zlib writes, byte for byte, at the levels of
/// deflate_fast in particular (its free run), with memLevel 9 as Clip Studio Paint has it.
#[test]
fn zlib_compress_is_exact() {
    use aqr_flate::zlib::{Params, Strategy};
    let mut inputs = zlib_inputs();
    inputs.push(vec![]);
    // Tiles as a painting has them: mostly empty, flat strokes, a gradient.
    let mut tile = vec![0u8; 1 << 20];
    for (i, px) in tile.chunks_mut(4).enumerate() {
        let (x, y) = (i % 256, i / 256);
        if (x / 37 + y / 53) % 5 == 1 {
            px.copy_from_slice(&[200, 40, 90, 255]);
        } else if y % 512 > 400 {
            px.copy_from_slice(&[x as u8, y as u8, 0, (x ^ y) as u8]);
        }
    }
    inputs.push(tile);
    let zlib = flate_zoo::backend("zlib").unwrap();
    let mut failures = vec![];
    for (level, strategy, wbits, mem) in (1..=9)
        .flat_map(|l| [(l, 0, 15, 8), (l, 0, 15, 9), (l, 0, 10, 1)])
        .chain([(1, 1, 15, 9), (1, 4, 15, 9), (2, 0, 12, 9), (3, 0, 15, 7)])
    {
        let enc = flate_zoo::Encoder::new(zlib)
            .set("level", level)
            .and_then(|e| e.set("strategy", strategy))
            .and_then(|e| e.set("wbits", wbits))
            .and_then(|e| e.set("mem", mem))
            .unwrap();
        let p = Params {
            level: level as u8,
            strategy: Strategy::ALL[strategy as usize],
            wbits: wbits as u8,
            mem: mem as u8,
        };
        for input in &inputs {
            if aqr_flate::zlib::compress(input, p) != Some(enc.compress(input).unwrap()) {
                failures.push(format!("{p} on {} bytes", input.len()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The emulation reproduces every token of real zlib output (all levels, strategies, window
/// sizes and memory levels), and its block size rule holds.
#[test]
fn zlib_emulation_is_exact() {
    use aqr_flate::deflate::Body;
    let mut failures = vec![];
    for enc in presets_of(flate_zoo::backend("zlib").unwrap(), Sweep::OneAtATime) {
        if enc.get("inbuf") != Some(0) || enc.get("outbuf") != Some(0) {
            continue;
        }
        let Some(p) = zlib_params(&enc) else { continue };
        for input in zlib_inputs() {
            let raw = enc.compress(&input).unwrap();
            let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
            let d = aqr_flate::detect::trial(&stream, &data, p, usize::MAX, usize::MAX);
            let fixed_strategy = p.strategy == aqr_flate::zlib::Strategy::Fixed;
            let blocks_ok = stream.blocks.iter().all(|b| match &b.body {
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
                    let fixed = matches!(b.body, Body::Fixed { .. });
                    (tokens.len() == p.block_tokens() || b.last)
                        && fixed == (fixed_strategy || aqr_flate::zlib::zlib_prefers_fixed(tokens))
                }
                Body::Stored { .. } => true,
            });
            // zlib alone regenerates the whole stream.
            let mut e = aqr_flate::zlib::Emulator::new(&data, p);
            let mut generated = vec![];
            let mut pos = 0;
            while let Some(g) = aqr_flate::zlib::zlib_block(&mut e, pos) {
                pos += match &g.body {
                    Body::Stored { len, .. } => *len as usize,
                    Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
                        tokens.iter().map(|t| t.span()).sum()
                    }
                };
                let last = g.last;
                generated.push(g);
                if last {
                    break;
                }
            }
            let regenerated =
                generated == stream.blocks && aqr_flate::zlib::compress(&input, p) == Some(raw);
            match d {
                Some(d) if d.misses == 0 && blocks_ok && regenerated => {}
                _ => failures.push(format!(
                    "{enc} on {} bytes: {d:?}, blocks ok: {blocks_ok}, regenerated: {regenerated}",
                    input.len()
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The SPMD emulation of several deflate_slow levels counts exactly the misses of separate
/// emulations, on zlib's own output and on other encoders'.
#[test]
fn multi_slow_matches_single() {
    use aqr_flate::detect;
    use aqr_flate::zlib::{Params, Strategy};
    let inputs = [
        include_bytes!("../src/zlib.rs").repeat(6),
        b"abcabcabd".repeat(20_000),
    ];
    let specs = [
        "zlib:level=4",
        "zlib:level=6",
        "zlib:level=9",
        "zlib:level=2",
        "zlib:strategy=filtered",
        "zlib:wbits=10",
        "zlib-ng:level=6",
        "zlib-chromium:level=9",
        "libdeflate:level=6",
    ];
    let lanes: Vec<Params> = (4..=9)
        .flat_map(|level| {
            [Strategy::Default, Strategy::Filtered].map(|strategy| Params {
                level,
                strategy,
                ..Params::default()
            })
        })
        .collect();
    for input in &inputs {
        for spec in specs {
            let raw = spec
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(input)
                .unwrap();
            let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
            for wbits in [15, 10] {
                let lanes: Vec<Params> = lanes.iter().map(|&p| Params { wbits, ..p }).collect();
                let multi = detect::multi_trial(&stream, &data, &lanes, usize::MAX, usize::MAX);
                assert_eq!(multi.len(), lanes.len(), "{spec}");
                for (m, &p) in multi.iter().zip(&lanes) {
                    let single = detect::trial(&stream, &data, p, usize::MAX, usize::MAX).unwrap();
                    assert_eq!(
                        (m.params, m.tokens, m.misses),
                        (p, single.tokens, single.misses),
                        "{spec} {p}"
                    );
                }
            }
        }
    }
}

/// The zlib header zlib itself writes for these parameters (deflate.c).
fn zlib_header_for(p: aqr_flate::zlib::Params) -> [u8; 2] {
    use aqr_flate::zlib::Strategy;
    let special = matches!(
        p.strategy,
        Strategy::HuffmanOnly | Strategy::Rle | Strategy::Fixed
    );
    let flevel = if special || p.level < 2 {
        0
    } else if p.level < 6 {
        1
    } else if p.level == 6 {
        2
    } else {
        3
    };
    let mut h: u16 = (0x08 | ((p.wbits as u16 - 8) << 4)) << 8 | flevel << 6;
    h += 31 - (h % 31);
    h.to_be_bytes()
}

#[test]
fn zlib_header_hints() {
    use aqr_flate::Hints;
    assert_eq!(Hints::zlib_header([0x78, 0x9c]).zlib_flevel, Some(2));
    assert_eq!(Hints::zlib_header([0x78, 0x9c]).wbits, Some(15));
    assert_eq!(Hints::zlib_header([0x78, 0x01]).zlib_flevel, Some(0));
    assert_eq!(Hints::zlib_header([0x78, 0xda]).zlib_flevel, Some(3));
    assert_eq!(Hints::zlib_header([0x78, 0x5e]).zlib_flevel, Some(1));
    assert_eq!(Hints::zlib_header([0x78, 0x00]), Hints::default());

    // With the hints of zlib's own header, every preset is still detected exactly.
    let input = include_bytes!("../src/zlib.rs").repeat(4);
    for enc in presets_of(flate_zoo::backend("zlib").unwrap(), Sweep::OneAtATime) {
        if enc.get("inbuf") != Some(0) || enc.get("outbuf") != Some(0) {
            continue;
        }
        let Some(p) = zlib_params(&enc) else { continue };
        let hints = Hints::zlib_header(zlib_header_for(p));
        assert!(hints.allows(&p), "{enc}");
        let raw = enc.compress(&input).unwrap();
        let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
        let candidates = aqr_flate::detect::candidates_hinted(&stream, &hints);
        let d = aqr_flate::detect::detect_refined(&stream, &data, candidates);
        assert!(d.is_some_and(|d| d.misses == 0), "{enc}: {d:?}");
    }
}

/// A large input that zlib compresses without long repeats across windows.
fn large_input() -> Vec<u8> {
    let srcs: [&[u8]; 4] = [
        include_bytes!("../src/lib.rs"),
        include_bytes!("../src/zlib.rs"),
        include_bytes!("../src/detect.rs"),
        include_bytes!("../src/deflate.rs"),
    ];
    let mut lcg = 3u32;
    let mut out = vec![];
    for k in 0..40u8 {
        // A different rendering of the text each time.
        out.extend(srcs[k as usize % 4].iter().map(|&b| {
            if b.is_ascii_lowercase() {
                b'a' + (b - b'a' + k) % 26
            } else {
                b
            }
        }));
        out.extend((0..2_000 * (k as usize % 3)).map(|_| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 24) as u8
        }));
    }
    out
}

/// Long zlib streams are cut into segments that each resume zlib's emulation from a
/// checkpoint: they cost next to nothing, and are rebuilt the same with any number of threads.
#[test]
fn zlib_checkpoints_reconstruct_in_parallel() {
    let input = large_input();
    assert!(input.len() > 1 << 20, "{}", input.len());
    let opts = aqr_flate::Options {
        segment: 128 << 10,
        ..aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap()
    };
    for (level, wbits) in [(6, 15), (9, 12), (4, 10)] {
        let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap())
            .set("level", level)
            .unwrap()
            .set("wbits", wbits)
            .unwrap();
        let raw = enc.compress(&input).unwrap();
        let (data, meta, report) = aqr_flate::deconstruct_report(&raw, &opts).unwrap();
        assert!(report.zlib.is_some(), "{enc}");
        assert_eq!(report.misses, 0, "{enc}");
        assert!(meta.len() < 1000, "{enc}: {} B", meta.len());
        for threads in [1, 3, 8] {
            assert!(
                aqr_flate::reconstruct_with_threads(&data, &meta, threads).unwrap() == raw,
                "{enc}, {threads} threads"
            );
        }
    }
}

/// A container's hints narrow zlib detection: a level-6 stream whose header claims levels 7-9
/// is not emulated at level 6, and its own header leads to level 6.
#[test]
fn deconstruction_follows_hints() {
    use aqr_flate::Hints;
    let input = include_bytes!("../src/zlib.rs").repeat(2);
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&input)
        .unwrap();
    let opts = aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap();
    for (header, level) in [([0x78, 0xda], None), ([0x78, 0x9c], Some(6))] {
        let hints = Hints::zlib_header(header);
        let (data, meta, report) = aqr_flate::deconstruct_hinted(&raw, &opts, &hints).unwrap();
        assert!(
            report.zlib.is_none_or(|p| hints.allows(&p)),
            "{header:02x?}: {:?}",
            report.zlib
        );
        if let Some(level) = level {
            assert_eq!(report.zlib.map(|p| p.level), Some(level), "{header:02x?}");
        }
        assert!(aqr_flate::reconstruct_with_threads(&data, &meta, 1).unwrap() == raw);
    }
}

/// Parameters given as a hint are taken when they predict every token, even where detection
/// would prefer others that do too.
#[test]
fn exact_hinted_params_are_taken() {
    use aqr_flate::zlib::Params;
    let input = include_bytes!("../src/zlib.rs")[..3000].to_vec();
    let raw = "zlib:level=7"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&input)
        .unwrap();
    let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
    let exact: Vec<Params> = (4..=9)
        .map(|level| Params {
            level,
            ..Params::default()
        })
        .filter(|&p| {
            aqr_flate::detect::trial(&stream, &data, p, usize::MAX, 0).is_some_and(|d| d.complete)
        })
        .collect();
    assert!(exact.len() >= 2, "{exact:?}");
    let opts = aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap();
    for p in exact {
        let hints = aqr_flate::Hints {
            params: Some(p),
            ..Default::default()
        };
        let (data, meta, report) = aqr_flate::deconstruct_hinted(&raw, &opts, &hints).unwrap();
        assert_eq!(report.zlib, Some(p));
        assert!(
            aqr_flate::reconstruct_with_threads(&data, &meta, 1).unwrap() == raw,
            "{p}"
        );
    }
}

/// Parameters given as a hint that do not predict every token change nothing.
#[test]
fn inexact_hinted_params_change_nothing() {
    let input = include_bytes!("../src/zlib.rs").repeat(2);
    let opts = aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap();
    let hints = aqr_flate::Hints {
        params: Some(aqr_flate::zlib::Params::default()),
        ..Default::default()
    };
    for spec in [
        "zlib:level=1",
        "zlib:level=9",
        "zlib:level=6,mem=9",
        "libdeflate:level=6",
    ] {
        let raw = spec
            .parse::<flate_zoo::Encoder>()
            .unwrap()
            .compress(&input)
            .unwrap();
        let plain = aqr_flate::deconstruct_with(&raw, &opts).unwrap();
        let (data, meta, _) = aqr_flate::deconstruct_hinted(&raw, &opts, &hints).unwrap();
        assert!((data, meta) == plain, "{spec}");
    }
}

/// Checks that `f`, an emulation with parameters `p` that has followed `stream` up to
/// `f.pos`, logged the mispredictions an emulator of its own makes on the way there, and then
/// carries on as that emulator does.
fn check_followed(
    stream: &aqr_flate::deflate::Stream,
    data: &[u8],
    p: aqr_flate::zlib::Params,
    f: aqr_flate::zlib::Followed,
    what: &str,
) {
    use aqr_flate::deflate::Body;
    let mut e = aqr_flate::zlib::Emulator::new(data, p);
    let (mut followed, mut log, mut pos) = (Some(f), vec![], 0);
    let mut follow = |log: &[(usize, _)], pos: usize| {
        // Once the emulator of its own gets where `f` is, they agree from there on.
        if followed.as_ref().is_some_and(|f| f.pos == pos) {
            let f = followed.take().unwrap();
            assert_eq!(f.log, log, "{what}: log");
            return Some(f.emu);
        }
        None
    };
    let mut fe: Option<aqr_flate::zlib::Emulator> = None;
    for b in &stream.blocks {
        let tokens = match &b.body {
            Body::Stored { len, .. } => {
                pos += *len as usize;
                let ok = e.skip_to(pos);
                if let Some(fe) = &mut fe {
                    assert_eq!(fe.skip_to(pos), ok, "{what}: skip to {pos}");
                }
                fe = fe.or_else(|| follow(&log, pos));
                continue;
            }
            Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens,
        };
        for &t in tokens {
            fe = fe.or_else(|| follow(&log, pos));
            let pred = e.predict();
            if let Some(fe) = &mut fe {
                assert_eq!(fe.predict(), pred, "{what}: at {pos}");
                assert_eq!(fe.commit(t), e.commit(t), "{what}: at {pos}");
            } else {
                match pred {
                    Some((at, pt)) if at == pos && pt != t => log.push((at, pt)),
                    Some((at, _)) if at == pos => {}
                    _ => panic!("{what}: lost at {pos}"),
                }
                assert!(e.commit(t), "{what}: lost at {pos}");
            }
            pos += t.span();
        }
    }
    let fe = fe.or_else(|| follow(&log, pos));
    assert!(
        followed.is_none(),
        "{what}: never got to where the emulation was"
    );
    assert_eq!(fe.unwrap().predict(), e.predict(), "{what}: at the end");
}

/// A lane taken out of a MultiSlow partway carries on as an emulator of its own would, and
/// so does the emulation detection followed a stream with (stored blocks or not).
#[test]
fn followed_emulations_carry_on() {
    use aqr_flate::deflate::Body;
    use aqr_flate::zlib::{MultiSlow, Params};
    let lanes: Vec<Params> = (4..=9)
        .map(|level| Params {
            level,
            ..Params::default()
        })
        .collect();
    let inputs = zlib_inputs();
    for spec in [
        "zlib:level=6",
        "zlib:level=9,mem=1",
        "zlib-ng:level=6",
        "libdeflate:level=9",
    ] {
        for input in &inputs {
            let raw = spec
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(input)
                .unwrap();
            let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
            if let Some((d, Some(f))) = aqr_flate::detect::detect_refined_followed(
                &stream,
                &data,
                aqr_flate::detect::candidates(&stream),
            ) {
                check_followed(&stream, &data, d.params, f, &format!("{spec} detected"));
            }
            let stored = stream
                .blocks
                .iter()
                .any(|b| matches!(b.body, Body::Stored { .. }));
            if stored {
                continue;
            }
            let tokens: Vec<_> = stream
                .blocks
                .iter()
                .flat_map(|b| match &b.body {
                    Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => tokens.clone(),
                    Body::Stored { .. } => unreachable!(),
                })
                .collect();
            // After literals and matches alike.
            for cut in
                [0, 1, 2, 3, 7, 100, 999, 1000, 1001, tokens.len()].map(|c| c.min(tokens.len()))
            {
                for k in 0..lanes.len() {
                    let mut m = MultiSlow::new(&data, &lanes);
                    let mut pos = 0;
                    for &t in &tokens[..cut] {
                        assert_eq!(m.predict(), Some(pos));
                        assert!(m.commit(t));
                        pos += t.span();
                    }
                    let what = format!("{spec} lane {} after {cut}", lanes[k]);
                    check_followed(&stream, &data, lanes[k], m.into_followed(k, pos), &what);
                }
            }
        }
    }
}
