//! A syntax walked once for both directions. A recompressor of a bit-packed format (aqr-div3, aqr-ac3) writes the
//! format's syntax as one function over [`io::Io`]: compressing, an element is read from the original bits and coded
//! ([`io::Enc`]); decompressing, it is decoded and its bits written ([`io::Dec`]), so that the data comes back bit for
//! bit; [`io::Check`] only reads, to see that the data parses before anything is coded. The elements are coded by an
//! adaptive binary range coder ([`rc`]) under contexts hashed into a table of probabilities ([`model`]).
#![no_std]
extern crate alloc;

pub mod bits;
pub mod chunks;
pub mod io;
pub mod model;
pub mod rc;
pub mod reader;

/// The data is not what the syntax walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Parse,
}
