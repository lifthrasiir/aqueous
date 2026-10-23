//! The layout: the records of an archive in order, with every fixed field stored as its XOR
//! with a prediction from the records before it and from the entry's contents (`FORMAT.md` §3).
//! [`State`] makes the predictions, for [`Writer`] (encoding) and [`build`] (decoding) alike.

use std::ops::Range;

use aqr_common::varint;

use crate::zip::{
    self, CENTRAL_FIXED, DESCRIPTOR, Desc, END_FIXED, END64_FIXED, LOCAL_FIXED, LOCATOR_FIXED, u16_at, u32_at,
};
use crate::{Error, Result, corrupt};

/// Record tags.
pub const GAP: u8 = 0;
pub const LOCAL: u8 = 1;
pub const CENTRAL: u8 = 2;
pub const END64: u8 = 3;
pub const LOCATOR: u8 = 4;
pub const END: u8 = 5;

/// How a local entry's body is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// The data as it is (stored).
    Stored = 0,
    /// The DEFLATE stream that aqr-flate rebuilds from the data and a record.
    Deflated = 1,
    /// Bytes of `kept`, with the CRC-32 and size of their data as the layout gives them.
    Kept = 2,
    /// The body of an earlier entry again.
    Same = 3,
}

/// What an entry's fields are predicted from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Entry {
    pub crc: u32,
    pub csize: u64,
    pub usize: u64,
}

/// A local entry seen, by where its parts are in the archive.
#[derive(Debug, Clone)]
struct Seen {
    pos: u64,
    fixed: [u8; LOCAL_FIXED],
    name: Range<usize>,
    extra: Range<usize>,
    body: Range<usize>,
    entry: Entry,
}

/// The records so far, as far as predictions go.
#[derive(Debug, Clone)]
pub struct State {
    base: i64,
    local: [u8; LOCAL_FIXED],
    central: [u8; CENTRAL_FIXED],
    seen: Vec<Seen>,
    /// The local entry the next central header is expected to refer to.
    next: usize,
    /// The central headers since the last end record: how many, their bytes, the first one's
    /// position; and the last zip64 end record.
    count: u64,
    bytes: u64,
    first: Option<u64>,
    end64: Option<u64>,
}

