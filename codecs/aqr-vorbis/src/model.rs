//! What each of a stream's packet values is coded as: a symbol of a **key** (the table it is coded
//! with), the keys chosen by the value's role, its book and a context of the values before it
//! (FORMAT.md §6). The encoder and the decoder keep the same [`Contexts`].
//!
//! - A codebook entry is coded by its **rank**, the entries in order of codeword length (then
//!   entry number); ranks past 254 are an escape (255) and the rank less 255 in the book's next
//!   **chunk**, and so on, as the tables hold up to 256 symbols.
//! - A residue VQ entry's key has a context of the magnitude of the entry before it in its vector
//!   and pass (in the packet). A floor's or a classword's, of the value of the same field in the
//!   previous packet of its block size. A field of bits (`n` of them) is coded whole as a symbol of 2^n,
//!   in a context of the same where it is a floor's.

use crate::packet::Field;
use crate::setup::{Setup, lookup1_values};

/// The most symbols of a table.
pub const ALPHABET: usize = 256;
/// The escape symbol of a chunk with more ranks after it.
pub const ESCAPE: u32 = 255;

/// The roles of entries, by their contexts' count.
const ROLES: [usize; 4] = [VQ_CONTEXTS, VALUE_CONTEXTS, VALUE_CONTEXTS, VALUE_CONTEXTS];
const ROLE_VQ: usize = 0;
const ROLE_MASTER: usize = 1;
const ROLE_Y: usize = 2;
const ROLE_CLASS: usize = 3;
/// A VQ entry's: the magnitude of the entry before, 0..=3 (or 4 for none).
const VQ_CONTEXTS: usize = 5;
/// A floor value's or classword's: the previous packet's, 0..=15 (or 16 for none).
const VALUE_CONTEXTS: usize = 17;
/// Fields, and the contexts of each: type, mode, window, nonzero (previous 0, 1 or none), Y[0]
/// and Y[1] (the previous's value shifted right by 3, at most 31, or none: 32).
const FIELDS: [usize; 6] = [1, 1, 1, 3, 33, 33];
/// Fields are of up to 8 bits (Vorbis I has no wider one an audio packet reads).
pub const FIELD_BITS: usize = 8;

/// The chunks of `n` ranks: in each but the last, the first 255 and an escape.
pub fn chunks(n: usize) -> usize {
    if n <= ALPHABET { 1 } else { 1 + (n - ALPHABET).div_ceil(ALPHABET - 1) }
}

/// Chunk `c`'s alphabet, of a book of `n` ranks.
pub fn alphabet(n: usize, c: usize) -> usize {
    (n - c * (ALPHABET - 1)).min(ALPHABET)
}

/// The keys of the roles of one book, and where each role's are in them.
const ROLE_KEYS: u32 = (ROLES[0] + ROLES[1] + ROLES[2] + ROLES[3]) as u32;
const ROLE_AT: [u32; 4] = [0, ROLES[0] as u32, (ROLES[0] + ROLES[1]) as u32, (ROLES[0] + ROLES[1] + ROLES[2]) as u32];
/// Where each field's keys are among the fields' (`FIELD_BITS` bit counts of each).
const FIELD_AT: [u32; 6] = {
    let (mut at, mut i) = ([0u32; 6], 1);
    while i < 6 {
        at[i] = at[i - 1] + (FIELD_BITS * FIELDS[i - 1]) as u32;
        i += 1;
    }
    at
};

/// The keys of a stream's setup, numbered: per book, each role's contexts of its chunk 0; then
/// per book its later chunks; then per field and bit count its contexts. And each key's default
/// table: its book chunk's (weighted by the code) or flat (a field); the tables numbered per
/// book and chunk, then the flat ones of 1 to 8 bits.
#[derive(Debug, Clone)]
pub struct Keys {
    /// Each key's alphabet.
    alphabet: Vec<u16>,
    /// Each key's table: its default, or (where the record gives one) its own.
    pub default: Vec<u32>,
    /// The key of chunk 1 of each book.
    chunk_base: Vec<u32>,
    /// Each book's entries' ranks (u32::MAX unused), for an encoder.
    rank: Vec<Vec<u32>>,
    /// Each book's used entries in the order of their ranks (the entries in order of codeword
    /// length, then entry number), book `b`'s at `entry[off[b]..off[b + 1]]`.
    entry: Vec<u32>,
    off: Vec<u32>,
    /// The first key of a field.
    field0: u32,
}

