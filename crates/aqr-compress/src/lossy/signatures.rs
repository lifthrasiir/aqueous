//! Detached signatures of a file archived along with it.
//!
//! A file that a signature beside it signs must be kept as it is: a rewritten one would no longer
//! verify. Recognised, by name (`NAME` + suffix, exactly):
//!
//! - `.sig`, `.asc`, `.gpg`: OpenPGP signatures (RFC 4880, RFC 9580), binary or ASCII-armored. A
//!   signature of type 0x00 (binary) or 0x01 (text) is tied to the file by its hash's leftmost 16
//!   bits (the version 3, 4 and 6 formats; MD5, SHA-1 and SHA-2 hashes); a mismatch is another
//!   file's signature. What is not OpenPGP, or does not parse, is nothing of ours (an armor of a
//!   wrong CRC-24 included). A signature of an unknown version or hash cannot be checked, and is
//!   taken as the file's.
//! - `.minisig`: minisign's, of the shape alone (it cannot be tied to the file without the key).

use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};
use std::collections::HashMap;

/// Why the file `name` of `data` is kept as it is, if a detached signature of it is among the
/// contents `names` (by name: `NAME.sig`, `NAME.asc`, `NAME.gpg`, `NAME.minisig`), whose bytes
/// `read` gives.
pub(super) fn signed(
    name: &[u8],
    data: &[u8],
    names: &HashMap<&[u8], usize>,
    read: &dyn Fn(usize) -> Option<Vec<u8>>,
) -> Option<String> {
    for suffix in ["sig", "asc", "gpg", "minisig"] {
        let mut sibling = name.to_vec();
        sibling.push(b'.');
        sibling.extend_from_slice(suffix.as_bytes());
        let Some(&index) = names.get(sibling.as_slice()) else { continue };
        let Some(bytes) = read(index) else { continue };
        let (hit, kind) = if suffix == "minisig" {
            (is_minisign(&bytes), "minisign")
        } else {
            (openpgp_signs(&bytes, data), "OpenPGP")
        };
        if hit {
            return Some(format!("signed by {} ({kind})", String::from_utf8_lossy(&sibling)));
        }
    }
    None
}

fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0, 0);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None;
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (pad <= 2 && acc == 0).then_some(out)
}

pub(super) fn crc24(data: &[u8]) -> u32 {
    let mut crc = 0xB704CE_u32;
    for &b in data {
        crc ^= (b as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x100_0000 != 0 {
                crc ^= 0x186_4CFB;
            }
        }
    }
    crc & 0xFF_FFFF
}

/// The binary of an ASCII armor of a signature (None: not one, or a wrong CRC-24).
fn dearmor(data: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.trim_start().lines().map(|l| l.trim_end());
    if lines.next()? != "-----BEGIN PGP SIGNATURE-----" {
        return None;
    }
    // Armor headers up to the blank line.
    loop {
        if lines.next()?.is_empty() {
            break;
        }
    }
    let mut body = Vec::new();
    let mut crc = None;
    for line in lines {
        if line == "-----END PGP SIGNATURE-----" {
            let bin = base64_decode(&body)?;
            if let Some(crc) = crc
                && crc24(&bin) != crc
            {
                return None;
            }
            return Some(bin);
        }
        if let Some(c) = line.strip_prefix('=') {
            let c = base64_decode(c.as_bytes())?;
            if c.len() != 3 {
                return None;
            }
            crc = Some((c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32);
        } else {
            body.extend_from_slice(line.as_bytes());
        }
    }
    None
}

/// The packets (tag, body) of binary OpenPGP data; None if it does not parse whole.
fn packets(mut d: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    while !d.is_empty() {
        let b = d[0];
        if b & 0x80 == 0 {
            return None;
        }
        let (tag, len, head): (u8, usize, usize);
        if b & 0x40 != 0 {
            tag = b & 0x3F;
            let o = *d.get(1)? as usize;
            match o {
                0..192 => (len, head) = (o, 2),
                192..224 => (len, head) = (((o - 192) << 8) + *d.get(2)? as usize + 192, 3),
                255 => (len, head) = (u32::from_be_bytes(d.get(2..6)?.try_into().ok()?) as usize, 6),
                _ => return None, // partial lengths: of data packets, not signatures
            }
        } else {
            tag = b >> 2 & 15;
            match b & 3 {
                0 => (len, head) = (*d.get(1)? as usize, 2),
                1 => (len, head) = (u16::from_be_bytes(d.get(1..3)?.try_into().ok()?) as usize, 3),
                2 => (len, head) = (u32::from_be_bytes(d.get(1..5)?.try_into().ok()?) as usize, 5),
                _ => (len, head) = (d.len() - 1, 1),
            }
        }
        out.push((tag, d.get(head..head.checked_add(len)?)?));
        d = &d[head + len..];
    }
    Some(out)
}

fn digest<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = D::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}

