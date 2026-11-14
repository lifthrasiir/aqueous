//! Minecraft's region files (Anvil, `.mca`; FORMAT.md, §1): a header of 1024 locations and 1024
//! timestamps, then sectors of 4 KiB, each chunk in a run of them: its length (4 bytes, of the
//! compression type and the data), the compression type (1 byte) and the data. Nothing of a
//! file is magic, so [`starts`] tells one by its locations, checked against the file.

/// Bytes of a sector, and of the header (two sectors: the locations, then the timestamps).
pub const SECTOR: usize = 4096;
pub const HEADER: usize = 2 * SECTOR;

/// Chunks of a region (32×32), entries of each table of the header.
pub const CHUNKS: usize = 1024;

/// The compression types: gzip, zlib, none, LZ4 (Minecraft's own framing), a named one; with
/// [`EXTERNAL`], the data is in a file of its own (`c.X.Z.mcc`) and the chunk holds the type only.
pub const GZIP: u8 = 1;
pub const ZLIB: u8 = 2;
pub const NONE: u8 = 3;
pub const LZ4: u8 = 4;
pub const CUSTOM: u8 = 127;
pub const EXTERNAL: u8 = 128;

/// A chunk of a region file, as its location tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Its entry in the header (`x + 32 z`, of the chunk's coordinates in the region).
    pub index: u16,
    /// Its first sector, and how many it takes.
    pub sector: u32,
    pub count: u8,
}

impl Chunk {
    /// Its bytes in the file.
    pub fn range(&self) -> core::ops::Range<usize> {
        let at = self.sector as usize * SECTOR;
        at..at + self.count as usize * SECTOR
    }
}

/// The chunks of the location table `header` (at least [`HEADER`] bytes), in the order of
/// their sectors: those of a location of a sector past the header and at least one sector (any
/// other location holds no chunk). None where two overlap, or one does not end by `sectors`.
pub fn chunks(header: &[u8], sectors: u64) -> Option<Vec<Chunk>> {
    let mut v = Vec::with_capacity(CHUNKS);
    for (i, l) in header[..SECTOR].as_chunks::<4>().0.iter().enumerate() {
        let l = u32::from_be_bytes(*l);
        let (sector, count) = (l >> 8, l as u8);
        if sector >= 2 && count > 0 {
            v.push(Chunk { index: i as u16, sector, count });
        }
    }
    v.sort_unstable_by_key(|c| c.sector);
    let mut end = 2u64;
    for c in &v {
        if (c.sector as u64) < end {
            return None;
        }
        end = c.sector as u64 + c.count as u64;
    }
    (end <= sectors).then_some(v)
}

/// The chunks of the region file `d` ([`chunks`]), where it is one: of whole sectors (but
/// what follows the last chunk), the length of each chunk within its sectors.
pub fn parse(d: &[u8]) -> Option<Vec<Chunk>> {
    let header = d.get(..HEADER)?;
    let v = chunks(header, (d.len() / SECTOR) as u64)?;
    for c in &v {
        let at = c.range().start;
        let len = u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        if len + 4 > c.count as usize * SECTOR {
            return None;
        }
    }
    Some(v)
}

/// Whether `d` is a region file of at least one chunk ([`parse`]), each of a compression type
/// that Minecraft writes and of data (or of the type alone, its data in a file of its own).
pub fn starts(d: &[u8]) -> bool {
    let Some(v) = d.get(..HEADER).and_then(|h| chunks(h, (d.len() / SECTOR) as u64)) else { return false };
    !v.is_empty()
        && v.iter().all(|c| {
            let at = c.range().start;
            let len = u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize;
            let t = d[at + 4];
            len + 4 <= c.count as usize * SECTOR
                && match t {
                    GZIP | ZLIB | NONE | LZ4 | CUSTOM => len > 1,
                    _ => t & EXTERNAL != 0 && matches!(t & !EXTERNAL, GZIP | ZLIB | NONE | LZ4 | CUSTOM) && len == 1,
                }
        })
}

/// The header zlib writes for a stream of window bits `wbits` at `level` and `strategy`
/// (`deflateInit2`'s): CMF, and FLG of the level's flags (0 for Huffman only and runs).
pub fn zlib_header(p: aqr_flate::zlib::Params) -> [u8; 2] {
    use aqr_flate::zlib::Strategy;
    let flags = match (p.level, p.strategy) {
        (_, Strategy::HuffmanOnly | Strategy::Rle) | (0..2, _) => 0,
        (2..6, _) => 1,
        (6, _) => 2,
        _ => 3,
    };
    let mut h = ((8 + ((p.wbits as u16 - 8) << 4)) << 8) | (flags << 6);
    h += 31 - h % 31;
    h.to_be_bytes()
}
