//! The code of the module `b2t-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module. A part module: the text, the
//! decoded bytes and the layout are taken in order through windows and the file put out in order,
//! in pieces, so that nothing is held of their lengths.

use aqr_api::stream::{In, Pull, Put, Reader, Slice, Written};

use crate::{Error, Result, corrupt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 4;

/// The most separators a layout names, and the most bytes of one (`FORMAT.md`, "The layout").
pub const MAX_SEPS: usize = 64;
pub const MAX_SEP: usize = 32;

/// The alphabets of a run, by the bits 1 to 3 of its flags.
pub const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The hexadecimal alphabets of a run: lowercase and uppercase.
pub const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
pub const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// The flags of a run: padded with `=` (base64's alphabets only); and its alphabet's index
/// (`ALPHABET_SHIFT`): 0 standard, 1 URL-safe, 2 lowercase hex, 3 uppercase hex.
pub const PADDED: u8 = 1;
pub const ALPHABET_SHIFT: u32 = 1;
/// The alphabets that exist; those from `FIRST_HEX` on are hexadecimal.
const ALPHABETS: u8 = 4;
pub const FIRST_HEX: u8 = 2;

/// Bytes of the layout read at a time.
const LAYOUT_PIECE: usize = 4096;
/// Bytes of text or decoded data taken at a time: a multiple of 3.
const PIECE: usize = 3 * 8192;
/// Characters of a piece of decoded data (a last piece may end in a partial group, and padding).
const CHARS: usize = PIECE / 3 * 4 + 4;
/// Bytes of output put at a time.
pub(crate) const OUT: usize = 1 << 16;

/// The most bytes that the output's vector is made room for beforehand.
const PREALLOC: u64 = 1 << 30;

/// The memory plan of the module ([`aqr_api::stack`]): what [`decode_part`] takes beyond its
/// initial memory at most, whatever the lengths: the buffers it lays out, in the order of
/// [`build`].
pub fn plan() -> u64 {
    use aqr_api::stack::block;
    block(LAYOUT_PIECE as u64)
        + block((MAX_SEPS * MAX_SEP) as u64)
        + block(PIECE as u64)
        + block(CHARS as u64)
        + block(OUT as u64)
}

/// The output, put in pieces of [`OUT`] bytes.
pub(crate) struct Emit<'a> {
    buf: Vec<u8>,
    out: &'a mut dyn Put,
    pub(crate) total: u64,
}

