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

/// Longest free format frame searched for (640 kbps at 32 kHz is 2881 bytes).
const MAX_FREE: usize = 4096;

/// A frame of the stream with free format length `free` (0: not free format) at `p`.
fn is_frame(d: &[u8], p: usize, free: usize) -> Option<(Header, usize)> {
    if p + 4 > d.len() || d[p] != 0xff {
        return None;
    }
    let h = Header::from_bytes(&d[p..]);
    if !h.valid_any() || h.free_format() != (free > 0) {
        return None;
    }
    let len = h.frame_len_free(free);
    (len >= h.prefix_len() && p + len <= d.len()).then_some((h, len))
}

fn compatible(a: Header, b: Header) -> bool {
    a.version() == b.version() && a.sr_index() == b.sr_index()
}

/// Whether a run of frames with free format length `free` may start at `p`: two more
/// compatible frames follow, or the file ends after fewer.
fn run_starts_with(d: &[u8], p: usize, free: usize) -> bool {
    let Some((h, len)) = is_frame(d, p, free) else { return false };
    let mut q = p + len;
    for _ in 0..2 {
        if q == d.len() {
            return true;
        }
        match is_frame(d, q, free) {
            Some((h2, len2)) if compatible(h, h2) => q += len2,
            _ => return false,
        }
    }
    true
}

/// The free format length (0 if not free format) of a run that may start at `p`.
fn run_starts(d: &[u8], p: usize) -> Option<usize> {
    if p + 4 > d.len() || d[p] != 0xff {
        return None;
    }
    let h = Header::from_bytes(&d[p..]);
    if !h.valid_any() {
        return None;
    }
    if !h.free_format() {
        return run_starts_with(d, p, 0).then_some(0);
    }
    // Free format: the distance to the next matching header gives the length.
    let pad = h.padding() as usize;
    let mut q = p + h.prefix_len();
    while q + 4 <= d.len() && q < p + MAX_FREE {
        q = {
            let i = memchr::memchr(0xff, &d[q..(p + MAX_FREE).min(d.len())])?;
            q + i
        };
        if q + 4 > d.len() {
            return None;
        }
        let h2 = Header::from_bytes(&d[q..]);
        if h2.valid_any() && h2.free_format() && compatible(h, h2) {
            let free = q - p - pad;
            if free >= h.prefix_len() && run_starts_with(d, p, free) {
                return Some(free);
            }
        }
        q += 1;
    }
    None
}

/// Junk lengths, frame runs (count, free format length) and frames (offset, length).
pub type Segments = (Vec<u32>, Vec<(u32, u32)>, Vec<(usize, usize)>);

/// Splits `d` into junk and frame runs. Returns alternating junk lengths and frame runs
/// (as (count, free format length)), starting and ending with junk, and the frames
/// (offset, length).
pub(crate) fn segment(d: &[u8]) -> Segments {
    let mut junk = Vec::new();
    let mut runs = Vec::new();
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
        let Some(free) = run_starts(d, p) else {
            p = match memchr::memchr(0xff, &d[p + 1..]) {
                Some(i) => p + 1 + i,
                None => d.len(),
            };
            continue;
        };
        junk.push((p - junk_start) as u32);
        let first = Header::from_bytes(&d[p..]);
        let mut n = 0u32;
        while let Some((h, len)) = is_frame(d, p, free) {
            if !compatible(first, h) {
                break;
            }
            frames.push((p, len));
            p += len;
            n += 1;
        }
        runs.push((n, free as u32));
        junk_start = p;
    }
    junk.push((d.len() - junk_start) as u32);
    (junk, runs, frames)
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
pub fn segment_pub(d: &[u8]) -> Segments {
    segment(d)
}