impl Keys {
    /// The keys of `setup`; with the entries' ranks where `ranks`.
    pub fn of(setup: &Setup, ranks: bool) -> Keys {
        let books = &setup.codebooks;
        let nb = books.len();
        let mut off = Vec::with_capacity(nb + 1);
        off.push(0u32);
        for b in books {
            off.push(off[off.len() - 1] + b.lengths.iter().filter(|&&l| l > 0).count() as u32);
        }
        // the entries by length (a counting sort: the order of length, then entry)
        let mut entry = vec![0u32; off[nb] as usize];
        let mut rank = Vec::new();
        for (b, book) in books.iter().enumerate() {
            // (`at[l]`: where the entries of length `l` start)
            let mut at = [0u32; 34];
            for &l in book.lengths.iter().filter(|&&l| l > 0) {
                at[l as usize + 1] += 1;
            }
            for l in 1..34 {
                at[l] += at[l - 1];
            }
            let ents = &mut entry[off[b] as usize..off[b + 1] as usize];
            for (e, &l) in book.lengths.iter().enumerate().filter(|(_, l)| **l > 0) {
                ents[at[l as usize] as usize] = e as u32;
                at[l as usize] += 1;
            }
            if ranks {
                let mut rk = vec![u32::MAX; book.entries()];
                ents.iter().enumerate().for_each(|(r, &e)| rk[e as usize] = r as u32);
                rank.push(rk);
            }
        }
        let used = |b: usize| (off[b + 1] - off[b]) as usize;
        let mut chunk_base = Vec::with_capacity(nb);
        let mut next = nb as u32 * ROLE_KEYS;
        for b in 0..nb {
            chunk_base.push(next);
            next += chunks(used(b)) as u32 - 1;
        }
        let field0 = next;
        let keys = field0 as usize + FIELD_BITS * FIELDS.iter().sum::<usize>();
        let (mut default, mut alphabet) = (Vec::with_capacity(keys), Vec::with_capacity(keys));
        // (the numbers of the book tables before book `b`)
        let table = |b: usize| chunk_base[b] - nb as u32 * ROLE_KEYS + b as u32;
        let tables = field0 - nb as u32 * ROLE_KEYS + nb as u32;
        for b in 0..nb {
            let size = alphabet_of(used(b), 0) as u16;
            for &contexts in &ROLES {
                default.extend(std::iter::repeat_n(table(b), contexts));
                alphabet.extend(std::iter::repeat_n(size, contexts));
            }
        }
        for b in 0..nb {
            let n = used(b).max(1);
            for c in 1..chunks(n) {
                default.push(table(b) + c as u32);
                alphabet.push(alphabet_of(n, c) as u16);
            }
        }
        for &contexts in &FIELDS {
            for n in 1..=FIELD_BITS {
                default.extend(std::iter::repeat_n(tables + n as u32 - 1, contexts));
                alphabet.extend(std::iter::repeat_n(1u16 << n, contexts));
            }
        }
        Keys { alphabet, default, chunk_base, rank, entry, off, field0 }
    }

    pub fn len(&self) -> usize {
        self.default.len()
    }

    pub fn is_empty(&self) -> bool {
        self.default.is_empty()
    }

    /// Key `key`'s alphabet.
    pub fn alphabet(&self, key: usize) -> usize {
        self.alphabet[key] as usize
    }

    /// The books.
    pub fn books(&self) -> usize {
        self.chunk_base.len()
    }

    /// The ranks (used entries) of book `book`.
    pub fn used(&self, book: usize) -> usize {
        (self.off[book + 1] - self.off[book]) as usize
    }

    /// The entries of all books that are used.
    pub fn entries(&self) -> usize {
        self.entry.len()
    }

    /// The entry of rank `r` of book `book`.
    #[inline]
    pub fn entry(&self, book: usize, r: usize) -> Option<u32> {
        let (a, b) = (self.off[book] as usize, self.off[book + 1] as usize);
        if r < b - a { self.entry.get(a + r).copied() } else { None }
    }

