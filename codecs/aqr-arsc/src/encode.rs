//! The streams of a resource table (FORMAT.md §6, *encoder*): each chunk is taken where it is
//! laid out as the module rebuilds it, else kept as it is; the streams are decoded again
//! before they are given.

use aqr_common::varint::{self, zz};

use crate::arsc::{self, STRING, Seen, le16, le32, mode, s, ty};

/// Whether `d` starts like a resource table: the table's chunk header, of its whole length.
pub fn starts(d: &[u8]) -> bool {
    d.len() >= 12
        && le16(d, 0).ok() == Some(ty::TABLE)
        && le16(d, 2).ok() == Some(12)
        && le32(d, 4).ok().is_some_and(|n| n as usize == d.len())
}

/// The inputs of the module for resource table `d` ([`arsc::INPUTS`]), or `None` where it is
/// not one the module rebuilds.
pub fn split(d: &[u8]) -> Option<Vec<Vec<u8>>> {
    if !starts(d) {
        return None;
    }
    let mut e = Enc { st: vec![Vec::new(); s::N], seen: Seen::default(), last_str: 0 };
    e.chunk(d, 0, d.len(), 0)?;
    let st = arsc::pack(&e.st);
    let refs: Vec<&[u8]> = st.iter().map(|v| v.as_slice()).collect();
    (arsc::decode(&refs, d.len()).ok()? == d).then_some(st)
}

enum Saved {
    Type(u8, Vec<(u32, u8, u32)>),
    All(Seen),
}

struct Enc {
    st: Vec<Vec<u8>>,
    seen: Seen,
    last_str: u32,
}

impl Enc {
    /// The chunk at `p` of `d`, nested `depth` deep, within `end`; its end.
    fn chunk(&mut self, d: &[u8], p: usize, end: usize, depth: usize) -> Option<usize> {
        let t = le16(d, p).ok()?;
        let hs = le16(d, p + 2).ok()? as usize;
        let size = le32(d, p + 4).ok()? as usize;
        let stop = p.checked_add(size).filter(|&e| e <= end)?;
        if hs < 8 || size < hs {
            return None;
        }
        self.st[s::HEAD].extend_from_slice(&d[p..p + hs]);
        let at = self.st.iter().map(Vec::len).collect::<Vec<_>>();
        // (what earlier entries predict, to go back to where the chunk is not taken: of its type,
        // or all of it for a container)
        let seen = match t {
            ty::TYPE if hs >= 20 => Some(Saved::Type(d[p + 8], self.seen.of_type(d[p + 8]))),
            ty::TABLE | ty::PACKAGE => Some(Saved::All(self.seen.clone())),
            _ => None,
        };
        let last_str = self.last_str;
        let m = match t {
            ty::TABLE | ty::PACKAGE if depth < arsc::DEPTH && self.children(d, p + hs, stop, depth + 1) => {
                mode::CONTAINER
            }
            ty::STRING_POOL if self.pool(d, p, hs, stop).is_some() => mode::POOL,
            ty::TYPE if self.entries(d, p, hs, stop).is_some() => mode::TYPE,
            _ => {
                // (what was written of it is dropped)
                for (v, &n) in self.st.iter_mut().zip(&at) {
                    v.truncate(n);
                }
                match seen {
                    Some(Saved::Type(tid, v)) => self.seen.set_type(tid, v),
                    Some(Saved::All(v)) => self.seen = v,
                    None => {}
                }
                self.last_str = last_str;
                self.st[s::RAW].extend_from_slice(&d[p + hs..stop]);
                mode::RAW
            }
        };
        // (the mode after the header, though the children's heads came before it)
        let head = &mut self.st[s::HEAD];
        head.insert(at[s::HEAD], m);
        Some(stop)
    }

    /// Chunks one after another from `p` to `stop`: whether they are.
    fn children(&mut self, d: &[u8], mut p: usize, stop: usize, depth: usize) -> bool {
        while p < stop {
            match self.chunk(d, p, stop, depth) {
                Some(e) => p = e,
                None => return false,
            }
        }
        true
    }

