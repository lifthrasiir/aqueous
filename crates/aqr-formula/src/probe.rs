//! Whether recompressing a stream may pay, told before doing it. A recompressor turns a coded
//! stream (DEFLATE, LZ4, ...) that does not compress further into its data and a record of
//! how it was coded; what that saves comes from the data archiving smaller than the coded
//! stream did, and finding out by recompressing it costs about as much as the encoder took.
//!
//! [`Probe::pays`] tells from estimates instead: the record's (the recompressor's own, on a
//! sample say), and the fewest bytes the data may archive in at the level of the archive
//! ([`Archiver::least`], by a sample), what it repeats from farther than the sample shows
//! ([`repeated`]), what other data archived with it holds too ([`shared`]) and the files in it
//! that would be recompressed in turn counted as nothing (what they come to is not told without
//! recompressing them). What is turned down would not have paid; what is taken may
//! still not, and is then dropped as before.

use std::collections::HashMap;
use std::sync::Arc;

use crate::{Kind, Result};

/// The archive encoder at the level data will be archived at, as a probe asks of it
/// (aqr-archive's `Encoder`).
pub trait Archiver {
    /// About the fewest bytes `d` archives in, by a sample, at the levels the encoder may pick
    /// for it: at most `enough` once one gives that few, the others then not tried. The encoder
    /// keeps what it compressed of `d`, for archiving it.
    fn least(&self, d: &[u8], enough: usize) -> Result<usize>;

    /// Drops what it keeps of `d` compressed, where `d` will not be archived.
    fn forget(&self, d: &[u8]);

    /// Bytes of the sample [`Archiver::least`] takes of data, and of each of the chunks it is
    /// cut in, spread over the data.
    fn sample(&self) -> (usize, usize);

    /// The window that data of `len` bytes is archived in where it is all of the data
    /// (long-distance matching), else None (zstd's own window of the level).
    fn long_window(&self, len: usize) -> Option<usize>;
}

/// [`Archiver`]s of a number of threads each: [`Probe::archiver`].
pub type Archivers = Arc<dyn Fn(usize) -> Box<dyn Archiver> + Send + Sync>;

/// How data of a kind is looked into for files that are recompressed in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Not at all.
    No,
    /// As a file, all of it.
    Whole,
    /// Scanned: [`Probe::scan`] tells the bytes of the files found.
    Scan,
}

/// What a probe needs of the archive around: its encoder, and how files are found in data.
#[derive(Clone)]
pub struct Probe {
    /// The archive encoder at the level the data will be archived at, of a number of threads.
    pub archiver: Archivers,
    /// How data of a kind is looked into ([`Look::No`] for all, where it is as deep as formats
    /// nest).
    pub look: fn(Kind) -> Look,
    /// Whether data starts as a file: it is then scanned first, as it is often all of it.
    pub starts: fn(&[u8]) -> bool,
    /// Bytes of the files that a scan of data finds.
    pub scan: fn(&[u8]) -> usize,
}

aqr_common::zopt_namespace!("probe", "probes of whether a recompressor pays (aqr-formula)");

