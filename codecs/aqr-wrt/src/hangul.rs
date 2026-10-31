//! The Hangul transform (FORMAT.md, "The module `wrt-recons-hangul`"): Hangul syllables in two
//! bytes rather than UTF-8's three, and the words of Hangul that the texts use most (with the
//! space before them) replaced by codes of one to three bytes of a dictionary ([`Dictionary`],
//! [`decode`]). Text is coded in pieces (`pieces`), which decode on threads of their own.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Mutex;

use aqr_common::varint;

use crate::words::{PIECE, in_pieces};
use crate::{Result, corrupt};

/// The longest token coded by an entry, in bytes (16 syllables, or 15 and the space before).
pub const MAX_TOKEN: usize = 48;

/// Tokens seen fewer times than this have no entry.
pub const MIN_COUNT: u32 = 4;

/// The first Hangul syllable (U+AC00), and how many there are (to U+D7A3).
const FIRST: u32 = 0xac00;
const SYLLABLES: u32 = 11172;

/// The first bytes of a syllable's two (`0x80 + (j >> 8)` for syllable `j`) end before this.
const SYLLABLE_END: u8 = 0x80 + (SYLLABLES >> 8) as u8 + 1;

/// The byte before a byte as it is.
const ESC: u8 = 0xff;

/// The first bytes of the codes of two and three bytes, in order: none of them starts a
/// character in UTF-8 (nor does [`ESC`]) or a syllable's code.
const LEADS: [u8; 32] = {
    let mut l = [0; 32];
    let mut i = 0;
    while i < 22 {
        l[i] = SYLLABLE_END + i as u8;
        i += 1;
    }
    while i < 32 {
        l[i] = 0xf5 + (i - 22) as u8;
        i += 1;
    }
    l
};

/// The shards of the counts ([`count`]), merged on threads of their own.
const SHARDS: usize = 64;

/// The bytes after `b`, the first byte of a character in UTF-8 of more than one, if it is one.
fn tail(b: u8) -> usize {
    match b {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    }
}

fn continues(b: u8) -> bool {
    b & 0xc0 == 0x80
}

/// The syllable (from 0) that `d[i..]` starts with in UTF-8, if it does.
fn syllable(d: &[u8], i: usize) -> Option<u32> {
    let &[a, b, c] = d.get(i..i + 3)? else { unreachable!() };
    if !(0xea..=0xed).contains(&a) || !continues(b) || !continues(c) {
        return None;
    }
    let j = ((a as u32 & 15) << 12 | (b as u32 & 63) << 6 | c as u32 & 63).wrapping_sub(FIRST);
    (j < SYLLABLES).then_some(j)
}

/// The bytes of `d` in syllables (in UTF-8).
pub fn syllable_bytes(d: &[u8]) -> usize {
    let (mut i, mut n) = (0, 0);
    while i < d.len() {
        match syllable(d, i) {
            Some(_) => (i, n) = (i + 3, n + 3),
            None => i += 1,
        }
    }
    n
}

/// Calls `f` with the start and end of each token of `d` in order: a run of syllables as long
/// as it goes, with the space before it if there is one.
fn tokens(d: &[u8], mut f: impl FnMut(usize, usize)) {
    let mut i = 0;
    while i < d.len() {
        let s = i;
        let mut e = i + (d[i] == b' ') as usize;
        while syllable(d, e).is_some() {
            e += 3;
        }
        if e > s + (d[s] == b' ') as usize {
            f(s, e);
            i = e;
        } else {
            i += 1;
        }
    }
}

/// `d` in pieces of about [`PIECE`] bytes, each but the last ending before an ASCII byte other
/// than a space: no token holds one, nor is it in the middle of a character of UTF-8, so that
/// the pieces code to the same bytes as the whole. (Text of JSON may have no LF at all.)
fn pieces(d: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while d.len() - at > PIECE {
        let Some(e) = d[at + PIECE..].iter().position(|&b| b < 0x80 && b != b' ') else { break };
        out.push(&d[at..at + PIECE + e]);
        at += PIECE + e;
    }
    out.push(&d[at..]);
    out
}

