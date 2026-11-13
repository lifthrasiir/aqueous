//! ALZip's archives, ALZ and EGG, read by their records (aqr-alz's and aqr-egg's readers): the
//! files' names (ALZ's, which tell no encoding, decided for the archive as ZIP's of none are, CP949
//! the likeliest; EGG's UTF-8, or of the code page its filename record gives: CP949, Shift_JIS,
//! else of no declared encoding), directories, modification times
//! (ALZ's MS-DOS time as local time; EGG's FILETIME, else its POSIX time) and attributes
//! (Windows' read-only, hidden, system and archive bits; EGG's POSIX mode and owner); the data
//! decoded on the pool's threads, a file at a time (an EGG solid archive's block as one), its
//! CRC-32s checked (ALZ's of the file, EGG's of each block).
//!
//! Encrypted files, symbolic links (whose targets neither format is known to store), methods not
//! known, and what is past the archive's records where they do not end as
//! they should (a volume of a split archive, an archive cut short) are left out and told.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use aqr_alz::alz;
use aqr_common::{crc32, pool};
use aqr_egg::egg;
use aqr_formula::aqr::{EntryNode, Extra, Reached, Step, Time, Tree, WIN_ATTRS_ALLOWED};

use crate::build::{self, Builder, Named, Undeclared};
use crate::{Done, Select, Sink, codec};
use aqr_foreign::attrs::{DOS_DIR, DOS_KEPT, S_IFDIR, S_IFMT, S_IFREG};
use aqr_foreign::charset::{Charset, Choice};

/// How a piece of data is coded.
#[derive(Clone, Copy, Debug)]
enum Method {
    Stored,
    Deflate,
    /// bzip2 of ALZ's framing.
    AlzBzip2,
    Bzip2,
    /// EGG's LZMA: 4 bytes, LZMA's properties (5), the stream.
    Lzma,
    Azo,
}

/// A stream of data: an ALZ file's body, an EGG block; its CRC-32 where it has one of its own.
struct Piece {
    method: Method,
    body: Range<usize>,
    size: usize,
    crc: Option<u32>,
}

/// A file of a unit: its content (`None` where left out of the tree), name (shown), size and
/// CRC-32 (where it has one of its own).
struct File {
    c: Option<usize>,
    name: String,
    size: usize,
    crc: Option<u32>,
}

/// The data of `files`, one after another, decoded from `pieces`; `left` where it cannot be
/// decoded here (why).
struct Unit {
    pieces: Vec<Piece>,
    files: Vec<File>,
    name: String,
    left: Option<String>,
}

/// What a file of the archive is.
enum Kind {
    Dir,
    /// A file of data: its pieces (none where they come later, in an EGG solid archive), size and
    /// CRC-32.
    Data {
        pieces: Vec<Piece>,
        size: usize,
        crc: Option<u32>,
    },
    /// Not extracted: why (of data, `size` bytes of a solid block, where `size` is not 0).
    Left {
        why: String,
        size: usize,
    },
}

/// A file as read from the records.
struct Head {
    name: Named,
    kind: Kind,
    extra: Extra,
}

pub struct Listing<'d> {
    d: &'d [u8],
    b: Builder,
    units: Vec<Unit>,
    /// The contents of no data (empty files).
    empty: Vec<usize>,
    notes: Vec<build::Note>,
    /// How the names of no declared encoding are taken.
    pub undeclared: Undeclared,
}

/// Name `b` of code page `cp` (949, 932; else of no declared encoding).
fn named(b: &[u8], cp: u16) -> Named {
    match cp {
        949 => Named::of(Charset::Std(encoding_rs::EUC_KR), b.to_vec()),
        932 => Named::of(Charset::Std(encoding_rs::SHIFT_JIS), b.to_vec()),
        _ => Named::Bytes(b.to_vec()),
    }
}

/// Windows' attributes of `a` (bits that are Windows' own).
fn win_attrs(a: u8) -> Option<u32> {
    Some(a as u32 & DOS_KEPT & WIN_ATTRS_ALLOWED).filter(|&a| a != 0)
}

fn size_of(n: u64) -> Option<usize> {
    usize::try_from(n).ok()
}

