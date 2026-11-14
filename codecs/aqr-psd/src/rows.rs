//! Row by row access to the planes of pixel data, without decoding whole planes.

use crate::psd::{Compression, Error, Psd, Result, err};

/// Decodes PackBits `src` into exactly `out`; false if malformed or of another length.
pub fn unpackbits_into(mut src: &[u8], out: &mut [u8]) -> bool {
    let mut o = 0;
    while let [h, rest @ ..] = src {
        let h = *h as i8;
        if h >= 0 {
            let k = h as usize + 1;
            if rest.len() < k || o + k > out.len() {
                return false;
            }
            out[o..o + k].copy_from_slice(&rest[..k]);
            o += k;
            src = &rest[k..];
        } else if h != -128 {
            let k = (1 - h as isize) as usize;
            let Some(&b) = rest.first() else { return false };
            if o + k > out.len() {
                return false;
            }
            out[o..o + k].fill(b);
            o += k;
            src = &rest[1..];
        } else {
            src = rest;
        }
    }
    o == out.len()
}

/// Reads big-endian row byte count `i` of `counts`.
#[inline]
pub fn count(counts: &[u8], cb: usize, i: usize) -> usize {
    counts[i * cb..(i + 1) * cb].iter().fold(0usize, |n, &b| n << 8 | b as usize)
}

enum Source<'a> {
    Rle {
        counts: &'a [u8],
        cb: usize,
        data: &'a [u8],
        first: usize,
    },
    Raw(&'a [u8]),
    /// A channel without data (its field is cut short): rows of zeros.
    Zero,
}

/// The rows of one plane, in order.
pub struct PlaneRows<'a> {
    src: Source<'a>,
    w: usize,
    h: usize,
    next: usize,
    pos: usize,
}

impl<'a> PlaneRows<'a> {
    /// Plane `plane` of `planes` in `data` (pixel data past the compression field), of `h`
    /// rows of `w` bytes.
    pub fn new(
        psb: bool,
        comp: Compression,
        data: &'a [u8],
        w: usize,
        h: usize,
        plane: usize,
        planes: usize,
    ) -> Result<Self> {
        let rows = h.checked_mul(planes).ok_or(Error("size".into()))?;
        let src = match comp {
            Compression::Rle => {
                let cb = if psb { 4 } else { 2 };
                let Some(counts) = data.get(..rows * cb) else { return err("RLE counts truncated") };
                let mut pos = rows * cb;
                for i in 0..plane * h {
                    pos += count(counts, cb, i);
                }
                return Ok(PlaneRows { src: Source::Rle { counts, cb, data, first: plane * h }, w, h, next: 0, pos });
            }
            Compression::Raw => {
                let n = w.checked_mul(h).ok_or(Error("size".into()))?;
                match data.get(plane * n..(plane + 1) * n) {
                    Some(d) => Source::Raw(d),
                    None => return err("raw data too short"),
                }
            }
            _ => return err("not row-decodable"),
        };
        Ok(PlaneRows { src, w, h, next: 0, pos: 0 })
    }

    /// A channel of a layer. ZIP channels are not read by rows (they would be decoded whole, and
    /// the memory of the module is laid out in advance): the hint is not made of layers that have
    /// them.
    pub fn channel(psd: &'a Psd, d: &'a [u8], layer: usize, ch: usize) -> Result<Self> {
        let c = &psd.layer_info.layers[layer].channels[ch];
        let (w, h) = (psd.row_bytes(c.width) as usize, c.height as usize);
        match c.compression(d) {
            Some(comp @ (Compression::Rle | Compression::Raw)) => {
                Self::new(psd.psb, comp, &d[c.span.start + 2..c.span.end], w, h, 0, 1)
            }
            Some(_) => err("not row-decodable"),
            None => Ok(PlaneRows { src: Source::Zero, w, h, next: 0, pos: 0 }),
        }
    }

    pub fn rows_left(&self) -> usize {
        self.h - self.next
    }

    /// The encoded bytes of the next row (RLE only).
    pub fn encoded(&self) -> Option<&'a [u8]> {
        match &self.src {
            Source::Rle { counts, cb, data, first } => {
                let n = count(counts, *cb, first + self.next);
                data.get(self.pos..self.pos + n)
            }
            _ => None,
        }
    }

    /// Decodes the next row into `out` (`w` bytes).
    pub fn next(&mut self, out: &mut [u8]) -> Result<()> {
        if self.next >= self.h {
            return err("past the last row");
        }
        let w = self.w;
        match &self.src {
            Source::Rle { counts, cb, data, first } => {
                let n = count(counts, *cb, first + self.next);
                let Some(src) = data.get(self.pos..self.pos + n) else { return err("RLE row truncated") };
                if !unpackbits_into(src, &mut out[..w]) {
                    return err("RLE row length mismatch");
                }
                self.pos += n;
            }
            Source::Raw(d) => out[..w].copy_from_slice(&d[self.next * w..(self.next + 1) * w]),
            Source::Zero => out[..w].fill(0),
        }
        self.next += 1;
        Ok(())
    }

    /// Skips the next row.
    pub fn skip(&mut self) -> Result<()> {
        if self.next >= self.h {
            return err("past the last row");
        }
        if let Source::Rle { counts, cb, first, .. } = &self.src {
            self.pos += count(counts, *cb, first + self.next);
        }
        self.next += 1;
        Ok(())
    }
}

// (the sizes of the structures on WebAssembly, which the memory plan uses)
#[cfg(target_family = "wasm")]
const _: () = {
    assert!(size_of::<Option<PlaneRows>>() == crate::module::wasm::PLANE_ROWS as usize);
};
