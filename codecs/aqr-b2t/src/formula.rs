//! aqr-b2t as a module of entry formulas (see aqr-formula): the text of a file without its
//! base64 blocks, the bytes they decode to, and the layout that writes them again (FORMAT.md).
//! The text and each block's bytes are contents, recompressed as what they are.
//!
//! The encoder goes over the file once ([`scan`]): a block is found by its lines (an alphabet's
//! characters, a separator, lines of one length), and taken only where writing it again from its
//! bytes gives its bytes back.

use aqr_api::stream::{Put, Slice};
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::module::{self, ALPHABET_SHIFT, HEX_LOWER, HEX_UPPER, MAX_SEP, MAX_SEPS, PADDED, STANDARD, URL_SAFE};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("b2t-recons", 1).with_plan(memory).with_contents(&[0, 1]);

/// The memory of the module: its initial memory, and its plan ([`module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    (u.inputs.len() == 3 && u.outputs.len() == 1).then(|| u.initial() + module::plan())
}

aqr_common::zopt! {
    /// The bytes that a base64 block decodes to at least to be taken.
    pub static MIN: Opt<usize> = Opt::size("b2t:min", 192, "bytes a base64 block decodes to at least");
    /// The bytes a block decodes to at least to be a content of its own; the others between
    /// such blocks are one content together.
    pub static APART: Opt<usize> =
        Opt::size("b2t:apart", 16 << 10, "bytes a base64 block decodes to at least to be recompressed apart");
    /// Whether hexadecimal blocks are taken (base64's are in any case).
    pub static HEX: Opt<bool> = Opt::flag("b2t:hex", true, "take hexadecimal blocks");
    /// The percent of a file that its blocks decode to at least for them to be taken: the
    /// formula copies all of the text, which for a few blocks in much text (enwik9: 4 blocks
    /// of 1,883 bytes in 1 GB) costs the copy and gains nothing.
    pub static SHARE: Opt<usize> =
        Opt::value("b2t:share", 1, "percent of a file its base64 blocks decode to at least to be taken");
}

/// The least characters of a line that a block may start with: a block starts at a line that
/// long, and a file with none is not looked at further ([`worth_a_look`]).
pub const MIN_LINE: usize = 40;

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 3] = ["text", "data", "layout"];

/// The alphabets a run may be of: the flags' alphabet index (`module`) is the discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alpha {
    Standard = 0,
    UrlSafe = 1,
    HexLower = 2,
    HexUpper = 3,
}

impl Alpha {
    pub const ALL: [Alpha; 4] = [Alpha::Standard, Alpha::UrlSafe, Alpha::HexLower, Alpha::HexUpper];

    /// The bit of the alphabet in [`CLASS`] (and in what [`worth_a_look`] gives).
    const fn bit(self) -> u8 {
        1 << self as u8
    }

    fn is_hex(self) -> bool {
        self as u8 >= module::FIRST_HEX
    }

    /// The value of each character of the alphabet, 0xff for the others.
    fn values(self) -> [u8; 256] {
        let table: &[u8] = match self {
            Alpha::Standard => STANDARD,
            Alpha::UrlSafe => URL_SAFE,
            Alpha::HexLower => HEX_LOWER,
            Alpha::HexUpper => HEX_UPPER,
        };
        let mut v = [0xffu8; 256];
        for (i, &c) in table.iter().enumerate() {
            v[c as usize] = i as u8;
        }
        v
    }
}

/// What a byte is of the alphabets: bit 0 of the standard's, bit 1 of the URL-safe's, bit 2 of
/// the lowercase hexadecimal's (`0-9a-f`), bit 3 of the uppercase's.
const fn classes() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let alnum = b.is_ascii_alphanumeric();
        t[i] = (alnum || b == b'+' || b == b'/') as u8
            | ((alnum || b == b'-' || b == b'_') as u8) << 1
            | ((b.is_ascii_digit() || matches!(b, b'a'..=b'f')) as u8) << 2
            | ((b.is_ascii_digit() || matches!(b, b'A'..=b'F')) as u8) << 3;
        i += 1;
    }
    t
}
static CLASS: [u8; 256] = classes();

