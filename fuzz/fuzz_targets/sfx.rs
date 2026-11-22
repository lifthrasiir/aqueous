//! What a self-extracting program reads of what follows its image (`src/sfx.rs`): its bundle of
//! blobs and the record of its settings.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| aqr_fuzz::sfx::sfx(d));
