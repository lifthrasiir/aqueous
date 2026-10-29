//! What of an HDF5 file the encoder needs (the HDF5 File Format Specification 3.0, cited by its
//! sections): the datasets reached from the root group and the filtered chunks of each, where
//! they are and through which filters they went. The rest of the file is not interpreted.
//!
//! Groups are followed through symbol tables (v1 B-trees and symbol nodes, §III.A.1, §III.C),
//! link messages, and dense link storage, whose fractal heap's direct blocks are read as link
//! messages one after another (as a heap written once holds them). Chunks are found through
//! version 1 B-trees (layout messages 1 to 3, and 4), single chunk and fixed array indexes
//! (layout 4); the other indexes of layout 4 are not read.

use std::collections::HashSet;
use std::ops::Range;

/// The signature of a superblock.
pub const SIGNATURE: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

/// Filter ids (§IV.A.2.l).
pub const DEFLATE: u16 = 1;
pub const SHUFFLE: u16 = 2;
pub const FLETCHER32: u16 = 3;

/// An undefined address is all ones.
const UNDEF: u64 = u64::MAX;

/// Bounds on what is read, so that a corrupt file cannot take long.
const MAX_OBJECTS: usize = 1 << 20;
const MAX_DEPTH: u32 = 64;

/// A filter of a pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub id: u16,
    pub flags: u16,
    pub client: Vec<u32>,
}

/// A datatype's class and size (§IV.A.2.d), what the encoder needs of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dtype {
    pub class: u8,
    pub size: u32,
    /// Big endian (fixed-point and floating-point).
    pub be: bool,
    /// Signed (fixed-point).
    pub signed: bool,
}

pub const FIXED: u8 = 0;
pub const FLOAT: u8 = 1;

#[derive(Debug, Clone)]
pub struct Dataset {
    /// The address of its object header.
    pub header: u64,
    pub filters: Vec<Filter>,
    pub dtype: Option<Dtype>,
    /// The dimensions of a chunk, the last the element's bytes.
    pub chunk: Vec<u64>,
}

impl Dataset {
    /// Bytes of a chunk before filtering.
    pub fn chunk_bytes(&self) -> Option<u64> {
        self.chunk.iter().try_fold(1u64, |a, &d| a.checked_mul(d))
    }

    /// The filters a chunk went through, in order, by its mask (a bit set: skipped).
    pub fn active(&self, mask: u32) -> impl Iterator<Item = &Filter> {
        self.filters.iter().enumerate().filter(move |(i, _)| *i >= 32 || mask >> i & 1 == 0).map(|(_, f)| f)
    }
}

/// A filtered chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Its bytes in the data given to [`parse`].
    pub at: Range<usize>,
    pub mask: u32,
    pub dataset: usize,
}

#[derive(Debug, Clone, Default)]
pub struct File {
    /// Where the superblock is: the user block before it.
    pub start: usize,
    /// The end of the file, as its superblock tells (in the data given to [`parse`]).
    pub end: usize,
    pub datasets: Vec<Dataset>,
    /// The filtered chunks, by address, none overlapping.
    pub chunks: Vec<Chunk>,
}

/// Where a superblock may start: at 0 or at 512 times a power of two (§II.A, the user block).
pub fn superblock(d: &[u8]) -> Option<usize> {
    [0usize, 512, 1024, 2048, 4096].into_iter().find(|&p| d.get(p..p + 8) == Some(&SIGNATURE[..]))
}

/// Whether `d` starts with a superblock.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(SIGNATURE) && matches!(d.get(8), Some(0..=3))
}

/// The end of the HDF5 file at the start of `d` (its superblock's end-of-file address).
pub fn extent(d: &[u8]) -> Option<usize> {
    let s = Super::read(d, superblock(d)?)?;
    usize::try_from(s.base.checked_add(s.eof)?).ok()
}

