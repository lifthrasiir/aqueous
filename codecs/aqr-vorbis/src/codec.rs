//! A stream's packets, each after its length (16 bits, little-endian, as FSB5 frames them), as
//! a record of their values coded by rANS with tables of the stream's keys, and back
//! (FORMAT.md §4).

use aqr_common::rans32::{BitR, BitW};
use aqr_common::rans64::{self, Tables};
use aqr_common::varint;

use crate::bits::{Reader, Writer};
use crate::model::{Contexts, ESCAPE, Keys};
use crate::packet::{self, End, Field, Scratch, Source};
use crate::setup::Setup;
use crate::{Error, Result};

fn corrupt() -> Error {
    Error("corrupt record")
}

/// Levels of a table: level `l` (1 to 63) weighs about 2^(l / 4); 0 is a symbol not seen.
const LEVELS: u32 = 64;

/// The weight of level `l`: 2^((l − 1) / 4), of 8 fractional bits.
fn level_weight(l: u32) -> u64 {
    const FRAC: [u64; 4] = [65536, 77936, 92682, 110218];
    if l == 0 { 0 } else { (FRAC[((l - 1) % 4) as usize] << ((l - 1) / 4)) >> 8 }
}

/// The frequencies (of 12 bits, each at least 1) of a table of levels, into `out`.
fn level_freqs_into(levels: &[u32], out: &mut [u16]) {
    let mut w = [0u64; 256];
    levels.iter().zip(&mut w).for_each(|(&l, w)| *w = level_weight(l));
    rans64::normalize(&w[..levels.len()], out);
}

fn level_freqs(levels: &[u32]) -> Vec<u16> {
    let mut f = vec![0u16; levels.len()];
    level_freqs_into(levels, &mut f);
    f
}

/// The level of `count` in a table whose largest count is `max`.
fn level_of(count: u32, max: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    let l = (LEVELS - 1) as f64 + (4.0 * (count as f64 / max as f64).log2()).round();
    l.clamp(1.0, (LEVELS - 1) as f64) as u32
}

/// Writes levels: each as the zigzag difference from the one before plus 1 (gamma), a run of 0s
/// as gamma 0 and its length less 1.
fn write_levels(w: &mut BitW, levels: &[u32]) {
    let (mut prev, mut i) = (0i64, 0);
    while i < levels.len() {
        if levels[i] == 0 {
            let run = levels[i..].iter().take_while(|&&l| l == 0).count();
            w.gamma(0);
            w.gamma(run as u32 - 1);
            i += run;
            continue;
        }
        w.gamma(varint::zz(levels[i] as i64 - prev) as u32 + 1);
        prev = levels[i] as i64;
        i += 1;
    }
}

fn gamma(r: &mut BitR) -> Result<usize> {
    let g = r.gamma().map_err(|_| corrupt())? as usize;
    if r.overrun() { Err(corrupt()) } else { Ok(g) }
}

/// Reads `n` levels (up to 256) into `levels`.
fn read_levels(r: &mut BitR, n: usize, levels: &mut [u32; 256]) -> Result<()> {
    let (mut at, mut prev) = (0, 0i64);
    while at < n {
        let g = gamma(r)?;
        if g == 0 {
            let run = gamma(r)? + 1;
            if at + run > n {
                return Err(corrupt());
            }
            levels[at..at + run].fill(0);
            at += run;
            continue;
        }
        prev += varint::unzz(g as u64 - 1);
        if !(1..LEVELS as i64).contains(&prev) {
            return Err(corrupt());
        }
        levels[at] = prev as u32;
        at += 1;
    }
    Ok(())
}

/// A packet that is not as most are (whole, its tail its padding of zeros): where it is, its
/// values' count where it is cut short, and its tail.
struct Odd {
    packet: usize,
    short: Option<usize>,
    tail: Vec<u8>,
}

/// Reads a packet's values, putting each as symbols of its key.
struct Gather<'a, 'k> {
    setup: &'a Setup,
    keys: &'k Keys,
    cx: &'k mut Contexts<'a>,
    r: Reader<'a>,
    syms: &'k mut Vec<(u32, u16)>,
    values: usize,
}

