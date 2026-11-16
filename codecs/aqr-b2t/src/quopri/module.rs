//! The code of the module `b2t-recons-quopri` (see `formula`, and `FORMAT.md`, "The module
//! `b2t-recons-quopri`"), which needs no aqr-formula, so that aqr-modules builds it into its Wasm
//! module. A part module: the text, the decoded bytes and the layout are taken in order through
//! windows and the file put out in order, so that nothing is held of their lengths.

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};

use crate::module::{Emit, OUT, fill};
use crate::{Error, Result, corrupt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 4;

/// The flags of a run: its line breaks are LF, not CRLF; its escapes are written in lowercase
/// hexadecimal; it ends in a soft line break; its encoder breaks a line by where a token starts,
/// not by where it ends (Ruby's `pack("M")`).
pub const LF: u8 = 1;
pub const LOWER: u8 = 2;
pub const SOFT_END: u8 = 4;
pub const START: u8 = 8;
const FLAGS: u8 = LF | LOWER | SOFT_END | START;

/// The kinds of an exception: the escape decision of a token flipped, or the soft break's.
pub const ESCAPE: u8 = 0;
pub const SOFT: u8 = 1;

/// The least and the most a run's line length may be (with the soft break's `=`).
pub const MIN_LINE: u64 = 4;
pub const MAX_LINE: u64 = 1 << 16;

/// Bytes of the layout read at a time.
const LAYOUT_PIECE: usize = 4096;
/// Bytes of text or decoded data taken at a time, and the bytes after the next token that are
/// looked at.
const PIECE: usize = 24576;
const LOOK: usize = 4;

/// The memory plan of the module ([`aqr_api::stack`]): what [`decode_part`] takes beyond its
/// initial memory at most, whatever the lengths: the buffers it lays out, in the order of
/// [`build`].
pub fn plan() -> u64 {
    use aqr_api::stack::block;
    block(LAYOUT_PIECE as u64) + block((PIECE + LOOK) as u64) + block(OUT as u64)
}

/// Whether a byte of the data is written as it is where it is not before a line's end: the
/// printable ASCII but `=`.
pub const fn literal(b: u8) -> bool {
    matches!(b, 33..=60 | 62..=126)
}

/// Whether a canonical encoder escapes `byte` (not a line break): all but the printable ASCII,
/// and a space or tab before a line's end (`before_end`: a line break follows, or the run ends).
pub const fn canon_escape(byte: u8, before_end: bool) -> bool {
    let ws = byte == b' ' || byte == b'\t';
    !(literal(byte) || ws) || (ws && before_end)
}

/// Whether a canonical encoder breaks the line before a token of width `w` at column `col`: where
/// it would not fit with the `=` of a soft break after it (`line` is the length of a line with
/// it), or the line's last (`before_end`), with no room needed. Where `start`, the token's
/// width is taken as 1, so that a line breaks only where a token starts past its limit.
pub const fn canon_soft(col: u64, w: u64, before_end: bool, line: u64, start: bool) -> bool {
    col + if start { 1 } else { w } > if before_end { line } else { line - 1 }
}

/// The data of a run, in pieces, with the bytes after the one looked at.
struct Data<'a> {
    src: &'a mut dyn Pull,
    buf: Vec<u8>,
    at: usize,
    end: usize,
    /// The bytes of the run not yet in the buffer.
    unread: u64,
}

impl Data<'_> {
    /// The bytes of the run left, in the buffer or not.
    fn remaining(&self) -> u64 {
        (self.end - self.at) as u64 + self.unread
    }

    /// Makes the buffer have at least `LOOK` bytes from `at`, or all that is left.
    fn ensure(&mut self) -> Result<()> {
        if self.end - self.at >= LOOK || self.unread == 0 {
            return Ok(());
        }
        self.buf.copy_within(self.at..self.end, 0);
        self.end -= self.at;
        self.at = 0;
        let k = ((self.buf.len() - self.end) as u64).min(self.unread) as usize;
        fill(self.src, &mut self.buf[self.end..self.end + k])?;
        self.end += k;
        self.unread -= k as u64;
        Ok(())
    }
}

/// Whether a line break is at the start of `b`.
pub fn is_break(b: &[u8], crlf: bool) -> bool {
    match crlf {
        true => b.starts_with(b"\r\n"),
        false => b.first() == Some(&b'\n'),
    }
}

/// The exceptions of a run, read from the layout as they are needed: each a token's index
/// (a delta from the one before) and a kind.
struct Exceptions {
    left: u64,
    last: Option<(u64, u8)>,
}

impl Exceptions {
    /// The next exception, or none.
    fn next(&mut self, layout: &mut Reader) -> Result<Option<(u64, u8)>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let v = layout.num().map_err(|_| corrupt())?;
        let (delta, kind) = (v >> 1, (v & 1) as u8);
        let pos = match self.last {
            None => delta,
            // (strictly after the one before: the same token's escape, then its soft break)
            Some((p, k)) if delta > 0 || (k == ESCAPE && kind == SOFT) => p.checked_add(delta).ok_or_else(corrupt)?,
            Some(_) => return Err(Error("exceptions out of order")),
        };
        self.last = Some((pos, kind));
        Ok(self.last)
    }
}

