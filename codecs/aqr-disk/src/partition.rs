//! Partition tables: MBR (with its extended partitions' chains of EBRs), GPT and APM. What is
//! read is where each partition is; their types are not looked at (a file system is known by its
//! own bytes), but those of no data (empty entries, APM's map and free space).

use std::ops::Range;

/// A partition: where it is in the image (perhaps past its end, of an image cut short).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub span: Range<usize>,
    pub scheme: Scheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Mbr,
    Gpt,
    Apm,
}

/// Most entries read of a table, and most EBRs of a chain (a loop's end).
const MOST: usize = 1024;

fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

fn le64(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

fn be16(b: &[u8], p: usize) -> u16 {
    u16::from_be_bytes([b[p], b[p + 1]])
}

fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}

/// `n` units of `unit` bytes from unit `start`, where it is a range of `usize`, and not empty.
fn span(start: u64, n: u64, unit: u64) -> Option<Range<usize>> {
    let a = usize::try_from(start.checked_mul(unit)?).ok()?;
    let b = usize::try_from(start.checked_add(n)?.checked_mul(unit)?).ok()?;
    (n > 0).then_some(a..b)
}

/// The partitions of image `d`, in the order of its table: of its GPT where it has one, else of
/// its MBR, else of its APM. None where it has none of them.
pub fn partitions(d: &[u8]) -> Vec<Partition> {
    let (gpt, mbr) = (gpt(d), mbr(d));
    let (spans, scheme) = match () {
        _ if !gpt.is_empty() => (gpt, Scheme::Gpt),
        _ if !mbr.is_empty() => (mbr, Scheme::Mbr),
        _ => (apm(d), Scheme::Apm),
    };
    spans.into_iter().map(|span| Partition { span, scheme }).collect()
}

/// An MBR's partitions (of 512-byte sectors): its primary ones, and the logical ones of its
/// extended ones, but GPT's protective one. None where any of its four entries is not one (a
/// boot sector of a volume, or other data).
pub fn mbr(d: &[u8]) -> Vec<Range<usize>> {
    const SECTOR: u64 = 512;
    let table = |s: &[u8]| -> Option<[(u8, u64, u64); 4]> {
        if s.get(510..512)? != [0x55, 0xaa] {
            return None;
        }
        let mut out = [(0, 0, 0); 4];
        for (i, o) in out.iter_mut().enumerate() {
            let e = &s[446 + 16 * i..][..16];
            if e[0] & 0x7f != 0 || e[4] == 0 && e[8..16] != [0; 8] {
                return None;
            }
            *o = (e[4], le32(e, 8) as u64, le32(e, 12) as u64);
        }
        Some(out)
    };
    let extended = |t: u8| matches!(t, 0x05 | 0x0f | 0x85);
    let Some(primary) = table(d) else { return Vec::new() };
    let mut out = Vec::new();
    for (t, start, n) in primary {
        if t == 0 || t == 0xee || n == 0 || start == 0 {
            continue;
        }
        if !extended(t) {
            out.extend(span(start, n, SECTOR));
            continue;
        }
        // the chain of EBRs: each a logical partition (from it), and where the next is (from the
        // extended partition's start)
        let mut ebr = start;
        for _ in 0..MOST {
            let Some(s) = span(ebr, 1, SECTOR).and_then(|r| d.get(r)) else { break };
            let Some([(t0, s0, n0), (t1, s1, n1), ..]) = table(s) else { break };
            if t0 != 0 && n0 > 0 && s0 > 0 {
                out.extend(ebr.checked_add(s0).and_then(|s| span(s, n0, SECTOR)));
            }
            if !extended(t1) || n1 == 0 || s1 == 0 {
                break;
            }
            ebr = match start.checked_add(s1) {
                Some(next) if next != ebr => next,
                _ => break,
            };
        }
    }
    out
}

/// A GPT's partitions, of sectors of 512 bytes, or else of 4096: its primary header's (its
/// checksums not checked: a file system is known by its own bytes anyway), the entries of a
/// type in use.
pub fn gpt(d: &[u8]) -> Vec<Range<usize>> {
    for sector in [512u64, 4096] {
        let Some(h) = d.get(sector as usize..sector as usize + 92) else { break };
        if &h[..8] != b"EFI PART" || le32(h, 12) < 92 {
            continue;
        }
        let (entries, count, size) = (le64(h, 72), le32(h, 80) as usize, le32(h, 84) as usize);
        if !(128..=4096).contains(&size) || size % 8 != 0 {
            continue;
        }
        let Some(at) = entries.checked_mul(sector).and_then(|a| usize::try_from(a).ok()) else { continue };
        let mut out = Vec::new();
        for i in 0..count.min(MOST) {
            let Some(e) = d.get(at + i * size..).and_then(|e| e.get(..128)) else { break };
            let (first, last) = (le64(e, 32), le64(e, 40));
            if e[..16] == [0; 16] || last < first {
                continue;
            }
            out.extend(span(first, last - first + 1, sector));
        }
        return out;
    }
    Vec::new()
}

/// An Apple partition map's partitions, but the map itself and free space: of blocks of the
/// size its driver descriptor (block 0) tells, where it has one, else of 512 bytes.
pub fn apm(d: &[u8]) -> Vec<Range<usize>> {
    let block = match d.get(..4) {
        Some(b) if &b[..2] == b"ER" && matches!(be16(b, 2), 512 | 1024 | 2048 | 4096) => be16(b, 2) as usize,
        _ => 512,
    };
    let entry = |i: usize| d.get(i * block..).and_then(|e| e.get(..80)).filter(|e| &e[..2] == b"PM");
    let Some(first) = entry(1) else { return Vec::new() };
    let mut out = Vec::new();
    for i in 1..=(be32(first, 4) as usize).min(MOST) {
        let Some(e) = entry(i) else { break };
        let kind = &e[48..80];
        let kind = &kind[..kind.iter().position(|&b| b == 0).unwrap_or(32)];
        if kind.eq_ignore_ascii_case(b"Apple_partition_map") || kind.eq_ignore_ascii_case(b"Apple_Free") {
            continue;
        }
        out.extend(span(be32(e, 8) as u64, be32(e, 12) as u64, block as u64));
    }
    out
}
