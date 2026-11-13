//! aqr-wwise as formulas (see aqr-formula): a medium as its head (up to its first audio packet,
//! the stripped setup header among it), its audio packets rebuilt by aqr-vorbis's module from
//! the record of their values, with the Vorbis setup header that the stripped one stands for,
//! and what follows them (FORMAT.md §3).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aqr_formula::{Formula, Node};
use aqr_vorbis::packet;
use aqr_vorbis::setup::{Ident, Setup};
use aqr_vorbis::setups::{self, Setups};

use crate::stripped::{self, Stripped};
use crate::{ids, wem};

/// The packets a setup found by its skeleton (not by the table of ids) is tried on (but the
/// last, which often ends early).
const TRIED: usize = 64;

/// Whether every packet of `packets` (of `d`) parses exactly under `setup`: to its end, at most
/// 7 zero bits left.
fn parses(setup: &Setup, d: &[u8], packets: &[std::ops::Range<usize>]) -> bool {
    packets.iter().all(|p| {
        packet::parse(setup, &d[p.clone()])
            .is_ok_and(|v| !v.short && v.tail.len() < 8 && v.tail.iter().all(|&b| b == 0))
    })
}

/// The setup header of `s` (of a stream of `ident`) from codebook parts `parts`, if it parses.
pub fn assemble(db: &Setups, s: &Stripped, ident: Ident, parts: &[u32]) -> Option<(Vec<u8>, Setup)> {
    let books: Vec<_> = parts.iter().map(|&b| db.codebook(b as usize)).collect();
    let bytes = s.assemble(&books);
    let setup = Setup::parse(&bytes, ident).ok()?;
    Some((bytes, setup))
}

/// The Vorbis setup header that stripped header `s` of medium `w` (of `d`) stands for: its
/// codebooks by the table of ids, else by a skeleton of the database that has as many codebooks,
/// agrees with the table where it knows an id, and parses the first packets exactly (FORMAT.md
/// §2).
pub fn setup(s: &Stripped, ident: Ident, d: &[u8], w: &wem::Wem) -> Option<Vec<u8>> {
    found(s, ident, d, w).map(|(bytes, _)| bytes)
}

/// [`setup`], parsed.
fn found(s: &Stripped, ident: Ident, d: &[u8], w: &wem::Wem) -> Option<(Vec<u8>, Setup)> {
    let db = setups::database().ok()?;
    let known: Vec<Option<u32>> = s.ids.iter().map(|&id| ids::part(id)).collect();
    let sample = &w.packets[..w.packets.len().saturating_sub(1).min(TRIED)];
    if let Some(parts) = known.iter().copied().collect::<Option<Vec<u32>>>() {
        return assemble(db, s, ident, &parts);
    }
    db.skeletons().iter().find_map(|k| {
        let parts = k.codebooks();
        let agrees = parts.len() == known.len() && known.iter().zip(parts).all(|(id, &p)| id.is_none_or(|id| id == p));
        let (bytes, setup) = agrees.then(|| assemble(db, s, ident, parts))??;
        (!sample.is_empty() && parses(&setup, d, sample)).then_some((bytes, setup))
    })
}

/// The setups found lately, by their stripped header and stream: media share a few setups, and
/// parsing one costs as much as encoding a small medium's packets.
type Cache = Mutex<HashMap<(Vec<u8>, Ident), Arc<(Vec<u8>, Setup)>>>;

/// The most setups kept (then all are dropped).
const CACHED: usize = 64;

/// [`found`] through the cache, for a setup found by the table of ids (one found by its packets
/// is those packets', not the stripped header's alone).
fn cached(stripped: &[u8], s: &Stripped, ident: Ident, d: &[u8], w: &wem::Wem) -> Option<Arc<(Vec<u8>, Setup)>> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Cache::default);
    let key = (stripped.to_vec(), ident);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return Some(hit.clone());
    }
    let by_table = s.ids.iter().all(|&id| ids::part(id).is_some());
    let made = Arc::new(found(s, ident, d, w)?);
    if by_table {
        let mut c = cache.lock().unwrap();
        if c.len() >= CACHED {
            c.clear();
        }
        c.insert(key, made.clone());
    }
    Some(made)
}

/// The class of a medium's head (its bytes up to its first audio packet).
pub const HEAD: &str = "wem/head";

/// Whether formula `f` is one of [`encode`]'s, or holds one.
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == HEAD)
}

/// The formula of medium `input` (a `.wem` of Wwise's Vorbis), or None where it is not one, its
/// setup is not found, or its streams but the setup (which media share) come to no less than
/// its bytes: so a formula given is smaller than the medium, which zstd hardly compresses (its
/// packets are coded), and need not be compared with it.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let f = formula_of(input)?;
    let own: usize = f.streams.iter().filter(|s| s.class != "vorbis/setup").map(|s| s.data.len()).sum();
    (own < input.len()).then_some(f)
}

/// The formula of medium `input`, whatever its size ([`encode`]'s, before it is weighed).
pub fn formula_of(input: &[u8]) -> Option<Formula> {
    let w = wem::parse(input).ok()?;
    let [b0, b1] = w.blocksize_pows;
    let ident = Ident::from_exps(w.channels, b0, b1, w.mod_packets)?;
    let s = stripped::parse(&input[w.setup.clone()], w.channels)?;
    let setup = cached(&input[w.setup.clone()], &s, ident, input, &w)?;
    // the whole packets (each after its 16-bit length)
    let end = w.packets.last().map_or(w.audio, |p| p.end);
    let mut f = Formula::default();
    let (node, used) = aqr_vorbis::formula::packets_parsed(&mut f, &setup.0, &setup.1, &input[w.audio..end])?;
    let mut parts = vec![f.push(HEAD, input[..w.audio].to_vec()), node];
    if w.audio + used < input.len() {
        parts.push(f.push("wem/tail", input[w.audio + used..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}