fn put16(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// `v` in a 32-bit field: all ones from there on.
fn clip32(v: u64) -> u64 {
    v.min(u32::MAX as u64)
}

fn xor_into(out: &mut [u8], a: &[u8], b: &[u8]) {
    for ((o, a), b) in out.iter_mut().zip(a).zip(b) {
        *o = a ^ b;
    }
}

/// XORs the zip64 data of `extra` with `vals` (value, bytes), in order, as far as it holds them.
fn xor_zip64(extra: &mut [u8], vals: &[(u64, usize)]) {
    let Some(r) = zip::zip64(extra) else { return };
    let mut at = r.start;
    for &(v, w) in vals {
        if at + w > r.end {
            break;
        }
        for (b, x) in extra[at..at + w].iter_mut().zip(v.to_le_bytes()) {
            *b ^= x;
        }
        at += w;
    }
}

impl State {
    pub fn new(base: i64) -> State {
        State {
            base,
            local: [0; LOCAL_FIXED],
            central: [0; CENTRAL_FIXED],
            seen: Vec::new(),
            next: 0,
            count: 0,
            bytes: 0,
            first: None,
            end64: None,
        }
    }

    /// An archive position as an offset from the base.
    fn rel(&self, pos: u64) -> u64 {
        (pos as i64).wrapping_sub(self.base) as u64
    }

    /// The local header predicted for an entry made by `body` with `flags` (the actual ones).
    fn local(&self, body: Body, e: &Entry, flags: u16, zip64: bool, name: usize, extra: usize) -> [u8; LOCAL_FIXED] {
        let mut p = self.local;
        match body {
            Body::Stored => put16(&mut p, 4, zip::STORED as u64),
            Body::Deflated => put16(&mut p, 4, zip::DEFLATED as u64),
            Body::Kept | Body::Same => {}
        }
        let sizes = |v: u64| {
            if flags & DESCRIPTOR != 0 {
                0
            } else if zip64 {
                u32::MAX as u64
            } else {
                v
            }
        };
        put32(&mut p, 10, if flags & DESCRIPTOR != 0 { 0 } else { e.crc as u64 });
        put32(&mut p, 14, sizes(e.csize));
        put32(&mut p, 18, sizes(e.usize));
        put16(&mut p, 22, name as u64);
        put16(&mut p, 24, extra as u64);
        p
    }

    fn local_zip64(e: &Entry, flags: u16) -> [(u64, usize); 2] {
        if flags & DESCRIPTOR != 0 { [(0, 8); 2] } else { [(e.usize, 8), (e.csize, 8)] }
    }

    fn desc(e: &Entry, desc: Desc) -> Vec<u8> {
        if desc == Desc::None {
            return Vec::new();
        }
        let mut p = e.crc.to_le_bytes().to_vec();
        for v in [e.csize, e.usize] {
            p.extend_from_slice(&v.to_le_bytes()[..if desc.long() { 8 } else { 4 }]);
        }
        p
    }

    fn central(&self, local: Option<usize>, name: usize, extra: usize, comment: usize) -> [u8; CENTRAL_FIXED] {
        let mut p = self.central;
        let (e, off) = match local.map(|i| &self.seen[i]) {
            Some(s) => {
                p[2..12].copy_from_slice(&s.fixed[..10]);
                (s.entry, clip32(self.rel(s.pos)))
            }
            None => (Entry::default(), 0),
        };
        put32(&mut p, 12, e.crc as u64);
        put32(&mut p, 16, clip32(e.csize));
        put32(&mut p, 20, clip32(e.usize));
        put16(&mut p, 24, name as u64);
        put16(&mut p, 26, extra as u64);
        put16(&mut p, 28, comment as u64);
        put32(&mut p, 38, off);
        p
    }

    /// The zip64 values of a central header with `fixed`, in the order they come.
    fn central_zip64(&self, local: Option<usize>, fixed: &[u8]) -> Vec<(u64, usize)> {
        let (e, off) = local.map_or((Entry::default(), 0), |i| (self.seen[i].entry, self.rel(self.seen[i].pos)));
        let mut vals = Vec::new();
        for (at, v) in [(20, e.usize), (16, e.csize), (38, off)] {
            if u32_at(fixed, at) == u32::MAX {
                vals.push((v, 8));
            }
        }
        if u16_at(fixed, 30) == u16::MAX {
            vals.push((0, 4));
        }
        vals
    }

    /// Where the central directory before an end record at `pos` starts.
    fn cd_start(&self, pos: u64) -> u64 {
        self.rel(self.first.or(self.end64).unwrap_or(pos))
    }

    fn end64(&self, pos: u64, ext: usize) -> [u8; END64_FIXED] {
        let mut p = [0; END64_FIXED];
        put64(&mut p, 0, (END64_FIXED - 8 + ext) as u64);
        put16(&mut p, 8, 45);
        put16(&mut p, 10, 45);
        put64(&mut p, 20, self.count);
        put64(&mut p, 28, self.count);
        put64(&mut p, 36, self.bytes);
        put64(&mut p, 44, self.cd_start(pos));
        p
    }

    fn locator(&self) -> [u8; LOCATOR_FIXED] {
        let mut p = [0; LOCATOR_FIXED];
        put64(&mut p, 4, self.end64.map_or(0, |e| self.rel(e)));
        put32(&mut p, 12, 1);
        p
    }

    fn end(&self, pos: u64, comment: usize) -> [u8; END_FIXED] {
        let mut p = [0; END_FIXED];
        let n = self.count.min(u16::MAX as u64);
        put16(&mut p, 4, n);
        put16(&mut p, 6, n);
        put32(&mut p, 8, clip32(self.bytes));
        put32(&mut p, 12, clip32(self.cd_start(pos)));
        put16(&mut p, 16, comment as u64);
        p
    }

    /// The local entry a central header refers to, by the code of its reference.
    fn referred(&self, code: u64) -> Result<Option<usize>> {
        if code == 0 {
            return Ok(None);
        }
        let i = (self.next as i64)
            .checked_add(varint::unzz(code - 1))
            .filter(|&i| i >= 0 && (i as usize) < self.seen.len());
        i.map(|i| Some(i as usize)).ok_or_else(corrupt)
    }

    fn after_central(&mut self, local: Option<usize>, pos: u64, fixed: &[u8], len: usize) {
        self.central.copy_from_slice(fixed);
        if let Some(i) = local {
            self.next = i + 1;
        }
        self.count += 1;
        self.bytes += len as u64;
        self.first.get_or_insert(pos);
    }

    fn after_end(&mut self) {
        (self.count, self.bytes, self.first, self.end64) = (0, 0, None, None);
    }
}

/// Writes the layout of an archive, record by record, keeping the [`State`] the decoder will
/// have.
pub struct Writer<'a> {
    d: &'a [u8],
    st: State,
    pub layout: Vec<u8>,
    pub kept: Vec<u8>,
    /// Local entries by position, for central headers to refer to.
    locals: std::collections::HashMap<u64, usize>,
}

