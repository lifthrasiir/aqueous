//! A tree written by aqr-pack as an archive of another format and read back by aqr-unpack
//! (`src/pack.rs`): its files as they were.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| aqr_fuzz::pack::pack(d));
