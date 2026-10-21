//! Deconstruction.
//!
//! Working memory: the table (20 bytes an id, which become the `sha` stream, plus an index
//! of 8 bytes an id), 16 bytes per pack entry, the resolved-object cache (bounded), and
//! stream staging buffers that are flushed to a [`Sink`] after each file or pack entry.

use crate::gitfile;
use crate::obj::{self, Id, Op};
use crate::rd::{put_ref, put_sv, put_uv};
use crate::rec::*;
use crate::table::Table;
use crate::typed::{Lit, Sinks, State, enc_run};
use crate::zl;
use crate::{NSTREAMS, Options, S_ENTRY, S_FILES, S_OPS, S_RAW, S_REFS, S_SHA, Stats, Streams, budget};
use std::collections::HashMap;
use std::rc::Rc;
use xxhash_rust::xxh3::xxh3_128;

/// Where the streams go, piece by piece.
pub trait Sink {
    fn put(&mut self, k: usize, d: &[u8]);
    /// A whole piece handed over (the default copies it).
    fn put_owned(&mut self, k: usize, d: Vec<u8>) {
        self.put(k, &d)
    }
}

/// Streams in memory, as chunks until [`VecSink::finish`].
#[derive(Default)]
pub struct VecSink {
    chunks: [Vec<Vec<u8>>; NSTREAMS],
}

const CHUNK: usize = 1 << 20;

impl Sink for VecSink {
    fn put(&mut self, k: usize, mut d: &[u8]) {
        while !d.is_empty() {
            let c = match self.chunks[k].last_mut() {
                Some(c) if c.len() < c.capacity() => c,
                _ => {
                    self.chunks[k].push(Vec::with_capacity(CHUNK.max(d.len())));
                    self.chunks[k].last_mut().unwrap()
                }
            };
            let n = (c.capacity() - c.len()).min(d.len());
            c.extend_from_slice(&d[..n]);
            d = &d[n..];
        }
    }
    fn put_owned(&mut self, k: usize, d: Vec<u8>) {
        self.chunks[k].push(d);
    }
}

impl VecSink {
    /// Bytes held (capacity).
    pub fn bytes(&self) -> usize {
        self.chunks.iter().flatten().map(|c| c.capacity()).sum()
    }

    /// The streams, each made contiguous (chunks are freed as they are copied).
    pub fn finish(self) -> Streams {
        self.chunks.map(|cs| {
            if cs.len() == 1 {
                return cs.into_iter().next().unwrap();
            }
            let mut v = Vec::with_capacity(cs.iter().map(|c| c.len()).sum());
            for c in cs {
                v.extend_from_slice(&c);
            }
            v
        })
    }
}

/// Where an object lives in the input.
#[derive(Clone, Copy)]
struct Ent {
    file: usize,
    start: usize,
    end: usize,
    loose: bool,
    j: u32,
}

/// Type, base (an entry, or an id without one; `None` if not a delta) and payload.
type RawObj = (u8, Option<Result<usize, Id>>, Vec<u8>);

pub struct Obj {
    pub typ: u8,
    pub data: Vec<u8>,
}

/// Resolved objects, bounded by their bytes; the least recently used go first.
pub struct Lru {
    map: HashMap<usize, (Rc<Obj>, u64)>,
    bytes: usize,
    tick: u64,
    budget: usize,
}

impl Lru {
    pub fn new(budget: usize) -> Self {
        Lru { map: HashMap::new(), bytes: 0, tick: 0, budget }
    }
    pub fn get(&mut self, k: usize) -> Option<Rc<Obj>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&k).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }
    pub fn put(&mut self, k: usize, o: Rc<Obj>) {
        self.tick += 1;
        self.bytes += o.data.len() + 64;
        if let Some(old) = self.map.insert(k, (o, self.tick)) {
            self.bytes -= old.0.data.len() + 64;
        }
        if self.bytes > self.budget {
            let mut ticks: Vec<u64> = self.map.values().map(|e| e.1).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 2];
            let mut bytes = 0;
            self.map.retain(|_, e| {
                let keep = e.1 > cut;
                if keep {
                    bytes += e.0.data.len() + 64;
                }
                keep
            });
            self.bytes = bytes;
        }
    }
}

/// A pack file validated against its index: its entries in offset order.
struct Pack {
    file: usize,
    /// Global index of its first entry.
    e0: usize,
    /// Entry offsets, then the trailer's.
    offs: Vec<u64>,
    /// Table index per entry.
    js: Vec<u32>,
}

