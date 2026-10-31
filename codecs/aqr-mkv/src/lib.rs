//! Matroska and WebM files: their elements walked (EBML) down to the blocks, by which the
//! frames are known, and the formula of a file as its bytes with the frames seen before taken
//! from where they were first ([`formula`]). The frames are not looked into: what is stored
//! once is what the muxer wrote, the codec's bytes as they are (README).

use std::ops::Range;

pub mod formula;

/// The EBML header's ID, the first 4 bytes of a file.
pub const EBML: u32 = 0x1a45_dfa3;
/// The EBML header's DocType.
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const CLUSTER: u32 = 0x1f43_b675;
const BLOCK_GROUP: u32 = 0xa0;
const BLOCK: u32 = 0xa1;
const SIMPLE_BLOCK: u32 = 0xa3;

/// The IDs of the elements of a Segment (level 1), at which a Cluster of unknown size ends.
const LEVEL_1: [u32; 9] =
    [0x114d_9b74, 0x1549_a966, 0x1654_ae6b, CLUSTER, 0x1c53_bb6b, 0x1941_a469, 0x1043_a770, 0x1254_c367, EBML];

/// An element's header at `p` of `d`: its ID (the marker bit kept, as the specification writes
/// IDs), its size (None where unknown, all of its bits set) and the header's length. None where
/// `d` does not hold one there.
fn header(d: &[u8], p: usize) -> Option<(u32, Option<u64>, usize)> {
    let b = *d.get(p)?;
    let n = b.leading_zeros() as usize + 1;
    if n > 4 {
        return None;
    }
    let id = d.get(p..p + n)?.iter().fold(0u32, |a, &b| a << 8 | b as u32);
    let (size, m) = vint(d, p + n)?;
    Some((id, size, n + m))
}

/// A variable-size integer at `p` of `d` (1 to 8 bytes, its length told by the leading zeros
/// of its first byte): its value (None where all of its bits are set, an unknown size) and its
/// length.
fn vint(d: &[u8], p: usize) -> Option<(Option<u64>, usize)> {
    let b = *d.get(p)?;
    if b == 0 {
        return None;
    }
    let n = b.leading_zeros() as usize + 1;
    let v = d.get(p + 1..p + n)?.iter().fold(b as u64 & 0xff >> n, |a, &b| a << 8 | b as u64);
    Some(((v != (1 << (7 * n)) - 1).then_some(v), n))
}

/// The frames of a block ([`walk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Its bytes in the file.
    pub span: Range<usize>,
    /// The block's track number.
    pub track: u64,
    /// A SimpleBlock's flags (0x80 a keyframe, 0x01 discardable, bits 1–2 the lacing); 0 for a
    /// Block, whose flags tell neither.
    pub flags: u8,
}

/// Whether `d` starts with an EBML header of DocType `webm` or `matroska` followed by a
/// Segment.
pub fn starts(d: &[u8]) -> bool {
    let Some((EBML, Some(size), h)) = header(d, 0) else { return false };
    let Some(end) = (h as u64).checked_add(size).filter(|&e| e <= d.len() as u64).map(|e| e as usize) else {
        return false;
    };
    let mut p = h;
    let mut doc = false;
    while let Some((id, Some(size), h)) = header(&d[..end], p) {
        let Some(e) = (p + h).checked_add(size as usize).filter(|&e| e <= end) else { break };
        if id == DOC_TYPE {
            doc = matches!(&d[p + h..e], b"webm" | b"matroska");
        }
        p = e;
    }
    doc && p == end && matches!(header(d, end), Some((SEGMENT, _, _)))
}

/// What a walk over a file's elements finds ([`walk`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Walk {
    /// The frames of the blocks (SimpleBlocks and the Blocks of BlockGroups, in Clusters of
    /// Segments), in order.
    pub frames: Vec<Frame>,
    /// The numbers of the video tracks (TrackType 1 in the Tracks' entries).
    pub video: Vec<u64>,
}

/// The frames and the video tracks of file `d`. The walk stops where the elements do not
/// read, or pass the end of the data: the bytes from there hold no frames. An element of a
/// known size that does not read within is passed over whole.
pub fn walk(d: &[u8]) -> Walk {
    let mut w = Walk::default();
    elements(d, 0, d.len(), None, &mut w);
    w
}

const TRACKS: u32 = 0x1654_ae6b;
const TRACK_ENTRY: u32 = 0xae;
const TRACK_NUMBER: u32 = 0xd7;
const TRACK_TYPE: u32 = 0x83;

