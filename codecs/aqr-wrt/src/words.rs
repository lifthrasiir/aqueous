//! The transform (FORMAT.md, "Codes" and "Rebuilding the text"): the bytes of the codes and
//! flags ([`Layout`]), the dictionary of the text's words and markup, and the text with them
//! replaced by codes ([`Dictionary::encode`], [`decode`]).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Mutex;

use aqr_common::varint;

use crate::{Result, corrupt};

/// The most bytes of a token that [`MAX_TOKEN`] may be set to (the size of the buffers).
const TOKEN_CAP: usize = 256;

aqr_common::zopt! {
    /// The longest word or markup coded, in bytes (at most [`TOKEN_CAP`]).
    static MAX_TOKEN: Opt<usize> = Opt::value("wrt:max-token", 48, "longest word or markup coded, in bytes (at most 256)");
    /// Words and markup seen fewer times than this are left as they are.
    static MIN_COUNT: Opt<u32> = Opt::value("wrt:min-count", 4, "words and markup seen fewer times than this are left as they are");
    /// A word with no entry is coded as the longest of its starts of at least this many letters
    /// that has one, and the rest of its letters as they are (0: not at all).
    static PREFIX: Opt<usize> = Opt::value("wrt:prefix", 2, "a word with no entry coded as its longest start of at least this many letters with one (0: not)");
}

/// [`MAX_TOKEN`], checked against the size of the buffers.
fn max_token() -> usize {
    let n = MAX_TOKEN.get();
    assert!(n <= TOKEN_CAP, "wrt:max-token is at most {TOKEN_CAP}");
    n
}

/// The flags, by their place in [`Layout::flags`]: the next byte as it is, the word
/// capitalized, the word in upper case, no space before the word.
const ESC: usize = 0;
const CAP: usize = 1;
const UP: usize = 2;
const NOSP: usize = 3;

/// What a byte of the coded text is ([`Layout::classes`]).
const LIT: u8 = 0;
const FLAG: u8 = 1; // + the flag
const ONE: u8 = 5;
const TWO: u8 = 6;
const THREE: u8 = 7;

/// The bytes of the flags and of the codes, none of them a letter and no two the same.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    /// ESC, CAP, UP and NOSP.
    pub flags: [u8; 4],
    /// The codes of one byte.
    pub one: Vec<u8>,
    /// The first bytes of the codes of two bytes.
    pub two: Vec<u8>,
    /// The first bytes of the codes of three bytes.
    pub three: Vec<u8>,
}

impl Layout {
    /// The module's parameters.
    pub fn params(&self) -> Vec<u8> {
        let mut p = self.flags.to_vec();
        for list in [&self.one, &self.two, &self.three] {
            varint::put(&mut p, list.len() as u64);
            p.extend_from_slice(list);
        }
        p
    }

    /// The layout of parameters `p`, checked, nothing following.
    pub fn parse(p: &[u8]) -> Result<Layout> {
        let mut at = 0;
        let l = Layout::read(p, &mut at)?;
        if at != p.len() {
            return Err(corrupt());
        }
        Ok(l)
    }

    /// The layout at `p[*at..]`, checked, `*at` moved past it.
    fn read(p: &[u8], at: &mut usize) -> Result<Layout> {
        let flags: [u8; 4] = p.get(*at..*at + 4).ok_or_else(corrupt)?.try_into().unwrap();
        *at += 4;
        let mut list = || -> Result<Vec<u8>> {
            let n = varint::get(p, at).map_err(|_| corrupt())?;
            let n = usize::try_from(n).ok().filter(|&n| n <= 256).ok_or_else(corrupt)?;
            let l = p.get(*at..*at + n).ok_or_else(corrupt)?.to_vec();
            *at += n;
            Ok(l)
        };
        let (one, two, three) = (list()?, list()?, list()?);
        let l = Layout { flags, one, two, three };
        let mut seen = [false; 256];
        for b in l.bytes() {
            if b.is_ascii_alphabetic() || std::mem::replace(&mut seen[b as usize], true) {
                return Err(corrupt());
            }
        }
        Ok(l)
    }

    /// The flags' and codes' bytes.
    fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.flags.iter().chain(&self.one).chain(&self.two).chain(&self.three).copied()
    }

    /// The entries the codes tell apart.
    pub fn capacity(&self) -> u64 {
        self.one.len() as u64 + 256 * self.two.len() as u64 + 65536 * self.three.len() as u64
    }

    /// What each byte is in the coded text, and its place in its list.
    fn classes(&self) -> ([u8; 256], [u8; 256]) {
        let (mut class, mut place) = ([LIT; 256], [0; 256]);
        for (i, &b) in self.flags.iter().enumerate() {
            class[b as usize] = FLAG + i as u8;
        }
        for (c, list) in [(ONE, &self.one), (TWO, &self.two), (THREE, &self.three)] {
            for (i, &b) in list.iter().enumerate() {
                (class[b as usize], place[b as usize]) = (c, i as u8);
            }
        }
        (class, place)
    }

    /// The code of entry `i` (below [`Layout::capacity`]).
    fn put_code(&self, out: &mut Vec<u8>, i: usize) {
        let (n1, n2) = (self.one.len(), self.two.len() * 256);
        if i < n1 {
            out.push(self.one[i]);
        } else if i < n1 + n2 {
            let j = i - n1;
            out.extend_from_slice(&[self.two[j >> 8], j as u8]);
        } else {
            let j = i - n1 - n2;
            out.extend_from_slice(&[self.three[j >> 16], (j >> 8) as u8, j as u8]);
        }
    }
}