/// The superblock's fields that matter.
struct Super {
    /// The base address in `d`.
    base: u64,
    /// The end-of-file address, relative to the base.
    eof: u64,
    o: usize,
    l: usize,
    root: u64,
}

impl Super {
    fn read(d: &[u8], at: usize) -> Option<Super> {
        let mut r = Reader { d, p: at + 8, o: 8, l: 8 };
        let version = r.u8()?;
        let (o, l);
        let root;
        let (base, eof);
        match version {
            0 | 1 => {
                r.skip(4)?;
                (o, l) = (r.u8()? as usize, r.u8()? as usize);
                r.skip(1 + 4 + 4)?;
                if version == 1 {
                    r.skip(4)?;
                }
                (r.o, r.l) = (o, l);
                check_sizes(o, l)?;
                base = r.addr()?;
                r.addr()?;
                eof = r.addr()?;
                r.addr()?;
                // the root group's symbol table entry
                r.addr()?;
                root = r.addr()?;
            }
            2 | 3 => {
                (o, l) = (r.u8()? as usize, r.u8()? as usize);
                check_sizes(o, l)?;
                (r.o, r.l) = (o, l);
                r.skip(1)?;
                base = r.addr()?;
                r.addr()?;
                eof = r.addr()?;
                root = r.addr()?;
            }
            _ => return None,
        }
        // The end-of-file address is written with the base added (as the library writes it). A
        // base of 0 with the superblock past a user block is the superblock's address (as the
        // library takes it).
        let eof = eof.checked_sub(base).filter(|_| eof != UNDEF)?;
        let base = if base == 0 { at as u64 } else { base };
        (base == at as u64).then_some(Super { base, eof, o, l, root })
    }
}

fn check_sizes(o: usize, l: usize) -> Option<()> {
    (matches!(o, 2 | 4 | 8) && matches!(l, 2 | 4 | 8)).then_some(())
}

/// A reader of little-endian fields, with the file's sizes of offsets and lengths.
#[derive(Clone)]
struct Reader<'a> {
    d: &'a [u8],
    p: usize,
    o: usize,
    l: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(b)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }

    fn uint(&mut self, n: usize) -> Option<u64> {
        let b = self.bytes(n)?;
        if n > 8 {
            return None;
        }
        Some(b.iter().rev().fold(0u64, |a, &x| a << 8 | x as u64))
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.uint(2).map(|v| v as u16)
    }

    fn u32(&mut self) -> Option<u32> {
        self.uint(4).map(|v| v as u32)
    }

    /// An address; all ones (undefined) as [`UNDEF`].
    fn addr(&mut self) -> Option<u64> {
        let v = self.uint(self.o)?;
        Some(if self.o < 8 && v == (1u64 << (8 * self.o)) - 1 { UNDEF } else { v })
    }

    fn len(&mut self) -> Option<u64> {
        self.uint(self.l)
    }

    fn sig(&mut self, s: &[u8; 4]) -> Option<()> {
        (self.bytes(4)? == s).then_some(())
    }
}

/// The parts of `d` that the encoder needs, or None where it does not start with an HDF5
/// file (a user block aside).
pub fn parse(d: &[u8]) -> Option<File> {
    let start = superblock(d)?;
    let s = Super::read(d, start)?;
    let end = usize::try_from(s.base.checked_add(s.eof)?).ok()?.min(d.len());
    let mut w = Walk {
        d: &d[..end],
        base: s.base as usize,
        o: s.o,
        l: s.l,
        seen: HashSet::new(),
        datasets: Vec::new(),
        chunks: Vec::new(),
        nodes: 0,
    };
    w.object(s.root, 0);
    let Walk { datasets, mut chunks, .. } = w;
    chunks.sort_by_key(|c| (c.at.start, c.at.end));
    chunks.dedup_by(|next, kept| next.at.start < kept.at.end);
    Some(File { start, end, datasets, chunks })
}