/// Walks the elements from `p` up to `end`, of a parent of ID `parent` (None at the top), into
/// `w`; returns where it stopped: at `end`, at an element that ends a Cluster of unknown size,
/// or where an element does not read.
fn elements(d: &[u8], mut p: usize, end: usize, parent: Option<u32>, w: &mut Walk) -> usize {
    while p < end {
        let Some((id, size, h)) = header(&d[..end], p) else { return p };
        if parent == Some(CLUSTER) && LEVEL_1.contains(&id) {
            // (an element of the Segment ends the Cluster of unknown size it is in)
            return p;
        }
        let start = p + h;
        let container = matches!(id, SEGMENT | CLUSTER | BLOCK_GROUP | TRACKS | TRACK_ENTRY);
        let stop = match size.map(|n| start.checked_add(n as usize)) {
            Some(Some(e)) if e <= end || container => e,
            None if matches!(id, SEGMENT | CLUSTER) => end,
            _ => return p,
        };
        match id {
            TRACK_ENTRY if parent == Some(TRACKS) && stop <= end => {
                if let Some(n) = track_entry(&d[start..stop]).filter(|&(_, kind)| kind == 1) {
                    w.video.push(n.0);
                }
            }
            _ if container => {
                // (one cut short is walked up to where the data ends)
                let q = elements(d, start, stop.min(end), Some(id), w);
                p = match size {
                    Some(_) if stop <= end => stop,
                    Some(_) => return q,
                    None => q,
                };
                continue;
            }
            SIMPLE_BLOCK | BLOCK if matches!(parent, Some(CLUSTER) | Some(BLOCK_GROUP)) => {
                // the track number, the timecode (2 bytes) and the flags
                let Some((Some(track), n)) = vint(&d[..stop], start) else { return p };
                let at = start + n + 3;
                if at > stop {
                    return p;
                }
                if at < stop {
                    let flags = if id == SIMPLE_BLOCK { d[at - 1] } else { 0 };
                    w.frames.push(Frame { span: at..stop, track, flags });
                }
            }
            _ => {}
        }
        p = stop;
    }
    p
}

/// The TrackNumber and TrackType of a TrackEntry of data `d`, where it tells both.
fn track_entry(d: &[u8]) -> Option<(u64, u64)> {
    let (mut number, mut kind, mut p) = (None, None, 0);
    while let Some((id, Some(size), h)) = header(d, p) {
        let e = (p + h).checked_add(size as usize).filter(|&e| e <= d.len())?;
        let v = (size <= 8).then(|| d[p + h..e].iter().fold(0u64, |a, &b| a << 8 | b as u64));
        match id {
            TRACK_NUMBER => number = v,
            TRACK_TYPE => kind = v,
            _ => {}
        }
        p = e;
    }
    Some((number?, kind?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An element of ID `id` (its bytes) and `data`, its size in `w` bytes (0 for unknown).
    pub(crate) fn element(id: &[u8], data: &[u8], w: usize) -> Vec<u8> {
        let size = match w {
            0 => vec![0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            _ => {
                let v = data.len() as u64 | 1 << (7 * w);
                v.to_be_bytes()[8 - w..].to_vec()
            }
        };
        [id, &size, data].concat()
    }

    /// A file of a video track 1 and an audio track 2, and of `clusters`, each of its blocks:
    /// SimpleBlocks of a track and a keyframe or not, and their frames; the second cluster of
    /// unknown size where `open`.
    pub(crate) fn file(clusters: &[&[(u8, bool, &[u8])]], open: bool) -> Vec<u8> {
        let head = element(&[0x1a, 0x45, 0xdf, 0xa3], &element(&[0x42, 0x82], b"webm", 1), 1);
        let mut seg = element(&[0x15, 0x49, 0xa9, 0x66], &[0x2a, 0xd7, 0xb1, 0x83, 0x0f, 0x42, 0x40], 1);
        let entry = |n: u8, kind: u8| {
            let codec = element(&[0x63, 0xa2], &[9; 20], 1);
            element(&[0xae], &[element(&[0xd7], &[n], 1), codec, element(&[0x83], &[kind], 1)].concat(), 1)
        };
        seg.extend(element(&[0x16, 0x54, 0xae, 0x6b], &[entry(1, 1), entry(2, 2)].concat(), 2));
        for (i, c) in clusters.iter().enumerate() {
            let mut body = element(&[0xe7], &[i as u8], 1);
            for &(track, key, f) in c.iter() {
                let head = [0x80 | track, 0, 0, if key { 0x80 } else { 0 }];
                body.extend(element(&[0xa3], &[&head[..], f].concat(), 4));
            }
            seg.extend(element(&[0x1f, 0x43, 0xb6, 0x75], &body, if open && i == 1 { 0 } else { 4 }));
        }
        seg.extend(element(&[0x1c, 0x53, 0xbb, 0x6b], &[0xbb, 0x80], 2));
        [head, element(&[0x18, 0x53, 0x80, 0x67], &seg, 8)].concat()
    }

    /// The frames of SimpleBlocks in Clusters, of known and unknown sizes, and the video
    /// track; files that are none.
    #[test]
    fn walked() {
        for open in [false, true] {
            let c: [&[(u8, bool, &[u8])]; 3] = [
                &[(1, true, b"key"), (2, true, b"a"), (1, false, b"p1")],
                &[(1, true, b"key"), (1, false, b"p2")],
                &[(1, false, b"x")],
            ];
            let d = file(&c, open);
            assert!(starts(&d));
            let w = walk(&d);
            assert_eq!(w.video, [1]);
            let got: Vec<(&[u8], u64, u8)> = w.frames.into_iter().map(|f| (&d[f.span], f.track, f.flags)).collect();
            let want: Vec<(&[u8], u64, u8)> =
                c.iter().flat_map(|c| c.iter()).map(|&(t, k, f)| (f, t as u64, if k { 0x80 } else { 0 })).collect();
            assert_eq!(got, want, "{open}");
            // (cut short, the walk stops at the block cut)
            assert_eq!(walk(&d[..d.len() - 12]).frames.len(), 5);
        }
        let d = file(&[&[(1, true, b"a")]], false);
        assert!(!starts(&d[..20]));
        let other = [&d[..8], b"mkv3", &d[12..]].concat();
        assert!(!starts(&other));
    }
}
