//! The memory a worker is let take: an application's inputs and outputs held beside its memory.
//! (A test of its own, as it sets the worker's overhead for the whole process.)

#![cfg(all(feature = "sandbox", feature = "wasm"))]

use aqr_decompress::sandbox::Sandbox;
use aqr_formula::aqr::Runner;

/// A part module's application whose inputs and outputs are far beyond its memory (so that they
/// are in the messages, not an arena) holds both in the worker: the outputs too are let in, not
/// only the inputs sent (a DIV3 AVI of 1.5 GB of a release's div3-recons ended its worker).
#[test]
fn outputs_let_in() {
    aqr_common::zopt::apply(&["sandbox:overhead=64M"]).unwrap();
    let d: Vec<u8> = (0..96u32 << 20).map(|i| (i ^ i >> 11) as u8).collect();
    let mut want = d.clone();
    aqr_bcj::delta::encode(&mut want, 4);
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../aqr-modules/wasm/bcj-delta.wasm")).unwrap();
    let s = Sandbox::new(env!("CARGO_BIN_EXE_aqr-decompress-worker"), [""; 0]);
    let l = s.load(&wasm).unwrap();
    // (the module's memory of its windows: a few pages)
    let got = l.apply(&[3], &[], &[&d], &[want.len() as u64], 4 << 20, &[]);
    assert!(got.as_ref().is_ok_and(|g| g.len() == 1 && g[0] == want), "{:?}", got.err());
}
