//! The code signatures of Mach-O images, their pages' hashes derived (`exe-recons/macho-codesign`,
//! FORMAT.md §G). A CodeDirectory holds a hash of each page of its image up to its code limit;
//! the encoder zeroes those it finds to be so, and the decoder computes them again once the
//! rest of the file is there. The signature's other parts (the CodeDirectory's other fields and
//! special slots, the requirements, the CMS signature) are kept as they are.
//!
//! Params: the count of signatures, then of each (in the order of the file) the offset of its
//! image, its offset and its length, and a mask of the blobs of its SuperBlob (by their index)
//! whose code slots are to be computed, as varints. Inputs: the file without the signatures
//! (the body), then the signatures one after another, those code slots zeroed.

use alloc::vec::Vec;

/// The magic of a SuperBlob of an embedded signature.
const EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
/// The magic of a CodeDirectory.
const CODE_DIRECTORY: u32 = 0xfade_0c02;
/// Blobs of a SuperBlob that a mask tells of at most.
const MOST_BLOBS: usize = 64;

fn be32(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

/// What the code slots of a CodeDirectory are: where they start (in the signature), their
/// count, the code limit, the hash, its size and the page size (0: one page, to the limit).
#[derive(Clone, PartialEq)]
struct Slots {
    at: usize,
    count: usize,
    limit: usize,
    hash: u8,
    size: usize,
    page: usize,
}

/// The digest of `d` by hash type `t` (1 SHA-1, 2 SHA-256, 3 SHA-256 cut to 20 bytes, 4 SHA-384),
/// into `out` (of its size); false for another type or size.
#[cfg(feature = "encode")]
fn digest(t: u8, d: &[u8], out: &mut [u8]) -> bool {
    use sha2::Digest;
    match (t, out.len()) {
        (1, 20) => out.copy_from_slice(&sha1::Sha1::digest(d)),
        (2, 32) => out.copy_from_slice(&sha2::Sha256::digest(d)),
        (3, 20) => out.copy_from_slice(&sha2::Sha256::digest(d)[..20]),
        (4, 48) => out.copy_from_slice(&sha2::Sha384::digest(d)),
        _ => return false,
    }
    true
}

/// The code slots of the `k`-th blob of `sig` (a SuperBlob), where it is a CodeDirectory whose
/// slots lie within it.
fn slots(sig: &[u8], k: usize) -> Option<Slots> {
    if be32(sig, 0)? != EMBEDDED_SIGNATURE || k >= be32(sig, 8)? as usize {
        return None;
    }
    let at = be32(sig, 12 + 8 * k + 4)? as usize;
    let cd = sig.get(at..)?;
    let len = be32(cd, 4)? as usize;
    if be32(cd, 0)? != CODE_DIRECTORY || len > cd.len() || len < 40 {
        return None;
    }
    let cd = &cd[..len];
    let (hash_offset, count, limit) = (be32(cd, 16)? as usize, be32(cd, 28)? as usize, be32(cd, 32)? as usize);
    let (size, hash, page_bits) = (cd[36] as usize, cd[37], cd[39]);
    let page = match page_bits {
        0 => 0,
        b @ 1..=31 => 1usize << b,
        _ => return None,
    };
    let pages = if page == 0 { usize::from(limit > 0) } else { limit.div_ceil(page) };
    let end = hash_offset.checked_add(count.checked_mul(size)?)?;
    (count == pages && end <= len).then_some(Slots { at: at + hash_offset, count, limit, hash, size, page })
}

/// The code slots `s` of a signature at `sig` of an image from `image`, of `code` (the bytes
/// before the signature): `None` where they would hash bytes past it, or of a hash not known.
#[cfg(feature = "encode")]
fn hashes(code: &[u8], image: usize, sig: usize, s: &Slots) -> Option<Vec<u8>> {
    if image.checked_add(s.limit).is_none_or(|e| e > sig) {
        return None;
    }
    let page = if s.page == 0 { s.limit.max(1) } else { s.page };
    let mut out = alloc::vec![0; s.count * s.size];
    for (i, h) in out.chunks_mut(s.size).enumerate() {
        let (a, b) = (image + i * page, image + ((i + 1) * page).min(s.limit));
        if !digest(s.hash, &code[a..b], h) {
            return None;
        }
    }
    Some(out)
}

#[cfg(feature = "decode")]
fn varint(s: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for k in 0..10 {
        let (&x, rest) = s.split_first()?;
        *s = rest;
        v |= ((x & 0x7f) as u64) << (7 * k);
        if x < 0x80 {
            return Some(v);
        }
    }
    None
}

/// A signature of the params: where its image starts, where it lies in the file, how long it
/// is, and the mask of its blobs.
#[cfg(feature = "decode")]
struct Entry {
    image: u64,
    at: u64,
    n: usize,
    mask: u64,
}

#[cfg(feature = "decode")]
fn entry(p: &mut &[u8]) -> Option<Entry> {
    let (image, at, n, mask) = (varint(p)?, varint(p)?, varint(p)?, varint(p)?);
    Some(Entry { image, at, n: usize::try_from(n).ok()?, mask })
}

/// What the params tell of the signatures, checked: their count and the most of their bytes
/// and of the code directories at a time.
#[cfg(feature = "decode")]
struct Scan {
    count: u64,
    /// The params after the count.
    rest_at: usize,
    max_n: usize,
    max_jobs: usize,
}

/// The params of a file of `size` bytes, or `None` where they are not of one: the signatures
/// in the file's order, each within the file, its image starting at its predecessor's end or
/// after, and at its own start at the latest.
#[cfg(feature = "decode")]
fn scan(params: &[u8], size: u64) -> Option<Scan> {
    let mut p = params;
    let count = varint(&mut p)?;
    // (an entry 4 bytes of the params at least)
    if count > p.len() as u64 / 4 {
        return None;
    }
    let rest_at = params.len() - p.len();
    let (mut end, mut max_n, mut max_jobs) = (0u64, 0, 0);
    for _ in 0..count {
        let e = entry(&mut p)?;
        let last = e.at.checked_add(e.n as u64).filter(|&l| l <= size)?;
        if e.image < end || e.image > e.at {
            return None;
        }
        (end, max_n, max_jobs) = (last, max_n.max(e.n), max_jobs.max(e.mask.count_ones() as usize));
    }
    p.is_empty().then_some(Scan { count, rest_at, max_n, max_jobs })
}

/// A running digest of one page of a hash type of [`digest`].
#[cfg(feature = "decode")]
enum Hasher {
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    /// SHA-256 cut to 20 bytes.
    Sha256Cut(sha2::Sha256),
    Sha384(sha2::Sha384),
}

#[cfg(feature = "decode")]
impl Hasher {
    fn new(t: u8, size: usize) -> Option<Hasher> {
        use sha2::Digest;
        Some(match (t, size) {
            (1, 20) => Hasher::Sha1(Digest::new()),
            (2, 32) => Hasher::Sha256(Digest::new()),
            (3, 20) => Hasher::Sha256Cut(Digest::new()),
            (4, 48) => Hasher::Sha384(Digest::new()),
            _ => return None,
        })
    }

    fn update(&mut self, d: &[u8]) {
        use sha2::Digest;
        match self {
            Hasher::Sha1(h) => h.update(d),
            Hasher::Sha256(h) | Hasher::Sha256Cut(h) => h.update(d),
            Hasher::Sha384(h) => h.update(d),
        }
    }

    /// The digest into `out` (of its size), the hasher begun again.
    fn finish(&mut self, out: &mut [u8]) {
        use sha2::Digest;
        match self {
            Hasher::Sha1(h) => out.copy_from_slice(&h.finalize_reset()),
            Hasher::Sha256(h) => out.copy_from_slice(&h.finalize_reset()),
            Hasher::Sha256Cut(h) => out.copy_from_slice(&h.finalize_reset()[..20]),
            Hasher::Sha384(h) => out.copy_from_slice(&h.finalize_reset()),
        }
    }
}

/// The code slots of a CodeDirectory being computed as the file goes by: the pages of the
/// image from `image` for `limit` bytes hashed, their digests put into the signature's slots.
#[cfg(feature = "decode")]
struct Job {
    hasher: Hasher,
    /// The slots' offset in the signature, a digest's size and the page's.
    slots: usize,
    size: usize,
    page: usize,
    image: u64,
    limit: usize,
    /// Bytes of the image hashed and slots put.
    fed: usize,
    done: usize,
}

/// Bytes of a [`Job`] on WebAssembly.
#[cfg(feature = "decode")]
pub const JOB_BYTES: u64 = 272;
#[cfg(all(feature = "decode", target_family = "wasm"))]
const _: () = assert!(core::mem::size_of::<Job>() as u64 == JOB_BYTES);

#[cfg(feature = "decode")]
impl Job {
    /// The job of slots `s` of a signature at `at` of an image from `image`; `None` for a hash
    /// not known or an image past the signature.
    fn new(s: &Slots, image: u64, at: u64) -> Option<Job> {
        let hasher = Hasher::new(s.hash, s.size)?;
        if image.checked_add(s.limit as u64).is_none_or(|e| e > at) {
            return None;
        }
        let page = if s.page == 0 { s.limit.max(1) } else { s.page };
        Some(Job { hasher, slots: s.at, size: s.size, page, image, limit: s.limit, fed: 0, done: 0 })
    }

    /// The bytes of all its slots (the pages' count is that of the slots, as `slots` checked).
    fn range(&self) -> core::ops::Range<usize> {
        self.slots..self.slots + self.limit.div_ceil(self.page) * self.size
    }

    /// Hashes the part of `data`, which starts at `pos` of the file, that is in the image's
    /// code, and puts a page's digest into `sig` as it ends.
    fn feed(&mut self, pos: u64, data: &[u8], sig: &mut [u8]) {
        let lo = self.image.max(pos);
        let hi = (self.image + self.limit as u64).min(pos + data.len() as u64);
        if lo >= hi {
            return;
        }
        let mut d = &data[(lo - pos) as usize..(hi - pos) as usize];
        while !d.is_empty() {
            let k = d.len().min(self.page - self.fed % self.page);
            self.hasher.update(&d[..k]);
            self.fed += k;
            d = &d[k..];
            if self.fed.is_multiple_of(self.page) || self.fed == self.limit {
                let at = self.slots + self.done * self.size;
                self.hasher.finish(&mut sig[at..at + self.size]);
                self.done += 1;
            }
        }
    }

    /// Whether every page is hashed (the image's whole code gone by).
    fn complete(&self) -> bool {
        self.fed == self.limit
    }
}

/// Bytes of the body that [`decode_core`] takes at a time, and of the signatures.
#[cfg(feature = "decode")]
pub const BODY_PIECE: usize = 64 << 10;
#[cfg(feature = "decode")]
pub const SIG_PIECE: usize = 8 << 10;

/// The memory plan of [`decode_core`] ([`aqr_api::stack`]): what it takes beyond its initial
/// memory (`initial` bytes) at most, of params `params` and a file of `size` bytes: the params,
/// a piece of each input, the largest signature and the jobs of the most blobs. It does not
/// depend on the file's length but through its signatures'.
#[cfg(feature = "decode")]
pub fn part_plan(params: &[u8], size: u64, initial: u64) -> Option<u64> {
    let s = scan(params, size)?;
    let mut m = aqr_api::stack::Sim::new(initial);
    m.take(params.len() as u64);
    m.take(BODY_PIECE as u64);
    m.take(SIG_PIECE as u64);
    m.take(s.max_n as u64);
    m.take(s.max_jobs as u64 * JOB_BYTES);
    Some(m.plan())
}

/// The file of `size` bytes put to `sink` from `params`, `body` and `sigs` (both read in
/// order), or `None` where they are not of one. Every page is hashed as the file goes by (the
/// signature follows the code it hashes), the digests put into the signature just before it.
#[cfg(feature = "decode")]
pub fn decode_core(
    params: &[u8],
    body: &mut dyn aqr_api::stream::Pull,
    sigs: &mut dyn aqr_api::stream::Pull,
    size: u64,
    sink: &mut dyn aqr_api::stream::Put,
) -> Option<()> {
    use aqr_api::stream::Reader;
    let scan = scan(params, size)?;
    let mut body = Reader::new(body, BODY_PIECE);
    let mut sigs = Reader::new(sigs, SIG_PIECE);
    let mut buf = alloc::vec![0u8; scan.max_n];
    let mut jobs: Vec<Job> = Vec::with_capacity(scan.max_jobs);
    let mut p = &params[scan.rest_at..];
    let mut pos = 0u64;
    for _ in 0..scan.count {
        let e = entry(&mut p)?;
        let sig = &mut buf[..e.n];
        sigs.exact(sig)?;
        jobs.clear();
        for k in (0..MOST_BLOBS).filter(|&k| e.mask >> k & 1 != 0) {
            let s = slots(sig, k)?;
            let job = Job::new(&s, e.image, e.at)?;
            // (slots lying over each other would come out by the order of the pages)
            if jobs.iter().any(|j| j.range().start < job.range().end && job.range().start < j.range().end) {
                return None;
            }
            jobs.push(job);
        }
        let mut at = pos;
        body.each(e.at - pos, |d| {
            jobs.iter_mut().for_each(|j| j.feed(at, d, &mut buf));
            at += d.len() as u64;
            sink.put(d)
        })?;
        if !jobs.iter().all(|j| j.complete()) {
            return None;
        }
        sink.put(&buf[..e.n])?;
        pos = e.at + e.n as u64;
    }
    body.each(size - pos, |d| sink.put(d))?;
    (body.done() && sigs.done()).then_some(())
}

/// The file of `len` bytes from `params`, `body` and `sigs`, or `None` where they are not of one.
#[cfg(feature = "decode")]
pub fn decode(params: &[u8], body: &[u8], sigs: &[u8], len: usize) -> Option<Vec<u8>> {
    use aqr_api::stream::Slice;
    if body.len().checked_add(sigs.len())? != len {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    decode_core(params, &mut Slice(body), &mut Slice(sigs), len as u64, &mut out)?;
    Some(out)
}

/// A file's signatures, of [`encode`]: the params, the signatures (code slots zeroed) and where
/// they are in the file, in order.
#[cfg(feature = "encode")]
pub struct Signed {
    pub params: Vec<u8>,
    pub sigs: Vec<u8>,
    pub ranges: Vec<core::ops::Range<usize>>,
}

/// The signatures of `d` among `signatures` (of each, the offset of its image and its own
/// offset and length in `d`, as `format::macho` finds them) whose code slots are the hashes of
/// their images' pages; `None` where none is.
#[cfg(feature = "encode")]
pub fn encode(d: &[u8], signatures: &[(usize, usize, usize)]) -> Option<Signed> {
    let mut signatures: Vec<_> = signatures.to_vec();
    signatures.sort_by_key(|s| s.1);
    let mut found = Vec::new();
    let mut end = 0;
    for (image, at, n) in signatures {
        // (the decoder reads a signature before its image starts, so after the last one's end)
        if at < end || image < end || image > at || at.checked_add(n).is_none_or(|e| e > d.len()) {
            continue;
        }
        let sig = &d[at..at + n];
        let count = be32(sig, 8).filter(|_| be32(sig, 0) == Some(EMBEDDED_SIGNATURE)).unwrap_or(0) as usize;
        let mut zeroed = sig.to_vec();
        let mut mask = 0u64;
        let mut found_slots: Vec<(usize, Slots)> = Vec::new();
        for k in 0..count.min(MOST_BLOBS) {
            let Some(s) = slots(sig, k) else { continue };
            let slots = s.at..s.at + s.count * s.size;
            // (slots lying over each other have no one order of the pages)
            let apart = found_slots.iter().all(|(_, o)| o.at + o.count * o.size <= slots.start || slots.end <= o.at);
            if apart && hashes(&d[..at], image, at, &s).is_some_and(|h| h == sig[slots.clone()]) {
                mask |= 1 << k;
                zeroed[slots].fill(0);
                found_slots.push((k, s));
            }
        }
        // (the blobs read again from the signature zeroed, as the decoder does)
        if found_slots.iter().any(|(k, s)| slots(&zeroed, *k).as_ref() != Some(s)) {
            continue;
        }
        if mask != 0 {
            found.push((image, at, n, mask, zeroed));
            end = at + n;
        }
    }
    if found.is_empty() {
        return None;
    }
    let mut params = Vec::new();
    let put = |out: &mut Vec<u8>, mut v: u64| {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    };
    put(&mut params, found.len() as u64);
    let (mut sigs, mut ranges) = (Vec::new(), Vec::new());
    for (image, at, n, mask, zeroed) in found {
        [image as u64, at as u64, n as u64, mask].into_iter().for_each(|v| put(&mut params, v));
        sigs.extend_from_slice(&zeroed);
        ranges.push(at..at + n);
    }
    Some(Signed { params, sigs, ranges })
}

#[cfg(all(test, feature = "encode", feature = "decode"))]
mod tests {
    use super::*;
    use alloc::vec;

    /// A CodeDirectory of `hash` (type, size) over `code` in pages of 2^`bits` bytes (one page
    /// for 0), its slots those hashes (or not, where `wrong`).
    fn code_directory(code: &[u8], hash: (u8, usize), bits: u8, wrong: bool) -> Vec<u8> {
        let page = if bits == 0 { code.len() } else { 1usize << bits };
        let n = code.len().div_ceil(page);
        let (hash_offset, ident) = (52usize, b"test\0");
        let len = hash_offset + ident.len() + n * hash.1;
        let mut cd = vec![0u8; 44];
        cd[0..4].copy_from_slice(&CODE_DIRECTORY.to_be_bytes());
        cd[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        cd[8..12].copy_from_slice(&0x20100u32.to_be_bytes());
        cd[16..20].copy_from_slice(&((hash_offset + ident.len()) as u32).to_be_bytes());
        cd[20..24].copy_from_slice(&(hash_offset as u32).to_be_bytes());
        cd[28..32].copy_from_slice(&(n as u32).to_be_bytes());
        cd[32..36].copy_from_slice(&(code.len() as u32).to_be_bytes());
        (cd[36], cd[37], cd[39]) = (hash.1 as u8, hash.0, bits);
        cd.resize(hash_offset, 0);
        cd.extend_from_slice(ident);
        for p in code.chunks(page) {
            let mut h = vec![0u8; hash.1];
            assert!(digest(hash.0, p, &mut h));
            h[0] ^= wrong as u8;
            cd.extend_from_slice(&h);
        }
        cd
    }

    /// An image of `code` and its signature after it, of the blobs given.
    fn signed(code: &[u8], blobs: &[Vec<u8>]) -> (Vec<u8>, (usize, usize, usize)) {
        let mut sig = vec![0u8; 12 + 8 * blobs.len()];
        sig[0..4].copy_from_slice(&EMBEDDED_SIGNATURE.to_be_bytes());
        sig[8..12].copy_from_slice(&(blobs.len() as u32).to_be_bytes());
        for (k, b) in blobs.iter().enumerate() {
            let at = sig.len() as u32;
            sig[12 + 8 * k..16 + 8 * k].copy_from_slice(&(k as u32).to_be_bytes());
            sig[16 + 8 * k..20 + 8 * k].copy_from_slice(&at.to_be_bytes());
            sig.extend_from_slice(b);
        }
        let n = sig.len();
        sig[4..8].copy_from_slice(&(n as u32).to_be_bytes());
        ([code, &sig].concat(), (0, code.len(), n))
    }

    fn body(d: &[u8], s: &Signed) -> Vec<u8> {
        let (mut out, mut at) = (Vec::new(), 0);
        for r in &s.ranges {
            out.extend_from_slice(&d[at..r.start]);
            at = r.end;
        }
        out.extend_from_slice(&d[at..]);
        out
    }

    #[test]
    fn round_trip() {
        let code: Vec<u8> = (0..10_000u32).map(|i| ((i * 7919) >> 3) as u8).collect();
        let requirements = vec![0xfa, 0xde, 0x0c, 0x01, 0, 0, 0, 12, 0, 0, 0, 0];
        let blobs = [
            code_directory(&code, (2, 32), 12, false),
            requirements,
            code_directory(&code, (1, 20), 12, false),
            code_directory(&code, (2, 32), 0, false),
            code_directory(&code, (3, 20), 14, true),
        ];
        let (d, sig) = signed(&code, &blobs);
        let s = encode(&d, &[sig]).unwrap();
        // (all but the requirements and the slots not of the pages)
        let n = d.len() - code.len();
        assert_eq!(s.params, [1, 0, 0x90, 0x4e, n as u8 | 0x80, (n >> 7) as u8, 0b01101]);
        assert_eq!(s.sigs.len(), d.len() - code.len());
        assert_eq!(decode(&s.params, &body(&d, &s), &s.sigs, d.len()).as_deref(), Some(&d[..]));
        // a signature of no slots of the pages, none
        let (d, sig) = signed(&code, &[code_directory(&code, (2, 32), 12, true)]);
        assert!(encode(&d, &[sig]).is_none());
    }

    #[test]
    fn bad_params() {
        let code = vec![7u8; 5000];
        let (d, sig) = signed(&code, &[code_directory(&code, (2, 32), 12, false)]);
        let s = encode(&d, &[sig]).unwrap();
        let b = body(&d, &s);
        assert!(decode(&s.params, &b, &s.sigs, d.len() + 1).is_none());
        assert!(decode(&s.params[..s.params.len() - 1], &b, &s.sigs, d.len()).is_none());
        let mut p = s.params.clone();
        *p.last_mut().unwrap() = 2; // a blob that is no CodeDirectory
        assert!(decode(&p, &b, &s.sigs, d.len()).is_none());
        let mut p = s.params.clone();
        p[1] = 1; // the image from 1: past the signature
        assert!(decode(&p, &b, &s.sigs, d.len()).is_none());
    }

    /// Two images (a fat file's) of code and a signature each, and bytes after them, the pages
    /// of different sizes and hashes, over several pieces of the body.
    #[test]
    fn two_images_over_pieces() {
        let noise =
            |n: usize, seed: u32| -> Vec<u8> { (0..n as u32).map(|i| (i.wrapping_mul(seed) >> 5) as u8).collect() };
        let (a, b) = (noise(200_001, 2654435761), noise(150_000, 40503));
        let blobs_a = [code_directory(&a, (2, 32), 14, false), code_directory(&a, (1, 20), 12, false)];
        let (d, sig_a) = signed(&a, &blobs_a);
        let image_b = d.len() + 100;
        let mut d = d;
        d.resize(image_b, 0xaa);
        let (db, sig_b) = signed(&b, &[code_directory(&b, (4, 48), 12, false), code_directory(&b, (3, 20), 0, false)]);
        d.extend_from_slice(&db);
        d.extend_from_slice(&[7; 1000]);
        let sigs = [sig_a, (image_b, image_b + sig_b.1, sig_b.2)];
        let s = encode(&d, &sigs).unwrap();
        assert_eq!(s.ranges.len(), 2);
        let body = body(&d, &s);
        assert_eq!(decode(&s.params, &body, &s.sigs, d.len()).as_deref(), Some(&d[..]));
        // through pieces of every size the sink could be given
        let mut out = Vec::new();
        decode_core(
            &s.params,
            &mut aqr_api::stream::Slice(&body),
            &mut aqr_api::stream::Slice(&s.sigs),
            d.len() as u64,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, d);
        // the plan holds the largest signature and does not depend on the body
        let n = sig_a.2.max(sig_b.2) as u64;
        assert!(part_plan(&s.params, d.len() as u64, 0).unwrap() < 1 << 20);
        assert!(part_plan(&s.params, d.len() as u64, 0).unwrap() >= n);
        // cut, extended, damaged
        assert!(decode(&s.params, &body[1..], &s.sigs, d.len()).is_none());
        assert!(decode(&s.params, &body, &s.sigs[1..], d.len()).is_none());
        assert!(decode(&s.params, &body[..body.len() - 1], &s.sigs, d.len()).is_none());
        // (a byte more of the body at the end is a longer file, an extra of the signatures is none)
        assert!(decode(&s.params, &[&body[..], &[0][..]].concat(), &s.sigs, d.len() + 1).is_some());
        assert!(decode(&s.params, &[&body[..], &[0][..]].concat(), &s.sigs, d.len()).is_none());
        assert!(decode(&s.params, &body, &[&s.sigs[..], &[0][..]].concat(), d.len() + 1).is_none());
        for cut in [1, 5, s.params.len() - 1] {
            assert!(decode(&s.params[..cut], &body, &s.sigs, d.len()).is_none());
        }
        // the second image starting before the first signature's end: not what the encoder makes
        let put = |out: &mut Vec<u8>, mut v: u64| {
            while v >= 0x80 {
                out.push(v as u8 | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
        };
        let mut p = Vec::new();
        put(&mut p, 2);
        for (image, at, n, mask) in [(0, sigs[0].1, sigs[0].2, 3), (sigs[0].1 as u64 + 1, sigs[1].1, sigs[1].2, 3)] {
            [image as u64, at as u64, n as u64, mask].into_iter().for_each(|v| put(&mut p, v));
        }
        assert!(decode(&p, &body, &s.sigs, d.len()).is_none());
    }

    /// The code slots of two blobs lying over each other are not computed by the encoder.
    #[test]
    fn overlapping_slots_stay() {
        let code = vec![9u8; 9000];
        let cd = code_directory(&code, (2, 32), 12, false);
        // (the second blob the same bytes as the first: its slots at the same place)
        let mut sig = vec![0u8; 12 + 16];
        sig[0..4].copy_from_slice(&EMBEDDED_SIGNATURE.to_be_bytes());
        sig[8..12].copy_from_slice(&2u32.to_be_bytes());
        for k in 0..2 {
            sig[16 + 8 * k..20 + 8 * k].copy_from_slice(&28u32.to_be_bytes());
        }
        sig.extend_from_slice(&cd);
        let n = sig.len();
        sig[4..8].copy_from_slice(&(n as u32).to_be_bytes());
        let d = [&code[..], &sig].concat();
        let s = encode(&d, &[(0, code.len(), n)]).unwrap();
        assert_eq!(s.params.last(), Some(&1)); // blob 0 alone
        assert_eq!(decode(&s.params, &body(&d, &s), &s.sigs, d.len()).as_deref(), Some(&d[..]));
    }
}
