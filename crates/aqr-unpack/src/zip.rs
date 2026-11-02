//! ZIP archives read by their central directory (found from the end, past data before the
//! archive): the entries' names (UTF-8 where flagged or given by Info-ZIP's Unicode path field,
//! else of no declared encoding, decided for the archive: [`Undeclared`]), kinds (directories, files, Unix's
//! symbolic links), times (NTFS's field, else Info-ZIP's extended timestamp, else the MS-DOS
//! time as local time), Unix modes and owners, and Windows' attributes; the contents decoded
//! by entry on the pool's threads (stored, DEFLATE, bzip2, LZMA, Zstandard, xz), their CRC-32s
//! checked. Encrypted entries (the traditional encryption and WinZip's AES, [`aqr_foreign::crypt`])
//! are decrypted by the passphrase given ([`Listing::unlock`]), else left out (told), as are
//! those of PKWARE's strong encryption and methods not known.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use aqr_common::{crc32, pool};
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, Tree, WIN_ATTRS_ALLOWED, name_from};
use aqr_zip::zip::{self as z, u16_at, u32_at, u64_at};

use crate::build::{self, Builder, Named, Undeclared};
use crate::{Done, Filter, Select, Sink, codec, stream, sys};
use aqr_foreign::charset::Choice;
use aqr_foreign::crypt::{self, Fail, ZipCrypto};
use aqr_foreign::time;

/// Hosts of "version made by" whose names may separate by `\` and whose attributes are MS-DOS's.
fn dos_host(host: u8) -> bool {
    matches!(host, 0 | 11 | 14)
}

/// Hosts whose external attributes hold a Unix mode in their high 16 bits.
fn unix_host(host: u8) -> bool {
    matches!(host, 3 | 19)
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

/// An entry's data to decode: its content, its name (shown), method, body, size and CRC-32,
/// and how it is encrypted.
struct Unit {
    c: usize,
    name: String,
    method: u16,
    body: Range<usize>,
    size: u64,
    crc: u32,
    enc: Enc,
}

/// How an entry is encrypted.
#[derive(Clone, Copy, PartialEq)]
enum Enc {
    None,
    /// The traditional encryption, its header's last byte one of these (the CRC's high byte,
    /// or the time's where a data descriptor follows: either is taken, as encoders differ).
    Zip([u8; 2]),
    /// WinZip's AES of a strength (1 to 3), its CRC-32 checked or not (AE-1, AE-2).
    Aes(u8, bool),
}

/// Bytes of an encrypted entry decoded whole to check a passphrase by (a larger one is checked
/// by its header alone).
const CHECKED: usize = 64 << 20;

pub struct Listing<'d> {
    d: &'d [u8],
    b: Builder,
    units: Vec<Unit>,
    /// The contents of no data to decode (empty files).
    empty: Vec<usize>,
    notes: Vec<build::Note>,
    /// The passphrase of the encrypted entries ([`Listing::unlock`]), and those entries until
    /// it is taken (their names, metadata and data): not in the tree without it.
    pass: Option<Vec<u8>>,
    sealed: Vec<(Vec<Vec<u8>>, Extra, Unit)>,
    /// How the names of no declared encoding are taken.
    pub undeclared: Undeclared,
}

/// The blocks of extra field `x`: by id, the data.
fn fields(x: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut p = 0;
    std::iter::from_fn(move || {
        if p + 4 > x.len() {
            return None;
        }
        let (id, n) = (u16_at(x, p), u16_at(x, p + 2) as usize);
        let d = x.get(p + 4..p + 4 + n)?;
        p += 4 + n;
        Some((id, d))
    })
}