impl<'d> Listing<'d> {
    /// ALZ archive `d`, its names (of no declared encoding) taken as `choice` asks.
    pub fn read_alz(d: &'d [u8], choice: &Choice) -> Result<Listing<'d>, String> {
        let a = alz::parse(d).ok_or("ALZ: not an ALZ archive")?;
        let mut heads = Vec::new();
        for e in &a.entries {
            let name = Named::Bytes(d[e.name.clone()].to_vec());
            let extra = Extra {
                mtime: aqr_foreign::time::from_dos((e.time >> 16) as u16, e.time as u16),
                win_attrs: win_attrs(e.attrs),
                ..Extra::default()
            };
            let kind = if e.attrs as u32 & DOS_DIR != 0 {
                Kind::Dir
            } else if e.attrs & 0x40 != 0 {
                Kind::Left { why: "a symbolic link".into(), size: 0 }
            } else if e.encrypted {
                Kind::Left { why: "encrypted".into(), size: 0 }
            } else {
                let method = match e.method {
                    None => None,
                    Some(alz::STORED) => Some(Method::Stored),
                    Some(alz::BZIP2) => Some(Method::AlzBzip2),
                    Some(alz::DEFLATE) => Some(Method::Deflate),
                    Some(m) => {
                        heads.push(Head {
                            name,
                            kind: Kind::Left { why: format!("of method {m}, not known"), size: 0 },
                            extra,
                        });
                        continue;
                    }
                };
                match (method, size_of(e.size)) {
                    (_, None) => Kind::Left { why: "too large".into(), size: 0 },
                    (None, _) | (_, Some(0)) => Kind::Data { pieces: Vec::new(), size: 0, crc: None },
                    (Some(method), Some(size)) => Kind::Data {
                        pieces: vec![Piece { method, body: e.body.clone(), size, crc: None }],
                        size,
                        crc: Some(e.crc),
                    },
                }
            };
            heads.push(Head { name, kind, extra });
        }
        let mut l = Listing::new(d);
        // (ALZip's are CP949; all UTF-8, they are of an archiver writing UTF-8, as names of CP949
        // rarely are, and never all of them)
        l.files(heads, false, choice, Some(Charset::Std(encoding_rs::EUC_KR)));
        if !a.ended {
            l.notes.push((
                Vec::new(),
                "ALZ: the archive goes on in another volume or is cut short: what is past it is not extracted".into(),
            ));
        }
        Ok(l)
    }

    /// EGG archive `d`, its names of no declared encoding taken as `choice` asks.
    pub fn read_egg(d: &'d [u8], choice: &Choice) -> Result<Listing<'d>, String> {
        let a = egg::parse(d).ok_or("EGG: not an EGG archive")?;
        let mut heads = Vec::new();
        for e in &a.entries {
            let raw = &d[e.name.clone()];
            let name = match e.locale {
                None => Named::utf8(raw.to_vec()),
                Some(cp) => named(raw, cp),
            };
            let mut extra = Extra::default();
            let (mut dir, mut link) = (false, false);
            if let Some((t, attrs)) = e.windows {
                extra.mtime = aqr_foreign::time::from_filetime(t);
                extra.win_attrs = win_attrs(attrs & 0x07);
                (dir, link) = (attrs & 0x80 != 0, attrs & 0x08 != 0);
            }
            if let Some(p) = e.posix {
                extra.mode = Some(p.mode & 0o7777);
                (extra.uid, extra.gid) = (Some(p.uid as u64), Some(p.gid as u64));
                extra.mtime = extra.mtime.or(Some(Time::new(p.mtime as i64, 0)));
                match p.mode & S_IFMT {
                    S_IFDIR => dir = true,
                    0 | S_IFREG => {}
                    _ => link = true,
                }
            }
            let size = size_of(e.size);
            let held = || size.unwrap_or(0);
            let kind = if dir && e.blocks.is_empty() {
                Kind::Dir
            } else if link {
                Kind::Left { why: "a symbolic link".into(), size: held() }
            } else if e.encrypted {
                Kind::Left { why: "encrypted".into(), size: held() }
            } else if let Some(m) = e.blocks.iter().map(|b| b.method).find(|&m| m > egg::LZMA) {
                Kind::Left { why: format!("of method {m}, not known"), size: held() }
            } else {
                match size {
                    None => Kind::Left { why: "too large".into(), size: 0 },
                    Some(size) => Kind::Data {
                        pieces: e
                            .blocks
                            .iter()
                            .map(|b| Piece {
                                method: match b.method {
                                    egg::STORED => Method::Stored,
                                    egg::DEFLATE => Method::Deflate,
                                    egg::BZIP2 => Method::Bzip2,
                                    egg::AZO => Method::Azo,
                                    _ => Method::Lzma,
                                },
                                body: b.body.clone(),
                                size: b.size as usize,
                                crc: Some(b.crc),
                            })
                            .collect(),
                        size,
                        crc: None,
                    },
                }
            };
            heads.push(Head { name, kind, extra });
        }
        let mut l = Listing::new(d);
        l.files(heads, a.solid, choice, None);
        if !a.ended {
            l.notes.push((
                Vec::new(),
                match a.split {
                    true => "EGG: a volume of a split archive: what goes on in other volumes is not extracted".into(),
                    false => "EGG: the archive is cut short: what is past it is not extracted".into(),
                },
            ));
        }
        Ok(l)
    }

    fn new(d: &'d [u8]) -> Listing<'d> {
        Listing {
            d,
            b: Builder::default(),
            units: Vec::new(),
            empty: Vec::new(),
            notes: Vec::new(),
            undeclared: Undeclared::default(),
        }
    }

    /// The tree and units of `heads`. In a `solid` archive, files of data of no pieces have their
    /// data in the pieces of the file of data after them, one after another. Their names of no
    /// declared encoding are taken as `choice` asks, of `convention` where they can be
    /// ([`Undeclared::decide`]).
    fn files(&mut self, heads: Vec<Head>, solid: bool, choice: &Choice, convention: Option<Charset>) {
        self.undeclared = Undeclared::decide(choice, heads.iter().filter_map(|h| h.name.undeclared()), convention);
        let mut pending: Vec<File> = Vec::new();
        // (why the pending files cannot be decoded, where one of them cannot)
        let mut broken: Option<String> = None;
        for h in heads {
            let name = self.undeclared.path(&h.name, true);
            let shown = String::from_utf8_lossy(&name).into_owned();
            let lax = || build::lax(&name, false);
            let names = build::split(&name, false);
            let dir_named = name.last() == Some(&b'/');
            let r = match h.kind {
                Kind::Left { why, size } => {
                    self.notes.push((lax(), format!("{shown}: {why}, not extracted")));
                    if solid && size > 0 {
                        pending.push(File { c: None, name: shown.clone(), size, crc: None });
                        broken.get_or_insert_with(|| format!("of a solid block with {shown}, which is not"));
                    }
                    continue;
                }
                Kind::Dir => match names {
                    None => Err(build::outside(&shown)),
                    Some(n) if n.is_empty() => self.b.add(&[], None, h.extra),
                    Some(n) => self.b.add(&n, Some(EntryNode::Dir), h.extra),
                },
                Kind::Data { pieces, size, .. } if size == 0 && pieces.is_empty() && dir_named => match names {
                    None => Err(build::outside(&shown)),
                    Some(n) => self.b.add(&n, (!n.is_empty()).then_some(EntryNode::Dir), h.extra),
                },
                Kind::Data { pieces, size, crc } => {
                    let names = names.filter(|n| !n.is_empty());
                    let c = names.as_ref().map(|_| self.b.content());
                    if c.is_none() {
                        self.notes.push((lax(), build::outside(&shown)));
                    }
                    let f = File { c, name: shown.clone(), size, crc };
                    if size == 0 && pieces.is_empty() {
                        self.empty.extend(c);
                    } else if pieces.is_empty() && solid {
                        pending.push(f);
                    } else {
                        let mut files = std::mem::take(&mut pending);
                        files.push(f);
                        let left = match pieces.is_empty() {
                            true => Some("its data not in the archive".to_string()),
                            false => broken.take(),
                        };
                        self.units.push(Unit { pieces, files, name: String::new(), left });
                    }
                    match names {
                        Some(n) => self.b.add(&n, Some(EntryNode::File(c.unwrap())), h.extra),
                        None => continue,
                    }
                }
            };
            if let Err(e) = r {
                self.notes.push((lax(), e));
            }
        }
        if !pending.is_empty() {
            let left = broken.take().or_else(|| Some("its data not in the archive".to_string()));
            self.units.push(Unit { pieces: Vec::new(), files: pending, name: String::new(), left });
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

    /// The data of piece `p`, of its size, its CRC-32 checked.
    fn piece(&self, p: &Piece) -> Result<Vec<u8>, String> {
        let body = &self.d[p.body.clone()];
        let size = p.size;
        let data = match p.method {
            Method::Stored if body.len() != size => return Err("stored data not of its size".into()),
            Method::Stored => body.to_vec(),
            Method::Deflate => codec::inflate(body, size, size)?.0,
            Method::AlzBzip2 => {
                aqr_bz2::stream::decode_framed(body, aqr_bz2::Framing::Alz, true, size, &mut |_, _| {})
                    .map_err(|e| format!("bzip2: {e}"))?
                    .data
            }
            Method::Bzip2 => codec::bzip2(body, size)?,
            Method::Lzma => {
                let props = *body.get(4).filter(|_| body.len() >= egg::LZMA_HEADER).ok_or("LZMA: no header")?;
                codec::lzma(&body[egg::LZMA_HEADER..], props, Some(size as u64), size)?.0
            }
            Method::Azo => aqr_azo::azo::decode(body, size).map_err(|e| format!("AZO: {e}"))?.data,
        };
        if data.len() != size {
            return Err("data not of its size".into());
        }
        if p.crc.is_some_and(|c| crc32::hash(&data) != c) {
            return Err("CRC mismatch".into());
        }
        Ok(data)
    }

    /// Decodes unit `u`, giving each file's content that `want` (by its index) to `give`. Of a
    /// solid one, only the pieces from that of the first file wanted to that of the last are.
    fn decode(
        &self,
        u: &Unit,
        want: &dyn Fn(usize) -> bool,
        give: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), String>,
    ) -> Result<(), String> {
        let at = |e: String| format!("{}: {e}", u.name);
        if (0..u.files.len()).any(|k| !want(k)) {
            return self.decode_some(u, want, give).map_err(at);
        }
        let mut data = match u.pieces.len() {
            1 => self.piece(&u.pieces[0]).map_err(at)?,
            _ => {
                let mut v = Vec::new();
                for p in &u.pieces {
                    v.extend_from_slice(&self.piece(p).map_err(at)?);
                }
                v
            }
        };
        if u.files.iter().map(|f| f.size).sum::<usize>() != data.len() {
            return Err(at("data not of the files' sizes".into()));
        }
        if let [f] = &u.files[..] {
            if f.crc.is_some_and(|c| crc32::hash(&data) != c) {
                return Err(format!("{}: CRC mismatch", f.name));
            }
            return give(0, std::mem::take(&mut data));
        }
        let mut start = 0;
        for (k, f) in u.files.iter().enumerate() {
            let d = &data[start..start + f.size];
            start += f.size;
            if f.crc.is_some_and(|c| crc32::hash(d) != c) {
                return Err(format!("{}: CRC mismatch", f.name));
            }
            if f.c.is_some() {
                give(k, d.to_vec())?;
            }
        }
        Ok(())
    }

    /// [`Listing::decode`] of some of the files of a unit: the pieces from that of the first file
    /// wanted to that of the last decoded, and those files given.
    fn decode_some(
        &self,
        u: &Unit,
        want: &dyn Fn(usize) -> bool,
        give: &mut dyn FnMut(usize, Vec<u8>) -> Result<(), String>,
    ) -> Result<(), String> {
        let starts: Vec<usize> = u.files.iter().scan(0, |at, f| Some(std::mem::replace(at, *at + f.size))).collect();
        let (Some(first), Some(last)) = ((0..u.files.len()).find(|&k| want(k)), (0..u.files.len()).rfind(|&k| want(k)))
        else {
            return Ok(());
        };
        let (lo, hi) = (starts[first], starts[last] + u.files[last].size);
        // the pieces of [lo, hi), and where the first starts
        let (mut base, mut at, mut data) = (None, 0, Vec::new());
        for p in &u.pieces {
            if at < hi && at + p.size > lo || p.size == 0 && (lo..hi).contains(&at) {
                base.get_or_insert(at);
                data.extend_from_slice(&self.piece(p)?);
            }
            at += p.size;
        }
        let base = base.unwrap_or(lo);
        if at != starts.last().map_or(0, |s| s + u.files.last().unwrap().size) || base + data.len() < hi {
            return Err("data not of the files' sizes".into());
        }
        for k in (first..=last).filter(|&k| want(k)) {
            let (f, s) = (&u.files[k], starts[k] - base);
            let d = &data[s..s + f.size];
            if f.crc.is_some_and(|c| crc32::hash(d) != c) {
                return Err(format!("{}: CRC mismatch", f.name));
            }
            give(k, d.to_vec())?;
        }
        Ok(())
    }

    pub fn tree(&self) -> &Tree {
        &self.b.tree
    }

    pub fn notes(&self) -> &[build::Note] {
        &self.notes
    }

    pub fn unpack(
        &self,
        format: &str,
        threads: usize,
        sink: &dyn Sink,
        select: Option<&Select>,
    ) -> Result<Done, String> {
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
        for u in &self.units {
            if let Some(why) = &u.left {
                for f in u.files.iter().filter(|f| f.c.is_some_and(|c| build::wanted(wanted.as_deref(), c))) {
                    sink.note(&format!("{}: {why}, not extracted", f.name));
                    skipped += 1;
                }
            }
        }
        // (of the units decoded)
        let decoded =
            || self.units.iter().filter(|u| u.left.is_none() && (select.is_none() || u.files.iter().any(want)));
        let packed: u64 = decoded().flat_map(|u| &u.pieces).map(|p| p.body.len() as u64).sum();
        let total: u64 = decoded().flat_map(|u| &u.files).map(|f| f.size as u64).sum();
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
                .decode(u, &decodes, &mut |k, data| match u.files[k].c {
                    Some(c) if want(&u.files[k]) => sink.content(c, data),
                    _ => Ok(()),
                })
                .map_err(|e| format!("{format}: {e}"));
            let p = u.pieces.iter().map(|p| p.body.len() as u64).sum::<u64>();
            let n = u.files.iter().map(|f| f.size as u64).sum::<u64>();
            let p = done_packed.fetch_add(p, Relaxed) + p;
            let n = done.fetch_add(n, Relaxed) + n;
            sink.step(Step::Reached(Reached { decompressed: (p, packed), evaluated: (n, total) }));
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
