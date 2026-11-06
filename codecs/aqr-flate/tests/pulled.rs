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
