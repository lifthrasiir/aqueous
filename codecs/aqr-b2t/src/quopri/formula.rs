//! `b2t-recons-quopri` as a module of entry formulas (see aqr-formula): the text of a MIME
//! message without its quoted-printable bodies, the bytes they decode to, and the layout that
//! writes them again (FORMAT.md, "The module `b2t-recons-quopri`"). The text and each body's
//! bytes are contents, recompressed as what they are.
//!
//! The encoder finds bodies by the header that names the encoding ([`runs`]), decodes each, and
//! writes it again in the way of a canonical encoder along the text, noting where the text
//! differs ([`analyze`]); a body is taken where those exceptions are few.

use std::ops::Range;

use aqr_api::stream::Slice;
use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use super::module::{
    self, ESCAPE, LF, LOWER, MAX_LINE, MIN_LINE, SOFT, SOFT_END, START, canon_escape, canon_soft, is_break,
};
use crate::formula::{Same, apart_slices};

pub use super::module::decode;

pub const MODULE: Module = Module::new("b2t-recons-quopri", 1).with_plan(memory).with_contents(&[0, 1]);

/// The memory of the module: its initial memory, and its plan ([`module::plan`]).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    (u.inputs.len() == 3 && u.outputs.len() == 1).then(|| u.initial() + module::plan())
}

aqr_common::zopt! {
    /// The bytes that a quoted-printable body decodes to at least to be taken.
    pub static MIN: Opt<usize> =
        Opt::size("b2t:quopri-min", 256, "bytes a quoted-printable body decodes to at least");
    /// The bytes a body decodes to at least to be a content of its own (of bodies that are not
    /// text: those of text are one content, as aqr-wrt takes text of 256 KiB or more); the others
    /// between such bodies are one content together.
    pub static APART: Opt<usize> = Opt::size(
        "b2t:quopri-apart",
        16 << 10,
        "bytes a quoted-printable body decodes to at least to be recompressed apart",
    );
    /// The percent of a file that its bodies decode to at least for them to be taken (as base64's).
    pub static SHARE: Opt<usize> = Opt::value(
        "b2t:quopri-share",
        1,
        "percent of a file its quoted-printable bodies decode to at least to be taken",
    );
    /// The most exceptions of a body, in percent of its tokens: more is an encoder not worth
    /// modeling, and its layout would cost.
    pub static EXCEPTIONS: Opt<usize> = Opt::value(
        "b2t:quopri-exceptions",
        2,
        "most exceptions of a quoted-printable body's layout, in percent of its tokens",
    );
}

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 3] = ["text", "data", "layout"];

const CTE: &[u8] = b"content-transfer-encoding";
const QP: &[u8] = b"quoted-printable";

fn starts_with_ci(d: &[u8], p: &[u8]) -> bool {
    d.len() >= p.len() && d[..p.len()].eq_ignore_ascii_case(p)
}

/// Whether `d` has `QP` in any case.
fn mentions(d: &[u8]) -> bool {
    let mut at = 0;
    while let Some(i) = d[at..].iter().position(|&c| c | 0x20 == b'q') {
        if starts_with_ci(&d[at + i..], QP) {
            return true;
        }
        at += i + 1;
    }
    false
}

/// Whether `d` may have quoted-printable bodies that [`encode`] takes: it is long enough and
/// names the encoding in a header's way (a quick look: it is asked of every text).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= MIN.get() && mentions(d)
}

/// The end of the line at `p` (without its line break) and the start of the next.
fn line_at(d: &[u8], p: usize) -> (usize, usize) {
    match d[p..].iter().position(|&c| c == b'\n') {
        Some(i) => (if i > 0 && d[p + i - 1] == b'\r' { p + i - 1 } else { p + i }, p + i + 1),
        None => (d.len(), d.len()),
    }
}

/// Whether `line` is the header `Content-Transfer-Encoding: quoted-printable`.
fn is_qp_header(line: &[u8]) -> bool {
    if !starts_with_ci(line, CTE) {
        return false;
    }
    let rest = line[CTE.len()..].trim_ascii_start();
    let Some(rest) = rest.strip_prefix(b":") else { return false };
    let rest = rest.trim_ascii_start();
    starts_with_ci(rest, QP) && rest[QP.len()..].first().is_none_or(|c| c.is_ascii_whitespace() || *c == b';')
}

/// Whether `line` is a header line or a continuation of one: `Name:` or leading whitespace.
fn is_header_line(line: &[u8]) -> bool {
    match line.first() {
        Some(b' ' | b'\t') => true,
        Some(_) => {
            let name = line.iter().take_while(|&&c| c.is_ascii_graphic() && c != b':').count();
            name > 0 && line.get(name) == Some(&b':')
        }
        None => false,
    }
}