    /// The rank of entry `e` of book `book` (with ranks asked for).
    pub fn rank(&self, book: usize, e: u32) -> u32 {
        self.rank[book][e as usize]
    }

    /// The chunks of book `book`.
    #[inline]
    pub fn chunks(&self, book: usize) -> usize {
        chunks(self.used(book))
    }

    /// The key of chunk `c` (> 0) of book `book`.
    #[inline]
    pub fn chunk(&self, book: usize, c: usize) -> u32 {
        self.chunk_base[book] + c as u32 - 1
    }

    /// The key of chunk 0 of book `book` as `role`, at its context 0.
    #[inline]
    fn role(&self, book: usize, role: usize) -> u32 {
        book as u32 * ROLE_KEYS + ROLE_AT[role]
    }

    /// The key of field kind `kind` of `n` bits, at its context 0.
    #[inline]
    fn field(&self, kind: usize, n: usize) -> u32 {
        self.field0 + FIELD_AT[kind] + ((n - 1) * FIELDS[kind]) as u32
    }

    /// The default tables: of each book's chunks, then the flat ones of 1 to 8 bits.
    pub fn tables(&self) -> usize {
        (0..self.books()).map(|b| self.chunks(b)).sum::<usize>() + FIELD_BITS
    }

    /// `f` of each default table's size, in the order of their numbers.
    pub fn table_sizes(&self, mut f: impl FnMut(usize)) {
        for b in 0..self.books() {
            let n = self.used(b).max(1);
            (0..self.chunks(b)).for_each(|c| f(alphabet_of(n, c)));
        }
        (1..=FIELD_BITS).for_each(|n| f(1 << n));
    }

    /// `f` of each default table's weights, in the order of their numbers.
    pub fn for_each_table(&self, setup: &Setup, mut f: impl FnMut(&[u64])) {
        let mut w = [0u64; ALPHABET];
        for (b, book) in setup.codebooks.iter().enumerate() {
            let n = self.used(b).max(1);
            let (chunks, ents) = (self.chunks(b), &self.entry[self.off[b] as usize..self.off[b + 1] as usize]);
            for c in 0..chunks {
                let lo = c * (ALPHABET - 1);
                let a = alphabet_of(n, c);
                for (i, x) in w[..a].iter_mut().enumerate() {
                    let escape = i == ALPHABET - 1 && lo + ALPHABET < n + 1 && chunks > c + 1;
                    let ws = if escape { &ents[lo + i..] } else { ents.get(lo + i..lo + i + 1).unwrap_or(&[]) };
                    *x = ws.iter().map(|&e| weight(book.lengths[e as usize])).sum::<u64>().max(1);
                }
                f(&w[..a]);
            }
        }
        for n in 1..=FIELD_BITS {
            w[..1 << n].fill(1);
            f(&w[..1 << n]);
        }
    }
}

fn alphabet_of(n: usize, c: usize) -> usize {
    alphabet(n.max(1), c)
}

/// The weight of a codeword of `len` bits: 2^(32 − len).
fn weight(len: u8) -> u64 {
    1 << (32 - len as u32)
}

const PASSES: usize = 8;
const NONE8: u8 = 255;
const NONE16: u16 = u16::MAX;

/// The slots of the values a stream's packets have coded so far, as the contexts need them: per
/// channel `stride` of them (nonzero, `Y[0]`, `Y[1]`, 32 master entries, the Y values), then the
/// classwords', per (submap, vector) `parts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub stride: usize,
    pub class_base: usize,
    /// The vectors of a submap, and the partitions of a residue (1024 at most), of the most.
    pub vectors: usize,
    pub parts: usize,
    pub total: usize,
}

impl Slots {
    pub fn of(s: &Setup) -> Slots {
        let stride = 35 + s.floors.iter().map(|f| f.xs().len()).max().unwrap_or(0);
        let (mut submaps, mut vectors, mut parts) = (0, 0, 0);
        crate::packet::residue_shapes(s, |submap, p, rows, _| {
            (submaps, vectors, parts) = (submaps.max(submap + 1), vectors.max(rows), parts.max(p));
        });
        let parts = parts.min(1024);
        let class_base = s.ident.channels * stride;
        Slots { stride, class_base, vectors, parts, total: class_base + submaps * vectors * parts }
    }
}

