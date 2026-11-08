//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds them into their Wasm modules. Both are part modules: the inputs taken in
//! order in pieces and the output put in order, through windows.

use aqr_api::stream::{In, Pull, Put, Slice, Written};
use aqr_common::varint;

use crate::layout::{self, Entries, PIECE, SMALL_PIECE};
use crate::pack::ID;
use crate::{Result, corrupt, index};

/// Pages of the windows of the modules as part modules ([`decode_part`], [`decode_index_part`]):
/// of the streams and the bytes kept, and of the output; and of the others.
pub const WINDOW: u32 = 16;
pub const SMALL_WINDOW: u32 = 4;

/// The memory plan of `git-pack-recons` as a part module ([`aqr_api::stack`]): what
/// [`decode_part`] takes beyond its initial memory (`initial` bytes) at most, whatever the
/// lengths: a piece of each of the inputs.
pub fn plan(initial: u64) -> u64 {
    let mut m = aqr_api::stack::Sim::new(initial);
    pieces(&mut m);
    m.plan()
}

/// The pieces of [`layout::build_stream`].
fn pieces(m: &mut aqr_api::stack::Sim) {
    m.take(PIECE as u64);
    m.take(SMALL_PIECE as u64);
    m.take(SMALL_PIECE as u64);
    m.take(PIECE as u64);
}

/// The memory plan of `git-pack-index-recons` as a part module, as [`plan`]'s, of ids of
/// `ids` bytes (none where they are not of whole entries): the CRC-32's tables (on WebAssembly),
/// the pieces, the entries' offsets and CRC-32s (12 bytes each, and 8 more), then ids, the
/// entries in the index's order and the output's piece ([`index::plan`]). Proportional to the
/// count of entries only, not to the lengths of the pack.
pub fn index_plan(initial: u64, ids: u64) -> Option<u64> {
    if !ids.is_multiple_of(ID as u64) {
        return None;
    }
    let n = ids / ID as u64;
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(aqr_common::crc32::WASM_TABLES);
    pieces(&mut m);
    m.take(8 * (n + 1));
    m.take(4 * n);
    index::plan(&mut m, n);
    Some(m.plan())
}

/// The flags and the pack's length of [`decode_index`]'s params.
fn index_params(params: &[u8]) -> Result<(u8, u64)> {
    let &[flags, ..] = params else { return Err(corrupt()) };
    let mut at = 1;
    let pack_len = varint::get(params, &mut at)?;
    if flags & !WITH_REV != 0 || at != params.len() {
        return Err(corrupt());
    }
    Ok((flags, pack_len))
}

/// The module: the pack of `len` bytes rebuilt from `inputs`: the rebuilt entries' DEFLATE
/// streams, what aqr-flate's module derives of them (apart, in the same order), the layout and
/// the bytes kept.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[streams, table, layout, kept]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, streams, table, kept, len)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]), in pieces:
/// nothing is held, whatever the lengths.
pub fn decode_part(params_len: u64, streams_len: u64, table_len: u64, layout_len: u64, kept_len: u64, size: u64) {
    aqr_api::finish(part(params_len, [streams_len, table_len, layout_len, kept_len], size));
}

fn part(params_len: u64, lens: [u64; 4], size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let (mut a, mut b, mut c, mut d) =
        (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]), In::<3>::new(lens[3]));
    let mut out = Written::new(size);
    layout::build_stream([&mut a, &mut b, &mut c, &mut d], lens, size, &mut out, None, u64::MAX)?;
    Ok(())
}

/// Bit of the flags of [`decode_index`]'s params: a reverse index follows the index.
pub const WITH_REV: u8 = 1;

/// The module of a pack with its index: the pack of `pack_len` bytes as [`decode`] makes it of
/// the first four `inputs`, then its index and, where the flags tell, its reverse index, of
/// the ids of its entries in order (the fifth input), `len` bytes in all. Params: the flags,
/// then `pack_len` (a varint).
pub fn decode_index(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let &[streams, table, layout, kept, ids] = inputs else { return Err(corrupt()) };
    let (flags, pack_len) = index_params(params)?;
    let lens = [streams.len(), table.len(), layout.len(), kept.len(), ids.len()].map(|l| l as u64);
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| corrupt())?);
    let (mut a, mut b, mut c, mut d, mut e) = (Slice(streams), Slice(table), Slice(layout), Slice(kept), Slice(ids));
    let total =
        index_stream([&mut a, &mut b, &mut c, &mut d, &mut e], lens, pack_len, flags & WITH_REV != 0, &mut out)?;
    match total == len {
        true => Ok(out),
        false => Err(corrupt()),
    }
}

/// The module as a part module, as [`decode_part`]; the params (up to 11 bytes) are read here.
pub fn decode_index_part(
    params_len: u64,
    streams_len: u64,
    table_len: u64,
    layout_len: u64,
    kept_len: u64,
    ids_len: u64,
    size: u64,
) {
    aqr_api::finish(index_part(params_len, [streams_len, table_len, layout_len, kept_len, ids_len], size));
}

fn index_part(params_len: u64, lens: [u64; 5], size: u64) -> Result<()> {
    use aqr_api::Memory;
    let mut p = [0; 11];
    let params = p.get_mut(..usize::try_from(params_len).map_err(|_| corrupt())?).ok_or_else(corrupt)?;
    aqr_api::Params::read(0, params);
    let (flags, pack_len) = index_params(params)?;
    let (mut a, mut b, mut c, mut d, mut e) = (
        In::<0>::new(lens[0]),
        In::<1>::new(lens[1]),
        In::<2>::new(lens[2]),
        In::<3>::new(lens[3]),
        In::<4>::new(lens[4]),
    );
    let mut out = Written::new(size);
    let total =
        index_stream([&mut a, &mut b, &mut c, &mut d, &mut e], lens, pack_len, flags & WITH_REV != 0, &mut out)?;
    match total == size {
        true => Ok(()),
        false => Err(corrupt()),
    }
}

/// The pack of `pack_len` bytes and its indexes put to `out` in order, taking the five inputs
/// (see [`decode_index`]) of `lens` bytes each; the bytes put.
pub fn index_stream(
    src: [&mut dyn Pull; 5],
    lens: [u64; 5],
    pack_len: u64,
    rev: bool,
    out: &mut dyn Put,
) -> Result<u64> {
    // (the CRC-32's tables, made on the first use, are the first block of the stack)
    aqr_common::crc32::Hasher::new().update(&[]);
    let [streams, table, layout, kept, ids] = src;
    let mut entries = Entries::default();
    let max = lens[4] / ID as u64;
    let trailer = layout::build_stream(
        [streams, table, layout, kept],
        [lens[0], lens[1], lens[2], lens[3]],
        pack_len,
        out,
        Some(&mut entries),
        max,
    )?
    .ok_or_else(corrupt)?;
    if entries.crcs.len() as u64 != max || !lens[4].is_multiple_of(ID as u64) {
        return Err(corrupt());
    }
    Ok(pack_len + index::write(ids, &entries, &trailer, rev, out)?)
}