struct Enc<'a> {
    files: &'a [(String, Vec<u8>)],
    opt: &'a Options,
    tab: Table,
    /// Packs in file order; their entries are global entries 0.., loose objects follow.
    packs: Vec<Pack>,
    pack_ents: usize,
    /// Loose objects: file and table index.
    loose: Vec<(usize, u32)>,
    /// Table index → its entry (packs first, then loose objects).
    loc: Vec<u32>,
    cache: Lru,
    /// Staging: flushed to `sink` after each file or entry; `flushed` counts what went.
    s: Streams,
    flushed: [u64; NSTREAMS],
    sink: &'a mut dyn Sink,
    st: Stats,
    /// Per entry, whether its zlib stream is to be rebuilt by emulation (else kept).
    emulate: Vec<bool>,
    /// The level that last reproduced a stream, and the file's default level.
    last_level: u8,
    def: Option<u8>,
    /// The table index the next pack entry new to the table takes.
    next_new: u32,
}

/// Deconstructs `files` into `sink`; returns the xxh3-64 of each file (in path order) and
/// the stats.
pub fn encode(mut files: Vec<(String, Vec<u8>)>, opt: &Options, sink: &mut dyn Sink) -> (Vec<u64>, Stats) {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    let hashes = files.iter().map(|f| xxhash_rust::xxh3::xxh3_64(&f.1)).collect();
    let mut e = Enc {
        files: &files,
        opt,
        tab: Table::new(),
        packs: vec![],
        pack_ents: 0,
        loose: vec![],
        loc: vec![],
        cache: Lru::new(opt.cache_bytes),
        s: Default::default(),
        flushed: [0; NSTREAMS],
        sink,
        st: Stats::default(),
        emulate: vec![],
        last_level: 6,
        def: None,
        next_new: 0,
    };
    let same = e.find_same();
    let (pack_of_file, loose_of_file) = e.find_objects(&same);
    e.choose();
    put_uv(&mut e.s[S_FILES], files.len() as u64);
    let mut prev_path = "";
    let mut prev_blob = -1i64;
    let mut prev_loose = -1i64;
    let mut head = vec![];
    let mut fields = vec![];
    for i in 0..files.len() {
        let (path, data) = (&files[i].0, &files[i].1);
        let before: [u64; NSTREAMS] = std::array::from_fn(|k| e.len(k));
        head.clear();
        fields.clear();
        let kind;
        let mut path_dir: Option<&str> = None;
        if let Some(k) = same[i] {
            kind = K_SAME;
            put_sv(&mut fields, k as i64 - i as i64);
        } else if let Some(&p) = pack_of_file.get(&i) {
            kind = K_PACK;
            e.encode_pack(p, &mut fields);
        } else if let Some(&ge) = loose_of_file.get(&i) {
            kind = K_LOOSE;
            let j = e.ent(ge).j as i64;
            path_dir = Some(loose_path(path).unwrap().0);
            put_sv(&mut fields, j - (prev_loose + 1));
            prev_loose = j;
            // git writes loose objects at level 1.
            e.def = Some(1);
            let rec = e.encode_entry(ge);
            rec.write(&mut e.s[S_ENTRY]);
        } else if path.ends_with(".idx") && e.is_derived_idx(path, data, false) {
            kind = K_IDX;
            put_sv(&mut fields, e.packs[e.pack_of(path).unwrap()].file as i64 - i as i64);
        } else if path.ends_with(".rev") && e.is_derived_idx(path, data, true) {
            kind = K_REV;
            put_sv(&mut fields, e.packs[e.pack_of(path).unwrap()].file as i64 - i as i64);
        } else if let Some((j, crlf)) = e.blob_of(data) {
            // Working files, and blobs under .git (lost-found/other, ...).
            kind = if crlf { K_BLOB_CRLF } else { K_BLOB };
            put_sv(&mut fields, j as i64 - (prev_blob + 1));
            prev_blob = j as i64;
        } else if in_git_dir(path) {
            kind = K_GIT;
            let flags = gitfile::encode(data, &e.tab, &mut e.s);
            fields.push(flags);
        } else {
            kind = K_RAW;
            e.s[S_RAW].extend_from_slice(data);
        }
        e.st.kinds[kind as usize] += 1;
        e.st.kind_bytes[kind as usize] += data.len() as u64;
        head.push(kind);
        let common = prev_path.bytes().zip(path.bytes()).take_while(|(a, b)| a == b).count();
        let shown = path_dir.unwrap_or(path);
        let common = common.min(shown.len());
        put_uv(&mut head, common as u64);
        put_uv(&mut head, (shown.len() - common) as u64);
        head.extend_from_slice(&shown.as_bytes()[common..]);
        head.extend_from_slice(&fields);
        let mut mask = 0u64;
        let mut lens = vec![];
        for k in S_ENTRY..NSTREAMS {
            let l = e.len(k) - before[k];
            if l != 0 {
                mask |= 1 << (k - S_ENTRY);
                put_uv(&mut lens, l);
            }
        }
        put_uv(&mut head, mask);
        head.extend_from_slice(&lens);
        e.s[S_FILES].extend_from_slice(&head);
        prev_path = path;
        e.flush(0);
    }
    e.flush(0);
    let ids = std::mem::take(&mut e.tab.ids);
    e.sink.put_owned(S_SHA, ids);
    (hashes, e.st)
}

