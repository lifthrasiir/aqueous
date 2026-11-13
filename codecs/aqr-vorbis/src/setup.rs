//! The setup header (Vorbis I §4.2.4): codebooks (§3), floors of type 1 (§7.2), residues (§8),
//! mappings and modes, as far as parsing audio packets needs them.
//!
//! The memory a setup takes is exactly what [`Setup::parse`] reserves, in the order it parses
//! (the module's plan, `plan.rs`, follows it): the lists are reserved at their lengths, every
//! codebook's vectors at theirs (its decoding tree last, as it grows), and everything else is
//! fixed in size, so that a setup holds no more vectors than its codebooks'.

use crate::bits::{Reader, ilog};
use crate::{Error, Result};

fn bad(what: &'static str) -> Error {
    Error(what)
}

fn get(r: &mut Reader, n: u32) -> Result<u32> {
    r.bits(n).ok_or(bad("setup header ends early"))
}

/// What a stream's identification header tells that the setup and the packets depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ident {
    pub channels: usize,
    /// The short and long block sizes, in samples.
    pub blocksizes: [usize; 2],
    /// The audio packets leave out their packet type and window flags (Wwise's).
    pub stripped: bool,
}

/// A codebook. (Its vectors are declared in the reverse of the order they are made in, so that
/// they are freed top first.)
#[derive(Debug, Clone, PartialEq)]
pub struct Codebook {
    /// The decoding tree: per node, its two children ([`LEAF`] | entry, a node, or [`NONE`]).
    tree: Vec<[u32; 2]>,
    /// Decoding by the next [`FAST`] bits (as packed): an entry and its length (`entry | len <<
    /// 24`), the node reached past them ([`LEAF`] clear, `NODE`), or nothing ([`NONE`] as `u32::MAX`).
    fast: Vec<u32>,
    /// Each entry's codeword in the order it is packed (its first bit lowest).
    packed: Vec<u32>,
    pub lookup: Option<Lookup>,
    /// Each entry's codeword length, 0 where the entry is unused.
    pub lengths: Vec<u8>,
    pub dims: usize,
}

/// A setup's codebooks, freed the last first (as they are made, each above the one before).
#[derive(Debug, Clone, PartialEq)]
pub struct Codebooks(Vec<Codebook>);

impl std::ops::Deref for Codebooks {
    type Target = [Codebook];

    fn deref(&self) -> &[Codebook] {
        &self.0
    }
}

impl<'a> IntoIterator for &'a Codebooks {
    type Item = &'a Codebook;
    type IntoIter = std::slice::Iter<'a, Codebook>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Drop for Codebooks {
    fn drop(&mut self) {
        while self.0.pop().is_some() {}
    }
}

/// The bits [`Codebook::read`] looks up at once.
const FAST: u32 = 10;
const FAST_NODE: u32 = 1 << 30;

/// What a bit of a codeword leads to in a codebook's decoding tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    None,
    Node(usize),
    Entry(u32),
}

const LEAF: u32 = 1 << 31;
const NONE: u32 = 0;
/// Set in a node's second child while the tree is made: the node is full.
const FULL: u32 = 1 << 30;

/// A codebook's vector lookup table (§3.2.1, lookup types 1 and 2); the multiplicands are
/// skipped (nothing here reads them).
#[derive(Debug, Clone, PartialEq)]
pub struct Lookup {
    pub kind: u8,
    /// The packed float32 minimum and delta, as stored.
    pub min: u32,
    pub delta: u32,
    pub value_bits: u8,
    pub sequence: bool,
    /// The multiplicands.
    pub count: u32,
}

/// lookup1_values (§9.2.3): the greatest r with r^dims ≤ entries.
pub fn lookup1_values(entries: usize, dims: usize) -> usize {
    let mut r = (entries as f64).powf(1.0 / dims as f64).floor() as usize;
    let pow = |r: usize| (0..dims).try_fold(1usize, |p, _| p.checked_mul(r));
    while pow(r + 1).is_some_and(|p| p <= entries) {
        r += 1;
    }
    while r > 0 && pow(r).is_none_or(|p| p > entries) {
        r -= 1;
    }
    r
}