/// The times of extra fields `x` (the central directory's and the local header's): NTFS's
/// (modification, access, creation), else Info-ZIP's extended timestamp (of the local header:
/// modification, access, creation).
fn times(x: &mut Extra, central: &[u8], local: &[u8]) {
    for (id, f) in fields(central).chain(fields(local)) {
        match id {
            0x000a if f.len() >= 32 => {
                // (attributes of tag 1, 24 bytes, after 4 reserved)
                let mut p = 4;
                while p + 4 <= f.len() {
                    let (tag, n) = (u16_at(f, p), u16_at(f, p + 2) as usize);
                    if tag == 1 && n >= 24 && p + 4 + 24 <= f.len() {
                        let t = |k: usize| time::from_filetime(u64_at(f, p + 4 + 8 * k));
                        (x.mtime, x.atime, x.crtime) = (t(0), t(1), t(2));
                        return;
                    }
                    p += 4 + n;
                }
            }
            _ => {}
        }
    }
    for (id, f) in fields(local).chain(fields(central)) {
        if id == 0x5455 && !f.is_empty() {
            let flags = f[0];
            let mut p = 1;
            let mut next = |bit: u8| {
                if flags & bit == 0 || p + 4 > f.len() {
                    return None;
                }
                p += 4;
                Some(sys::unix(u32_at(f, p - 4) as i32 as i64))
            };
            let (m, a, c) = (next(1), next(2), next(4));
            x.mtime = m.or(x.mtime);
            x.atime = a.or(x.atime);
            x.crtime = c.or(x.crtime);
            return;
        }
    }
}

/// The owner of Info-ZIP's Unix fields (`ux`, else the old `UX`).
fn owner(x: &mut Extra, central: &[u8], local: &[u8]) {
    for (id, f) in fields(local).chain(fields(central)) {
        match id {
            0x7875 if f.len() >= 3 && f[0] == 1 => {
                let le = |b: &[u8]| (b.len() <= 8).then(|| b.iter().rev().fold(0u64, |v, &c| v << 8 | c as u64));
                let n = f[1] as usize;
                let Some(uid) = f.get(2..2 + n).and_then(le) else { continue };
                let Some(&m) = f.get(2 + n) else { continue };
                let Some(gid) = f.get(3 + n..3 + n + m as usize).and_then(le) else { continue };
                (x.uid, x.gid) = (Some(uid), Some(gid));
                return;
            }
            0x7855 if f.len() >= 4 => {
                (x.uid, x.gid) = (Some(u16_at(f, 0) as u64), Some(u16_at(f, 2) as u64));
                return;
            }
            _ => {}
        }
    }
}

/// The path of a central header: UTF-8 where flagged, else that of Info-ZIP's Unicode path
/// field where it is of this path, else of no declared encoding (spec, "Entry names from ZIP").
fn entry_name(raw: &[u8], flags: u16, central: &[u8]) -> Named {
    if flags & 0x800 != 0 {
        return Named::utf8(raw.to_vec());
    }
    for (id, f) in fields(central) {
        if id == 0x7075
            && f.len() >= 5
            && f[0] == 1
            && u32_at(f, 1) == crc32::hash(raw)
            && let Named::Text(t) = Named::utf8(f[5..].to_vec())
        {
            return Named::Text(t);
        }
    }
    Named::Bytes(raw.to_vec())
}