/// How the encoder has a local entry's body made.
#[derive(Debug, Clone)]
pub enum Made {
    /// Stored: the body is the data.
    Stored,
    /// Deflated, with the bytes of data and record.
    Deflated { data: Vec<u8>, record: Vec<u8> },
    /// Kept, with the CRC-32 and size of the data given.
    Kept { crc: u32, usize: u64 },
    /// The body of local entry `i` (in order) again.
    Same(usize),
}

impl<'a> Writer<'a> {
    pub fn new(d: &'a [u8], base: i64) -> Writer<'a> {
        let mut layout = Vec::new();
        varint::put_signed(&mut layout, base);
        Writer { d, st: State::new(base), layout, kept: Vec::new(), locals: Default::default() }
    }

    fn bytes(&mut self, r: Range<usize>) {
        varint::put(&mut self.layout, r.len() as u64);
        self.layout.extend_from_slice(&self.d[r]);
    }

    pub fn gap(&mut self, r: Range<usize>) {
        self.layout.push(GAP);
        varint::put(&mut self.layout, r.len() as u64);
        self.kept.extend_from_slice(&self.d[r]);
    }

    pub fn local(&mut self, l: &zip::Local, made: &Made, crc: u32) {
        let d = self.d;
        let n = l.body.len() as u64;
        let (body, entry) = match made {
            Made::Stored => (Body::Stored, Entry { crc, csize: n, usize: n }),
            Made::Deflated { data, .. } => (Body::Deflated, Entry { crc, csize: n, usize: data.len() as u64 }),
            Made::Kept { crc, usize } => (Body::Kept, Entry { crc: *crc, csize: n, usize: *usize }),
            Made::Same(i) => (Body::Same, self.st.seen[*i].entry),
        };
        self.layout.extend_from_slice(&[LOCAL, body as u8]);
        match made {
            Made::Stored => varint::put(&mut self.layout, n),
            Made::Deflated { data, record } => {
                varint::put(&mut self.layout, data.len() as u64);
                varint::put(&mut self.layout, record.len() as u64);
            }
            Made::Kept { crc, usize } => {
                varint::put(&mut self.layout, n);
                self.layout.extend_from_slice(&crc.to_le_bytes());
                varint::put(&mut self.layout, *usize);
                self.kept.extend_from_slice(&d[l.body.clone()]);
            }
            Made::Same(i) => varint::put(&mut self.layout, (self.st.seen.len() - i) as u64),
        }
        self.layout.push(l.desc as u8);
        self.bytes(l.name.clone());
        let flags = l.flags(d);
        let mut extra = d[l.extra.clone()].to_vec();
        let zip64 = zip::zip64(&extra).is_some();
        xor_zip64(&mut extra, &State::local_zip64(&entry, flags));
        varint::put(&mut self.layout, extra.len() as u64);
        self.layout.extend_from_slice(&extra);
        let fixed: [u8; LOCAL_FIXED] = l.fixed(d).try_into().unwrap();
        let p = self.st.local(body, &entry, flags, zip64, l.name.len(), l.extra.len());
        let at = self.layout.len();
        self.layout.resize(at + LOCAL_FIXED, 0);
        xor_into(&mut self.layout[at..], &fixed, &p);
        let at = self.layout.len();
        self.layout.resize(at + l.desc.fields(), 0);
        xor_into(&mut self.layout[at..], l.desc_fields(d), &State::desc(&entry, l.desc));
        self.locals.entry(l.pos as u64).or_insert(self.st.seen.len());
        self.st.local = fixed;
        self.st.seen.push(Seen {
            pos: l.pos as u64,
            fixed,
            name: l.name.clone(),
            extra: l.extra.clone(),
            body: l.body.clone(),
            entry,
        });
    }

    pub fn central(&mut self, c: &zip::Central) {
        let d = self.d;
        let (off, _) = c.info(d);
        let local = self.locals.get(&((off as i64).wrapping_add(self.st.base) as u64)).copied();
        self.layout.push(CENTRAL);
        let code = local.map_or(0, |i| varint::zz(i as i64 - self.st.next as i64) + 1);
        varint::put(&mut self.layout, code);
        let s = local.map(|i| &self.st.seen[i]);
        let same_name = s.is_some_and(|s| d[s.name.clone()] == d[c.name.clone()]);
        let same_extra = s.is_some_and(|s| d[s.extra.clone()] == d[c.extra.clone()]);
        self.layout.push(same_name as u8 | (same_extra as u8) << 1);
        if !same_name {
            self.bytes(c.name.clone());
        }
        let fixed = c.fixed(d);
        if !same_extra {
            let mut extra = d[c.extra.clone()].to_vec();
            xor_zip64(&mut extra, &self.st.central_zip64(local, fixed));
            varint::put(&mut self.layout, extra.len() as u64);
            self.layout.extend_from_slice(&extra);
        }
        self.bytes(c.comment.clone());
        let p = self.st.central(local, c.name.len(), c.extra.len(), c.comment.len());
        let at = self.layout.len();
        self.layout.resize(at + CENTRAL_FIXED, 0);
        xor_into(&mut self.layout[at..], fixed, &p);
        self.st.after_central(local, c.pos as u64, fixed, c.comment.end - c.pos);
    }

    pub fn end64(&mut self, pos: usize, ext: Range<usize>) {
        self.layout.push(END64);
        let p = self.st.end64(pos as u64, ext.len());
        self.bytes(ext);
        let at = self.layout.len();
        self.layout.resize(at + END64_FIXED, 0);
        xor_into(&mut self.layout[at..], &self.d[pos + 4..pos + 4 + END64_FIXED], &p);
        self.st.end64 = Some(pos as u64);
    }

    pub fn locator(&mut self, pos: usize) {
        self.layout.push(LOCATOR);
        let p = self.st.locator();
        let at = self.layout.len();
        self.layout.resize(at + LOCATOR_FIXED, 0);
        xor_into(&mut self.layout[at..], &self.d[pos + 4..pos + 4 + LOCATOR_FIXED], &p);
    }

    pub fn end(&mut self, pos: usize, comment: Range<usize>) {
        self.layout.push(END);
        let p = self.st.end(pos as u64, comment.len());
        self.bytes(comment);
        let at = self.layout.len();
        self.layout.resize(at + END_FIXED, 0);
        xor_into(&mut self.layout[at..], &self.d[pos + 4..pos + 4 + END_FIXED], &p);
        self.st.after_end();
    }
}

/// Reads the layout: varints, bytes and ranges of the other inputs, all checked.
struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8> {
        let b = *self.d.get(self.at).ok_or_else(corrupt)?;
        self.at += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64> {
        Ok(varint::get(self.d, &mut self.at)?)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let r = self.d.get(self.at..self.at.checked_add(n).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        self.at += n;
        Ok(r)
    }

    /// A length-prefixed field of at most `max` bytes.
    fn bytes(&mut self, max: u64) -> Result<&'a [u8]> {
        let n = self.varint()?;
        if n > max {
            return Err(corrupt());
        }
        self.take(n as usize)
    }
}

/// The next `n` bytes of an input at `*p`.
fn take(d: &[u8], p: &mut usize, n: u64) -> Result<Range<usize>> {
    let end = usize::try_from(n).ok().and_then(|n| p.checked_add(n)).filter(|&e| e <= d.len()).ok_or_else(corrupt)?;
    let r = *p..end;
    *p = end;
    Ok(r)
}

/// The deflated bodies that a layout makes, in order: the ranges of their data and records.
pub fn jobs(layout: &[u8], data: &[u8], records: &[u8]) -> Result<Vec<(Range<usize>, Range<usize>)>> {
    let mut out = Vec::new();
    let mut r = Reader { d: layout, at: 0 };
    r.varint()?;
    let (mut dp, mut rp) = (0, 0);
    while r.at < layout.len() {
        let tag = r.u8()?;
        if tag == LOCAL {
            match r.u8()? {
                0 => {
                    take(data, &mut dp, r.varint()?)?;
                }
                1 => {
                    let d = take(data, &mut dp, r.varint()?)?;
                    out.push((d, take(records, &mut rp, r.varint()?)?));
                }
                2 => {
                    r.varint()?;
                    r.take(4)?;
                    r.varint()?;
                }
                3 => {
                    r.varint()?;
                }
                _ => return Err(corrupt()),
            }
        }
        skip(&mut r, tag)?;
    }
    if dp != data.len() || rp != records.len() {
        return Err(corrupt());
    }
    Ok(out)
}

/// Skips the rest of a record of `tag` after what [`jobs`] reads of it.
fn skip(r: &mut Reader, tag: u8) -> Result<()> {
    match tag {
        GAP => {
            r.varint()?;
        }
        LOCAL => {
            let desc = Desc::from_u8(r.u8()?).ok_or_else(corrupt)?;
            r.bytes(0xffff)?;
            r.bytes(0xffff)?;
            r.take(LOCAL_FIXED + desc.fields())?;
        }
        CENTRAL => {
            r.varint()?;
            let same = r.u8()?;
            if same > 3 {
                return Err(corrupt());
            }
            if same & 1 == 0 {
                r.bytes(0xffff)?;
            }
            if same & 2 == 0 {
                r.bytes(0xffff)?;
            }
            r.bytes(0xffff)?;
            r.take(CENTRAL_FIXED)?;
        }
        END64 => {
            r.bytes(u64::MAX)?;
            r.take(END64_FIXED)?;
        }
        LOCATOR => {
            r.take(LOCATOR_FIXED)?;
        }
        END => {
            r.bytes(0xffff)?;
            r.take(END_FIXED)?;
        }
        _ => return Err(corrupt()),
    }
    Ok(())
}

/// The archive of `len` bytes that `layout` makes from `data`, `kept` and `bodies`, the deflated
/// bodies in order (those that [`jobs`] lists).
pub fn build(layout: &[u8], data: &[u8], kept: &[u8], bodies: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let out = assemble(layout, data, kept, bodies, len)?;
    if out.len() as u64 != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}

/// [`build`], with an output of at most `len` bytes.
pub fn assemble(layout: &[u8], data: &[u8], kept: &[u8], bodies: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out: Vec<u8> = Vec::with_capacity(len.min(1 << 32));
    // Bytes are added only where they fit within `len`.
    let room =
        |out: &Vec<u8>, n: usize| if n <= len - out.len() { Ok(()) } else { Err(Error("output past its length")) };
    let mut r = Reader { d: layout, at: 0 };
    let mut st = State::new(varint::unzz(r.varint()?));
    let (mut dp, mut kp) = (0, 0);
    let mut bodies = bodies.iter();
    while r.at < layout.len() {
        let pos = out.len() as u64;
        match r.u8()? {
            GAP => {
                let k = take(kept, &mut kp, r.varint()?)?;
                room(&out, k.len())?;
                out.extend_from_slice(&kept[k]);
            }
            LOCAL => {
                // the body, from an input or from the archive so far, and its entry
                let (kind, body, within, entry): (Body, &[u8], Option<Range<usize>>, Entry) = match r.u8()? {
                    0 => {
                        let b = &data[take(data, &mut dp, r.varint()?)?];
                        let n = b.len() as u64;
                        (Body::Stored, b, None, Entry { crc: crc32fast::hash(b), csize: n, usize: n })
                    }
                    1 => {
                        let b = &data[take(data, &mut dp, r.varint()?)?];
                        r.varint()?;
                        let s = *bodies.next().ok_or_else(corrupt)?;
                        let entry = Entry { crc: crc32fast::hash(b), csize: s.len() as u64, usize: b.len() as u64 };
                        (Body::Deflated, s, None, entry)
                    }
                    2 => {
                        let k = &kept[take(kept, &mut kp, r.varint()?)?];
                        let crc = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
                        (Body::Kept, k, None, Entry { crc, csize: k.len() as u64, usize: r.varint()? })
                    }
                    3 => {
                        let back = r.varint()?;
                        let s = usize::try_from(back)
                            .ok()
                            .filter(|&b| b > 0)
                            .and_then(|b| st.seen.len().checked_sub(b))
                            .map(|i| &st.seen[i])
                            .ok_or_else(corrupt)?;
                        (Body::Same, &[][..], Some(s.body.clone()), s.entry)
                    }
                    _ => return Err(corrupt()),
                };
                let desc = Desc::from_u8(r.u8()?).ok_or_else(corrupt)?;
                let name = r.bytes(0xffff)?;
                let mut extra = r.bytes(0xffff)?.to_vec();
                let zip64 = zip::zip64(&extra).is_some();
                let x = r.take(LOCAL_FIXED)?;
                // the flags first: the CRC and sizes are predicted by them
                let flags = u16_at(x, 2) ^ u16_at(&st.local, 2);
                let mut fixed = [0; LOCAL_FIXED];
                xor_into(&mut fixed, x, &st.local(kind, &entry, flags, zip64, name.len(), extra.len()));
                xor_zip64(&mut extra, &State::local_zip64(&entry, flags));
                let mut dfields = State::desc(&entry, desc);
                let dx = r.take(desc.fields())?;
                dfields.iter_mut().zip(dx).for_each(|(a, b)| *a ^= b);
                let blen = within.as_ref().map_or(body.len(), |w| w.len());
                room(&out, 30 + name.len() + extra.len())?;
                out.extend_from_slice(&zip::LOCAL);
                out.extend_from_slice(&fixed);
                let name_at = out.len();
                out.extend_from_slice(name);
                out.extend_from_slice(&extra);
                let body_at = out.len();
                room(&out, blen)?;
                match within {
                    Some(w) => out.extend_from_within(w),
                    None => out.extend_from_slice(body),
                }
                room(&out, desc.bytes())?;
                if desc.sig() {
                    out.extend_from_slice(&zip::DESC);
                }
                out.extend_from_slice(&dfields);
                st.local = fixed;
                st.seen.push(Seen {
                    pos,
                    fixed,
                    name: name_at..name_at + name.len(),
                    extra: name_at + name.len()..body_at,
                    body: body_at..body_at + blen,
                    entry,
                });
            }
            CENTRAL => {
                let local = st.referred(r.varint()?)?;
                let same = r.u8()?;
                // the name and extra field of the local header, where they are the same
                let s = match local {
                    Some(i) => Some(&st.seen[i]),
                    None if same == 0 => None,
                    None => return Err(corrupt()),
                };
                if same > 3 {
                    return Err(corrupt());
                }
                let name = match s {
                    Some(s) if same & 1 != 0 => out[s.name.clone()].to_vec(),
                    _ => r.bytes(0xffff)?.to_vec(),
                };
                let mut extra = match s {
                    Some(s) if same & 2 != 0 => out[s.extra.clone()].to_vec(),
                    _ => r.bytes(0xffff)?.to_vec(),
                };
                let comment = r.bytes(0xffff)?;
                let mut fixed = [0; CENTRAL_FIXED];
                xor_into(
                    &mut fixed,
                    r.take(CENTRAL_FIXED)?,
                    &st.central(local, name.len(), extra.len(), comment.len()),
                );
                if same & 2 == 0 {
                    xor_zip64(&mut extra, &st.central_zip64(local, &fixed));
                }
                let n = 4 + CENTRAL_FIXED + name.len() + extra.len() + comment.len();
                room(&out, n)?;
                out.extend_from_slice(&zip::CENTRAL);
                out.extend_from_slice(&fixed);
                out.extend_from_slice(&name);
                out.extend_from_slice(&extra);
                out.extend_from_slice(comment);
                st.after_central(local, pos, &fixed, n);
            }
            END64 => {
                let ext = r.bytes(u64::MAX)?;
                let mut fixed = [0; END64_FIXED];
                xor_into(&mut fixed, r.take(END64_FIXED)?, &st.end64(pos, ext.len()));
                room(&out, 4 + END64_FIXED + ext.len())?;
                out.extend_from_slice(&zip::END64);
                out.extend_from_slice(&fixed);
                out.extend_from_slice(ext);
                st.end64 = Some(pos);
            }
            LOCATOR => {
                let mut fixed = [0; LOCATOR_FIXED];
                xor_into(&mut fixed, r.take(LOCATOR_FIXED)?, &st.locator());
                room(&out, 4 + LOCATOR_FIXED)?;
                out.extend_from_slice(&zip::LOCATOR);
                out.extend_from_slice(&fixed);
            }
            END => {
                let comment = r.bytes(0xffff)?;
                let mut fixed = [0; END_FIXED];
                xor_into(&mut fixed, r.take(END_FIXED)?, &st.end(pos, comment.len()));
                room(&out, 4 + END_FIXED + comment.len())?;
                out.extend_from_slice(&zip::END);
                out.extend_from_slice(&fixed);
                out.extend_from_slice(comment);
                st.after_end();
            }
            _ => return Err(corrupt()),
        }
    }
    if dp != data.len() || kp != kept.len() || bodies.next().is_some() {
        return Err(corrupt());
    }
    Ok(out)
}

/// How the bodies of the local entries of a layout are made, in order.
pub fn bodies(layout: &[u8]) -> Result<Vec<Body>> {
    let mut out = Vec::new();
    let mut r = Reader { d: layout, at: 0 };
    r.varint()?;
    while r.at < layout.len() {
        let tag = r.u8()?;
        if tag == LOCAL {
            let b = r.u8()?;
            out.push(
                [Body::Stored, Body::Deflated, Body::Kept, Body::Same].get(b as usize).copied().ok_or_else(corrupt)?,
            );
            match b {
                1 => {
                    r.varint()?;
                    r.varint()?;
                }
                2 => {
                    r.varint()?;
                    r.take(4)?;
                    r.varint()?;
                }
                _ => {
                    r.varint()?;
                }
            }
        }
        skip(&mut r, tag)?;
    }
    Ok(out)
}
