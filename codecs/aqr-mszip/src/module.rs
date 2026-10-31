//! The code of the module `mszip-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.

use aqr_common::varint;

use crate::mszip::{self, BitR};
use crate::{Error, Result, corrupt};

/// A block of the output, after the bytes kept before it: an MSZIP block of so many DEFLATE
/// blocks of the joined streams, or so many bytes of the raw input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Mszip { kept: usize, blocks: usize },
    Raw { kept: usize, len: usize },
}

/// The index of `blocks`: for each, the bytes kept before it, then its DEFLATE blocks times two
/// or its bytes times two plus one, as varints.
pub fn index(blocks: &[Block]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in blocks {
        let (kept, v) = match *b {
            Block::Mszip { kept, blocks } => (kept, 2 * blocks as u64),
            Block::Raw { kept, len } => (kept, 2 * len as u64 + 1),
        };
        varint::put(&mut out, kept as u64);
        varint::put(&mut out, v);
    }
    out
}

/// The blocks of `index`.
fn blocks(index: &[u8]) -> Result<Vec<Block>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < index.len() {
        let mut get = || varint::get(index, &mut at).ok().and_then(|n| usize::try_from(n).ok()).ok_or_else(corrupt);
        let (kept, v) = (get()?, get()?);
        out.push(match v & 1 {
            0 => Block::Mszip { kept, blocks: v / 2 },
            _ => Block::Raw { kept, len: v / 2 },
        });
    }
    Ok(out)
}

/// The first `n` bytes of `from`, taken from it.
fn take<'a>(from: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    let (a, b) = from.split_at_checked(n).ok_or_else(corrupt)?;
    *from = b;
    Ok(a)
}

/// The module: of the inputs (the joined streams, the raw bytes, the bytes kept, the index), each
/// block of the index after the bytes kept before it, then the bytes kept after the last, `len`
/// bytes in all (see `FORMAT.md`).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let &[joined, raw, kept, index] = inputs else { return Err(Error("four inputs wanted")) };
    if !params.is_empty() {
        return Err(corrupt());
    }
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    let mut out = Vec::with_capacity(len);
    let (mut r, mut raw, mut kept) = (BitR::new(joined), raw, kept);
    for b in blocks(index)? {
        match b {
            Block::Mszip { kept: k, blocks } => {
                out.extend_from_slice(take(&mut kept, k)?);
                mszip::cut_onto(&mut r, blocks, &mut out)?;
            }
            Block::Raw { kept: k, len } => {
                out.extend_from_slice(take(&mut kept, k)?);
                out.extend_from_slice(take(&mut raw, len)?);
            }
        }
        if out.len() > len {
            return Err(Error("output of another length"));
        }
    }
    out.extend_from_slice(kept);
    if r.pos() != 8 * joined.len() || !raw.is_empty() || out.len() != len {
        return Err(Error("output of another length"));
    }
    Ok(out)
}
