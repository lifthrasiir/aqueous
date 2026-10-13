//! zlib's emulation and detection against real zlib output from flate-zoo.

use flate_zoo::{Sweep, presets_of};

/// zlib's Huffman headers are what [`flate_recons::header::zlib_header`] builds for each block.
#[test]
fn zlib_headers_are_predicted() {
    use flate_recons::deflate::Body;
    let text = include_bytes!("../src/zlib.rs").repeat(3);
    for level in 1..=9 {
        let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap())
            .set("level", level)
            .unwrap();
        let raw = enc.compress(&text).unwrap();
        let (stream, _) = flate_recons::deflate::parse(&raw).unwrap();
        for (i, b) in stream.blocks.iter().enumerate() {
            if let Body::Dynamic { header, tokens } = &b.body {
                assert!(
                    flate_recons::header::zlib_header(tokens) == *header,
                    "level {level}, block {i}"
                );
            }
        }
    }
}

fn zlib_params(enc: &flate_zoo::Encoder) -> Option<flate_recons::zlib::Params> {
    use flate_recons::zlib::{Params, Strategy};
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

/// The emulation reproduces every token of real zlib output (all levels, strategies, window
/// sizes and memory levels), and its block size rule holds.
#[test]
fn zlib_emulation_is_exact() {
    use flate_recons::deflate::Body;
    let mut failures = vec![];
    for enc in presets_of(flate_zoo::backend("zlib").unwrap(), Sweep::OneAtATime) {
        if enc.get("inbuf") != Some(0) || enc.get("outbuf") != Some(0) {
            continue;
        }
        let Some(p) = zlib_params(&enc) else { continue };
        for input in zlib_inputs() {
            let raw = enc.compress(&input).unwrap();
            let (stream, data) = flate_recons::deflate::parse(&raw).unwrap();
            let d = flate_recons::detect::trial(&stream, &data, p, usize::MAX, usize::MAX);
            let fixed_strategy = p.strategy == flate_recons::zlib::Strategy::Fixed;
            let blocks_ok = stream.blocks.iter().all(|b| match &b.body {
                Body::Fixed { tokens } | Body::Dynamic { tokens, .. } => {
                    let fixed = matches!(b.body, Body::Fixed { .. });
                    (tokens.len() == p.block_tokens() || b.last)
                        && fixed
                            == (fixed_strategy || flate_recons::zlib::zlib_prefers_fixed(tokens))
                }
                Body::Stored { .. } => true,
            });
            // zlib alone regenerates the whole stream.
            let mut e = flate_recons::zlib::Emulator::new(&data, p);
            let mut generated = vec![];
            let mut pos = 0;
            while let Some(g) = flate_recons::zlib::zlib_block(&mut e, pos) {
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
            let regenerated = generated == stream.blocks;
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
    use flate_recons::detect;
    use flate_recons::zlib::{Params, Strategy};
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
            let (stream, data) = flate_recons::deflate::parse(&raw).unwrap();
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
fn zlib_header_for(p: flate_recons::zlib::Params) -> [u8; 2] {
    use flate_recons::zlib::Strategy;
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
    use flate_recons::Hints;
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
        let (stream, data) = flate_recons::deflate::parse(&raw).unwrap();
        let candidates = flate_recons::detect::candidates_hinted(&stream, &hints);
        let d = flate_recons::detect::detect_refined(&stream, &data, candidates);
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
    let opts = flate_recons::Options {
        segment: 128 << 10,
        ..flate_recons::Options::for_profile(flate_recons::ADAPTIVE).unwrap()
    };
    for (level, wbits) in [(6, 15), (9, 12), (4, 10)] {
        let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap())
            .set("level", level)
            .unwrap()
            .set("wbits", wbits)
            .unwrap();
        let raw = enc.compress(&input).unwrap();
        let (data, meta, report) = flate_recons::deconstruct_report(&raw, &opts).unwrap();
        assert!(report.zlib.is_some(), "{enc}");
        assert_eq!(report.misses, 0, "{enc}");
        assert!(meta.len() < 1000, "{enc}: {} B", meta.len());
        for threads in [1, 3, 8] {
            assert!(
                flate_recons::reconstruct_with_threads(&data, &meta, threads).unwrap() == raw,
                "{enc}, {threads} threads"
            );
        }
    }
}
