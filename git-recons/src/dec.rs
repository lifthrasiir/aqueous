//! Reconstruction: a view over the streams that rebuilds any file.
//!
//! Working memory does not grow with the objects: `open` keeps a cursor every [`CP`] files
//! and every [`CP`] entries of each pack, and records are parsed again from the nearest
//! one when needed (recently parsed ones are cached, as are resolved objects, both
//! bounded). Rebuilding a pack keeps its offsets and CRC-32s until its .idx is written.

use crate::enc::{Lru, Obj, lf_to_crlf};
use crate::gitfile;
use crate::obj::{self, Id};
use crate::rd::{Rd, Ref, get_ref};
use crate::rec::*;
use crate::table::Ids;
use crate::typed::{Lit, Srcs, State, dec_run};
use crate::zl;
use crate::{Error, NSTREAMS, Result, S_ENTRY, S_FILES, S_OPS, S_RAW, S_REFS, S_SHA, S_TEXT, corrupt};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Files (and pack entries) between cursors kept by `open`.
const CP: usize = 64;
/// Parsed records kept.
const EINFO_CACHE: usize = 1 << 14;

/// The state of the file table before file `i`.
#[derive(Clone)]
struct FCur {
    i: usize,
    at: usize,
    prev: String,
    offs: [usize; NSTREAMS],
    prev_blob: i64,
    prev_loose: i64,
    /// The highest table index of a loose object so far (-1).
    max_loose: i64,
    /// Global index of the next entry (pack entries and loose objects, in file order).
    ent: usize,
    /// Packs so far.
    pack: usize,
}

struct FileRec {
    path: String,
    kind: u8,
    /// SAME/IDX/REV: the file; BLOB/LOOSE: the table index; GIT: the flags.
    arg: u64,
    /// PACK: version, trailer mode, count, index into `packs`.
    version: u32,
    trailer: u8,
    /// PACK: the default level.
    def: u8,
    count: usize,
    /// PACK: its size.
    size: usize,
    pack: usize,
    offs: [usize; NSTREAMS],
    lens: [usize; NSTREAMS],
    /// The global index of its (first) entry.
    ent0: usize,
}

/// The state of a pack's records before an entry.
#[derive(Clone, Copy)]
struct ECur {
    at: usize,
    offs: [usize; REC_N],
    next_new: u32,
}

struct PackIdx {
    /// The end of its records; its default level.
    entry_end: usize,
    def: u8,
    e0: usize,
    n: usize,
    /// Table indices it introduces: j0..j0 + nnew.
    j0: u32,
    nnew: u32,
    /// Bounds of its parts of the streams ops..text.
    end: [usize; REC_N],
    /// Before entries 0, CP, 2 CP, ...
    cps: Vec<ECur>,
}

#[derive(Clone, Copy, Debug)]
enum Base {
    None,
    Ent(usize),
    /// A ref-delta on an id with no entry.
    Missing,
}

/// A parsed record.
struct EInfo {
    rec: Rec,
    j: u32,
    /// Where its parts of the streams ops through text start.
    offs: [usize; REC_N],
    /// The type, from the record or (type 0) the entry's header.
    typ: u8,
    base: Base,
    /// ref-delta: the base id.
    base_id: Id,
    loose: bool,
    /// The pack's default level (loose: 1).
    def: u8,
    /// The type at the bottom of its chain, once known (0: not yet).
    chain: Cell<u8>,
}

/// A rebuilt pack: its bytes, entry offsets and CRC-32s.
struct Built {
    bytes: Vec<u8>,
    starts: Vec<u64>,
    crcs: Vec<u32>,
}

pub struct Archive<'a> {
    s: [&'a [u8]; NSTREAMS],
    ids: Ids<'a>,
    nfiles: usize,
    /// Before files 0, CP, 2 CP, ..., with the path of that file.
    fcps: Vec<(FCur, String)>,
    packs: Vec<PackIdx>,
    total_new: u32,
    einfo: RefCell<HashMap<usize, Rc<EInfo>>>,
    cache: RefCell<Lru>,
    built: RefCell<HashMap<usize, Rc<Built>>>,
    /// Trailers of packs rebuilt and handed out.
    trailers: RefCell<HashMap<usize, Id>>,
}

