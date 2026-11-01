//! NSIS installers (Nullsoft): the data after the stub's image, its overlay, starts with a first
//! header (flags, `0xDEADBEEF`, `NullsoftInst`, the sizes of the header and of the data), then,
//! unless it is solid, blocks of a 4-byte size each, the installer's header first: a block of
//! the size's top bit set is compressed by the installer's compressor (zlib, bzip2 or LZMA of
//! NSIS's own), else stored as it is. A CRC-32 of the file may end the data. The stored blocks
//! are the files installed where the installer was made without compression (`SetCompress
//! off`) or did not compress them: tarballs and other archives, mostly, which are taken as
//! files of their formats ([`crate::exe`]), and whose contents [`sums`] tells
//! (`archive::together`).
//!
//! The compressed blocks and solid data are kept as they are: NSIS's compressors are not
//! reproduced (10gb's installers hold some tens of KB of LZMA).

use std::ops::Range;

use crate::detect::{Format, detect};

/// The first header's signature, after its flags.
const SIGNATURE: &[u8; 16] = b"\xef\xbe\xad\xdeNullsoftInst";

/// Bytes of the first header.
pub(crate) const FIRST_HEADER: usize = 28;

/// The size of an installer's data (its first header's on), if `h` starts with a first header.
pub(crate) fn data_len(h: &[u8]) -> Option<usize> {
    let h = h.get(..FIRST_HEADER)?;
    (&h[4..20] == SIGNATURE).then(|| u32::from_le_bytes(h[24..28].try_into().expect("4 bytes")) as usize)
}

/// The stored blocks of the installer whose data starts at `at` in `d` (its first header), but
/// the installer's header: of the walk over its blocks ending at the end of its data (or 4
/// bytes before it, its CRC-32), else None (solid data, or not an installer).
pub(crate) fn stored(d: &[u8], at: usize) -> Option<Vec<Range<usize>>> {
    let end = at.checked_add(data_len(d.get(at..)?)?).filter(|&e| e <= d.len())?;
    let (mut p, mut blocks) = (at.checked_add(FIRST_HEADER)?, Vec::new());
    let mut header = true;
    // (4 bytes left after a block are the CRC-32)
    while p < end && (header || end - p != 4) {
        let word = u32::from_le_bytes(d.get(p..p + 4).filter(|_| end - p >= 4)?.try_into().expect("4 bytes"));
        let len = (word & 0x7fff_ffff) as usize;
        let body = p + 4..(p + 4).checked_add(len).filter(|&e| e <= end)?;
        if word >> 31 == 0 && !header {
            blocks.push(body.clone());
        }
        (p, header) = (body.end, false);
    }
    (!header).then_some(blocks)
}

/// [`stored`] of the PE image `d`, an installer's stub, its first header at its overlay.
pub(crate) fn in_pe(d: &[u8]) -> Option<Vec<Range<usize>>> {
    stored(d, usize::try_from(crate::extent::pe_overlay(d)?).ok()?)
}

/// Stored blocks shorter than this are left with the rest of the image.
pub(crate) const MIN_BLOCK: usize = 4 << 10;

/// The format a stored block is taken as, if any: those that detect at its start.
pub(crate) fn format(b: &[u8]) -> Option<Format> {
    detect(b).filter(|f| !matches!(f, Format::Text | Format::Sized | Format::Records))
}

/// Of the stored blocks `blocks` of `d` (of at least [`MIN_BLOCK`] bytes): the CRC-32 and size
/// of each, and of what it holds as `archive::together` takes those of files: the members of
/// a tarball (gzip, bzip2, `.lzma` or `.xz`, or none), the entries of a ZIP archive.
pub(crate) fn sums(d: &[u8], blocks: &[Range<usize>], threads: usize) -> Vec<(u32, u64)> {
    let blocks: Vec<&[u8]> = blocks.iter().map(|b| &d[b.clone()]).filter(|b| b.len() >= MIN_BLOCK).collect();
    let each = aqr_common::pool::par_map(blocks.len(), threads, |i| {
        let b = blocks[i];
        let mut sums = vec![(crc32fast::hash(b), b.len() as u64)];
        let data = match format(b) {
            Some(Format::Gzip) => crate::flate::gunzip(b, DATA_MAX),
            Some(Format::Bzip2) => aqr_bz2::stream::decode(b, true, DATA_MAX).ok().map(|s| s.data),
            Some(Format::Lzma) => aqr_lzma::formula::data(b),
            Some(Format::Xz) => aqr_lzma::formula::data_xz(b),
            Some(Format::Zip) => {
                sums.extend(aqr_zip::zip::entry_sums(b));
                None
            }
            Some(f) if f.is_bundle() => Some(b.to_vec()),
            _ => None,
        };
        sums.extend(crate::archive::stream_sums(data));
        sums
    });
    each.into_iter().flatten().collect()
}

/// The most bytes a block is decoded to for [`sums`].
const DATA_MAX: usize = 1 << 30;

#[cfg(test)]
mod tests {
    use super::*;

    /// An installer's data at 0: its first header, the header block, then `blocks` (stored
    /// or compressed), and a CRC-32.
    fn installer(blocks: &[(&[u8], bool)]) -> Vec<u8> {
        let mut d = vec![0; 4];
        d.extend_from_slice(SIGNATURE);
        d.extend_from_slice(&[0; 8]);
        for (b, compressed) in [(&b"header"[..], true)].iter().chain(blocks) {
            d.extend_from_slice(&(b.len() as u32 | (*compressed as u32) << 31).to_le_bytes());
            d.extend_from_slice(b);
        }
        d.extend_from_slice(&[1, 2, 3, 4]);
        let len = d.len() as u32;
        d[24..28].copy_from_slice(&len.to_le_bytes());
        d
    }

    #[test]
    fn stored_blocks() {
        let d = installer(&[(b"abc", false), (b"xyzw", true), (b"", false), (b"0123456789", false)]);
        let got: Vec<&[u8]> = stored(&d, 0).unwrap().into_iter().map(|r| &d[r]).collect();
        assert_eq!(got, [&b"abc"[..], b"", b"0123456789"]);
        // (without its CRC-32)
        let mut e = d[..d.len() - 4].to_vec();
        let len = e.len() as u32;
        e[24..28].copy_from_slice(&len.to_le_bytes());
        assert_eq!(stored(&e, 0).unwrap().len(), 3);
        // (a block past the data: solid data, whose first word is no size)
        let mut f = d.clone();
        f[FIRST_HEADER..FIRST_HEADER + 4].copy_from_slice(&0x4000_005du32.to_le_bytes());
        assert_eq!(stored(&f, 0), None);
        let mut g = d.clone();
        g[4] ^= 1;
        assert_eq!(stored(&g, 0), None);
    }
}