pub(super) fn hash(alg: u8, parts: &[&[u8]]) -> Option<Vec<u8>> {
    Some(match alg {
        1 => digest::<md5::Md5>(parts),
        2 => digest::<Sha1>(parts),
        8 => digest::<Sha256>(parts),
        9 => digest::<Sha384>(parts),
        10 => digest::<Sha512>(parts),
        11 => digest::<Sha224>(parts),
        _ => return None,
    })
}

/// Text in the canonical form of RFC 4880 section 5.2.1: line ends are CR LF.
pub(super) fn canonical(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 32);
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && (i == 0 || data[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Whether the signature packet `p` may be of `data`: Some(true) if its hash prefix matches (or
/// cannot be told), Some(false) if it does not, None if the packet is malformed.
#[allow(clippy::type_complexity)]
fn packet_signs(p: &[u8], data: &[u8]) -> Option<bool> {
    let version = *p.first()?;
    let (sigtype, hash_alg, left16, prefix, suffix, salt): (u8, u8, &[u8], &[u8], Vec<u8>, &[u8]);
    match version {
        3 => {
            if *p.get(1)? != 5 {
                return None;
            }
            sigtype = *p.get(2)?;
            hash_alg = *p.get(16)?;
            left16 = p.get(17..19)?;
            prefix = &[];
            suffix = p.get(2..7)?.to_vec();
            salt = &[];
        }
        4 | 6 => {
            sigtype = *p.get(1)?;
            hash_alg = *p.get(3)?;
            let (n, mut at) = if version == 4 { (2, 4) } else { (4, 4) };
            let hl_bytes = p.get(at..at + n)?;
            let hl = hl_bytes.iter().fold(0usize, |a, &b| a << 8 | b as usize);
            at += n;
            let hashed_end = at.checked_add(hl)?;
            p.get(at..hashed_end)?;
            let ul_bytes = p.get(hashed_end..hashed_end + n)?;
            let ul = ul_bytes.iter().fold(0usize, |a, &b| a << 8 | b as usize);
            let rest = hashed_end + n + ul;
            left16 = p.get(rest..rest + 2)?;
            let mut s = p[..hashed_end].to_vec();
            s.extend_from_slice(&[version, 0xFF]);
            s.extend_from_slice(&(hashed_end as u32).to_be_bytes());
            suffix = s;
            salt = if version == 6 {
                let sl = *p.get(rest + 2)? as usize;
                p.get(rest + 3..rest + 3 + sl)?
            } else {
                &[]
            };
            prefix = &[];
        }
        _ => return Some(true), // unknown version: cannot be checked
    }
    if sigtype > 1 {
        return Some(false);
    }
    let text;
    let body = if sigtype == 1 {
        text = canonical(data);
        &text[..]
    } else {
        data
    };
    let Some(h) = hash(hash_alg, &[salt, prefix, body, &suffix]) else { return Some(true) };
    Some(h[..2] == *left16)
}

/// Whether `sig` (binary or armored OpenPGP) holds a signature that may be of `data`.
fn openpgp_signs(sig: &[u8], data: &[u8]) -> bool {
    let armored;
    let bin = if sig.first().is_some_and(|&b| b & 0x80 != 0) {
        sig
    } else {
        let Some(b) = dearmor(sig) else { return false };
        armored = b;
        &armored
    };
    let Some(pkts) = packets(bin) else { return false };
    // The version 3 prefix (type, time) is in `suffix`; v4's trailer is built in packet_signs.
    pkts.iter().filter(|(tag, _)| *tag == 2).any(|(_, p)| packet_signs(p, data) == Some(true))
}

/// minisign's `.minisig`: comment, base64 of 74 bytes ("Ed" or "ED", key id, signature), trusted
/// comment, base64 of 64 bytes.
fn is_minisign(sig: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(sig) else { return false };
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let (Some(c1), Some(s1), Some(c2), Some(s2)) = (lines.next(), lines.next(), lines.next(), lines.next()) else {
        return false;
    };
    if !c1.starts_with("untrusted comment:") || !c2.starts_with("trusted comment:") {
        return false;
    }
    if lines.any(|l| !l.is_empty()) {
        return false;
    }
    let a = base64_decode(s1.as_bytes());
    let b = base64_decode(s2.as_bytes());
    matches!(a, Some(a) if a.len() == 74 && (&a[..2] == b"Ed" || &a[..2] == b"ED"))
        && matches!(b, Some(b) if b.len() == 64)
}
