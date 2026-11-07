//! `reconstruct_pulled`, the data read a piece at a time, rebuilds what `reconstruct` does.

use aqr_flate::{
    Options, Pull, profile_data, profiles, reconstruct_pulled, reconstruct_with_threads,
};

/// About 7 MB: text varied as it goes, noise (stored blocks), runs.
fn sample() -> Vec<u8> {
    let mut d = vec![];
    for k in 0..24u8 {
        for src in [
            include_bytes!("../src/lib.rs").as_slice(),
            include_bytes!("../src/zlib.rs"),
        ] {
            d.extend(
                src.iter()
                    .map(|&b| if b == b'e' { b'a' + k % 20 } else { b }),
            );
        }
        if k % 8 == 3 {
            d.extend(aqr_common::rng::noise(300_000));
            d.extend(std::iter::repeat_n(k, 200_000));
        }
    }
    d
}

/// The stream rebuilt from `data` and `meta` pulled in pieces of `piece` bytes at most.
fn pulled(data: &[u8], meta: &[u8], threads: usize, piece: usize) -> Vec<u8> {
    let mut at = 0;
    let mut read = |v: &mut Vec<u8>, mut k: usize| {
        while k > 0 {
            let m = k.min(piece);
            v.extend_from_slice(&data[at..at + m]);
            (at, k) = (at + m, k - m);
        }
        Ok(())
    };
    let mut pull = Pull::new(&mut read);
    let mut out = vec![];
    let model = profile_data(aqr_flate::record_profile(meta).unwrap()).unwrap();
    let r = reconstruct_pulled(data.len(), &mut pull, meta, model, threads, &mut |b| {
        out.extend_from_slice(b)
    });
    if let Err(e) = r {
        panic!(
            "{e:?} after {} bytes of the stream, {} of data, {threads} threads",
            out.len(),
            data.len()
        );
    }
    drop(pull);
    assert_eq!(at, data.len(), "all of the data read");
    out
}

#[test]
fn pulled_as_whole() {
    let d = sample();
    for spec in [
        "zlib:level=1",
        "zlib:level=6",
        "zlib:level=9",
        "libdeflate:level=6",
        "zlib:level=6,wbits=10,mem=3",
    ] {
        let raw = spec
            .parse::<flate_zoo::Encoder>()
            .unwrap()
            .compress(&d)
            .unwrap();
        for profile in profiles() {
            // (adaptive profiles on the first two only: slow)
            if profile >= aqr_flate::ADAPTIVE
                && !spec.ends_with("level=6")
                && !spec.ends_with("level=1")
            {
                continue;
            }
            let opts = Options {
                segment: 64 << 10,
                threads: 4,
                ..Options::for_profile(profile).unwrap()
            };
            let (data, meta) = aqr_flate::deconstruct_with(&raw, &opts).unwrap();
            assert_eq!(reconstruct_with_threads(&data, &meta, 4).unwrap(), raw);
            for (threads, piece) in [(1, 1 << 20), (3, 4097), (8, 70_000)] {
                assert!(
                    pulled(&data, &meta, threads, piece) == raw,
                    "{spec}, profile {profile}, {threads} threads"
                );
            }
        }
    }
}

/// A long stream of one byte: few tokens for its data, and a short stream for it.
#[test]
fn pulled_zeros() {
    for mib in [1, 2, 3, 5, 9, 96] {
        let d = vec![0; mib << 20];
        for spec in ["zlib:level=6", "zlib:level=1"] {
            let raw = spec
                .parse::<flate_zoo::Encoder>()
                .unwrap()
                .compress(&d)
                .unwrap();
            let (data, meta) = aqr_flate::deconstruct(&raw).unwrap();
            assert!(aqr_flate::reconstruct(&data, &meta).unwrap() == raw);
            for threads in [1, 3] {
                assert!(
                    pulled(&data, &meta, threads, 1 << 20) == raw,
                    "{mib} MiB {spec} {threads}"
                );
            }
        }
    }
}

