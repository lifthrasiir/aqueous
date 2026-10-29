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
use crate::setup::{Codebook, Setup, lookup1_values};

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

/// A book's ranks.
#[derive(Debug, Clone)]
pub struct Ranks {
    /// Each entry's rank (u32::MAX unused).
    pub rank: Vec<u32>,
    /// Each rank's entry.
    pub entry: Vec<u32>,
}

impl Ranks {
    fn of(b: &Codebook) -> Ranks {
        let mut entry: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        entry.sort_by_key(|&e| (b.lengths[e as usize], e));
        let mut rank = vec![u32::MAX; b.entries()];
        for (r, &e) in entry.iter().enumerate() {
            rank[e as usize] = r as u32;
        }
        Ranks { rank, entry }
    }

    /// The chunks of `n` ranks: in each but the last, the first 255 and an escape.
    pub fn chunks(n: usize) -> usize {
        if n <= ALPHABET { 1 } else { 1 + Ranks::chunks(n - (ALPHABET - 1)) }
    }

    /// Chunk `c`'s alphabet, of a book of `n` ranks.
    pub fn alphabet(n: usize, c: usize) -> usize {
        (n - c * (ALPHABET - 1)).min(ALPHABET)
    }
}

/// The keys of a stream's setup, numbered: per book, each role's contexts of its chunk 0; then
/// per book its later chunks; then per field and bit count its contexts. And each key's default
/// table: its book chunk's (weighted by the code) or flat (a field).
#[derive(Debug, Clone)]
pub struct Keys {
    pub ranks: Vec<Ranks>,
    /// Each book's chunks.
    pub chunks: Vec<usize>,
    role_base: Vec<[u32; 4]>,
    chunk_base: Vec<u32>,
    field_base: Vec<[u32; FIELD_BITS + 1]>,
    /// Each key's default table, an index into `defaults`.
    pub default: Vec<u32>,
    /// The default tables' weights.
    pub defaults: Vec<Vec<u64>>,
    /// Each key's alphabet.
    pub alphabet: Vec<u16>,
}

impl Keys {
    pub fn of(setup: &Setup) -> Keys {
        let ranks: Vec<Ranks> = setup.codebooks.iter().map(Ranks::of).collect();
        let (mut default, mut defaults, mut alphabet) = (Vec::new(), Vec::new(), Vec::new());
        let mut chunk_default = Vec::new();
        for (b, r) in setup.codebooks.iter().zip(&ranks) {
            let n = r.entry.len().max(1);
            chunk_default.push(defaults.len() as u32);
            for c in 0..Ranks::chunks(n) {
                let lo = c * (ALPHABET - 1);
                let w: Vec<u64> = (0..Ranks::alphabet(n, c))
                    .map(|i| {
                        let escape = i == ALPHABET - 1 && lo + ALPHABET < n + 1 && Ranks::chunks(n) > c + 1;
                        let ws =
                            if escape { &r.entry[lo + i..] } else { r.entry.get(lo + i..lo + i + 1).unwrap_or(&[]) };
                        ws.iter().map(|&e| weight(b.lengths[e as usize])).sum::<u64>().max(1)
                    })
                    .collect();
                defaults.push(w);
            }
        }
        let mut role_base = Vec::new();
        for (b, r) in ranks.iter().enumerate() {
            let size = Ranks::alphabet(r.entry.len().max(1), 0) as u16;
            let mut bases = [0u32; 4];
            for (role, &contexts) in ROLES.iter().enumerate() {
                bases[role] = default.len() as u32;
                default.extend(std::iter::repeat_n(chunk_default[b], contexts));
                alphabet.extend(std::iter::repeat_n(size, contexts));
            }
            role_base.push(bases);
        }
        let mut chunk_base = Vec::new();
        for (b, r) in ranks.iter().enumerate() {
            chunk_base.push(default.len() as u32);
            let n = r.entry.len().max(1);
            for c in 1..Ranks::chunks(n) {
                default.push(chunk_default[b] + c as u32);
                alphabet.push(Ranks::alphabet(n, c) as u16);
            }
        }
        let flat: Vec<u32> = (1..=FIELD_BITS)
            .map(|n| {
                defaults.push(vec![1; 1 << n]);
                (defaults.len() - 1) as u32
            })
            .collect();
        let mut field_base = Vec::new();
        for &contexts in &FIELDS {
            let mut bases = [0u32; FIELD_BITS + 1];
            for n in 1..=FIELD_BITS {
                bases[n] = default.len() as u32;
                default.extend(std::iter::repeat_n(flat[n - 1], contexts));
                alphabet.extend(std::iter::repeat_n(1u16 << n, contexts));
            }
            field_base.push(bases);
        }
        let chunks = ranks.iter().map(|r| Ranks::chunks(r.entry.len())).collect();
        Keys { ranks, chunks, role_base, chunk_base, field_base, default, defaults, alphabet }
    }