impl Codebook {
    fn parse(r: &mut Reader) -> Result<Codebook> {
        if get(r, 24)? != 0x564342 {
            return Err(bad("codebook sync"));
        }
        let (dims, entries) = (get(r, 16)? as usize, get(r, 24)? as usize);
        let mut lengths = vec![0u8; entries];
        if get(r, 1)? == 1 {
            let (mut at, mut len) = (0, get(r, 5)? + 1);
            while at < entries {
                let n = get(r, ilog((entries - at) as u32))? as usize;
                if n > entries - at || len > 32 {
                    return Err(bad("codebook lengths"));
                }
                lengths[at..at + n].fill(len as u8);
                at += n;
                len += 1;
            }
        } else {
            let sparse = get(r, 1)? == 1;
            for l in &mut lengths {
                if !sparse || get(r, 1)? == 1 {
                    *l = get(r, 5)? as u8 + 1;
                }
            }
        }
        let lookup = match get(r, 4)? {
            0 => None,
            kind @ (1 | 2) => {
                let (min, delta) = (get(r, 32)?, get(r, 32)?);
                let value_bits = get(r, 4)? as u8 + 1;
                let sequence = get(r, 1)? == 1;
                let n = if kind == 1 { lookup1_values(entries, dims) } else { entries.saturating_mul(dims) };
                let bits = n.saturating_mul(value_bits as usize);
                if bits > r.left() {
                    return Err(bad("setup header ends early"));
                }
                r.skip(bits);
                Some(Lookup { kind: kind as u8, min, delta, value_bits, sequence, count: n as u32 })
            }
            _ => return Err(bad("codebook lookup type")),
        };
        if dims == 0 && lookup.is_some() {
            return Err(bad("codebook dimensions"));
        }
        let mut packed = vec![0u32; entries];
        let mut fast = vec![u32::MAX; 1 << FAST];
        let mut tree = Vec::with_capacity(4);
        assign(&lengths, &mut packed, &mut tree)?;
        tree.shrink_to_fit();
        fast_table(&tree, &mut fast);
        Ok(Codebook { tree, fast, packed, lookup, lengths, dims })
    }

    pub fn entries(&self) -> usize {
        self.lengths.len()
    }

    /// Reads an entry's codeword; None at the end of the packet (`r` then where it was) or on a
    /// path no entry has.
    #[inline]
    pub fn read(&self, r: &mut Reader) -> Option<u32> {
        let t = self.fast[(r.peek() & ((1 << FAST) - 1)) as usize];
        if t == u32::MAX {
            return None;
        }
        if t & FAST_NODE == 0 {
            let len = (t >> 24) as usize;
            if len > r.left() {
                return None;
            }
            r.skip(len);
            return Some(t & 0xFF_FFFF);
        }
        if (FAST as usize) > r.left() {
            return None;
        }
        let start = r.pos();
        r.skip(FAST as usize);
        let mut node = (t & !FAST_NODE) as usize;
        loop {
            let Some(b) = r.bit() else {
                r.seek(start);
                return None;
            };
            match self.tree.get(node)?[b as usize] {
                NONE => {
                    r.seek(start);
                    return None;
                }
                c if c & LEAF != 0 => return Some(c & !LEAF),
                c => node = c as usize,
            }
        }
    }

    /// Writes entry `e`'s codeword.
    #[inline(always)]
    pub fn write(&self, e: u32, w: &mut crate::bits::Writer) {
        w.bits(self.packed[e as usize], self.lengths[e as usize] as u32);
    }

    /// Entry `e`'s codeword (its first bit the most significant of its length's bits, as §3.2.1
    /// assigns them), 0 for an entry without one.
    pub fn codeword(&self, e: u32) -> u32 {
        match self.lengths[e as usize] {
            0 => 0,
            l => self.packed[e as usize].reverse_bits() >> (32 - l as u32),
        }
    }

