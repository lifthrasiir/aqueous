//! An archive opened and decoded: its footer, segments, metadata, formula blocks and their
//! evaluation (by toy modules), the data blocks decompressed; and where it decodes, read
//! otherwise (`src/decode.rs`), alike.
#![no_main]

use libfuzzer_sys::{fuzz_mutator, fuzz_target};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| aqr_fuzz::decode::decode(c));

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    aqr_fuzz::mutate(data, size, max_size, seed)
});
