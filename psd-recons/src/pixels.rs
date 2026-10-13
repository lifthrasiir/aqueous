//! Decoding of channel and composite image data into planes.

use crate::psd::{Compression, Error, Psd, Result};

fn err<T>(s: impl Into<String>) -> Result<T> {
    Err(Error(s.into()))
}

/// Appends the PackBits decoding of `src` to `out`; returns false if `src` is malformed.
pub fn unpackbits(mut src: &[u8], out: &mut Vec<u8>) -> bool {
    while let [h, rest @ ..] = src {
        let h = *h as i8;
        if h >= 0 {
            let k = h as usize + 1;
            if rest.len() < k {
                return false;
            }
            out.extend_from_slice(&rest[..k]);
            src = &rest[k..];
        } else if h != -128 {
            let Some(&b) = rest.first() else { return false };
            out.resize(out.len() + (1 - h as isize) as usize, b);
            src = &rest[1..];
        } else {
            src = rest;
        }
    }
    true
}

fn inflate(zlib: &[u8]) -> Result<Vec<u8>> {
    if zlib.len() < 2 {
        return err("short zlib stream");
    }
    let f = flate_recons::inflate::parse(&zlib[2..]).map_err(|e| Error(format!("zip channel: {e}")))?;
    Ok(f.data)
}

/// Undoes the row-wise delta of "ZIP with prediction".
fn unpredict(buf: &mut [u8], width: usize, depth: u16) {
    match depth {
        8 => {
            for row in buf.chunks_mut(width.max(1)) {
                for i in 1..row.len() {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
            }
        }
        16 => {
            for row in buf.chunks_mut((2 * width).max(1)) {
                for i in (2..row.len() - row.len() % 2).step_by(2) {
                    let v = u16::from_be_bytes([row[i], row[i + 1]])
                        .wrapping_add(u16::from_be_bytes([row[i - 2], row[i - 1]]));
                    row[i..i + 2].copy_from_slice(&v.to_be_bytes());
                }
            }
        }
        32 => {
            // Bytes are delta coded across the row, then de-interleaved into byte planes.
            for row in buf.chunks_mut((4 * width).max(1)) {
                for i in 1..row.len() {
                    row[i] = row[i].wrapping_add(row[i - 1]);
                }
                let w = row.len() / 4;
                let planar = row.to_vec();
                for x in 0..w {
                    for b in 0..4 {
                        row[4 * x + b] = planar[b * w + x];
                    }
                }
            }
        }
        _ => {}
    }
}

/// Decodes `data` (after the compression field) holding `planes` planes of `width` x
/// `height` pixels.
fn decode_planes(psd: &Psd, comp: Compression, data: &[u8], width: u64, height: u64, planes: u64) -> Result<Vec<u8>> {
    let rb = psd.row_bytes(width) as usize;
    let size = rb * height as usize * planes as usize;
    match comp {
        Compression::Raw => {
            if data.len() < size {
                return err("raw data too short");
            }
            Ok(data[..size].to_vec())
        }
        Compression::Rle => {
            let cb = if psd.psb { 4 } else { 2 };
            let rows = (height * planes) as usize;
            if data.len() < rows * cb {
                return err("RLE counts truncated");
            }
            let mut out = Vec::with_capacity(size);
            let mut p = rows * cb;
            for r in 0..rows {
                let c = &data[r * cb..r * cb + cb];
                let n = if cb == 2 {
                    u16::from_be_bytes([c[0], c[1]]) as usize
                } else {
                    u32::from_be_bytes(c.try_into().unwrap()) as usize
                };
                let Some(src) = data.get(p..p + n) else { return err("RLE row truncated") };
                let before = out.len();
                if !unpackbits(src, &mut out) || out.len() - before != rb {
                    return err("RLE row length mismatch");
                }
                p += n;
            }
            Ok(out)
        }
        Compression::Zip | Compression::ZipPred => {
            let mut out = inflate(data)?;
            if out.len() != size {
                return err("ZIP data size mismatch");
            }
            if comp == Compression::ZipPred {
                for plane in out.chunks_mut(rb * height as usize) {
                    unpredict(plane, width as usize, psd.depth);
                }
            }
            Ok(out)
        }
        Compression::Other(c) => err(format!("compression {c}")),
    }
}

/// The decoded plane of channel `ch` of layer `layer`.
pub fn layer_channel(psd: &Psd, d: &[u8], layer: usize, ch: usize) -> Result<Vec<u8>> {
    let c = &psd.layer_info.layers[layer].channels[ch];
    let Some(comp) = c.compression(d) else { return Ok(Vec::new()) };
    decode_planes(psd, comp, &d[c.span.start + 2..c.span.end], c.width, c.height, 1)
}

/// The decoded composite image, planar.
pub fn composite(psd: &Psd, d: &[u8]) -> Result<Vec<u8>> {
    let comp = psd.composite_compression(d);
    decode_planes(psd, comp, &d[psd.composite.start + 2..], psd.width, psd.height, psd.channels as u64)
}