    /// The decoding tree's nodes (the root is node 0).
    pub fn nodes(&self) -> usize {
        self.tree.len()
    }

    /// Where bit `b` leads from node `node`: another node, an entry, or nowhere.
    pub fn child(&self, node: usize, b: u32) -> Child {
        match self.tree[node][b as usize] {
            NONE => Child::None,
            c if c & LEAF != 0 => Child::Entry(c & !LEAF),
            c => Child::Node(c as usize),
        }
    }

    /// Whether entry `e` has a codeword.
    pub fn used(&self, e: u32) -> bool {
        self.lengths.get(e as usize).is_some_and(|&l| l > 0)
    }
}

/// [`Codebook::fast`] from the decoding tree.
fn fast_table(tree: &[[u32; 2]], t: &mut [u32]) {
    for (idx, slot) in t.iter_mut().enumerate() {
        let mut node = 0usize;
        for depth in 0..FAST {
            match tree[node][(idx >> depth) & 1] {
                NONE => break,
                c if c & LEAF != 0 => {
                    *slot = (c & !LEAF) | (depth + 1) << 24;
                    break;
                }
                c => {
                    node = c as usize;
                    if depth + 1 == FAST {
                        *slot = FAST_NODE | node as u32;
                    }
                }
            }
        }
    }
}

/// The codewords of §3.2.1 (into `packed`, in the order they are packed): in entry order, each
/// used entry takes the lowest codeword of its length that neither is prefixed by one taken nor
/// prefixes one; and the tree that decodes them (`tree` the only vector that grows here, so
/// that it stays the last block). A single used entry takes the codeword 0 whatever its length;
/// a tree with unused paths is allowed (those paths decode to nothing).
fn assign(lengths: &[u8], packed: &mut [u32], tree: &mut Vec<[u32; 2]>) -> Result<()> {
    // nodes: children, and in the second child's FULL bit whether the node is full (a leaf, or
    // both children full)
    tree.push([NONE; 2]);
    let child = |tree: &[[u32; 2]], node: usize, b: usize| tree[node][b] & !FULL;
    let full = |tree: &[[u32; 2]], node: usize| tree[node][1] & FULL != 0;
    // the leftmost free node at depth `len` below `node` (at `depth`): its path, or None
    fn find(tree: &[[u32; 2]], node: usize, depth: u32, len: u32) -> Option<u32> {
        for b in 0..2u32 {
            let c = tree[node][b as usize] & !FULL;
            if depth + 1 == len {
                if c == NONE {
                    return Some(b);
                }
            } else if c == NONE {
                return Some(b << (len - depth - 1));
            } else if c & LEAF == 0
                && tree[c as usize][1] & FULL == 0
                && let Some(p) = find(tree, c as usize, depth + 1, len)
            {
                return Some(b << (len - depth - 1) | p);
            }
        }
        None
    }
    let used = lengths.iter().filter(|&&l| l > 0).count();
    for (e, &len) in lengths.iter().enumerate() {
        if len == 0 {
            continue;
        }
        let len = len as u32;
        let code = if used == 1 { 0 } else { find(tree, 0, 0, len).ok_or(bad("codebook overspecified"))? };
        packed[e] = code.reverse_bits() >> (32 - len);
        let (mut node, mut path, mut depth) = (0usize, [0usize; 32], 0);
        for i in (0..len).rev() {
            let b = (code >> i & 1) as usize;
            path[depth] = node;
            depth += 1;
            if i == 0 {
                tree[node][b] = LEAF | e as u32 | tree[node][b] & FULL;
            } else {
                if child(tree, node, b) == NONE {
                    tree.push([NONE; 2]);
                    let at = (tree.len() - 1) as u32;
                    tree[node][b] = at | tree[node][b] & FULL;
                }
                node = child(tree, node, b) as usize;
            }
        }
        // a node is full once both its children are leaves or full
        for &n in path[..depth].iter().rev() {
            let f = |c: u32| c != NONE && (c & LEAF != 0 || full(tree, c as usize));
            if f(child(tree, n, 0)) && f(child(tree, n, 1)) {
                tree[n][1] |= FULL;
            } else {
                break;
            }
        }
    }
    tree.iter_mut().for_each(|n| n[1] &= !FULL);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloorClass {
    pub dims: u8,
    pub subclasses: u8,
    pub masterbook: u8,
    /// A book per subclass (the first `1 << subclasses`), or None.
    pub subbooks: [Option<u8>; 8],
}

const NO_CLASS: FloorClass = FloorClass { dims: 0, subclasses: 0, masterbook: 0, subbooks: [None; 8] };

/// A floor of type 1 (§7.2.2), in fixed room: 31 partitions, 16 classes, 65 values at most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    partitions: [u8; 31],
    classes: [FloorClass; 16],
    xs: [u32; 65],
    n_partitions: u8,
    n_classes: u8,
    n_xs: u8,
    pub multiplier: u32,
    pub rangebits: u32,
}