/// The boundaries that `Content-Type` headers name anywhere in `d`: `boundary="…"` or
/// `boundary=token`.
fn boundaries(d: &[u8]) -> Vec<Vec<u8>> {
    let mut found: Vec<Vec<u8>> = Vec::new();
    let mut at = 0;
    while let Some(i) = d[at..].iter().position(|&c| c | 0x20 == b'b') {
        let p = at + i;
        at = p + 1;
        if !starts_with_ci(&d[p..], b"boundary") {
            continue;
        }
        let rest = d[p + 8..].trim_ascii_start();
        let Some(rest) = rest.strip_prefix(b"=") else { continue };
        let rest = rest.trim_ascii_start();
        let value: &[u8] = match rest.first() {
            Some(b'"') => {
                let r = &rest[1..];
                &r[..r.iter().position(|&c| c == b'"' || c == b'\n').unwrap_or(0)]
            }
            _ => &rest[..rest.iter().take_while(|&&c| c.is_ascii_graphic() && c != b';' && c != b'"').count()],
        };
        if !value.is_empty() && value.len() <= 200 && !found.iter().any(|b| b == value) {
            found.push(value.to_vec());
        }
    }
    found
}

/// Where a body that starts at `start` ends: before the line break that precedes a line of
/// `--` and a boundary, else one of `From ` and a time (mbox), else at the end.
fn run_end(d: &[u8], start: usize, boundaries: &[Vec<u8>]) -> usize {
    let mut p = start;
    while p < d.len() {
        let line = &d[p..line_at(d, p).0];
        let ends = (line.starts_with(b"--") && boundaries.iter().any(|b| line[2..].starts_with(b)))
            || (line.starts_with(b"From ") && line.contains(&b':'));
        if ends {
            return match p == start {
                true => start,
                false if p >= 2 && d[p - 2] == b'\r' => p - 2,
                false => p - 1,
            };
        }
        p = line_at(d, p).1;
    }
    d.len()
}

/// The bodies of `d` that a header names as quoted-printable, in order: where each is.
pub fn runs(d: &[u8]) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut names: Option<Vec<Vec<u8>>> = None;
    let mut p = 0;
    while p < d.len() {
        let first = p;
        let (e, next) = line_at(d, first);
        p = next;
        if !is_qp_header(&d[first..e]) {
            continue;
        }
        // (the blank line that ends the header block: of lines that are headers)
        let mut q = next;
        let start = loop {
            if q >= d.len() {
                break None;
            }
            let (e2, n2) = line_at(d, q);
            if e2 == q {
                break Some(n2);
            }
            if !is_header_line(&d[q..e2]) {
                break None;
            }
            q = n2;
        };
        let Some(start) = start else { continue };
        let end = run_end(d, start, names.get_or_insert_with(|| boundaries(d)));
        if end > start {
            found.push(start..end);
            p = end.max(p);
        }
    }
    found
}

/// What an encoder's text of a body is, as the module writes it again from the bytes.
#[derive(Debug, Clone)]
pub struct Body {
    pub data: Vec<u8>,
    pub flags: u8,
    pub line: u64,
    /// The exceptions: a token's index and a kind ([`ESCAPE`], [`SOFT`]), in order.
    pub exceptions: Vec<(u64, u8)>,
    /// The tokens of the data (its bytes, a line break one).
    pub tokens: u64,
}

fn eol_len(t: &[u8], p: usize) -> Option<usize> {
    match t.get(p..)? {
        [b'\r', b'\n', ..] => Some(2),
        [b'\n', ..] => Some(1),
        _ => None,
    }
}

fn hex_value(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|v| v as u8)
}

/// The text `t` of a body decoded: its bytes, the line break it uses (CRLF: true), whether it
/// writes its escapes in lowercase; none where it is not quoted-printable that the module
/// writes (a `=` that is not followed by two hexadecimal digits or a line break, a lone CR, a
/// byte that is not printable ASCII, a tab, a space or a line break, mixed breaks or cases).
fn decode_text(t: &[u8]) -> Option<(Vec<u8>, bool, bool)> {
    let mut data = Vec::with_capacity(t.len());
    let (mut crlf, mut lower, mut upper) = (None, false, false);
    let mut marks = 0;
    let mut p = 0;
    while p < t.len() {
        let c = t[p];
        match c {
            b'=' => match (t.get(p + 1).copied().and_then(hex_value), t.get(p + 2).copied().and_then(hex_value)) {
                (Some(a), Some(b)) => {
                    for &x in &t[p + 1..p + 3] {
                        lower |= x.is_ascii_lowercase();
                        upper |= x.is_ascii_uppercase();
                    }
                    data.push(a << 4 | b);
                    p += 3;
                    marks += 1;
                }
                _ => {
                    let n = eol_len(t, p + 1)?;
                    if *crlf.get_or_insert(n == 2) != (n == 2) {
                        return None;
                    }
                    p += 1 + n;
                    marks += 1;
                }
            },
            b'\r' | b'\n' => {
                let n = eol_len(t, p)?;
                if *crlf.get_or_insert(n == 2) != (n == 2) {
                    return None;
                }
                data.extend_from_slice(&t[p..p + n]);
                p += n;
            }
            b' ' | b'\t' => {
                data.push(c);
                p += 1;
            }
            _ if module::literal(c) => {
                data.push(c);
                p += 1;
            }
            _ => return None,
        }
    }
    (marks > 0 && !(lower && upper)).then_some((data, crlf.unwrap_or(false), lower))
}

