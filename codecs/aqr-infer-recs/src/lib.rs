//! Records inferred in files whose format nothing knows (a header, entries, a footer), and
//! their fields transposed for a general-purpose coder.
//!
//! Two kinds of regions are found: tables of fixed-size records ([`fixed`]) and chains of
//! entries each holding its own length ([`chain`]). The records (or the entries' heads) are
//! split into fields ([`fields`]), each coded as it is, against the record before or against
//! an earlier field, and written as byte planes, field by field; the entries' tails follow.
//! Floats of the records that are whole multiples of a scale are lifted to those whole numbers
//! first ([`floats`]).
//! A region is taken only where zstd -3 gets the file smaller ([`choose`]), and the inference is
//! run only where a quick estimate tells a large saving ([`probe`]). It is the port of
//! `scripts/rec_probe.py`, the prototype. `formula` is the entry point (the modules
//! `recs-fields`, `recs-entries` and `recs-floats` of aqr-formula, whose code is `module`);
//! `FORMAT.md` specifies them.

#![cfg_attr(not(feature = "encode"), allow(unused))]

#[cfg(feature = "encode")]
pub mod chain;
#[cfg(feature = "encode")]
pub mod fields;
#[cfg(feature = "encode")]
pub mod fixed;
#[cfg(feature = "encode")]
pub mod floats;
#[cfg(feature = "encode")]
pub mod formula;
#[cfg(feature = "encode")]
pub mod kt;
pub mod module;
#[cfg(feature = "encode")]
pub mod probe;
#[cfg(feature = "encode")]
mod regions;
#[cfg(feature = "encode")]
mod zcost;

#[cfg(feature = "encode")]
pub use regions::*;

/// The fewest records a region holds.
pub const MIN_RECORDS: usize = 16;

/// `count` points over [0, last], rounded down, without repeats: numpy's `linspace`.
pub(crate) fn linspace(last: usize, count: usize) -> Vec<usize> {
    let mut v: Vec<usize> = if count <= 1 {
        vec![0]
    } else {
        (0..count).map(|i| (last as f64 * i as f64 / (count - 1) as f64) as usize).collect()
    };
    v.dedup();
    v
}
