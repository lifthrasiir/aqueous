//! Native and WebAssembly decoding alike on damaged inputs (common::fail_alike): of each module's
//! applications, every stream damaged in turn decodes to the same bytes or fails in both.

#![cfg(feature = "wasm")]

mod common;

use std::time::Duration;

use aqr_formula::Formula;

/// bcj-delta, whose only input is the data: the helper's own check.
#[test]
fn delta_alike() {
    let data: Vec<u8> = (0..5000u32).map(|i| ((i * 7) >> 3) as u8).collect();
    let mut f = Formula::default();
    let d = f.push("raw", data.clone());
    f.root = aqr_bcj::formula::Filter::Delta(2).apply(d, data.len() as u64);
    common::fail_alike("delta", &f, 8, 4, Duration::from_secs(30));
}
