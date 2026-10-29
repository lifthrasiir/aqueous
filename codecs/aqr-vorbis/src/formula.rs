//! aqr-vorbis as a module of entry formulas (see aqr-formula): a stream's packets, each after its
//! 16-bit length, rebuilt from its setup header and the record of their values (FORMAT.md).

use aqr_common::rans32::BitR;
use aqr_common::varint;
use aqr_formula::{Formula, Module, Node, PAGE, Usage};

use crate::model::Keys;
use crate::setup::{Ident, Setup};
use crate::{codec, module};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("vorbis-recons", 2).with_memory(memory);

/// The capacity of a vector grown by pushes from a few elements: a power of two, 4 at least.
fn grown(n: u64) -> u64 {
    n.next_power_of_two().max(4)
}

/// What the record's header (`codec::decode`) holds that takes memory: the count of the packets
/// that are not as most are and their tails' bytes, and the alphabets of the keys with tables of
/// their own.
fn record_header(record: &[u8], keys: &Keys, len: u64) -> Option<(u64, u64, Vec<usize>)> {
    let mut at = 0;
    let hlen = varint::get(record, &mut at).ok()? as usize;
    let mut r = BitR::<false>::new(record.get(at..at.checked_add(hlen)?)?);
    let gamma = |r: &mut BitR<false>| -> Option<u64> {
        let g = r.gamma().ok()? as u64;
        (!r.overrun()).then_some(g)
    };
    gamma(&mut r)?;
    let (odd, mut tails) = (gamma(&mut r)?.checked_sub(1)?, 0u64);
    for _ in 0..odd {
        gamma(&mut r)?;
        if r.bits(1) == 1 {
            gamma(&mut r)?;
        }
        let n = gamma(&mut r)?.checked_sub(1)?;
        if n > 8 * len {
            return None;
        }
        (0..n).for_each(|_| _ = r.bits(1));
        if r.overrun() {
            return None;
        }
        tails += n;
    }
    let mut own = Vec::new();
    let mut next = 0usize;
    for _ in 0..gamma(&mut r)?.checked_sub(1)? {
        let key = next + gamma(&mut r)?.checked_sub(1)? as usize;
        let alphabet = *keys.alphabet.get(key)? as usize;
        let mut got = 0;
        while got < alphabet {
            let g = gamma(&mut r)? as usize;
            got += if g == 0 { gamma(&mut r)? as usize + 1 } else { 1 };
        }
        own.push(alphabet);
        next = key + 1;
    }
    Some((odd, tails, own))
}