/// The longest line of `t` (as a text, with a soft break's `=`), of those that end in a soft
/// break if there are such.
fn longest_lines(t: &[u8]) -> (usize, usize) {
    let (mut soft, mut all) = (0, 0);
    let mut p = 0;
    while p < t.len() {
        let (e, next) = line_at(t, p);
        all = all.max(e - p);
        if t[p..e].ends_with(b"=") {
            soft = soft.max(e - p);
        }
        p = next;
    }
    (soft, all)
}

/// The exceptions of a body, whether it ends in a soft break, and its tokens.
type Simulated = (Vec<(u64, u8)>, bool, u64);

/// The text of a body written again by the module's rules along `t`, with `data` its bytes: the
/// exceptions, and whether it ends in a soft break; none where `t` is not of the tokens.
///
/// Its caller stops it (none) where it has more than `most` exceptions.
fn simulate(t: &[u8], data: &[u8], flags: u8, line: u64, most: usize) -> Option<Simulated> {
    let (crlf, lower) = (flags & LF == 0, flags & LOWER != 0);
    let digits: &[u8; 16] = if lower { b"0123456789abcdef" } else { b"0123456789ABCDEF" };
    let escape_at = |p: usize, byte: u8| {
        t.get(p..p + 3) == Some(&[b'=', digits[(byte >> 4) as usize], digits[(byte & 15) as usize]])
    };
    let soft_at = |p: usize| match t.get(p) {
        Some(b'=') => eol_len(t, p + 1).map(|n| 1 + n),
        _ => None,
    };
    let (mut p, mut col, mut index, mut k) = (0usize, 0u64, 0u64, 0usize);
    let mut exceptions = Vec::new();
    while k < data.len() {
        let rest = &data[k..];
        let is_brk = is_break(rest, crlf);
        let len = if is_brk && crlf { 2 } else { 1 };
        let before_end = rest.len() == len || is_break(&rest[len..], crlf);
        let soft = match soft_at(p) {
            Some(n) => {
                p += n;
                // (two in a row: not an encoder's)
                if soft_at(p).is_some() {
                    return None;
                }
                true
            }
            None => false,
        };
        let byte = rest[0];
        let (esc, w, canon_esc, canon_soft_) = if is_brk {
            let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
            let esc = if t.get(p..).is_some_and(|r| r.starts_with(eol)) {
                p += eol.len();
                false
            } else if rest[..len].iter().enumerate().all(|(i, &x)| escape_at(p + 3 * i, x)) {
                p += 3 * len;
                true
            } else {
                return None;
            };
            (esc, if esc { 3 * len as u64 } else { 0 }, false, false)
        } else {
            let esc = match t.get(p)? {
                b'=' => {
                    if !escape_at(p, byte) {
                        return None;
                    }
                    p += 3;
                    true
                }
                &c => {
                    if c != byte {
                        return None;
                    }
                    p += 1;
                    false
                }
            };
            let w = if esc { 3 } else { 1 };
            (esc, w, canon_escape(byte, before_end), canon_soft(col, w, before_end, line, flags & START != 0))
        };
        if esc != canon_esc {
            exceptions.push((index, ESCAPE));
        }
        if soft != canon_soft_ {
            exceptions.push((index, SOFT));
        }
        if exceptions.len() > most {
            return None;
        }
        if soft {
            col = 0;
        }
        match (is_brk, esc) {
            (true, false) => col = 0,
            _ => col += w,
        }
        k += len;
        index += 1;
    }
    let soft_end = match t.get(p..) {
        Some([]) => false,
        Some(r) if soft_at(p) == Some(r.len()) => true,
        _ => return None,
    };
    Some((exceptions, soft_end, index))
}