/// What `d` has of the alphabets: the bit of each ([`Alpha::bit`]) of which there is a line of
/// [`MIN_LINE`] characters (for hexadecimal, of the characters of 2 bytes' worth each).
fn worth_a_look(d: &[u8]) -> u8 {
    if d.len() <= CHUNK {
        return worth_a_look_in(d);
    }
    // (each chunk with the characters after it that a line starting in it may take; none
    // looked at once all are found, as the serial loop stops)
    use std::sync::atomic::{AtomicU8, Ordering};
    let found = AtomicU8::new(0);
    let chunks = d.len().div_ceil(CHUNK);
    aqr_common::pool::par_map(chunks, aqr_common::pool::size(), |i| {
        if found.load(Ordering::Relaxed) != 15 {
            let f = worth_a_look_in(&d[i * CHUNK..((i + 1) * CHUNK + MIN_LINE - 1).min(d.len())]);
            found.fetch_or(f, Ordering::Relaxed);
        }
    });
    found.into_inner()
}

/// The bytes of a piece of the file that [`worth_a_look`] and [`long_runs`] look at on a thread.
const CHUNK: usize = 1 << 20;

/// [`worth_a_look`] of `d` on this thread.
fn worth_a_look_in(d: &[u8]) -> u8 {
    let mut n = [0usize; 4];
    let mut found = 0u8;
    for &c in d {
        let k = CLASS[c as usize];
        for (i, n) in n.iter_mut().enumerate() {
            *n = if k >> i & 1 != 0 { *n + 1 } else { 0 };
            if *n >= MIN_LINE {
                found |= 1 << i;
            }
        }
        if found == 15 {
            break;
        }
    }
    found
}

/// Whether `d` may have blocks that [`encode`] takes: it is long enough, and has a line of
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

/// The bytes of `chars` (of the alphabet of `value`, hexadecimal) if there is an even number of
/// them.
fn decode_hex(chars: &[u8], value: &[u8; 256]) -> Option<Vec<u8>> {
    chars
        .len()
        .is_multiple_of(2)
        .then(|| chars.as_chunks::<2>().0.iter().map(|p| value[p[0] as usize] << 4 | value[p[1] as usize]).collect())
}

/// Whether the characters of a block are the kind of a payload and not another string. Base64:
/// capitals, lowercase letters and digits all in them, and not hexadecimal digits alone (with
/// the `x` of a prefix, `0x` or `\x`, before them: those are of the hexadecimal scans).
/// Hexadecimal: digits and letters both (a number of decimal digits, or a word, is not data).
fn looks_like_data(chars: &[u8], hex: bool) -> bool {
    let (mut upper, mut lower, mut digit, mut hexdigits) = (false, false, false, true);
    for &c in chars {
        upper |= c.is_ascii_uppercase();
        lower |= c.is_ascii_lowercase();
        digit |= c.is_ascii_digit();
    }
    let body = [&b"0x"[..], b"0X", b"x", b"X"].iter().find_map(|p| chars.strip_prefix(*p)).unwrap_or(chars);
    for &c in body {
        hexdigits &= c.is_ascii_hexdigit();
    }
    match hex {
        true => digit && (upper || lower),
        false => upper && lower && digit && !hexdigits,
    }
}

/// The characters at `at` of an alphabet's: how many.
fn run_len(d: &[u8], at: usize, bit: u8) -> usize {
    d[at..].iter().take_while(|&&c| CLASS[c as usize] & bit != 0).count()
}

/// Of each alphabet (by [`Alpha`]'s index), where the runs of `d` of its characters that are of
/// [`MIN_LINE`] at least start (each as long as it goes: after a character not of it, or the
/// file's start), in order; found in one pass, a [`CHUNK`] at a time on the threads, each the
/// runs that start in it.
pub fn long_runs(d: &[u8]) -> [Vec<usize>; 4] {
    let runs_in = |a: usize, b: usize| {
        let mut runs: [Vec<usize>; 4] = Default::default();
        // the start of each alphabet's run going on; none (`usize::MAX`) for a run started
        // before `a`, which is not this chunk's
        const NONE: usize = usize::MAX;
        let mut start = [NONE; 4];
        let before = if a > 0 { CLASS[d[a - 1] as usize] } else { 0 };
        let mut mine = [0, 1, 2, 3].map(|i| before >> i & 1 == 0);
        let mut p = a;
        while p <= d.len() {
            let k = if p < d.len() { CLASS[d[p] as usize] } else { 0 };
            for i in 0..4 {
                if k >> i & 1 != 0 {
                    if start[i] == NONE && mine[i] && p < b {
                        start[i] = p;
                    }
                } else {
                    mine[i] = true;
                    if start[i] != NONE {
                        if p - start[i] >= MIN_LINE {
                            runs[i].push(start[i]);
                        }
                        start[i] = NONE;
                    }
                }
            }
            p += 1;
            // (past the chunk, the runs started in it followed to their ends)
            if p >= b && start == [NONE; 4] {
                break;
            }
        }
        runs
    };
    if d.len() <= CHUNK {
        return runs_in(0, d.len());
    }
    let chunks = d.len().div_ceil(CHUNK);
    let all = aqr_common::pool::par_map(chunks, aqr_common::pool::size(), |i| {
        runs_in(i * CHUNK, ((i + 1) * CHUNK).min(d.len()))
    });
    [0, 1, 2, 3].map(|a| all.iter().flat_map(|r| r[a].iter().copied()).collect())
}

