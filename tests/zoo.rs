//! Round trips over real encoders from flate-zoo.

use flate_recons::{Priors, coder::Recorder, deconstruct, model, reconstruct};
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
    let priors = Priors::builtin();
    let mut failures = vec![];
    for &b in backends() {
        for enc in presets_of(b, Sweep::Default) {
            for input in &inputs {
                let Ok(raw) = enc.compress(input) else {
                    continue;
                };
                let what = format!("{enc} on {} bytes", input.len());
                let r = deconstruct(&raw).and_then(|(data, meta)| {
                    assert_eq!(data, *input, "{what}");
                    assert_eq!(reconstruct(&data, &meta)?, raw, "{what}");
                    let packed = model::pack(&meta, priors);
                    let mut cost = model::CostCoder::new(priors);
                    for ch in &meta.channels {
                        for d in ch {
                            flate_recons::Coder::bit(&mut cost, d.ctx, d.bit);
                        }
                    }
                    // Arithmetic coding overhead: a header plus a few bytes per channel.
                    let est = cost.total() / 8.0;
                    assert!(
                        (packed.len() as f64) < est * 1.01 + 64.0,
                        "{what}: {} vs {est}",
                        packed.len()
                    );
                    assert_eq!(model::unpack(&data, &packed, priors)?, raw, "{what}");
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

/// zlib's Huffman trees are predicted exactly, so its headers cost one decision each.
#[test]
fn zlib_headers_are_predicted() {
    let text = include_bytes!("../src/predict.rs").repeat(3);
    for level in 1..=9 {
        let enc = flate_zoo::Encoder::new(flate_zoo::backend("zlib").unwrap())
            .set("level", level)
            .unwrap();
        let raw = enc.compress(&text).unwrap();
        let parsed = flate_recons::Parsed::new(&raw).unwrap();
        let mut rec = Recorder::default();
        parsed
            .code(&mut rec, "depth=128,nice=128".parse().unwrap())
            .unwrap();
        let tree = &rec.meta.channels[flate_recons::ctx::Channel::Tree as usize];
        let wrong: Vec<_> = tree.iter().filter(|d| !d.bit).collect();
        assert!(wrong.is_empty(), "level {level}: {wrong:?}");
    }
}