    pub fn len(&self) -> usize {
        self.default.len()
    }

    pub fn is_empty(&self) -> bool {
        self.default.is_empty()
    }

    /// The key of chunk `c` (> 0) of book `book`.
    pub fn chunk(&self, book: usize, c: usize) -> u32 {
        self.chunk_base[book] + c as u32 - 1
    }
}

/// The weight of a codeword of `len` bits: 2^(32 − len).
fn weight(len: u8) -> u64 {
    1 << (32 - len as u32)
}

const PASSES: usize = 8;
const NONE8: u8 = 255;
const NONE16: u16 = u16::MAX;

/// The values a stream's packets have coded so far, as the contexts need them.
pub struct Contexts<'s> {
    setup: &'s Setup,
    keys: &'s Keys,
    /// Each lookup-1 book's entries' magnitudes (the sum of the distances of the entry's values'
    /// indices from the centre, at most 254), 0 for other books.
    mags: Vec<Vec<u8>>,
    long: usize,
    mode: usize,
    vectors: usize,
    /// Per block size, the last value of each floor field and classword, by slot.
    last: [Vec<u16>; 2],
    /// Per (vector, pass), the magnitude of the entry before in this packet.
    before: Vec<u8>,
    /// The (vector, pass) of the VQ entry whose key was asked last.
    pending: usize,
}

impl<'s> Contexts<'s> {
    pub fn new(setup: &'s Setup, keys: &'s Keys) -> Self {
        let mags = setup
            .codebooks
            .iter()
            .map(|b| {
                let q = lookup1_values(b.entries(), b.dims.max(1)).max(1);
                if b.lookup.as_ref().is_none_or(|l| l.kind != 1) {
                    return vec![0; b.entries()];
                }
                (0..b.entries())
                    .map(|e| {
                        let mut t = e;
                        let mut m = 0usize;
                        for _ in 0..b.dims {
                            m += (2 * (t % q)).abs_diff(q - 1) / 2;
                            t /= q;
                        }
                        m.min(254) as u8
                    })
                    .collect()
            })
            .collect();
        let vectors = setup.ident.channels;
        Contexts {
            setup,
            keys,
            mags,
            long: 0,
            mode: 0,
            vectors,
            last: [Vec::new(), Vec::new()],
            before: vec![NONE8; vectors * PASSES],
            pending: 0,
        }
    }

    fn get_last(&self, slot: usize) -> u16 {
        self.last[self.long].get(slot).copied().unwrap_or(NONE16)
    }

    fn set_last(&mut self, slot: usize, v: u32) {
        let l = &mut self.last[self.long];
        if l.len() <= slot {
            l.resize(slot + 1, NONE16);
        }
        l[slot] = v.min(NONE16 as u32 - 1) as u16;
    }

