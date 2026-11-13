//! Inputs that claim more than they can be: an error, not an allocation that aborts.

use aqr_lzma::lzma::{self, Props};
use aqr_lzma::module;

const NONE: &[&[u8]] = &[&[], &[], &[]];

/// The output length of an application is reserved up front.
#[test]
fn length_beyond_memory() {
    assert!(module::decode(&[], NONE, u64::MAX, 1).is_err());
    assert!(module::decode2(&[], NONE, u64::MAX, 1).is_err());
}

/// An `.lzma` header's size is reserved up front, up to the limit.
#[test]
fn size_beyond_memory() {
    let props = Props { lc: 3, lp: 0, pb: 2 };
    assert!(lzma::decode(&[0; 8], props, Some(u64::MAX), usize::MAX).is_err());
}
