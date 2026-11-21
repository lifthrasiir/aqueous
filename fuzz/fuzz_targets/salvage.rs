//! A damaged archive checked for its mirror blocks, repaired, and opened salvaging.
#![no_main]

use aqr_archive::mirror;
use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| {
    let runner = aqr_fuzz::toys();
    let check = mirror::check(c);
    let repaired = mirror::repair(c, &check, true, &[]).ok().flatten();
    for a in [Some(c), repaired.as_ref().map(|r| &r.bytes[..])].into_iter().flatten() {
        if let Ok(a) = aqr_archive::open_salvaging(a, &runner, false) {
            let _ = a.value(1);
        }
    }
});
