//! Whether recompressing a stream may pay, told before doing it. A recompressor turns a coded
//! stream (DEFLATE, LZ4, ...) that does not compress further into its data and a record of
//! how it was coded; what that saves comes from the data archiving smaller than the coded
//! stream did, and finding out by recompressing it costs about as much as the encoder took.
//!
//! [`Probe::pays`] tells from estimates instead: the record's (the recompressor's own, on a
//! sample say), and the fewest bytes the data may archive in at the level of the archive
//! ([`Encoder::least`]), what other data archived with it holds too ([`shared`]) and the files
//! in it that would be recompressed in turn counted as nothing (what they come to is not told
//! without recompressing them). What is turned down would not have paid; what is taken may
//! still not, and is then dropped as before.

use std::collections::HashMap;

use crate::Kind;
use crate::aqr::{Encoder, Level};

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

/// What a probe needs of the archive around: its level, and how files are found in data.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    /// The level the data will be archived at.
    pub level: Level,
    /// How data of a kind is looked into ([`Look::No`] for all, where it is as deep as formats
    /// nest).
    pub look: fn(Kind) -> Look,
    /// Whether data starts as a file: it is then scanned first, as it is often all of it.
    pub starts: fn(&[u8]) -> bool,
    /// Bytes of the files that a scan of data finds.
    pub scan: fn(&[u8]) -> usize,
}

/// The least a recompressor must save, as a fraction of the stream: estimates are off by about
/// as much (a sample, a prefix), and what saves less is not worth its time.
const MARGIN: usize = 32;

impl Probe {
    /// A probe at `level` that looks for no files.
    pub fn flat(level: Level) -> Probe {
        Probe { level, look: |_| Look::No, starts: |_| false, scan: |_| 0 }
    }

    /// Whether recompressing a stream of `coded` bytes, which does not compress, into a record
    /// of about `record()` bytes (asked only where needed; `None` where it cannot be told) and
    /// `data` of `kind` may pay: whether they may archive in at least 1/32 less than the
    /// stream, the fraction `shared()` of the data held by other data archived with it as well
    /// ([`shared`]; asked only where needed) counted as nothing, by `enc` (an encoder at
    /// [`Probe::level`], which keeps what it compressed of the data for archiving it, and
    /// drops it where it turns it down).
    pub fn pays(
        &self,
        enc: &Encoder,
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
        let Some(enough) = (coded - coded / MARGIN).checked_sub(record + 1) else { return false };
        let scanned = look == Look::Scan;
        let first = (scanned && (self.starts)(data)).then(|| (self.scan)(data));
        if first == Some(data.len()) {
            return true;
        }
        let Ok(archived) = enc.least(data, enough) else { return true };
        let pays = |free: f64| archived as f64 * (1.0 - free.min(1.0)) <= enough as f64;
        // (else what other data holds, and the files, asked only where they would make the
        // difference)
        let pays = pays(0.0) || {
            let shared = shared().clamp(0.0, 1.0);
            let files = || first.unwrap_or_else(|| (self.scan)(data)) as f64 / data.len().max(1) as f64;
            shared > 0.0 && pays(shared) || scanned && pays(shared + files())
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
    let anchors: Vec<Vec<u64>> = datas
        .iter()
        .map(|d| {
            let (mut h, mut a) = (0u64, Vec::with_capacity(d.len() / 200));
            for &b in d.iter() {
                h = (h << 1).wrapping_add(GEAR[b as usize]);
                if h & 0xff == 0 {
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
    anchors
        .iter()
        .map(|a| a.iter().filter(|h| owner[h] == MANY).count() as f64 / a.len().max(1) as f64)
        .collect()
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
            (0..n).map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            }).collect::<Vec<u8>>()
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
}
