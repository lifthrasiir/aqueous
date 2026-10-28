//! The container: `CSFCHUNK`, the file's size and the offset of the first chunk (u64 BE each),
//! then chunks of `CHNK`, a 4-byte kind and the body's size (u64 BE): `Head`, `Exta` (external
//! data: a layer's tiles as block data, or data of other kinds), `SQLi` (an SQLite database of
//! everything else) and `Foot`.
//!
//! Block data, after the external ID (`extrnlid` and 32 hex digits) and the data's size:
//!
//! ```text
//! per block:  u32 size of the record (all of it), str "BlockDataBeginChunk",
//!             u32 index, u32 inflated size, u32 width, u32 height, u32 has data,
//!             with data: u32 BE zlib size + 4, u32 LE zlib size, the zlib stream,
//!             str "BlockDataEndChunk"
//! then:       str "BlockStatus", str "BlockCheckSum": each u32 12, u32 blocks, u32 4, a u32 per block
//! ```
//!
//! Strings are a u32 count of UTF-16BE units, then the units. Integers are big-endian.

pub struct Chunk<'a> {
    pub kind: [u8; 4],
    /// Where the body starts in the file.
    pub start: usize,
    pub body: &'a [u8],
}

fn u32be(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

fn u64be(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

/// The chunks of a `.clip` file, if its structure holds to its end.
pub fn chunks(d: &[u8]) -> Option<Vec<Chunk<'_>>> {
    if !d.starts_with(b"CSFCHUNK") {
        return None;
    }
    let mut p = usize::try_from(u64be(d, 16)?).ok()?;
    let mut v = Vec::new();
    while p < d.len() {
        if d.get(p..p + 4)? != b"CHNK" {
            return None;
        }
        let kind = *d[p + 4..].first_chunk()?;
        let n = usize::try_from(u64be(d, p + 8)?).ok()?;
        let start = p + 16;
        let body = d.get(start..start.checked_add(n)?)?;
        v.push(Chunk { kind, start, body });
        p = start + n;
    }
    Some(v)
}

/// An `Exta` chunk's ID and data.
pub fn external(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let idl = usize::try_from(u64be(body, 0)?).ok()?;
    let id = body.get(8..8usize.checked_add(idl)?)?;
    let n = usize::try_from(u64be(body, 8 + idl)?).ok()?;
    let data = body.get(16 + idl..(16 + idl).checked_add(n)?)?;
    Some((id, data))
}

pub struct Block<'a> {
    pub index: u32,
    /// Bytes once inflated.
    pub len: u32,
    pub width: u32,
    pub height: u32,
    pub zlib: Option<&'a [u8]>,
    /// The record's span in the data.
    pub span: std::ops::Range<usize>,
}

pub struct Blocks<'a> {
    pub blocks: Vec<Block<'a>>,
    /// The tables after the blocks, by name, as (name, values).
    pub tables: Vec<(String, &'a [u8])>,
}

/// A string at `p`: its text and where it ends.
fn string(d: &[u8], p: usize) -> Option<(String, usize)> {
    let n = u32be(d, p)? as usize;
    let s = d.get(p + 4..(p + 4).checked_add(n.checked_mul(2)?)?)?;
    let units: Vec<u16> = s.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
    Some((String::from_utf16(&units).ok()?, p + 4 + 2 * n))
}

const BEGIN: &str = "BlockDataBeginChunk";
const END: &str = "BlockDataEndChunk";

/// Whether data of an `Exta` chunk starts as block data.
pub fn is_block_data(data: &[u8]) -> bool {
    string(data, 4).is_some_and(|(s, _)| s == BEGIN)
}

/// Block data as its blocks and tables, if all of it has that structure.
pub fn blocks(data: &[u8]) -> Option<Blocks<'_>> {
    if !is_block_data(data) {
        return None;
    }
    let (mut blocks, mut tables, mut q) = (Vec::new(), Vec::new(), 0);
    while q < data.len() {
        if let Some((_, r)) = string(data, q + 4).filter(|(s, _)| s == BEGIN) {
            let size = u32be(data, q)? as usize;
            let f: Vec<u32> = (0..5).map(|i| u32be(data, r + 4 * i)).collect::<Option<_>>()?;
            let mut r = r + 20;
            let zlib = if f[4] != 0 {
                let n = u32::from_le_bytes(*data.get(r + 4..)?.first_chunk()?) as usize;
                if u32be(data, r)? as usize != n + 4 {
                    return None;
                }
                let z = data.get(r + 8..r + 8 + n)?;
                r += 8 + n;
                Some(z)
            } else {
                None
            };
            let (e, r) = string(data, r)?;
            if e != END || r - q != size {
                return None;
            }
            blocks.push(Block { index: f[0], len: f[1], width: f[2], height: f[3], zlib, span: q..r });
            q = r;
        } else {
            let (name, r) = string(data, q)?;
            let (hs, n, es) = (u32be(data, r)? as usize, u32be(data, r + 4)? as usize, u32be(data, r + 8)? as usize);
            let v = data.get(r + hs..(r + hs).checked_add(n.checked_mul(es)?)?)?;
            tables.push((name, v));
            q = r + hs + v.len();
        }
    }
    Some(Blocks { blocks, tables })
}
