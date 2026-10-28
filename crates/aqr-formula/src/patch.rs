//! Patches (spec, "Patch instruction"): Zstandard blocks without a frame, decompressed with an
//! earlier byte sequence, the *source*, as a raw content dictionary, as `zstd --patch-from`
//! does. A match may reach any byte of the source or of the output so far.

use zstd::zstd_safe::{CCtx, CParameter, DCtx, DParameter, InBuffer, OutBuffer};

use crate::{Error, Result};

/// The magic number and the frame header descriptor of the frame put around the blocks to
/// decompress them: a single segment, its content size in 8 bytes, no checksum and no
/// dictionary ID.
const FRAME: [u8; 5] = [0x28, 0xb5, 0x2f, 0xfd, 0xe0];

/// The patch from `source` to `target`, at zstd level `level`.
pub fn make(source: &[u8], target: &[u8], level: i32) -> Result<Vec<u8>> {
    let fail = |e| Error::new(format!("patch: {}", zstd::zstd_safe::get_error_name(e)));
    let mut c = CCtx::create();
    // A window over the source and the target, matched from afar where it is long.
    let span = (source.len() + target.len()).max(1) as u64;
    let window = (64 - (span - 1).leading_zeros()).clamp(10, 31);
    let params = [
        CParameter::CompressionLevel(level),
        CParameter::WindowLog(window),
        CParameter::EnableLongDistanceMatching(source.len() > 1 << 20),
        CParameter::ChecksumFlag(false),
        CParameter::ContentSizeFlag(true),
    ];
    params.into_iter().try_for_each(|p| c.set_parameter(p).map(drop)).map_err(fail)?;
    c.ref_prefix(source).map_err(fail)?;
    let mut frame = Vec::with_capacity(zstd::zstd_safe::compress_bound(target.len()));
    c.compress2(&mut frame, target).map_err(fail)?;
    // the blocks, past the frame header (and with no checksum after them)
    Ok(frame.split_off(header_len(frame[4])))
}

/// Bytes of a frame header of descriptor `fhd` (RFC 8878 §3.1.1.1), the magic number included.
fn header_len(fhd: u8) -> usize {
    let single = fhd & 0x20 != 0;
    let window = if single { 0 } else { 1 };
    let dict = [0, 1, 2, 4][(fhd & 3) as usize];
    let size = [if single { 1 } else { 0 }, 2, 4, 8][(fhd >> 6) as usize];
    4 + 1 + window + dict + size
}

/// The output of `patch`, of `len` bytes, from `source`. Fails where `patch` is not one or
/// more Zstandard blocks the last of which is the final one, or does not give `len` bytes.
pub fn apply(source: &[u8], patch: &[u8], len: u64) -> Result<Vec<u8>> {
    let fail = |e| Error::new(format!("patch: {}", zstd::zstd_safe::get_error_name(e)));
    let n = usize::try_from(len).map_err(|_| Error::new("patch: too large"))?;
    let mut frame = Vec::with_capacity(FRAME.len() + 8 + patch.len());
    frame.extend_from_slice(&FRAME);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(patch);
    let mut d = DCtx::create();
    d.set_parameter(DParameter::WindowLogMax(31)).map_err(fail)?;
    d.ref_prefix(source).map_err(fail)?;
    let mut out = vec![0u8; n];
    let (mut input, mut output) = (InBuffer::around(&frame), OutBuffer::around(&mut out[..]));
    let left = d.decompress_stream(&mut output, &mut input).map_err(fail)?;
    // all of it, to the end of the frame, and nothing beyond
    if left != 0 || input.pos() != frame.len() || output.pos() != n {
        return Err(Error::new("patch: not blocks of the size given"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Patches give their targets; one of the target is smaller than the target alone, also
    /// from afar; empty sources and targets; patches cut, extended or given the wrong length
    /// fail.
    #[test]
    fn round_trip() {
        let noise = aqr_common::rng::noise(300_000);
        let mut target = noise.clone();
        target[1000..1100].fill(7);
        target.extend_from_slice(b"and some more");
        for (source, target) in [(&noise[..], &target[..]), (b"", b"abc"), (b"abc", b""), (b"", b"")] {
            let p = make(source, target, 3).unwrap();
            assert_eq!(apply(source, &p, target.len() as u64).unwrap(), target);
        }
        let p = make(&noise, &target, 3).unwrap();
        assert!(p.len() < 1000, "{}", p.len());
        // from afar: a long source
        let far = [&aqr_common::rng::noise(3 << 20)[..], &noise].concat();
        let p = make(&far, &target, 3).unwrap();
        assert!(p.len() < 4000, "{}", p.len());
        assert_eq!(apply(&far, &p, target.len() as u64).unwrap(), target);
        let p = make(&noise, &target, 3).unwrap();
        let n = target.len() as u64;
        assert!(apply(&noise, &p[..p.len() - 1], n).is_err());
        assert!(apply(&noise, &[&p[..], &[0]].concat(), n).is_err());
        assert!(apply(&noise, &p, n - 1).is_err());
        assert!(apply(&noise, &p, n + 1).is_err());
        assert!(apply(&noise, &[], n).is_err());
    }
}