impl Floor {
    fn parse(r: &mut Reader, books: &[Codebook]) -> Result<Floor> {
        match get(r, 16)? {
            1 => {}
            0 => return Err(bad("floor type 0 is not handled")),
            _ => return Err(bad("floor type")),
        }
        let mut f = Floor {
            partitions: [0; 31],
            classes: [NO_CLASS; 16],
            xs: [0; 65],
            n_partitions: get(r, 5)? as u8,
            n_classes: 0,
            n_xs: 0,
            multiplier: 0,
            rangebits: 0,
        };
        for p in &mut f.partitions[..f.n_partitions as usize] {
            *p = get(r, 4)? as u8;
        }
        f.n_classes = f.partitions().iter().max().map_or(0, |&m| m + 1);
        for c in &mut f.classes[..f.n_classes as usize] {
            let dims = get(r, 3)? as u8 + 1;
            let subclasses = get(r, 2)? as u8;
            let masterbook = if subclasses > 0 { get(r, 8)? as u8 } else { 0 };
            let mut subbooks = [None; 8];
            for b in &mut subbooks[..1 << subclasses] {
                *b = get(r, 8)?.checked_sub(1).map(|b| b as u8);
            }
            let book_ok = |b: u8| (b as usize) < books.len();
            if subclasses > 0 && !book_ok(masterbook) || subbooks.iter().flatten().any(|&b| !book_ok(b)) {
                return Err(bad("floor book"));
            }
            *c = FloorClass { dims, subclasses, masterbook, subbooks };
        }
        f.multiplier = get(r, 2)? + 1;
        f.rangebits = get(r, 4)?;
        let values = 2 + f.partitions().iter().map(|&c| f.classes[c as usize].dims as usize).sum::<usize>();
        if values > 65 {
            return Err(bad("floor values"));
        }
        f.n_xs = values as u8;
        (f.xs[0], f.xs[1]) = (0, 1 << f.rangebits);
        for x in &mut f.xs[2..values] {
            *x = get(r, f.rangebits)?;
        }
        Ok(f)
    }

    /// The class of each partition.
    pub fn partitions(&self) -> &[u8] {
        &self.partitions[..self.n_partitions as usize]
    }

    pub fn classes(&self) -> &[FloorClass] {
        &self.classes[..self.n_classes as usize]
    }

    /// The X of each value: 0, 2^rangebits, then those of the partitions.
    pub fn xs(&self) -> &[u32] {
        &self.xs[..self.n_xs as usize]
    }

    /// The bits of `Y[0]` and `Y[1]`.
    pub fn range_bits(&self) -> u32 {
        ilog([256, 128, 86, 64][self.multiplier as usize - 1] - 1)
    }
}

/// A residue (§8.6.1), in fixed room: 64 classifications at most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residue {
    pub kind: u32,
    pub begin: u32,
    pub end: u32,
    pub partition: u32,
    pub classifications: u8,
    pub classbook: u8,
    /// Per classification, the book of each of the 8 passes, or None.
    books: [[Option<u8>; 8]; 64],
}

