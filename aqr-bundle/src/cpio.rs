//! cpio archives: members of a header, a name (with its NUL) and a body, in one of four forms
//! ([`Cpio`]): newc and crc (`070701`, `070702`: hexadecimal fields, the name and the body
//! each padded to 4 bytes), odc (`070707`: octal fields, no padding) and binary (16-bit words
//! of either byte order, padded to 2 bytes). The last member is named `TRAILER!!!`, and zeros
//! usually follow it (to a block). Archives one after another (initramfs) are read on.

use crate::layout::{Body, Cpio, Rec, Role, pad_to};

fn hex(f: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(f).ok()?;
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

fn oct(f: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(f).ok()?;
    if !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u64::from_str_radix(s, 8).ok()
}

/// The form of the header at `at` of `d`, and the sizes of its name and body, if a header of a
/// name that fits (of at least its NUL) is there.
fn header(d: &[u8], at: usize) -> Option<(Cpio, usize, usize)> {
    let h = d.get(at..)?;
    let (form, m, n) = match h.get(..6)? {
        b"070701" | b"070702" => {
            let h = h.get(..110)?;
            let fields: Vec<u64> = h[6..].chunks(8).map(hex).collect::<Option<_>>()?;
            let form = if h[5] == b'1' { Cpio::Newc } else { Cpio::Crc };
            (form, fields[11], fields[6])
        }
        b"070707" => {
            let h = h.get(..76)?;
            if !h[6..59].iter().all(|b| (b'0'..=b'7').contains(b)) {
                return None;
            }
            (Cpio::Odc, oct(&h[59..65])?, oct(&h[65..76])?)
        }
        &[a, b, ..] if [a, b] == [0xc7, 0x71] || [a, b] == [0x71, 0xc7] => {
            let h = h.get(..26)?;
            let le = a == 0xc7;
            let word = |p: usize| {
                let w = [h[p], h[p + 1]];
                (if le { u16::from_le_bytes(w) } else { u16::from_be_bytes(w) }) as u64
            };
            (if le { Cpio::BinLe } else { Cpio::BinBe }, word(20), word(22) << 16 | word(24))
        }
        _ => return None,
    };
    let (m, n) = (usize::try_from(m).ok()?, usize::try_from(n).ok()?);
    let name = h.get(form.header()..)?.get(..m)?;
    (name.last() == Some(&0)).then_some((form, m, n))
}

/// Whether `d` starts like a cpio archive: a header.
pub fn starts(d: &[u8]) -> bool {
    header(d, 0).is_some()
}

/// The records of cpio archive `d`, or `None` where it does not start with a header. Zeros
/// between and after archives are zeros, and what follows the last member that fits a gap.
pub fn records(d: &[u8]) -> Option<Vec<Rec>> {
    header(d, 0)?;
    let mut recs = Vec::new();
    let mut p = 0;
    while p < d.len() {
        if let Some((form, m, n)) = header(d, p) {
            let name_end = p + form.header() + m;
            let start = name_end + pad_to(name_end, form.align());
            let Some(end) = start.checked_add(n).filter(|&e| e <= d.len()) else { break };
            let next = end + pad_to(end, form.align());
            if next > d.len() {
                break;
            }
            let name = d[p + form.header()..name_end - 1].to_vec();
            recs.push(Rec::Cpio { at: p, form, m, body: Body { span: start..end, role: Role::File, name } });
            p = next;
        } else if d[p] == 0 {
            let q = d[p..].iter().position(|&b| b != 0).map_or(d.len(), |q| p + q);
            recs.push(Rec::Zeros(p..q));
            p = q;
        } else {
            break;
        }
    }
    if p < d.len() {
        recs.push(Rec::Gap(p..d.len()));
    }
    Some(recs)
}
