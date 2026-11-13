//! Inputs that claim more than they can be: an error, not an allocation that aborts.

use aqr_mszip::module;

/// The output length of an application is reserved up front.
#[test]
fn length_beyond_memory() {
    assert!(module::decode(&[], &[&[], &[], &[], &[]], u64::MAX).is_err());
}