impl Residue {
    fn parse(r: &mut Reader, books: &[Codebook]) -> Result<Residue> {
        let kind = get(r, 16)?;
        if kind > 2 {
            return Err(bad("residue type"));
        }
        let (begin, end, partition) = (get(r, 24)?, get(r, 24)?, get(r, 24)? + 1);
        let classifications = get(r, 6)? as usize + 1;
        let classbook = get(r, 8)? as u8;
        let mut cascade = [0u32; 64];
        for c in &mut cascade[..classifications] {
            let low = get(r, 3)?;
            let high = if get(r, 1)? == 1 { get(r, 5)? } else { 0 };
            *c = high << 3 | low;
        }
        let mut rbooks = [[None; 8]; 64];
        for (b, &c) in rbooks.iter_mut().zip(&cascade[..classifications]) {
            for (j, slot) in b.iter_mut().enumerate() {
                if c >> j & 1 == 1 {
                    let book = get(r, 8)? as usize;
                    if book >= books.len() || books[book].lookup.is_none() {
                        return Err(bad("residue book"));
                    }
                    *slot = Some(book as u8);
                }
            }
        }
        if books.get(classbook as usize).is_none_or(|b| b.dims == 0) {
            return Err(bad("residue classbook"));
        }
        Ok(Residue { kind, begin, end, partition, classifications: classifications as u8, classbook, books: rbooks })
    }

    /// The book of classification `class` in pass `pass`.
    pub fn book(&self, class: usize, pass: usize) -> Option<usize> {
        self.books[class][pass].map(usize::from)
    }
}

/// A mapping, in fixed room: 256 coupling steps, 255 channels, 16 submaps at most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    coupling: [(u8, u8); 256],
    mux: [u8; 255],
    submaps: [(u8, u8); 16],
    n_coupling: u16,
    n_mux: u8,
    n_submaps: u8,
}

impl Mapping {
    fn parse(r: &mut Reader, channels: usize, floors: usize, residues: usize) -> Result<Mapping> {
        if get(r, 16)? != 0 {
            return Err(bad("mapping type"));
        }
        let submaps = if get(r, 1)? == 1 { get(r, 4)? as usize + 1 } else { 1 };
        let mut m = Mapping {
            coupling: [(0, 0); 256],
            mux: [0; 255],
            submaps: [(0, 0); 16],
            n_coupling: 0,
            n_mux: channels as u8,
            n_submaps: submaps as u8,
        };
        if get(r, 1)? == 1 {
            let bits = ilog(channels as u32 - 1);
            m.n_coupling = get(r, 8)? as u16 + 1;
            for c in &mut m.coupling[..m.n_coupling as usize] {
                let (a, b) = (get(r, bits)? as usize, get(r, bits)? as usize);
                if a == b || a >= channels || b >= channels {
                    return Err(bad("mapping coupling"));
                }
                *c = (a as u8, b as u8);
            }
        }
        if get(r, 2)? != 0 {
            return Err(bad("mapping reserved"));
        }
        if submaps > 1 {
            for x in &mut m.mux[..channels] {
                *x = get(r, 4)? as u8;
            }
        }
        if m.mux().iter().any(|&x| x as usize >= submaps) {
            return Err(bad("mapping mux"));
        }
        for s in &mut m.submaps[..submaps] {
            get(r, 8)?;
            let (f, res) = (get(r, 8)? as usize, get(r, 8)? as usize);
            if f >= floors || res >= residues {
                return Err(bad("mapping submap"));
            }
            *s = (f as u8, res as u8);
        }
        Ok(m)
    }

    /// (magnitude, angle) channels.
    pub fn coupling(&self) -> &[(u8, u8)] {
        &self.coupling[..self.n_coupling as usize]
    }

    /// Each channel's submap.
    pub fn mux(&self) -> &[u8] {
        &self.mux[..self.n_mux as usize]
    }