impl<'a> Emit<'a> {
    pub(crate) fn new(out: &'a mut dyn Put) -> Emit<'a> {
        Emit { buf: Vec::with_capacity(OUT), out, total: 0 }
    }

    pub(crate) fn put(&mut self, mut d: &[u8]) -> Result<()> {
        while !d.is_empty() {
            let k = (OUT - self.buf.len()).min(d.len());
            self.buf.extend_from_slice(&d[..k]);
            d = &d[k..];
            if self.buf.len() == OUT {
                self.flush()?;
            }
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> Result<()> {
        self.out.put(&self.buf).ok_or(Error("output of another length"))?;
        self.total += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }
}

/// Fills `buf` from `src`; an error where it ends before.
pub(crate) fn fill(src: &mut dyn Pull, buf: &mut [u8]) -> Result<()> {
    let mut at = 0;
    while at < buf.len() {
        match src.pull(&mut buf[at..]) {
            0 => return Err(Error("input too short")),
            k => at += k,
        }
    }
    Ok(())
}

/// The separators of a layout.
struct Seps {
    bytes: Vec<u8>,
    lens: [u8; MAX_SEPS],
    count: usize,
}

impl Seps {
    fn get(&self, i: usize) -> &[u8] {
        &self.bytes[i * MAX_SEP..][..self.lens[i] as usize]
    }
}

/// The base64 of `src`, whole groups of three bytes except the last group: into `out`, which
/// has room for all of it; the number of characters.
fn encode_chars(src: &[u8], table: &[u8; 64], padded: bool, out: &mut [u8]) -> usize {
    let (groups, tail) = src.as_chunks::<3>();
    let mut at = 0;
    for (g, o) in groups.iter().zip(out.as_chunks_mut::<4>().0) {
        let v = (g[0] as u32) << 16 | (g[1] as u32) << 8 | g[2] as u32;
        o[0] = table[(v >> 18) as usize];
        o[1] = table[(v >> 12 & 63) as usize];
        o[2] = table[(v >> 6 & 63) as usize];
        o[3] = table[(v & 63) as usize];
        at += 4;
    }
    match *tail {
        [a] => {
            let v = (a as u32) << 16;
            out[at] = table[(v >> 18) as usize];
            out[at + 1] = table[(v >> 12 & 63) as usize];
            at += 2;
            if padded {
                out[at..at + 2].copy_from_slice(b"==");
                at += 2;
            }
        }
        [a, b] => {
            let v = (a as u32) << 16 | (b as u32) << 8;
            out[at] = table[(v >> 18) as usize];
            out[at + 1] = table[(v >> 12 & 63) as usize];
            out[at + 2] = table[(v >> 6 & 63) as usize];
            at += 3;
            if padded {
                out[at] = b'=';
                at += 1;
            }
        }
        _ => {}
    }
    at
}

/// The hexadecimal of `src`, two characters a byte, high nibble first: into `out`, which has
/// room for all of it; the number of characters.
fn encode_hex(src: &[u8], table: &[u8; 16], out: &mut [u8]) -> usize {
    for (&b, o) in src.iter().zip(out.as_chunks_mut::<2>().0) {
        o[0] = table[(b >> 4) as usize];
        o[1] = table[(b & 15) as usize];
    }
    src.len() * 2
}

/// The characters of a run of `n` bytes.
fn chars_of(n: u64, padded: bool) -> Option<u64> {
    match padded {
        true => n.div_ceil(3).checked_mul(4),
        false => n.checked_mul(4).map(|v| v.div_ceil(3)),
    }
}

/// One run of the layout (`FORMAT.md`, "The layout").
struct Run {
    gap: u64,
    n: u64,
    first: u64,
    line: u64,
    sep: usize,
    flags: u8,
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
    let mut seps = Seps { bytes: vec![0; MAX_SEPS * MAX_SEP], lens: [0; MAX_SEPS], count: 0 };
    let mut piece = vec![0u8; PIECE];
    let mut chars = vec![0u8; CHARS];
    let mut e = Emit::new(out);
    let c = |_| corrupt();
    // the separators
    let count = layout.num().map_err(c)?;
    if count > MAX_SEPS as u64 {
        return Err(Error("too many separators"));
    }
    for i in 0..count as usize {
        let len = layout.num().map_err(c)?;
        if len == 0 || len > MAX_SEP as u64 {
            return Err(Error("separator of a bad length"));
        }
        layout.exact(&mut seps.bytes[i * MAX_SEP..][..len as usize]).ok_or_else(corrupt)?;
        seps.lens[i] = len as u8;
    }
    seps.count = count as usize;
    // the runs
    let mut data_left = data_len;
    while !layout.done() {
        let run = Run {
            gap: layout.num().map_err(c)?,
            n: layout.num().map_err(c)?,
            first: layout.num().map_err(c)?,
            line: layout.num().map_err(c)?,
            sep: usize::try_from(layout.num().map_err(c)?).map_err(|_| corrupt())?,
            flags: layout.byte().ok_or_else(corrupt)?,
        };
        // (text before it)
        let mut gap = run.gap;
        while gap > 0 {
            let k = gap.min(PIECE as u64) as usize;
            fill(text, &mut piece[..k])?;
            e.put(&piece[..k])?;
            gap -= k as u64;
        }
        let alphabet = run.flags >> ALPHABET_SHIFT;
        if run.flags & !(PADDED | (ALPHABETS - 1) << ALPHABET_SHIFT) != 0 || alphabet >= ALPHABETS {
            return Err(Error("unknown flags of a run"));
        }
        let padded = run.flags & PADDED != 0;
        let hex = alphabet >= FIRST_HEX;
        if hex && padded {
            return Err(Error("padded hexadecimal"));
        }
        if run.n == 0 || run.n > data_left {
            return Err(Error("run of bytes the data does not have"));
        }
        let total = if hex { run.n.checked_mul(2) } else { chars_of(run.n, padded) }.ok_or_else(corrupt)?;
        // (lines: none of a run on one; else a first line shorter than the run, and lines)
        let mut line_left = match run.first {
            0 => {
                if run.line != 0 || run.sep != 0 {
                    return Err(Error("lines of an unwrapped run"));
                }
                u64::MAX
            }
            f => {
                if f >= total || run.line == 0 || run.sep >= seps.count {
                    return Err(Error("lines of a run that do not fit"));
                }
                f
            }
        };
        let sep = if run.first == 0 { &[][..] } else { seps.get(run.sep) };
        let mut left = run.n;
        // (a piece of hexadecimal takes half the bytes: its characters fit `chars` as well)
        let step = if hex { PIECE / 2 } else { PIECE } as u64;
        while left > 0 {
            let k = left.min(step) as usize;
            fill(data, &mut piece[..k])?;
            left -= k as u64;
            let m = match alphabet {
                0 => encode_chars(&piece[..k], STANDARD, padded, &mut chars),
                1 => encode_chars(&piece[..k], URL_SAFE, padded, &mut chars),
                2 => encode_hex(&piece[..k], HEX_LOWER, &mut chars),
                _ => encode_hex(&piece[..k], HEX_UPPER, &mut chars),
            };
            let mut s = &chars[..m];
            while !s.is_empty() {
                if line_left == 0 {
                    e.put(sep)?;
                    line_left = run.line;
                }
                let k = (s.len() as u64).min(line_left) as usize;
                e.put(&s[..k])?;
                s = &s[k..];
                line_left -= k as u64;
            }
        }
        data_left -= run.n;
    }
    if data_left != 0 {
        return Err(Error("data not used up"));
    }
    // the rest of the text
    loop {
        match text.pull(&mut piece) {
            0 => break,
            k => e.put(&piece[..k])?,
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

/// The module `b2t-recons`: the file of `len` bytes from the text, the decoded bytes and the
/// layout (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let (&[], &[text, data, layout]) = (params, inputs) else { return Err(corrupt()) };
    let mut out = Vec::with_capacity(len.min(PREALLOC) as usize);
    let n = build(&mut Slice(text), &mut Slice(data), data.len() as u64, &mut Slice(layout), &mut out)?;
    match n == len {
        true => Ok(out),
        false => Err(Error("output of another length")),
    }
}
