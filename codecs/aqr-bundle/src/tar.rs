//! tar archives: 512-byte headers (v7, ustar, GNU, pax), each followed by its body padded to a
//! block, and zero blocks at the end. A header is known by its checksum. pax extended headers
//! (`x`, `g`) and GNU long names (`L`, `K`) are members whose bodies tell of the next member:
//! its size (pax `size`) and name. GNU sparse files of the old form (extension blocks after the
//! header) are not read: what follows them is a gap.

use crate::layout::{Body, Rec, Role, TAR_BLOCK, pad_to};

/// An octal field: spaces, digits, then a NUL or space (or none); none at all is 0.
fn octal(f: &[u8]) -> Option<u64> {
    let f = &f[f.iter().position(|&b| b != b' ').unwrap_or(f.len())..];
    let n = f.iter().position(|b| !(b'0'..=b'7').contains(b)).unwrap_or(f.len());
    if !f[n..].iter().all(|&b| b == 0 || b == b' ') || n > 22 {
        return None;
    }
    Some(f[..n].iter().fold(0, |v, &b| v << 3 | (b - b'0') as u64))
}

/// The size field: octal, or base-256 (GNU, a set top bit).
fn size(h: &[u8]) -> Option<u64> {
    let f = &h[124..136];
    if f[0] & 0x80 != 0 {
        if f[0] != 0x80 || f[1..4] != [0; 3] {
            return None;
        }
        return Some(u64::from_be_bytes(f[4..].try_into().unwrap()));
    }
    octal(f)
}

/// Whether a header is at `at` of `d`: its checksum, as the unsigned sum of its bytes (or the
/// signed one of some old writers), the field taken as spaces.
pub fn header(d: &[u8], at: usize) -> bool {
    let Some(h) = d.get(at..).and_then(|r| r.get(..TAR_BLOCK)) else { return false };
    let Some(sum) = octal(&h[148..156]) else { return false };
    let rest = || h[..148].iter().chain(&[b' '; 8]).chain(&h[156..]);
    let unsigned: u64 = rest().map(|&b| b as u64).sum();
    let signed: i64 = rest().map(|&b| b as i8 as i64).sum();
    // (an all-zero block sums to 256: not a header)
    unsigned != 256 && (sum == unsigned || sum as i64 == signed) && size(h).is_some()
}

/// Whether `d` starts like a tar archive: a header, of ustar or GNU magic or else a name and an
/// octal mode (v7).
pub fn starts(d: &[u8]) -> bool {
    header(d, 0)
        && (&d[257..262] == b"ustar"
            || d[0] != 0 && d[..100].iter().all(|&b| b == 0 || b >= b' ') && octal(&d[100..108]).is_some())
}

fn zero_block(d: &[u8], at: usize) -> bool {
    d.get(at..).and_then(|r| r.get(..TAR_BLOCK)).is_some_and(|b| b.iter().all(|&x| x == 0))
}

/// Up to the first NUL.
fn c_str(f: &[u8]) -> &[u8] {
    &f[..f.iter().position(|&b| b == 0).unwrap_or(f.len())]
}

/// The records of tar archive `d`, or `None` where it does not start with a header. Zero
/// blocks are zeros, and what follows the last member or zero block that fits a gap (zeros
/// where it is all zeros).
pub fn records(d: &[u8]) -> Option<Vec<Rec>> {
    if !header(d, 0) {
        return None;
    }
    let mut recs = Vec::new();
    // what extended headers tell of the next member
    let (mut next_size, mut next_name): (Option<u64>, Option<Vec<u8>>) = (None, None);
    let mut p = 0;
    // whether a header, zero blocks or the end follow a member of `n` bytes at `p`
    let ends = |p: usize, n: usize| {
        let q = p + TAR_BLOCK + n + pad_to(n, TAR_BLOCK);
        q == d.len() || header(d, q) || zero_block(d, q)
    };
    while p + TAR_BLOCK <= d.len() {
        if zero_block(d, p) {
            let mut q = p + TAR_BLOCK;
            while zero_block(d, q) {
                q += TAR_BLOCK;
            }
            recs.push(Rec::Zeros(p..q));
            p = q;
            continue;
        }
        if !header(d, p) {
            break;
        }
        let h = &d[p..p + TAR_BLOCK];
        let ty = h[156];
        let field = size(h).unwrap();
        let declared = next_size.take().unwrap_or(field);
        // links, devices, directories and FIFOs may have a size and no body
        let n = [0, declared]
            .into_iter()
            .take(if matches!(ty, b'1'..=b'6') { 2 } else { 1 })
            .filter_map(|n| usize::try_from(n).ok())
            .find(|&n| n <= d.len() - p - TAR_BLOCK && ends(p, n))
            .or_else(|| usize::try_from(declared).ok().filter(|&n| n <= d.len() - p - TAR_BLOCK));
        // (the padding too: what is cut short of it is not a member)
        let Some(n) = n.filter(|&n| span_end(p, n) <= d.len()) else { break };
        let span = p + TAR_BLOCK..p + TAR_BLOCK + n;
        let body = &d[span.clone()];
        let role = if matches!(ty, b'x' | b'g' | b'L' | b'K') { Role::Meta } else { Role::File };
        let name = next_name.take().unwrap_or_else(|| {
            let (name, prefix) = (c_str(&h[..100]), c_str(&h[345..500]));
            if &h[257..263] == b"ustar\0" && !prefix.is_empty() { [prefix, b"/", name].concat() } else { name.to_vec() }
        });
        match ty {
            b'x' => {
                for (k, v) in pax(body) {
                    match k {
                        b"size" => next_size = std::str::from_utf8(v).ok().and_then(|v| v.parse().ok()),
                        b"path" => next_name = Some(v.to_vec()),
                        _ => {}
                    }
                }
            }
            b'L' => next_name = Some(c_str(body).to_vec()),
            _ => {}
        }
        recs.push(Rec::Tar { at: p, body: Body { span, role, name } });
        p = span_end(p, n);
    }
    if p < d.len() {
        recs.push(if d[p..].iter().all(|&b| b == 0) { Rec::Zeros(p..d.len()) } else { Rec::Gap(p..d.len()) });
    }
    Some(recs)
}

/// The end of the padding of a member of `n` bytes at `p`.
fn span_end(p: usize, n: usize) -> usize {
    p + TAR_BLOCK + n + pad_to(n, TAR_BLOCK)
}

/// The records of a pax extended header, `length key=value\n` each, while they parse.
fn pax(mut b: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    while let Some(sp) = b.iter().position(|&c| c == b' ') {
        let Some(len) = std::str::from_utf8(&b[..sp]).ok().and_then(|l| l.parse::<usize>().ok()) else { break };
        let Some(rec) = b.get(sp + 1..len).filter(|r| r.ends_with(b"\n")) else { break };
        let rec = &rec[..rec.len() - 1];
        let Some(eq) = rec.iter().position(|&c| c == b'=') else { break };
        out.push((&rec[..eq], &rec[eq + 1..]));
        b = &b[len..];
    }
    out
}