impl Gather<'_, '_> {
    /// Puts entry `e` of book `book` as symbols from key `key` of its chunk 0.
    #[inline(always)]
    fn put(&mut self, book: usize, mut key: u32, e: u32) {
        let (mut r, mut c) = (self.keys.rank(book, e), 0);
        while r >= ESCAPE && c + 1 < self.keys.chunks(book) {
            self.syms.push((key, ESCAPE as u16));
            r -= ESCAPE;
            c += 1;
            key = self.keys.chunk(book, c);
        }
        self.syms.push((key, r as u16));
    }
}

impl Source for Gather<'_, '_> {
    fn vq(&mut self, f: Field, book: usize, reads: usize) -> bool {
        let Field::Vq { v, pass, .. } = f else { return false };
        let (base, slot) = self.cx.vq_run(book, v, pass);
        let cb = &self.setup.codebooks[book];
        for _ in 0..reads {
            let Some(e) = cb.read(&mut self.r) else { return false };
            let key = base + self.cx.vq_key(slot);
            self.put(book, key, e);
            self.cx.vq_entry(book, slot, e);
            self.values += 1;
        }
        true
    }

    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        // (a field of no bits is kept, not coded)
        if n == 0 {
            self.cx.field(f, 0);
            return Some(0);
        }
        let v = self.r.bits(n)?;
        self.syms.push((self.cx.field_key(f, n), v as u16));
        self.cx.field(f, v);
        self.values += 1;
        Some(v)
    }

    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        let e = self.setup.codebooks[book].read(&mut self.r)?;
        let key = self.cx.entry_key(f, book);
        self.put(book, key, e);
        self.cx.entry(f, book, e);
        self.values += 1;
        Some(e)
    }
}

/// The record of the packets at the start of `d` (each after its length; up to the end of `d`
/// or a length of 0, which is not taken): the bytes taken and the record, or None where a
/// packet does not parse under `setup`.
pub fn encode(setup: &Setup, d: &[u8]) -> Option<(usize, Vec<u8>)> {
    let keys = Keys::of(setup, true);
    let mut cx = Contexts::new(setup, &keys);
    let mut scratch = Scratch::new(setup);
    let mut syms: Vec<(u32, u16)> = Vec::with_capacity(d.len() * 2);
    let mut odd = Vec::new();
    let (mut at, mut packets) = (0, 0);
    while at + 2 <= d.len() {
        let n = u16::from_le_bytes([d[at], d[at + 1]]) as usize;
        if n == 0 {
            break;
        }
        let p = d.get(at + 2..at + 2 + n)?;
        let mut g = Gather { setup, keys: &keys, cx: &mut cx, r: Reader::new(p), syms: &mut syms, values: 0 };
        let end = packet::walk_in(setup, &mut scratch, &mut g).ok()?;
        let (values, mut r) = (g.values, g.r);
        let tail: Vec<u8> = (r.pos()..r.len()).map(|_| r.bit().expect("within the packet") as u8).collect();
        if end == End::Short || tail.len() >= 8 || tail.iter().any(|&b| b != 0) {
            odd.push(Odd { packet: packets, short: (end == End::Short).then_some(values), tail });
        }
        packets += 1;
        at += 2 + n;
    }

    // each key's counts; a key gets a table of its own where it pays for itself
    let mut counts: Vec<Vec<u32>> = vec![Vec::new(); keys.len()];
    for &(k, s) in &syms {
        let c = &mut counts[k as usize];
        if c.is_empty() {
            *c = vec![0; keys.alphabet(k as usize)];
        }
        c[s as usize] += 1;
    }
    let mut defaults: Vec<Vec<u16>> = Vec::new();
    keys.for_each_table(setup, |w| {
        let mut f = vec![0u16; w.len()];
        rans64::normalize(w, &mut f);
        defaults.push(f);
    });
    let cost = |c: &[u32], f: &[u16]| -> f64 {
        c.iter().zip(f).filter(|(n, _)| **n > 0).map(|(&n, &f)| n as f64 * (12.0 - (f as f64).log2())).sum()
    };
    let mut own: Vec<(u32, Vec<u32>)> = Vec::new();
    for (k, c) in counts.iter().enumerate().filter(|(_, c)| !c.is_empty()) {
        let max = *c.iter().max().expect("counts");
        let levels: Vec<u32> = c.iter().map(|&n| level_of(n, max)).collect();
        let mut w = BitW::<false>::default();
        write_levels(&mut w, &levels);
        // (and about 8 bits for the key)
        let mine = cost(c, &level_freqs(&levels)) + w.bit_len() as f64 + 8.0;
        if mine < cost(c, &defaults[keys.default[k] as usize]) {
            own.push((k as u32, levels));
        }
    }

    let mut h = BitW::<false>::default();
    h.gamma(packets as u32 + 1);
    h.gamma(odd.len() as u32 + 1);
    let mut last = 0;
    for o in &odd {
        h.gamma((o.packet - last) as u32 + 1);
        last = o.packet + 1;
        h.bits(1, o.short.is_some() as u32);
        if let Some(n) = o.short {
            h.gamma(n as u32 + 1);
        }
        h.gamma(o.tail.len() as u32 + 1);
        o.tail.iter().for_each(|&b| h.bits(1, b as u32));
    }
    h.gamma(own.len() as u32 + 1);
    let mut last = 0;
    for (k, levels) in &own {
        h.gamma(k - last + 1);
        last = k + 1;
        write_levels(&mut h, levels);
    }
    let header = h.finish();

    let (tables, map) = tables(&keys, &defaults, own);
    let mut enc = rans64::Encoder::with_capacity(syms.len());
    let mut l = enc.local_tracked(&tables);
    for &(k, s) in &syms {
        l.put(map[k as usize] as usize, s as u32);
    }
    enc.put_back(l);
    let words = enc.finish(&tables);
    let mut out = Vec::with_capacity(header.len() + 4 * words.len() + 4);
    varint::put(&mut out, header.len() as u64);
    out.extend_from_slice(&header);
    words.iter().for_each(|w| out.extend_from_slice(&w.to_le_bytes()));
    Some((at, out))
}