/// FxHash, for the tokens counted.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, b: &[u8]) {
        let (chunks, rest) = b.as_chunks::<8>();
        for &c in chunks {
            self.add(u64::from_le_bytes(c));
        }
        let mut last = [0; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(last));
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl Fx {
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type Map<V> = HashMap<Box<[u8]>, V, BuildHasherDefault<Fx>>;

/// Whether `c` may be in the name of a tag after its first letter.
fn name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b':' | b'_' | b'.' | b'-')
}

/// The end of the run of `d` from `i` of bytes `f` takes.
fn run(d: &[u8], mut i: usize, f: impl Fn(u8) -> bool) -> usize {
    while i < d.len() && f(d[i]) {
        i += 1;
    }
    i
}

/// The end of the markup at `d[i]` (`<` or `&`), if any: `<` and a name, `</`, a name and `>`,
/// `&`, letters and `;`, or `&#`, digits and `;`.
fn markup(d: &[u8], i: usize) -> Option<usize> {
    let at = |j: usize| d.get(j).copied().unwrap_or(0);
    let e = match d[i] {
        b'<' if at(i + 1) == b'/' && at(i + 2).is_ascii_alphabetic() => {
            let e = run(d, i + 3, name_byte);
            return (at(e) == b'>').then_some(e + 1);
        }
        b'<' if at(i + 1).is_ascii_alphabetic() => return Some(run(d, i + 2, name_byte)),
        b'&' if at(i + 1) == b'#' => run(d, i + 2, |c| c.is_ascii_digit()).max(i + 2),
        b'&' => run(d, i + 1, |c| c.is_ascii_alphabetic()),
        _ => return None,
    };
    (e > i + 1 + (at(i + 1) == b'#') as usize && at(e) == b';').then_some(e + 1)
}

/// Calls `f` with the start and end of each token of `d` in order, and whether it is a word:
/// the runs of letters, and the markup ([`markup`]) but within other markup.
fn tokens(d: &[u8], mut f: impl FnMut(usize, usize, bool)) {
    let mut i = 0;
    while i < d.len() {
        let c = d[i];
        if c.is_ascii_alphabetic() {
            let e = run(d, i + 1, |c| c.is_ascii_alphabetic());
            f(i, e, true);
            i = e;
        } else if (c == b'<' || c == b'&')
            && let Some(e) = markup(d, i)
        {
            f(i, e, false);
            i = e;
        } else {
            i += 1;
        }
    }
}

/// How a word is written against its entry, in lower case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Lower,
    Cap,
    Up,
}

/// Token `t` as an entry would be (in `buf`), and its case; `None` where no entry may be it: a
/// word of one letter or of mixed case, or a token longer than `max` ([`MAX_TOKEN`]).
fn entry<'a>(t: &[u8], word: bool, max: usize, buf: &'a mut [u8; TOKEN_CAP]) -> Option<(&'a [u8], Case)> {
    if t.len() > max || word && t.len() < 2 {
        return None;
    }
    let e = &mut buf[..t.len()];
    e.copy_from_slice(t);
    if !word {
        return Some((e, Case::Lower));
    }
    let case = if t.iter().all(u8::is_ascii_lowercase) {
        Case::Lower
    } else if t.iter().all(u8::is_ascii_uppercase) {
        Case::Up
    } else if t[0].is_ascii_uppercase() && t[1..].iter().all(u8::is_ascii_lowercase) {
        Case::Cap
    } else {
        return None;
    };
    e.make_ascii_lowercase();
    Some((e, case))
}

/// The entries of a dictionary and the layout of their codes, for coding texts.
#[derive(Debug, Clone)]
pub struct Dictionary {
    pub layout: Layout,
    /// The entries, in the order of their codes.
    entries: Vec<Box<[u8]>>,
    /// Each entry's place in `entries`.
    index: Map<u32>,
}

aqr_common::zopt! {
    /// Data longer than this is counted and coded in pieces of about as many bytes, each ending
    /// with a byte that no token holds ([`apart`]; or the data), on threads of their own, so that
    /// the pieces code to the same bytes as the whole.
    pub(crate) static PIECE: Opt<usize> = Opt::size("wrt:piece", 16 << 20, "bytes of a piece coded on a thread of its own");
}

/// Whether `c` is neither in any token nor a space: the tokens of what follows it are found as
/// they would be from there, and a word just after it is not preceded by a space.
fn apart(c: u8) -> bool {
    !name_byte(c) && !matches!(c, b'<' | b'>' | b'/' | b'&' | b'#' | b';' | b' ')
}

/// `d` in pieces ([`PIECE`]).
fn pieces(d: &[u8]) -> Vec<&[u8]> {
    split(d, PIECE.get(), apart, 1)
}

aqr_common::zopt! {
    /// Text is coded and decoded in segments of about this many bytes, each an application of the
    /// module with the one dictionary of all of them (FORMAT.md, "Segments"): what the module holds
    /// of a text at once.
    pub static SEGMENT: Opt<usize> = Opt::size("wrt:segment", 32 << 20, "bytes of text of a segment, coded on its own");
}

/// `d` in segments of about `size` bytes ([`SEGMENT`]), cut as pieces are.
pub fn segments(d: &[u8], size: usize) -> Vec<&[u8]> {
    split(d, size, apart, 1)
}

/// The most bytes past `size` that [`split`] looks for a byte to cut at.
const GAP: usize = 1 << 20;

