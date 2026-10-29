//! aqr-gnu-gettext as a module of entry formulas (see aqr-formula): the file rebuilt from the
//! file without its tables and the NULs within its strings (FORMAT.md).

use aqr_common::varint;
use aqr_formula::{Formula, Module, Node};

use crate::mo;

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "gnu-gettext-recons", version: 1 };

/// Whether `d` starts like a `.mo` file: the magic, and a header of tables in order.
pub fn starts(d: &[u8]) -> bool {
    mo::Layout::of(d).is_some()
}

/// The bytes of the `.mo` file at the start of `d`: to the end of its tables or of its last
/// string, whichever is later (clamped to `d`).
pub fn extent(d: &[u8]) -> Option<usize> {
    let l = mo::Layout::of(d)?;
    let strings = [l.originals, l.translations].into_iter().flat_map(|t| {
        (0..l.n).map_while(move |i| {
            let e = d.get(t + 8 * i..t + 8 * i + 8)?;
            Some(mo::word(e, l.big) as usize + mo::word(&e[4..], l.big) as usize + 1)
        })
    });
    Some(strings.fold(l.end(), usize::max).min(d.len()))
}

/// The formula of `.mo` file `input`, or `None` where it is not one this crate takes
/// ([`mo::split`]). The same whatever `threads` is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let (rest, start, nuls) = mo::split(input)?;
    let mut params = Vec::new();
    varint::put(&mut params, start as u64);
    let mut f = Formula::default();
    let inputs = vec![f.push("gnu-gettext/rest", rest), f.push("gnu-gettext/nuls", nuls)];
    f.root = Node::apply(MODULE, params, inputs, input.len() as u64);
    Some(f)
}
