//! tar archives (v7, ustar, GNU, pax), alone or under a compressor ([`crate::stream`]), read as
//! they are decoded: the stream decoded on a worker of its own and handed over in chunks, the
//! members parsed as they come, their entries told and their contents written a batch at a time
//! (the files of a batch on the pool's threads), so that what is held is a few batches whatever
//! the size of the archive.
//!
//! Kept of the members: the path (prefix, GNU long names, pax `path`), the kind (file, hard
//! link, symbolic link, directory, FIFO, device), the mode, owner (ids and names), times
//! (modification; access and status change of GNU and pax; creation of libarchive's pax) and
//! extended attributes (pax `SCHILY.xattr.` and `LIBARCHIVE.xattr.`). A path given again takes
//! the place of the one before, as tar does. Sparse files and multi-volume parts are not
//! extracted (told).

use std::sync::Mutex;

use aqr_common::pool;
use aqr_formula::aqr::{EntryNode, Extra, Step, Time};

use crate::build::{self, Builder};
use crate::stream;
use crate::{Done, Filter, Select, Sink};

const BLOCK: usize = 512;
/// A batch is told once it holds this many bytes of contents, or this many files.
const BATCH_BYTES: usize = 32 << 20;
const BATCH_FILES: usize = 4096;

/// Extracts tar archive `d` (under `f`) to `sink`; where `select` is given, the members it does
/// not select are left out of the tree and their bodies not kept (the archive read through all
/// the same: what is selected may come anywhere).
pub fn unpack(
    f: Option<Filter>,
    d: &[u8],
    threads: usize,
    sink: &dyn Sink,
    select: Option<&Select>,
) -> Result<Done, String> {
    let total = stream::estimate(f, d);
    let mut p = Parser::new(sink, threads, total, d.len() as u64);
    p.select = select;
    sink.step(Step::Started(0));
    let fed = pool::scope(1, |s| -> Result<(), String> {
        let mut failed = None;
        let mut feed = |v: &[u8]| match p.feed(v) {
            Ok(()) => true,
            Err(e) => {
                failed = Some(e);
                false
            }
        };
        // (decoded on this thread where no worker is free)
        if s.workers() == 0 {
            let r = stream::decode(f, d, threads, &mut |v, at| feed(&v[at..]));
            return failed.map_or(r.map(|_| ()), Err);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, usize)>(2);
        let h = s.spawn(move || stream::decode(f, d, threads, &mut |v, at| tx.send((v, at)).is_ok()));
        for (v, at) in rx.iter() {
            if !feed(&v[at..]) {
                break;
            }
        }
        drop(rx);
        let r = h.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
        failed.map_or(r.map(|_| ()), Err)
    });
    fed?;
    p.end()?;
    sink.step(Step::Ended(0));
    Ok(Done { tree: p.b.tree, skipped: p.skipped })
}

/// What the parser waits for.
enum State {
    /// A header (its bytes so far in `buf`).
    Header,
    /// The body of the member of header `h`, `left` bytes of it to come, then padding.
    Body { h: Box<Header>, left: u64, keep: bool },
    /// Bytes to skip: padding, or a body not kept.
    Skip(u64),
    /// Old GNU sparse headers following a member's header, then its body of `size` bytes.
    Sparse { size: u64 },
    /// The end of the archive found: the rest (zeros) not read.
    End,
}

struct Parser<'s> {
    sink: &'s dyn Sink,
    /// The members extracted, where not all.
    select: Option<&'s Select>,
    threads: usize,
    b: Builder,
    state: State,
    buf: Vec<u8>,
    body: Vec<u8>,
    /// What pax headers and GNU long names tell of the next member, and pax's global headers.
    pax: Records,
    global: Records,
    long_name: Option<Vec<u8>>,
    long_link: Option<Vec<u8>>,
    /// The contents of the batch not yet told, and their bytes.
    batch: Vec<(usize, Vec<u8>)>,
    batch_bytes: usize,
    /// The link groups numbered so far.
    groups: i128,
    skipped: usize,
    /// Whether a zero block was the last header (two end the archive).
    zero: bool,
    /// Bytes of contents written, of the archive's (as far as known), and the archive's bytes.
    made: u64,
    total: u64,
    packed: u64,
    told_types: Vec<u8>,
}

/// A member's header, with what pax headers and GNU long names before it told.
struct Header {
    path: Vec<u8>,
    link: Vec<u8>,
    kind: u8,
    size: u64,
    extra: Extra,
    major: u64,
    minor: u64,
    sparse: bool,
}