impl<'a> Enc<'a> {
    fn len(&self, k: usize) -> u64 {
        self.flushed[k] + self.s[k].len() as u64
    }

    /// Hands the staged streams to the sink once they hold more than `min` bytes.
    fn flush(&mut self, min: usize) {
        if self.s.iter().map(|s| s.len()).sum::<usize>() <= min {
            return;
        }
        for k in 0..NSTREAMS {
            if !self.s[k].is_empty() {
                self.sink.put(k, &self.s[k]);
                self.flushed[k] += self.s[k].len() as u64;
                self.s[k].clear();
                if self.s[k].capacity() > 4 * CHUNK {
                    self.s[k] = Vec::new();
                }
            }
        }
    }

    /// For each file, the first file with the same bytes, if another (that one is not SAME).
    /// Short files are not worth it.
    fn find_same(&self) -> Vec<Option<usize>> {
        let mut first: HashMap<u128, usize> = HashMap::new();
        let mut same = vec![None; self.files.len()];
        for (i, (_, d)) in self.files.iter().enumerate() {
            if d.len() < 32 {
                continue;
            }
            let h = xxh3_128(d);
            match first.get(&h) {
                Some(&k) if self.files[k].1 == *d => same[i] = Some(k),
                Some(_) => {}
                None => {
                    first.insert(h, i);
                }
            }
        }
        same
    }

    /// Validates packs (against their indexes) and loose objects, and builds the table:
    /// packs in file order with entries by offset, then loose objects. Returns the pack of
    /// each pack file, and the entry of each loose object file.
    fn find_objects(&mut self, same: &[Option<usize>]) -> (HashMap<usize, usize>, HashMap<usize, usize>) {
        let mut pack_of_file = HashMap::new();
        for (i, (path, data)) in self.files.iter().enumerate() {
            if same[i].is_some() || !in_git_dir(path) || !path.ends_with(".pack") {
                continue;
            }
            let idx_path = format!("{}.idx", &path[..path.len() - 5]);
            let Ok(xi) = self.files.binary_search_by(|f| f.0.as_str().cmp(&idx_path)) else { continue };
            let Some(idx) = IdxView::new(&self.files[xi].1) else { continue };
            let n = idx.n;
            if data.len() < 32
                || &data[..4] != b"PACK"
                || !matches!(u32::from_be_bytes(data[4..8].try_into().unwrap()), 2 | 3)
                || u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize != n
            {
                continue;
            }
            let Some(mut order) = (0..n).map(|k| idx.off(k).map(|o| (o, k as u32))).collect::<Option<Vec<_>>>() else {
                continue;
            };
            order.sort_unstable();
            let end = data.len() as u64 - 20;
            let ok = order.windows(2).all(|w| w[0].0 < w[1].0)
                && order.first().is_none_or(|x| x.0 == 12)
                && order.last().is_none_or(|x| x.0 < end)
                && (n > 0 || data.len() == 32);
            if !ok {
                continue;
            }
            let e0 = self.pack_ents;
            let mut offs = Vec::with_capacity(n + 1);
            let mut js = Vec::with_capacity(n);
            for (r, &(o, k)) in order.iter().enumerate() {
                let (j, new) = self.tab.add(idx.id(k as usize));
                if new {
                    self.loc.push((e0 + r) as u32);
                }
                offs.push(o);
                js.push(j);
            }
            drop(order);
            offs.push(end);
            self.pack_ents += n;
            pack_of_file.insert(i, self.packs.len());
            self.packs.push(Pack { file: i, e0, offs, js });
        }
        let mut loose_of_file = HashMap::new();
        for (i, (path, data)) in self.files.iter().enumerate() {
            if same[i].is_some() {
                continue;
            }
            let Some((_, id)) = loose_path(path) else { continue };
            let Some((payload, used)) = obj::inflate(data, 256) else { continue };
            if used != data.len() {
                continue;
            }
            let Some((_, size, hl)) = obj::parse_loose_header(&payload) else { continue };
            if size != (payload.len() - hl) as u64 {
                continue;
            }
            let ge = self.pack_ents + self.loose.len();
            let (j, new) = self.tab.add(&id);
            if new {
                self.loc.push(ge as u32);
            }
            loose_of_file.insert(i, ge);
            self.loose.push((i, j));
        }
        (pack_of_file, loose_of_file)
    }

