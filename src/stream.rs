//! File-level decomposition: junk and frames, and the placement of main data in the
//! reservoir.
//!
//! The main data of all frames (everything after the side information) is concatenated
//! into one reservoir buffer `R`. A frame's main data starts `main_data_begin` bytes before
//! its own payload in `R` and spans the sum of its `part2_3_length`s. A frame is *placed*
//! when that span starts at or after the end of the previous placed frame's span and ends
//! within its own payload; only placed frames have their granules decoded. All other bits
//! of `R` are gap bits, stored verbatim. Whether a frame is placed depends only on side
//! information, so the reconstruction makes the same decisions.

use crate::frame::{Header, SideInfo};

/// Zero bytes after `R`, enough for any over-read of a granule's big values.
pub(crate) const R_PAD: usize = 4096;

fn is_frame(d: &[u8], p: usize) -> Option<Header> {
    if p + 4 > d.len() || d[p] != 0xff {
        return None;
    }
    let h = Header::from_bytes(&d[p..]);
    (h.valid() && p + h.frame_len() <= d.len()).then_some(h)
}

fn compatible(a: Header, b: Header) -> bool {
    a.version() == b.version() && a.sr_index() == b.sr_index()
}

/// A run of frames may start at `p` if two more compatible frames follow, or the file ends
/// after fewer.
fn run_starts(d: &[u8], p: usize) -> bool {
    let Some(h) = is_frame(d, p) else { return false };
    let mut q = p + h.frame_len();
    for _ in 0..2 {
        if q == d.len() {
            return true;
        }
        match is_frame(d, q) {
            Some(h2) if compatible(h, h2) => q += h2.frame_len(),
            _ => return false,
        }
    }
    true
}

/// Splits `d` into junk and frame runs: returns alternating lengths, and frame offsets.
pub(crate) fn segment(d: &[u8]) -> (Vec<u32>, Vec<usize>) {
    let mut segs = Vec::new();
    let mut frames = Vec::new();
    let mut p = 0;
    let mut junk_start = 0;
    // An ID3v2 tag is junk as a whole, so that frames are not searched inside it.
    if d.len() >= 10 && &d[0..3] == b"ID3" && d[6..10].iter().all(|&b| b < 0x80) {
        let size = d[6..10].iter().fold(0usize, |a, &b| a << 7 | b as usize);
        let footer = if d[5] & 0x10 != 0 { 10 } else { 0 };
        p = (10 + size + footer).min(d.len());
    }
    while p < d.len() {
        if !run_starts(d, p) {
            p = match memchr::memchr(0xff, &d[p + 1..]) {
                Some(i) => p + 1 + i,
                None => d.len(),
            };
            continue;
        }
        segs.push((p - junk_start) as u32);
        let first = Header::from_bytes(&d[p..]);
        let mut n = 0u32;
        while let Some(h) = is_frame(d, p) {
            if !compatible(first, h) {
                break;
            }
            frames.push(p);
            p += h.frame_len();
            n += 1;
        }
        segs.push(n);
        junk_start = p;
    }
    segs.push((d.len() - junk_start) as u32);
    (segs, frames)
}

/// Where the main data of a frame lies in `R`, in bits, if the frame is placed.
pub(crate) fn place(si: &SideInfo, h: Header, payload: usize, len: usize, end: usize) -> Option<usize> {
    let start = payload.checked_sub(si.main_data_begin as usize)? * 8;
    let mut total = 0usize;
    for gr in 0..h.granules() {
        for ch in 0..h.channels() {
            total += si.gr[gr][ch].part2_3_length as usize;
        }
    }
    (start >= end && start + total <= (payload + len) * 8).then_some(start)
}

/// [`segment`], for benchmarks.
pub fn segment_pub(d: &[u8]) -> (Vec<u32>, Vec<usize>) {
    segment(d)
}
