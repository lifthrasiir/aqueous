//! ISO 9660 (ECMA-119) directory trees: those of the primary volume descriptor and of the
//! supplementary ones (Joliet, of UCS-2 names), from sector 16 on to the terminator. Each
//! directory is read once, however many records point at it; the records of a file of several
//! extents (the multi-extent flag on all but the last) are one file.

use crate::{File, SECTOR, le16, le32};

/// The files of every volume descriptor's tree in image `d`, Joliet's first (whose names are
/// the longer). None where sector 16 is not a volume descriptor.
pub fn files(d: &[u8]) -> Vec<File> {
    let (mut joliet, mut primary) = (Vec::new(), Vec::new());
    // (a disc has a handful of descriptors; past this many, it is no list of them)
    for s in 16..16 + 64 {
        let Some(v) = d.get(s * SECTOR..(s + 1) * SECTOR) else { break };
        if &v[1..6] != b"CD001" || v[0] == 255 {
            break;
        }
        // UCS-2 of levels 1 to 3
        let ucs2 = v[0] == 2 && v[88..90] == *b"%/" && matches!(v[90], b'@' | b'C' | b'E');
        let block = le16(v, 128) as usize;
        if !matches!(v[0], 1 | 2) || !block.is_power_of_two() || !(512..=SECTOR).contains(&block) {
            continue;
        }
        let out = if ucs2 { &mut joliet } else { &mut primary };
        walk(d, &v[156..190], block, ucs2, out);
    }
    joliet.append(&mut primary);
    joliet
}

/// The files below the directory of record `root`.
fn walk(d: &[u8], root: &[u8], block: usize, ucs2: bool, out: &mut Vec<File>) {
    let mut seen = std::collections::HashSet::new();
    let mut dirs = vec![(le32(root, 2) as usize, le32(root, 10) as usize, Vec::new())];
    while let Some((lba, size, path)) = dirs.pop() {
        if !seen.insert(lba) {
            continue;
        }
        let Some(data) = d.get(lba * block..).map(|r| &r[..size.min(r.len())]) else { continue };
        // the file whose next record continues it
        let mut open: Option<File> = None;
        let mut p = 0;
        while p < data.len() {
            let len = data[p] as usize;
            // (records do not cross sectors: a zero length pads to the next)
            if len == 0 {
                p = (p / SECTOR + 1) * SECTOR;
                continue;
            }
            let Some(r) = data.get(p..p + len).filter(|r| r.len() >= 33 && r.len() >= 33 + r[32] as usize) else {
                break;
            };
            p += len;
            let id = &r[33..33 + r[32] as usize];
            let flags = r[25];
            if id == [0] || id == [1] {
                continue;
            }
            let name = [&path[..], if path.is_empty() { b"" } else { b"/" }, &name(id, ucs2)].concat();
            let (at, size) = ((le32(r, 2) as usize + r[1] as usize) * block, le32(r, 10) as usize);
            if flags & 2 != 0 {
                dirs.push((le32(r, 2) as usize, size, name));
                continue;
            }
            let mut f = match open.take() {
                Some(f) if f.name == name => f,
                Some(f) => {
                    out.push(f);
                    File { name, extents: Vec::new(), rank: 1 + !ucs2 as u8 }
                }
                None => File { name, extents: Vec::new(), rank: 1 + !ucs2 as u8 },
            };
            f.extents.push(at..at.saturating_add(size));
            match flags & 0x80 != 0 {
                true => open = Some(f),
                false => out.push(f),
            }
        }
        out.extend(open);
    }
}

/// A file identifier as a name: UCS-2 to UTF-8, without its version (`;1`) or a trailing dot.
fn name(id: &[u8], ucs2: bool) -> Vec<u8> {
    let mut s = match ucs2 {
        true => char::decode_utf16(id.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .into_bytes(),
        false => id.to_vec(),
    };
    if let Some(semi) = s.iter().rposition(|&b| b == b';') {
        s.truncate(semi);
    }
    if s.len() > 1 && s.ends_with(b".") {
        s.pop();
    }
    s
}