/// The tables (the defaults, then the keys' own) and each key's table.
fn tables(keys: &Keys, defaults: &[Vec<u16>], own: Vec<(u32, Vec<u32>)>) -> (Tables, Vec<u32>) {
    let mut map = keys.default.clone();
    let mut all: Vec<Vec<u16>> = defaults.to_vec();
    for (k, levels) in own {
        map[k as usize] = all.len() as u32;
        all.push(level_freqs(&levels));
    }
    (Tables::new(&all), map)
}

/// Takes a packet's values from the rANS decoder, writing their bits; up to `left` values where
/// the packet is cut short.
struct Emit<'a, 'k> {
    setup: &'a Setup,
    keys: &'k Keys,
    cx: &'k mut Contexts<'a>,
    map: &'k [u32],
    dec: &'k mut rans64::DecLocal,
    w: Writer,
    left: Option<usize>,
    bad: bool,
}

impl Emit<'_, '_> {
    fn take(&mut self) -> bool {
        match &mut self.left {
            Some(0) => false,
            Some(n) => {
                *n -= 1;
                true
            }
            None => true,
        }
    }
}

impl Emit<'_, '_> {
    /// An entry of book `book` from key `key` of its chunk 0, written; None where its rank is
    /// past the book's.
    #[inline(always)]
    fn get(&mut self, book: usize, mut key: u32) -> Option<u32> {
        let (mut r, mut c) = (0u32, 0);
        loop {
            let s = self.dec.get(self.map[key as usize] as usize);
            if s == ESCAPE && c + 1 < self.keys.chunks(book) {
                r += ESCAPE;
                c += 1;
                key = self.keys.chunk(book, c);
                continue;
            }
            r += s;
            break;
        }
        let e = self.keys.entry(book, r as usize)?;
        self.setup.codebooks[book].write(e, &mut self.w);
        Some(e)
    }
}

impl Source for Emit<'_, '_> {
    fn vq(&mut self, f: Field, book: usize, reads: usize) -> bool {
        let Field::Vq { v, pass, .. } = f else { return false };
        let (base, slot) = self.cx.vq_run(book, v, pass);
        for _ in 0..reads {
            if !self.take() {
                return false;
            }
            let key = base + self.cx.vq_key(slot);
            let Some(e) = self.get(book, key) else {
                self.bad = true;
                return false;
            };
            self.cx.vq_entry(book, slot, e);
        }
        true
    }

    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        if n == 0 {
            self.cx.field(f, 0);
            return Some(0);
        }
        if !self.take() {
            return None;
        }
        let v = self.dec.get(self.map[self.cx.field_key(f, n) as usize] as usize);
        self.w.bits(v, n);
        self.cx.field(f, v);
        Some(v)
    }

    fn entry(&mut self, f: Field, book: usize) -> Option<u32> {
        if !self.take() {
            return None;
        }
        let key = self.cx.entry_key(f, book);
        let Some(e) = self.get(book, key) else {
            self.bad = true;
            return None;
        };
        self.cx.entry(f, book, e);
        Some(e)
    }
}

