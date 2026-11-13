//! Setup headers that are none: they are refused (or taken as they decode), within bounded time
//! and memory.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use aqr_vorbis::bits::Writer;
use aqr_vorbis::packet::Scratch;
use aqr_vorbis::setup::Setup;
use common::{IDENT, setup_bytes_with_residue};

/// A setup header of one codebook, written by `book`, then whatever follows.
fn one_book(book: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    w.bits(5, 8);
    b"vorbis".iter().for_each(|&c| w.bits(c as u32, 8));
    w.bits(0, 8);
    book(&mut w);
    w.bits(0, 32);
    w.finish()
}

/// Parses `d` on a thread, which a hang does not take the test with it.
fn parse(d: Vec<u8>) -> Result<Setup, aqr_vorbis::Error> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || _ = tx.send(Setup::parse(&d, IDENT)));
    rx.recv_timeout(Duration::from_secs(20)).expect("the parse does not end")
}

/// A codebook of lookup type 1 and no dimensions has no values to look up: refused, not looped
/// over.
#[test]
fn lookup_without_dimensions_is_refused() {
    let d = one_book(|w| {
        w.bits(0x564342, 24);
        w.bits(0, 16); // dimensions
        w.bits(1, 24); // entries
        w.bits(0, 1); // not ordered
        w.bits(0, 1); // not sparse
        w.bits(0, 5); // one entry of length 1
        w.bits(1, 4); // lookup type 1
        w.bits(0, 32);
        w.bits(0, 32);
        w.bits(0, 4);
        w.bits(0, 1);
    });
    assert_eq!(parse(d).unwrap_err().0, "codebook dimensions");
}

/// A codebook of 2^24 - 1 entries of 24 bits each takes a few bytes to write and hundreds of
/// megabytes to hold: refused.
#[test]
fn ordered_codebook_of_millions_of_entries_is_refused() {
    let d = one_book(|w| {
        w.bits(0x564342, 24);
        w.bits(1, 16);
        w.bits(0xFF_FFFF, 24);
        w.bits(1, 1); // ordered
        w.bits(23, 5); // from length 24
        w.bits(0xFF_FFFF, 24); // all the entries of that length
        w.bits(0, 4);
    });
    assert_eq!(parse(d).unwrap_err().0, "codebook entries");
}

/// A residue that ends before it begins decodes nothing.
#[test]
fn residue_ending_before_it_begins_has_no_partitions() {
    let s = parse(setup_bytes_with_residue(100, 10)).unwrap();
    let r = std::panic::catch_unwind(|| Scratch::new(&s));
    assert!(r.is_ok(), "the scratch of the setup is not made");
}