/// The body whose text is `t`, or none where it is not taken: not quoted-printable that the
/// module writes, or an encoder with more exceptions than [`EXCEPTIONS`] lets.
pub fn analyze(t: &[u8]) -> Option<Body> {
    let (data, crlf, lower) = decode_text(t)?;
    let (soft, all) = longest_lines(t);
    // (the line lengths of encoders: the longest of the text's, 76 as RFC 2045 has it, 78 of
    // Python's email policies, the others a little less, as Ruby's)
    let derived = if soft > 0 { soft } else { all.max(76) } as u64;
    let mut lines = vec![derived, 76, 78, 75, 74, 73, 72, 70, 68, 64, 60];
    lines.retain(|l| (MIN_LINE..=MAX_LINE).contains(l));
    let base = (!crlf as u8 * LF) | (lower as u8 * LOWER);
    let most = (data.len() as u64 * EXCEPTIONS.get() as u64 / 100) as usize;
    let mut best: Option<(u8, u64, Simulated)> = None;
    'search: for start in [0, START] {
        for &line in &lines {
            // (the first of those with the fewest exceptions: one fewer than the best's is the most)
            let most = match &best {
                Some((.., (e, ..))) if e.is_empty() => break 'search,
                Some((.., (e, ..))) => most.min(e.len() - 1),
                None => most,
            };
            if let Some(r) = simulate(t, &data, base | start, line, most) {
                best = Some((base | start, line, r));
            }
        }
    }
    let (flags, line, (exceptions, soft_end, tokens)) = best?;
    // (the decoded bytes are an upper bound of the tokens: a line break is one for two bytes)
    if exceptions.len() as u64 * 100 > tokens * EXCEPTIONS.get() as u64 {
        return None;
    }
    let flags = flags | (soft_end as u8 * SOFT_END);
    Some(Body { data, flags, line, exceptions, tokens })
}

/// Whether `d` is text: nearly all of its bytes are printable ASCII, white space or part of a
/// multi-byte character (at most 1 in 64 a control byte or a NUL).
fn looks_like_text(d: &[u8]) -> bool {
    let controls = d.iter().filter(|&&c| c < 0x20 && !matches!(c, b'\t' | b'\n' | b'\r' | 0x0c) || c == 0x7f).count();
    controls * 64 <= d.len()
}

/// The formula of text `d` with its quoted-printable bodies cut out, or `None` where it has none
/// that this crate takes. The same whatever `threads` is.
pub fn encode(d: &[u8], _threads: usize) -> Option<Formula> {
    if !starts(d) {
        return None;
    }
    let mut taken = Vec::new();
    for r in runs(d) {
        if let Some(b) = analyze(&d[r.clone()]).filter(|b| b.data.len() >= MIN.get()) {
            taken.push((r, b));
        }
    }
    let decoded: usize = taken.iter().map(|(_, b)| b.data.len()).sum();
    if taken.is_empty() || decoded.saturating_mul(100) < d.len().saturating_mul(SHARE.get()) {
        return None;
    }
    let (mut text, mut data, mut layout) = (Vec::new(), Vec::new(), Vec::new());
    let mut at = 0;
    let mut lens = Vec::with_capacity(taken.len());
    for (r, b) in &taken {
        text.extend_from_slice(&d[at..r.start]);
        varint::put(&mut layout, (r.start - at) as u64);
        varint::put(&mut layout, b.data.len() as u64);
        layout.push(b.flags);
        varint::put(&mut layout, b.line);
        varint::put(&mut layout, b.exceptions.len() as u64);
        let mut last = 0;
        for &(i, kind) in &b.exceptions {
            varint::put(&mut layout, (i - last) << 1 | kind as u64);
            last = i;
        }
        data.extend_from_slice(&b.data);
        lens.push(b.data.len() as u64);
        at = r.end;
    }
    text.extend_from_slice(&d[at..]);
    // (written again from them, or not taken)
    let mut same = Same(d);
    let written =
        module::build(&mut Slice(&text), &mut Slice(&data), data.len() as u64, &mut Slice(&layout), &mut same);
    if written.ok()? != d.len() as u64 || !same.0.is_empty() {
        return None;
    }
    let mut f = Formula::default();
    let text = f.push_content("b2t-quopri/text", Kind::Text, text);
    // (the bodies of mail are text of a charset, nearly always: recompressed as text, where the
    // bytes are not that, as the file they are)
    let kind = if looks_like_text(&data) { Kind::Text } else { Kind::Unknown };
    let Node::Slice { stream, .. } = f.push_content("b2t-quopri/data", kind, data) else { unreachable!() };
    let layout = f.push("b2t-quopri/layout", layout);
    // (text all in one content: aqr-wrt takes it of 256 KiB or more, and the bodies of a mailbox
    // are of one kind of words and markup; bodies that are not text apart, as base64's are)
    let blocks = match kind {
        Kind::Text => vec![Node::slice(stream, 0, lens.iter().sum())],
        _ => apart_slices(stream, &lens, APART.get()),
    };
    f.root = Node::apply(MODULE, Vec::new(), vec![text, Node::Concat(blocks), layout], d.len() as u64);
    Some(f)
}
