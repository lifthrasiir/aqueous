//! Round trips of the rule-based (fast) representation over real encoders from flate-zoo.

use flate_recons::fast;
use flate_zoo::{Sweep, backends, presets_of};

fn inputs() -> Vec<Vec<u8>> {
    let mut lcg = 0x1234_5678u32;
    let noise: Vec<u8> = (0..70_000)
        .map(|_| {
            lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (lcg >> 24) as u8
        })
        .collect();
    let text = include_bytes!("../src/predict.rs").repeat(3);
    vec![
        vec![],
        b"a".to_vec(),
        b"abcabcabd".repeat(20_000),
        noise,
        text,
        vec![0; 100_000],
    ]
}

#[test]
fn every_backend_round_trips() {
    let inputs = inputs();
    let mut failures = vec![];
    for &b in backends() {
        for enc in presets_of(b, Sweep::Default) {
            for input in &inputs {
                let Ok(raw) = enc.compress(input) else {
                    continue;
                };
                let what = format!("{enc} on {} bytes", input.len());
                let r = fast::deconstruct(&raw).and_then(|(data, meta)| {
                    assert_eq!(data, *input, "{what}");
                    for threads in [1, 3] {
                        let back = fast::reconstruct_with_threads(&data, &meta, threads)?;
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
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Small segments, so that streams are cut in many places (also around stored blocks), give
/// the same reconstruction whatever the number of threads.
#[test]
fn segments_round_trip() {
    let mut lcg = 7u32;
    let mut mixed = include_bytes!("../src/predict.rs").repeat(4);
    mixed.extend((0..150_000).map(|_| {
        lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (lcg >> 24) as u8
    }));
    mixed.extend(include_bytes!("../src/deflate.rs").repeat(4));
    let opts = fast::Options {
        segment: 10_000,
        prefix: 20_000,
        ..Default::default()
    };
    for spec in ["zlib:level=6", "zlib:level=1", "libdeflate:level=9", "stb", "zopfli"] {
        let raw = spec.parse::<flate_zoo::Encoder>().unwrap().compress(&mixed).unwrap();
        let (data, meta) = fast::deconstruct_with(&raw, &opts).unwrap();
        for threads in [1, 2, 7] {
            assert!(
                fast::reconstruct_with_threads(&data, &meta, threads).unwrap() == raw,
                "{spec}, {threads} threads"
            );
        }
    }
}

/// Damaged metadata gives an error or a different stream, never a panic or a hang.
#[test]
fn corrupt_metadata_is_rejected() {
    let input = include_bytes!("../src/predict.rs").repeat(2);
    let raw = "libdeflate:level=6"
        .parse::<flate_zoo::Encoder>()
        .unwrap()
        .compress(&input)
        .unwrap();
    let (data, meta) = fast::deconstruct(&raw).unwrap();
    let mut lcg = 99u32;
    for i in 0..meta.len() * 8 {
        let mut bad = meta.clone();
        bad[i / 8] ^= 1 << (i % 8);
        let _ = fast::reconstruct_with_threads(&data, &bad, 2);
        lcg = lcg.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let mut cut = meta.clone();
        cut.truncate(lcg as usize % meta.len());
        let _ = fast::reconstruct_with_threads(&data, &cut, 1);
    }
}
