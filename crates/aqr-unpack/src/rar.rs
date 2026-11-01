//! RAR archives (RAR5, and RAR 1.5–4.x of RAR 2.9's algorithm, that of RAR 3.x and 4.x) read by
//! their headers (aqr-rar's readers): the files' names (RAR5's UTF-8; RAR 3.x's Unicode names,
//! else of the system's code page where they are not UTF-8), kinds (directories, files, symbolic
//! links and junctions), times (modification, creation, access) and attributes (Windows', or the
//! Unix mode); the streams decoded by aqr-rar on the pool's threads, a solid group (or a file) at
//! a time, its files' contents given as each is decoded and their CRC-32s checked. Encrypted
//! files, files split across volumes, RAR 1.5's and 2.0's algorithms, hard links and copies of
//! files are left out (told), and so are the files after them in a solid group.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use aqr_common::{crc32, pool};
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, Time, Tree, WIN_ATTRS_ALLOWED, name_from};
use aqr_rar::{lz3, lz5, rar4, rar5};

use crate::build::{self, Builder};
use crate::{Done, Select, Sink, sys};

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// How a unit's data is decoded.
#[derive(Clone, Copy)]
enum How {
    /// A stored file: its data.
    Stored,
    /// A RAR5 stream (the algorithm of RAR 7 or not, the dictionary).
    V5(bool, u64),
    /// A RAR 2.9 stream (the window).
    V3(u64),
}

/// A file of a unit: its content (`None` where left out of the tree), name (shown), packed data,
/// size and CRC-32 (where the header has one).
struct File {
    c: Option<usize>,
    name: String,
    data: Range<usize>,
    size: usize,
    crc: Option<u32>,
}

/// A stream to decode: its files in order, how, what it is shown as; `left` where it cannot be
/// decoded here (why).
struct Unit {
    how: How,
    files: Vec<File>,
    name: String,
    left: Option<String>,
}

pub struct Listing<'d> {
    d: &'d [u8],
    b: Builder,
    units: Vec<Unit>,
    /// The contents of no data (empty files).
    empty: Vec<usize>,
    notes: Vec<build::Note>,
}

/// A RAR5 header's time.
fn time5(t: rar5::Time) -> Option<Time> {
    match t {
        rar5::Time::Unix(s, ns) => Some(Time { secs: s as i64, nanos: ns }),
        rar5::Time::Filetime(t) => sys::filetime(t),
    }
}

/// A RAR 3.x header's time: an MS-DOS one (local) and nanoseconds past it (up to 2 s).
fn time4(t: (u32, u32)) -> Option<Time> {
    let base = sys::dos((t.0 >> 16) as u16, t.0 as u16)?;
    let ns = base.nanos as u64 + t.1 as u64;
    Some(Time { secs: base.secs + (ns / 1_000_000_000) as i64, nanos: (ns % 1_000_000_000) as u32 })
}

/// What a file of the listing is.
enum Kind {
    Dir,
    /// A file's data: stored, or of a stream (continuing the solid group before it where `solid`).
    Data {
        how: How,
        solid: bool,
        size: usize,
        crc: Option<u32>,
    },
    /// A symbolic link or junction (`win`: its `win-link-type`) to `target`.
    Link {
        target: Vec<u8>,
        win: Option<u8>,
    },
    /// Not extracted: why (a file of data, breaking a solid group where `data`).
    Left {
        why: String,
        data: bool,
    },
}

/// A file as read from a header.
struct Head {
    names: Option<Vec<u8>>,
    backslash: bool,
    shown: String,
    data: Range<usize>,
    kind: Kind,
    extra: Extra,
}

