//! An archive opened and decoded: its footer, segments, metadata, formula blocks and their
//! evaluation (by toy modules), the data blocks decompressed.
#![no_main]

use libfuzzer_sys::{fuzz_mutator, fuzz_target};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| {
    let runner = aqr_fuzz::toys();
    if let Ok(a) = aqr_archive::open(c, &runner) {
        let _ = a.archival();
        let _ = a.value(1);
    }
    let _ = aqr_archive::inspect::inspect(c, false, &|_| None, Some(64));
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    aqr_fuzz::mutate(data, size, max_size, seed)
});
