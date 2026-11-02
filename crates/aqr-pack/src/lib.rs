//! Archives of other formats than Aqueous written from a [`Tree`] as `aqr -c` gathers it, the
//! counterpart of aqr-unpack: ZIP ([`zip`]) so far.
//!
//! What a format cannot keep of the tree (special files, extended attributes, forks) is left
//! out and told ([`Written::notes`]); the rest of the metadata goes where the format has room
//! for it.

use std::io;

pub use aqr_formula::Bytes;
pub use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree};

mod time;
pub mod zip;

/// The contents of the files of a [`Tree`] ([`EntryNode::File`]), read as they are written.
pub trait Contents: Sync {
    /// The bytes of content `c`, as gathered (read in whole by [`Contents::read`]).
    fn size(&self, c: usize) -> u64;
    fn read(&self, c: usize) -> io::Result<Bytes>;
}

/// How far the writing is: content `c` begun and done, and the bytes of the contents done of
/// all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Started(usize),
    Ended(usize),
    Done { done: u64, total: u64 },
}

/// An archive written: the bytes read of the files and written, and what of the tree it does
/// not keep.
#[derive(Debug, Clone, Default)]
pub struct Written {
    pub read: u64,
    pub len: u64,
    pub notes: Vec<String>,
}

/// The path of entry `i` of `t`, `/` between the names (as their bytes are), and whether every
/// name was UTF-8 ([`aqr_formula::aqr::name_bytes`]).
pub(crate) fn path_of(t: &Tree, i: usize) -> (Vec<u8>, bool) {
    let mut names = Vec::new();
    let mut at = Some(i);
    while let Some(k) = at {
        names.push(&t.entries[k].name[..]);
        at = t.entries[k].parent;
    }
    let (mut path, mut utf8) = (Vec::new(), true);
    for (n, name) in names.iter().rev().enumerate() {
        let (b, raw) = aqr_formula::aqr::name_bytes(name);
        utf8 &= !raw && std::str::from_utf8(b).is_ok();
        if n > 0 {
            path.push(b'/');
        }
        path.extend_from_slice(b);
    }
    (path, utf8)
}