struct Walk<'a> {
    /// The file, to its end.
    d: &'a [u8],
    base: usize,
    o: usize,
    l: usize,
    seen: HashSet<u64>,
    datasets: Vec<Dataset>,
    chunks: Vec<Chunk>,
    /// Nodes read (B-tree nodes, heap blocks), bounded.
    nodes: usize,
}

/// A message of an object header: its type and data.
struct Message<'a> {
    t: u16,
    flags: u8,
    data: &'a [u8],
}

impl<'a> Walk<'a> {
    /// A reader at address `a`.
    fn at(&self, a: u64) -> Option<Reader<'a>> {
        let p = usize::try_from(a).ok()?.checked_add(self.base)?;
        (a != UNDEF && p < self.d.len()).then_some(Reader { d: self.d, p, o: self.o, l: self.l })
    }

    /// A reader of `data` alone.
    fn of(&self, data: &'a [u8]) -> Reader<'a> {
        Reader { d: data, p: 0, o: self.o, l: self.l }
    }

    fn node(&mut self) -> Option<()> {
        self.nodes += 1;
        (self.nodes <= MAX_OBJECTS).then_some(())
    }

    /// The object whose header is at `a`, and those it links to.
    fn object(&mut self, a: u64, depth: u32) {
        if depth > MAX_DEPTH || self.seen.len() >= MAX_OBJECTS || !self.seen.insert(a) {
            return;
        }
        let Some(msgs) = self.messages(a) else { return };
        let mut children = Vec::new();
        for m in &msgs {
            match m.t {
                // symbol table: a v1 B-tree of symbol nodes
                0x11 => {
                    let mut r = self.of(m.data);
                    if let Some(tree) = r.addr() {
                        let _ = self.group_tree(tree, &mut children, 0);
                    }
                }
                0x06 => children.extend(self.link(m.data).map(|l| l.0)),
                // link info: dense links in a fractal heap
                0x02 => {
                    let _ = self.dense_links(m.data, &mut children);
                }
                _ => {}
            }
        }
        if let Some((ds, index)) = self.dataset(a, &msgs) {
            self.datasets.push(ds);
            // (what is found before an index goes wrong is kept: each chunk is checked)
            let _ = self.chunks_of(self.datasets.len() - 1, index);
        }
        for c in children {
            self.object(c, depth + 1);
        }
    }

    /// The messages of the object header at `a` (§IV.A), continuations followed.
    fn messages(&mut self, a: u64) -> Option<Vec<Message<'a>>> {
        let mut r = self.at(a)?;
        let mut out = Vec::new();
        let mut blocks: Vec<(u64, u64)> = Vec::new();
        if self.d.get(r.p..r.p + 4) == Some(b"OHDR") {
            r.skip(4)?;
            if r.u8()? != 2 {
                return None;
            }
            let flags = r.u8()?;
            if flags & 0x20 != 0 {
                r.skip(16)?;
            }
            if flags & 0x10 != 0 {
                r.skip(4)?;
            }
            let size = r.uint(1 << (flags & 3))?;
            let end = r.p.checked_add(usize::try_from(size).ok()?)?;
            self.v2_messages(r, end, flags, &mut out, &mut blocks)?;
            let mut k = 0;
            while k < blocks.len() && k < 4096 {
                let (ba, bl) = blocks[k];
                k += 1;
                let Some(mut r) = self.at(ba) else { continue };
                if r.sig(b"OCHK").is_none() {
                    continue;
                }
                let Some(end) = usize::try_from(bl).ok().and_then(|n| (r.p - 4).checked_add(n)?.checked_sub(4)) else {
                    continue;
                };
                let _ = self.v2_messages(r, end, flags, &mut out, &mut blocks);
            }
        } else {
            if r.u8()? != 1 {
                return None;
            }
            r.skip(1)?;
            let n = r.u16()? as usize;
            r.skip(4)?;
            let size = r.u32()? as u64;
            r.skip(4)?;
            blocks.push((a + 16, size));
            let mut k = 0;
            while k < blocks.len() && out.len() < n && k < 4096 {
                let (ba, bl) = blocks[k];
                k += 1;
                let Some(mut r) = self.at(ba) else { continue };
                let Some(end) = usize::try_from(bl).ok().and_then(|n| r.p.checked_add(n)) else { continue };
                while r.p + 8 <= end && out.len() < n {
                    let t = r.u16()?;
                    let size = r.u16()? as usize;
                    let flags = r.u8()?;
                    r.skip(3)?;
                    let Some(data) = r.bytes(size) else { break };
                    if t == 0x10 {
                        let mut c = self.of(data);
                        if let (Some(ca), Some(cl)) = (c.addr(), c.len()) {
                            blocks.push((ca, cl));
                        }
                    }
                    out.push(Message { t, flags, data });
                }
            }
        }
        Some(out)
    }

    fn v2_messages(
        &self,
        mut r: Reader<'a>,
        end: usize,
        flags: u8,
        out: &mut Vec<Message<'a>>,
        blocks: &mut Vec<(u64, u64)>,
    ) -> Option<()> {
        let head = if flags & 0x04 != 0 { 6 } else { 4 };
        while r.p + head <= end {
            let t = r.u8()? as u16;
            let size = r.u16()? as usize;
            let mflags = r.u8()?;
            if flags & 0x04 != 0 {
                r.skip(2)?;
            }
            if r.p + size > end {
                break;
            }
            let data = r.bytes(size)?;
            if t == 0x10 {
                let mut c = self.of(data);
                if let (Some(ca), Some(cl)) = (c.addr(), c.len()) {
                    blocks.push((ca, cl));
                }
            }
            out.push(Message { t, flags: mflags, data });
        }
        Some(())
    }

    /// The objects of the group whose v1 B-tree (of type 0) is at `a`.
    fn group_tree(&mut self, a: u64, out: &mut Vec<u64>, depth: u32) -> Option<()> {
        self.node()?;
        let mut r = self.at(a)?;
        r.sig(b"TREE")?;
        let (t, level, n) = (r.u8()?, r.u8()?, r.u16()? as usize);
        if t != 0 || depth > MAX_DEPTH {
            return None;
        }
        r.skip(2 * self.o)?;
        let mut children = Vec::with_capacity(n);
        for _ in 0..n {
            r.len()?;
            children.push(r.addr()?);
        }
        for c in children {
            if level > 0 {
                let _ = self.group_tree(c, out, depth + 1);
            } else {
                let _ = self.symbol_node(c, out);
            }
        }
        Some(())
    }

    /// The objects of a symbol table node (§III.C).
    fn symbol_node(&mut self, a: u64, out: &mut Vec<u64>) -> Option<()> {
        self.node()?;
        let mut r = self.at(a)?;
        r.sig(b"SNOD")?;
        r.skip(2)?;
        let n = r.u16()?;
        for _ in 0..n {
            r.addr()?;
            out.push(r.addr()?);
            r.skip(24)?;
        }
        Some(())
    }

    /// A link message's target (a hard link's) and its bytes (§IV.A.2.g).
    fn link(&self, data: &'a [u8]) -> Option<(u64, usize)> {
        let mut r = self.of(data);
        if r.u8()? != 1 {
            return None;
        }
        let flags = r.u8()?;
        let t = if flags & 0x08 != 0 { r.u8()? } else { 0 };
        if flags & 0x04 != 0 {
            r.skip(8)?;
        }
        if flags & 0x10 != 0 {
            r.skip(1)?;
        }
        let n = usize::try_from(r.uint(1 << (flags & 3))?).ok()?;
        r.skip(n)?;
        if t == 0 {
            let a = r.addr()?;
            Some((a, r.p))
        } else {
            let n = r.u16()? as usize;
            r.skip(n)?;
            // (not a hard link: no target)
            Some((UNDEF, r.p))
        }
    }

    /// The hard links of a group's dense storage, from its link info message (§IV.A.2.c).
    fn dense_links(&mut self, data: &[u8], out: &mut Vec<u64>) -> Option<()> {
        let mut r = self.of(data);
        r.skip(1)?;
        if r.u8()? & 1 != 0 {
            r.skip(8)?;
        }
        let heap = r.addr()?;
        let h = self.heap(heap)?;
        let mut blocks = Vec::new();
        self.heap_blocks(&h, h.root, h.root_rows, &mut blocks, 0)?;
        for (a, size) in blocks {
            let _ = self.heap_links(&h, a, size, out);
        }
        Some(())
    }

    /// A fractal heap's header (§III.G).
    fn heap(&mut self, a: u64) -> Option<Heap> {
        let mut r = self.at(a)?;
        r.sig(b"FRHP")?;
        r.skip(1 + 2)?;
        let filters = r.u16()?;
        let flags = r.u8()?;
        r.skip(4)?;
        r.len()?;
        r.addr()?;
        r.len()?;
        r.addr()?;
        for _ in 0..8 {
            r.len()?;
        }
        let width = r.u16()? as u64;
        let start = r.len()?;
        let max_direct = r.len()?;
        let max_bits = r.u16()? as u32;
        r.skip(2)?;
        let root = r.addr()?;
        let root_rows = r.u16()? as u32;
        let ok = filters == 0
            && width > 0
            && start.is_power_of_two()
            && max_direct.is_power_of_two()
            && max_direct >= start
            && (1..=64).contains(&max_bits);
        ok.then_some(Heap {
            width,
            start,
            max_direct,
            offset_bytes: max_bits.div_ceil(8) as usize,
            flags,
            root,
            root_rows,
        })
    }

    /// The direct blocks (address and size) under a heap's block at `a` of `rows` rows (a
    /// direct block of the starting size where none).
    fn heap_blocks(&mut self, h: &Heap, a: u64, rows: u32, out: &mut Vec<(u64, u64)>, depth: u32) -> Option<()> {
        if a == UNDEF {
            return Some(());
        }
        if rows == 0 {
            out.push((a, h.start));
            return Some(());
        }
        self.node()?;
        if depth > 8 || rows > 64 {
            return None;
        }
        let mut r = self.at(a)?;
        r.sig(b"FHIB")?;
        r.skip(1)?;
        r.addr()?;
        r.skip(h.offset_bytes)?;
        let direct_rows = h.max_direct.trailing_zeros() - h.start.trailing_zeros() + 2;
        let mut children = Vec::new();
        for row in 0..rows {
            let size = if row == 0 { h.start } else { h.start.checked_shl(row - 1)? };
            for _ in 0..h.width {
                let c = r.addr()?;
                if c == UNDEF {
                    continue;
                }
                if row < direct_rows {
                    out.push((c, size));
                } else {
                    // an indirect block of `size × width` bytes
                    let sub = size.checked_mul(h.width)?;
                    let sub_rows = sub.trailing_zeros().checked_sub((h.start * h.width).trailing_zeros())? + 1;
                    children.push((c, sub_rows));
                }
            }
        }
        for (c, rows) in children {
            self.heap_blocks(h, c, rows, out, depth + 1)?;
        }
        Some(())
    }

    /// The hard links in a heap's direct block at `a` of `size` bytes, its objects read as link
    /// messages one after another up to the first that is not one.
    fn heap_links(&mut self, h: &Heap, a: u64, size: u64, out: &mut Vec<u64>) -> Option<()> {
        self.node()?;
        let mut r = self.at(a)?;
        let start = r.p;
        r.sig(b"FHDB")?;
        r.skip(1)?;
        r.addr()?;
        r.skip(h.offset_bytes)?;
        if h.flags & 2 != 0 {
            r.skip(4)?;
        }
        let end = start.checked_add(usize::try_from(size).ok()?)?.min(self.d.len());
        let mut p = r.p;
        while p < end {
            let Some((t, n)) = self.link(&self.d[p..end]) else { break };
            if t != UNDEF {
                out.push(t);
            }
            p += n;
        }
        Some(())
    }

    /// A dataset's description and the index of its chunks, if its header is one of a dataset
    /// of filtered chunks.
    fn dataset(&self, a: u64, msgs: &[Message<'a>]) -> Option<(Dataset, Index)> {
        let layout = msgs.iter().find(|m| m.t == 0x08)?;
        let filters = msgs.iter().find(|m| m.t == 0x0b && m.flags & 2 == 0)?;
        let filters = self.filters(filters.data)?;
        let dtype = msgs.iter().find(|m| m.t == 0x03 && m.flags & 2 == 0).and_then(|m| dtype(m.data));
        let (chunk, index) = self.layout(layout.data)?;
        Some((Dataset { header: a, filters, dtype, chunk }, index))
    }

    /// A filter pipeline message (§IV.A.2.l).
    fn filters(&self, data: &[u8]) -> Option<Vec<Filter>> {
        let mut r = self.of(data);
        let version = r.u8()?;
        let n = r.u8()?;
        if version == 1 {
            r.skip(6)?;
        } else if version != 2 {
            return None;
        }
        let mut out = Vec::new();
        for _ in 0..n {
            let id = r.u16()?;
            let name = if version == 1 || id >= 256 { r.u16()? as usize } else { 0 };
            let flags = r.u16()?;
            let k = r.u16()? as usize;
            r.skip(if version == 1 { name.next_multiple_of(8) } else { name })?;
            let client = (0..k).map(|_| r.u32()).collect::<Option<Vec<_>>>()?;
            if version == 1 && k % 2 == 1 {
                r.skip(4)?;
            }
            out.push(Filter { id, flags, client });
        }
        Some(out)
    }

    /// The chunk dimensions and the index of a layout message of chunked storage (§IV.A.2.i).
    fn layout(&self, data: &[u8]) -> Option<(Vec<u64>, Index)> {
        let mut r = self.of(data);
        match r.u8()? {
            1 | 2 => {
                let n = r.u8()? as usize;
                if r.u8()? != 2 {
                    return None;
                }
                r.skip(5)?;
                let a = r.addr()?;
                let dims = (0..n).map(|_| r.u32().map(u64::from)).collect::<Option<Vec<_>>>()?;
                Some((dims, Index::Btree(a)))
            }
            3 => {
                if r.u8()? != 2 {
                    return None;
                }
                let n = r.u8()? as usize;
                let a = r.addr()?;
                let dims = (0..n).map(|_| r.u32().map(u64::from)).collect::<Option<Vec<_>>>()?;
                Some((dims, Index::Btree(a)))
            }
            4 | 5 => {
                if r.u8()? != 2 {
                    return None;
                }
                let flags = r.u8()?;
                let n = r.u8()? as usize;
                let w = r.u8()? as usize;
                let dims = (0..n).map(|_| r.uint(w)).collect::<Option<Vec<_>>>()?;
                let index = match r.u8()? {
                    1 => {
                        let filtered = if flags & 2 != 0 { Some((r.len()?, r.u32()?)) } else { None };
                        Index::Single(r.addr()?, filtered?)
                    }
                    3 => {
                        r.skip(1)?;
                        Index::Fixed(r.addr()?)
                    }
                    _ => return None,
                };
                Some((dims, index))
            }
            _ => None,
        }
    }

    /// The chunks of dataset `k` through `index`.
    fn chunks_of(&mut self, k: usize, index: Index) -> Option<()> {
        let rank = self.datasets[k].chunk.len();
        match index {
            Index::Btree(a) => self.chunk_tree(k, a, rank, 0),
            Index::Single(a, (size, mask)) => self.chunk(k, a, size, mask),
            Index::Fixed(a) => self.fixed_array(k, a),
        }
    }

    /// The chunk of dataset `k` at `a` of `size` bytes, if it is within the file.
    fn chunk(&mut self, k: usize, a: u64, size: u64, mask: u32) -> Option<()> {
        let start = usize::try_from(a).ok()?.checked_add(self.base)?;
        let end = start.checked_add(usize::try_from(size).ok()?)?;
        if a == UNDEF || size == 0 || end > self.d.len() {
            return None;
        }
        self.chunks.push(Chunk { at: start..end, mask, dataset: k });
        Some(())
    }

    /// The chunks under the v1 B-tree node (of type 1) at `a`, of keys of `dims` offsets.
    fn chunk_tree(&mut self, k: usize, a: u64, dims: usize, depth: u32) -> Option<()> {
        self.node()?;
        let mut r = self.at(a)?;
        r.sig(b"TREE")?;
        let (t, level, n) = (r.u8()?, r.u8()?, r.u16()? as usize);
        if t != 1 || depth > MAX_DEPTH {
            return None;
        }
        r.skip(2 * self.o)?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let size = r.u32()?;
            let mask = r.u32()?;
            r.skip(8 * dims)?;
            entries.push((size, mask, r.addr()?));
        }
        for (size, mask, c) in entries {
            if level > 0 {
                let _ = self.chunk_tree(k, c, dims, depth + 1);
            } else {
                let _ = self.chunk(k, c, size as u64, mask);
            }
        }
        Some(())
    }

    /// The chunks of a fixed array index of filtered chunks (§VII.B).
    fn fixed_array(&mut self, k: usize, a: u64) -> Option<()> {
        self.node()?;
        let mut r = self.at(a)?;
        r.sig(b"FAHD")?;
        r.skip(1)?;
        let client = r.u8()?;
        let entry = r.u8()? as usize;
        let page_bits = r.u8()? as u32;
        let n = r.len()?;
        let block = r.addr()?;
        let size_bytes = entry.checked_sub(self.o + 4)?;
        if client != 1 || page_bits >= 32 || size_bytes == 0 || size_bytes > 8 {
            return None;
        }
        let mut r = self.at(block)?;
        r.sig(b"FADB")?;
        r.skip(2)?;
        r.addr()?;
        let page = 1u64 << page_bits;
        let paged = n > page;
        let pages = n.div_ceil(page);
        if paged {
            r.skip(usize::try_from(pages.div_ceil(8)).ok()? + 4)?;
        }
        for i in 0..n {
            if paged && i > 0 && i % page == 0 {
                r.skip(4)?;
            }
            let c = r.addr()?;
            let size = r.uint(size_bytes)?;
            let mask = r.u32()?;
            if c != UNDEF {
                let _ = self.chunk(k, c, size, mask);
            }
        }
        Some(())
    }
}

/// Where a dataset's chunks are told.
#[derive(Debug, Clone, Copy)]
enum Index {
    /// A v1 B-tree of type 1.
    Btree(u64),
    /// A single chunk: its address, filtered size and filter mask.
    Single(u64, (u64, u32)),
    /// A fixed array.
    Fixed(u64),
}

struct Heap {
    width: u64,
    start: u64,
    max_direct: u64,
    /// Bytes of a block's offset in the heap.
    offset_bytes: usize,
    flags: u8,
    root: u64,
    root_rows: u32,
}

/// A datatype message's class and size (§IV.A.2.d).
fn dtype(data: &[u8]) -> Option<Dtype> {
    let &[cv, b0, ..] = data else { return None };
    let size = u32::from_le_bytes(*data.get(4..)?.first_chunk()?);
    let class = cv & 15;
    Some(Dtype {
        class,
        size,
        be: matches!(class, FIXED | FLOAT) && b0 & 1 != 0,
        signed: class == FIXED && b0 & 8 != 0,
    })
}