    /// The pack holding global entry `e` (which must be a pack entry).
    fn pack_at(&self, e: usize) -> usize {
        self.packs.partition_point(|p| p.e0 <= e) - 1
    }

    fn ent(&self, e: usize) -> Ent {
        if e >= self.pack_ents {
            let (file, j) = self.loose[e - self.pack_ents];
            return Ent { file, start: 0, end: self.files[file].1.len(), loose: true, j };
        }
        let p = &self.packs[self.pack_at(e)];
        let k = e - p.e0;
        Ent { file: p.file, start: p.offs[k] as usize, end: p.offs[k + 1] as usize, loose: false, j: p.js[k] }
    }

    fn bytes(&self, e: usize) -> &'a [u8] {
        let t = self.ent(e);
        &self.files[t.file].1[t.start..t.end]
    }

    /// The entry an ofs-delta at entry `e` refers to, `off` bytes back.
    fn ofs_base(&self, e: usize, off: u64) -> Option<usize> {
        let p = &self.packs[self.pack_at(e)];
        let target = p.offs[e - p.e0].checked_sub(off)?;
        let k = p.offs[..p.offs.len() - 1].binary_search(&target).ok()?;
        Some(p.e0 + k)
    }

    /// An object's type, base (entry, or an id not located) and payload, from its bytes;
    /// `None` if they are unreadable.
    fn raw_obj(&self, e: usize) -> Option<RawObj> {
        let b = self.bytes(e);
        if e >= self.pack_ents {
            let (mut p, _) = obj::inflate(b, 256)?;
            let (t, _, hl) = obj::parse_loose_header(&p)?;
            p.drain(..hl);
            return Some((t, None, p));
        }
        let (t, size, mut p) = obj::entry_header_any(b)?;
        let base = match t {
            obj::OFS => {
                let (off, l) = obj::parse_ofs(&b[p..])?;
                p += l;
                Some(Ok(self.ofs_base(e, off)?))
            }
            obj::REF => {
                let id: Id = b.get(p..p + 20)?.try_into().unwrap();
                p += 20;
                Some(match self.tab.find(&id) {
                    Some(j) => Ok(self.loc[j as usize] as usize),
                    None => Err(id),
                })
            }
            1..=4 => None,
            _ => return None,
        };
        let (data, _) = obj::inflate(&b[p..], size as usize)?;
        Some((t, base, data))
    }

    /// The object at entry `e`, deltas applied.
    fn resolve(&mut self, e: usize) -> Option<Rc<Obj>> {
        let mut chain = vec![];
        let mut cur = e;
        let mut base = loop {
            if let Some(o) = self.cache.get(cur) {
                break o;
            }
            let (t, b, data) = self.raw_obj(cur)?;
            match b {
                None => {
                    let o = Rc::new(Obj { typ: t, data });
                    self.cache.put(cur, o.clone());
                    break o;
                }
                Some(Ok(k)) => {
                    chain.push((cur, data));
                    cur = k;
                    if chain.len() > 100_000 {
                        return None;
                    }
                }
                Some(Err(_)) => return None,
            }
        };
        for (ent, delta) in chain.into_iter().rev() {
            let data = obj::apply_delta(&base.data, &delta)?;
            let o = Rc::new(Obj { typ: base.typ, data });
            self.cache.put(ent, o.clone());
            base = o;
        }
        Some(base)
    }

    /// The type at the bottom of an entry's delta chain (its own, if not a delta).
    fn chain_type(&self, e: usize) -> Option<u8> {
        let mut cur = e;
        for _ in 0..100_000 {
            if cur >= self.pack_ents {
                return Some(obj::parse_loose_header(&obj::inflate_prefix(self.bytes(cur), 32))?.0);
            }
            let b = self.bytes(cur);
            let (t, _, p) = obj::entry_header_any(b)?;
            cur = match t {
                obj::OFS => self.ofs_base(cur, obj::parse_ofs(&b[p..])?.0)?,
                obj::REF => self.loc_of(b.get(p..p + 20)?)?,
                1..=4 => return Some(t),
                _ => return None,
            };
        }
        None
    }

    fn loc_of(&self, id: &[u8]) -> Option<usize> {
        self.tab.find(id).map(|j| self.loc[j as usize] as usize)
    }

    /// The size an entry claims (as the decoder derives it from records).
    fn claimed(&self, e: usize) -> u64 {
        if e >= self.pack_ents {
            let p = obj::inflate_prefix(self.bytes(e), 32);
            return obj::parse_loose_header(&p).map_or(0, |h| h.1);
        }
        claimed_raw(self.bytes(e))
    }

    /// Chooses the zlib streams to rebuild, by the cost model.
    fn choose(&mut self) {
        let n = self.pack_ents + self.loose.len();
        let mut input = budget::Input { files: self.files.len() as u64, objects: n as u64, ..Default::default() };
        for (path, d) in self.files {
            input.bytes += d.len() as u64;
            if !in_git_dir(path) {
                input.work_bytes += d.len() as u64;
            }
        }
        for p in &self.packs {
            input.pack_bytes += self.files[p.file].1.len() as u64;
        }
        for &(f, _) in &self.loose {
            input.loose_bytes += self.files[f].1.len() as u64;
        }
        self.st.input = input;
        let Some(ns) = self.opt.ns_per_byte else {
            self.emulate = vec![true; n];
            return;
        };
        // The order, from the headers: bytes of data, of the entry, and the class.
        let key = |e: usize| {
            let b = self.bytes(e);
            let (class, size) = if e >= self.pack_ents {
                let s = obj::parse_loose_header(&obj::inflate_prefix(b, 32)).map_or(0, |(_, s, hl)| s + hl as u64);
                (budget::Class::Loose, s)
            } else {
                match obj::entry_header_any(b) {
                    Some((t @ 1..=4, s, _)) => {
                        (if t == obj::BLOB { budget::Class::Blob } else { budget::Class::Text }, s)
                    }
                    Some((_, s, _)) => (budget::Class::Delta, s),
                    None => (budget::Class::Delta, 0),
                }
            };
            let z = if e >= self.pack_ents { Some(b) } else { self.zlib_of(e) };
            let flevel = z.and_then(|z| z.get(1)).map_or(2, |b| b >> 6);
            (
                self.opt.order.key(class, size, b.len() as u64, flevel),
                budget::rough(size, b.len() as u64, flevel) as f32,
            )
        };
        let mut order: Vec<(u64, u32, f32)> = (0..n).map(key).enumerate().map(|(e, (k, r))| (k, e as u32, r)).collect();
        order.sort_unstable_by_key(|a| (a.0, a.1));
        let mut c = budget::Chooser::new(&input, ns, self.opt.skip);
        self.emulate = vec![false; n];
        for (_, e, rough) in order {
            let e = e as usize;
            if self.opt.skip && !c.might_fit(rough as f64) {
                if c.full() {
                    break;
                }
                continue;
            }
            let z = if e >= self.pack_ents { Some(self.bytes(e)) } else { self.zlib_of(e) };
            let s = z.filter(|z| z.len() >= 6).and_then(|z| {
                let f = aqr_flate::inflate::parse(&z[2..z.len() - 4]).ok()?;
                Some(budget::Stream::new(&f.data, z, &f.tokens))
            });
            if c.take(s.as_ref()) {
                self.emulate[e] = true;
                self.st.emulated_est += budget::emulation(&s.unwrap());
            }
            if c.full() {
                break;
            }
        }
        self.st.model_ns = c.used[0].max(c.used[1]);
        self.st.budget_ns = c.budget;
    }

    /// The zlib stream of a pack entry.
    fn zlib_of(&self, e: usize) -> Option<&'a [u8]> {
        let b = self.bytes(e);
        let (t, _, mut p) = obj::entry_header_any(b)?;
        match t {
            obj::OFS => p += obj::parse_ofs(&b[p..])?.1,
            obj::REF => p += 20,
            _ => {}
        }
        b.get(p..)
    }

    /// The level zlib reproduces `z` (the stream of `data`) at, with the record's zlib mode;
    /// `None` if it does not.
    fn emulated(&mut self, z: &[u8], data: &[u8], rec: &mut Rec) -> Option<()> {
        let l = zl::find_level(z, data, self.last_level)?;
        self.last_level = l;
        if *self.def.get_or_insert(l) != l {
            rec.h |= (Z_LEVEL as u32) << H_ZSHIFT;
            rec.level = l;
        }
        self.st.zlib_emulated += 1;
        self.st.emulated_bytes += data.len() as u64;
        self.st.emulated_zbytes += z.len() as u64;
        Some(())
    }

    /// Codes entry `e` and returns its record, lengths filled in.
    fn encode_entry(&mut self, e: usize) -> Rec {
        let before: [u64; NSTREAMS] = std::array::from_fn(|i| self.len(i));
        let mut rec = self.encode_entry_inner(e);
        for i in 0..REC_N {
            rec.lens[i] = self.len(REC_FIRST + i) - before[REC_FIRST + i];
        }
        rec
    }

    fn encode_entry_inner(&mut self, e: usize) -> Rec {
        let ent = self.ent(e);
        let b = self.bytes(e);
        let mut rec = Rec::default();
        let j = ent.j;
        if !ent.loose {
            // Pack entries new to the table take the next index (the decoder counts them).
            if self.loc[j as usize] as usize == e {
                debug_assert_eq!(j, self.next_new);
                self.next_new += 1;
            } else {
                rec.h |= H_DUP;
                rec.dup = j as i64 - self.next_new as i64;
            }
        }
        let mut cur = j as i64;
        if ent.loose {
            let payload = obj::inflate(b, 256).unwrap().0;
            let (t, size, hl) = obj::parse_loose_header(&payload).unwrap();
            rec.h |= t as u32;
            rec.size = size;
            if self.emulate[e] && self.emulated(b, &payload, &mut rec).is_some() {
                self.content(Lit::of_type(t), &payload[hl..], &mut cur);
            } else {
                rec.h |= (Z_VERBATIM as u32) << H_ZSHIFT;
                self.s[S_RAW].extend_from_slice(b);
                self.st.zlib_verbatim += 1;
            }
            return rec;
        }
        let verbatim = |s: &mut Self, mut rec: Rec| {
            rec.h &= H_DUP;
            s.s[S_RAW].extend_from_slice(b);
            s.st.entry_verbatim += 1;
            rec.dist = 0;
            match obj::entry_header_any(b) {
                Some((obj::OFS, _, p)) => {
                    if let Some((off, _)) = obj::parse_ofs(&b[p..])
                        && let Some(base) = s.ofs_base(e, off)
                    {
                        rec.dist = (e - base) as u64 + 1;
                    }
                }
                // The base's index, so that the decoder needs no lookup by id.
                Some((obj::REF, _, p)) => {
                    if let Some(jb) = b.get(p..p + 20).and_then(|id| s.tab.find(id)) {
                        rec.dist = aqr_common::varint::zz(jb as i64 - j as i64) + 1;
                    }
                }
                _ => {}
            }
            rec
        };
        // Not chosen: kept whole (nothing to inflate).
        if !self.emulate[e] {
            self.st.zlib_verbatim += 1;
            return verbatim(self, rec);
        }
        let Some((t, size, mut p)) = obj::parse_entry_header(b) else { return verbatim(self, rec) };
        let mut base = None;
        let mut ref_base = None;
        match t {
            1..=4 => {}
            obj::OFS => {
                let Some((off, l)) = obj::parse_ofs(&b[p..]) else { return verbatim(self, rec) };
                let Some(be) = self.ofs_base(e, off) else { return verbatim(self, rec) };
                p += l;
                rec.dist = (e - be) as u64;
                base = Some(be);
            }
            obj::REF => {
                if b.len() < p + 20 {
                    return verbatim(self, rec);
                }
                let id = &b[p..p + 20];
                p += 20;
                let jb = self.tab.find(id);
                ref_base = Some((jb, id));
                base = jb.map(|jb| self.loc[jb as usize] as usize);
            }
            _ => return verbatim(self, rec),
        }
        let z = &b[p..];
        let Some((payload, used)) = obj::inflate(z, size as usize) else { return verbatim(self, rec) };
        if used != z.len() || payload.len() as u64 != size || self.emulated(z, &payload, &mut rec).is_none() {
            self.st.zlib_verbatim += (used == z.len()) as u64;
            return verbatim(self, rec);
        }
        rec.h |= t as u32;
        if let Some((jb, id)) = ref_base {
            put_ref(&mut self.s[S_REFS], jb, id, &mut cur);
        }
        if t <= 4 {
            rec.size = size;
            self.content(Lit::of_type(t), &payload, &mut cur);
            return rec;
        }
        rec.size = obj::delta_sizes(&payload).map_or(0, |s| s.1);
        self.encode_delta(&payload, base, &mut rec, &mut cur);
        rec
    }

    /// A non-delta object's content.
    fn content(&mut self, kind: Lit, data: &[u8], cur: &mut i64) {
        let (lit, refs) = two(&mut self.s, kind.stream(), S_REFS);
        enc_run(&mut State::new(kind), data, &self.tab, cur, &mut Sinks { lit, refs });
    }

    /// A delta: ops and typed literals if canonical, else verbatim.
    fn encode_delta(&mut self, d: &[u8], base: Option<usize>, rec: &mut Rec, cur: &mut i64) {
        // Parse fully first.
        let mut at = 0;
        let parsed = (|| {
            let bs = obj::parse_dsize(d, &mut at)?;
            let ts = obj::parse_dsize(d, &mut at)?;
            let mut ops = vec![];
            let mut produced = 0u64;
            while at < d.len() {
                let op = obj::parse_op(d, &mut at)?;
                match op {
                    Op::Insert(n) => {
                        ops.push((op, at));
                        at += n;
                        if at > d.len() {
                            return None;
                        }
                        produced += n as u64;
                    }
                    Op::Copy { size, .. } => {
                        ops.push((op, 0));
                        produced += size;
                    }
                }
            }
            (produced == ts).then_some((bs, ops))
        })();
        let Some((bs, ops)) = parsed else {
            rec.h |= H_PLAIN;
            self.s[S_RAW].extend_from_slice(d);
            self.st.delta_verbatim += 1;
            return;
        };
        let derived = base.map_or(0, |b| self.claimed(b));
        if base.is_none() || derived != bs {
            rec.h |= H_XBASE;
            rec.base_size = bs;
        }
        let ct = base.and_then(|b| self.chain_type(b));
        let mut kind = ct.map_or(Lit::Plain, Lit::of_type);
        let mut base_obj = None;
        if ct.is_none() {
            rec.h |= H_UNTYPED;
        } else if kind != Lit::Plain {
            let o = self.resolve(base.unwrap());
            let fits = o.as_ref().is_some_and(|o| {
                o.data.len() as u64 == bs
                    && ops.iter().all(|(op, _)| match *op {
                        Op::Copy { off, size } => off + size <= bs,
                        _ => true,
                    })
            });
            if fits {
                base_obj = o;
            } else {
                rec.h |= H_UNTYPED;
                kind = Lit::Plain;
            }
        }
        if rec.h & H_UNTYPED != 0 {
            self.st.delta_untyped += 1;
        }
        let mut st = State::new(kind);
        let mut prev_end = 0u64;
        for (op, p) in ops {
            match op {
                Op::Insert(n) => {
                    put_uv(&mut self.s[S_OPS], (n as u64) << 1);
                    let (lit, refs) = two(&mut self.s, kind.stream(), S_REFS);
                    enc_run(&mut st, &d[p..p + n], &self.tab, cur, &mut Sinks { lit, refs });
                }
                Op::Copy { off, size } => {
                    put_uv(&mut self.s[S_OPS], size << 1 | 1);
                    put_sv(&mut self.s[S_OPS], off as i64 - prev_end as i64);
                    prev_end = off + size;
                    if let Some(o) = &base_obj {
                        st.feed_all(&o.data[off as usize..(off + size) as usize]);
                    }
                }
            }
        }
    }

    fn encode_pack(&mut self, p: usize, fields: &mut Vec<u8>) {
        let (i, e0, n) = (self.packs[p].file, self.packs[p].e0, self.packs[p].js.len());
        let data = &self.files[i].1;
        let path = &self.files[i].0;
        let version = u32::from_be_bytes(data[4..8].try_into().unwrap());
        // The default level: the first emulated entry's.
        self.def = None;
        for e in e0..e0 + n {
            let rec = self.encode_entry(e);
            self.st.entry_types[rec.typ() as usize] += 1;
            rec.write(&mut self.s[S_ENTRY]);
            self.flush(CHUNK);
        }
        let trailer = &data[data.len() - 20..];
        let mode = if pack_name_id(path).is_some_and(|id| id[..] == *trailer) {
            1
        } else if obj::sha1(&[&data[..data.len() - 20]])[..] == *trailer {
            0
        } else {
            self.s[S_RAW].extend_from_slice(trailer);
            2
        };
        put_uv(fields, version as u64);
        put_uv(fields, n as u64);
        // The size, so that the decoder allocates it once.
        put_uv(fields, data.len() as u64);
        fields.push(self.def.unwrap_or(6));
        fields.push(mode);
    }

    /// The pack (index into `packs`) of an .idx or .rev path.
    fn pack_of(&self, path: &str) -> Option<usize> {
        let stem = path.rsplit_once('.')?.0;
        let p = format!("{stem}.pack");
        let k = self.files.binary_search_by(|f| f.0.as_str().cmp(&p)).ok()?;
        self.packs.binary_search_by_key(&k, |p| p.file).ok()
    }

    /// Whether `data` is the .idx (or .rev) git writes for its pack, checked in place: the
    /// ids (which came from the index) in order, the offsets and CRC-32s of the entries,
    /// large offsets in order, and the checksums.
    fn is_derived_idx(&self, path: &str, data: &[u8], rev: bool) -> bool {
        let Some(p) = self.pack_of(path) else { return false };
        let pk = &self.packs[p];
        let pack = &self.files[pk.file].1;
        let trailer = &pack[pack.len() - 20..];
        let idx_path = format!("{}.idx", path.rsplit_once('.').unwrap().0);
        let Ok(xi) = self.files.binary_search_by(|f| f.0.as_str().cmp(&idx_path)) else { return false };
        let Some(idx) = IdxView::new(&self.files[xi].1) else { return false };
        let n = pk.js.len();
        let offs = &pk.offs[..n];
        let rank = |k: usize| idx.off(k).and_then(|o| offs.binary_search(&o).ok());
        // The decoder sorts the ids.
        if !(1..n).all(|k| idx.id(k - 1) < idx.id(k)) {
            return false;
        }
        if rev {
            if data.len() != 12 + 4 * n + 40 || data[..12] != *b"RIDX\0\0\0\x01\0\0\0\x01" {
                return false;
            }
            let be = |r: usize| u32::from_be_bytes(data[12 + 4 * r..16 + 4 * r].try_into().unwrap()) as usize;
            return (0..n).all(|k| rank(k).is_some_and(|r| be(r) == k))
                && data[12 + 4 * n..32 + 4 * n] == *trailer
                && obj::sha1(&[&data[..32 + 4 * n]])[..] == data[32 + 4 * n..];
        }
        if data.len() < 8 + 1024 + 28 * n + 40 {
            return false;
        }
        let mut fan = [0u32; 256];
        for k in 0..n {
            fan[idx.id(k)[0] as usize] += 1;
        }
        let mut acc = 0;
        for (b, f) in fan.iter().enumerate() {
            acc += f;
            if idx.be(8 + 4 * b) != acc {
                return false;
            }
        }
        let crc_at = 8 + 1024 + 20 * n;
        let mut large = 0;
        for k in 0..n {
            let Some(r) = rank(k) else { return false };
            let e = pk.e0 + r;
            if idx.be(crc_at + 4 * k) != crc32fast::hash(self.bytes(e)) {
                return false;
            }
            let o = idx.be(crc_at + 4 * n + 4 * k);
            if o & 0x8000_0000 != 0 {
                if o & 0x7fff_ffff != large || offs[r] < 0x8000_0000 {
                    return false;
                }
                large += 1;
            }
        }
        let end = 8 + 1024 + 28 * n + 8 * large as usize;
        data.len() == end + 40
            && data[end..end + 20] == *trailer
            && obj::sha1(&[&data[..end + 20]])[..] == data[end + 20..]
    }

    /// A working file as a blob in the table (possibly with CRLF line ends): its index and
    /// whether CRLF.
    fn blob_of(&mut self, data: &[u8]) -> Option<(u32, bool)> {
        if data.is_empty() {
            return None;
        }
        let try_ = |s: &mut Self, d: &[u8]| -> Option<u32> {
            let j = s.tab.find(&obj::object_id(obj::BLOB, d))?;
            let o = s.resolve(s.loc[j as usize] as usize)?;
            (o.typ == obj::BLOB && o.data == d).then_some(j)
        };
        if let Some(j) = try_(self, data) {
            return Some((j, false));
        }
        if !data.windows(2).any(|w| w == b"\r\n") {
            return None;
        }
        let lf = crlf_to_lf(data);
        if lf_to_crlf(&lf) != data {
            return None;
        }
        try_(self, &lf).map(|j| (j, true))
    }
}

