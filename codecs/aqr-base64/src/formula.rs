//! aqr-base64 as a module of entry formulas (see aqr-formula): the text of a file without its
//! base64 blocks, the bytes they decode to, and the layout that writes them again (FORMAT.md).
//! The text and each block's bytes are contents, recompressed as what they are.
//!
//! The encoder goes over the file once ([`scan`]): a block is found by its lines (an alphabet's
//! characters, a separator, lines of one length), and taken only where writing it again from its
//! bytes gives its bytes back.

use aqr_api::stream::{Put, Slice};
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::module::{self, ALPHABET_SHIFT, MAX_SEP, MAX_SEPS, PADDED, STANDARD, URL_SAFE};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("base64-recons", 1).with_plan(memory).with_contents(&[0, 1]);

/// The memory of the module: its initial memory, and its plan ([`module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    (u.inputs.len() == 3 && u.outputs.len() == 1).then(|| u.initial() + module::plan())
}

aqr_common::zopt! {
    /// The bytes that a base64 block decodes to at least to be taken.
    pub static MIN: Opt<usize> = Opt::size("base64:min", 192, "bytes a base64 block decodes to at least");
    /// The bytes a block decodes to at least to be a content of its own; the others between
    /// such blocks are one content together.
    pub static APART: Opt<usize> =
        Opt::size("base64:apart", 16 << 10, "bytes a base64 block decodes to at least to be recompressed apart");
    /// The percent of a file that its blocks decode to at least for them to be taken: the
    /// formula copies all of the text, which for a few blocks in much text (enwik9: 4 blocks
    /// of 1,883 bytes in 1 GB) costs the copy and gains nothing.
    pub static SHARE: Opt<usize> =
        Opt::value("base64:share", 1, "percent of a file its base64 blocks decode to at least to be taken");
}

/// The least characters of a line that a block may start with: a block starts at a line that
/// long, and a file with none is not looked at further ([`worth_a_look`]).
pub const MIN_LINE: usize = 40;

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 3] = ["text", "data", "layout"];

/// What a byte is of the alphabets: bit 0 of the standard's, bit 1 of the URL-safe's.
const fn classes() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let alnum = b.is_ascii_alphanumeric();
        t[i] = (alnum || b == b'+' || b == b'/') as u8 | ((alnum || b == b'-' || b == b'_') as u8) << 1;
        i += 1;
    }
    t
}
static CLASS: [u8; 256] = classes();

/// What `d` has of the alphabets: bit 0 where a line of [`MIN_LINE`] characters of the standard
/// one, bit 1 where one of the URL-safe.
fn worth_a_look(d: &[u8]) -> u8 {
    let (mut a, mut b) = (0usize, 0usize);
    let mut found = 0u8;
    for &c in d {
        let k = CLASS[c as usize];
        a = if k & 1 != 0 { a + 1 } else { 0 };
        b = if k & 2 != 0 { b + 1 } else { 0 };
        if a >= MIN_LINE {
            found |= 1;
        }
        if b >= MIN_LINE {
            found |= 2;
        }
        if found == 3 {
            break;
        }
    }
    found
}

/// Whether `d` may have base64 blocks that [`encode`] takes: it is long enough, and has a line of
/// [`MIN_LINE`] characters of an alphabet (where it would not take any).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= MIN.get() * 4 / 3 && worth_a_look(d) != 0
}

/// A block found: where it is in the file, how it is cut into lines and what it decodes to.
#[derive(Debug, Clone)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub first: usize,
    pub line: usize,
    /// The separator between lines (of the file), none where the block is of one line.
    pub sep: Option<(usize, usize)>,
    pub flags: u8,
    pub bytes: Vec<u8>,
}

/// The value of each character of an alphabet, 0xff for the others.
fn values(table: &[u8; 64]) -> [u8; 256] {
    let mut v = [0xffu8; 256];
    for (i, &c) in table.iter().enumerate() {
        v[c as usize] = i as u8;
    }
    v
}