/// A number of a header's field: octal (spaces before, a space or a zero byte after), or
/// base-256 where its first byte's high bit is set (negative: none).
fn number(f: &[u8]) -> Option<u64> {
    if f.first().is_some_and(|&b| b & 0x80 != 0) {
        if f[0] == 0xff {
            return None;
        }
        return f[1..].iter().try_fold((f[0] & 0x7f) as u64, |v, &b| v.checked_mul(256).map(|v| v | b as u64));
    }
    let f = &f[f.iter().position(|&b| b != b' ').unwrap_or(f.len())..];
    let n = f.iter().position(|b| !(b'0'..=b'7').contains(b)).unwrap_or(f.len());
    if !f[n..].iter().all(|&b| b == 0 || b == b' ') {
        return None;
    }
    f[..n].iter().try_fold(0u64, |v, &b| v.checked_mul(8).map(|v| v | (b - b'0') as u64))
}

/// A string field: up to its first zero byte.
fn string(f: &[u8]) -> &[u8] {
    &f[..f.iter().position(|&b| b == 0).unwrap_or(f.len())]
}

/// A pax time: seconds since 1970, with a fraction where given.
fn pax_time(v: &[u8]) -> Option<Time> {
    let s = std::str::from_utf8(v).ok()?;
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |s| (true, s));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let secs: i64 = int.parse().ok()?;
    let mut nanos = 0u32;
    for (k, c) in frac.bytes().take(9).enumerate() {
        nanos += (c.checked_sub(b'0').filter(|&d| d < 10)? as u32) * 10u32.pow(8 - k as u32);
    }
    Some(match (neg, nanos) {
        (false, _) => Time::new(secs, nanos),
        (true, 0) => Time::new(-secs, 0),
        (true, n) => Time::new(-secs - 1, 1_000_000_000 - n),
    })
}

/// The records of a pax header, keys and values.
type Records = Vec<(Vec<u8>, Vec<u8>)>;

/// The records of a pax header: `LEN KEY=VALUE\n`.
fn pax_records(d: &[u8]) -> Result<Records, String> {
    let bad = || "tar: a damaged pax header".to_string();
    let mut out = Vec::new();
    let mut at = 0;
    while at < d.len() {
        if d[at..].iter().all(|&b| b == 0) {
            break;
        }
        let sp = d[at..].iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let len: usize = std::str::from_utf8(&d[at..at + sp]).ok().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
        let rec = d.get(at + sp + 1..at + len).filter(|r| r.last() == Some(&b'\n')).ok_or_else(bad)?;
        let rec = &rec[..rec.len() - 1];
        let eq = rec.iter().position(|&b| b == b'=').ok_or_else(bad)?;
        out.push((rec[..eq].to_vec(), rec[eq + 1..].to_vec()));
        at += len;
    }
    Ok(out)
}

