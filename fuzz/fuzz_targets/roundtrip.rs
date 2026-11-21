//! A formula made of the input (`src/formula.rs`) archived by the encoder and decoded again:
//! where the reference evaluator gives its output, the encoder takes it and the archive gives it
//! back, whole and streamed; where it does not, whatever the encoder makes decodes or fails,
//! without a crash.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|d: &[u8]| aqr_fuzz::formula::roundtrip(d));