    /// Each submap's (floor, residue).
    pub fn submaps(&self) -> &[(u8, u8)] {
        &self.submaps[..self.n_submaps as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub blockflag: bool,
    pub mapping: u8,
}

/// A parsed setup header, for a stream of [`Ident`]. (Its lists are declared in the reverse of
/// the order they are made in.)
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub modes: Vec<Mode>,
    pub mappings: Vec<Mapping>,
    pub residues: Vec<Residue>,
    pub floors: Vec<Floor>,
    pub codebooks: Codebooks,
    pub ident: Ident,
}

/// The lists of a header, each of `n` items parsed by `item`.
fn list<T>(n: usize, mut item: impl FnMut() -> Result<T>) -> Result<Vec<T>> {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(item()?);
    }
    Ok(v)
}

impl Setup {
    /// Parses setup header `d` (the whole packet, type 5) of a stream of `ident`.
    pub fn parse(d: &[u8], ident: Ident) -> Result<Setup> {
        if !(1..=255).contains(&ident.channels) || ident.blocksizes.iter().any(|b| !b.is_power_of_two() || *b < 64) {
            return Err(bad("identification"));
        }
        let mut r = Reader::new(d);
        let r = &mut r;
        let mut magic = get(r, 8)? == 5;
        for c in b"vorbis" {
            magic &= get(r, 8)? == *c as u32;
        }
        if !magic {
            return Err(bad("not a setup header"));
        }
        let n = get(r, 8)? as usize + 1;
        let mut codebooks = Codebooks(Vec::with_capacity(n));
        for _ in 0..n {
            let book = Codebook::parse(r)?;
            codebooks.0.push(book);
        }
        for _ in 0..get(r, 6)? + 1 {
            if get(r, 16)? != 0 {
                return Err(bad("time domain"));
            }
        }
        let n = get(r, 6)? as usize + 1;
        let floors = list(n, || Floor::parse(r, &codebooks))?;
        let n = get(r, 6)? as usize + 1;
        let residues = list(n, || Residue::parse(r, &codebooks))?;
        let n = get(r, 6)? as usize + 1;
        let mappings = list(n, || Mapping::parse(r, ident.channels, floors.len(), residues.len()))?;
        let n = get(r, 6)? as usize + 1;
        let mut modes = Vec::with_capacity(n);
        for _ in 0..n {
            let blockflag = get(r, 1)? == 1;
            if get(r, 16)? != 0 || get(r, 16)? != 0 {
                return Err(bad("mode window or transform"));
            }
            let mapping = get(r, 8)? as usize;
            if mapping >= mappings.len() {
                return Err(bad("mode mapping"));
            }
            modes.push(Mode { blockflag, mapping: mapping as u8 });
        }
        if get(r, 1)? != 1 {
            return Err(bad("framing"));
        }
        Ok(Setup { ident, codebooks, floors, residues, mappings, modes })
    }

    /// [`Setup::parse`] through a cache of the setups parsed lately (natively, not in the
    /// module): streams share a few setups (the media of a bank, the samples of FSB5 banks), and
    /// parsing one costs as much as decoding a small stream's packets. Its plan, an encoder's
    /// and the module's decode each parsed a stream's setup again.
    #[cfg(not(target_family = "wasm"))]
    pub fn parse_cached(d: &[u8], ident: Ident) -> Result<std::sync::Arc<Setup>> {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        type Cache = Mutex<HashMap<(Vec<u8>, Ident), Arc<Setup>>>;
        /// The most setups kept (then all are dropped).
        const CACHED: usize = 64;
        static CACHE: OnceLock<Cache> = OnceLock::new();
        let cache = CACHE.get_or_init(Cache::default);
        let key = (d.to_vec(), ident);
        if let Some(s) = cache.lock().unwrap().get(&key) {
            return Ok(s.clone());
        }
        let s = Arc::new(Setup::parse(d, ident)?);
        let mut c = cache.lock().unwrap();
        if c.len() >= CACHED {
            c.clear();
        }
        c.insert(key, s.clone());
        Ok(s)
    }
}