/// Streams of stored blocks only, longer than what is written at once.
#[test]
fn pulled_stored() {
    let d = aqr_common::rng::noise(9 << 20);
    let raw = "zlib:level=1"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&d)
        .unwrap();
    let (data, meta) = aqr_flate::deconstruct(&raw).unwrap();
    assert!(pulled(&data, &meta, 2, 5000) == raw);
}

/// A stream of `data` (literals only, so that tokens are many) cut in blocks of `counts` tokens
/// (the last block takes the rest), each as dynamic as zlib would write it or fixed (`fixed`).
fn blocks_of(data: &[u8], counts: &[usize], fixed: &[usize]) -> Vec<u8> {
    use aqr_flate::deflate::{self, Block, Body, Stream};
    let raw = "zlib:level=1,strategy=huffman"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(data)
        .unwrap();
    let (stream, parsed) = deflate::parse(&raw).unwrap();
    assert!(parsed == data);
    let mut tokens = vec![];
    for b in &stream.blocks {
        match &b.body {
            Body::Dynamic { tokens: t, .. } | Body::Fixed { tokens: t } => {
                tokens.extend_from_slice(t)
            }
            Body::Stored { .. } => panic!("stored"),
        }
    }
    let mut blocks = vec![];
    let mut rest = &tokens[..];
    for (i, &c) in counts.iter().chain(&[rest.len()]).enumerate() {
        let (t, r) = rest.split_at(c.min(rest.len()));
        rest = r;
        let last = i == counts.len();
        let body = match fixed.contains(&i) {
            true => Body::Fixed { tokens: t.to_vec() },
            false => Body::Dynamic {
                header: aqr_flate::header::zlib_header(t),
                tokens: t.to_vec(),
            },
        };
        blocks.push(Block { last, body });
    }
    let stream = Stream {
        blocks,
        tail_pad: 0,
        trailing: vec![],
    };
    let out = deflate::write(&stream, data).unwrap();
    assert!(deflate::parse(&out).unwrap().1 == data);
    out
}

/// Blocks of BIG tokens or more are written as their tokens are decoded (their frequencies in
/// the record): blocks around BIG, a few times it, fixed ones, many batches of segments.
#[test]
fn pulled_big_blocks() {
    let big = aqr_flate::BIG;
    // (text only: no stored blocks)
    let d: Vec<u8> = (0..24u8)
        .flat_map(|k| {
            let src = include_bytes!("../src/lib.rs");
            src.iter()
                .map(move |&b| if b == b'e' { b'a' + k % 20 } else { b })
        })
        .take(3 * big * 2 + big / 2)
        .collect();
    assert!(d.len() == 3 * big * 2 + big / 2);
    let cases: [(&[usize], &[usize]); 4] = [
        (&[big - 1, big, big + 1, 100], &[]),
        (&[1000, 2 * big + 7, 5, big], &[1]),
        (&[big], &[0, 1]),
        (&[], &[]),
    ];
    for (counts, fixed) in cases {
        let raw = blocks_of(&d, counts, fixed);
        for profile in [aqr_flate::DEFAULT_PROFILE, aqr_flate::ADAPTIVE] {
            let opts = Options {
                segment: 100_000,
                threads: 4,
                ..Options::for_profile(profile).unwrap()
            };
            let (data, meta) = aqr_flate::deconstruct_with(&raw, &opts).unwrap();
            assert!(
                reconstruct_with_threads(&data, &meta, 1).unwrap() == raw,
                "{counts:?} {profile}"
            );
            assert!(
                reconstruct_with_threads(&data, &meta, 4).unwrap() == raw,
                "{counts:?} {profile}"
            );
            for (threads, piece) in [(1, 1 << 20), (3, 4097), (8, 70_000)] {
                assert!(
                    pulled(&data, &meta, threads, piece) == raw,
                    "{counts:?} {profile} {threads} threads"
                );
            }
        }
    }
}
