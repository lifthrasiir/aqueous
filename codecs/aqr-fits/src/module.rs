//! The code of the modules (see `formula`, and FORMAT.md), which needs no aqr-formula, so that
//! aqr-modules builds it into their Wasm modules.

use crate::image::{self, Coded, Mode, Shape};
use crate::table::{self, Column};
use crate::tiled::{self, Layout};
use crate::{Error, Result, corrupt};
use aqr_common::varint;

/// The pixels of an image as numbers for the model: big-endian, of `BITPIX` 8 (unsigned), 16
/// or 32 (signed), or −32 (IEEE floats, mapped so that their order is that of the numbers as
/// signed: a negative one's other bits flipped), without the `shift` low bits that are zero in
/// every pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: usize,
    pub bitpix: i8,
    pub mode: Mode,
    pub shift: u8,
}

impl Pixels {
    pub fn valid(&self) -> bool {
        self.width > 0 && matches!(self.bitpix, 8 | 16 | 32 | -32) && (self.shift as usize) < self.bytes() * 8
    }

    pub fn bytes(&self) -> usize {
        self.bitpix.unsigned_abs() as usize / 8
    }

    pub fn shape(&self) -> Shape {
        Shape { width: self.width, bits: self.bits(), signed: self.bitpix != 8 }
    }

    /// Bits of a number.
    fn bits(&self) -> u32 {
        self.bytes() as u32 * 8 - self.shift as u32
    }

    /// The params: a varint of the width, then `BITPIX` as a byte, the mode and the shift.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        varint::put(&mut v, self.width as u64);
        v.extend([self.bitpix as u8, self.mode as u8, self.shift]);
        v
    }

    pub fn from_bytes(b: &[u8]) -> Option<Pixels> {
        let mut at = 0;
        let width = usize::try_from(varint::get(b, &mut at).ok()?).ok()?;
        let &[bitpix, mode, shift] = b.get(at..)? else { return None };
        let p = Pixels { width, bitpix: bitpix as i8, mode: Mode::from_byte(mode)?, shift };
        p.valid().then_some(p)
    }

    /// The pixels of data `d`, whole ones, as they are.
    pub fn pixels(&self, d: &[u8]) -> Vec<u32> {
        d.chunks_exact(self.bytes()).map(|c| c.iter().fold(0u32, |a, &b| a << 8 | b as u32)).collect()
    }

    /// The bits that are zero in every one of `pixels`, at most all but one.
    pub fn zero_bits(&self, pixels: &[u32]) -> u8 {
        let or = pixels.iter().fold(0u32, |a, &p| a | p);
        (or.trailing_zeros()).min(self.bytes() as u32 * 8 - 1) as u8
    }

    /// The numbers of `pixels` ([`Pixels::pixels`]).
    pub fn numbers(&self, pixels: &[u32]) -> Vec<u32> {
        let (float, bits) = (self.bitpix < 0, self.bits());
        pixels.iter().map(|&x| if float { order(x >> self.shift, bits) } else { x >> self.shift }).collect()
    }

    /// The data of [`Pixels::numbers`].
    pub fn data(&self, numbers: &[u32]) -> Vec<u8> {
        let n = self.bytes();
        let (float, bits) = (self.bitpix < 0, self.bits());
        let mut d = Vec::with_capacity(numbers.len() * n);
        for &x in numbers {
            let x = if float { order(x, bits) } else { x } << self.shift;
            d.extend_from_slice(&x.to_be_bytes()[4 - n..]);
        }
        d
    }
}

/// A float's bits, `bits` of them, as a number of the same order (its own inverse): the other
/// bits of a negative one flipped.
#[inline(always)]
fn order(x: u32, bits: u32) -> u32 {
    let sign = (x >> (bits - 1)) & 1;
    x ^ (sign.wrapping_neg() & ((1u64 << (bits - 1)) - 1) as u32)
}

/// Whether `n` bytes or pixels of output may come of `coded` bytes: not more than 256 times
/// them and 64, which keeps a small input from claiming a large output. The encoder leaves
/// whatever codes smaller to the archive.
pub fn within(n: u64, coded: usize) -> bool {
    n <= (coded as u64 + 64) * 256
}