/// The bytes of `chars` (of the alphabet of `value`) if they are a block's characters
/// that write it again as they are: no 4k+1 characters, and the bits of the last group
/// past the bytes zero.
fn decode_chars(chars: &[u8], value: &[u8; 256]) -> Option<Vec<u8>> {
    if chars.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(chars.len() / 4 * 3 + 2);
    let (groups, tail) = chars.as_chunks::<4>();
    for g in groups {
        let v = (value[g[0] as usize] as u32) << 18
            | (value[g[1] as usize] as u32) << 12
            | (value[g[2] as usize] as u32) << 6
            | value[g[3] as usize] as u32;
        out.extend_from_slice(&v.to_be_bytes()[1..]);
    }
    match *tail {
        [a, b] => {
            let v = (value[a as usize] as u32) << 6 | value[b as usize] as u32;
            if v & 0xf != 0 {
                return None;
            }
            out.push((v >> 4) as u8);
        }
        [a, b, c] => {
            let v = (value[a as usize] as u32) << 12 | (value[b as usize] as u32) << 6 | value[c as usize] as u32;
            if v & 0x3 != 0 {
                return None;
            }
            out.extend_from_slice(&[(v >> 10) as u8, (v >> 2) as u8]);
        }
        _ => {}
    }
    Some(out)
}

/// Whether the characters of a block are the kind of a base64 payload and not another string:
/// capitals, lowercase letters and digits all in them, and not hexadecimal digits alone.
fn looks_like_data(chars: &[u8]) -> bool {
    let (mut upper, mut lower, mut digit, mut hex) = (false, false, false, true);
    for &c in chars {
        upper |= c.is_ascii_uppercase();
        lower |= c.is_ascii_lowercase();
        digit |= c.is_ascii_digit();
        hex &= c.is_ascii_hexdigit();
    }
    upper && lower && digit && !hex
}

/// The characters at `at` of an alphabet's: how many.
fn run_len(d: &[u8], at: usize, bit: u8) -> usize {
    d[at..].iter().take_while(|&&c| CLASS[c as usize] & bit != 0).count()
}

/// The blocks of `d` of the alphabet of `url`, in order.
pub fn scan(d: &[u8], url: bool) -> Vec<Found> {
    let bit = if url { 2 } else { 1 };
    let value = values(if url { URL_SAFE } else { STANDARD });
    let min = MIN.get();
    let mut found = Vec::new();
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut chars: Vec<u8> = Vec::new();
    let mut p = 0;
    while p < d.len() {
        if CLASS[d[p] as usize] & bit == 0 {
            p += 1;
            continue;
        }
        let s0 = p;
        p += run_len(d, p, bit);
        let e = p;
        // (an escape before it, `\n`, is not of it)
        let s = match d[s0 - s0.min(1)..=s0] {
            [b'\\', b'n' | b'r' | b't'] => s0 + 1,
            _ => s0,
        };
        if e - s < MIN_LINE {
            continue;
        }
        // its lines
        lines.clear();
        lines.push((s, e - s));
        let mut walked = e;
        let mut pads = 0;
        let mut sep = None;
        let mut line_len = 0;
        // (a line with signs after it is the last)
        match separator(d, e, bit).filter(|_| pads_at(d, e) == 0) {
            None => {
                pads = pads_at(d, e);
                walked = e + pads;
            }
            Some(q) => {
                let n = q - e;
                sep = Some((e, n));
                let more = |after: usize| {
                    d[after..].starts_with(&d[e..q]) && d.get(after + n).is_some_and(|&c| CLASS[c as usize] & bit != 0)
                };
                let mut at = q;
                loop {
                    let l = run_len(d, at, bit);
                    let after = at + l;
                    let pd = pads_at(d, after);
                    if lines.len() == 1 {
                        line_len = if pd == 0 && more(after) { l } else { (e - s).max(l + pd) };
                    }
                    if l > line_len {
                        break;
                    }
                    lines.push((at, l));
                    walked = after;
                    if pd > 0 {
                        if l + pd <= line_len {
                            pads = pd;
                            walked = after + pd;
                        }
                        break;
                    }
                    if l == line_len && more(after) {
                        at = after + n;
                        continue;
                    }
                    break;
                }
            }
        }
        // (lines shorter than an encoder's are not of a block: the first line alone, then, as
        // `%2520` again and again after a long URL is not)
        if lines.len() > 1 && line_len < MIN_LINE {
            lines.truncate(1);
            (pads, walked, sep) = (0, e, None);
        }
        // the characters of the first `n` lines and `pd` signs: as a block, or not; the lines
        // as they are, else without the signs, else without the last line (where the text
        // goes on with something that is not of the block)
        let mut cands = vec![(lines.len(), pads)];
        if pads > 0 {
            cands.push((lines.len(), 0));
        }
        if lines.len() >= 2 {
            cands.push((lines.len() - 1, 0));
        }
        let mut chosen = None;
        for (n, pd) in cands {
            chars.clear();
            lines[..n].iter().for_each(|&(a, l)| chars.extend_from_slice(&d[a..a + l]));
            if pd > 0 && !(chars.len() + pd).is_multiple_of(4) {
                continue;
            }
            if let Some(bytes) = decode_chars(&chars, &value) {
                let (a, l) = lines[n - 1];
                chosen = Some((n, pd, a + l + pd, bytes));
                break;
            }
        }
        let ok = chosen.as_ref().is_some_and(|&(n, _, end, ref b)| {
            (n > 1 || delimited(d, s, end))
                && b.len() >= min
                && looks_like_data(&chars)
                && (!url || chars.iter().any(|&c| c == b'-' || c == b'_'))
        });
        let Some((n, pd, end, bytes)) = chosen.filter(|_| ok) else {
            p = p.max(walked);
            continue;
        };
        let single = n == 1;
        found.push(Found {
            start: s,
            end,
            first: if single { 0 } else { lines[0].1 },
            line: if single { 0 } else { line_len },
            sep: if single { None } else { sep },
            flags: ((pd > 0) as u8 * PADDED) | ((url as u8) << ALPHABET_SHIFT),
            bytes,
        });
        p = end;
    }
    found
}

