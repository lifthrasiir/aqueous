//! Inputs that claim more than they can be: an error, not an allocation that aborts.

use aqr_lzx::module;

/// The output length of an application is reserved up front.
#[test]
fn length_beyond_memory() {
    assert!(module::decode(&[], &[&[], &[], &[], &[]], u64::MAX, 1).is_err());
}