    fn pool(&mut self, d: &[u8], p: usize, hs: usize, stop: usize) -> Option<()> {
        if hs < 28 {
            return None;
        }
        let (n, ns, flags) = (le32(d, p + 8).ok()? as usize, le32(d, p + 12).ok()? as usize, le32(d, p + 16).ok()?);
        let (strings, styles) = (le32(d, p + 20).ok()? as usize, le32(d, p + 24).ok()? as usize);
        if n == 0 || strings != hs + 4 * (n + ns) || (ns > 0 && (styles < strings || p + styles > stop)) {
            return None;
        }
        let offs = p + hs;
        self.st[s::STYLE].extend_from_slice(d.get(offs + 4 * n..p + strings)?);
        let base = p + strings;
        let mut at = base;
        let mut len = Vec::new();
        for i in 0..n {
            if le32(d, offs + 4 * i).ok()? as usize != at - base {
                return None;
            }
            len.clear();
            if flags & 0x100 != 0 {
                let s0 = at;
                // (the lengths are written again from the bytes: they must be as written)
                let n8 = {
                    let mut q = at + if d.get(at)? & 0x80 != 0 { 2 } else { 1 };
                    let b = *d.get(q)?;
                    let n =
                        if b & 0x80 != 0 { ((b as usize & 0x7f) << 8) | *d.get(q + 1)? as usize } else { b as usize };
                    q += if b & 0x80 != 0 { 2 } else { 1 };
                    at = q;
                    n
                };
                let s = d.get(at..at + n8)?;
                if *d.get(at + n8)? != 0 || s.contains(&0) {
                    return None;
                }
                arsc::put_len8(&mut len, arsc::utf16_len(s))?;
                arsc::put_len8(&mut len, n8)?;
                if d[s0..at] != len[..] {
                    return None;
                }
                self.st[s::TEXT].extend_from_slice(s);
                self.st[s::TEXT].push(0);
                at += n8 + 1;
            } else {
                let u = le16(d, at).ok()? as usize;
                let (n16, q) = if u & 0x8000 != 0 {
                    (((u & 0x7fff) << 16) | le16(d, at + 2).ok()? as usize, at + 4)
                } else {
                    (u, at + 2)
                };
                arsc::put_len16(&mut len, n16)?;
                if d[at..q] != len[..] {
                    return None;
                }
                let s = d.get(q..q + 2 * n16)?;
                if le16(d, q + 2 * n16).ok()? != 0 || s.as_chunks::<2>().0.contains(&[0, 0]) {
                    return None;
                }
                self.st[s::TEXT16].extend_from_slice(s);
                self.st[s::TEXT16].extend_from_slice(&[0, 0]);
                at = q + 2 * n16 + 2;
            }
        }
        let pad = if ns > 0 { p + styles } else { stop };
        if at > pad || d[at..pad].iter().any(|&b| b != 0) {
            return None;
        }
        self.st[s::STYLE].extend_from_slice(&d[pad..stop]);
        Some(())
    }

    fn entries(&mut self, d: &[u8], p: usize, hs: usize, stop: usize) -> Option<()> {
        if hs < 20 || d[p + 9] != 0 {
            return None;
        }
        let tid = d[p + 8];
        let (n, es) = (le32(d, p + 12).ok()? as usize, le32(d, p + 16).ok()? as usize);
        if es != hs.checked_add(n.checked_mul(4)?)? || p + es > stop {
            return None;
        }
        let base = p + es;
        let mut at = base;
        for i in 0..n {
            let o = le32(d, p + hs + 4 * i).ok()?;
            if o == u32::MAX {
                self.st[s::PRESENT].push(0);
                continue;
            }
            if o as usize != at - base {
                return None;
            }
            self.st[s::PRESENT].push(1);
            let (size, flags, key) = (le16(d, at).ok()?, le16(d, at + 2).ok()?, le32(d, at + 4).ok()?);
            let complex = flags & 1 != 0;
            // (compact entries, and others of another size, are not taken)
            if flags & 8 != 0 || size != if complex { 16 } else { 8 } {
                return None;
            }
            varint::put(&mut self.st[s::FLAGS], flags as u64);
            let (pk, pt, pv) = self.seen.get(tid, i);
            varint::put(&mut self.st[s::KEY], if key == pk { 0 } else { key as u64 + 1 });
            if complex {
                self.st[s::MAP].extend_from_slice(d.get(at + 8..at + 12)?);
                let count = le32(d, at + 12).ok()?;
                varint::put(&mut self.st[s::FLAGS], count as u64);
                let end = (at + 16).checked_add((count as usize).checked_mul(12)?)?;
                self.st[s::MAP].extend_from_slice(d.get(at + 16..end)?);
                self.seen.set(tid, i, (key, pt, pv));
                at = end;
            } else {
                let v = d.get(at + 8..at + 16)?;
                if v[..3] != [8, 0, 0] {
                    return None;
                }
                let (vt, data) = (v[3], le32(v, 4).ok()?);
                self.st[s::VTYPE].push(vt);
                if vt == STRING {
                    varint::put(&mut self.st[s::VSTR], zz(data as i64 - self.last_str as i64));
                    self.last_str = data;
                } else {
                    varint::put(&mut self.st[s::VDATA], if vt == pt && data == pv { 0 } else { data as u64 + 1 });
                }
                self.seen.set(tid, i, (key, vt, data));
                at += 16;
            }
            if at > stop {
                return None;
            }
        }
        (at == stop).then_some(())
    }
}
