//! Inputs that claim more than they can be: an error, not an allocation that aborts (the
//! allocator refuses what an exhausted heap would).

use aqr_zip::module;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// The output length of an application is reserved up front: a layout of nothing (its header
/// alone) with a length of gigabytes is an error.
#[test]
fn length_beyond_memory() {
    for len in [u64::MAX, u32::MAX as u64, 3 << 30] {
        aqr_testkit::case(&format!("len {len}"));
        assert!(module::decode(&[], &[&[], &[], &[0, 0, 0], &[]], len, 1).is_err(), "{len}");
    }
}
