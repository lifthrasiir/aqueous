//! Compression and decompression of whole FLAC files.
//!
//! Residuals go through [`crate::residual`] into one rANS stream and two raw bit streams
//! (by channel parity). Everything else (the bytes before the first frame, junk between
//! frames, frame headers, subframe parameters) goes into small byte streams compressed
//! with zstd.

use crate::bits;
use crate::flac::{self, ESCAPE, Frame};
pub use crate::residual::{Coded, Error, Rd, freqs_to_bytes, layout_id, put_varint, tables};
use crate::residual::{ResCounter, ResDecoder, ResEncoder, get_subframes, put_subframes};

/// Byte streams.
pub const S_PREFIX: usize = 0;
pub const S_LAYOUT: usize = 1;
pub const S_JUNK: usize = 2;
pub const S_HDR: usize = 3;
pub const S_SUB: usize = 4;
pub const S_WARM: usize = 5;
pub const N_BYTE_STREAMS: usize = 6;
pub const STREAM_NAMES: [&str; N_BYTE_STREAMS + 3] =
    ["prefix", "layout", "junk", "headers", "subframes", "warmup", "rans", "raw0", "raw1"];

/// The coded number of a header (bytes from offset 4) and its length.
fn coded_number(h: &[u8]) -> (u64, usize) {
    let b = h[4];
    let n = (b.leading_ones() as usize).max(1);
    let mut v = if n == 1 { b as u64 } else { (b & (0x7f >> n)) as u64 };
    for &c in &h[5..4 + n] {
        v = v << 6 | (c & 0x3f) as u64;
    }
    (v, n)
}

fn put_coded_number(out: &mut Vec<u8>, v: u64) {
    if v < 0x80 {
        out.push(v as u8);
        return;
    }
    let mut n = 2;
    while n < 7 && v >= 1u64 << (5 * n + 1) {
        n += 1;
    }
    let lead = if n == 7 { 0xfe } else { (0xff00u16 >> n) as u8 };
    out.push(lead | (v >> (6 * (n - 1))) as u8);
    for i in (0..n - 1).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3f) as u8);
    }
}

/// The header expected after `prev` (same fields, next frame / sample number).
fn predict_header(prev: &[u8], prev_block: usize, out: &mut Vec<u8>) {
    out.clear();
    if prev.len() < 5 {
        return;
    }
    let (v, n) = coded_number(prev);
    let step = if prev[1] & 1 != 0 { prev_block as u64 } else { 1 };
    out.extend_from_slice(&prev[..4]);
    put_coded_number(out, v + step);
    out.extend_from_slice(&prev[4 + n..]);
}

/// Encodes a FLAC file; `None` if it has no FLAC stream.
pub fn encode(input: &[u8]) -> Option<Coded> {
    let (si, start) = flac::parse_metadata(input)?;
    let pd = bits::padded(input);
    let mut s: Vec<Vec<u8>> = vec![Vec::new(); N_BYTE_STREAMS];
    s[S_PREFIX].extend_from_slice(&input[..start]);
    let mut re = ResEncoder::new(input.len());
    let mut f = Frame::default();
    let mut prev_hdr: Vec<u8> = Vec::new();
    let mut prev_block = 0;
    let mut pred = Vec::new();
    let mut p = start;
    let mut run = 0u64;
    while p < input.len() {
        let Some(e) = flac::parse_frame(&pd, p, input.len(), si.bps, &mut f) else {
            // Junk: up to the next position where a frame parses.
            let mut q = p + 1;
            loop {
                match memchr::memchr(0xff, &input[q.min(input.len())..]) {
                    Some(i) => q += i,
                    None => {
                        q = input.len();
                        break;
                    }
                }
                let mut g = Frame::default();
                if flac::parse_frame(&pd, q, input.len(), si.bps, &mut g).is_some() {
                    break;
                }
                q += 1;
            }
            put_varint(&mut s[S_LAYOUT], run);
            put_varint(&mut s[S_LAYOUT], (q - p) as u64);
            s[S_JUNK].extend_from_slice(&input[p..q]);
            run = 0;
            p = q;
            continue;
        };
        run += 1;
        p = e;
        // Header.
        predict_header(&prev_hdr, prev_block, &mut pred);
        if pred == f.header {
            s[S_HDR].push(0);
        } else {
            s[S_HDR].push(f.header.len() as u8);
            s[S_HDR].extend_from_slice(&f.header);
        }
        prev_hdr.clone_from(&f.header);
        prev_block = f.block_size;
        let [sub, warm] = s.get_disjoint_mut([S_SUB, S_WARM]).unwrap();
        put_subframes(&f, sub, warm, true);
        re.frame(&mut f);
    }
    put_varint(&mut s[S_LAYOUT], run);
    let (words, raw) = re.finish();
    Some(Coded { streams: s, words, raw })
}