/// `d` in parts of about `size` bytes, each but the last ending where the first byte of `apart`
/// after its first `size` bytes is (`after` bytes of it with the part, 0 or 1), so that the
/// parts code to the same bytes as the whole. Where none is within [`GAP`] bytes, the cut is at
/// the first byte that does not continue a character of UTF-8: the parts then code to other
/// bytes, but still to the same text, each on its own.
pub(crate) fn split(d: &[u8], size: usize, apart: impl Fn(u8) -> bool, after: usize) -> Vec<&[u8]> {
    // (parts of no bytes would never end)
    let size = size.max(1);
    let mut out = Vec::new();
    let mut at = 0;
    while d.len() - at > size {
        let from = at + size;
        let window = &d[from..(from + GAP).min(d.len())];
        let cut = match window.iter().position(|&b| apart(b)) {
            Some(e) => from + e + after,
            None => from + window.iter().position(|&b| b & 0xC0 != 0x80).unwrap_or(window.len()),
        };
        out.push(&d[at..cut]);
        at = cut;
    }
    out.push(&d[at..]);
    out
}

/// The counts of the entries of `d`, and of its bytes.
fn count(d: &[u8]) -> (Map<u32>, [u64; 256]) {
    let mut counts: Map<u32> = Map::default();
    let mut buf = [0; TOKEN_CAP];
    let max = max_token();
    tokens(d, |s, e, word| {
        if let Some((t, _)) = entry(&d[s..e], word, max, &mut buf) {
            match counts.get_mut(t) {
                Some(c) => *c += 1,
                None => {
                    counts.insert(t.into(), 1);
                }
            }
        }
    });
    let mut bytes = [0u64; 256];
    d.iter().for_each(|&b| bytes[b as usize] += 1);
    (counts, bytes)
}

impl Dictionary {
    /// The dictionary of the entries that `datas` have [`MIN_COUNT`] times or more, most
    /// common first, counted on up to `threads` threads; `None` where there are none.
    pub fn of(datas: &[&[u8]], threads: usize) -> Option<Dictionary> {
        let pieces: Vec<&[u8]> = datas.iter().flat_map(|d| pieces(d)).collect();
        let found = aqr_common::pool::par_map(pieces.len(), threads, |i| count(pieces[i]));
        let mut found = found.into_iter();
        let (mut counts, mut bytes) = found.next()?;
        for (c, b) in found {
            for (t, n) in c {
                *counts.entry(t).or_insert(0) += n;
            }
            (0..256).for_each(|i| bytes[i] += b[i]);
        }
        let mut entries: Vec<(Box<[u8]>, u32)> = counts.into_iter().filter(|e| e.1 >= MIN_COUNT.get()).collect();
        if entries.is_empty() {
            return None;
        }
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let counts: Vec<u64> = entries.iter().map(|e| e.1 as u64).collect();
        let layout = layout(&bytes, &counts);
        entries.truncate(layout.capacity() as usize);
        let mut entries: Vec<Box<[u8]>> = entries.into_iter().map(|e| e.0).collect();
        // (the codes of three bytes in order of their bytes, within each first byte: their order
        // there costs the coded text nothing, and the resource compresses better)
        let three = layout.one.len() + layout.two.len() * 256;
        if three < entries.len() {
            entries[three..].chunks_mut(65536).for_each(<[_]>::sort_unstable);
        }
        let index = entries.iter().enumerate().map(|(i, e)| (e.clone(), i as u32)).collect();
        Some(Dictionary { layout, entries, index })
    }

    /// The entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The dictionary as the module's resource: the layout, then each entry as the count of its
    /// first bytes that are the entry before's (a byte), the rest of it and LF.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.layout.params();
        let mut prev: &[u8] = &[];
        for e in &self.entries {
            let k = e.iter().zip(prev).take_while(|(a, b)| a == b).count().min(255);
            out.push(k as u8);
            out.extend_from_slice(&e[k..]);
            out.push(b'\n');
            prev = e;
        }
        out
    }

    /// Text `d` coded, on up to `threads` threads, and the coded and text lengths of its pieces;
    /// the same whatever `threads` is.
    pub fn encode(&self, d: &[u8], threads: usize) -> (Vec<u8>, Vec<(usize, usize)>) {
        let pieces = pieces(d);
        if pieces.len() == 1 {
            let text = self.encode_piece(d);
            let lens = vec![(text.len(), d.len())];
            return (text, lens);
        }
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
        let layout = &self.layout;
        let (class, _) = layout.classes();
        let esc = layout.flags[ESC];
        let mut text = Vec::with_capacity(d.len());
        let lit = |out: &mut Vec<u8>, s: &[u8]| {
            for &b in s {
                if class[b as usize] != LIT {
                    out.push(esc);
                }
                out.push(b);
            }
        };
        let (mut at, mut buf, max) = (0, [0; TOKEN_CAP], max_token());
        let prefix = PREFIX.get();
        // (the entry of the token from `s` to `e`, its case and where it ends: the token, or the
        // longest start of a word with one ([`PREFIX`]), the rest of its letters left as they are)
        let find = |s: usize, e: usize, word: bool, buf: &mut [u8; TOKEN_CAP]| {
            if let Some((t, case)) = entry(&d[s..e], word, max, buf)
                && let Some(&i) = self.index.get(t)
            {
                return Some((i, case, e));
            }
            if !word || prefix == 0 || e - s <= prefix {
                return None;
            }
            for l in (prefix..(e - s).min(max + 1)).rev() {
                if let Some((t, case)) = entry(&d[s..s + l], true, max, buf)
                    && let Some(&i) = self.index.get(t)
                {
                    return Some((i, case, s + l));
                }
            }
            None
        };
        tokens(d, |s, e, word| {
            if let Some((i, case, end)) = find(s, e, word, &mut buf) {
                let gap = &d[at..s];
                match gap.strip_suffix(b" ") {
                    Some(gap) if word => lit(&mut text, gap),
                    _ => {
                        lit(&mut text, gap);
                        if word {
                            text.push(layout.flags[NOSP]);
                        }
                    }
                }
                match case {
                    Case::Lower => {}
                    Case::Cap => text.push(layout.flags[CAP]),
                    Case::Up => text.push(layout.flags[UP]),
                }
                layout.put_code(&mut text, i as usize);
                at = end;
            }
        });
        lit(&mut text, &d[at..]);
        text
    }
}