aqr_common::zopt! {
    /// The least a recompressor must save, as a fraction of the stream: estimates are off by about
    /// as much (a sample, a prefix), and what saves less is not worth its time.
    static MARGIN: Opt<usize> = Opt::value("probe:margin", 32, "a recompressor must save 1/N of the stream or more");

    /// Data shorter than this is not looked for among what is archived with it ([`Held::MIN`] is
    /// its default): its anchors are too few to tell.
    static HELD_MIN: Opt<usize> = Opt::size("probe:held-min", Held::MIN, "data shorter is not looked for in what is archived with it");

    /// Data shorter than this does not count as holding any: it has about no anchor. Shorter
    /// data than [`HELD_MIN`] does, as data looked for may be many such (the files of a tar
    /// archive, each held by a file of its own).
    static HELD_INDEX_MIN: Opt<usize> = Opt::size("probe:index-min", 4 << 10, "data shorter holds no anchors of others");

    /// Of the gear hash, the low bits that are zero where a position is an anchor ([`shared`],
    /// [`repeated`]): 1 in 256 of all.
    static ANCHOR_BITS: Opt<u32> = Opt::value("probe:anchor-bits", 8, "anchors of shared and repeated are 1 in 2^N positions");

    /// The window [`repeated`] looks in for data that is not long (zstd's own at level 9).
    static REPEATED_WINDOW: Opt<usize> = Opt::size("probe:repeated-window", 4 << 20, "window that repeats are looked for in, for data that is not long");

    /// The shortest and longest chunk of [`chunks`], and its bits: boundaries fall at 1 in
    /// 2048.
    static CHUNK_MIN: Opt<usize> = Opt::size("probe:chunk-min", 512, "shortest content-defined chunk");
    static CHUNK_MAX: Opt<usize> = Opt::size("probe:chunk-max", 8 << 10, "longest content-defined chunk");
    static CHUNK_BITS: Opt<u32> = Opt::value("probe:chunk-bits", 11, "chunk boundaries are 1 in 2^N positions");

    /// A hash that more than this many data left have in a bin is not counted by
    /// [`alike_chains`]: it tells little, and would take time as the square of the data.
    static LIKENESS_COMMON: Opt<usize> = Opt::value("probe:likeness-common", 1024, "a likeness hash in more data than this is not counted");
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe").finish_non_exhaustive()
    }
}

impl Probe {
    /// A probe by `archiver` that looks for no files.
    pub fn flat(archiver: Archivers) -> Probe {
        Probe { archiver, look: |_| Look::No, starts: |_| false, scan: |_| 0 }
    }

    /// Whether recompressing a stream of `coded` bytes, which does not compress, into a record
    /// of about `record()` bytes (asked only where needed; `None` where it cannot be told) and
    /// `data` of `kind` may pay: whether they may archive in at least 1/32 less than the
    /// stream, what the data repeats from afar ([`repeated`]) and the fraction `shared()` of it
    /// held by other data archived with it as well ([`shared`]; asked only where needed)
    /// counted as nothing, by `enc` (an encoder as [`Probe::archiver`] makes, which keeps what
    /// it compressed of the data for archiving it, and drops it where it turns it down).
    pub fn pays(
        &self,
        enc: &dyn Archiver,
        coded: usize,
        record: impl FnOnce() -> Option<usize>,
        data: &[u8],
        shared: impl FnOnce() -> f64,
        kind: Kind,
    ) -> bool {
        let look = (self.look)(kind);
        if look == Look::Whole {
            return true;
        }
        // (what cannot be told is the recompressor's to tell)
        let Some(record) = record() else { return true };
        let Some(enough) = (coded - coded / MARGIN.get()).checked_sub(record + 1) else { return false };
        let scanned = look == Look::Scan;
        let first = (scanned && (self.starts)(data)).then(|| (self.scan)(data));
        if first == Some(data.len()) {
            return true;
        }
        let Ok(archived) = enc.least(data, enough) else { return true };
        let pays = |free: f64| archived as f64 * (1.0 - free.min(1.0)) <= enough as f64;
        // (else what the data repeats from afar, what other data holds, and the files, asked
        // only where they would make the difference)
        let pays = pays(0.0) || {
            let own = repeated(data, enc);
            own > 0.0 && pays(own) || {
                let shared = 1.0 - (1.0 - own) * (1.0 - shared().clamp(0.0, 1.0));
                let files = || first.unwrap_or_else(|| (self.scan)(data)) as f64 / data.len().max(1) as f64;
                shared > own && pays(shared) || scanned && pays(shared + files())
            }
        };
        if !pays {
            enc.forget(data);
        }
        pays
    }
}

