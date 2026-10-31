//! The transform (FORMAT.md, "Codes" and "Rebuilding the text"): the bytes of the codes and
//! flags ([`Layout`]), the dictionary of the text's words and markup, and the text with them
//! replaced by codes ([`Dictionary::encode`], [`decode`]).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Arc, Mutex};

use aqr_common::varint;

use crate::{Result, corrupt};

/// The longest word or markup coded, in bytes.
pub const MAX_TOKEN: usize = 48;

/// Words and markup seen fewer times than this are left as they are.
pub const MIN_COUNT: u32 = 4;

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
/// word of one letter or of mixed case, or a token longer than [`MAX_TOKEN`].
fn entry<'a>(t: &[u8], word: bool, buf: &'a mut [u8; MAX_TOKEN]) -> Option<(&'a [u8], Case)> {
    if t.len() > MAX_TOKEN || word && t.len() < 2 {
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

/// Data longer than this is counted and coded in pieces of about as many bytes, each ending
/// with a byte that no token holds ([`apart`]; or the data), on threads of their own, so that
/// the pieces code to the same bytes as the whole.
pub(crate) const PIECE: usize = 16 << 20;

/// Whether `c` is neither in any token nor a space: the tokens of what follows it are found as
/// they would be from there, and a word just after it is not preceded by a space.
fn apart(c: u8) -> bool {
    !name_byte(c) && !matches!(c, b'<' | b'>' | b'/' | b'&' | b'#' | b';' | b' ')
}

/// `d` in pieces ([`PIECE`]).
fn pieces(d: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while d.len() - at > PIECE {
        let Some(e) = d[at + PIECE..].iter().position(|&b| apart(b)) else { break };
        out.push(&d[at..at + PIECE + e + 1]);
        at += PIECE + e + 1;
    }
    out.push(&d[at..]);
    out
}

/// The counts of the entries of `d`, and of its bytes.
fn count(d: &[u8]) -> (Map<u32>, [u64; 256]) {
    let mut counts: Map<u32> = Map::default();
    let mut buf = [0; MAX_TOKEN];
    tokens(d, |s, e, word| {
        if let Some((t, _)) = entry(&d[s..e], word, &mut buf) {
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
        let mut entries: Vec<(Box<[u8]>, u32)> = counts.into_iter().filter(|e| e.1 >= MIN_COUNT).collect();
        if entries.is_empty() {
            return None;
        }
        entries.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let counts: Vec<u64> = entries.iter().map(|e| e.1 as u64).collect();
        let layout = layout(&bytes, &counts);
        entries.truncate(layout.capacity() as usize);
        let entries: Vec<Box<[u8]>> = entries.into_iter().map(|e| e.0).collect();
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

    /// The dictionary as the module's resource: the layout, then each entry followed by LF.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.layout.params();
        for e in &self.entries {
            out.extend_from_slice(e);
            out.push(b'\n');
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
        let (mut at, mut buf) = (0, [0; MAX_TOKEN]);
        tokens(d, |s, e, word| {
            let Some((t, case)) = entry(&d[s..e], word, &mut buf) else { return };
            let Some(&i) = self.index.get(t) else { return };
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
            at = e;
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

/// A dictionary as the decoder takes it: its resource, the layout, and where each entry is in
/// the resource and whether it is a word.
struct Parsed {
    bytes: Vec<u8>,
    layout: Layout,
    entries: Vec<(usize, usize, bool)>,
}

impl Parsed {
    fn of(bytes: &[u8]) -> Result<Parsed> {
        let mut at = 0;
        let layout = Layout::read(bytes, &mut at)?;
        let mut entries = Vec::new();
        while at < bytes.len() {
            let e = at + bytes[at..].iter().position(|&b| b == b'\n').ok_or_else(corrupt)?;
            let t = &bytes[at..e];
            let word = t.iter().all(u8::is_ascii_lowercase);
            if t.is_empty() || !word && t[0].is_ascii_alphabetic() {
                return Err(corrupt());
            }
            entries.push((at, e, word));
            at = e + 1;
        }
        if entries.len() as u64 > layout.capacity() {
            return Err(corrupt());
        }
        Ok(Parsed { bytes: bytes.to_vec(), layout, entries })
    }

    /// The text `out` from the coded text `text` of a piece, which must fill all of `out`.
    fn piece(&self, text: &[u8], out: &mut [u8]) -> Result<()> {
        let (layout, entries, dict) = (&self.layout, &self.entries, &self.bytes[..]);
        let (class, place) = layout.classes();
        let (n1, n2) = (layout.one.len(), layout.two.len() * 256);
        let byte = |j: usize| text.get(j).copied().ok_or_else(corrupt);
        let put = |out: &mut [u8], o: &mut usize, s: &[u8]| -> Result<()> {
            out.get_mut(*o..*o + s.len()).ok_or_else(corrupt)?.copy_from_slice(s);
            *o += s.len();
            Ok(())
        };
        let (mut i, mut o) = (0, 0);
        while i < text.len() {
            let s = i;
            while i < text.len() && class[text[i] as usize] == LIT {
                i += 1;
            }
            put(out, &mut o, &text[s..i])?;
            if i == text.len() {
                break;
            }
            let mut c = class[text[i] as usize];
            if c == FLAG + ESC as u8 {
                let b = byte(i + 1)?;
                if class[b as usize] == LIT {
                    return Err(corrupt());
                }
                put(out, &mut o, &[b])?;
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
            let t = &dict[s..e];
            if !word {
                if nosp || case != Case::Lower {
                    return Err(corrupt());
                }
                put(out, &mut o, t)?;
            } else {
                if !nosp {
                    put(out, &mut o, b" ")?;
                }
                let w = o;
                put(out, &mut o, t)?;
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

thread_local! {
    /// The dictionary last parsed on the thread, as the texts of an archive share one.
    static PARSED: std::cell::RefCell<Option<Arc<Parsed>>> = const { std::cell::RefCell::new(None) };
}

/// The coded and text lengths of the pieces (each a pair of varints), as the modules'
/// parameters.
pub fn params(lens: &[(usize, usize)]) -> Vec<u8> {
    let mut p = Vec::new();
    for &(c, t) in lens {
        varint::put(&mut p, c as u64);
        varint::put(&mut p, t as u64);
    }
    p
}

/// The text of `len` bytes from coded text `text` in the pieces of `params` ([`params`]), each
/// decoded by `piece` (its coded text, and its text to fill) on up to `threads` threads.
pub(crate) fn in_pieces(
    params: &[u8],
    text: &[u8],
    len: usize,
    threads: usize,
    piece: impl Fn(&[u8], &mut [u8]) -> Result<()> + Sync,
) -> Result<Vec<u8>> {
    let mut lens = Vec::new();
    let (mut at, mut coded, mut total) = (0, 0usize, 0usize);
    while at < params.len() {
        let mut get = || varint::get(params, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
        let (c, t) = (get()?, get()?);
        coded = coded.checked_add(c).ok_or_else(corrupt)?;
        total = total.checked_add(t).ok_or_else(corrupt)?;
        lens.push((c, t));
    }
    if lens.is_empty() || coded != text.len() || total != len {
        return Err(corrupt());
    }
    let mut out = vec![0; len];
    let mut jobs = Vec::with_capacity(lens.len());
    let (mut rest, mut text) = (&mut out[..], text);
    for &(c, t) in &lens {
        let (o, r) = rest.split_at_mut(t);
        let (x, y) = text.split_at(c);
        jobs.push(Mutex::new((x, o)));
        (rest, text) = (r, y);
    }
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
    let p = PARSED.with(|c| {
        let mut c = c.borrow_mut();
        match &*c {
            Some(p) if p.bytes == dict => Ok(p.clone()),
            _ => {
                let p = Arc::new(Parsed::of(dict)?);
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
        let (r, p) = (dict.to_bytes(), params(&lens));
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
        let mut d = line.repeat((PIECE * 5 / 2) / line.len()).into_bytes();
        // (a line of minified JSON, cut where no token is)
        let json = r#"{"t":"Some words","u":"</b>&amp;&#38;","v":"x"},"#;
        d.extend_from_slice(&json.repeat((PIECE * 3 / 2) / json.len()).into_bytes());
        // (a piece that cannot be cut)
        d.extend_from_slice(&b"the end ".repeat(PIECE / 4));
        assert!(pieces(&d).len() == 4 && pieces(&d).concat() == d);
        let one = Dictionary::of(&[&d], 1).unwrap();
        let four = Dictionary::of(&[&d], 4).unwrap();
        assert_eq!(one.to_bytes(), four.to_bytes());
        let (text, lens) = one.encode(&d, 4);
        assert!(text == one.encode_piece(&d) && lens.len() == 4);
        let (r, p) = (one.to_bytes(), params(&lens));
        for threads in [1, 4] {
            assert!(decode(&p, &r, &text, d.len(), threads).unwrap() == d);
        }
        // pieces not where they were coded, or none
        let mut moved = lens.clone();
        (moved[0].0, moved[1].0) = (moved[0].0 - 1, moved[1].0 + 1);
        assert!(decode(&params(&moved), &r, &text, d.len(), 4).is_err());
        assert!(decode(&[], &r, &text, d.len(), 4).is_err());
    }

    #[test]
    fn rejects() {
        let d = times("the cat and The CAT and the cat. ", 10);
        let dict = Dictionary::of(&[&d], 1).unwrap();
        let (text, _) = dict.encode(&d, 1);
        let l = &dict.layout;
        let p = l.params();
        // (as one piece)
        let decode = |r: &[u8], text: &[u8], len: usize| decode(&params(&[(text.len(), len)]), r, text, len, 1);
        let bad = |entries: &[u8], text: &[u8]| decode(&[&p[..], entries].concat(), text, d.len()).is_err();
        // an entry empty, unterminated or of letters not all lower case; a code past the entries
        assert!(!bad(b"cat\nthe\nand\n", &text));
        assert!(bad(b"cat\n\n", &text));
        assert!(bad(b"cat\nthe\nand", &text));
        assert!(bad(b"cat\nThe\nand\n", &text));
        assert!(bad(b"cat\nthe\n", &text));
        // a flag at the end, an escape of a byte not of the layout, flags before markup
        assert!(bad(&entries(&dict), &[l.flags[NOSP]]));
        assert!(bad(&entries(&dict), &[l.flags[ESC], b'a']));
        let r = [&p[..], b"<a\n"].concat();
        assert!(decode(&r, &[l.flags[CAP], l.one[0]], 2).is_err());
        assert_eq!(decode(&r, &[l.one[0]], 2).unwrap(), b"<a");
        assert!(decode(&p[..3], &[], 0).is_err());
        assert!(Layout::parse(&[1, 2, 3, 3, 0, 0, 0]).is_err());
        assert!(Layout::parse(&[1, 2, 3, b'a', 0, 0, 0]).is_err());
        assert!(Layout::parse(&[1, 2, 3, 4, 0, 0, 0, 0]).is_err());
    }
}