/// What a record's header holds, counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub packets: usize,
    /// The odd packets, and the bits of their tails.
    pub odd: usize,
    pub tail: usize,
    /// The keys with tables of their own, and the symbols of those.
    pub own: usize,
    pub symbols: usize,
}

/// What a record's header fills, reserved at the size of its [`Counts`]: the odd packets (their
/// numbers, the values' counts of those cut short, the ends of their tails in `tail`, one byte a
/// bit), and the tables (the default ones, then the own).
struct Parsed {
    packet: Vec<u32>,
    short: Vec<u32>,
    tail_end: Vec<u32>,
    tail: Vec<u8>,
    n: Vec<u16>,
    freqs: Vec<u16>,
    canon: Vec<u32>,
}

/// Where a packet's value count is none.
const NOT_SHORT: u32 = u32::MAX;

/// The lengths of the vectors of a `Parsed` of a header of `counts` and keys of `tables`
/// default tables of `symbols` symbols in all, in the order they are made.
pub fn parsed_sizes(counts: &Counts, tables: usize, symbols: usize) -> [usize; 7] {
    let n = tables + counts.own;
    [counts.odd, counts.odd, counts.odd, counts.tail, n, symbols + counts.symbols, n]
}

/// Reads the record's header `h` for a stream of `len` bytes, counting what it holds; into
/// `fill` where given, and the keys' tables to theirs; `own` told each own table's alphabet.
fn header(
    h: &[u8],
    keys: &mut Keys,
    len: usize,
    mut fill: Option<&mut Parsed>,
    own: &mut dyn FnMut(usize),
) -> Result<Counts> {
    let mut r = BitR::<false>::new(h);
    let packets = gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?;
    // (a packet is 3 bytes at least, with its length)
    if packets > len / 3 || packets >= NOT_SHORT as usize {
        return Err(corrupt());
    }
    let mut c = Counts { packets, odd: gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?, tail: 0, own: 0, symbols: 0 };
    let mut next = 0usize;
    for _ in 0..c.odd {
        let packet = next + gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?;
        let short = if r.bits(1) == 1 { Some(gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?) } else { None };
        let n = gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?;
        if packet >= packets || n > 8 * len || short.is_some_and(|s| s >= NOT_SHORT as usize) {
            return Err(corrupt());
        }
        match fill.as_deref_mut() {
            Some(p) => {
                p.packet.push(packet as u32);
                p.short.push(short.map_or(NOT_SHORT, |s| s as u32));
                p.tail.extend((0..n).map(|_| r.bits(1) as u8));
                p.tail_end.push(p.tail.len() as u32);
            }
            None => (0..n).for_each(|_| _ = r.bits(1)),
        }
        if r.overrun() {
            return Err(corrupt());
        }
        c.tail += n;
        next = packet + 1;
    }
    c.own = gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?;
    let mut next = 0usize;
    for _ in 0..c.own {
        let key = next + gamma(&mut r)?.checked_sub(1).ok_or(corrupt())?;
        if key >= keys.len() {
            return Err(corrupt());
        }
        let alphabet = keys.alphabet(key);
        let mut levels = [0u32; 256];
        read_levels(&mut r, alphabet, &mut levels)?;
        if let Some(p) = fill.as_deref_mut() {
            keys.default[key] = p.n.len() as u32;
            p.canon.push(p.n.len() as u32);
            p.n.push(alphabet as u16);
            let at = p.freqs.len();
            p.freqs.resize(at + alphabet, 0);
            level_freqs_into(&levels[..alphabet], &mut p.freqs[at..]);
        }
        c.symbols += alphabet;
        own(alphabet);
        next = key + 1;
    }
    Ok(c)
}

/// The record's header and the rANS words of `record`, for a stream of `len` bytes.
pub fn split(record: &[u8]) -> Result<(&[u8], &[u8])> {
    let mut at = 0;
    let hlen = varint::get(record, &mut at).map_err(|_| corrupt())? as usize;
    let header = record.get(at..at.checked_add(hlen).ok_or(corrupt())?).ok_or(corrupt())?;
    let words = &record[at + hlen..];
    if !words.len().is_multiple_of(4) {
        return Err(corrupt());
    }
    Ok((header, words))
}