impl<'d> Listing<'d> {
    /// Archive `d`, its names of no declared encoding taken as `choice` asks.
    pub fn read(d: &'d [u8], choice: &Choice) -> Result<Listing<'d>, String> {
        let (base, cd) = z::find_end(d).ok_or("ZIP: no central directory (not a ZIP archive, or cut short)")?;
        let mut headers = Vec::new();
        let mut p = cd.start;
        while p < cd.end {
            let c = z::central_at(d, p).ok_or("ZIP: a damaged central directory")?;
            if d[p..p + 4] != z::CENTRAL {
                return Err("ZIP: a damaged central directory".into());
            }
            p = c.comment.end;
            let f = c.fixed(d);
            let name = entry_name(&d[c.name.clone()], u16_at(f, 4), &d[c.extra.clone()]);
            headers.push((c, name));
        }
        let undeclared = Undeclared::decide(choice, headers.iter().filter_map(|h| h.1.undeclared()), None);
        let mut l = Listing {
            d,
            b: Builder::default(),
            units: Vec::new(),
            empty: Vec::new(),
            notes: Vec::new(),
            pass: None,
            sealed: Vec::new(),
            undeclared,
        };
        for (c, name) in &headers {
            l.entry(c, name, base)?;
        }
        Ok(l)
    }

    fn entry(&mut self, c: &z::Central, path: &Named, base: i64) -> Result<(), String> {
        let d = self.d;
        let f = c.fixed(d);
        let (host, flags, method) = ((u16_at(f, 0) >> 8) as u8, u16_at(f, 4), u16_at(f, 6));
        let external = u32_at(f, 34);
        let (offset, info) = c.info(d);
        let cx = &d[c.extra.clone()];
        let name = self.undeclared.path(path, dos_host(host));
        let shown = String::from_utf8_lossy(&name).into_owned();
        let lax = build::lax(&name, false);
        let Some(names) = build::split(&name, false) else {
            self.notes.push((lax.clone(), format!("{shown}: a path reaching out of the directory, not extracted")));
            return Ok(());
        };
        // the local header, and the body after it
        let local = usize::try_from((offset as i64).wrapping_add(base))
            .ok()
            .filter(|&at| d.get(at..at + 4) == Some(&z::LOCAL[..]) && at + 30 <= d.len())
            .ok_or_else(|| format!("ZIP: {shown}: no local header where the directory tells"))?;
        let (n, m) = (u16_at(d, local + 26) as usize, u16_at(d, local + 28) as usize);
        let lx = d.get(local + 30 + n..local + 30 + n + m).unwrap_or(&[]);
        let start = local + 30 + n + m;
        let body = usize::try_from(info.csize)
            .ok()
            .and_then(|s| start.checked_add(s))
            .filter(|&e| e <= d.len())
            .map(|e| start..e)
            .ok_or_else(|| format!("ZIP: {shown}: cut short"))?;
        let mode = if unix_host(host) { external >> 16 } else { 0 };
        let dir = name.ends_with(b"/") || dos_host(host) && external & 0x10 != 0 || mode & S_IFMT == S_IFDIR;
        let mut x = Extra { mtime: time::from_dos(u16_at(f, 10), u16_at(f, 8)), ..Extra::default() };
        times(&mut x, cx, lx);
        owner(&mut x, cx, lx);
        if mode != 0 {
            x.mode = Some(mode & 0o7777);
        }
        if dos_host(host) && external & WIN_ATTRS_ALLOWED & 0xff != 0 {
            x.win_attrs = Some(external & WIN_ATTRS_ALLOWED & 0xff);
        }
        if dir {
            let r = match names.is_empty() {
                true => self.b.add(&[], None, x),
                false => self.b.add(&names, Some(EntryNode::Dir), x),
            };
            if let Err(e) = r {
                self.notes.push((lax.clone(), e));
            }
            return Ok(());
        }
        let (enc, method) = match (flags & 1 != 0, method) {
            (true, _) if flags & 0x40 != 0 => {
                self.notes.push((
                    lax.clone(),
                    format!("{shown}: of PKWARE's strong encryption, not known here, not extracted"),
                ));
                return Ok(());
            }
            // WinZip's AES: its field gives the strength and the method
            (_, 99) => match fields(cx).chain(fields(lx)).find(|f| f.0 == 0x9901 && f.1.len() >= 7) {
                Some((_, a)) => (Enc::Aes(a[4], u16_at(a, 0) == 1), u16_at(a, 5)),
                None => {
                    self.notes.push((lax.clone(), format!("{shown}: of AES without its field, not extracted")));
                    return Ok(());
                }
            },
            (true, m) => (Enc::Zip([(info.crc >> 24) as u8, (u16_at(f, 8) >> 8) as u8]), m),
            (false, m) => (Enc::None, m),
        };
        if ![0, 8, 12, 14, 93, 95].contains(&method) {
            let what = match method {
                9 => "Deflate64".into(),
                98 => "PPMd".into(),
                m => format!("method {m}"),
            };
            self.notes.push((lax.clone(), format!("{shown}: of {what}, not known here, not extracted")));
            return Ok(());
        }
        let unit =
            |c| Unit { c, name: shown.clone(), method, body: body.clone(), size: info.usize, crc: info.crc, enc };
        let node = if mode & S_IFMT == S_IFLNK {
            if enc != Enc::None {
                self.notes.push((lax.clone(), format!("{shown}: an encrypted link, not extracted")));
                return Ok(());
            }
            // (the target, the entry's data, read now: the tree has it)
            let target = decode(d, &unit(0), None).map_err(|e| format!("ZIP: {shown}: {}", told(&e)))?;
            if target.is_empty() {
                self.notes.push((lax.clone(), format!("{shown}: a link to nothing, not extracted")));
                return Ok(());
            }
            EntryNode::Link(name_from(&self.undeclared.target(&target, path.undeclared().is_some())))
        } else if enc != Enc::None {
            let names = names.iter().map(|n| n.to_vec()).collect();
            self.sealed.push((names, x, unit(0)));
            return Ok(());
        } else {
            let c = self.b.content();
            match info.usize {
                0 => self.empty.push(c),
                _ => self.units.push(unit(c)),
            }
            EntryNode::File(c)
        };
        if let Err(e) = self.b.add(&names, Some(node), x) {
            self.notes.push((lax.clone(), e));
        }
        Ok(())
    }

    pub fn units(&self) -> Vec<String> {
        self.units.iter().map(|u| u.name.clone()).collect()
    }

    /// Whether entries are encrypted.
    pub fn encrypted(&self) -> bool {
        !self.sealed.is_empty() || self.units.iter().any(|u| u.enc != Enc::None)
    }

    /// Whether `pass` decrypts the encrypted entries, taken for them where it does: as the
    /// smallest of them is decoded by it (or its header tells, where it is large).
    pub fn unlock(&mut self, pass: &[u8]) -> bool {
        if self.pass.as_deref() == Some(pass) {
            return true;
        }
        let Some(u) = self.sealed.iter().map(|s| &s.2).min_by_key(|u| u.body.len()) else {
            return false;
        };
        let body = &self.d[u.body.clone()];
        let ok = match u.enc {
            _ if body.len() <= CHECKED => decode(self.d, u, Some(pass)).is_ok(),
            Enc::Zip(checks) => ZipCrypto::open(pass, &body[..12.min(body.len())], &checks).is_some(),
            Enc::Aes(strength, _) => crypt::winzip_aes_verifies(pass, strength, body),
            Enc::None => true,
        };
        if ok {
            self.pass = Some(pass.to_vec());
            for (names, x, mut u) in std::mem::take(&mut self.sealed) {
                let names: Vec<&[u8]> = names.iter().map(|n| &n[..]).collect();
                u.c = self.b.content();
                let c = u.c;
                match u.size {
                    0 => self.empty.push(c),
                    _ => self.units.push(u),
                }
                if let Err(e) = self.b.add(&names, Some(EntryNode::File(c)), x) {
                    self.notes.push((names.iter().map(|n| n.to_vec()).collect(), e));
                }
            }
        }
        ok
    }

    pub fn tree(&self) -> &Tree {
        &self.b.tree
    }

    pub fn notes(&self) -> &[build::Note] {
        &self.notes
    }

    /// The notes of the encrypted entries without their passphrase.
    pub fn sealed_notes(&self) -> Vec<build::Note> {
        let note = |u: &Unit| format!("{}: encrypted, and no passphrase given, not extracted", u.name);
        self.sealed.iter().map(|(names, _, u)| (names.clone(), note(u))).collect()
    }

    pub fn unpack(&self, threads: usize, sink: &dyn Sink, select: Option<&Select>) -> Result<Done, String> {
        let mut skipped = crate::tell(&self.notes, select, sink);
        skipped += crate::tell(&self.sealed_notes(), select, sink);
        let wanted = select.map(|s| s.contents(&self.b.tree, self.b.contents));
        let want = |c: usize| self.b.live(c) && build::wanted(wanted.as_deref(), c);
        let tree = self.b.tree.clone();
        let all: Vec<usize> = (0..tree.entries.len()).collect();
        if !all.is_empty() {
            sink.entries(&tree, &all)?;
        }
        for &c in self.empty.iter().filter(|&&c| want(c)) {
            sink.content(c, Vec::new())?;
        }
        // (by their indices among all, as Opened::units has them)
        let units: Vec<usize> = (0..self.units.len()).filter(|&k| want(self.units[k].c)).collect();
        let packed: u64 = units.iter().map(|&k| self.units[k].body.len() as u64).sum();
        let total: u64 = units.iter().map(|&k| self.units[k].size).sum();
        let (done_packed, done) = (AtomicU64::new(0), AtomicU64::new(0));
        let failed = AtomicBool::new(false);
        let left = AtomicU64::new(0);
        let made = pool::par_map(units.len(), threads, |k| -> Result<(), String> {
            if failed.load(Relaxed) {
                return Ok(());
            }
            let k = units[k];
            let u = &self.units[k];
            sink.step(Step::Started(k));
            let r = match decode(self.d, u, self.pass.as_deref()) {
                Ok(data) => sink.content(u.c, data),
                // (an encrypted entry left out, as it cannot be told from a damaged one)
                Err(e) if u.enc != Enc::None => {
                    sink.note(&format!("{}: {}, not extracted", u.name, told(&e)));
                    left.fetch_add(1, Relaxed);
                    Ok(())
                }
                Err(e) => Err(format!("ZIP: {}: {}", u.name, told(&e))),
            };
            sink.step(Step::Ended(k));
            if r.is_err() {
                failed.store(true, Relaxed);
            }
            let p = done_packed.fetch_add(u.body.len() as u64, Relaxed) + u.body.len() as u64;
            let n = done.fetch_add(u.size, Relaxed) + u.size;
            sink.step(Step::Reached(Reached { decompressed: (p, packed), evaluated: (n, total) }));
            r
        });
        made.into_iter().collect::<Result<(), String>>()?;
        Ok(Done { tree, skipped: skipped + left.into_inner() as usize })
    }
}

/// What failure `f` of an entry's data is, as told.
fn told(f: &Fail) -> String {
    match f {
        Fail::Wrong => "encrypted, and no passphrase given opens it".into(),
        Fail::Bad(e) => e.clone(),
    }
}

/// The data of unit `u` of archive `d` (decrypted by `pass`), its size and CRC-32 checked. An
/// encrypted one fails as of a wrong passphrase, but where it is damaged as nothing else is.
fn decode(d: &[u8], u: &Unit, pass: Option<&[u8]>) -> Result<Vec<u8>, Fail> {
    let body = &d[u.body.clone()];
    let decrypted;
    let body = match (u.enc, pass) {
        (Enc::None, _) => body,
        (_, None) => return Err(Fail::Wrong),
        (Enc::Zip(checks), Some(p)) => {
            if body.len() < 12 {
                return Err(Fail::Bad("cut short".into()));
            }
            decrypted = ZipCrypto::open(p, body, &checks).ok_or(Fail::Wrong)?;
            &decrypted
        }
        (Enc::Aes(strength, _), Some(p)) => {
            decrypted = crypt::winzip_aes_decrypt(p, strength, body)?;
            &decrypted
        }
    };
    // (of the traditional encryption, a passphrase taken by its header may yet be wrong)
    let wrong = |e: String| if matches!(u.enc, Enc::Zip(_)) { Fail::Wrong } else { Fail::Bad(e) };
    let data = plain(body, u).map_err(wrong)?;
    if data.len() as u64 != u.size {
        return Err(wrong("not of its size".into()));
    }
    // (AE-2 has no CRC-32: its code authenticates the data)
    if !matches!(u.enc, Enc::Aes(_, false)) && crc32::hash(&data) != u.crc {
        return Err(wrong("CRC mismatch".into()));
    }
    Ok(data)
}

/// The data of `body` (decrypted) of unit `u`.
fn plain(body: &[u8], u: &Unit) -> Result<Vec<u8>, String> {
    let size = usize::try_from(u.size).map_err(|_| "too large")?;
    let data = match u.method {
        0 if body.len() != size => return Err("not of its size".into()),
        0 => body.to_vec(),
        8 => codec::inflate(body, size, size)?.0,
        12 => codec::bzip2(body, size)?,
        14 => {
            // version (2 bytes), the properties' size (2) and the properties
            let n = body.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or("LZMA: no header")?;
            let props = body.get(4..4 + n).filter(|p| p.len() >= 5).ok_or("LZMA: no header")?;
            codec::lzma(&body[4 + n..], props[0], Some(u.size), size)?.0
        }
        93 => codec::zstd(body, size, size)?,
        95 => {
            let mut out = codec::room(size)?;
            let mut over = false;
            stream::decode(Some(Filter::Xz), body, 1, &mut |v, at| {
                over = out.len() + v.len() - at > size;
                out.extend_from_slice(&v[at..]);
                !over
            })?;
            out
        }
        m => return Err(format!("method {m} not known")),
    };
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ZIP archive of `entries` (names and data, stored) made on `host`, of `flags`.
    pub(crate) fn zip_of(entries: &[(&[u8], &[u8])], host: u8, flags: u16) -> Vec<u8> {
        let (mut out, mut cd) = (Vec::new(), Vec::new());
        for (name, data) in entries {
            let crc = crc32::hash(data);
            let at = out.len() as u32;
            let common = |v: &mut Vec<u8>| {
                v.extend_from_slice(&20u16.to_le_bytes());
                v.extend_from_slice(&flags.to_le_bytes());
                v.extend_from_slice(&[0, 0, 0, 0, 0x21, 0]);
                v.extend_from_slice(&crc.to_le_bytes());
                v.extend_from_slice(&(data.len() as u32).to_le_bytes());
                v.extend_from_slice(&(data.len() as u32).to_le_bytes());
                v.extend_from_slice(&(name.len() as u16).to_le_bytes());
                v.extend_from_slice(&0u16.to_le_bytes());
            };
            out.extend_from_slice(&z::LOCAL);
            common(&mut out);
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            cd.extend_from_slice(&z::CENTRAL);
            cd.extend_from_slice(&(((host as u16) << 8) | 20).to_le_bytes());
            common(&mut cd);
            cd.extend_from_slice(&[0; 6]);
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&at.to_le_bytes());
            cd.extend_from_slice(name);
        }
        let (start, n) = (out.len() as u32, entries.len() as u16);
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// The paths of `t`'s entries, their names decoded (a name kept as bytes in `<>`), a
    /// directory's ending with `/`.
    pub(crate) fn paths(t: &Tree) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &t.entries {
            let (b, raw) = aqr_formula::aqr::name_bytes(&e.name);
            let name = if raw {
                format!("<{}>", b.iter().map(|c| format!("{c:02x}")).collect::<String>())
            } else {
                String::from_utf8_lossy(b).into_owned()
            };
            let parent = e.parent.map_or(String::new(), |p| out[p].clone());
            let dir = if e.node == EntryNode::Dir { "/" } else { "" };
            out.push(format!("{parent}{name}{dir}"));
        }
        out
    }