/// The blocks of `d` of the alphabet `alpha`, in order.
pub fn scan(d: &[u8], alpha: Alpha) -> Vec<Found> {
    scan_from(d, alpha, &long_runs(d)[alpha as usize])
}

/// [`scan`] of `d`, of the runs of `alpha` that [`long_runs`] finds in it: where a block may
/// start, but where the one before it leaves off (a character not of the alphabet is followed by
/// a whole run, which is a block's start only if long enough).
fn scan_from(d: &[u8], alpha: Alpha, runs: &[usize]) -> Vec<Found> {
    let bit = alpha.bit();
    let mut next = 0;
    let (hex, url) = (alpha.is_hex(), alpha == Alpha::UrlSafe);
    let value = alpha.values();
    let min = MIN.get();
    let mut found = Vec::new();
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut chars: Vec<u8> = Vec::new();
    let mut p = 0;
    while p < d.len() {
        if CLASS[d[p] as usize] & bit == 0 {
            while runs.get(next).is_some_and(|&r| r < p) {
                next += 1;
            }
            match runs.get(next) {
                Some(&r) => p = r,
                None => break,
            }
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
        let pads_at = |d: &[u8], at: usize| if hex { 0 } else { pads_at(d, at) };
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
        // goes on with something that is not of the block); hexadecimal of an odd number of
        // characters gives the last back to the text
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
            let (a, l) = lines[n - 1];
            let mut back = 0;
            if hex && chars.len() % 2 == 1 {
                // (not a line of that character alone: the separator before it would end the block)
                if l == 1 {
                    continue;
                }
                chars.pop();
                back = 1;
            }
            let bytes = if hex { decode_hex(&chars, &value) } else { decode_chars(&chars, &value) };
            if let Some(bytes) = bytes {
                chosen = Some((n, pd, a + l + pd - back, bytes));
                break;
            }
        }
        let ok = chosen.as_ref().is_some_and(|&(n, _, end, ref b)| {
            (n > 1 || (if hex { hex_prefixed(d, s) && delimited_after(d, end) } else { delimited(d, s, end) }))
                && b.len() >= min
                && looks_like_data(&chars, hex)
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
            flags: ((pd > 0) as u8 * PADDED) | ((alpha as u8) << ALPHABET_SHIFT),
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
    before && delimited_after(d, e)
}

/// Whether the byte at `e`, after a block of one line, delimits a value in text (or it is the end
/// of the file).
fn delimited_after(d: &[u8], e: usize) -> bool {
    d.get(e).is_none_or(|c| {
        matches!(c, b'"' | b'\'' | b'<' | b')' | b',' | b';' | b'&' | b'\\' | b' ' | b'\t' | b'\n' | b'\r')
    })
}

/// Whether a hexadecimal block of one line at `s` follows the prefix of a literal: the `x` of
/// `0x` (MySQL's `--hex-blob`, C), `\x` (PostgreSQL's bytea, in a string or in COPY's text) or
/// `X'` (SQLite's `.dump`, SQL's).
fn hex_prefixed(d: &[u8], s: usize) -> bool {
    s >= 2 && matches!(&d[s - 2..s], b"0x" | b"0X" | b"\\x" | b"X'" | b"x'")
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
pub(crate) struct Same<'a>(pub(crate) &'a [u8]);

impl Put for Same<'_> {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        let rest = self.0.strip_prefix(d)?;
        self.0 = rest;
        Some(())
    }
}

/// The formula of text `d` with its base64 and hexadecimal blocks cut out, or `None` where it has none that
/// this crate takes. The same whatever `threads` is.
pub fn encode(d: &[u8], _threads: usize) -> Option<Formula> {
    if d.len() < MIN.get() * 4 / 3 {
        return None;
    }
    let runs = long_runs(d);
    let seen = Alpha::ALL.iter().filter(|a| !runs[**a as usize].is_empty()).fold(0, |s, a| s | a.bit());
    if seen == 0 {
        return None;
    }
    // (where blocks of two alphabets overlap, the later goes: below; of the same start, the
    // alphabet of the lower index, as the sort is stable. A block of hexadecimal digits alone is
    // never taken as base64: its scan turns it down)
    let mut runs: Vec<Found> = Alpha::ALL
        .iter()
        .filter(|a| seen & a.bit() != 0 && (!a.is_hex() || HEX.get()))
        .flat_map(|&a| scan_from(d, a, &runs[a as usize]))
        .collect();
    runs.sort_by_key(|r| r.start);
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
    let text = f.push_content("b2t/text", Kind::Text, text);
    let Node::Slice { stream, .. } = f.push_content("b2t/data", Kind::Unknown, data) else { unreachable!() };
    let layout = f.push("b2t/layout", layout);
    let blocks = apart_slices(stream, &lens, APART.get());
    f.root = Node::apply(MODULE, Vec::new(), vec![text, Node::Concat(blocks), layout], d.len() as u64);
    Some(f)
}

/// The slices of the stream `stream`, of the bytes of `lens` one after another: a slice for each
/// of `apart` bytes or more, a content of its own; the smaller ones between them joined, one
/// content (a PEM bundle's certificates, each its own, cost more as contents than they gain).
pub(crate) fn apart_slices(stream: usize, lens: &[u64], apart: usize) -> Vec<Node> {
    let mut blocks: Vec<(u64, u64)> = Vec::new();
    let mut start = 0;
    let mut joins = false;
    for &n in lens {
        let big = n >= apart as u64;
        match blocks.last_mut() {
            Some(b) if joins && !big => b.1 += n,
            _ => blocks.push((start, n)),
        }
        joins = !big;
        start += n;
    }
    blocks.into_iter().map(|(s, n)| Node::slice(stream, s, n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text of a few MiB, with runs of every length about [`MIN_LINE`] and base64 blocks of
    /// lines across the edges of [`CHUNK`].
    fn text() -> Vec<u8> {
        let mut d = Vec::new();
        let mut x = 1u32;
        let mut block = |d: &mut Vec<u8>| {
            for _ in 0..8 {
                for _ in 0..76 {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    d.push(STANDARD[(x >> 16) as usize % 64]);
                }
                d.push(b'\n');
            }
        };
        let mut n = 0;
        while d.len() < 3 * CHUNK + 5000 {
            // (a block ending right after each edge, another starting right before it)
            let edge = (d.len() / CHUNK + 1) * CHUNK;
            if edge - d.len() < 1000 {
                d.resize(edge - 300, b' ');
                block(&mut d);
                d.resize(edge + 2000 - 7 * 77, b' ');
                block(&mut d);
            }
            d.extend(std::iter::repeat_n(b'a', 30 + n % 20));
            d.extend_from_slice(b" some words, ");
            n += 1;
        }
        d
    }

    #[test]
    fn long_runs_by_chunks_are_those_of_one_pass() {
        let d = text();
        for a in Alpha::ALL {
            let bit = a.bit();
            let mut serial = Vec::new();
            let mut p = 0;
            while p < d.len() {
                let l = run_len(&d, p, bit);
                if l >= MIN_LINE {
                    serial.push(p);
                }
                p += l.max(1);
            }
            assert_eq!(long_runs(&d)[a as usize], serial, "{a:?}");
        }
        let seen = (0..d.len()).fold(0u8, |s, i| {
            s | Alpha::ALL
                .iter()
                .filter(|a| d[i..].len() >= MIN_LINE && run_len(&d[..i + MIN_LINE], i, a.bit()) >= MIN_LINE)
                .fold(0, |s, a| s | a.bit())
        });
        assert_eq!(worth_a_look(&d), seen);
    }

    #[test]
    fn blocks_across_chunks_are_found() {
        let d = text();
        let found = scan(&d, Alpha::Standard);
        let starts: Vec<usize> = found.iter().map(|f| f.start).collect();
        let edges: Vec<usize> = (1..=3).map(|i| i * CHUNK).collect();
        for e in edges {
            assert!(starts.contains(&(e - 300)), "the block before {e}: {starts:?}");
            assert!(starts.contains(&(e + 2000 - 7 * 77)), "the block about {e}: {starts:?}");
        }
        assert_eq!(found.len(), 6);
    }
}
