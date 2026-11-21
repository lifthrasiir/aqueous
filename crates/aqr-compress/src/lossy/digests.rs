//! Files whose digests the others archived along hold (docs/lossy-design.md §8, item 3): a
//! checksum list (`SHA256SUMS`, `NAME.sha256`), a lockfile's integrity (npm's and yarn's
//! `sha512-…`, SRI's), a package index or a catalog of digests tells of the file as it is, and
//! would no longer once it is rewritten. Before the batches, of the contents that a rewriter of
//! [`Lossy`] may take (by their first bytes, [`HEAD`]): their digests ([`ALGORITHMS`]), each as
//! its bytes, its hex (lower and upper case) and its base64 (standard and URL-safe, without the
//! padding, which the forms with it hold), looked for in every other content of [`SCAN_MAX`]
//! bytes or fewer at once (Aho-Corasick); and a detached signature beside them
//! ([`super::signatures`]). Those found are kept as they are, told where.
//!
//! The digests of a content are made in one pass over it, a [`CHUNK`] at a time given to each
//! hash in turn (the chunk still in the cache), the contents on the threads.

use std::collections::HashMap;

use aho_corasick::AhoCorasick;

use super::{Class, Lossy, REWRITERS, Rewrite};
use crate::{Format, Options, Source, detect};

aqr_common::zopt! {
    /// The most bytes of a content looked into for the digests of the others ([`referenced`]).
    static SCAN_MAX: Opt<usize> = Opt::size("lossy:scan-max", 64 << 20, "most bytes of a file looked into for others' digests");
}

/// Bytes of a content's start its format is told by.
const HEAD: u64 = 64 << 10;
/// Bytes given to each hash in turn.
const CHUNK: usize = 256 << 10;

/// The digests looked for, by name.
pub(super) const ALGORITHMS: [&str; 6] = ["MD5", "SHA-1", "SHA-256", "SHA-384", "SHA-512", "BLAKE2b-512"];

/// The digests of `d` ([`ALGORITHMS`], in order).
pub(super) fn digests(d: &[u8]) -> [Vec<u8>; 6] {
    use sha2::Digest;
    let (mut a, mut b, mut c) = (md5::Md5::new(), sha1::Sha1::new(), sha2::Sha256::new());
    let (mut e, mut f, mut g) = (sha2::Sha384::new(), sha2::Sha512::new(), blake2::Blake2b512::new());
    for piece in d.chunks(CHUNK) {
        a.update(piece);
        b.update(piece);
        c.update(piece);
        e.update(piece);
        f.update(piece);
        g.update(piece);
    }
    [
        a.finalize().to_vec(),
        b.finalize().to_vec(),
        c.finalize().to_vec(),
        e.finalize().to_vec(),
        f.finalize().to_vec(),
        g.finalize().to_vec(),
    ]
}

/// `d` in base64 of `alphabet`, without the padding.
fn base64(d: &[u8], alphabet: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..=c.len() {
            out.push(alphabet[(n >> (18 - 6 * k) & 63) as usize]);
        }
    }
    out
}

/// The forms a digest is written in, looked for: its bytes, hex (lower, upper), base64 (standard,
/// URL-safe), the last two without padding.
pub(super) fn forms(h: &[u8]) -> [Vec<u8>; 5] {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let hex =
        |digits: &[u8; 16]| h.iter().flat_map(|b| [digits[(b >> 4) as usize], digits[(b & 15) as usize]]).collect();
    [h.to_vec(), hex(b"0123456789abcdef"), hex(b"0123456789ABCDEF"), base64(h, STD), base64(h, URL)]
}

/// The class and format of content `head` (of `len` bytes) that `lossy` lets a rewriter take, if any.
fn taken(head: &[u8], len: u64, lossy: &Lossy, opt: &Options) -> Option<(Format, Class)> {
    let format = detect(head)?;
    let w = REWRITERS.iter().find(|w| {
        w.format == format
            && lossy.allows(w.class, format)
            && (w.class != Class::Coding || lossy.codes(format, len, opt))
    })?;
    Some((format, w.class))
}

/// The contents of `src` that a rewriter would take but are to be kept as they are, as their
/// digests are in other contents, or a signature of them is beside them: what became of them.
pub(crate) fn referenced<S: Source + ?Sized>(src: &S, opt: &Options) -> Vec<(usize, Rewrite)> {
    let Some(lossy) = &opt.lossy else { return Vec::new() };
    let _s = aqr_common::trace::span("lossy:digests");
    let n = src.len();
    let heads = aqr_common::pool::par_map(n, opt.threads, |c| src.read_range(c, 0..HEAD).ok());
    let candidates: Vec<(usize, Format, Class)> = (0..n)
        .filter_map(|c| heads[c].as_deref().and_then(|h| taken(h, src.size(c), lossy, opt)).map(|(f, k)| (c, f, k)))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let names: HashMap<&[u8], usize> = (0..n).map(|c| (src.name(c), c)).collect();
    let read = |c: usize| src.read(c).ok().map(|d| d.to_vec());
    // (each candidate's digests, and whether a signature is beside it)
    let made = aqr_common::pool::par_map(candidates.len(), opt.threads, |i| {
        let c = candidates[i].0;
        let d = src.read(c).ok()?;
        let signed = super::signatures::signed(src.name(c), &d, &names, &read);
        Some((digests(&d), signed))
    });
    let mut kept: HashMap<usize, String> = HashMap::new();
    let mut patterns: Vec<Vec<u8>> = Vec::new();
    let mut owners: Vec<(usize, usize)> = Vec::new();
    for (i, m) in made.iter().enumerate() {
        let Some((ds, signed)) = m else { continue };
        if let Some(reason) = signed {
            kept.insert(i, reason.clone());
            continue;
        }
        for (a, h) in ds.iter().enumerate() {
            for f in forms(h) {
                patterns.push(f);
                owners.push((i, a));
            }
        }
    }
    if !patterns.is_empty() {
        let ac = AhoCorasick::new(&patterns).expect("patterns of digests");
        let max = SCAN_MAX.get() as u64;
        let found = aqr_common::pool::par_map(n, opt.threads, |c| {
            if src.size(c) > max {
                return Vec::new();
            }
            let Ok(d) = src.read(c) else { return Vec::new() };
            let mut hits: Vec<(usize, usize)> = ac
                .find_overlapping_iter(&d[..])
                .map(|m| owners[m.pattern().as_usize()])
                .filter(|&(i, _)| candidates[i].0 != c)
                .collect();
            hits.sort_unstable();
            hits.dedup_by_key(|h| h.0);
            hits
        });
        for (c, hits) in found.into_iter().enumerate() {
            for (i, a) in hits {
                let shown = String::from_utf8_lossy(src.name(c)).into_owned();
                kept.entry(i).or_insert_with(|| format!("its {} is in {shown}", ALGORITHMS[a]));
            }
        }
    }
    let mut out: Vec<(usize, Rewrite)> = kept
        .into_iter()
        .map(|(i, reason)| {
            let (c, format, class) = candidates[i];
            (c, Rewrite::Kept { format, class, reason })
        })
        .collect();
    out.sort_unstable_by_key(|o| o.0);
    out
}
