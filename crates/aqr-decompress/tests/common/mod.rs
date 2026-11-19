//! What the tests of native and WebAssembly decoding alike share (`tests/alike*.rs`).

use std::time::Duration;

use aqr_archive::{Encoder, Level};
use aqr_decompress::{Native, Wasm, wasm_of};
use aqr_formula::Formula;

/// Formula `f` (of an archive's applications) with each of its streams damaged in every way of
/// aqr-testkit's `damages` (at about `per` places each, and every place of its first `head`
/// bytes), each archived with the modules aqr-modules built: natively and as WebAssembly, each
/// decodes to the same bytes or fails in both (each telling its own failure), within `limit`. The cases that do not are told
/// all together.
pub fn fail_alike(name: &str, f: &Formula, per: usize, head: usize, limit: Duration) {
    let enc = Encoder::new(Level::Zstd(1)).with_modules(wasm_of);
    let mut bad = Vec::new();
    for k in 0..f.streams.len() {
        let d = f.streams[k].data.to_vec();
        for x in aqr_testkit::damages(d.len(), per, head) {
            let mut g = f.clone();
            g.streams[k].data = x.apply(&d).into();
            let Ok(c) = enc.encode(&g, None) else { continue };
            let what = format!("{name}: stream {k} ({}), {x}", f.streams[k].class);
            match aqr_testkit::within(limit, || {
                (aqr_archive::decode(&c, &Native::default()), aqr_archive::decode(&c, &Wasm))
            }) {
                Err(e) => bad.push(format!("{what}: {e}")),
                Ok((n, w)) if n.is_ok() != w.is_ok() => {
                    bad.push(format!("{what}: {:?} natively, {:?} as Wasm", n.err(), w.err()))
                }
                // (failures in both: alike, whatever each tells)
                Ok((Ok(n), Ok(w))) if n != w => bad.push(format!("{what}: other bytes natively and as Wasm")),
                Ok(_) => {}
            }
        }
    }
    assert!(bad.is_empty(), "{} cases not alike:\n{}", bad.len(), bad.join("\n"));
}
