//! aqr-vorbis as a module of entry formulas (see aqr-formula): a stream's packets, each after its
//! 16-bit length, rebuilt from its setup header and the record of their values (FORMAT.md).

use aqr_formula::{Formula, Module, Node, Usage};

use crate::setup::{Ident, Setup};
use crate::{codec, module};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("vorbis-recons", 3).with_plan(plan);

/// The memory of the module: its plan ([`crate::plan::decode`]).
fn plan(u: &Usage<'_>) -> Option<u64> {
    let [setup, record] = u.all_contents::<2>()?;
    crate::plan::decode(u.initial(), u.params, u.inputs, u.output(), setup, record)
}

/// The node rebuilding the packets at the start of `data` (each after its 16-bit length, up to
/// the end of `data` or a length of 0) of a stream of `ident` and setup header `setup`, and the
/// bytes it takes; None where the setup or a packet does not parse, or there are no packets.
pub fn packets(f: &mut Formula, setup: &[u8], ident: Ident, data: &[u8]) -> Option<(Node, usize)> {
    packets_parsed(f, setup, &Setup::parse(setup, ident).ok()?, data)
}

/// As [`packets`], with the setup header `setup` parsed already (as `s`).
pub fn packets_parsed(f: &mut Formula, setup: &[u8], s: &Setup, data: &[u8]) -> Option<(Node, usize)> {
    let ident = s.ident;
    let (used, record) = codec::encode(s, data)?;
    if used == 0 {
        return None;
    }
    let inputs = vec![f.push("vorbis/setup", setup.to_vec()), f.push("vorbis/record", record)];
    Some((Node::apply(MODULE, module::params(ident), inputs, used as u64), used))
}
