//! The memory plan of the module `vorbis-recons` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode`] lays it out: the instance's memory taken block by block in the order
//! the module takes it ([`Sim`]): the copies of the inputs, the setup (codebook by codebook), the
//! keys, the header's lists and the tables, the decoder, the contexts, the walk's room and the
//! output. What it takes follows from the setup header and the record's header, which it parses.

use aqr_api::stack::{Sim, block};
use aqr_common::rans64::{SRC_PAD, Tables};

use crate::codec::{self, parsed_sizes};
use crate::model::{Contexts, Keys};
use crate::module::{ident, wasm};
use crate::packet::Scratch;
use crate::setup::{Floor, Mapping, Mode, Residue, Setup};

/// The size a vector grown by pushes from 4 elements reaches holding `n`: a power of two.
fn grown(n: u64) -> u64 {
    n.next_power_of_two().max(4)
}

/// The memory plan of `vorbis-recons`: of its initial memory (`initial` bytes), parameters
/// `params`, inputs of these lengths (the setup header `setup` and the record `record`), and an
/// output of `len` bytes.
pub fn decode(initial: u64, params: &[u8], inputs: &[u64], len: u64, setup: &[u8], record: &[u8]) -> Option<u64> {
    if inputs.len() != 2 {
        return None;
    }
    let mut m = Sim::new(initial);
    m.take(aqr_api::stack::whole(params.len() as u64, &[], inputs));
    m.take(len);
    let s = Setup::parse_cached(setup, ident(params).ok()?).ok()?;
    // `Setup::parse`: the lists, and the vectors of each codebook (its tree grown to a power of
    // two of nodes, then cut to its size)
    m.take(wasm::CODEBOOK * s.codebooks.len() as u64);
    for b in &s.codebooks {
        let (e, nodes) = (b.entries() as u64, b.nodes() as u64);
        m.take(e);
        m.take(4 * e);
        m.take(4 << 10);
        m.peak(block(8 * grown(nodes)));
        m.take(8 * nodes);
    }
    m.take((size_of::<Floor>() * s.floors.len()) as u64);
    m.take((size_of::<Residue>() * s.residues.len()) as u64);
    m.take((size_of::<Mapping>() * s.mappings.len()) as u64);
    m.take((size_of::<Mode>() * s.modes.len()) as u64);
    // `codec::decode`: the keys, ...
    let mut keys = Keys::of(&s, false);
    let (nb, nk) = (s.codebooks.len() as u64, keys.len() as u64);
    m.take(4 * (nb + 1));
    m.take(4 * keys.entries() as u64);
    m.take(4 * nb);
    m.take(4 * nk);
    m.take(2 * nk);
    // ... the header's lists and the tables (the default ones, then the own), their offsets and
    // the decoder's
    let len = usize::try_from(len).ok()?;
    let mut sizes = Vec::new();
    keys.table_sizes(|a| sizes.push(a as u16));
    let (tables, symbols) = (sizes.len(), sizes.iter().map(|&a| a as usize).sum());
    let mut own = Vec::new();
    let c = codec::counts(record, &mut keys, len, &mut |a| own.push(a as u16)).ok()?;
    let [odd, _, _, tail, n, freqs, _] = parsed_sizes(&c, tables, symbols);
    for k in [4 * odd, 4 * odd, 4 * odd, tail, 2 * n, 2 * freqs, 4 * n] {
        m.take(k as u64);
    }
    // (`normalize` makes a vector of the table's alphabet, `usize`s, for a while)
    m.peak(block(4 * 256));
    m.take(4 * n as u64);
    sizes.extend(own);
    m.take(Tables::dec_plan(&sizes, block));
    let words = codec::split(record).ok()?.1.len() as u64;
    m.take(4 * ((words / 4).max(16) + SRC_PAD as u64));
    // the contexts and the walk's room
    let [at, mags, last0, last1, before] = Contexts::sizes(&s);
    for k in [4 * at, mags, 2 * last0, 2 * last1, before] {
        m.take(k as u64);
    }
    for k in Scratch::sizes(&s) {
        m.take(k as u64);
    }
    Some(m.plan())
}