/// A version 2 pack index, read in place.
pub struct IdxView<'a> {
    b: &'a [u8],
    pub n: usize,
}

impl<'a> IdxView<'a> {
    pub fn new(b: &'a [u8]) -> Option<Self> {
        if b.len() < 8 + 1024 + 40 || b[..8] != [0xff, 0x74, 0x4f, 0x63, 0, 0, 0, 2] {
            return None;
        }
        let n = u32::from_be_bytes(b[8 + 1020..8 + 1024].try_into().unwrap()) as usize;
        (b.len() >= 8 + 1024 + n * 28 + 40).then_some(IdxView { b, n })
    }
    fn be(&self, p: usize) -> u32 {
        u32::from_be_bytes(self.b[p..p + 4].try_into().unwrap())
    }
    pub fn id(&self, k: usize) -> &'a Id {
        let p = 8 + 1024 + 20 * k;
        self.b[p..p + 20].try_into().unwrap()
    }
    pub fn off(&self, k: usize) -> Option<u64> {
        let o = self.be(8 + 1024 + 24 * self.n + 4 * k);
        if o & 0x8000_0000 == 0 {
            return Some(o as u64);
        }
        let p = 8 + 1024 + 28 * self.n + 8 * (o & 0x7fff_ffff) as usize;
        Some(u64::from_be_bytes(self.b.get(p..p + 8)?.try_into().unwrap()))
    }
}

pub fn crlf_to_lf(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut i = 0;
    while i < d.len() {
        if d[i] == b'\r' && d.get(i + 1) == Some(&b'\n') {
            i += 1;
        }
        out.push(d[i]);
        i += 1;
    }
    out
}

pub fn lf_to_crlf(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + d.len() / 16);
    for &c in d {
        if c == b'\n' {
            out.push(b'\r');
        }
        out.push(c);
    }
    out
}

/// Two distinct streams, mutably.
fn two(s: &mut Streams, a: usize, b: usize) -> (&mut Vec<u8>, &mut Vec<u8>) {
    if a < b {
        let (x, y) = s.split_at_mut(b);
        (&mut x[a], &mut y[0])
    } else {
        let (x, y) = s.split_at_mut(a);
        (&mut y[0], &mut x[b])
    }
}
