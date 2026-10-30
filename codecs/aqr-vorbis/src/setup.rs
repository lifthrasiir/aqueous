//! The setup header (Vorbis I §4.2.4): codebooks (§3), floors of type 1 (§7.2), residues (§8),
//! mappings and modes, as far as parsing audio packets needs them, and the codebooks' lookup
//! tables for telling what an entry codes.

use crate::bits::{Reader, ilog};
use crate::{Error, Result};

fn bad(what: &'static str) -> Error {
    Error(what)
}

/// What a stream's identification header tells that the setup and the packets depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub channels: usize,
    /// The short and long block sizes, in samples.
    pub blocksizes: [usize; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Codebook {
    pub dims: usize,
    /// Each entry's codeword length, 0 where the entry is unused.
    pub lengths: Vec<u8>,
    pub lookup: Option<Lookup>,
    /// Each entry's codeword (its first bit the most significant of `len` bits), as §3.2.1
    /// assigns them.
    pub codewords: Vec<u32>,
    /// The decoding tree: per node, its two children ([`LEAF`] | entry, a node, or [`NONE`]).
    tree: Vec<[u32; 2]>,
    /// Each entry's codeword in the order it is packed (its first bit lowest).
    packed: Vec<u32>,
    /// Decoding by the next [`FAST`] bits (as packed): an entry and its length (`entry | len <<
    /// 24`), the node reached past them ([`LEAF`] clear, `NODE`), or nothing ([`NONE`] as `u32::MAX`).
    fast: Vec<u32>,
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

/// A codebook's vector lookup table (§3.2.1, lookup types 1 and 2).
#[derive(Debug, Clone, PartialEq)]
pub struct Lookup {
    pub kind: u8,
    /// The packed float32 minimum and delta, as stored.
    pub min: u32,
    pub delta: u32,
    pub value_bits: u8,
    pub sequence: bool,
    pub multiplicands: Vec<u32>,
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
        let bits_left = r.len() - r.pos();
        let mut get = |n| r.bits(n).ok_or(bad("setup header ends early"));
        if get(24)? != 0x564342 {
            return Err(bad("codebook sync"));
        }
        let (dims, entries) = (get(16)? as usize, get(24)? as usize);
        let mut lengths = vec![0u8; entries];
        if get(1)? == 1 {
            let (mut at, mut len) = (0, get(5)? + 1);
            while at < entries {
                let n = get(ilog((entries - at) as u32))? as usize;
                if n > entries - at || len > 32 {
                    return Err(bad("codebook lengths"));
                }
                lengths[at..at + n].fill(len as u8);
                at += n;
                len += 1;
            }
        } else {
            let sparse = get(1)? == 1;
            for l in &mut lengths {
                if !sparse || get(1)? == 1 {
                    *l = get(5)? as u8 + 1;
                }
            }
        }
        let lookup = match get(4)? {
            0 => None,
            kind @ (1 | 2) => {
                let (min, delta) = (get(32)?, get(32)?);
                let value_bits = get(4)? as u8 + 1;
                let sequence = get(1)? == 1;
                let n = if kind == 1 { lookup1_values(entries, dims) } else { entries.saturating_mul(dims) };
                if n.saturating_mul(value_bits as usize) > bits_left {
                    return Err(bad("setup header ends early"));
                }
                let multiplicands = (0..n).map(|_| get(value_bits as u32)).collect::<Result<_>>()?;
                Some(Lookup { kind: kind as u8, min, delta, value_bits, sequence, multiplicands })
            }
            _ => return Err(bad("codebook lookup type")),
        };
        if dims == 0 && lookup.is_some() {
            return Err(bad("codebook dimensions"));
        }
        let (codewords, tree) = assign(&lengths)?;
        let packed: Vec<u32> = codewords
            .iter()
            .zip(&lengths)
            .map(|(&c, &l)| if l == 0 { 0 } else { c.reverse_bits() >> (32 - l as u32) })
            .collect();
        let fast = fast_table(&tree);
        Ok(Codebook { dims, lengths, lookup, codewords, tree, packed, fast })
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
    #[inline]
    pub fn write(&self, e: u32, w: &mut crate::bits::Writer) {
        w.bits(self.packed[e as usize], self.lengths[e as usize] as u32);
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

/// The table of [`Codebook::fast`] from the decoding tree.
fn fast_table(tree: &[[u32; 2]]) -> Vec<u32> {
    let mut t = vec![u32::MAX; 1 << FAST];
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
    t
}

/// The codewords of §3.2.1: in entry order, each used entry takes the lowest codeword of its
/// length that neither is prefixed by one taken nor prefixes one; and the tree that decodes them.
/// A single used entry takes the codeword 0 whatever its length; a tree with unused paths is
/// allowed (those paths decode to nothing).
fn assign(lengths: &[u8]) -> Result<(Vec<u32>, Vec<[u32; 2]>)> {
    // nodes: children, and whether the node is full (a leaf, or both children full)
    let mut tree: Vec<[u32; 2]> = vec![[NONE; 2]];
    let mut full = vec![false];
    let mut codewords = vec![0u32; lengths.len()];
    // the leftmost free node at depth `len` below `node` (at `depth`): its path, or None
    fn find(tree: &[[u32; 2]], full: &[bool], node: usize, depth: u32, len: u32) -> Option<u32> {
        for b in 0..2u32 {
            let c = tree[node][b as usize];
            if depth + 1 == len {
                if c == NONE {
                    return Some(b);
                }
            } else if c == NONE {
                return Some(b << (len - depth - 1));
            } else if c & LEAF == 0
                && !full[c as usize]
                && let Some(p) = find(tree, full, c as usize, depth + 1, len)
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
        let code = if used == 1 { 0 } else { find(&tree, &full, 0, 0, len).ok_or(bad("codebook overspecified"))? };
        codewords[e] = code;
        let (mut node, mut path) = (0usize, Vec::with_capacity(len as usize));
        for i in (0..len).rev() {
            let b = (code >> i & 1) as usize;
            path.push(node);
            if i == 0 {
                tree[node][b] = LEAF | e as u32;
            } else {
                if tree[node][b] == NONE {
                    tree.push([NONE; 2]);
                    full.push(false);
                    tree[node][b] = (tree.len() - 1) as u32;
                }
                node = tree[node][b] as usize;
            }
        }
        // a node is full once both its children are leaves or full
        for &n in path.iter().rev() {
            let f = |c: u32| c != NONE && (c & LEAF != 0 || full[c as usize]);
            if f(tree[n][0]) && f(tree[n][1]) {
                full[n] = true;
            } else {
                break;
            }
        }
    }
    Ok((codewords, tree))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorClass {
    pub dims: usize,
    pub subclasses: u32,
    pub masterbook: usize,
    /// A book per subclass, or None.
    pub subbooks: Vec<Option<usize>>,
}

/// A floor of type 1 (§7.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floor {
    pub partitions: Vec<usize>,
    pub classes: Vec<FloorClass>,
    pub multiplier: u32,
    pub rangebits: u32,
    /// The X of each value: 0, 2^rangebits, then those of the partitions.
    pub xs: Vec<u32>,
}

impl Floor {
    fn parse(r: &mut Reader, books: &[Codebook]) -> Result<Floor> {
        let mut get = |n| r.bits(n).ok_or(bad("setup header ends early"));
        match get(16)? {
            1 => {}
            0 => return Err(bad("floor type 0 is not handled")),
            _ => return Err(bad("floor type")),
        }
        let n = get(5)? as usize;
        let partitions: Vec<usize> = (0..n).map(|_| get(4).map(|c| c as usize)).collect::<Result<_>>()?;
        let mut classes = Vec::new();
        for _ in 0..partitions.iter().max().map_or(0, |&m| m + 1) {
            let dims = get(3)? as usize + 1;
            let subclasses = get(2)?;
            let masterbook = if subclasses > 0 { get(8)? as usize } else { 0 };
            let subbooks: Vec<Option<usize>> = (0..1 << subclasses)
                .map(|_| get(8).map(|b| b.checked_sub(1).map(|b| b as usize)))
                .collect::<Result<_>>()?;
            let book_ok = |b: usize| b < books.len();
            if subclasses > 0 && !book_ok(masterbook) || subbooks.iter().flatten().any(|&b| !book_ok(b)) {
                return Err(bad("floor book"));
            }
            classes.push(FloorClass { dims, subclasses, masterbook, subbooks });
        }
        let multiplier = get(2)? + 1;
        let rangebits = get(4)?;
        let mut xs = vec![0, 1 << rangebits];
        for &c in &partitions {
            for _ in 0..classes[c].dims {
                xs.push(get(rangebits)?);
            }
        }
        if xs.len() > 65 {
            return Err(bad("floor values"));
        }
        Ok(Floor { partitions, classes, multiplier, rangebits, xs })
    }

    /// The bits of `Y[0]` and `Y[1]`.
    pub fn range_bits(&self) -> u32 {
        ilog([256, 128, 86, 64][self.multiplier as usize - 1] - 1)
    }
}

/// A residue (§8.6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residue {
    pub kind: u32,
    pub begin: usize,
    pub end: usize,
    pub partition: usize,
    pub classifications: usize,
    pub classbook: usize,
    /// Per classification, the book of each of the 8 passes, or None.
    pub books: Vec<[Option<usize>; 8]>,
}

impl Residue {
    fn parse(r: &mut Reader, books: &[Codebook]) -> Result<Residue> {
        let mut get = |n| r.bits(n).ok_or(bad("setup header ends early"));
        let kind = get(16)?;
        if kind > 2 {
            return Err(bad("residue type"));
        }
        let (begin, end, partition) = (get(24)? as usize, get(24)? as usize, get(24)? as usize + 1);
        let classifications = get(6)? as usize + 1;
        let classbook = get(8)? as usize;
        let mut cascade = Vec::with_capacity(classifications);
        for _ in 0..classifications {
            let low = get(3)?;
            let high = if get(1)? == 1 { get(5)? } else { 0 };
            cascade.push(high << 3 | low);
        }
        let mut rbooks = Vec::with_capacity(classifications);
        for c in cascade {
            let mut b = [None; 8];
            for (j, slot) in b.iter_mut().enumerate() {
                if c >> j & 1 == 1 {
                    let book = get(8)? as usize;
                    if book >= books.len() || books[book].lookup.is_none() {
                        return Err(bad("residue book"));
                    }
                    *slot = Some(book);
                }
            }
            rbooks.push(b);
        }
        if books.get(classbook).is_none_or(|b| b.dims == 0) {
            return Err(bad("residue classbook"));
        }
        Ok(Residue { kind, begin, end, partition, classifications, classbook, books: rbooks })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// (magnitude, angle) channels.
    pub coupling: Vec<(usize, usize)>,
    /// Each channel's submap.
    pub mux: Vec<usize>,
    /// Each submap's (floor, residue).
    pub submaps: Vec<(usize, usize)>,
}

impl Mapping {
    fn parse(r: &mut Reader, channels: usize, floors: usize, residues: usize) -> Result<Mapping> {
        let mut get = |n| r.bits(n).ok_or(bad("setup header ends early"));
        if get(16)? != 0 {
            return Err(bad("mapping type"));
        }
        let submaps = if get(1)? == 1 { get(4)? as usize + 1 } else { 1 };
        let mut coupling = Vec::new();
        if get(1)? == 1 {
            let bits = ilog(channels as u32 - 1);
            for _ in 0..get(8)? + 1 {
                let (m, a) = (get(bits)? as usize, get(bits)? as usize);
                if m == a || m >= channels || a >= channels {
                    return Err(bad("mapping coupling"));
                }
                coupling.push((m, a));
            }
        }
        if get(2)? != 0 {
            return Err(bad("mapping reserved"));
        }
        let mux = if submaps > 1 {
            (0..channels).map(|_| get(4).map(|m| m as usize)).collect::<Result<Vec<_>>>()?
        } else {
            vec![0; channels]
        };
        if mux.iter().any(|&m| m >= submaps) {
            return Err(bad("mapping mux"));
        }
        let mut subs = Vec::with_capacity(submaps);
        for _ in 0..submaps {
            get(8)?;
            let (f, res) = (get(8)? as usize, get(8)? as usize);
            if f >= floors || res >= residues {
                return Err(bad("mapping submap"));
            }
            subs.push((f, res));
        }
        Ok(Mapping { coupling, mux, submaps: subs })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub blockflag: bool,
    pub mapping: usize,
}

/// A parsed setup header, for a stream of [`Ident`].
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub ident: Ident,
    pub codebooks: Vec<Codebook>,
    pub floors: Vec<Floor>,
    pub residues: Vec<Residue>,
    pub mappings: Vec<Mapping>,
    pub modes: Vec<Mode>,
}

impl Setup {
    /// Parses setup header `d` (the whole packet, type 5) of a stream of `ident`.
    pub fn parse(d: &[u8], ident: Ident) -> Result<Setup> {
        if !(1..=255).contains(&ident.channels) || ident.blocksizes.iter().any(|b| !b.is_power_of_two() || *b < 64) {
            return Err(bad("identification"));
        }
        let mut r = Reader::new(d);
        let get = |r: &mut Reader, n| r.bits(n).ok_or(bad("setup header ends early"));
        if get(&mut r, 8)? != 5
            || (0..6).map(|_| get(&mut r, 8)).collect::<Result<Vec<_>>>()? != b"vorbis".map(u32::from)
        {
            return Err(bad("not a setup header"));
        }
        let n = get(&mut r, 8)? as usize + 1;
        let codebooks = (0..n).map(|_| Codebook::parse(&mut r)).collect::<Result<Vec<_>>>()?;
        for _ in 0..get(&mut r, 6)? + 1 {
            if get(&mut r, 16)? != 0 {
                return Err(bad("time domain"));
            }
        }
        let n = get(&mut r, 6)? as usize + 1;
        let floors = (0..n).map(|_| Floor::parse(&mut r, &codebooks)).collect::<Result<Vec<_>>>()?;
        let n = get(&mut r, 6)? as usize + 1;
        let residues = (0..n).map(|_| Residue::parse(&mut r, &codebooks)).collect::<Result<Vec<_>>>()?;
        let n = get(&mut r, 6)? as usize + 1;
        let mappings = (0..n)
            .map(|_| Mapping::parse(&mut r, ident.channels, floors.len(), residues.len()))
            .collect::<Result<Vec<_>>>()?;
        let n = get(&mut r, 6)? as usize + 1;
        let mut modes = Vec::with_capacity(n);
        for _ in 0..n {
            let blockflag = get(&mut r, 1)? == 1;
            if get(&mut r, 16)? != 0 || get(&mut r, 16)? != 0 {
                return Err(bad("mode window or transform"));
            }
            let mapping = get(&mut r, 8)? as usize;
            if mapping >= mappings.len() {
                return Err(bad("mode mapping"));
            }
            modes.push(Mode { blockflag, mapping });
        }
        if get(&mut r, 1)? != 1 {
            return Err(bad("framing"));
        }
        Ok(Setup { ident, codebooks, floors, residues, mappings, modes })
    }
}