/// Whether a block of one line at `s..e` is between the bytes that delimit a value in text:
/// quotes, `>` and `<`, `base64,`, `: ` and the end of a line, an escape of JSON; the start and
/// the end of the file. Not NULs: long C++ names between them (a string table of libstdc++) are
/// lines of the URL-safe alphabet, and a block of lines regular enough is found by its lines.
fn delimited(d: &[u8], s: usize, e: usize) -> bool {
    let before = match s {
        0 => true,
        _ if s >= 2 && d[s - 2] == b'\\' && matches!(d[s - 1], b'n' | b'r' | b't') => true,
        _ => matches!(d[s - 1], b',' | b'"' | b'\'' | b'>' | b':' | b'=' | b'(' | b' ' | b'\t' | b'\n' | b'\r'),
    };
    let after = d.get(e).is_none_or(|c| {
        matches!(c, b'"' | b'\'' | b'<' | b')' | b',' | b';' | b'&' | b'\\' | b' ' | b'\t' | b'\n' | b'\r')
    });
    before && after
}

/// The `=` signs at `at`, up to two.
fn pads_at(d: &[u8], at: usize) -> usize {
    d[at..].iter().take(2).take_while(|&&c| c == b'=').count()
}

/// The end of the separator after a line that ends at `e`: the bytes up to the next character
/// of the alphabet that are not of it (or are the `n`, `r` or `t` of an escape after a `\`), at
/// most [`MAX_SEP`] of them, and at least one, all printable ASCII, tabs and line breaks; None
/// where there is none.
fn separator(d: &[u8], e: usize, bit: u8) -> Option<usize> {
    let mut q = e;
    while q < d.len() && q - e <= MAX_SEP {
        let c = d[q];
        // (control bytes but tabs and line breaks, and bytes past ASCII, are not of one: a
        // string table's NULs between names)
        if c != b'\t' && c != b'\n' && c != b'\r' && !(0x20..0x7f).contains(&c) {
            return None;
        }
        if CLASS[c as usize] & bit == 0 || (q > e && d[q - 1] == b'\\' && matches!(c, b'n' | b'r' | b't')) {
            q += 1;
        } else {
            break;
        }
    }
    (q > e && q - e <= MAX_SEP && q < d.len() && CLASS[d[q] as usize] & bit != 0).then_some(q)
}

/// What of a file is left to be written: a [`Put`] that takes only the bytes of it, in order.
struct Same<'a>(&'a [u8]);