/// `%XX` escapes undone (libarchive's names of extended attributes).
fn unescape(s: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && let (Some(h), Some(l)) = (s.get(i + 1).copied().and_then(hex), s.get(i + 2).copied().and_then(hex))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// Base64 (of libarchive's values of extended attributes), padding or not.
fn base64(s: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let s: Vec<u8> = s.iter().copied().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut v = 0u32;
        for (k, &c) in chunk.iter().enumerate() {
            v |= (val(c)? as u32) << (18 - 6 * k);
        }
        out.extend_from_slice(&v.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

impl<'s> Parser<'s> {
    fn new(sink: &'s dyn Sink, threads: usize, total: u64, packed: u64) -> Self {
        Parser {
            sink,
            select: None,
            threads,
            b: Builder::default(),
            state: State::Header,
            buf: Vec::with_capacity(BLOCK),
            body: Vec::new(),
            pax: Vec::new(),
            global: Vec::new(),
            long_name: None,
            long_link: None,
            batch: Vec::new(),
            batch_bytes: 0,
            groups: 0,
            skipped: 0,
            zero: false,
            made: 0,
            total,
            packed,
            told_types: Vec::new(),
        }
    }

    fn note(&mut self, what: String) {
        self.sink.note(&what);
        self.skipped += 1;
    }

    /// Takes `d`, the next bytes of the archive.
    fn feed(&mut self, mut d: &[u8]) -> Result<(), String> {
        while !d.is_empty() {
            match &mut self.state {
                State::End => return Ok(()),
                State::Header => {
                    let n = (BLOCK - self.buf.len()).min(d.len());
                    self.buf.extend_from_slice(&d[..n]);
                    d = &d[n..];
                    if self.buf.len() == BLOCK {
                        let block = std::mem::take(&mut self.buf);
                        self.header(&block)?;
                        self.buf = block;
                        self.buf.clear();
                    }
                }
                State::Skip(left) => {
                    let n = (*left).min(d.len() as u64) as usize;
                    *left -= n as u64;
                    d = &d[n..];
                    if *left == 0 {
                        self.state = State::Header;
                    }
                }
                State::Sparse { size } => {
                    let size = *size;
                    let n = (BLOCK - self.buf.len()).min(d.len());
                    self.buf.extend_from_slice(&d[..n]);
                    d = &d[n..];
                    if self.buf.len() == BLOCK {
                        // (another follows where its last byte but 7 is set)
                        self.state = if self.buf[504] != 0 { State::Sparse { size } } else { skip(size) };
                        self.buf.clear();
                    }
                }
                State::Body { left, keep, .. } => {
                    let n = (*left).min(d.len() as u64) as usize;
                    if *keep {
                        self.body.extend_from_slice(&d[..n]);
                    }
                    *left -= n as u64;
                    d = &d[n..];
                    if *left == 0 {
                        let State::Body { h, .. } = std::mem::replace(&mut self.state, State::Header) else {
                            unreachable!()
                        };
                        let pad = (BLOCK as u64 - h.size % BLOCK as u64) % BLOCK as u64;
                        let body = std::mem::take(&mut self.body);
                        self.member(*h, body)?;
                        if pad > 0 {
                            self.state = State::Skip(pad);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads the header `b`.
    fn header(&mut self, b: &[u8]) -> Result<(), String> {
        if b.iter().all(|&x| x == 0) {
            if self.zero {
                self.state = State::End;
            }
            self.zero = true;
            return Ok(());
        }
        self.zero = false;
        let sum = number(&b[148..156]).ok_or("tar: a damaged header")?;
        let unsigned: u64 =
            b.iter().enumerate().map(|(i, &x)| if (148..156).contains(&i) { 32 } else { x as u64 }).sum();
        let signed: i64 =
            b.iter().enumerate().map(|(i, &x)| if (148..156).contains(&i) { 32 } else { x as i8 as i64 }).sum();
        if sum != unsigned && sum as i64 != signed {
            return Err("tar: a damaged header (its checksum)".into());
        }
        let kind = b[156];
        let posix = &b[257..263] == b"ustar\0";
        let gnu = &b[257..265] == b"ustar  \0";
        let mut path = string(&b[..100]).to_vec();
        if posix && b[345] != 0 {
            path = [string(&b[345..500]), b"/", &path].concat();
        }
        let field = |r: std::ops::Range<usize>| number(&b[r]);
        let mut x = Extra { mtime: field(136..148).map(|t| Time::new(t as i64, 0)), ..Extra::default() };
        x.mode = field(100..108).map(|m| m as u32 & 0o7777);
        x.uid = field(108..116);
        x.gid = field(116..124);
        if posix || gnu {
            let name = |r: std::ops::Range<usize>| Some(string(&b[r])).filter(|n| !n.is_empty()).map(Into::into);
            x.uname = name(265..297);
            x.gname = name(297..329);
        }
        if gnu {
            let t = |r: std::ops::Range<usize>| field(r).filter(|&t| t != 0).map(|t| Time::new(t as i64, 0));
            x.atime = t(345..357);
            x.ctime = t(357..369);
        }
        let mut h = Header {
            path,
            link: string(&b[157..257]).to_vec(),
            kind,
            size: field(124..136).ok_or("tar: a damaged header (its size)")?,
            extra: x,
            major: field(329..337).unwrap_or(0),
            minor: field(337..345).unwrap_or(0),
            sparse: kind == b'S',
        };
        // what is told of the member (but of headers of the next one's)
        if !matches!(kind, b'x' | b'g' | b'L' | b'K') {
            let pax: Vec<_> = self.global.iter().chain(&std::mem::take(&mut self.pax)).cloned().collect();
            for (k, v) in pax {
                pax_record(&mut h, &k, &v);
            }
            if let Some(n) = self.long_name.take() {
                h.path = n;
            }
            if let Some(l) = self.long_link.take() {
                h.link = l;
            }
        }
        let size = h.size;
        // the bodies kept: of the headers of the next member, and of files (types not known
        // taken for files, as POSIX has it)
        let meta = matches!(kind, b'x' | b'g' | b'L' | b'K');
        let file = !matches!(kind, b'1'..=b'6' | b'D' | b'S' | b'V' | b'M' | b'N') && !meta;
        let keep = meta || file && !h.sparse && self.wants(&h.path);
        if kind == b'S' && b[482] != 0 {
            // (old GNU sparse: headers of the map follow; the file left out)
            self.member(h, Vec::new())?;
            self.state = State::Sparse { size };
            return Ok(());
        }
        if size == 0 {
            return self.member(h, Vec::new());
        }
        if keep {
            self.body = Vec::with_capacity(size.min(BATCH_BYTES as u64) as usize);
        }
        self.state = State::Body { h: Box::new(h), left: size, keep };
        Ok(())
    }
}

/// Takes pax record `k` of `v` into the header of the member it is of.
fn pax_record(h: &mut Header, k: &[u8], v: &[u8]) {
    let x = &mut h.extra;
    let num = || std::str::from_utf8(v).ok().and_then(|s| s.parse::<u64>().ok());
    match k {
        b"path" => h.path = v.to_vec(),
        b"linkpath" => h.link = v.to_vec(),
        b"size" => h.size = num().unwrap_or(h.size),
        b"mtime" => x.mtime = pax_time(v).or(x.mtime),
        b"atime" => x.atime = pax_time(v),
        b"ctime" => x.ctime = pax_time(v),
        b"LIBARCHIVE.creationtime" => x.crtime = pax_time(v),
        b"uid" => x.uid = num().or(x.uid),
        b"gid" => x.gid = num().or(x.gid),
        b"uname" => x.uname = Some(v).filter(|v| !v.is_empty()).map(Into::into),
        b"gname" => x.gname = Some(v).filter(|v| !v.is_empty()).map(Into::into),
        b"SCHILY.devmajor" => h.major = num().unwrap_or(h.major),
        b"SCHILY.devminor" => h.minor = num().unwrap_or(h.minor),
        _ if k.starts_with(b"SCHILY.xattr.") => {
            set_xattr(x, k[b"SCHILY.xattr.".len()..].to_vec(), v.to_vec());
        }
        _ if k.starts_with(b"LIBARCHIVE.xattr.") => {
            if let Some(v) = base64(v) {
                set_xattr(x, unescape(&k[b"LIBARCHIVE.xattr.".len()..]), v);
            }
        }
        _ if k.starts_with(b"GNU.sparse.") => h.sparse = true,
        _ => {}
    }
}

impl Parser<'_> {
    /// Whether the member of `path` is selected.
    fn wants(&self, path: &[u8]) -> bool {
        self.select.is_none_or(|s| s.wants(&build::lax(path, false)))
    }

    /// Takes member `h` of body `body` (its body where it is kept).
    fn member(&mut self, h: Header, body: Vec<u8>) -> Result<(), String> {
        match h.kind {
            b'x' => return pax_records(&body).map(|r| self.pax = r),
            b'g' => return pax_records(&body).map(|r| self.global.extend(r)),
            b'L' => {
                self.long_name = Some(string(&body).to_vec());
                return Ok(());
            }
            b'K' => {
                self.long_link = Some(string(&body).to_vec());
                return Ok(());
            }
            _ => {}
        }
        // (not selected: left out, and nothing told of it)
        if !self.wants(&h.path) {
            return Ok(());
        }
        let Some(names) = build::split(&h.path, false) else {
            self.note(build::outside(&show(&h.path)));
            return Ok(());
        };
        let shown = build::shown(&names);
        if h.sparse {
            self.note(format!("{shown}: a sparse file, not extracted"));
            return Ok(());
        }
        let node = match h.kind {
            b'5' | b'D' => EntryNode::Dir,
            b'2' => {
                if h.link.is_empty() {
                    self.note(build::dangling(&shown));
                    return Ok(());
                }
                EntryNode::Link(aqr_formula::aqr::name_from(&h.link))
            }
            b'1' => return self.hard_link(&names, &h),
            b'3' | b'4' => EntryNode::Device { block: h.kind == b'4', major: h.major, minor: h.minor },
            b'6' => EntryNode::Fifo,
            b'V' => return Ok(()),
            b'M' | b'N' => {
                self.note(format!("{shown}: a part of a multi-volume archive, not extracted"));
                return Ok(());
            }
            k => {
                if !matches!(k, b'0' | 0 | b'7') && !self.told_types.contains(&k) {
                    self.told_types.push(k);
                    self.sink.note(&format!("{shown}: a member of type {:?} taken for a file", k as char));
                }
                let c = self.b.content();
                match self.b.add(&names, Some(EntryNode::File(c)), h.extra) {
                    Ok(_) => {
                        self.batch_bytes += body.len();
                        self.batch.push((c, body));
                        if self.batch_bytes >= BATCH_BYTES || self.batch.len() >= BATCH_FILES {
                            self.flush()?;
                        }
                    }
                    Err(e) => self.note(e),
                }
                return Ok(());
            }
        };
        if names.is_empty() && node == EntryNode::Dir {
            // the directory extracted into (`./`)
            self.b.add(&[], None, h.extra)?;
            return Ok(());
        }
        if let Err(e) = self.b.add(&names, Some(node), h.extra) {
            self.note(e);
        }
        Ok(())
    }

    /// A hard link at `names` to the file `h` links to, given before: a file of its content,
    /// in a link group with it.
    fn hard_link(&mut self, names: &[&[u8]], h: &Header) -> Result<(), String> {
        let shown = build::shown(names);
        let Some(target) = build::split(&h.link, false) else {
            self.note(format!("{shown}: a hard link reaching out of the directory, not extracted"));
            return Ok(());
        };
        let Some((j, c)) = self.b.file(&target) else {
            let why = match self.select.is_some_and(|s| !s.wants(&target)) {
                true => "not selected",
                false => "not a file before it",
            };
            self.note(format!("{shown}: a hard link to {}, {why}, not extracted", show(&h.link)));
            return Ok(());
        };
        let g = match self.b.tree.entries[j].extra.link_group {
            Some(g) => g,
            None => {
                self.groups += 1;
                self.b.tree.entries[j].extra.link_group = Some(self.groups);
                self.b.touch(j);
                self.groups
            }
        };
        // (the link's own metadata is the file's: it is the same inode)
        let extra = Extra { link_group: Some(g), ..self.b.tree.entries[j].extra.clone() };
        if let Err(e) = self.b.add(names, Some(EntryNode::File(c)), extra) {
            self.note(e);
        }
        Ok(())
    }

    /// Tells the entries so far, and writes the contents of the batch.
    fn flush(&mut self) -> Result<(), String> {
        let new = self.b.take();
        if !new.is_empty() {
            self.sink.entries(&self.b.tree, &new)?;
        }
        let b = &self.b;
        let batch: Vec<_> =
            std::mem::take(&mut self.batch).into_iter().filter(|x| b.live(x.0)).map(|x| Mutex::new(Some(x))).collect();
        let sink = self.sink;
        let made = pool::par_map(batch.len(), self.threads, |k| {
            let (c, d) = batch[k].lock().unwrap().take().unwrap();
            let n = d.len() as u64;
            sink.content(c, d).map(|()| n)
        });
        for n in made {
            self.made += n?;
        }
        self.batch_bytes = 0;
        self.sink.step(stream::reached(self.made, self.total, self.packed));
        Ok(())
    }

    /// The end of the archive: what is left told.
    fn end(&mut self) -> Result<(), String> {
        match self.state {
            State::Header | State::End if self.buf.is_empty() => {}
            // (a last zero block alone, or none: as tar takes it)
            _ => return Err("tar: cut short".into()),
        }
        self.flush()
    }
}

/// Skipping a body of `size` bytes and its padding.
fn skip(size: u64) -> State {
    match size.div_ceil(BLOCK as u64) * BLOCK as u64 {
        0 => State::Header,
        n => State::Skip(n),
    }
}

fn set_xattr(x: &mut Extra, name: Vec<u8>, value: Vec<u8>) {
    if name.is_empty() {
        return;
    }
    match x.xattrs.iter_mut().find(|a| *a.0 == name[..]) {
        Some(a) => a.1 = value.into(),
        None => x.xattrs.push((name.into(), value.into())),
    }
}

fn show(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(number(b"0000644\0"), Some(0o644));
        assert_eq!(number(b"   644 "), Some(0o644));
        assert_eq!(number(b"\0\0\0\0"), Some(0));
        assert_eq!(number(&[0x80, 0, 0, 1, 0]), Some(256));
        assert_eq!(number(b"12x"), None);
    }

    #[test]
    fn pax() {
        assert_eq!(pax_time(b"1.5"), Some(Time::new(1, 500_000_000)));
        assert_eq!(pax_time(b"-1.25"), Some(Time::new(-2, 750_000_000)));
        assert_eq!(pax_time(b"12"), Some(Time::new(12, 0)));
        let r = pax_records(b"12 path=a/b\n16 uname=\xea\xb0\x80\xea\xb0\x80\n").unwrap();
        assert_eq!(r[0], (b"path".to_vec(), b"a/b".to_vec()));
        assert_eq!(r[1].1, "가가".as_bytes());
        assert_eq!(base64(b"aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64(b"aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(unescape(b"user.a%3Db"), b"user.a=b");
    }
}
