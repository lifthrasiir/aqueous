//! A WebAssembly module checked against the profile and run by aqr-wasmrt, as the decoder runs
//! one it does not know (`src/modules.rs`): of the input's bytes, or made by wasm-smith.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| {
    aqr_fuzz::modules::run(d);
});
