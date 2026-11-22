//! An archive of recompressed files decoded twice, by the native modules and by the same
//! modules as WebAssembly (Wasmtime): both fail alike (of the same kind: invalid, unsupported),
//! or both give the same. A resource failure of either is let be (natively, some modules decode
//! whole on threads, not held to their plans; in Wasm, an application over its limits fails).
#![no_main]

use aqr_archive::Failure;
use libfuzzer_sys::{fuzz_mutator, fuzz_target};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| {
    let native = aqr_archive::decode_value(c, &aqr_fuzz::Known);
    if matches!(native, Err(Failure::Resource(_))) {
        return;
    }
    let wasm = aqr_archive::decode_value(c, &aqr_decompress::Wasm);
    match (native, wasm) {
        (Ok(n), Ok(w)) => assert!(n == w, "natively and in Wasm decoded otherwise"),
        (_, Err(Failure::Resource(_))) => {}
        (Err(n), Err(w)) => assert!(
            std::mem::discriminant(&n) == std::mem::discriminant(&w),
            "failing otherwise: natively {n}, in Wasm {w}"
        ),
        (Ok(_), Err(e)) => panic!("decoded natively, not in Wasm: {e}"),
        (Err(e), Ok(_)) => panic!("decoded in Wasm, not natively: {e}"),
    }
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    aqr_fuzz::mutate(data, size, max_size, seed)
});