/// What the record's header of a stream of `len` bytes holds, for the keys of its setup; `own`
/// told each own table's alphabet.
pub fn counts(record: &[u8], keys: &mut Keys, len: usize, own: &mut dyn FnMut(usize)) -> Result<Counts> {
    header(split(record)?.0, keys, len, None, own)
}

/// The packets of `record`, each after its length, `len` bytes in all.
pub fn decode(setup: &Setup, record: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    decode_into(setup, record, len, &mut out)?;
    Ok(out)
}

/// [`decode`] into `out`, empty and with room for `len` bytes. Everything it takes is reserved
/// at its size as it goes, in this order (the memory plan, `plan.rs`, follows it): the keys, the
/// header's lists and tables, the tables' offsets and decoder, the rANS words, the contexts, and
/// the walk's room; and freed in the reverse order, as `out` is below them all.
pub fn decode_into(setup: &Setup, record: &[u8], len: usize, out: &mut Vec<u8>) -> Result<()> {
    let (head, words) = split(record)?;
    let mut keys = Keys::of(setup, false);
    let c = header(head, &mut keys, len, None, &mut |_| ())?;
    let (mut tables, mut symbols) = (0, 0);
    keys.table_sizes(|a| (tables, symbols) = (tables + 1, symbols + a));
    let [odd, _, _, tail, n, freqs, _] = parsed_sizes(&c, tables, symbols);
    let mut p = Parsed {
        packet: Vec::with_capacity(odd),
        short: Vec::with_capacity(odd),
        tail_end: Vec::with_capacity(odd),
        tail: Vec::with_capacity(tail),
        n: Vec::with_capacity(n),
        freqs: Vec::with_capacity(freqs),
        canon: Vec::with_capacity(n),
    };
    keys.for_each_table(setup, |w| {
        p.canon.push(p.n.len() as u32);
        p.n.push(w.len() as u16);
        let at = p.freqs.len();
        p.freqs.resize(at + w.len(), 0);
        rans64::normalize(w, &mut p.freqs[at..]);
    });
    header(head, &mut keys, len, Some(&mut p), &mut |_| ())?;
    let Parsed { packet: odd_packet, short: odd_short, tail_end, tail, n, freqs, canon } = p;
    let tables = Tables::from_canon(n, freqs, canon);
    tables.prepare_dec(1);
    let mut dec = rans64::Decoder::from_le_bytes(&tables, words);
    let mut cx = Contexts::new(setup, &keys);
    let mut scratch = Scratch::new(setup);
    let mut odds = 0;
    let mut l = dec.local();
    for i in 0..c.packets {
        let o = (odd_packet.get(odds).is_some_and(|&p| p as usize == i)).then(|| {
            odds += 1;
            odds - 1
        });
        let left = o.and_then(|o| (odd_short[o] != NOT_SHORT).then_some(odd_short[o] as usize));
        // (the length, then the packet after it: the output never grows)
        if out.len() + 2 > len {
            return Err(corrupt());
        }
        out.extend_from_slice(&[0, 0]);
        let w = Writer::after(std::mem::take(out), len);
        let mut e = Emit { setup, keys: &keys, cx: &mut cx, map: &keys.default, dec: &mut l, w, left, bad: false };
        let end = packet::walk_in(setup, &mut scratch, &mut e)?;
        let (bad, rest, mut w) = (e.bad, e.left, e.w);
        dec.put_back(l);
        if bad || (end == End::Short) != left.is_some() || rest.is_some_and(|n| n != 0) || !dec.in_bounds() {
            return Err(corrupt());
        }
        match o {
            Some(o) => {
                let from = if o == 0 { 0 } else { tail_end[o - 1] as usize };
                tail[from..tail_end[o] as usize].iter().for_each(|&b| w.bit(b as u32))
            }
            None => w.bits(0, ((8 - w.pos() % 8) % 8) as u32),
        }
        let size = w.pos() / 8;
        if w.over() || !w.pos().is_multiple_of(8) || size == 0 || size > u16::MAX as usize {
            return Err(corrupt());
        }
        *out = w.finish();
        let at = out.len() - size;
        out[at - 2..at].copy_from_slice(&(size as u16).to_le_bytes());
    }
    if out.len() != len {
        return Err(corrupt());
    }
    Ok(())
}