impl<'a> Archive<'a> {
    /// Parses the file table and every object record, keeping cursors.
    pub fn open(s: &[&'a [u8]; NSTREAMS]) -> Result<Self> {
        Self::open_with(s, crate::DEFAULT_CACHE)
    }

    /// `open` with a budget for resolved objects.
    pub fn open_with(s: &[&'a [u8]; NSTREAMS], cache_bytes: usize) -> Result<Self> {
        if !s[S_SHA].len().is_multiple_of(20) {
            return Err(corrupt());
        }
        let mut a = Archive {
            s: *s,
            ids: Ids(s[S_SHA]),
            nfiles: 0,
            fcps: vec![],
            packs: vec![],
            total_new: 0,
            einfo: RefCell::new(HashMap::new()),
            cache: RefCell::new(Lru::new(cache_bytes)),
            built: RefCell::new(HashMap::new()),
            trailers: RefCell::new(HashMap::new()),
        };
        let mut r = Rd::new(s[S_FILES]);
        a.nfiles = r.usize()?;
        let mut c = FCur {
            i: 0,
            at: r.p,
            prev: String::new(),
            offs: [0; NSTREAMS],
            prev_blob: -1,
            prev_loose: -1,
            max_loose: -1,
            ent: 0,
            pack: 0,
        };
        let mut next_new = 0u32;
        while c.i < a.nfiles {
            let before = c.clone();
            let f = a.read_file(&mut c)?;
            if f.kind == K_PACK {
                let p = a.index_pack(&f, next_new)?;
                next_new += p.nnew;
                a.packs.push(p);
            }
            if before.i.is_multiple_of(CP) {
                a.fcps.push((before, f.path));
            }
        }
        a.total_new = next_new;
        if c.at != s[S_FILES].len() || (S_ENTRY..NSTREAMS).any(|k| c.offs[k] != s[k].len()) {
            return Err(corrupt());
        }
        if c.max_loose >= a.ids.len() as i64 {
            return Err(corrupt());
        }
        Ok(a)
    }

    /// Parses the record of the file at `c` and moves past it.
    fn read_file(&self, c: &mut FCur) -> Result<FileRec> {
        let s = &self.s;
        let n = self.nfiles;
        let i = c.i;
        let mut r = Rd::at(s[S_FILES], c.at);
        let kind = r.u8()?;
        let common = r.usize()?;
        let slen = r.usize()?;
        let suffix = r.bytes(slen)?;
        let mut path = c.prev.as_bytes().get(..common).ok_or_else(corrupt)?.to_vec();
        path.extend_from_slice(suffix);
        let mut path = String::from_utf8(path).map_err(|_| corrupt())?;
        let mut f = FileRec {
            path: String::new(),
            kind,
            arg: 0,
            version: 0,
            trailer: 0,
            def: 1,
            count: 0,
            size: 0,
            pack: 0,
            offs: [0; NSTREAMS],
            lens: [0; NSTREAMS],
            ent0: c.ent,
        };
        let rel = |r: &mut Rd| -> Result<u64> {
            let k = i as i64 + r.sv()?;
            if k < 0 || k as usize >= n || k as usize == i {
                return Err(corrupt());
            }
            Ok(k as u64)
        };
        let nids = self.ids.len();
        let idx = |r: &mut Rd, prev: &mut i64| -> Result<u64> {
            let j = *prev + 1 + r.sv()?;
            if j < 0 || j as usize >= nids {
                return Err(corrupt());
            }
            *prev = j;
            Ok(j as u64)
        };
        match kind {
            K_RAW => {}
            K_SAME | K_IDX | K_REV => f.arg = rel(&mut r)?,
            K_BLOB | K_BLOB_CRLF => f.arg = idx(&mut r, &mut c.prev_blob)?,
            K_LOOSE => {
                f.arg = idx(&mut r, &mut c.prev_loose)?;
                c.max_loose = c.max_loose.max(f.arg as i64);
                path = loose_path_of(&path, self.ids.get(f.arg as usize));
                c.ent += 1;
            }
            K_PACK => {
                f.version = u32::try_from(r.uv()?).map_err(|_| corrupt())?;
                f.count = usize::try_from(r.uv()?).map_err(|_| corrupt())?;
                f.size = r.usize()?;
                f.def = r.u8()?;
                f.trailer = r.u8()?;
                if !(1..=9).contains(&f.def) || f.trailer > 2 || f.count > u32::MAX as usize {
                    return Err(corrupt());
                }
                f.pack = c.pack;
                c.pack += 1;
                c.ent += f.count;
            }
            K_GIT => f.arg = r.u8()? as u64,
            _ => return Err(corrupt()),
        }
        let mask = r.uv()?;
        if mask >> (NSTREAMS - S_ENTRY) != 0 {
            return Err(corrupt());
        }
        for k in S_ENTRY..NSTREAMS {
            f.offs[k] = c.offs[k];
            if mask & 1 << (k - S_ENTRY) != 0 {
                f.lens[k] = r.usize()?;
                c.offs[k] = c.offs[k].checked_add(f.lens[k]).ok_or_else(corrupt)?;
                if c.offs[k] > s[k].len() {
                    return Err(corrupt());
                }
            }
        }
        f.path = path;
        c.prev.clone_from(&f.path);
        c.at = r.p;
        c.i += 1;
        Ok(f)
    }

    /// Reads a pack's records once: cursors, and the table indices it introduces.
    fn index_pack(&self, f: &FileRec, j0: u32) -> Result<PackIdx> {
        let end: [usize; REC_N] = std::array::from_fn(|k| f.offs[REC_FIRST + k] + f.lens[REC_FIRST + k]);
        let entry_end = f.offs[S_ENTRY] + f.lens[S_ENTRY];
        let mut p = PackIdx {
            entry_end,
            def: f.def,
            e0: f.ent0,
            n: f.count,
            j0,
            nnew: 0,
            end,
            cps: Vec::with_capacity(f.count / CP + 1),
        };
        let mut c = ECur { at: f.offs[S_ENTRY], offs: std::array::from_fn(|k| f.offs[REC_FIRST + k]), next_new: j0 };
        for k in 0..f.count {
            if k % CP == 0 {
                p.cps.push(c);
            }
            self.step(&p, &mut c)?;
        }
        // (The pack's raw part ends with its trailer when kept: the records do not cover
        // it.)
        if c.at != entry_end {
            return Err(corrupt());
        }
        p.nnew = c.next_new - j0;
        Ok(p)
    }

    /// Parses the record at `c` and moves past it: the record, its table index and offsets.
    fn step(&self, p: &PackIdx, c: &mut ECur) -> Result<(Rec, u32, [usize; REC_N])> {
        let mut r = Rd::at(&self.s[S_ENTRY][..p.entry_end], c.at);
        let rec = Rec::read(&mut r)?;
        let j = if rec.h & H_DUP != 0 {
            let j = c.next_new as i64 + rec.dup;
            if j < 0 || j >= c.next_new as i64 {
                return Err(corrupt());
            }
            j as u32
        } else {
            if c.next_new as usize >= self.ids.len() {
                return Err(corrupt());
            }
            c.next_new += 1;
            c.next_new - 1
        };
        let offs = c.offs;
        for k in 0..REC_N {
            c.offs[k] = c.offs[k].checked_add(rec.lens[k] as usize).ok_or_else(corrupt)?;
            if c.offs[k] > p.end[k] {
                return Err(corrupt());
            }
        }
        c.at = r.p;
        Ok((rec, j, offs))
    }

    /// The cursor of the file table before file `i`.
    fn fcur(&self, i: usize) -> Result<FCur> {
        let mut c = self.fcps[i / CP].0.clone();
        while c.i < i {
            self.read_file(&mut c)?;
        }
        Ok(c)
    }

    fn file_rec(&self, i: usize) -> Result<FileRec> {
        let mut c = self.fcur(i)?;
        self.read_file(&mut c)
    }

    /// The pack holding global entry `e`, if any.
    fn pack_at(&self, e: usize) -> Option<usize> {
        let p = self.packs.partition_point(|p| p.e0 <= e).checked_sub(1)?;
        (e < self.packs[p].e0 + self.packs[p].n).then_some(p)
    }

    /// The parsed record of global entry `e`.
    fn einfo(&self, e: usize) -> Result<Rc<EInfo>> {
        if let Some(x) = self.einfo.borrow().get(&e) {
            return Ok(x.clone());
        }
        let x = match self.pack_at(e) {
            Some(p) => {
                let pk = &self.packs[p];
                let k = e - pk.e0;
                let mut c = pk.cps[k / CP];
                for _ in 0..k % CP {
                    self.step(pk, &mut c)?;
                }
                let (rec, j, offs) = self.step(pk, &mut c)?;
                self.make_einfo(e, Some((p, k)), rec, j, offs, pk.def)?
            }
            None => {
                // A loose object: find its file from the cursors.
                let g = self.fcps.partition_point(|c| c.0.ent <= e).checked_sub(1).ok_or_else(corrupt)?;
                let mut c = self.fcps[g].0.clone();
                loop {
                    if c.i >= self.nfiles {
                        return Err(corrupt());
                    }
                    let ent = c.ent;
                    let f = self.read_file(&mut c)?;
                    if f.kind == K_LOOSE && ent == e {
                        let mut r = Rd::at(&self.s[S_ENTRY][..f.offs[S_ENTRY] + f.lens[S_ENTRY]], f.offs[S_ENTRY]);
                        let rec = Rec::read(&mut r)?;
                        if !r.at_end() || !(1..=4).contains(&rec.typ()) {
                            return Err(corrupt());
                        }
                        let offs: [usize; REC_N] = std::array::from_fn(|k| f.offs[REC_FIRST + k]);
                        if (0..REC_N).any(|k| rec.lens[k] as usize != f.lens[REC_FIRST + k]) {
                            return Err(corrupt());
                        }
                        break self.make_einfo(e, None, rec, f.arg as u32, offs, 1)?;
                    }
                    if ent > e {
                        return Err(corrupt());
                    }
                }
            }
        };
        let x = Rc::new(x);
        let mut m = self.einfo.borrow_mut();
        if m.len() >= EINFO_CACHE {
            m.clear();
        }
        m.insert(e, x.clone());
        Ok(x)
    }

    fn make_einfo(
        &self,
        e: usize,
        pk: Option<(usize, usize)>,
        rec: Rec,
        j: u32,
        offs: [usize; REC_N],
        def: u8,
    ) -> Result<EInfo> {
        let mut x = EInfo {
            typ: rec.typ(),
            rec,
            j,
            offs,
            base: Base::None,
            base_id: [0; 20],
            loose: pk.is_none(),
            def,
            chain: Cell::new(0),
        };
        let ofs = |dist: u64| -> Result<Base> {
            let (_, k) = pk.ok_or_else(corrupt)?;
            if dist == 0 || dist as usize > k {
                return Err(corrupt());
            }
            Ok(Base::Ent(e - dist as usize))
        };
        match x.typ {
            0 => {
                let b = self.part(&x, S_RAW);
                let (t, _, p) = obj::entry_header_any(b).ok_or_else(corrupt)?;
                x.typ = t;
                match t {
                    obj::OFS => x.base = if x.rec.dist == 0 { Base::Missing } else { ofs(x.rec.dist - 1)? },
                    obj::REF => {
                        x.base_id = b.get(p..p + 20).ok_or_else(corrupt)?.try_into().unwrap();
                        x.base = match x.rec.dist {
                            0 => Base::Missing,
                            d => {
                                let jb = j as i64 + recons_common::varint::unzz(d - 1);
                                if jb < 0 || jb as usize >= self.ids.len() || *self.ids.get(jb as usize) != x.base_id {
                                    return Err(corrupt());
                                }
                                self.loc(jb as u32)?.map_or(Base::Missing, Base::Ent)
                            }
                        };
                    }
                    1..=4 => {}
                    _ => return Err(Error("unsupported entry type")),
                }
            }
            obj::OFS => x.base = ofs(x.rec.dist)?,
            obj::REF => {
                let mut r = Rd::new(self.part(&x, S_REFS));
                let mut cur = j as i64;
                match get_ref(&mut r, &mut cur, self.ids.len())? {
                    Ref::Idx(jb) => {
                        x.base_id = *self.ids.get(jb as usize);
                        x.base = self.loc(jb)?.map_or(Base::Missing, Base::Ent);
                    }
                    // Not in the table, so nowhere.
                    Ref::Id(id) => {
                        x.base_id = id;
                        x.base = Base::Missing;
                    }
                }
            }
            _ => {}
        }
        Ok(x)
    }

    /// The entry of table index `j`: the first pack entry with it, else the first loose
    /// object.
    fn loc(&self, j: u32) -> Result<Option<usize>> {
        if j < self.total_new {
            let p = self.packs.partition_point(|p| p.j0 + p.nnew <= j);
            let pk = self.packs.get(p).ok_or_else(corrupt)?;
            let g = pk.cps.partition_point(|c| c.next_new <= j).checked_sub(1).ok_or_else(corrupt)?;
            let mut c = pk.cps[g];
            for k in g * CP..pk.n {
                let nn = c.next_new;
                let (rec, _, _) = self.step(pk, &mut c)?;
                if rec.h & H_DUP == 0 && nn == j {
                    return Ok(Some(pk.e0 + k));
                }
            }
            return Err(corrupt());
        }
        let g = self.fcps.partition_point(|c| c.0.max_loose < j as i64).saturating_sub(1);
        let mut c = self.fcps[g].0.clone();
        while c.i < self.nfiles {
            let ent = c.ent;
            let f = self.read_file(&mut c)?;
            if f.kind == K_LOOSE && f.arg == j as u64 {
                return Ok(Some(ent));
            }
            if c.max_loose > j as i64 {
                break;
            }
        }
        Ok(None)
    }

    fn part(&self, x: &EInfo, k: usize) -> &'a [u8] {
        let i = k - REC_FIRST;
        &self.s[k][x.offs[i]..x.offs[i] + x.rec.lens[i] as usize]
    }

    /// The size an entry claims.
    fn claimed(&self, e: usize) -> Result<u64> {
        let x = self.einfo(e)?;
        Ok(if x.rec.typ() == 0 { claimed_raw(self.part(&x, S_RAW)) } else { x.rec.size })
    }

    fn chain_type(&self, e: usize) -> Result<u8> {
        let mut cur = e;
        let mut seen = vec![];
        let t = loop {
            let x = self.einfo(cur)?;
            if x.chain.get() != 0 {
                break x.chain.get();
            }
            match x.base {
                Base::None => break x.typ,
                Base::Ent(k) => cur = k,
                Base::Missing => return Err(corrupt()),
            }
            seen.push(x);
            if seen.len() > 100_000 {
                return Err(corrupt());
            }
        };
        for x in seen {
            x.chain.set(t);
        }
        Ok(t)
    }

    /// An entry's payload (a loose object's without its header): the object or the delta.
    /// `base`: the base object, if at hand.
    fn payload(&self, x: &EInfo, base: Option<&Obj>) -> Result<Vec<u8>> {
        let r = &x.rec;
        if r.typ() == 0 {
            let b = self.part(x, S_RAW);
            let (t, size, mut p) = obj::entry_header_any(b).ok_or_else(corrupt)?;
            match t {
                obj::OFS => p += obj::parse_ofs(&b[p..]).ok_or_else(corrupt)?.1,
                obj::REF => p += 20,
                _ => {}
            }
            return Ok(obj::inflate(b.get(p..).ok_or_else(corrupt)?, size as usize).ok_or_else(corrupt)?.0);
        }
        if r.zmode() == Z_VERBATIM {
            let (mut p, _) = obj::inflate(self.part(x, S_RAW), r.size as usize).ok_or_else(corrupt)?;
            if x.loose {
                let (_, _, hl) = obj::parse_loose_header(&p).ok_or_else(corrupt)?;
                p.drain(..hl);
            }
            return Ok(p);
        }
        let mut cur = x.j as i64;
        let mut refs = Rd::new(self.part(x, S_REFS));
        if x.typ == obj::REF {
            get_ref(&mut refs, &mut cur, self.ids.len())?;
        }
        if x.typ <= 4 {
            let kind = Lit::of_type(x.typ);
            let mut src = Srcs { lit: Rd::new(self.part(x, kind.stream())), refs };
            let n = usize::try_from(r.size).map_err(|_| corrupt())?;
            let mut out = Vec::with_capacity(n.min(1 << 26));
            dec_run(&mut State::new(kind), n, self.ids, &mut cur, &mut src, &mut out)?;
            return Ok(out);
        }
        if r.h & H_PLAIN != 0 {
            return Ok(self.part(x, S_RAW).to_vec());
        }
        let Base::Ent(b) = x.base else {
            if r.h & H_XBASE == 0 || r.h & H_UNTYPED == 0 {
                return Err(corrupt());
            }
            return self.delta(x, r.base_size, Lit::Plain, None, &mut cur, refs);
        };
        let bs = if r.h & H_XBASE != 0 { r.base_size } else { self.claimed(b)? };
        let kind = if r.h & H_UNTYPED != 0 { Lit::Plain } else { Lit::of_type(self.chain_type(b)?) };
        if kind == Lit::Plain {
            return self.delta(x, bs, kind, None, &mut cur, refs);
        }
        match base {
            Some(o) => self.delta(x, bs, kind, Some(o), &mut cur, refs),
            None => {
                let o = self.resolve(b)?;
                self.delta(x, bs, kind, Some(&o), &mut cur, refs)
            }
        }
    }

    fn delta(&self, x: &EInfo, bs: u64, kind: Lit, base: Option<&Obj>, cur: &mut i64, refs: Rd) -> Result<Vec<u8>> {
        let ts = x.rec.size;
        let mut out = Vec::with_capacity(ts.min(1 << 24) as usize + 16);
        obj::put_dsize(bs, &mut out);
        obj::put_dsize(ts, &mut out);
        let mut ops = Rd::new(self.part(x, S_OPS));
        let mut src = Srcs { lit: Rd::new(self.part(x, kind.stream())), refs };
        let mut st = State::new(kind);
        let mut produced = 0u64;
        let mut prev_end = 0i64;
        while produced < ts {
            let v = ops.uv()?;
            if v & 1 == 0 {
                let n = (v >> 1) as usize;
                if !(1..=127).contains(&n) {
                    return Err(corrupt());
                }
                out.push(n as u8);
                dec_run(&mut st, n, self.ids, cur, &mut src, &mut out)?;
                produced += n as u64;
            } else {
                let size = v >> 1;
                let off = prev_end + ops.sv()?;
                if off < 0 || off > u32::MAX as i64 || size == 0 || size > 0xff_ffff {
                    return Err(corrupt());
                }
                let off = off as u64;
                prev_end = (off + size) as i64;
                obj::put_copy(off, size, &mut out);
                if let Some(o) = base {
                    st.feed_all(o.data.get(off as usize..(off + size) as usize).ok_or_else(corrupt)?);
                }
                produced += size;
            }
        }
        if produced != ts {
            return Err(corrupt());
        }
        Ok(out)
    }

    /// The object at entry `e`, deltas applied.
    fn resolve(&self, e: usize) -> Result<Rc<Obj>> {
        let mut chain = vec![];
        let mut cur = e;
        let mut base = loop {
            if let Some(o) = self.cache.borrow_mut().get(cur) {
                break o;
            }
            let x = self.einfo(cur)?;
            match x.base {
                Base::None => {
                    let o = Rc::new(Obj { typ: x.typ, data: self.payload(&x, None)? });
                    self.cache.borrow_mut().put(cur, o.clone());
                    break o;
                }
                Base::Ent(k) => {
                    chain.push((cur, x));
                    cur = k;
                    if chain.len() > 100_000 {
                        return Err(corrupt());
                    }
                }
                Base::Missing => return Err(Error("delta base missing")),
            }
        };
        for (ent, x) in chain.into_iter().rev() {
            let delta = self.payload(&x, Some(&base))?;
            let data = obj::apply_delta(&base.data, &delta).ok_or_else(corrupt)?;
            let o = Rc::new(Obj { typ: base.typ, data });
            self.cache.borrow_mut().put(ent, o.clone());
            base = o;
        }
        Ok(base)
    }

    /// The zlib stream of an entry's payload (for a loose object, with its header).
    fn zlib(&self, x: &EInfo, payload: &[u8], out: &mut Vec<u8>) {
        let r = &x.rec;
        match r.zmode() {
            Z_DEFAULT => zl::zlib(payload, x.def, out),
            Z_LEVEL => zl::zlib(payload, r.level, out),
            _ => out.extend_from_slice(self.part(x, S_RAW)),
        }
    }

    fn build_pack(&self, i: usize, f: &FileRec) -> Result<Rc<Built>> {
        if let Some(b) = self.built.borrow().get(&i) {
            return Ok(b.clone());
        }
        let pk = &self.packs[f.pack];
        // The size is a hint (the result is checked by its hash anyway).
        let mut out = Vec::with_capacity(f.size.min(f.lens.iter().sum::<usize>() * 4 + (1 << 20)));
        out.extend_from_slice(b"PACK");
        out.extend_from_slice(&f.version.to_be_bytes());
        out.extend_from_slice(&(f.count as u32).to_be_bytes());
        let mut starts = Vec::with_capacity(f.count);
        let mut crcs = Vec::with_capacity(f.count);
        let mut c = pk.cps.first().copied().unwrap_or(ECur { at: 0, offs: [0; REC_N], next_new: pk.j0 });
        for k in 0..f.count {
            let e = pk.e0 + k;
            let start = out.len();
            starts.push(start as u64);
            let (rec, j, offs) = self.step(pk, &mut c)?;
            let x = Rc::new(self.make_einfo(e, Some((f.pack, k)), rec, j, offs, f.def)?);
            {
                let mut m = self.einfo.borrow_mut();
                if m.len() >= EINFO_CACHE {
                    m.clear();
                }
                m.insert(e, x.clone());
            }
            if x.rec.typ() == 0 {
                out.extend_from_slice(self.part(&x, S_RAW));
            } else {
                let payload = self.payload(&x, None)?;
                obj::entry_header(x.typ, payload.len() as u64, &mut out);
                match x.typ {
                    obj::OFS => obj::put_ofs(start as u64 - starts[k - x.rec.dist as usize], &mut out),
                    obj::REF => out.extend_from_slice(&x.base_id),
                    _ => {}
                }
                self.zlib(&x, &payload, &mut out);
            }
            crcs.push(crc32fast::hash(&out[start..]));
        }
        let trailer = self.pack_trailer(i, f, Some(&out))?;
        out.extend_from_slice(&trailer);
        let b = Rc::new(Built { bytes: out, starts, crcs });
        self.built.borrow_mut().insert(i, b.clone());
        Ok(b)
    }

    /// A pack's trailer; `body` its bytes before it, if rebuilt.
    fn pack_trailer(&self, i: usize, f: &FileRec, body: Option<&[u8]>) -> Result<Id> {
        Ok(match f.trailer {
            1 => pack_name_id(&f.path).ok_or_else(corrupt)?,
            0 => match body {
                Some(b) => obj::sha1(&[b]),
                None if let Some(t) = self.trailers.borrow().get(&i) => *t,
                None => {
                    let b = self.build_pack(i, f)?;
                    b.bytes[b.bytes.len() - 20..].try_into().unwrap()
                }
            },
            _ => {
                let end = f.offs[S_RAW] + f.lens[S_RAW];
                self.s[S_RAW]
                    .get(end.checked_sub(20).ok_or_else(corrupt)?..end)
                    .ok_or_else(corrupt)?
                    .try_into()
                    .unwrap()
            }
        })
    }

    /// The ids of a pack's entries, in pack order.
    fn pack_ids(&self, f: &FileRec) -> Result<Vec<Id>> {
        let pk = &self.packs[f.pack];
        let mut c = pk.cps.first().copied().unwrap_or(ECur { at: 0, offs: [0; REC_N], next_new: pk.j0 });
        (0..f.count).map(|_| Ok(*self.ids.get(self.step(pk, &mut c)?.1 as usize))).collect()
    }

    /// The paths, in order.
    pub fn paths(&self) -> impl Iterator<Item = String> + '_ {
        let mut c = self.fcps.first().map(|x| x.0.clone());
        std::iter::from_fn(move || {
            let cur = c.as_mut()?;
            if cur.i >= self.nfiles {
                return None;
            }
            self.read_file(cur).ok().map(|f| f.path)
        })
    }