/// The values a stream's packets have coded so far, as the contexts need them.
pub struct Contexts<'s> {
    setup: &'s Setup,
    keys: &'s Keys,
    /// Per (vector, pass), the magnitude of the entry before in this packet.
    before: Vec<u8>,
    /// Per block size, the last value of each floor field and classword, by slot (none for a
    /// block size no mode has).
    last: [Vec<u16>; 2],
    /// Each lookup-1 book's entries' magnitudes (the sum of the distances of the entry's values'
    /// indices from the centre, at most 254).
    mags: Vec<u8>,
    /// Where each book's entries' magnitudes are in `mags` (u32::MAX for none).
    mag_at: Vec<u32>,
    long: usize,
    mode: usize,
    vectors: usize,
    slots: Slots,
    /// The (vector, pass) of the VQ entry whose key was asked last.
    pending: usize,
}

impl<'s> Contexts<'s> {
    /// The lengths of the vectors of the contexts of `setup`, in the order they are made: the
    /// places of the magnitudes, the magnitudes, the last values by block size, the entries
    /// before.
    pub fn sizes(setup: &Setup) -> [usize; 5] {
        let mags = setup.codebooks.iter().filter(|b| b.lookup.as_ref().is_some_and(|l| l.kind == 1));
        let total = Slots::of(setup).total;
        let has = |flag: bool| setup.modes.iter().any(|m| m.blockflag == flag);
        [
            setup.codebooks.len(),
            mags.map(|b| b.entries()).sum(),
            if has(false) { total } else { 0 },
            if has(true) { total } else { 0 },
            setup.ident.channels * PASSES,
        ]
    }

    pub fn new(setup: &'s Setup, keys: &'s Keys) -> Self {
        let [at, mags, last0, last1, before] = Contexts::sizes(setup);
        let (mut mag_at, mut mag) = (Vec::with_capacity(at), Vec::with_capacity(mags));
        for b in &setup.codebooks {
            if b.lookup.as_ref().is_none_or(|l| l.kind != 1) {
                mag_at.push(u32::MAX);
                continue;
            }
            mag_at.push(mag.len() as u32);
            let q = lookup1_values(b.entries(), b.dims.max(1)).max(1);
            mag.extend((0..b.entries()).map(|e| {
                let mut t = e;
                let mut m = 0usize;
                for _ in 0..b.dims {
                    m += (2 * (t % q)).abs_diff(q - 1) / 2;
                    t /= q;
                }
                m.min(254) as u8
            }));
        }
        let slots = Slots::of(setup);
        let last = [vec![NONE16; last0], vec![NONE16; last1]];
        let vectors = setup.ident.channels;
        Contexts {
            setup,
            keys,
            before: vec![NONE8; before],
            last,
            mags: mag,
            mag_at,
            long: 0,
            mode: 0,
            vectors,
            slots,
            pending: 0,
        }
    }

    #[inline]
    fn mag(&self, book: usize, e: u32) -> u8 {
        match self.mag_at[book] {
            u32::MAX => 0,
            at => self.mags.get(at as usize + e as usize).copied().unwrap_or(0),
        }
    }

    fn get_last(&self, slot: usize) -> u16 {
        self.last[self.long].get(slot).copied().unwrap_or(NONE16)
    }

    fn set_last(&mut self, slot: usize, v: u32) {
        if let Some(l) = self.last[self.long].get_mut(slot) {
            *l = v.min(NONE16 as u32 - 1) as u16;
        }
    }

    /// The key of field `f` of `n` bits (1 to 8).
    pub fn field_key(&self, f: Field, n: u32) -> u32 {
        let (kind, ctx) = match f {
            Field::Type => (0, 0),
            Field::Mode => (1, 0),
            Field::Window => (2, 0),
            Field::Nonzero { ch } => (3, (self.get_last(self.slot_nonzero(ch)) as u32).min(2)),
            Field::Y01 { ch, i } => match self.get_last(self.slot_y01(ch, i)) {
                NONE16 => (4 + i, 32),
                v => (4 + i, (v as u32 >> 3).min(31)),
            },
            _ => (0, 0),
        };
        self.keys.field(kind, n as usize) + ctx
    }