impl<'d> Listing<'d> {
    pub fn read(d: &'d [u8]) -> Result<Listing<'d>, String> {
        let heads = if rar5::starts(d) {
            heads5(d)?
        } else if rar4::starts(d) {
            heads4(d)?
        } else {
            return Err("RAR: not a RAR archive".into());
        };
        let mut l = Listing { d, b: Builder::default(), units: Vec::new(), empty: Vec::new(), notes: Vec::new() };
        l.files(heads);
        Ok(l)
    }

    fn files(&mut self, heads: Vec<Head>) {
        // (the unit a solid file continues: the last stream's, while nothing broke it)
        let mut open: Option<usize> = None;
        for h in heads {
            let lax = h.names.as_deref().map_or_else(Vec::new, |n| build::lax(n, h.backslash));
            let shown = h.shown;
            let node = match h.kind {
                Kind::Dir => Some(EntryNode::Dir),
                Kind::Left { why, data } => {
                    self.notes.push((lax.clone(), format!("{shown}: {why}, not extracted")));
                    if data {
                        // (the files continuing its solid group cannot be decoded)
                        if let Some(u) = open.take() {
                            self.units[u].left.get_or_insert_with(|| format!("after {shown}, which is not"));
                        }
                    }
                    continue;
                }
                Kind::Link { target, win } => {
                    let mut x = h.extra.clone();
                    x.win_link_type = win;
                    let names = h.names.as_deref().and_then(|n| build::split(n, h.backslash));
                    match names {
                        None => self.notes.push((
                            lax.clone(),
                            format!("{shown}: a path reaching out of the directory, not extracted"),
                        )),
                        Some(names) => {
                            if let Err(e) = self.b.add(&names, Some(EntryNode::Link(name_from(&target))), x) {
                                self.notes.push((lax.clone(), e));
                            }
                        }
                    }
                    continue;
                }
                Kind::Data { how, solid, size, crc } => {
                    let names = h.names.as_deref().and_then(|n| build::split(n, h.backslash));
                    let c = match &names {
                        Some(n) if !n.is_empty() => Some(self.b.content()),
                        _ => {
                            self.notes.push((
                                lax.clone(),
                                format!("{shown}: a path reaching out of the directory, not extracted"),
                            ));
                            None
                        }
                    };
                    let f = File { c, name: shown.clone(), data: h.data.clone(), size, crc };
                    match how {
                        How::Stored => {
                            open = None;
                            if size == 0 {
                                if let Some(c) = c {
                                    self.empty.push(c);
                                }
                            } else {
                                self.units.push(Unit { how, files: vec![f], name: String::new(), left: None });
                            }
                        }
                        _ => match open {
                            Some(u) if solid => self.units[u].files.push(f),
                            _ => {
                                let left = solid.then(|| "a solid group's file without the files before it".into());
                                self.units.push(Unit { how, files: vec![f], name: String::new(), left });
                                open = Some(self.units.len() - 1);
                            }
                        },
                    }
                    let Some(names) = names.filter(|n| !n.is_empty()) else { continue };
                    if let Err(e) = self.b.add(&names, Some(EntryNode::File(c.unwrap())), h.extra) {
                        self.notes.push((lax.clone(), e));
                    }
                    continue;
                }
            };
            let Some(names) = h.names.as_deref().and_then(|n| build::split(n, h.backslash)) else {
                self.notes.push((lax.clone(), format!("{shown}: a path reaching out of the directory, not extracted")));
                continue;
            };
            let r = if names.is_empty() { self.b.add(&[], None, h.extra) } else { self.b.add(&names, node, h.extra) };
            if let Err(e) = r {
                self.notes.push((lax.clone(), e));
            }
        }
        for u in &mut self.units {
            u.name = match u.files.len() {
                1 => u.files[0].name.clone(),
                n => format!("{} and {} more files", u.files[0].name, n - 1),
            };
        }
    }

    pub fn units(&self) -> Vec<String> {
        self.units.iter().map(|u| u.name.clone()).collect()
    }

    /// Decodes unit `u`, giving each file's content that `want` (by its index) to `give` as it
    /// is decoded: as far as the last of them.
    fn decode(
        &self,
        u: &Unit,
        want: &dyn Fn(usize) -> bool,
        give: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), String>,
    ) -> Result<(), String> {
        let upto = (0..u.files.len()).rfind(|&k| want(k)).map_or(0, |k| k + 1);
        let check = |f: &File, data: &[u8]| {
            if f.crc.is_some_and(|c| crc32::hash(data) != c) {
                return Err(format!("{}: CRC mismatch", f.name));
            }
            Ok(())
        };
        match u.how {
            How::Stored => {
                let f = &u.files[0];
                let data = &self.d[f.data.clone()];
                check(f, data)?;
                give(0, data.to_vec())
            }
            How::V5(v1, dict) => {
                let mut dec = lz5::Decoder5::new(v1, dict);
                for (k, f) in u.files[..upto].iter().enumerate() {
                    let last = dec.last();
                    let at = dec.out.len();
                    let st = dec.file(&self.d[f.data.clone()], f.size).map_err(|e| format!("{}: {e}", f.name))?;
                    if !want(k) {
                        continue;
                    }
                    let mut data = dec.out[at..].to_vec();
                    lz5::unfilter(&mut data, &st.placed(last)).map_err(|e| format!("{}: {e}", f.name))?;
                    check(f, &data)?;
                    give(k, data)?;
                }
                Ok(())
            }
            How::V3(dict) => {
                let mut dec = lz3::Decoder3::new(dict);
                for (k, f) in u.files[..upto].iter().enumerate() {
                    let at = dec.out.len();
                    let st = dec.file(&self.d[f.data.clone()], f.size).map_err(|e| format!("{}: {e}", f.name))?;
                    if !want(k) {
                        continue;
                    }
                    let mut data = dec.out[at..].to_vec();
                    lz3::unfilter(&mut data, &st.filters).map_err(|e| format!("{}: {e}", f.name))?;
                    check(f, &data)?;
                    give(k, data)?;
                }
                Ok(())
            }
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.b.tree
    }

    pub fn unpack(&self, threads: usize, sink: &dyn Sink, select: Option<&Select>) -> Result<Done, String> {
        let mut skipped = crate::tell(&self.notes, select, sink);
        let wanted = select.map(|s| s.contents(&self.b.tree, self.b.contents));
        let want = |f: &File| f.c.is_some_and(|c| self.b.live(c) && build::wanted(wanted.as_deref(), c));
        let tree = self.b.tree.clone();
        let all: Vec<usize> = (0..tree.entries.len()).collect();
        if !all.is_empty() {
            sink.entries(&tree, &all)?;
        }
        for &c in self.empty.iter().filter(|&&c| self.b.live(c) && build::wanted(wanted.as_deref(), c)) {
            sink.content(c, Vec::new())?;
        }
        // the units left out
        for u in &self.units {
            if let Some(why) = &u.left {
                for f in u.files.iter().filter(|f| f.c.is_some() && build::wanted(wanted.as_deref(), f.c.unwrap())) {
                    sink.note(&format!("{}: {why}, not extracted", f.name));
                    skipped += 1;
                }
            }
        }
        // (of the files given)
        let given = || {
            self.units
                .iter()
                .filter(|u| u.left.is_none())
                .flat_map(|u| &u.files)
                .filter(|f| select.is_none() || want(f))
        };
        let packed: u64 = given().map(|f| f.data.len() as u64).sum();
        let total: u64 = given().map(|f| f.size as u64).sum();
        let (done_packed, done) = (AtomicU64::new(0), AtomicU64::new(0));
        let failed = AtomicBool::new(false);
        let made = pool::par_map(self.units.len(), threads, |i| -> Result<(), String> {
            let u = &self.units[i];
            let any = match select {
                None => u.files.iter().any(|f| f.c.is_some()),
                Some(_) => u.files.iter().any(want),
            };
            if failed.load(Relaxed) || u.left.is_some() || !any {
                return Ok(());
            }
            sink.step(Step::Started(i));
            // (all of it where nothing is selected, as the files' CRC-32s are checked)
            let decodes = |k: usize| select.is_none() || want(&u.files[k]);
            let r = self
                .decode(u, &decodes, &mut |k, data| {
                    let f = &u.files[k];
                    let p = done_packed.fetch_add(f.data.len() as u64, Relaxed) + f.data.len() as u64;
                    let n = done.fetch_add(f.size as u64, Relaxed) + f.size as u64;
                    sink.step(Step::Reached(Reached { decompressed: (p, packed), evaluated: (n, total) }));
                    match f.c {
                        Some(c) if want(f) => sink.content(c, data),
                        _ => Ok(()),
                    }
                })
                .map_err(|e| format!("RAR: {e}"));
            sink.step(Step::Ended(i));
            if r.is_err() {
                failed.store(true, Relaxed);
            }
            r
        });
        made.into_iter().collect::<Result<(), String>>()?;
        Ok(Done { tree, skipped })
    }
}

/// The files of a RAR5 archive.
fn heads5(d: &[u8]) -> Result<Vec<Head>, String> {
    let a = rar5::parse(d).ok_or("RAR: a damaged archive (or its headers encrypted)")?;
    let mut out = Vec::new();
    for e in a.entries.iter().filter(|e| !e.service) {
        let name = &d[e.name.clone()];
        let shown = String::from_utf8_lossy(name).into_owned();
        let mut x = Extra {
            mtime: e.mtime.and_then(time5),
            crtime: e.ctime.and_then(time5),
            atime: e.atime.and_then(time5),
            ..Extra::default()
        };
        match e.host {
            0 => x.win_attrs = Some(e.attrs as u32 & WIN_ATTRS_ALLOWED).filter(|&a| a != 0),
            1 => x.mode = Some(e.attrs as u32 & 0o7777),
            _ => {}
        }
        let kind = if e.dir {
            Kind::Dir
        } else if e.encrypted {
            Kind::Left { why: "encrypted".into(), data: e.method != 0 }
        } else if e.split {
            Kind::Left { why: "split across volumes".into(), data: e.method != 0 }
        } else if let Some((t, dir, target)) = e.redir.clone() {
            let target = d[target].to_vec();
            match t {
                1 => Kind::Link { target, win: None },
                2 => Kind::Link { target, win: Some(dir as u8) },
                3 => Kind::Link { target, win: Some(2) },
                4 => Kind::Left { why: "a hard link".into(), data: false },
                5 => Kind::Left { why: "a copy of another file".into(), data: false },
                _ => Kind::Left { why: "a redirection of no known type".into(), data: false },
            }
        } else {
            match (e.size, e.method, e.version) {
                (None, ..) => Kind::Left { why: "of no size given".into(), data: e.method != 0 },
                (Some(n), 0, _) if n == e.data.len() as u64 => {
                    Kind::Data { how: How::Stored, solid: false, size: n as usize, crc: e.crc }
                }
                (Some(_), 0, _) => Kind::Left { why: "stored data not of its size".into(), data: false },
                (Some(n), _, v @ 0..=1) => match usize::try_from(n) {
                    Ok(n) => Kind::Data { how: How::V5(v == 1, e.dict), solid: e.solid, size: n, crc: e.crc },
                    Err(_) => Kind::Left { why: "too large".into(), data: true },
                },
                _ => Kind::Left { why: "of an algorithm not known".into(), data: true },
            }
        };
        out.push(Head { names: Some(name.to_vec()), backslash: false, shown, data: e.data.clone(), kind, extra: x });
    }
    Ok(out)
}

/// The files of a RAR 1.5–4.x archive.
fn heads4(d: &[u8]) -> Result<Vec<Head>, String> {
    let a = rar4::parse(d).ok_or("RAR: a damaged archive (or its headers encrypted)")?;
    let mut out = Vec::new();
    for e in a.entries.iter().filter(|e| !e.service) {
        let field = &d[e.name.clone()];
        let name = match e.unicode {
            true => rar4::name(field, true),
            false => sys::legacy_name(field),
        };
        let shown = String::from_utf8_lossy(&name).into_owned();
        let mut x = Extra {
            mtime: e.mtime.and_then(time4),
            crtime: e.ctime.and_then(time4),
            atime: e.atime.and_then(time4),
            ..Extra::default()
        };
        let unix = matches!(e.host, 3 | 5);
        match e.host {
            0..=2 => x.win_attrs = Some(e.attrs & WIN_ATTRS_ALLOWED).filter(|&a| a != 0),
            _ if unix => x.mode = Some(e.attrs & 0o7777),
            _ => {}
        }
        let stored = e.method == 0x30;
        let kind = if e.dir {
            Kind::Dir
        } else if e.encrypted {
            Kind::Left { why: "encrypted".into(), data: !stored }
        } else if e.split {
            Kind::Left { why: "split across volumes".into(), data: !stored }
        } else if unix && e.attrs & S_IFMT == S_IFLNK {
            // (a symbolic link: its target is its data, stored)
            match stored && e.size == e.data.len() as u64 {
                true => Kind::Link { target: d[e.data.clone()].to_vec(), win: None },
                false => Kind::Left { why: "a link not stored".into(), data: !stored },
            }
        } else {
            match usize::try_from(e.size) {
                Err(_) => Kind::Left { why: "too large".into(), data: !stored },
                Ok(n) if stored && n == e.data.len() => {
                    Kind::Data { how: How::Stored, solid: false, size: n, crc: Some(e.crc) }
                }
                Ok(_) if stored => Kind::Left { why: "stored data not of its size".into(), data: false },
                Ok(n) if e.version == 29 && (0x31..=0x35).contains(&e.method) => {
                    Kind::Data { how: How::V3(e.dict), solid: e.solid, size: n, crc: Some(e.crc) }
                }
                Ok(_) => Kind::Left {
                    why: format!("of RAR {}.{}'s algorithm, not known here", e.version / 10, e.version % 10),
                    data: true,
                },
            }
        };
        out.push(Head { names: Some(name), backslash: true, shown, data: e.data.clone(), kind, extra: x });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Kept, file_of};

    /// Of a solid group (aqr-rar's `r5_solid` and `r4_solid`), a file selected decodes only as
    /// far as itself: the last file's data damaged fails the group extracted whole, not the file.
    #[test]
    fn solid_stops() {
        for name in ["r5_solid", "r4_solid"] {
            let path = format!("{}/../../codecs/aqr-rar/tests/data/{name}.rar", env!("CARGO_MANIFEST_DIR"));
            let d = std::fs::read(path).unwrap();
            let l = Listing::read(&d).unwrap();
            let u = l.units.iter().max_by_key(|u| u.files.len()).unwrap();
            assert!(u.files.len() >= 2, "{name}");
            let good = Kept::default();
            l.unpack(4, &good, None).unwrap();
            let good = good.0.into_inner().unwrap();
            let mut bad = d.clone();
            let last = &u.files.last().unwrap().data;
            bad[last.start + last.len() / 2] ^= 0x55;
            let lb = Listing::read(&bad).unwrap();
            assert!(lb.unpack(4, &Kept::default(), None).is_err(), "{name}");
            let k = u.files.len() - 2;
            let c = u.files[k].c.unwrap();
            let kept = Kept::default();
            lb.unpack(4, &kept, Some(&file_of(&l.b.tree, c))).unwrap();
            let kept = kept.0.into_inner().unwrap();
            assert_eq!(kept.keys().collect::<Vec<_>>(), [&c], "{name}");
            assert_eq!(kept[&c], good[&c], "{name}");
        }
    }
}
