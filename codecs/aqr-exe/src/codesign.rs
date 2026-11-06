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

/// The file of `len` bytes from `params`, `body` and `sigs`, or `None` where they are not of one.
#[cfg(feature = "decode")]
pub fn decode(params: &[u8], body: &[u8], sigs: &[u8], len: usize) -> Option<Vec<u8>> {
    let mut p = params;
    let n = varint(&mut p)?;
    // (an entry 4 bytes of the params at least)
    if n > p.len() as u64 / 4 {
        return None;
    }
    let mut list = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut v = [0usize; 3];
        for x in &mut v {
            *x = usize::try_from(varint(&mut p)?).ok()?;
        }
        list.push((v[0], v[1], v[2], varint(&mut p)?));
    }
    if !p.is_empty() || body.len().checked_add(sigs.len())? != len {
        return None;
    }
    // the file: the body with the signatures in their places, in order
    let mut out = Vec::with_capacity(len);
    let (mut b, mut s) = (0usize, 0usize);
    for &(_, at, n, _) in &list {
        let take = at.checked_sub(out.len())?;
        out.extend_from_slice(body.get(b..b.checked_add(take)?)?);
        out.extend_from_slice(sigs.get(s..s.checked_add(n)?)?);
        (b, s) = (b + take, s + n);
    }
    out.extend_from_slice(&body[b..]);
    if s != sigs.len() {
        return None;
    }
    for &(image, at, n, mask) in &list {
        for k in (0..MOST_BLOBS).filter(|&k| mask >> k & 1 != 0) {
            let s = slots(&out[at..at + n], k)?;
            let h = hashes(&out[..at], image, at, &s)?;
            out[at + s.at..at + s.at + h.len()].copy_from_slice(&h);
        }
    }
    Some(out)
}

/// Bytes that [`decode`] takes of [`aqr_api::stack`], of `params` and the signatures `sigs` and
/// a file of `len` bytes (`None` where they are not of one), in its blocks' order: the
/// signatures' entries, the file, and the code slots of a signature at a time (the most of them).
#[cfg(feature = "decode")]
pub fn decode_plan(params: &[u8], sigs: &[u8], len: u64) -> Option<u64> {
    use aqr_api::stack::block;
    let mut p = params;
    let n = varint(&mut p)?;
    if n > p.len() as u64 / 4 {
        return None;
    }
    let (mut s, mut most) = (0usize, 0);
    for _ in 0..n {
        let mut v = [0usize; 3];
        for x in &mut v {
            *x = usize::try_from(varint(&mut p)?).ok()?;
        }
        let mask = varint(&mut p)?;
        let sig = sigs.get(s..s.checked_add(v[2])?)?;
        for k in (0..MOST_BLOBS).filter(|&k| mask >> k & 1 != 0) {
            let sl = slots(sig, k)?;
            most = most.max(sl.count * sl.size);
        }
        s += v[2];
    }
    Some(block(ENTRY_BYTES * n) + block(len) + block(most as u64))
}

/// Bytes of an entry of [`decode`]'s list on WebAssembly.
#[cfg(feature = "decode")]
const ENTRY_BYTES: u64 = 24;
#[cfg(all(feature = "decode", target_family = "wasm"))]
const _: () = assert!(core::mem::size_of::<(usize, usize, usize, u64)>() as u64 == ENTRY_BYTES);

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
        if at < end || at.checked_add(n).is_none_or(|e| e > d.len()) {
            continue;
        }
        let sig = &d[at..at + n];
        let count = be32(sig, 8).filter(|_| be32(sig, 0) == Some(EMBEDDED_SIGNATURE)).unwrap_or(0) as usize;
        let mut zeroed = sig.to_vec();
        let mut mask = 0u64;
        for k in 0..count.min(MOST_BLOBS) {
            let Some(s) = slots(sig, k) else { continue };
            let slots = s.at..s.at + s.count * s.size;
            if hashes(&d[..at], image, at, &s).is_some_and(|h| h == sig[slots.clone()]) {
                mask |= 1 << k;
                zeroed[slots].fill(0);
            }
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
}