/// The hash of a token: tokens of the same hash are counted as one ([`count`]), the first
/// found standing for them all.
fn hash(t: &[u8]) -> u64 {
    let (chunks, rest) = t.as_chunks::<8>();
    let mut h = t.len() as u64;
    for &c in chunks {
        h = (h ^ u64::from_le_bytes(c)).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(31);
    }
    let mut last = [0; 8];
    last[..rest.len()].copy_from_slice(rest);
    h = (h ^ u64::from_le_bytes(last)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // (MurmurHash3's finalizer)
    h = (h ^ h >> 33).wrapping_mul(0xff51_afd7_ed55_8ccd);
    h = (h ^ h >> 33).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ h >> 33
}

/// The hasher of maps keyed by a [`hash`] already.
#[derive(Default)]
struct Hashed(u64);

impl Hasher for Hashed {
    fn write(&mut self, _: &[u8]) {
        unreachable!("keyed by hashes")
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Tokens by their [`hash`]: how many there are, and the first found.
type Counts<'a> = HashMap<u64, (u32, &'a [u8]), BuildHasherDefault<Hashed>>;

/// The shard of the counts ([`count`]) that a token of hash `h` is in: by bits of the hash that
/// the maps go by little.
fn shard(h: u64) -> usize {
    (h >> 32) as usize % SHARDS
}

/// The counts of the tokens of `d` in [`SHARDS`] by their [`hash`] ([`shard`]), and of its
/// bytes.
fn count(d: &[u8]) -> (Vec<Counts<'_>>, [u64; 256]) {
    let mut shards: Vec<Counts> = (0..SHARDS).map(|_| Counts::default()).collect();
    tokens(d, |s, e| {
        let t = &d[s..e];
        if t.len() <= MAX_TOKEN {
            let h = hash(t);
            shards[shard(h)].entry(h).or_insert((0, t)).0 += 1;
        }
    });
    let mut bytes = [0u64; 256];
    d.iter().for_each(|&b| bytes[b as usize] += 1);
    (shards, bytes)
}

/// The entries of a dictionary and the layout of their codes, for coding texts.
#[derive(Debug, Clone)]
pub struct Dictionary {
    /// The codes of one byte (ASCII, not LF).
    pub one: Vec<u8>,
    /// How many of `LEADS` start codes of two bytes; the rest start codes of three.
    pub two: usize,
    /// The entries, in the order of their codes.
    entries: Vec<Box<[u8]>>,
    /// Each entry's place in `entries` by its [`hash`] (the first of each hash).
    index: HashMap<u64, u32, BuildHasherDefault<Hashed>>,
}

impl Dictionary {
    /// The dictionary of the tokens that `datas` have [`MIN_COUNT`] times or more, counted on
    /// up to `threads` threads; `None` where there are none.
    ///
    /// The codes of one byte are the ASCII bytes the texts do not have (but LF), and go to the
    /// tokens that they save the most bytes of (by their count and length); then the codes of
    /// two bytes, then those of three, as many leads of two as leave room for the rest.
    pub fn of(datas: &[&[u8]], threads: usize) -> Option<Dictionary> {
        let pieces: Vec<&[u8]> = datas.iter().flat_map(|d| pieces(d)).collect();
        let found = aqr_common::pool::par_map(pieces.len(), threads, |i| count(pieces[i]));
        let mut bytes = [0u64; 256];
        let mut shards: Vec<Mutex<Vec<Counts>>> = (0..SHARDS).map(|_| Mutex::new(Vec::new())).collect();
        for (s, b) in found {
            (0..256).for_each(|i| bytes[i] += b[i]);
            for (k, m) in s.into_iter().enumerate() {
                shards[k].get_mut().unwrap().push(m);
            }
        }
        // (the shards merged on threads, each keeping the tokens counted often enough)
        let merged = aqr_common::pool::par_map(SHARDS, threads, |k| {
            let maps = std::mem::take(&mut *shards[k].lock().unwrap());
            let mut maps = maps.into_iter();
            let mut all = maps.next().unwrap_or_default();
            for m in maps {
                for (h, (n, t)) in m {
                    all.entry(h).or_insert((0, t)).0 += n;
                }
            }
            let found = all.into_values().filter(|e| e.0 >= MIN_COUNT);
            found.map(|(n, t)| (Box::<[u8]>::from(t), n)).collect::<Vec<_>>()
        });
        drop(shards);
        let found: Vec<(Box<[u8]>, u32)> = merged.into_iter().flatten().collect();
        if found.is_empty() {
            return None;
        }
        let one: Vec<u8> = (0..0x80u8).filter(|&b| b != b'\n' && bytes[b as usize] == 0).collect();
        // the entries of codes of `len` bytes: those of `cands` longer, that save the most
        let saving = |e: &(Box<[u8]>, u32), len: usize| e.1 as u64 * (e.0.len() - len) as u64;
        let take = |cands: Vec<(Box<[u8]>, u32)>, len: usize, n: usize| {
            let mut c: Vec<_> = cands.into_iter().filter(|e| e.0.len() > len).collect();
            c.sort_unstable_by(|a, b| saving(b, len).cmp(&saving(a, len)).then_with(|| a.0.cmp(&b.0)));
            let rest = c.split_off(n.min(c.len()));
            (c, rest)
        };
        let (ones, rest) = take(found, 1, one.len());
        let one = one[..ones.len()].to_vec();
        let n = rest.iter().filter(|e| e.0.len() > 2).count();
        let two = (0..=LEADS.len()).rev().find(|&k| 256 * k + 65536 * (LEADS.len() - k) >= n).unwrap_or(0);
        let (twos, rest) = take(rest, 2, 256 * two);
        let (threes, _) = take(rest, 3, 65536 * (LEADS.len() - two));
        let entries: Vec<Box<[u8]>> = [ones, twos, threes].into_iter().flatten().map(|e| e.0).collect();
        let mut index = HashMap::default();
        for (i, e) in entries.iter().enumerate() {
            index.entry(hash(e)).or_insert(i as u32);
        }
        Some(Dictionary { one, two, entries, index })
    }

    /// The entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The dictionary as the module's resource: the codes of one byte, the leads of two, then
    /// each entry followed by LF.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        varint::put(&mut out, self.one.len() as u64);
        out.extend_from_slice(&self.one);
        varint::put(&mut out, self.two as u64);
        for e in &self.entries {
            out.extend_from_slice(e);
            out.push(b'\n');
        }
        out
    }

    /// The entry of token `t`, if any.
    fn find(&self, t: &[u8]) -> Option<usize> {
        let &i = self.index.get(&hash(t))?;
        (*self.entries[i as usize] == *t).then_some(i as usize)
    }

    /// The code of entry `i`.
    fn put_code(&self, out: &mut Vec<u8>, i: usize) {
        let (n1, n2) = (self.one.len(), 256 * self.two);
        if i < n1 {
            out.push(self.one[i]);
        } else if i < n1 + n2 {
            let j = i - n1;
            out.extend_from_slice(&[LEADS[j >> 8], j as u8]);
        } else {
            let j = i - n1 - n2;
            out.extend_from_slice(&[LEADS[self.two + (j >> 16)], (j >> 8) as u8, j as u8]);
        }
    }

    /// Text `d` coded, on up to `threads` threads, and the coded and text lengths of its pieces;
    /// the same whatever `threads` is.
    pub fn encode(&self, d: &[u8], threads: usize) -> (Vec<u8>, Vec<(usize, usize)>) {
        let pieces = pieces(d);
        // (each piece told of as done: [`aqr_common::progress`])
        let coded = aqr_common::pool::par_map(pieces.len(), threads, |i| {
            let text = self.encode_piece(pieces[i]);
            aqr_common::progress::done(pieces[i]);
            text
        });
        let lens = coded.iter().zip(&pieces).map(|(c, p)| (c.len(), p.len())).collect();
        (coded.concat(), lens)
    }

    pub(crate) fn encode_piece(&self, d: &[u8]) -> Vec<u8> {
        let mut special = [false; 128];
        self.one.iter().for_each(|&b| special[b as usize] = true);
        let mut text = Vec::with_capacity(d.len());
        // bytes between tokens: as they are, but for the codes of one byte and bytes that start
        // no character of UTF-8 (each escaped), and the characters' other bytes
        let gap = |out: &mut Vec<u8>, g: &[u8]| {
            let mut i = 0;
            while i < g.len() {
                let b = g[i];
                let k = tail(b);
                if b < 0x80 {
                    if special[b as usize] {
                        out.push(ESC);
                    }
                    out.push(b);
                    i += 1;
                } else if k > 0 && g.get(i + 1..i + 1 + k).is_some_and(|t| t.iter().all(|&c| continues(c))) {
                    out.extend_from_slice(&g[i..i + 1 + k]);
                    i += 1 + k;
                } else {
                    out.extend_from_slice(&[ESC, b]);
                    i += 1;
                }
            }
        };
        let mut at = 0;
        tokens(d, |s, e| {
            gap(&mut text, &d[at..s]);
            at = e;
            let t = &d[s..e];
            if let Some(i) = self.find(t) {
                return self.put_code(&mut text, i);
            }
            let mut r = s;
            if t[0] == b' ' {
                gap(&mut text, b" ");
                r += 1;
                if let Some(i) = self.find(&d[r..e]) {
                    return self.put_code(&mut text, i);
                }
            }
            while r < e {
                let j = syllable(d, r).unwrap();
                text.extend_from_slice(&[0x80 + (j >> 8) as u8, j as u8]);
                r += 3;
            }
        });
        gap(&mut text, &d[at..]);
        text
    }
}

/// What a byte of the coded text is ([`Parsed::class`]).
const LIT: u8 = 0;
const ONE: u8 = 1;
const SYL: u8 = 2;
const TWO: u8 = 3;
const THREE: u8 = 4;
const ESCAPE: u8 = 5;
const UTF8: u8 = 6;

/// A dictionary as the decoder takes it: its resource, what each byte of the coded text is
/// and its place in its list, and where each entry is in the resource.
struct Parsed {
    bytes: Vec<u8>,
    class: [u8; 256],
    place: [u8; 256],
    n1: usize,
    two: usize,
    entries: Vec<(usize, usize)>,
}

impl Parsed {
    fn of(bytes: &[u8]) -> Result<Parsed> {
        let mut at = 0;
        let n1 = varint::get(bytes, &mut at).map_err(|_| corrupt())?;
        let n1 = usize::try_from(n1).ok().filter(|&n| n <= 0x80).ok_or_else(corrupt)?;
        let one = bytes.get(at..at + n1).ok_or_else(corrupt)?;
        at += n1;
        let two = varint::get(bytes, &mut at).map_err(|_| corrupt())?;
        let two = usize::try_from(two).ok().filter(|&n| n <= LEADS.len()).ok_or_else(corrupt)?;
        let (mut class, mut place) = ([LIT; 256], [0; 256]);
        for b in 0x80..=0xffu8 {
            class[b as usize] = match b {
                ESC => ESCAPE,
                _ if b < SYLLABLE_END => SYL,
                _ if tail(b) > 0 => UTF8,
                _ => LIT,
            };
        }
        for (i, &b) in LEADS.iter().enumerate() {
            (class[b as usize], place[b as usize]) = if i < two { (TWO, i as u8) } else { (THREE, (i - two) as u8) };
        }
        for (i, &b) in one.iter().enumerate() {
            if b >= 0x80 || b == b'\n' || class[b as usize] != LIT {
                return Err(corrupt());
            }
            (class[b as usize], place[b as usize]) = (ONE, i as u8);
        }
        let mut entries = Vec::new();
        while at < bytes.len() {
            let e = at + bytes[at..].iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
            if e == at {
                return Err(corrupt());
            }
            entries.push((at, e));
            at = e + 1;
        }
        let capacity = n1 + 256 * two + 65536 * (LEADS.len() - two);
        if entries.len() > capacity {
            return Err(corrupt());
        }
        Ok(Parsed { bytes: bytes.to_vec(), class, place, n1, two, entries })
    }

    /// Piece `text` decoded into `out`, which it must fill.
    fn piece(&self, text: &[u8], out: &mut [u8]) -> Result<()> {
        let (class, place) = (&self.class, &self.place);
        let (n1, n2) = (self.n1, 256 * self.two);
        let byte = |j: usize| text.get(j).copied().ok_or_else(corrupt);
        let (mut i, mut o) = (0, 0);
        let mut put = |o: &mut usize, s: &[u8]| -> Result<()> {
            out.get_mut(*o..*o + s.len()).ok_or_else(corrupt)?.copy_from_slice(s);
            *o += s.len();
            Ok(())
        };
        while i < text.len() {
            let s = i;
            while i < text.len() && class[text[i] as usize] == LIT {
                i += 1;
            }
            put(&mut o, &text[s..i])?;
            if i == text.len() {
                break;
            }
            let b = text[i];
            let k = match class[b as usize] {
                SYL => {
                    let j = (b as u32 - 0x80) << 8 | byte(i + 1)? as u32;
                    if j >= SYLLABLES {
                        return Err(corrupt());
                    }
                    let c = FIRST + j;
                    put(&mut o, &[0xe0 | (c >> 12) as u8, 0x80 | (c >> 6 & 63) as u8, 0x80 | (c & 63) as u8])?;
                    i += 2;
                    continue;
                }
                ESCAPE => {
                    put(&mut o, &[byte(i + 1)?])?;
                    i += 2;
                    continue;
                }
                UTF8 => {
                    let e = i + 1 + tail(b);
                    let c = text.get(i..e).ok_or_else(corrupt)?;
                    if !c[1..].iter().all(|&c| continues(c)) {
                        return Err(corrupt());
                    }
                    put(&mut o, c)?;
                    i = e;
                    continue;
                }
                ONE => {
                    i += 1;
                    place[b as usize] as usize
                }
                TWO => {
                    let k = n1 + ((place[b as usize] as usize) << 8) + byte(i + 1)? as usize;
                    i += 2;
                    k
                }
                THREE => {
                    let k = n1
                        + n2
                        + ((place[b as usize] as usize) << 16)
                        + ((byte(i + 1)? as usize) << 8)
                        + byte(i + 2)? as usize;
                    i += 3;
                    k
                }
                _ => return Err(corrupt()),
            };
            let &(s, e) = self.entries.get(k).ok_or_else(corrupt)?;
            put(&mut o, &self.bytes[s..e])?;
        }
        if o != out.len() {
            return Err(corrupt());
        }
        Ok(())
    }
}

thread_local! {
    /// The dictionary last parsed on the thread, as the texts of an archive share one.
    static PARSED: std::cell::RefCell<Option<std::sync::Arc<Parsed>>> = const { std::cell::RefCell::new(None) };
}

/// The text of `len` bytes from `text` coded with the dictionary of resource `dict`
/// ([`Dictionary::to_bytes`]), in pieces of `params` ([`params`]), decoded on up to `threads`
/// threads.
pub fn decode(params: &[u8], dict: &[u8], text: &[u8], len: usize, threads: usize) -> Result<Vec<u8>> {
    let p = PARSED.with(|c| {
        let mut c = c.borrow_mut();
        match &*c {
            Some(p) if p.bytes == dict => Ok(p.clone()),
            _ => {
                let p = std::sync::Arc::new(Parsed::of(dict)?);
                *c = Some(p.clone());
                Ok(p)
            }
        }
    })?;
    in_pieces(params, text, len, threads, |x, o| p.piece(x, o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::words::params;
    use aqr_common::rng::Rng;

    fn back_with(dict: &Dictionary, d: &[u8], threads: usize) -> Vec<u8> {
        let (text, lens) = dict.encode(d, threads);
        let (r, p) = (dict.to_bytes(), params(&lens));
        let out = decode(&p, &r, &text, d.len(), threads).unwrap();
        assert!(out == d, "{:?}", String::from_utf8_lossy(&out));
        assert!(decode(&p, &r, &text, d.len() + 1, 1).is_err());
        text
    }

    fn back(d: &[u8]) -> Option<Dictionary> {
        let dict = Dictionary::of(&[d], 1)?;
        back_with(&dict, d, 1);
        Some(dict)
    }

    fn entries(d: &Dictionary) -> Vec<&str> {
        d.entries.iter().map(|e| std::str::from_utf8(e).unwrap()).collect()
    }

    #[test]
    fn syllables() {
        assert_eq!(SYLLABLE_END, 0xac);
        assert_eq!(syllable("가".as_bytes(), 0), Some(0));
        assert_eq!(syllable("힣".as_bytes(), 0), Some(SYLLABLES - 1));
        // jamo, the first after the syllables, a surrogate's bytes, CJK
        for s in ["ㄱ", "\u{d7a4}", "中"] {
            assert_eq!(syllable(s.as_bytes(), 0), None, "{s}");
        }
        assert_eq!(syllable(b"\xed\xa0\x80", 0), None);
        assert_eq!(syllable(b"\xea\xb0", 0), None);
    }

    #[test]
    fn roundtrips() {
        assert!(back(b"").is_none());
        assert!(back("no hangul at all".repeat(10).as_bytes()).is_none());
        let d = back("나는 사람이다. 사람은 나다. 나는 사람이다.\n".repeat(4).as_bytes()).unwrap();
        assert_eq!(entries(&d), [" 사람이다", " 사람은", " 나는", " 나다", "나는"]);
        // every class of byte at once: escapes, codes of one byte in the text, broken UTF-8
        let mut d = "가각 힣 한글과 English, 中文 ㄱㄴ 😀 é\t\0\x01\x1f\x7f".repeat(8).into_bytes();
        d.extend_from_slice(b"\x80\xab\xac\xbf\xc0\xc1\xf5\xfe\xff\xea\xb0\xed\xa0\x80\xe0 \xf0\x9f\x98");
        d.extend_from_slice(&(0..=255u8).collect::<Vec<_>>());
        d.extend_from_slice("끝".as_bytes());
        back(&d).unwrap();
    }

    #[test]
    fn random() {
        let mut rng = Rng::new(5);
        let words = ["사람", "이", "가", "는", "한국어", "글", " ", "  ", "\n", ".", "a", "\u{1100}", "語"];
        for n in [100, 1000, 30000] {
            let mut d = Vec::new();
            for _ in 0..n {
                match rng.below(20) {
                    0 => d.push(rng.below(256) as u8),
                    _ => d.extend_from_slice(words[rng.below(words.len() as u64) as usize].as_bytes()),
                }
            }
            assert!(back(&d).is_some() || n < 30000);
        }
        // many entries: codes of two and three bytes
        let mut d = Vec::new();
        for i in 0..80000u32 {
            let w: String = (0..2).map(|j| char::from_u32(FIRST + ((i >> (8 * j)) & 255) * 17).unwrap()).collect();
            for _ in 0..4 {
                d.push(b' ');
                d.extend_from_slice(w.as_bytes());
            }
        }
        let dict = back(&d).unwrap();
        assert!(dict.two < LEADS.len() && dict.len() > 256 * dict.two);
    }

    /// A dictionary of other text codes text all the same; text that holds another codes to
    /// what holds the other's code.
    #[test]
    fn shared() {
        let a = "나는 사람이다. 사람은 나다. 나는 사람이다.\n".repeat(4).into_bytes();
        let dict = Dictionary::of(&[&a], 1).unwrap();
        back_with(&dict, "다른 사람은\x00 \x01".as_bytes(), 1);
        back_with(&dict, b"", 1);
        let ca = back_with(&dict, &a, 1);
        let cb = back_with(&dict, &[&a[..], "\n사람은 더".as_bytes()].concat(), 1);
        assert!(cb.starts_with(&ca));
    }

    /// Long text is counted and coded in pieces, to the same as a whole, and decoded in them.
    #[test]
    fn pieces_as_whole() {
        // (no LF, as in JSON; a piece of no ASCII but spaces)
        let line = "한국어 문장이 여기에 있다. 그리고 English words 사이에 있다.\\n";
        let mut d = line.repeat((PIECE * 5 / 2) / line.len()).into_bytes();
        d.extend_from_slice(&"끝이다 ".repeat(PIECE / 20).into_bytes());
        assert!(pieces(&d).len() == 3 && pieces(&d).concat() == d);
        assert!(pieces(&d)[1].first().is_some_and(|&b| b < 0x80 && b != b' '));
        let one = Dictionary::of(&[&d], 1).unwrap();
        let four = Dictionary::of(&[&d], 4).unwrap();
        assert_eq!(one.to_bytes(), four.to_bytes());
        let text = back_with(&one, &d, 4);
        assert!(text == one.encode_piece(&d));
        assert_eq!(one.encode(&d, 1).1.len(), 3);
    }

    #[test]
    fn rejects() {
        let d = "나는 사람이다. 사람은 나다. 나는 사람이다.\n".repeat(4).into_bytes();
        let dict = Dictionary::of(&[&d], 1).unwrap();
        let (text, lens) = dict.encode(&d, 1);
        let (r, p) = (dict.to_bytes(), params(&lens));
        assert!(decode(&p, &r, &text, d.len(), 1).is_ok());
        // pieces not of the text's lengths, none
        assert!(decode(&params(&[(text.len() - 1, d.len())]), &r, &text, d.len(), 1).is_err());
        assert!(decode(&[], &r, &[], 0, 1).is_err());
        let one = |t: &[u8], n: usize| decode(&params(&[(t.len(), n)]), &r, t, n, 1);
        // a syllable past the last, a code past the entries, cut codes and escapes
        assert!(one(&[0xab, 0xa3], 3).is_ok() && one(&[0xab, 0xa4], 3).is_err());
        assert!(one(&[LEADS[0], 200], 9).is_err());
        assert!(one(&[0x80], 3).is_err() && one(&[ESC], 1).is_err() && one(&[LEADS[31], 0], 3).is_err());
        // UTF-8 that is not, and bytes that start nothing
        assert!(one("é".as_bytes(), 2).is_ok() && one(b"\xc3\x29", 2).is_err());
        assert!(one(b"\xe4\xb8", 3).is_err());
        // malformed dictionaries: a code of one byte not ASCII or LF, an empty entry, too many leads
        let bad = |r: &[u8]| decode(&params(&[(1, 1)]), r, b"a", 1, 1).is_err();
        assert!(!bad(&[0, 0, b'x', b'\n']));
        assert!(bad(&[1, 0x80, 0]) && bad(&[1, b'\n', 0]) && bad(&[0, 0, b'\n']) && bad(&[0, 33]));
        assert!(bad(&[0, 0, b'x']));
    }
}