/// The layout for entries of `counts` (in order) in text of byte counts `bytes`, of the
/// fewest bytes: the flags and codes are the rarest bytes but letters, each of them in the
/// text escaped (counted twice, as it also breaks the matches through it), and the entries
/// coded in order by codes of one, two and three bytes. Where no layout has room for all
/// entries, the one of the most room.
fn layout(bytes: &[u64; 256], counts: &[u64]) -> Layout {
    let mut order: Vec<u8> = (0..=255u8).filter(|b| !b.is_ascii_alphabetic()).collect();
    order.sort_by_key(|&b| (bytes[b as usize], b));
    let mut sum = vec![0u64];
    for &c in counts {
        sum.push(sum.last().unwrap() + c);
    }
    let n = counts.len();
    let (mut best, mut escaped) = (None::<(u64, usize, usize, usize)>, 0);
    let mut roomiest = (0usize, 0, 0, 0);
    for k in 5..=160 {
        escaped += bytes[order[k - 1] as usize];
        if k < 8 {
            continue;
        }
        for k3 in [0, 1, 2, 4, 8].into_iter().filter(|&k3| k3 + 6 <= k) {
            for k2 in 1..k - 4 - k3 {
                let k1 = k - 4 - k2 - k3;
                let room = k1 + 256 * k2 + 65536 * k3;
                if room > roomiest.0 {
                    roomiest = (room, k, k2, k3);
                }
                if room < n {
                    continue;
                }
                let n1 = k1.min(n);
                let n2 = (256 * k2).min(n - n1);
                let cost = 2 * escaped + sum[n1] + 2 * (sum[n1 + n2] - sum[n1]) + 3 * (sum[n] - sum[n1 + n2]);
                if best.is_none_or(|b| cost < b.0) {
                    best = Some((cost, k, k2, k3));
                }
            }
        }
    }
    let (k, k2, k3) = best.map_or((roomiest.1, roomiest.2, roomiest.3), |b| (b.1, b.2, b.3));
    let k1 = k - 4 - k2 - k3;
    let o = &order[..k];
    Layout {
        flags: o[..4].try_into().unwrap(),
        one: o[4..4 + k1].to_vec(),
        two: o[4 + k1..4 + k1 + k2].to_vec(),
        three: o[4 + k1 + k2..].to_vec(),
    }
}

/// A dictionary as the decoder takes it: the layout, the entries' bytes one after another, and
/// where each entry is in them and whether it is a word.
struct Parsed {
    layout: Layout,
    entries: Vec<(usize, usize, bool)>,
    bytes: Vec<u8>,
}

/// Bytes of an entry of `Parsed` on WebAssembly (32-bit pointers), checked there by an
/// assertion.
pub const ENTRY: u64 = 12;

#[cfg(target_family = "wasm")]
const _: () = assert!(size_of::<(usize, usize, bool)>() as u64 == ENTRY);

/// The shape of a dictionary, as `Parsed` holds it: what the module's memory plan follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Shape {
    /// The count of entries.
    pub entries: usize,
    /// The bytes of the entries, all of them.
    pub bytes: usize,
    /// The lengths of the layout's three lists.
    pub lists: [usize; 3],
}

/// The shape of the dictionary `res`.
pub fn shape(res: &[u8]) -> Result<Shape> {
    let p = Parsed::of(res)?;
    let lists = [p.layout.one.len(), p.layout.two.len(), p.layout.three.len()];
    Ok(Shape { entries: p.entries.len(), bytes: p.bytes.len(), lists })
}

impl Parsed {
    fn of(res: &[u8]) -> Result<Parsed> {
        let mut at = 0;
        let layout = Layout::read(res, &mut at)?;
        // (each entry: the count of the bytes it shares with the one before, the rest, its end)
        let next = |at: usize| -> Result<(usize, usize)> {
            let e = at + 1 + res[at + 1..].iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
            Ok((res[at] as usize, e))
        };
        // (the entries and their bytes counted first, so that both are taken at once)
        let (mut n, mut total, mut prev, mut i) = (0usize, 0usize, 0usize, at);
        while i < res.len() {
            let (shared, e) = next(i)?;
            if shared > prev {
                return Err(corrupt());
            }
            prev = shared + (e - i - 1);
            (n, total, i) = (n + 1, total + prev, e + 1);
        }
        if n as u64 > layout.capacity() {
            return Err(corrupt());
        }
        let mut entries = Vec::with_capacity(n);
        let mut bytes = Vec::with_capacity(total);
        let mut last = 0;
        while at < res.len() {
            let (shared, e) = next(at)?;
            let s = bytes.len();
            bytes.extend_from_within(last..last + shared);
            bytes.extend_from_slice(&res[at + 1..e]);
            let t = &bytes[s..];
            let word = t.iter().all(u8::is_ascii_lowercase);
            if t.is_empty() || !word && t[0].is_ascii_alphabetic() {
                return Err(corrupt());
            }
            entries.push((s, bytes.len(), word));
            (last, at) = (s, e + 1);
        }
        Ok(Parsed { layout, entries, bytes })
    }