/// Decodes into `out` (appending).
pub fn decode(c: &Coded, total_len: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    if c.streams.len() != N_BYTE_STREAMS {
        return Err(Error("stream count"));
    }
    out.reserve(total_len);
    let prefix = &c.streams[S_PREFIX];
    out.extend_from_slice(prefix);
    let (si, start) = flac::parse_metadata(prefix).ok_or(Error("metadata"))?;
    if start != prefix.len() {
        return Err(Error("metadata length"));
    }
    let mut layout = Rd::new(&c.streams[S_LAYOUT]);
    let mut junk = Rd::new(&c.streams[S_JUNK]);
    let mut hdr = Rd::new(&c.streams[S_HDR]);
    let mut sub = Rd::new(&c.streams[S_SUB]);
    let mut warm = Rd::new(&c.streams[S_WARM]);
    let raw_buf = [bits::padded(&c.raw[0]), bits::padded(&c.raw[1])];
    let mut rd = ResDecoder::new(&c.words, &raw_buf)?;
    let mut f = Frame::default();
    let mut prev_hdr: Vec<u8> = Vec::new();
    let mut prev_block = 0;
    let mut hbuf = Vec::new();
    loop {
        let run = layout.varint()?;
        for _ in 0..run {
            if out.len() > total_len {
                return Err(Error("too long"));
            }
            // Header.
            let hl = hdr.byte()? as usize;
            if hl == 0 {
                predict_header(&prev_hdr, prev_block, &mut hbuf);
            } else {
                hbuf.clear();
                hbuf.extend_from_slice(hdr.bytes(hl)?);
            }
            hbuf.push(flac::crc8(&hbuf));
            let (hlen, block, assign, bps) = flac::parse_header(&hbuf, 0, si.bps).ok_or(Error("bad header"))?;
            hbuf.pop();
            debug_assert_eq!(hlen, hbuf.len());
            f.header.clone_from(&hbuf);
            f.block_size = block;
            f.assign = assign;
            f.channels = if assign < 8 { assign as usize + 1 } else { 2 };
            f.bps = bps;
            prev_hdr.clone_from(&hbuf);
            prev_block = block;
            get_subframes(&mut f, &mut sub, &mut warm, true)?;
            rd.frame(&mut f)?;
            // Escaped partitions must hold their values.
            for ch in 0..f.channels {
                let sf = &f.sub[ch];
                if !sf.has_residual() || !sf.params.iter().any(|&k| k & ESCAPE != 0) {
                    continue;
                }
                let ps = block >> sf.porder;
                let mut at = 0;
                for (i, &k) in sf.params.iter().enumerate() {
                    let n = ps - if i == 0 { sf.order as usize } else { 0 };
                    if k & ESCAPE != 0 {
                        let nb = (k & 31) as u32;
                        for &v in &f.res[ch][at..at + n] {
                            let ok = if nb == 0 {
                                v == 0
                            } else {
                                nb >= 32 || (v >> (nb - 1)) == 0 || (v >> (nb - 1)) == -1
                            };
                            if !ok {
                                return Err(Error("escaped residual range"));
                            }
                        }
                    }
                    at += n;
                }
            }
            if !flac::write_frame(&f, out, total_len.saturating_sub(out.len())) {
                return Err(Error("too long"));
            }
        }
        if layout.done() {
            break;
        }
        let n = layout.varint()? as usize;
        out.extend_from_slice(junk.bytes(n)?);
    }
    rd.finish()
}

/// Adds the (context, token) counts of a file's residuals to `counts` (for training).
pub fn count(input: &[u8], counts: &mut [Vec<u64>]) {
    let Some((si, start)) = flac::parse_metadata(input) else { return };
    let pd = bits::padded(input);
    let mut rc = ResCounter::default();
    let mut f = Frame::default();
    let mut p = start;
    while p < input.len() {
        let Some(e) = flac::parse_frame(&pd, p, input.len(), si.bps, &mut f) else {
            p += 1;
            continue;
        };
        p = e;
        rc.frame(&mut f, counts);
    }
}