    #[test]
    fn shift_jis_backslash() {
        // (MS-DOS's names of Shift_JIS, `\` between them: 表 is 95 5C, ソ 83 5C)
        let sjis = |s: &str| encoding_rs::SHIFT_JIS.encode(s).0.into_owned();
        let (a, b) = (sjis("表示\\ソフト.txt"), sjis("予定表"));
        let zip = zip_of(&[(&a, b"x"), (&b, b"y")], 0, 0);
        let l = Listing::read(&zip, &Choice::Auto).unwrap();
        assert_eq!(paths(l.tree()), ["表示/", "表示/ソフト.txt", "予定表"]);
        assert_eq!(l.undeclared.d.charset, Some(aqr_foreign::charset::Charset::Std(encoding_rs::SHIFT_JIS)));
        // kept as they are, split between Shift_JIS's characters still
        let l = Listing::read(&zip, &Choice::Raw).unwrap();
        assert_eq!(paths(l.tree()), ["<955c8ea6>/", "<955c8ea6>/<835c837483672e747874>", "<975c92e8955c>"]);
    }

    #[test]
    fn declared_wrongly() {
        // (flagged UTF-8, but of EUC-KR; and one flagged rightly beside it)
        let euc = encoding_rs::EUC_KR.encode("사진/여름 휴가.jpg").0.into_owned();
        let zip = zip_of(&[(&euc, b"x")], 3, 0x800);
        let l = Listing::read(&zip, &Choice::Auto).unwrap();
        assert_eq!(paths(l.tree()), ["사진/", "사진/여름 휴가.jpg"]);
        let utf8 = "보고서.txt".as_bytes();
        let mixed = zip_of(&[(&euc, b"x"), (utf8, b"y")], 0, 0);
        let l = Listing::read(&mixed, &Choice::Auto).unwrap();
        // (the one of UTF-8 taken as UTF-8, the set guessed of the other)
        assert_eq!(l.undeclared.n, 2);
        assert_eq!(paths(l.tree()), ["사진/", "사진/여름 휴가.jpg", "보고서.txt"]);
    }
}
