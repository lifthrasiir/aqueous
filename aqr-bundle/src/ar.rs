//! ar archives: the global header `!<arch>\n`, then members, each a 60-byte header (name,
//! mtime, uid, gid, mode, size in decimal, `` `\n ``), its body, and a `\n` after a body of odd
//! size. Names are those of System V and GNU (`name/`, `/offset` into the `//` member of long
//! names, `/` and `/SYM64/` symbol tables; Windows libraries too) or of BSD (`#1/length`, the
//! name taking the first bytes of the body, `__.SYMDEF` symbol tables). Thin archives are not
//! taken.

use crate::layout::{AR_HEADER, Body, Rec, Role};

pub const MAGIC: &[u8; 8] = b"!<arch>\n";

/// Whether `d` starts like an ar archive: its global header, then a member header or nothing.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(MAGIC) && (d.len() == MAGIC.len() || size(d, MAGIC.len()).is_some())
}

/// A decimal field, padded with spaces after it.
fn decimal(f: &[u8]) -> Option<u64> {
    let n = f.iter().position(|&b| b == b' ').unwrap_or(f.len());
    if n == 0 || !f[..n].iter().all(u8::is_ascii_digit) || !f[n..].iter().all(|&b| b == b' ') {
        return None;
    }
    std::str::from_utf8(&f[..n]).ok()?.parse().ok()
}

/// The size of the member whose header is at `at`, if a header is there.
fn size(d: &[u8], at: usize) -> Option<usize> {
    let h = d.get(at..at.checked_add(AR_HEADER)?)?;
    if &h[58..] != b"`\n" {
        return None;
    }
    usize::try_from(decimal(&h[48..58])?).ok()
}

/// The records of ar archive `d` (its global header a gap), or `None` where it is not one.
/// What follows the last member that fits is a gap.
pub fn records(d: &[u8]) -> Option<Vec<Rec>> {
    if !d.starts_with(MAGIC) {
        return None;
    }
    let mut recs = vec![Rec::Gap(0..MAGIC.len())];
    let (mut p, mut names) = (MAGIC.len(), 0..0);
    while let Some(s) = size(d, p) {
        let field = &d[p..p + 16];
        let trimmed = trim(field, b' ');
        let pre = match field.strip_prefix(b"#1/") {
            Some(n) => match decimal(n).and_then(|n| usize::try_from(n).ok()) {
                Some(n) if n <= s => n,
                _ => break,
            },
            None => 0,
        };
        let start = p + AR_HEADER + pre;
        let Some(end) = (p + AR_HEADER).checked_add(s).filter(|&e| e <= d.len()) else { break };
        let (role, name) = if pre > 0 {
            let name = trim(&d[p + AR_HEADER..start], 0);
            (if name.starts_with(b"__.SYMDEF") { Role::Table } else { Role::File }, name.to_vec())
        } else if trimmed == b"/" || trimmed == b"/SYM64/" || trimmed.starts_with(b"__.SYMDEF") {
            (Role::Table, trimmed.to_vec())
        } else if trimmed == b"//" {
            names = start..end;
            (Role::Meta, trimmed.to_vec())
        } else if let Some(off) = trimmed.strip_prefix(b"/").and_then(decimal) {
            // a long name: up to its end in the table (`/\n` of GNU, NUL of Windows)
            let table = &d[names.clone()];
            let name = usize::try_from(off).ok().and_then(|o| table.get(o..)).unwrap_or_default();
            let name = &name[..name.iter().position(|&b| b == b'\n' || b == 0).unwrap_or(name.len())];
            (Role::File, name.strip_suffix(b"/").unwrap_or(name).to_vec())
        } else {
            (Role::File, trimmed.strip_suffix(b"/").unwrap_or(trimmed).to_vec())
        };
        // a body of odd size is followed by a pad byte, unless the next header is right after
        // it, or nothing is
        let pad = s % 2 == 1 && end < d.len() && !(size(d, end).is_some() && size(d, end + 1).is_none());
        recs.push(Rec::Ar { at: p, pre, body: Body { span: start..end, role, name }, pad });
        p = end + pad as usize;
    }
    if p < d.len() {
        recs.push(Rec::Gap(p..d.len()));
    }
    Some(recs)
}

/// `b` without the bytes `c` at its end.
fn trim(b: &[u8], c: u8) -> &[u8] {
    &b[..b.iter().rposition(|&x| x != c).map_or(0, |p| p + 1)]
}