/// For each of `datas` (archived together), about the fraction of it that another of them
/// holds too, in runs of some length: as in files much alike, which compress far better
/// together than alone (and a sample of each alone does not tell). By anchors, where a gear
/// hash of the last 64 bytes falls in 1 of 256 (content-defined, so that runs are found
/// wherever they lie): the fraction of a data's distinct anchors that another has. A pass over
/// the data, at about a byte per cycle.
pub fn shared(datas: &[&[u8]]) -> Vec<f64> {
    let mask = (1u64 << ANCHOR_BITS.get()) - 1;
    let anchors: Vec<Vec<u64>> = datas
        .iter()
        .map(|d| {
            let (mut h, mut a) = (0u64, Vec::with_capacity(d.len() / 200));
            for &b in d.iter() {
                h = (h << 1).wrapping_add(GEAR[b as usize]);
                if h & mask == 0 {
                    a.push(h);
                }
            }
            a.sort_unstable();
            a.dedup();
            a
        })
        .collect();
    // the data holding each anchor, or MANY
    const MANY: usize = usize::MAX;
    let mut owner: HashMap<u64, usize> = HashMap::new();
    for (i, a) in anchors.iter().enumerate() {
        for &h in a {
            owner.entry(h).and_modify(|o| *o = if *o == i { i } else { MANY }).or_insert(i);
        }
    }
    anchors.iter().map(|a| a.iter().filter(|h| owner[h] == MANY).count() as f64 / a.len().max(1) as f64).collect()
}

/// Bytes of the pieces [`repeated`] cuts data into: those of the chunks a sample of data is
/// made of ([`Archiver::least`]).
fn piece_len(a: &dyn Archiver) -> usize {
    let (sample, chunks) = a.sample();
    sample / chunks
}

/// About the fraction of `d` that repeats what comes earlier in it, from farther than a sample
/// of it shows but within the window it is archived in: by anchors as in [`shared`], those of
/// each piece of `PIECE` bytes that an earlier piece has, counted where they first come in
/// the piece (a repeat within a piece, the sample shows). Nothing for data that a sample is all
/// of. A pass over the data, at about a byte per cycle; `a` tells the sample and the window.
pub fn repeated(d: &[u8], a: &dyn Archiver) -> f64 {
    if d.len() <= a.sample().0 {
        return 0.0;
    }
    let (piece_len, mask) = (piece_len(a), (1u64 << ANCHOR_BITS.get()) - 1);
    // (zstd's own at level 9 for data that is not long, else one of all of it)
    let window = a.long_window(d.len()).unwrap_or_else(|| REPEATED_WINDOW.get());
    // each anchor with the last piece that has it
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let (mut h, mut total, mut afar) = (0u64, 0usize, 0usize);
    for (k, piece) in d.chunks(piece_len).enumerate() {
        let mut own = Vec::new();
        for &b in piece {
            h = (h << 1).wrapping_add(GEAR[b as usize]);
            if h & mask == 0 {
                total += 1;
                if !own.contains(&h) {
                    afar += seen.get(&h).is_some_and(|&j| (k - j) * piece_len <= window) as usize;
                    own.push(h);
                }
            }
        }
        own.into_iter().for_each(|a| _ = seen.insert(a, k));
    }
    afar as f64 / total.max(1) as f64
}

/// What the data archived together holds (the files of a batch, say), to tell whether data
/// is held there already: data that repeats one of them costs nothing, where the archive stores
/// it once (a file and the same file in an archive). By anchors as in [`shared`], but sparser,
/// where the gear hash's top 12 bits are zero (1 in 4096, of all 64 bytes hashed), each with
/// the data that has it.
#[derive(Debug, Default)]
pub struct Held {
    /// The anchors, each with the index of the only data that has it, or [`MANY`].
    anchors: HashMap<u64, usize>,
    /// Where each data is in memory (its start and end, and its index), by its start, to tell
    /// data within it.
    at: Vec<(usize, usize, usize)>,
}

const MANY: usize = usize::MAX;

impl Held {
    /// The default of `probe:held-min`: data shorter than this is not looked for, its anchors
    /// being too few to tell.
    pub const MIN: usize = 256 << 10;

