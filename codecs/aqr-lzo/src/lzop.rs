//! lzop files (`FORMAT.md` §5): a header, then blocks of up to 256 KiB of data (by default),
//! each an LZO1X stream of its own or stored.

pub const MAGIC: [u8; 9] = *b"\x89LZO\0\r\n\x1a\n";

/// Flags of the header.
const ADLER32_D: u32 = 0x0001;
const ADLER32_C: u32 = 0x0002;
const EXTRA_FIELD: u32 = 0x0040;
const CRC32_D: u32 = 0x0100;
const CRC32_C: u32 = 0x0200;
const FILTER: u32 = 0x0800;

/// The most data of a block.
pub const BLOCK_MAX: usize = 64 << 20;

/// A block of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Where its stream (or the data, if stored) starts in the file.
    pub at: usize,
    /// Bytes of data, and of its stream.
    pub data: usize,
    pub stream: usize,
}

impl Block {
    pub fn stored(&self) -> bool {
        self.data == self.stream
    }
}

/// An lzop file, by its structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The LZO method: 1 and 2 LZO1X-1 (and its variant of 15 bits), 3 LZO1X-999.
    pub method: u8,
    /// Bytes of the header, up to the first block.
    pub header: usize,
    pub blocks: Vec<Block>,
    /// Bytes up to the end of its last block.
    pub len: usize,
}

fn be32(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

/// The lzop file at the start of `d`, by its structure (checksums are not checked); None
/// where it does not hold or a method other than LZO1X is used.
pub fn parse(d: &[u8]) -> Option<File> {
    if !d.starts_with(&MAGIC) {
        return None;
    }
    let version = u16::from_be_bytes(*d.get(9..)?.first_chunk()?);
    let mut p = 13;
    if version >= 0x0940 {
        p += 2;
    }
    let method = *d.get(p)?;
    p += 1;
    if !(1..=3).contains(&method) || version < 0x0900 {
        return None;
    }
    if version >= 0x0940 {
        p += 1;
    }
    let flags = be32(d, p)?;
    p += 4;
    if flags & FILTER != 0 {
        p += 4;
    }
    p += 4 + 4 + if version >= 0x0940 { 4 } else { 0 };
    p += 1 + *d.get(p)? as usize + 4;
    if flags & EXTRA_FIELD != 0 {
        p += 4 + be32(d, p)? as usize + 4;
    }
    let header = p;
    let mut blocks = Vec::new();
    loop {
        let data = be32(d, p)? as usize;
        p += 4;
        if data == 0 {
            break;
        }
        let stream = be32(d, p)? as usize;
        p += 4;
        if data > BLOCK_MAX || stream > data || stream == 0 {
            return None;
        }
        p += 4
            * ((flags & (ADLER32_D | CRC32_D) != 0) as usize
                + (flags & (ADLER32_C | CRC32_C) != 0 && stream < data) as usize);
        blocks.push(Block { at: p, data, stream });
        p += stream;
    }
    (p <= d.len()).then_some(File { method, header, blocks, len: p })
}
