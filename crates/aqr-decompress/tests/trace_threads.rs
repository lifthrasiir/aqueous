//! `--stats`' trace sees the threads a part module runs on natively (`aqr-part`, and the tasks
//! of a gang): their work is attributed to the item the decoding is for, as the pool's workers'
//! is.

#![cfg(feature = "wasm")]

use aqr_common::trace;
use aqr_decompress::{Native, wasm_of};
use aqr_formula::aqr::{self, Encoder, Level};

/// Synthetic ProRes frames of 720x480.
fn prores_big(frames: usize) -> Vec<u8> {
    let mut d = b"\0\0\0\x08wide".to_vec();
    for i in 0..frames {
        d.extend_from_slice(&aqr_prores::synth::synthetic(720, 480, 2, 0, i as u64));
    }
    d
}

#[test]
fn gang_threads_work_for_the_item() {
    let enc = Encoder::new(Level::Zstd(3)).with_modules(wasm_of);
    let d = prores_big(12);
    let f = aqr_prores::formula::encode_with(&d, 2, aqr_prores::formula::CHUNK, Some(16)).unwrap();
    let c = enc.encode(&f, None).unwrap();
    trace::enable();
    let rec = trace::record().unwrap();
    {
        let _item = trace::item(Some(0));
        let _span = trace::span("test");
        let start = std::time::Instant::now();
        // (until the sampler has seen them at work)
        while start.elapsed().as_secs() < 20 {
            let native = Native::default();
            let mut a = aqr::open(&c, &native).unwrap();
            a.set_items(vec![Some(0); a.leaves_len()]);
            assert!(matches!(a.value(1).unwrap(), aqr::Value::File(v) if v == d));
            let t = rec.watch().trace();
            if attributed(&t) >= 2 {
                break;
            }
        }
    }
    let t = rec.finish();
    let most = attributed(&t);
    assert!(most >= 2, "threads outside the pool at work for the item at once: {most}");
}

/// The most threads outside the pool at once in a sample, at work (not waiting) for item 0.
fn attributed(t: &trace::Trace) -> usize {
    t.samples
        .iter()
        .map(|s| {
            s.threads
                .iter()
                .enumerate()
                .filter(|&(k, &(stack, item))| {
                    let busy = t.stacks.get(stack as usize).is_some_and(|st| {
                        !st.is_empty() && !st.iter().any(|&l| t.labels.get(l as usize).is_some_and(|l| l.1))
                    });
                    !t.threads[k].1 && t.threads[k].0 != "gang_threads_work_for_the_item" && item == 0 && busy
                })
                .count()
        })
        .max()
        .unwrap_or(0)
}