/// `fits-recons-image`: the data of an image of `len` bytes from `params` ([`Pixels`]) and `inputs`
/// (the model's coded part and raw bits).
pub fn image(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let p = Pixels::from_bytes(params).ok_or_else(corrupt)?;
    let &[model, raw] = inputs else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    // (no more pixels than 64 a byte coded)
    if len % p.bytes() != 0 || !within(len as u64, model.len() + raw.len()) {
        return Err(corrupt());
    }
    let mut numbers = vec![0u32; len / p.bytes()];
    let c = Coded { model: model.to_vec(), raw: raw.to_vec() };
    image::decode_as(&c, p.shape(), &mut numbers, None, p.mode)?;
    Ok(p.data(&numbers))
}

/// `fits-recons-rice`: a data unit of tile-compressed image from `params` ([`Layout`]) and `inputs`
/// (the table's rows, the residue, which tiles are rebuilt, the codes, the model's coded part
/// and raw bits).
pub fn rice(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let mut at = 0;
    let l = Layout::from_bytes(params, &mut at).filter(|l| l.len == len).ok_or_else(corrupt)?;
    let &[mode] = &params[at..] else { return Err(corrupt()) };
    let mode = Mode::from_byte(mode).ok_or_else(corrupt)?;
    let &[rows, residue, rebuilt, codes, model, raw] = inputs else { return Err(corrupt()) };
    let rebuilt = bitmap(rebuilt, l.rows as usize)?;
    let pixels = l.pixels().ok_or_else(corrupt)?;
    if !within(pixels, model.len() + raw.len()) {
        return Err(corrupt());
    }
    let (mut d, spans) = tiled::layout(&l, rows, residue, &rebuilt).ok_or_else(corrupt)?;
    let mut image = vec![0u32; pixels as usize];
    let coded = tiled::pixels_kept(&d, &l, &spans, &rebuilt, &mut image);
    let shape = Shape { width: l.width(), bits: l.rice.bits(), signed: l.rice.bytepix > 1 };
    let c = Coded { model: model.to_vec(), raw: raw.to_vec() };
    let known = (!coded.iter().all(|&c| c)).then(|| coded.iter().map(|&c| !c).collect::<Vec<_>>());
    image::decode_as(&c, shape, &mut image, known.as_deref(), mode)?;
    let codes = tiled::decode_codes(&l, &rebuilt, &image, codes).ok_or_else(corrupt)?;
    tiled::rebuild(&mut d, &l, &spans, &rebuilt, &image, &codes).ok_or(Error("tiles"))?;
    Ok(d)
}

/// Which of `n` tiles are rebuilt, from their bits (the first the lowest of the first byte).
fn bitmap(b: &[u8], n: usize) -> Result<Vec<bool>> {
    if b.len() != n.div_ceil(8) {
        return Err(corrupt());
    }
    Ok((0..n).map(|i| b[i / 8] >> (i % 8) & 1 != 0).collect())
}

/// The bits of `bitmap`.
pub fn to_bitmap(v: &[bool]) -> Vec<u8> {
    let mut b = vec![0u8; v.len().div_ceil(8)];
    for (i, _) in v.iter().enumerate().filter(|x| *x.1) {
        b[i / 8] |= 1 << (i % 8);
    }
    b
}

/// The layout of a binary table's rows as params: varints of the rows, then of each column
/// its bytes and its element's bytes, + 128 for differences.
pub fn table_params(rows: usize, cols: &[Column]) -> Vec<u8> {
    let mut v = Vec::new();
    varint::put(&mut v, rows as u64);
    for c in cols {
        varint::put(&mut v, c.len as u64);
        v.push(c.width as u8 | (c.delta as u8) << 7);
    }
    v
}

/// `fits-recons-table`: the rows of a binary table, `len` bytes, from `params` ([`table_params`]) and
/// `inputs` (their planes).
pub fn table(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[planes] = inputs else { return Err(corrupt()) };
    let mut at = 0;
    let rows = varint::get(params, &mut at).map_err(|_| corrupt())?;
    let mut cols = Vec::new();
    while at < params.len() {
        let len = usize::try_from(varint::get(params, &mut at).map_err(|_| corrupt())?).map_err(|_| corrupt())?;
        let &b = params.get(at).ok_or_else(corrupt)?;
        at += 1;
        cols.push(Column { len, width: (b & 0x7f) as usize, delta: b & 0x80 != 0 });
    }
    let row: usize = cols.iter().map(|c| c.len).sum();
    if row == 0 || (row as u64).checked_mul(rows) != Some(len) {
        return Err(corrupt());
    }
    table::join(planes, row, rows as usize, &cols).ok_or_else(corrupt)
}