    /// Records field `f`'s value `v`.
    pub fn field(&mut self, f: Field, v: u32) {
        match f {
            Field::Mode => {
                self.mode = v as usize;
                self.long = self.setup.modes.get(self.mode).is_some_and(|m| m.blockflag) as usize;
                self.before.fill(NONE8);
            }
            Field::Nonzero { ch } => self.set_last(self.slot_nonzero(ch), v),
            Field::Y01 { ch, i } => self.set_last(self.slot_y01(ch, i), v),
            _ => {}
        }
    }

    /// The key of chunk 0 of an entry of book `book` read as `f`.
    pub fn entry_key(&mut self, f: Field, book: usize) -> u32 {
        let keys = self.keys;
        let value = |slot: usize| match self.get_last(slot) {
            NONE16 => 16,
            v => (v as u32).min(15),
        };
        match f {
            Field::Vq { v, pass, .. } => {
                self.pending = v.min(self.vectors - 1) * PASSES + pass;
                keys.role(book, ROLE_VQ)
                    + match self.before[self.pending] {
                        NONE8 => 4,
                        m => m.min(3) as u32,
                    }
            }
            Field::Master { ch, part } => keys.role(book, ROLE_MASTER) + value(self.slot_master(ch, part)),
            Field::Y { ch, i } => keys.role(book, ROLE_Y) + value(self.slot_y(ch, i)),
            Field::Class { submap, v, part } => keys.role(book, ROLE_CLASS) + value(self.slot_class(submap, v, part)),
            _ => keys.role(book, ROLE_MASTER) + 16,
        }
    }

    /// For VQ entries of book `book` of vector `v` and pass `pass`: the key of their chunk 0 at
    /// context 0, and their (vector, pass) slot; then [`Contexts::vq_key`] and
    /// [`Contexts::vq_entry`] of each in turn.
    #[inline]
    pub fn vq_run(&self, book: usize, v: usize, pass: usize) -> (u32, usize) {
        (self.keys.role(book, ROLE_VQ), v.min(self.vectors - 1) * PASSES + pass)
    }

    /// The context of the next VQ entry of slot `slot` (of [`Contexts::vq_run`]).
    #[inline(always)]
    pub fn vq_key(&self, slot: usize) -> u32 {
        match self.before[slot] {
            NONE8 => 4,
            m => m.min(3) as u32,
        }
    }

    /// Records VQ entry `e` of book `book` of slot `slot`.
    #[inline(always)]
    pub fn vq_entry(&mut self, book: usize, slot: usize, e: u32) {
        self.before[slot] = self.mag(book, e);
    }

    /// Records entry `e` of book `book` read as `f`, after its key was asked.
    pub fn entry(&mut self, f: Field, book: usize, e: u32) {
        match f {
            Field::Vq { .. } => self.before[self.pending] = self.mag(book, e),
            Field::Master { ch, part } => self.set_last(self.slot_master(ch, part), e),
            Field::Y { ch, i } => self.set_last(self.slot_y(ch, i), e),
            Field::Class { submap, v, part } => {
                let slot = self.slot_class(submap, v, part);
                self.set_last(slot, e)
            }
            _ => {}
        }
    }

    fn slot_class(&self, submap: usize, v: usize, part: usize) -> usize {
        self.slots.class_base + (submap * self.slots.vectors + v) * self.slots.parts + part.min(1023)
    }

    fn slot_nonzero(&self, ch: usize) -> usize {
        ch * self.slots.stride
    }

    fn slot_y01(&self, ch: usize, i: usize) -> usize {
        ch * self.slots.stride + 1 + i
    }

    fn slot_master(&self, ch: usize, part: usize) -> usize {
        ch * self.slots.stride + 3 + part.min(31)
    }

    fn slot_y(&self, ch: usize, i: usize) -> usize {
        ch * self.slots.stride + 35 + i.min(92)
    }
}