    /// The key of field `f` of `n` bits (1 to 8).
    pub fn field_key(&self, f: Field, n: u32) -> u32 {
        let (kind, ctx) = match f {
            Field::Type => (0, 0),
            Field::Mode => (1, 0),
            Field::Window => (2, 0),
            Field::Nonzero { ch } => (3, (self.get_last(slot_nonzero(ch)) as u32).min(2)),
            Field::Y01 { ch, i } => match self.get_last(slot_y01(ch, i)) {
                NONE16 => (4 + i, 32),
                v => (4 + i, (v as u32 >> 3).min(31)),
            },
            _ => (0, 0),
        };
        self.keys.field_base[kind][n as usize] + ctx
    }

    /// Records field `f`'s value `v`.
    pub fn field(&mut self, f: Field, v: u32) {
        match f {
            Field::Mode => {
                self.mode = v as usize;
                self.long = self.setup.modes.get(self.mode).is_some_and(|m| m.blockflag) as usize;
                self.before.fill(NONE8);
            }
            Field::Nonzero { ch } => self.set_last(slot_nonzero(ch), v),
            Field::Y01 { ch, i } => self.set_last(slot_y01(ch, i), v),
            _ => {}
        }
    }

    /// The key of chunk 0 of an entry of book `book` read as `f`.
    pub fn entry_key(&mut self, f: Field, book: usize) -> u32 {
        let base = &self.keys.role_base[book];
        let value = |slot: usize| match self.get_last(slot) {
            NONE16 => 16,
            v => (v as u32).min(15),
        };
        match f {
            Field::Vq { v, pass, .. } => {
                self.pending = v.min(self.vectors - 1) * PASSES + pass;
                base[ROLE_VQ]
                    + match self.before[self.pending] {
                        NONE8 => 4,
                        m => m.min(3) as u32,
                    }
            }
            Field::Master { ch, part } => base[ROLE_MASTER] + value(slot_master(ch, part)),
            Field::Y { ch, i } => base[ROLE_Y] + value(slot_y(ch, i)),
            Field::Class { submap, v, part } => base[ROLE_CLASS] + value(self.slot_class(submap, v, part)),
            _ => base[ROLE_MASTER] + 16,
        }
    }

    fn slot_class(&self, submap: usize, v: usize, part: usize) -> usize {
        CLASS_SLOTS + (submap * self.vectors + v) * 1024 + part.min(1023)
    }

    /// For VQ entries of book `book` of vector `v` and pass `pass`: the key of their chunk 0 at
    /// context 0, and their (vector, pass) slot; then [`Contexts::vq_key`] and
    /// [`Contexts::vq_entry`] of each in turn.
    #[inline]
    pub fn vq_run(&self, book: usize, v: usize, pass: usize) -> (u32, usize) {
        (self.keys.role_base[book][ROLE_VQ], v.min(self.vectors - 1) * PASSES + pass)
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
        self.before[slot] = self.mags[book].get(e as usize).copied().unwrap_or(0);
    }

    /// Records entry `e` of book `book` read as `f`, after its key was asked.
    pub fn entry(&mut self, f: Field, book: usize, e: u32) {
        match f {
            Field::Vq { .. } => self.before[self.pending] = self.mags[book].get(e as usize).copied().unwrap_or(0),
            Field::Master { ch, part } => self.set_last(slot_master(ch, part), e),
            Field::Y { ch, i } => self.set_last(slot_y(ch, i), e),
            Field::Class { submap, v, part } => {
                let slot = self.slot_class(submap, v, part);
                self.set_last(slot, e)
            }
            _ => {}
        }
    }
}

// The slots of floor fields in `last`: per channel 128 of them (nonzero, Y[0], Y[1], 32 master
// entries, 93 Y values); then the classwords', per (submap, vector) 1024.
const CLASS_SLOTS: usize = 256 * 128;

fn slot_nonzero(ch: usize) -> usize {
    ch * 128
}

fn slot_y01(ch: usize, i: usize) -> usize {
    ch * 128 + 1 + i
}

fn slot_master(ch: usize, part: usize) -> usize {
    ch * 128 + 3 + part.min(31)
}

fn slot_y(ch: usize, i: usize) -> usize {
    ch * 128 + 35 + i.min(92)
}