/// The file of the three inputs, put to `out`; the bytes put. `data_len` is the length of `data`.
pub fn build(
    text: &mut dyn Pull,
    data: &mut dyn Pull,
    data_len: u64,
    layout: &mut dyn Pull,
    out: &mut dyn Put,
) -> Result<u64> {
    let mut layout = Reader::new(layout, LAYOUT_PIECE);
    let mut d = Data { src: data, buf: vec![0u8; PIECE + LOOK], at: 0, end: 0, unread: 0 };
    let mut e = Emit::new(out);
    let c = |_| corrupt();
    let mut data_left = data_len;
    while !layout.done() {
        let gap = layout.num().map_err(c)?;
        let n = layout.num().map_err(c)?;
        let flags = layout.byte().ok_or_else(corrupt)?;
        let line = layout.num().map_err(c)?;
        let count = layout.num().map_err(c)?;
        // (text before it; the buffer is free between runs)
        let mut gap = gap;
        while gap > 0 {
            let k = gap.min(PIECE as u64) as usize;
            fill(text, &mut d.buf[..k])?;
            e.put(&d.buf[..k])?;
            gap -= k as u64;
        }
        if flags & !FLAGS != 0 {
            return Err(Error("unknown flags of a run"));
        }
        if n == 0 || n > data_left {
            return Err(Error("run of bytes the data does not have"));
        }
        if !(MIN_LINE..=MAX_LINE).contains(&line) {
            return Err(Error("line length out of range"));
        }
        data_left -= n;
        (d.at, d.end, d.unread) = (0, 0, n);
        let crlf = flags & LF == 0;
        let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
        let digits: &[u8; 16] = if flags & LOWER != 0 { b"0123456789abcdef" } else { b"0123456789ABCDEF" };
        let mut exc = Exceptions { left: count, last: None };
        let mut next = exc.next(&mut layout)?;
        let (mut col, mut index) = (0u64, 0u64);
        while d.remaining() > 0 {
            d.ensure()?;
            let b = &d.buf[d.at..d.end];
            let is_brk = is_break(b, crlf);
            let len = if is_brk && crlf { 2 } else { 1 };
            let after = d.remaining() - len as u64;
            let before_end = after == 0 || is_break(&b[len..], crlf);
            let (mut flip_esc, mut flip_soft) = (false, false);
            while let Some((p, k)) = next
                && p == index
            {
                match k {
                    ESCAPE => flip_esc = true,
                    _ => flip_soft = true,
                }
                next = exc.next(&mut layout)?;
            }
            let byte = b[0];
            let (esc, w, soft) = if is_brk {
                (flip_esc, if flip_esc { 3 * len as u64 } else { 0 }, flip_soft)
            } else {
                let esc = canon_escape(byte, before_end) != flip_esc;
                if !esc && !(literal(byte) || byte == b' ' || byte == b'\t') {
                    return Err(Error("a byte that is not written as it is"));
                }
                let w = if esc { 3 } else { 1 };
                (esc, w, canon_soft(col, w, before_end, line, flags & START != 0) != flip_soft)
            };
            if soft {
                e.put(b"=")?;
                e.put(eol)?;
                col = 0;
            }
            match (is_brk, esc) {
                (true, false) => {
                    e.put(eol)?;
                    col = 0;
                }
                (true, true) => {
                    for &x in &b[..len] {
                        e.put(&[b'=', digits[(x >> 4) as usize], digits[(x & 15) as usize]])?;
                    }
                    col += w;
                }
                (false, false) => {
                    e.put(&[byte])?;
                    col += 1;
                }
                (false, true) => {
                    e.put(&[b'=', digits[(byte >> 4) as usize], digits[(byte & 15) as usize]])?;
                    col += 3;
                }
            }
            d.at += len;
            index += 1;
        }
        if next.is_some() {
            return Err(Error("exceptions past the data"));
        }
        if flags & SOFT_END != 0 {
            e.put(b"=")?;
            e.put(eol)?;
        }
    }
    if data_left != 0 {
        return Err(Error("data not used up"));
    }
    // the rest of the text
    loop {
        match text.pull(&mut d.buf[..PIECE]) {
            0 => break,
            k => e.put(&d.buf[..k])?,
        }
    }
    e.flush()?;
    Ok(e.total)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]).
pub fn decode_part(params_len: u64, text_len: u64, data_len: u64, layout_len: u64, size: u64) {
    aqr_api::finish(part(params_len, text_len, data_len, layout_len, size));
}

fn part(params_len: u64, text_len: u64, data_len: u64, layout_len: u64, size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let (mut text, mut data, mut layout) = (In::<0>::new(text_len), In::<1>::new(data_len), In::<2>::new(layout_len));
    let mut out = Written::new(size);
    let n = build(&mut text, &mut data, data_len, &mut layout, &mut out)?;
    match n == size {
        true => Ok(()),
        false => Err(Error("output of another length")),
    }
}

/// The module `b2t-recons-quopri`: the file of `len` bytes from the text, the decoded bytes and
/// the layout (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[], &[text, data, layout]) = (params, inputs) else { return Err(corrupt()) };
    let mut out = Vec::with_capacity(len.min(1 << 30) as usize);
    let n = build(&mut Slice(text), &mut Slice(data), data.len() as u64, &mut Slice(layout), &mut out)?;
    match n == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
}