    /// What `datas` hold, of those at least `probe:index-min` bytes long, hashed on up to
    /// `threads` threads.
    pub fn of(datas: &[&[u8]], threads: usize) -> Held {
        let _s = aqr_common::trace::span("held");
        let index_min = HELD_INDEX_MIN.get();
        let long: Vec<usize> = (0..datas.len()).filter(|&i| datas[i].len() >= index_min).collect();
        let found = aqr_common::pool::par_map(long.len(), threads, |j| held_anchors(datas[long[j]]));
        let mut anchors: HashMap<u64, usize> = HashMap::new();
        for (&i, a) in long.iter().zip(found) {
            for h in a {
                anchors.entry(h).and_modify(|o| *o = if *o == i { i } else { MANY }).or_insert(i);
            }
        }
        let mut at: Vec<(usize, usize, usize)> =
            datas.iter().enumerate().map(|(i, d)| (d.as_ptr() as usize, d.as_ptr() as usize + d.len(), i)).collect();
        at.sort_unstable();
        Held { anchors, at }
    }

    /// The data (its index) that `d` lies within, if any.
    pub fn owner(&self, d: &[u8]) -> Option<usize> {
        let p = d.as_ptr() as usize;
        // (the last that starts at it or before, of those not empty)
        let n = self.at.partition_point(|&(a, _, _)| a <= p);
        self.at[..n].iter().rev().find(|&&(a, b, _)| a < b).filter(|&&(_, b, _)| p + d.len() <= b).map(|&(_, _, i)| i)
    }

    /// Whether `data` is about all held by data other than `except` (that it is part of, say):
    /// all but 1 in 100 of its distinct anchors.
    pub fn holds(&self, data: &[u8], except: Option<usize>) -> bool {
        if data.len() < HELD_MIN.get() || self.anchors.is_empty() {
            return false;
        }
        let mut a = held_anchors(data);
        a.sort_unstable();
        a.dedup();
        let held = a.iter().filter(|h| self.anchors.get(h).is_some_and(|&o| Some(o) != except)).count();
        !a.is_empty() && held >= a.len() - a.len() / 100
    }
}