    /// The text `out` from the coded text `text` of a piece, which must fill all of `out`.
    fn piece(&self, text: &[u8], out: &mut [u8]) -> Result<()> {
        let (layout, entries) = (&self.layout, &self.entries);
        let (class, place) = layout.classes();
        let (n1, n2) = (layout.one.len(), layout.two.len() * 256);
        let byte = |j: usize| text.get(j).copied().ok_or_else(corrupt);
        let (mut i, mut o) = (0, 0);
        while i < text.len() {
            let s = i;
            while i < text.len() && class[text[i] as usize] == LIT {
                i += 1;
            }
            put_at(out, &mut o, &text[s..i])?;
            if i == text.len() {
                break;
            }
            let mut c = class[text[i] as usize];
            if c == FLAG + ESC as u8 {
                let b = byte(i + 1)?;
                if class[b as usize] == LIT {
                    return Err(corrupt());
                }
                put_at(out, &mut o, &[b])?;
                i += 2;
                continue;
            }
            let nosp = c == FLAG + NOSP as u8;
            if nosp {
                i += 1;
                c = class[byte(i)? as usize];
            }
            let case = match c {
                _ if c == FLAG + CAP as u8 => Case::Cap,
                _ if c == FLAG + UP as u8 => Case::Up,
                _ => Case::Lower,
            };
            if case != Case::Lower {
                i += 1;
                c = class[byte(i)? as usize];
            }
            let j = place[text[i] as usize] as usize;
            let k = match c {
                ONE => {
                    i += 1;
                    j
                }
                TWO => {
                    let k = n1 + j * 256 + byte(i + 1)? as usize;
                    i += 2;
                    k
                }
                THREE => {
                    let k = n1 + n2 + j * 65536 + (byte(i + 1)? as usize) * 256 + byte(i + 2)? as usize;
                    i += 3;
                    k
                }
                _ => return Err(corrupt()),
            };
            let &(s, e, word) = entries.get(k).ok_or_else(corrupt)?;
            let t = &self.bytes[s..e];
            if !word {
                if nosp || case != Case::Lower {
                    return Err(corrupt());
                }
                put_at(out, &mut o, t)?;
            } else {
                if !nosp {
                    put_at(out, &mut o, b" ")?;
                }
                let w = o;
                put_at(out, &mut o, t)?;
                match case {
                    Case::Lower => {}
                    Case::Cap => out[w].make_ascii_uppercase(),
                    Case::Up => out[w..o].make_ascii_uppercase(),
                }
            }
        }
        if o != out.len() {
            return Err(corrupt());
        }
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
thread_local! {
    /// The dictionary last parsed on the thread (and its resource), as the texts of an archive
    /// share one.
    static PARSED: std::cell::RefCell<Option<(Vec<u8>, std::sync::Arc<Parsed>)>> = const { std::cell::RefCell::new(None) };
}

/// `dict` parsed: the parse of the last on the thread where it is the same, natively. In Wasm an
/// instance parses it once and holds nothing else of it.
#[cfg(not(target_family = "wasm"))]
fn parsed(dict: &[u8]) -> Result<impl std::borrow::Borrow<Parsed>> {
    PARSED.with(|c| {
        let mut c = c.borrow_mut();
        match &*c {
            Some((bytes, p)) if bytes == dict => Ok(p.clone()),
            _ => {
                let p = std::sync::Arc::new(Parsed::of(dict)?);
                *c = Some((dict.to_vec(), p.clone()));
                Ok(p)
            }
        }
    })
}

#[cfg(target_family = "wasm")]
fn parsed(dict: &[u8]) -> Result<impl std::borrow::Borrow<Parsed>> {
    Parsed::of(dict)
}

/// `s` copied to `out` at `o`, which moves past it; fails where it does not fit.
pub(crate) fn put_at(out: &mut [u8], o: &mut usize, s: &[u8]) -> Result<()> {
    out.get_mut(*o..*o + s.len()).ok_or_else(corrupt)?.copy_from_slice(s);
    *o += s.len();
    Ok(())
}

/// The parameters of `wrt-recons`: the shape of the dictionary `dict` (its count of entries, of
/// their bytes, and the lengths of its layout's three lists, which the module's memory plan goes
/// by), then the coded and text lengths of the pieces (each a pair of varints).
pub fn params(dict: &[u8], lens: &[(usize, usize)]) -> Vec<u8> {
    [head(dict), params_of(lens)].concat()
}

/// The shape of the dictionary `dict` as the first of the parameters ([`params`]), for the
/// applications of segments that share it.
pub fn head(dict: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    let s = shape(dict).unwrap_or_default();
    for n in [s.entries, s.bytes, s.lists[0], s.lists[1], s.lists[2]] {
        varint::put(&mut p, n as u64);
    }
    p
}

/// The coded and text lengths of the pieces, each a pair of varints.
pub(crate) fn params_of(lens: &[(usize, usize)]) -> Vec<u8> {
    let mut p = Vec::new();
    for &(c, t) in lens {
        varint::put(&mut p, c as u64);
        varint::put(&mut p, t as u64);
    }
    p
}

/// The shape that `params` tell of the dictionary, and the rest of them, the pieces'.
pub fn header(params: &[u8]) -> Result<(Shape, &[u8])> {
    let mut at = 0;
    let mut get = || varint::get(params, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
    let (entries, bytes) = (get()?, get()?);
    let lists = [get()?, get()?, get()?];
    Ok((Shape { entries, bytes, lists }, &params[at..]))
}

/// Calls `f` with the coded and text lengths of each piece of `params` ([`params`]), in order.
fn each_piece(params: &[u8], mut f: impl FnMut(usize, usize) -> Result<()>) -> Result<()> {
    let mut at = 0;
    while at < params.len() {
        let mut get = || varint::get(params, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
        let (c, t) = (get()?, get()?);
        f(c, t)?;
    }
    Ok(())
}

/// The text of `len` bytes from coded text `text` in the pieces of `params` ([`params`]), each
/// decoded by `piece` (its coded text, and its text to fill) on up to `threads` threads; in Wasm,
/// which has none, one after another, the parameters read again for each and nothing held.
pub(crate) fn in_pieces(
    params: &[u8],
    text: &[u8],
    len: usize,
    threads: usize,
    piece: impl Fn(&[u8], &mut [u8]) -> Result<()> + Sync,
) -> Result<Vec<u8>> {
    let (mut coded, mut total, mut pieces) = (0usize, 0usize, 0usize);
    each_piece(params, |c, t| {
        coded = coded.checked_add(c).ok_or_else(corrupt)?;
        total = total.checked_add(t).ok_or_else(corrupt)?;
        pieces += 1;
        Ok(())
    })?;
    if pieces == 0 || coded != text.len() || total != len {
        return Err(corrupt());
    }
    let mut out = vec![0; len];
    if threads <= 1 || pieces == 1 || cfg!(target_family = "wasm") {
        let (mut rest, mut text) = (&mut out[..], text);
        each_piece(params, |c, t| {
            let (o, r) = std::mem::take(&mut rest).split_at_mut(t);
            let (x, y) = text.split_at(c);
            (rest, text) = (r, y);
            piece(x, o)
        })?;
        return Ok(out);
    }
    let mut jobs = Vec::with_capacity(pieces);
    let (mut rest, mut text) = (&mut out[..], text);
    each_piece(params, |c, t| {
        let (o, r) = std::mem::take(&mut rest).split_at_mut(t);
        let (x, y) = text.split_at(c);
        jobs.push(Mutex::new((x, o)));
        (rest, text) = (r, y);
        Ok(())
    })?;
    let done = aqr_common::pool::par_map(jobs.len(), threads, |i| {
        let mut j = jobs[i].lock().unwrap();
        let (x, o) = &mut *j;
        piece(x, o)
    });
    done.into_iter().collect::<Result<()>>()?;
    Ok(out)
}

/// The text of `len` bytes from `text` coded with the dictionary of resource `dict`
/// ([`Dictionary::to_bytes`]), in pieces of `params` ([`params`]), decoded on up to `threads`
/// threads.
pub fn decode(params: &[u8], dict: &[u8], text: &[u8], len: usize, threads: usize) -> Result<Vec<u8>> {
    let (s, params) = header(params)?;
    let p = parsed(dict)?;
    let p: &Parsed = std::borrow::Borrow::borrow(&p);
    // (what the memory plan went by)
    let lists = [p.layout.one.len(), p.layout.two.len(), p.layout.three.len()];
    if (p.entries.len(), p.bytes.len(), lists) != (s.entries, s.bytes, s.lists) {
        return Err(corrupt());
    }
    in_pieces(params, text, len, threads, |x, o| p.piece(x, o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_of_no_size() {
        // (a cut that makes no progress would never end: run apart)
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let d = b"abc\nde";
            let _ = tx.send((split(d, 0, |b| b == b'a', 0).concat() == d, split(d, 0, |b| b == b'x', 0).concat() == d));
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).ok(), Some((true, true)));
    }
    use aqr_common::rng::Rng;

    /// `d` coded with its own dictionary, decoded back.
    fn back(d: &[u8]) -> Option<Dictionary> {
        let dict = Dictionary::of(&[d], 1)?;
        back_with(&dict, d);
        Some(dict)
    }

    fn back_with(dict: &Dictionary, d: &[u8]) -> Vec<u8> {
        let (text, lens) = dict.encode(d, 1);
        assert_eq!(Layout::parse(&dict.layout.params()).unwrap(), dict.layout);
        let r = dict.to_bytes();
        let p = params(&r, &lens);
        let out = decode(&p, &r, &text, d.len(), 1).unwrap();
        assert!(out == d, "{:?}", String::from_utf8_lossy(&out));
        assert!(decode(&p, &r, &text, d.len() + 1, 1).is_err());
        text
    }

    fn times(s: &str, n: usize) -> Vec<u8> {
        s.repeat(n).into_bytes()
    }

    fn entries(d: &Dictionary) -> Vec<u8> {
        d.entries.iter().flat_map(|e| [&e[..], b"\n"].concat()).collect()
    }

    #[test]
    fn roundtrips() {
        assert!(back(b"").is_none());
        assert!(back(b"a b c d e").is_none());
        let d = back(&times("the cat and The CAT and the cat. ", 10)).unwrap();
        assert_eq!(entries(&d), b"cat\nthe\nand\n");
        back(&times("I am, Am I? aM i. iT is", 8));
        back(&times("<page><title>x</title>&amp;&lt;&#123;&#x41; </page", 6));
        back(&times("<a</b>& ;&;&#;</ b>< a><a1-:.b>", 6));
        // bytes of the codes and flags as they are, letters of every case, long words
        let mut d = b"word Word WORD wOrd words\n\0\x01\x7f\x80\xff".repeat(20);
        d.extend_from_slice(&times(&"x".repeat(60), 10));
        d.extend_from_slice(&(0..=255u8).collect::<Vec<_>>());
        back(&d);
    }

    /// A dictionary of other text codes text all the same; text that holds another codes to
    /// what holds the other's code (but where its edges cut a token).
    #[test]
    fn shared() {
        let a = times("the cat and The CAT and the cat. ", 10);
        let dict = Dictionary::of(&[&a], 1).unwrap();
        back_with(&dict, b"dog and cat\n\x00the");
        back_with(&dict, b"");
        let ca = back_with(&dict, &a);
        let cb = back_with(&dict, &[&a[..], b"\nmore of the text"].concat());
        assert!(cb.starts_with(&ca));
    }

    /// A word with no entry is coded as its longest start that has one, in its case, and the rest
    /// of its letters as they are.
    #[test]
    fn starts_of_words() {
        let dict = Dictionary::of(&[&times("nation national ", 4)], 1).unwrap();
        // (a code and a flag of a byte each: entries `nation` and `national`)
        for (text, coded) in [
            (" nationality", 1 + 3),
            (" Nationalist", 2 + 3),
            (" NATIONS", 2 + 1),
            (" nationAL", 1 + 2),
            ("Nationhood", 3 + 4),
            (" na", 3),
        ] {
            assert_eq!(back_with(&dict, text.as_bytes()).len(), coded, "{text}");
        }
        // the entries' order is the counts': a start does not count
        assert_eq!(entries(&dict), b"nation\nnational\n");
    }

    /// The codes of three bytes are in order of their entries' bytes within each first byte;
    /// the others in order of their counts.
    #[test]
    fn order_of_entries() {
        let mut d = Vec::new();
        for i in 0..70000u32 {
            let w: String = format!("{i:05}").bytes().map(|c| (b'a' + c - b'0') as char).collect();
            // (counts in another order than the bytes': the first ones the more the later, the
            // others by `i % 3`)
            for _ in 0..if i < 300 { 304 + i } else { 4 + i % 3 } {
                d.extend_from_slice(w.as_bytes());
                d.push(b' ');
            }
        }
        let dict = Dictionary::of(&[&d], 4).unwrap();
        let three = dict.layout.one.len() + dict.layout.two.len() * 256;
        assert!(three < dict.len(), "{:?}", dict.layout);
        assert!(dict.entries[..300.min(three)].windows(2).all(|w| w[0] > w[1]));
        for g in dict.entries[three..].chunks(65536) {
            assert!(g.is_sorted());
        }
        back_with(&dict, &d);
    }

    #[test]
    fn random() {
        let mut rng = Rng::new(7);
        let words = ["the", "of", "And", "XML", "a", "<tag", "</tag>", "&amp;", " ", "  ", "\n", ".", "\u{e9}t\u{e9}"];
        for n in [100, 1000, 30000] {
            let mut d = Vec::new();
            for _ in 0..n {
                match rng.below(20) {
                    0 => d.push(rng.below(256) as u8),
                    _ => d.extend_from_slice(words[rng.below(words.len() as u64) as usize].as_bytes()),
                }
                if rng.below(3) == 0 {
                    d.push(b' ');
                }
            }
            back(&d).unwrap();
        }
        // many entries: codes of two and three bytes
        let mut d = Vec::new();
        for i in 0..80000u32 {
            let w: Vec<u8> = (0..4).map(|j| b'a' + ((i >> (4 * j)) % 16) as u8).collect();
            for _ in 0..4 {
                d.push(b' ');
                d.extend_from_slice(&w);
            }
        }
        let dict = back(&d).unwrap();
        assert!(!dict.layout.three.is_empty());
    }

    /// Long text is counted and coded in pieces, to the same as a whole.
    #[test]
    fn pieces_as_whole() {
        let line = "Some words, and <b>some</b> MORE words &amp; others\n";
        let mut d = line.repeat((PIECE.get() * 5 / 2) / line.len()).into_bytes();
        // (a line of minified JSON, cut where no token is)
        let json = r#"{"t":"Some words","u":"</b>&amp;&#38;","v":"x"},"#;
        d.extend_from_slice(&json.repeat((PIECE.get() * 3 / 2) / json.len()).into_bytes());
        assert!(pieces(&d).len() == 4 && pieces(&d).concat() == d);
        let one = Dictionary::of(&[&d], 1).unwrap();
        let four = Dictionary::of(&[&d], 4).unwrap();
        assert_eq!(one.to_bytes(), four.to_bytes());
        let (text, lens) = one.encode(&d, 4);
        assert!(text == one.encode_piece(&d) && lens.len() == 4);
        let r = one.to_bytes();
        let p = params(&r, &lens);
        for threads in [1, 4] {
            assert!(decode(&p, &r, &text, d.len(), threads).unwrap() == d);
        }
        // pieces not where they were coded, or none
        let mut moved = lens.clone();
        (moved[0].0, moved[1].0) = (moved[0].0 - 1, moved[1].0 + 1);
        assert!(decode(&params(&r, &moved), &r, &text, d.len(), 4).is_err());
        assert!(decode(&[], &r, &text, d.len(), 4).is_err());
    }

    /// Text with no byte to cut at is cut anyway, each part coded on its own.
    #[test]
    fn uncuttable() {
        let d = b"the end ".repeat(PIECE.get() * 3 / 8);
        let parts = split(&d, PIECE.get(), apart, 1);
        assert!(parts.len() == 3 && parts.concat() == d && parts[0].len() == PIECE.get());
        let dict = Dictionary::of(&[&d], 2).unwrap();
        let (text, lens) = dict.encode(&d, 2);
        assert_eq!(lens.len(), 3);
        let r = dict.to_bytes();
        assert!(decode(&params(&r, &lens), &r, &text, d.len(), 2).unwrap() == d);
    }

    #[test]
    fn rejects() {
        let d = times("the cat and The CAT and the cat. ", 10);
        let dict = Dictionary::of(&[&d], 1).unwrap();
        let (text, _) = dict.encode(&d, 1);
        let l = &dict.layout;
        let p = l.params();
        // (as one piece)
        let decode = |r: &[u8], text: &[u8], len: usize| decode(&params(r, &[(text.len(), len)]), r, text, len, 1);
        let bad = |entries: &[u8], text: &[u8]| decode(&[&p[..], entries].concat(), text, d.len()).is_err();
        // an entry empty, unterminated or of letters not all lower case; a code past the entries
        assert!(!bad(b"\0cat\n\0the\n\0and\n", &text));
        assert!(bad(b"\0cat\n\0\n", &text));
        assert!(bad(b"\0cat\n\0the\n\0and", &text));
        assert!(bad(b"\0cat\n\0The\n\0and\n", &text));
        assert!(bad(b"\0cat\n\0the\n", &text));
        // more bytes shared than the entry before has, an entry of no bytes but its count
        assert!(bad(b"\x01cat\n\0the\n\0and\n", &text));
        assert!(bad(b"\0cat\n\x04he\n\0and\n", &text));
        assert!(bad(b"\0cat\n\0the\n\0and\n\0", &text));
        // bytes shared: the same entries, and their codes
        assert!(!bad(b"\0cat\n\0the\n\0and\n", &text));
        let shared = [&p[..], b"\0cat\n\x02r\n\x03\n"].concat();
        let one = |i: usize| [l.flags[NOSP], l.one[i]];
        assert_eq!(decode(&shared, &[one(0), one(1), one(2)].concat(), 9).unwrap(), b"catcarcar");
        // a flag at the end, an escape of a byte not of the layout, flags before markup
        let coded = &dict.to_bytes()[p.len()..];
        assert!(bad(coded, &[l.flags[NOSP]]));
        assert!(bad(coded, &[l.flags[ESC], b'a']));
        let r = [&p[..], b"\0<a\n"].concat();
        assert!(decode(&r, &[l.flags[CAP], l.one[0]], 2).is_err());
        assert_eq!(decode(&r, &[l.one[0]], 2).unwrap(), b"<a");
        assert!(decode(&p[..3], &[], 0).is_err());
        assert!(Layout::parse(&[1, 2, 3, 3, 0, 0, 0]).is_err());
        assert!(Layout::parse(&[1, 2, 3, b'a', 0, 0, 0]).is_err());
        assert!(Layout::parse(&[1, 2, 3, 4, 0, 0, 0, 0]).is_err());
    }

    /// A piece ends between the CR and the LF of a line end: the pieces code as the whole does.
    #[test]
    fn crlf_at_piece_boundary() {
        // (words and spaces up to the piece's size, where the first byte to cut at is a CR)
        let mut d = Vec::new();
        while d.len() + 4 <= PIECE.get() {
            d.extend_from_slice(b"ab ");
        }
        while d.len() < PIECE.get() {
            d.push(b' ');
        }
        d.extend_from_slice(b"\r\nthe end\r\nnext line here\r\n");
        d.extend_from_slice(&b"some more words\r\n".repeat(100));
        let parts = pieces(&d);
        assert!(parts.len() == 2 && parts[1].starts_with(b"\n"), "{}", parts.len());
        let dict = Dictionary::of(&[&d], 2).unwrap();
        let (text, lens) = dict.encode(&d, 2);
        assert_eq!(lens.len(), 2);
        let r = dict.to_bytes();
        assert!(decode(&params(&r, &lens), &r, &text, d.len(), 2).unwrap() == d);
        // (each line end alone, either end of the text, and a lone CR or LF)
        for s in [&b"\r\n"[..], b"\r", b"\n", b"\r\r\n\n\r", b"a\r\n", b"\r\nb"] {
            let d = s.repeat(500);
            if let Some(dict) = Dictionary::of(&[&d], 1) {
                back_with(&dict, &d);
            }
        }
    }

    #[test]
    fn damaged_text_and_params() {
        let d = times("the cat and The CAT &amp; <b>the</b> cat.\r\n", 30);
        let dict = Dictionary::of(&[&d], 1).unwrap();
        let (text, lens) = dict.encode(&d, 1);
        let r = dict.to_bytes();
        let p = params(&r, &lens);
        let limit = std::time::Duration::from_secs(20);
        aqr_testkit::damaged(&text, 60, 64, limit, |v| {
            let _ = decode(&p, &r, v, d.len(), 1);
        });
        aqr_testkit::damaged(&r, 60, 64, limit, |v| {
            let _ = decode(&p, v, &text, d.len(), 1);
        });
        aqr_testkit::damaged(&p, 60, 64, limit, |v| {
            let _ = decode(v, &r, &text, d.len(), 1);
        });
    }
}
