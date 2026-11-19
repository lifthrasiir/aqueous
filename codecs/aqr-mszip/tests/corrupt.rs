//! Inputs that claim more than they can be: an error, not an allocation that aborts.

use aqr_mszip::module;

/// The output length of an application is reserved up front.
#[test]
fn length_beyond_memory() {
    assert!(module::decode(&[], &[&[], &[], &[], &[]], u64::MAX).is_err());
}

/// An index of blocks over empty streams, of every output length near 0.
#[test]
fn index_over_nothing() {
    let index = module::index(&[module::Block::Mszip { kept: 0, blocks: 1 }]);
    for len in [0, 1, 2, u32::MAX as u64] {
        assert!(module::decode(&[], &[&[], &[], &[], &index], len).is_err());
    }
}