    pub fn len(&self) -> usize {
        self.nfiles
    }

    pub fn is_empty(&self) -> bool {
        self.nfiles == 0
    }

    pub fn get(&self, path: &str) -> Result<Vec<u8>> {
        let g = self.fcps.partition_point(|x| x.1.as_str() <= path).checked_sub(1).ok_or(Error("no such file"))?;
        let mut c = self.fcps[g].0.clone();
        while c.i < self.nfiles && c.i < (g + 1) * CP {
            let i = c.i;
            let f = self.read_file(&mut c)?;
            if f.path == path {
                return self.file(i, &f);
            }
        }
        Err(Error("no such file"))
    }

    /// File `i` (in path order), whose record is `f`.
    fn file(&self, i: usize, f: &FileRec) -> Result<Vec<u8>> {
        let part = |k: usize| &self.s[k][f.offs[k]..f.offs[k] + f.lens[k]];
        let target = |kind: u8| -> Result<(usize, FileRec)> {
            let k = f.arg as usize;
            let t = self.file_rec(k)?;
            if (kind == K_SAME && t.kind == K_SAME) || (kind != K_SAME && t.kind != K_PACK) {
                return Err(corrupt());
            }
            Ok((k, t))
        };
        Ok(match f.kind {
            K_RAW => part(S_RAW).to_vec(),
            K_SAME => {
                let (k, t) = target(K_SAME)?;
                self.file(k, &t)?
            }
            K_BLOB | K_BLOB_CRLF => {
                let e = self.loc(f.arg as u32)?.ok_or_else(corrupt)?;
                let o = self.resolve(e)?;
                if f.kind == K_BLOB { o.data.clone() } else { lf_to_crlf(&o.data) }
            }
            K_LOOSE => {
                if self.einfo(f.ent0)?.rec.zmode() == Z_VERBATIM {
                    return Ok(part(S_RAW).to_vec());
                }
                let x = self.einfo(f.ent0)?;
                let content = self.payload(&x, None)?;
                let mut p = Vec::with_capacity(content.len() + 32);
                obj::loose_header(x.typ, content.len() as u64, &mut p);
                p.extend_from_slice(&content);
                let mut out = vec![];
                self.zlib(&x, &p, &mut out);
                out
            }
            K_PACK => {
                // Handed out whole: what its .idx needed is done (it sorts before).
                let b = self.build_pack(i, f)?;
                self.built.borrow_mut().remove(&i);
                let bytes = Rc::try_unwrap(b).map_or_else(|b| b.bytes.clone(), |b| b.bytes);
                self.trailers.borrow_mut().insert(i, bytes[bytes.len() - 20..].try_into().unwrap());
                bytes
            }
            K_IDX => {
                let (k, t) = target(K_IDX)?;
                let b = self.build_pack(k, &t)?;
                gen_idx(&self.pack_ids(&t)?, &b.starts, &b.crcs, &b.bytes[b.bytes.len() - 20..])
            }
            K_REV => {
                let (k, t) = target(K_REV)?;
                gen_rev(&self.pack_ids(&t)?, &self.pack_trailer(k, &t, None)?)
            }
            K_GIT => {
                let mut er = Rd::new(part(S_ENTRY));
                let mut refs = Rd::new(part(S_REFS));
                gitfile::decode(f.arg as u8, &mut er, part(S_TEXT), &mut refs, self.ids)?
            }
            _ => return Err(corrupt()),
        })
    }

    /// Every file in order; a rebuilt pack is kept until its index files are done.
    pub fn for_each(&self, mut out: impl FnMut(&str, Vec<u8>) -> Result<()>) -> Result<()> {
        // When each rebuilt pack can go: after the last of it, its .idx and its .rev.
        let mut last_use: HashMap<usize, usize> = HashMap::new();
        let mut c = match self.fcps.first() {
            Some(x) => x.0.clone(),
            None => return Ok(()),
        };
        while c.i < self.nfiles {
            let i = c.i;
            let f = self.read_file(&mut c)?;
            let p = match f.kind {
                K_PACK => i,
                K_IDX | K_REV => f.arg as usize,
                _ => continue,
            };
            let u = last_use.entry(p).or_insert(i);
            *u = (*u).max(i);
        }
        let done: HashMap<usize, usize> = last_use.into_iter().map(|(p, i)| (i, p)).collect();
        let mut c = self.fcps[0].0.clone();
        while c.i < self.nfiles {
            let i = c.i;
            let f = self.read_file(&mut c)?;
            let b = self.file(i, &f)?;
            if let Some(p) = done.get(&i) {
                self.built.borrow_mut().remove(p);
            }
            out(&f.path, b)?;
        }
        Ok(())
    }
}