impl Put for Same<'_> {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        let rest = self.0.strip_prefix(d)?;
        self.0 = rest;
        Some(())
    }
}

/// The formula of text `d` with its base64 blocks cut out, or `None` where it has none that
/// this crate takes. The same whatever `threads` is.
pub fn encode(d: &[u8], _threads: usize) -> Option<Formula> {
    if d.len() < MIN.get() * 4 / 3 {
        return None;
    }
    let seen = worth_a_look(d);
    if seen == 0 {
        return None;
    }
    let mut runs = if seen & 1 != 0 { scan(d, false) } else { Vec::new() };
    if seen & 2 != 0 {
        // (where blocks of the two overlap, the later goes: below)
        runs.extend(scan(d, true));
        runs.sort_by_key(|r| r.start);
    }
    if runs.is_empty() {
        return None;
    }
    // the separators, each once
    let mut seps: Vec<&[u8]> = Vec::new();
    let mut taken = Vec::with_capacity(runs.len());
    let mut last = 0;
    for r in runs {
        let index = match r.sep {
            None => Some(0),
            Some((a, n)) => {
                let s = &d[a..a + n];
                match seps.iter().position(|&x| x == s) {
                    Some(i) => Some(i),
                    None if seps.len() < MAX_SEPS => {
                        seps.push(s);
                        Some(seps.len() - 1)
                    }
                    None => None,
                }
            }
        };
        // (those past the file's separators dropped, and those overlapping the one before)
        if let Some(i) = index
            && r.start >= last
        {
            last = r.end;
            taken.push((r, i));
        }
    }
    let decoded: usize = taken.iter().map(|(r, _)| r.bytes.len()).sum();
    if taken.is_empty() || decoded.saturating_mul(100) < d.len().saturating_mul(SHARE.get()) {
        return None;
    }
    let (mut text, mut data, mut layout) = (Vec::new(), Vec::new(), Vec::new());
    varint::put(&mut layout, seps.len() as u64);
    for s in &seps {
        varint::put(&mut layout, s.len() as u64);
        layout.extend_from_slice(s);
    }
    let mut at = 0;
    let mut lens = Vec::with_capacity(taken.len());
    for (r, i) in &taken {
        text.extend_from_slice(&d[at..r.start]);
        varint::put(&mut layout, (r.start - at) as u64);
        varint::put(&mut layout, r.bytes.len() as u64);
        varint::put(&mut layout, r.first as u64);
        varint::put(&mut layout, r.line as u64);
        varint::put(&mut layout, *i as u64);
        layout.push(r.flags);
        data.extend_from_slice(&r.bytes);
        lens.push(r.bytes.len() as u64);
        at = r.end;
    }
    text.extend_from_slice(&d[at..]);
    // (written again from them, or not taken: compared as it is written, not held, as `d` may
    // be a GB of text)
    let mut same = Same(d);
    let written =
        module::build(&mut Slice(&text), &mut Slice(&data), data.len() as u64, &mut Slice(&layout), &mut same);
    if written.ok()? != d.len() as u64 || !same.0.is_empty() {
        return None;
    }
    let mut f = Formula::default();
    let text = f.push_content("base64/text", Kind::Text, text);
    let Node::Slice { stream, .. } = f.push_content("base64/data", Kind::Unknown, data) else { unreachable!() };
    let layout = f.push("base64/layout", layout);
    // (a slice for each block of `APART` bytes or more, a content of its own; the blocks between
    // them joined, one content: a PEM bundle's certificates, each its own, cost more as contents
    // than they gain)
    let apart = APART.get();
    let mut blocks: Vec<(u64, u64)> = Vec::new();
    let mut start = 0;
    let mut joins = false;
    for n in lens {
        let big = n >= apart as u64;
        match blocks.last_mut() {
            Some(b) if joins && !big => b.1 += n,
            _ => blocks.push((start, n)),
        }
        joins = !big;
        start += n;
    }
    let blocks = blocks.into_iter().map(|(s, n)| Node::slice(stream, s, n)).collect();
    f.root = Node::apply(MODULE, Vec::new(), vec![text, Node::Concat(blocks), layout], d.len() as u64);
    Some(f)
}