/// The hashes of the pieces of `d` cut where a gear hash of the last 64 bytes falls in 1 of
/// 2048 (2 KiB long or so, of 512 bytes to 8 KiB), but those of fewer than 16 distinct
/// bytes (zeros, padding), sorted: data alike but for some bytes, or shifted by some
/// (another header), has most of them in common. Of a file's start, to tell files alike.
pub fn chunks(d: &[u8]) -> Vec<u64> {
    let (min, max, shift) = (CHUNK_MIN.get(), CHUNK_MAX.get(), 64 - CHUNK_BITS.get());
    let mut keys = Vec::with_capacity(d.len() / 1024);
    let mut push = |c: &[u8]| {
        let mut seen = [false; 256];
        c.iter().for_each(|&b| seen[b as usize] = true);
        if seen.iter().filter(|&&s| s).count() >= 16 {
            keys.push(xxhash_rust::xxh3::xxh3_64(c));
        }
    };
    let (mut start, mut h) = (0, 0u64);
    for (i, &b) in d.iter().enumerate() {
        h = (h << 1).wrapping_add(GEAR[b as usize]);
        let len = i + 1 - start;
        if (len >= min && h >> shift == 0) || len >= max {
            push(&d[start..=i]);
            start = i + 1;
        }
    }
    if start < d.len() {
        push(&d[start..]);
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// What data is like ([`likeness`]): the least hash of its anchors in each of `Likeness::BINS`
/// bins, by the hash ([`u64::MAX`] where none falls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Likeness([u64; Likeness::BINS]);

impl Likeness {
    const BINS: usize = 64;

    /// How many bins two data share: about the fraction of their anchors they share, of 64.
    #[cfg(test)]
    fn shared(&self, other: &Likeness) -> usize {
        self.0.iter().zip(&other.0).filter(|&(a, b)| a == b && *a != u64::MAX).count()
    }
}

/// What `d` is like, to put data much alike next to each other (the executables of a program
/// in several versions, say, which compress far better within a block of each other): a
/// MinHash of one permutation over its anchors (as in [`Held`], 1 in 64), at about a byte per
/// cycle.
pub fn likeness(d: &[u8]) -> Likeness {
    let mut bins = [u64::MAX; Likeness::BINS];
    let mut h = 0u64;
    for &b in d {
        h = (h << 1).wrapping_add(GEAR[b as usize]);
        if h >> 58 == 0 {
            // (SplitMix64's finalizer: the anchors themselves have their top bits zero)
            let mut v = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            v = (v ^ (v >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            v ^= v >> 31;
            let bin = &mut bins[(v % Likeness::BINS as u64) as usize];
            *bin = (*bin).min(v);
        }
    }
    Likeness(bins)
}

/// Chains of data `likes` alike (by index), each from the first left, each data followed by the
/// one most like it of those left (the first of those as much alike): a data like none is a
/// chain of its own. A hash that more than `probe:likeness-common` data left have in a bin is not
/// counted: it tells little, and would take time as the square of the data.
pub fn alike_chains(likes: &[Likeness]) -> Vec<Vec<usize>> {
    let n = likes.len();
    let common = LIKENESS_COMMON.get();
    let mut having: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, l) in likes.iter().enumerate() {
        for (k, &h) in l.0.iter().enumerate().filter(|&(_, &h)| h != u64::MAX) {
            having.entry((k, h)).or_default().push(i);
        }
    }
    let (mut left, mut chains, mut laid, mut first) = (vec![true; n], Vec::new(), 0, 0);
    // (the bins each data left shares with the last laid)
    let (mut shared, mut touched) = (vec![0usize; n], Vec::new());
    while laid < n {
        while !left[first] {
            first += 1;
        }
        let (mut at, mut chain) = (first, Vec::new());
        loop {
            left[at] = false;
            chain.push(at);
            laid += 1;
            for (k, &h) in likes[at].0.iter().enumerate().filter(|&(_, &h)| h != u64::MAX) {
                let Some(those) = having.get_mut(&(k, h)) else { continue };
                those.retain(|&j| left[j]);
                if those.len() > common {
                    continue;
                }
                for &j in those.iter() {
                    if shared[j] == 0 {
                        touched.push(j);
                    }
                    shared[j] += 1;
                }
            }
            let next = touched.iter().copied().max_by_key(|&j| (shared[j], std::cmp::Reverse(j)));
            touched.drain(..).for_each(|j| shared[j] = 0);
            match next {
                Some(j) => at = j,
                None => break,
            }
        }
        chains.push(chain);
    }
    chains
}

fn held_anchors(d: &[u8]) -> Vec<u64> {
    let (mut h, mut a) = (0u64, Vec::with_capacity(d.len() >> 12));
    for &b in d {
        h = (h << 1).wrapping_add(GEAR[b as usize]);
        if h >> 52 == 0 {
            a.push(h);
        }
    }
    a
}

/// The gear hash's table: 256 values of SplitMix64.
const GEAR: [u64; 256] = {
    let (mut t, mut x, mut i) = ([0u64; 256], 0u64, 0);
    while i < 256 {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Data held by another counts whole wherever it lies in it; data held by none, nothing.
    #[test]
    fn shared_runs() {
        let noise = |seed: u64, n: usize| {
            let mut x = seed;
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 56) as u8
                })
                .collect::<Vec<u8>>()
        };
        let (a, b) = (noise(1, 200_000), noise(2, 200_000));
        let ab = [&b[..50_000], &a[..], &b[50_000..]].concat();
        let f = shared(&[&a, &b, &ab]);
        assert!(f[0] > 0.97, "{f:?}");
        assert!(f[1] > 0.97, "{f:?}");
        assert!((0.97..=1.0).contains(&f[2]), "{f:?}");
        let f = shared(&[&a, &b]);
        assert!(f[0] < 0.01 && f[1] < 0.01, "{f:?}");
        assert_eq!(shared(&[&a]), [0.0]);
    }

    /// Data shifted by some bytes and edited has most pieces in common with the data; other
    /// data, and runs of a byte, none.
    #[test]
    fn chunks_alike() {
        let noise = |seed: u64, n: usize| {
            let mut x = seed;
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 56) as u8
                })
                .collect::<Vec<u8>>()
        };
        let a = noise(1, 64 << 10);
        let mut b = [&noise(2, 37)[..], &a[..]].concat();
        b[30_000] ^= 1;
        let (ka, kb, kc) = (chunks(&a), chunks(&b), chunks(&noise(3, 64 << 10)));
        let common = |x: &[u64], y: &[u64]| x.iter().filter(|k| y.binary_search(k).is_ok()).count();
        assert!(ka.len() > 16 && common(&ka, &kb) >= ka.len() - 3, "{} of {}", common(&ka, &kb), ka.len());
        assert_eq!(common(&ka, &kc), 0);
        assert!(chunks(&[0u8; 64 << 10]).is_empty());
    }

    /// Data much alike (another version of it: edits here and there) laid next to each other,
    /// the rest in its order.
    #[test]
    fn likeness_order() {
        let noise = |seed: u64, n: usize| {
            let mut x = seed;
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 56) as u8
                })
                .collect::<Vec<u8>>()
        };
        let edited = |d: &[u8], seed: u64| {
            let mut e = d.to_vec();
            (0..e.len()).step_by(5000).for_each(|i| e[i] ^= seed as u8 | 1);
            e
        };
        let (a, b, c) = (noise(1, 1 << 20), noise(2, 1 << 20), noise(3, 1 << 20));
        let (a2, b2, d) = (edited(&a, 7), edited(&b, 9), noise(4, 1 << 20));
        let likes: Vec<Likeness> = [&a, &b, &c, &a2, &d, &b2].iter().map(|x| likeness(x)).collect();
        assert!(likes[0].shared(&likes[3]) > 32 && likes[0].shared(&likes[1]) == 0, "{likes:?}");
        assert_eq!(alike_chains(&likes), [vec![0, 3], vec![1, 5], vec![2], vec![4]]);
        // none alike, and nothing to tell by
        assert_eq!(alike_chains(&likes[..3]), [[0], [1], [2]]);
        assert_eq!(alike_chains(&[likeness(&[]), likeness(b"short"), likes[0].clone()]), [[0], [1], [2]]);
    }

    /// Data held whole (all of a data, or part of it) by another, but not by itself alone;
    /// data held in part or by none, not.
    #[test]
    fn held_runs() {
        let noise = |seed: u64, n: usize| {
            let mut x = seed;
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    (x >> 56) as u8
                })
                .collect::<Vec<u8>>()
        };
        let (a, b, c) = (noise(1, 4 << 20), noise(2, 4 << 20), noise(3, 4 << 20));
        let held = Held::of(&[&a, &b], 2);
        assert!(held.holds(&a, None));
        assert!(held.holds(&a[1 << 20..3 << 20], None));
        assert!(!held.holds(&[&a[1 << 20..3 << 20], &c[..2 << 20]].concat(), None));
        assert!(!held.holds(&c, None));
        assert!(!held.holds(&a[..1000], None));
        // where data is, and not held by the data it is part of alone; but by another as well
        assert_eq!((held.owner(&a[5..100]), held.owner(&b), held.owner(&c)), (Some(0), Some(1), None));
        assert!(!held.holds(&a[1 << 20..3 << 20], held.owner(&a[1 << 20..3 << 20])));
        let ab = [&a[..], &b[..]].concat();
        let held = Held::of(&[&a, &ab], 2);
        assert!(held.holds(&a[1 << 20..3 << 20], held.owner(&a)));
    }
}
