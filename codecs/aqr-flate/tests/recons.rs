//! Round trips over real encoders from flate-zoo, in every kind of profile.

use flate_zoo::{Sweep, backends, presets_of};

fn inputs() -> Vec<Vec<u8>> {
    let mut lcg = 0x1234_5678u32;
    let noise: Vec<u8> = (0..70_000)
        .map(|_| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 24) as u8
        })
        .collect();
    let text = include_bytes!("../src/zlib.rs").repeat(3);
    vec![
        vec![],
        b"a".to_vec(),
        b"abcabcabd".repeat(20_000),
        noise,
        text,
        vec![0; 100_000],
    ]
}

/// Profiles worth testing apart: the fastest and smallest static ones, the default one, and
/// every adaptive one.
fn profiles() -> Vec<u16> {
    let mut p = vec![0, aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE - 1];
    p.extend(aqr_flate::ADAPTIVE..aqr_flate::profiles().end);
    p
}

#[test]
fn every_backend_round_trips() {
    let inputs = inputs();
    let mut failures = vec![];
    for profile in profiles() {
        let opts = aqr_flate::Options::for_profile(profile).unwrap();
        every_backend(&inputs, &opts, &mut failures);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn every_backend(inputs: &[Vec<u8>], opts: &aqr_flate::Options, failures: &mut Vec<String>) {
    {
        for enc in backends()
            .iter()
            .flat_map(|&b| presets_of(b, Sweep::Default))
        {
            for input in inputs {
                let Ok(raw) = enc.compress(input) else {
                    continue;
                };
                let what = format!("{enc} on {} bytes, profile {}", input.len(), opts.profile);
                let r = aqr_flate::deconstruct_with(&raw, opts).and_then(|(data, meta)| {
                    assert_eq!(meta[..2], opts.profile.to_le_bytes(), "{what}");
                    assert_eq!(data, *input, "{what}");
                    for threads in [1, 3] {
                        let back = aqr_flate::reconstruct_with_threads(&data, &meta, threads)?;
                        assert!(back == raw, "{what}: differs with {threads} threads");
                    }
                    Ok(())
                });
                if let Err(e) = r {
                    failures.push(format!("{what}: {e}"));
                }
            }
        }
    }
}

fn mixed() -> Vec<u8> {
    let mut lcg = 7u32;
    let mut mixed = include_bytes!("../src/zlib.rs").repeat(4);
    mixed.extend((0..150_000).map(|_| {
        lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (lcg >> 24) as u8
    }));
    mixed.extend(include_bytes!("../src/deflate.rs").repeat(4));
    mixed
}

/// Small segments, so that streams are cut in many places (also around stored blocks), give
/// the same reconstruction whatever the number of threads, and whether it is taken whole or
/// in pieces as it is written. With zlib emulation, every
/// segment resumes it from its own checkpoint.
#[test]
fn segments_round_trip() {
    let mixed = mixed();
    for profile in [aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
        let opts = aqr_flate::Options {
            segment: 10_000,
            prefix: 20_000,
            ..aqr_flate::Options::for_profile(profile).unwrap()
        };
        for spec in [
            "zlib:level=6",
            "zlib:level=1",
            "zlib:level=9",
            "zlib:level=4,strategy=filtered",
            "zlib:level=6,strategy=rle",
            "zlib:level=6,strategy=huffman",
            "zlib:level=6,wbits=10,mem=3",
            "libdeflate:level=9",
            "stb",
            "zopfli",
        ] {
            let raw = spec
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(&mixed)
                .unwrap();
            let (data, meta, report) = aqr_flate::deconstruct_report(&raw, &opts).unwrap();
            // (The rule for RLE predicts as well as zlib's own.)
            if profile >= aqr_flate::ADAPTIVE && spec.starts_with("zlib") && !spec.contains("rle") {
                assert!(report.zlib.is_some(), "{spec}: no zlib emulation");
            }
            for threads in [1, 2, 7] {
                assert!(
                    aqr_flate::reconstruct_with_threads(&data, &meta, threads).unwrap() == raw,
                    "{spec}, profile {profile}, {threads} threads"
                );
            }
            // given as it is written, in pieces
            let model = aqr_flate::profile_data(profile).unwrap();
            let mut pieces = Vec::new();
            aqr_flate::reconstruct_into(&data, &meta, model, 2, &mut |b| pieces.push(b.to_vec()))
                .unwrap();
            assert!(
                pieces.len() > 2 && pieces.concat() == raw,
                "{spec}, profile {profile}: {} pieces",
                pieces.len()
            );
        }
    }
}

/// zlib emulation that resumes at every segment costs about what one long segment does, and
/// little at all: the emulator's state at each cut is all it takes.
#[test]
fn zlib_emulation_resumes() {
    let mixed = mixed();
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&mixed)
        .unwrap();
    let size = |segment| {
        let opts = aqr_flate::Options {
            segment,
            ..aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap()
        };
        aqr_flate::deconstruct_with(&raw, &opts).unwrap().1.len()
    };
    let (one, many) = (size(usize::MAX), size(10_000));
    assert!(one < 200, "{one} B in one segment");
    assert!(many < one + 40 * 16, "{many} B in segments, {one} B in one");
}

/// deflate_fast's emulation (levels 1-3) cannot resume at a segment's start, so the stream is
/// coded as one segment rather than emulated in the first one only.
#[test]
fn unresumable_zlib_emulation_takes_one_segment() {
    let mixed = mixed();
    for spec in ["zlib:level=1", "zlib:level=3,strategy=filtered"] {
        let raw = spec
            .parse::<flate_zoo::Encoder>()
            .unwrap()
            .compress(&mixed)
            .unwrap();
        let opts = aqr_flate::Options {
            segment: 10_000,
            ..aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap()
        };
        let (data, meta, report) = aqr_flate::deconstruct_report(&raw, &opts).unwrap();
        assert!(report.zlib.is_some(), "{spec}: no zlib emulation");
        assert!(meta.len() < 300, "{spec}: {} B", meta.len());
        assert!(
            aqr_flate::reconstruct_with_threads(&data, &meta, 4).unwrap() == raw,
            "{spec}"
        );
    }
}

/// A chain rule alone, in small segments.
#[test]
fn chain_rule_round_trips() {
    let chain: aqr_flate::Rule = "chain,depth=256,nice=128".parse().unwrap();
    assert_eq!(chain.to_string(), "chain,depth=256,nice=128");
    let mixed = mixed();
    let opts = aqr_flate::Options {
        rules: vec![chain],
        map: Default::default(),
        segment: 10_000,
        zlib: false,
        chain: false,
        ..aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap()
    };
    for spec in ["libdeflate:level=9", "zopfli", "zlib:level=6", "stb"] {
        let raw = spec
            .parse::<flate_zoo::Encoder>()
            .unwrap()
            .compress(&mixed)
            .unwrap();
        let (data, meta, report) = aqr_flate::deconstruct_report(&raw, &opts).unwrap();
        assert_eq!(report.rule, chain, "{spec}");
        for threads in [1, 3] {
            assert!(
                aqr_flate::reconstruct_with_threads(&data, &meta, threads).unwrap() == raw,
                "{spec}, {threads} threads"
            );
        }
    }
}

/// The guaranteed encoding of FORMAT.md's formula module: the literals rule, one segment and
/// no zlib emulation code every stream, in static and adaptive profiles alike.
#[test]
fn literals_rule_in_one_segment_round_trips() {
    let inputs = inputs();
    let mut failures = vec![];
    for profile in [0, aqr_flate::ADAPTIVE] {
        let opts = aqr_flate::Options {
            rules: vec!["literals".parse().unwrap()],
            map: Default::default(),
            segment: usize::MAX,
            zlib: false,
            chain: false,
            ..aqr_flate::Options::for_profile(profile).unwrap()
        };
        every_backend(&inputs, &opts, &mut failures);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A stream with more blocks than a record may hold (the data's length plus 2^20) is not
/// deconstructed into a record that cannot be read.
#[test]
fn too_many_blocks_are_not_deconstructed() {
    // Empty fixed blocks of 10 bits each: BFINAL, BTYPE 1, and the end of block (seven 0s).
    let n: usize = (1 << 20) + 4096;
    let mut raw = vec![0u8; (n * 10).div_ceil(8)];
    for i in 0..n {
        let at = i * 10 + 1;
        raw[at / 8] |= 1 << (at % 8);
    }
    let last = (n - 1) * 10;
    raw[last / 8] |= 1 << (last % 8);
    if let Ok((data, meta)) = aqr_flate::deconstruct(&raw) {
        assert!(aqr_flate::reconstruct(&data, &meta).unwrap() == raw);
    }
}

#[test]
fn unknown_profiles_are_rejected() {
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(b"abcabcabd")
        .unwrap();
    let (data, mut meta) = aqr_flate::deconstruct(&raw).unwrap();
    meta[..2].copy_from_slice(&aqr_flate::profiles().end.to_le_bytes());
    assert!(aqr_flate::reconstruct(&data, &meta).is_err());
    assert!(aqr_flate::Options::for_profile(aqr_flate::profiles().end).is_err());
}

/// Damaged metadata gives an error or a different stream, never a panic or a hang.
#[test]
fn corrupt_metadata_is_rejected() {
    let input = include_bytes!("../src/zlib.rs").repeat(2);
    for (spec, profile, segment) in [
        ("libdeflate:level=6", aqr_flate::DEFAULT_PROFILE, 1 << 20),
        ("libdeflate:level=6", aqr_flate::ADAPTIVE, 1 << 20),
        ("zlib:level=6", aqr_flate::ADAPTIVE, 20_000),
    ] {
        let raw = spec
            .parse::<flate_zoo::Encoder>()
            .unwrap()
            .compress(&input)
            .unwrap();
        let opts = aqr_flate::Options {
            segment,
            ..aqr_flate::Options::for_profile(profile).unwrap()
        };
        let (data, meta) = aqr_flate::deconstruct_with(&raw, &opts).unwrap();
        damage(&data, &meta);
    }
}

fn damage(data: &[u8], meta: &[u8]) {
    {
        let mut lcg = 99u32;
        for i in 0..meta.len() * 8 {
            let mut bad = meta.to_vec();
            bad[i / 8] ^= 1 << (i % 8);
            let _ = aqr_flate::reconstruct_with_threads(data, &bad, 2);
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let mut cut = meta.to_vec();
            cut.truncate(lcg as usize % meta.len());
            let _ = aqr_flate::reconstruct_with_threads(data, &cut, 1);
        }
    }
}

/// Stream lengths that overflow the position arithmetic are rejected (in debug builds too).
#[test]
fn huge_stream_lengths_are_rejected() {
    let mut meta = aqr_flate::DEFAULT_PROFILE.to_le_bytes().to_vec();
    meta.extend([0xff; 9]);
    meta.push(0x01);
    meta.push(0);
    assert!(aqr_flate::reconstruct(b"", &meta).is_err());
    let mut meta = aqr_flate::DEFAULT_PROFILE.to_le_bytes().to_vec();
    meta.push(0);
    meta.extend([0xff; 9]);
    meta.push(0x01);
    assert!(aqr_flate::reconstruct(b"", &meta).is_err());
}

/// Streams of more data than [`aqr_flate::Options::max_data`] are refused, with no more of it
/// made than a match beyond, even within a block; streams of just that much are not.
#[test]
fn data_beyond_the_limit_is_refused() {
    let input = include_bytes!("../src/zlib.rs").repeat(2);
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&input)
        .unwrap();
    for (profile, verify) in [
        (aqr_flate::DEFAULT_PROFILE, false),
        (aqr_flate::DEFAULT_PROFILE, true),
        (aqr_flate::ADAPTIVE, false),
    ] {
        let opts = |max_data| aqr_flate::Options {
            verify,
            max_data,
            ..aqr_flate::Options::for_profile(profile).unwrap()
        };
        let (data, _) = aqr_flate::deconstruct_with(&raw, &opts(input.len())).unwrap();
        assert!(data == input);
        assert!(matches!(
            aqr_flate::deconstruct_with(&raw, &opts(input.len() - 1)),
            Err(aqr_flate::Error::Limit)
        ));
    }
    struct Count(usize);
    impl aqr_flate::inflate::Sink for Count {
        fn lit(&mut self, _: u8) {
            self.0 += 1;
        }
        fn mat(&mut self, len: u16, _: u16, _: bool) {
            self.0 += len as usize;
        }
        fn stored(&mut self, b: &[u8]) {
            self.0 += b.len();
        }
    }
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&vec![0; 1 << 20])
        .unwrap();
    let mut n = Count(0);
    let r = aqr_flate::inflate::parse_head_max(&raw, usize::MAX, 1000, &mut n);
    assert!(
        matches!(r, Err(aqr_flate::Error::Limit)) && n.0 <= 1000 + 258,
        "{}",
        n.0
    );
}

/// The formula module reads the record with the model data it is given as resources, in
/// binary, not with the built-in one: the profile only tells static coding from adaptive.
#[test]
fn formula_module_takes_its_resources() {
    use aqr_flate::formula;
    let input = include_bytes!("../src/zlib.rs").repeat(2);
    let raw = "zlib:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&input)
        .unwrap();
    for profile in profiles() {
        let opts = aqr_flate::Options::for_profile(profile).unwrap();
        let (data, meta) = aqr_flate::deconstruct_with(&raw, &opts).unwrap();
        let res = formula::resources(profile).unwrap();
        let res: Vec<&[u8]> = res.iter().map(|r| &r[..]).collect();
        let decode =
            |res: &[&[u8]]| formula::decode(&[], res, &[&data, &meta, &[]], raw.len() as u64, 1);
        assert_eq!(decode(&res).unwrap(), raw, "profile {profile}");
        // Too few, and the coding of the other kind of profile.
        assert!(decode(&res[..1]).is_err());
        let other = formula::resources(if profile < aqr_flate::ADAPTIVE {
            aqr_flate::ADAPTIVE
        } else {
            0
        })
        .unwrap();
        assert!(decode(&[res[0], &other[1]]).is_err());
        // Another catalog reads the rule's index (of a static profile) as another rule.
        if profile < aqr_flate::ADAPTIVE {
            let mut rev = formula::rules_from_bytes(res[0]).unwrap();
            rev.reverse();
            let rev = formula::rules_to_bytes(&rev);
            assert!(
                decode(&[&rev, res[1]]).ok().as_ref() != Some(&raw),
                "profile {profile}"
            );
        }
        // Rules damaged.
        let mut bad = res[0].to_vec();
        bad[1] = 9;
        assert!(decode(&[&bad, res[1]]).is_err());
    }
}

/// The formula module rebuilds several streams of a profile, one after another, then what its
/// parameters derive of them; and fails on records of another profile, on an index of other
/// lengths and on any other length.
#[test]
fn formula_module_of_several_streams() {
    use aqr_flate::formula::{self, Derived};
    let inputs: Vec<Vec<u8>> = vec![
        include_bytes!("../src/zlib.rs").to_vec(),
        Vec::new(),
        include_bytes!("../src/lib.rs").repeat(3),
    ];
    let raws: Vec<Vec<u8>> = inputs
        .iter()
        .map(|d| {
            "zlib:level=6"
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(d)
                .unwrap()
        })
        .collect();
    let profile = aqr_flate::DEFAULT_PROFILE;
    let opts = aqr_flate::Options::for_profile(profile).unwrap();
    let parts: Vec<(Vec<u8>, Vec<u8>)> = raws
        .iter()
        .map(|r| aqr_flate::deconstruct_with(r, &opts).unwrap())
        .collect();
    let data: Vec<u8> = parts.iter().flat_map(|p| p.0.clone()).collect();
    let records: Vec<u8> = parts.iter().flat_map(|p| p.1.clone()).collect();
    let lens: Vec<(usize, usize)> = parts.iter().map(|p| (p.0.len(), p.1.len())).collect();
    let index = formula::index(&lens);
    let res = formula::resources(profile).unwrap();
    let res: Vec<&[u8]> = res.iter().map(|r| &r[..]).collect();
    let all = raws.concat();
    for derived in [Derived::None, Derived::Zip, Derived::Gzip] {
        let mut want = all.clone();
        for (d, r) in inputs.iter().zip(&raws) {
            derived.put(d, r.len(), &mut want);
        }
        let decode = |index: &[u8], len: usize| {
            formula::decode(
                &derived.params(),
                &res,
                &[&data, &records, index],
                len as u64,
                4,
            )
        };
        assert_eq!(decode(&index, want.len()).unwrap(), want);
        assert!(decode(&index, want.len() + 1).is_err());
        assert!(decode(&formula::index(&lens[..2]), want.len()).is_err());
        assert!(decode(&[], want.len()).is_err());
    }
    // A gzip member's trailer: the check value of CRC-32, and the length.
    let mut t = Vec::new();
    Derived::Gzip.put(b"123456789", 0, &mut t);
    assert_eq!(t, [0x26, 0x39, 0xf4, 0xcb, 9, 0, 0, 0]);
    // Records of another profile.
    let other = aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE).unwrap();
    let mut mixed = parts[0].1.clone();
    mixed.extend_from_slice(&parts[1].1);
    let last = aqr_flate::deconstruct_with(&raws[2], &other).unwrap().1;
    mixed.extend_from_slice(&last);
    let mut lens = lens;
    lens[2].1 = last.len();
    let index = formula::index(&lens);
    assert!(formula::decode(&[], &res, &[&data, &mixed, &index], all.len() as u64, 1).is_err());
}

/// Coding under several chain rules at once (`tokens::code_chains`, one walk of each chain
/// for all of them) gives each rule's coding on its own, stored blocks and all.
#[test]
fn chain_rules_coded_at_once() {
    use aqr_flate::adapt::{Adaptive, Enc};
    use aqr_flate::deflate::Body;
    use aqr_flate::tokens::{self, Kind, Range, Rule};
    let priors = aqr_flate::profile_priors(aqr_flate::ADAPTIVE).unwrap();
    let rules: Vec<Rule> = [(7, 5), (0, 0), (4, 1), (12, 5)]
        .map(|(depth, nice)| Rule {
            kind: Kind::Chain,
            depth,
            nice,
            ..Rule::default()
        })
        .to_vec();
    let mut mixed = inputs().concat();
    mixed.extend(include_bytes!("../src/lib.rs"));
    for spec in ["zlib:level=6", "libdeflate:level=9", "miniz:level=1"] {
        for input in inputs().iter().chain([&mixed]) {
            let raw = spec
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(input)
                .unwrap();
            let (stream, data) = aqr_flate::deflate::parse(&raw).unwrap();
            let (mut tokens, mut skips) = (vec![], vec![]);
            for b in &stream.blocks {
                match &b.body {
                    Body::Stored { len, .. } => skips.push((tokens.len(), *len as usize)),
                    Body::Fixed { tokens: t } | Body::Dynamic { tokens: t, .. } => {
                        tokens.extend_from_slice(t)
                    }
                }
            }
            let range = Range {
                first: 0,
                count: tokens.len(),
                pos: 0,
                end: data.len(),
                skips: &skips,
            };
            let mut at_once: Vec<_> = rules
                .iter()
                .map(|_| Adaptive::new(Enc::new(), priors, None))
                .collect();
            tokens::code_chains(&mut at_once, &data, &rules, &range, &tokens, None).unwrap();
            for (&r, a) in rules.iter().zip(at_once) {
                let mut alone = Adaptive::new(Enc::new(), priors, None);
                tokens::code_range(&mut alone, &data, r, &range, Some(&tokens), None).unwrap();
                assert_eq!(a.misses, alone.misses, "{spec} {r}");
                assert!(a.b.finish() == alone.b.finish(), "{spec} {r}");
            }
        }
    }
}