/// The memory of the module (wasm32 sizes), from the setup header and the record's header:
///
/// - Always alive: the setup (per codebook its lengths, codewords and packed codewords of 4 bytes
///   an entry, the decoding tree of a power of two of nodes of 8 bytes, the fast table of 4 KiB,
///   the multiplicands in a power of two of 4 bytes; the books; the small floors, residues,
///   mappings); the keys (per book its ranks, `default` and `alphabet` at twice the keys at
///   most, the default weights); the default tables as u16 and `map`; the odd packets (24 bytes
///   each, and their tails).
/// - Building the tables (`codec::tables`): the owned levels, `all` (the defaults copied and
///   the owned tables pushed, twice its length at most), the `Tables`, and the hash map of
///   `Tables::new` (13 bytes a bucket, half again as it grows), and the parse's and the keys'
///   growing vectors.
/// - Decoding: the tables, the decoder's alias tables (a power of two of u64 per table, a
///   power of two of those as pushed with half again as it grows, and the flat copy where all
///   have one size), the decoder's words, the contexts (the books' magnitudes; the floor and
///   classword slots of u16 per block size, up to those of the greatest submap and vector: the
///   one finished at twice its length at most and the other as it grows, thrice), and a
///   packet's writer (half again as it grows, a u16 size at most) and class rows.
fn memory(u: &Usage<'_>) -> Option<u64> {
    let [setup, record] = u.all_contents::<2>()?;
    let mut at = 0;
    let channels = varint::get(u.params, &mut at).ok()?;
    let &[sizes] = &u.params[at..] else { return None };
    let ident = Ident { channels: usize::try_from(channels).ok()?, blocksizes: [1 << (sizes & 15), 1 << (sizes >> 4)] };
    let s = Setup::parse(setup, ident).ok()?;
    let keys = Keys::of(&s);
    let len = u.output();
    let (odd, tails, own) = record_header(record, &keys, len)?;
    let ch = channels;

    // (bytes, large allocations) of each part
    let add = |m: &mut (u64, u64), n: u64| {
        m.0 += n;
        m.1 += (n >= PAGE) as u64;
    };
    let mut live = (0u64, 0u64);
    add(&mut live, 96 * grown(s.codebooks.len() as u64));
    let (mut parse_extra, mut longest, mut mags) = (0u64, 0u64, 0u64);
    for b in &s.codebooks {
        let (e, nodes) = (b.entries() as u64, b.nodes() as u64);
        let tree = if nodes <= 1 { 1 } else { grown(nodes) };
        let mult = b.lookup.as_ref().map_or(0, |l| grown(l.multiplicands.len() as u64));
        for n in [e, 4 * e, 4 * e, 8 * tree, 4096, 4 * mult] {
            add(&mut live, n);
        }
        parse_extra = parse_extra.max(2 * mult + 4 * tree + tree);
        longest = longest.max(e);
        mags += e;
    }
    let mut small = 0;
    for f in &s.floors {
        small += 64 + 8 * grown(f.partitions.len() as u64) + 512;
        small += f.classes.iter().map(|c| 64 + 8 * c.subbooks.len() as u64).sum::<u64>();
    }
    small += s.residues.iter().map(|r| 96 + 64 * r.books.len() as u64).sum::<u64>();
    small += s.mappings.iter().map(|m| 96 + 8 * (m.coupling.len() + m.mux.len() + m.submaps.len()) as u64).sum::<u64>();
    small += 8 * grown(s.modes.len() as u64);
    live.0 += small;

    // the keys
    let (nb, keys_len) = (s.codebooks.len() as u64, keys.len() as u64);
    add(&mut live, 24 * nb + 16 * grown(nb) + 4 * grown(nb) + 36 * 8 + 4 * nb);
    for r in &keys.ranks {
        add(&mut live, 4 * r.rank.len() as u64);
        add(&mut live, 4 * grown(r.entry.len() as u64));
    }
    add(&mut live, 12 * keys_len);
    let nd = keys.defaults.len() as u64;
    let default_weights: u64 = keys.defaults.iter().map(|d| d.len() as u64).sum();
    add(&mut live, 8 * default_weights + 24 * grown(nd));
    // the default tables, map, the odd packets
    add(&mut live, 2 * default_weights + 24 * nd);
    add(&mut live, 4 * keys_len);
    add(&mut live, 24 * grown(odd) * 3 / 2 + tails + 16 * odd);

    // building the tables
    let tables_n = nd + own.len() as u64;
    let own_alphabets: u64 = own.iter().map(|&a| a as u64).sum();
    let mut build = live;
    add(&mut build, 4 * own_alphabets + 16 * grown(own.len() as u64));
    add(&mut build, 24 * 3 * tables_n + 2 * (default_weights + own_alphabets));
    let buckets = grown(tables_n * 8 / 7 + 1);
    add(&mut build, 3 * (13 * buckets + 16) / 2);
    let tables = 10 * tables_n + 2 * (default_weights + own_alphabets);
    add(&mut build, tables);
    build.0 += parse_extra + 6 * longest;

    // decoding
    let mut run = live;
    add(&mut run, tables);
    let widths: Vec<u64> =
        keys.defaults.iter().map(|d| d.len()).chain(own.iter().copied()).map(|n| grown(n as u64)).collect();
    let alias = widths.iter().sum::<u64>();
    let cap = grown(alias);
    let flat = if widths.iter().all(|&w| w == widths[0]) { 8 * alias } else { 0 };
    add(&mut run, 4 * tables_n);
    add(&mut run, 8 * cap);
    add(&mut run, flat);
    run.0 += 4 * cap;
    add(&mut run, 4 * (record.len() as u64 / 4).max(16) + 256);
    // the contexts
    add(&mut run, mags + 12 * nb);
    let half = s.ident.blocksizes[1] as u64 / 2;
    let (mut row_bytes, mut class_reads, mut submaps) = (0u64, false, 1);
    for r in &s.residues {
        let size = if r.kind == 2 { half * ch } else { half };
        let parts = (r.end as u64).min(size).saturating_sub((r.begin as u64).min(size)) / r.partition as u64;
        if parts > 0 {
            class_reads = true;
            let rows = if r.kind == 2 { 1 } else { ch };
            row_bytes = row_bytes.max((parts + s.codebooks[r.classbook].dims as u64) * rows);
        }
    }
    for m in &s.mappings {
        submaps = submaps.max(m.submaps.len() as u64);
    }
    let slots = if class_reads { 256 * 128 + submaps * ch * 1024 } else { ch * 128 };
    let arrays = 1 + (s.modes.iter().any(|m| m.blockflag) && s.modes.iter().any(|m| !m.blockflag)) as u64;
    add(&mut run, 2 * (2 * (arrays - 1) + 3) * slots);
    add(&mut run, 8 * ch);
    // a packet's writer and rows
    add(&mut run, 3 * grown(len.min(65535)).max(8) / 2);
    add(&mut run, row_bytes);

    let (bytes, buffers) = [build, run].into_iter().max_by_key(|&(b, n)| b + n * PAGE)?;
    Some(u.whole(bytes, buffers))
}

/// The node rebuilding the packets at the start of `data` (each after its 16-bit length, up to
/// the end of `data` or a length of 0) of a stream of `ident` and setup header `setup`, and the
/// bytes it takes; None where the setup or a packet does not parse, or there are no packets.
pub fn packets(f: &mut Formula, setup: &[u8], ident: Ident, data: &[u8]) -> Option<(Node, usize)> {
    let s = Setup::parse(setup, ident).ok()?;
    let (used, record) = codec::encode(&s, data)?;
    if used == 0 {
        return None;
    }
    let inputs = vec![f.push("vorbis/setup", setup.to_vec()), f.push("vorbis/record", record)];
    Some((Node::apply(MODULE, module::params(ident), inputs, used as u64), used))
}
