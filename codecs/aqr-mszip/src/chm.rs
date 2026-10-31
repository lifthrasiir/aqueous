//! CHM files (Microsoft's compiled HTML help, ITSF version 3) read: the directory's entries, and
//! the LZX section's content, control data and reset table.

use std::ops::Range;

/// The signature.
pub const MAGIC: &[u8; 4] = b"ITSF";

/// Names of the LZX section's files.
const CONTENT: &[u8] = b"::DataSpace/Storage/MSCompressed/Content";
const CONTROL: &[u8] = b"::DataSpace/Storage/MSCompressed/ControlData";
const RESET_TABLE: &[u8] =
    b"::DataSpace/Storage/MSCompressed/Transform/{7FC28940-9D31-11D0-9B27-00A0C91E9C7C}/InstanceData/ResetTable";

/// A directory entry: its name, section (0 stored, 1 LZX) and where it is in the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub section: u64,
    pub offset: u64,
    pub len: u64,
}

/// The LZX section: where its compressed content is in the file, its window (as a power of two),
/// the frames between resets, its bytes of data, and where each frame's bits start in the
/// content (by the reset table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lzx {
    pub content: Range<usize>,
    pub window_bits: u8,
    pub reset_frames: usize,
    pub len: usize,
    pub frames: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chm {
    pub entries: Vec<Entry>,
    /// Where section 0 starts in the file.
    pub data: usize,
    pub lzx: Option<Lzx>,
}

impl Chm {
    fn entry(&self, name: &[u8]) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// The span of section 0's entry `e` in the file.
    fn span(&self, e: &Entry) -> Option<Range<usize>> {
        let start = self.data.checked_add(usize::try_from(e.offset).ok()?)?;
        Some(start..start.checked_add(usize::try_from(e.len).ok()?)?)
    }

    /// The LZX section's entries, by their spans in its data.
    pub fn lzx_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.section == 1)
    }
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p.checked_add(4)?)?.try_into().unwrap()))
}

fn u64_at(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(p..p.checked_add(8)?)?.try_into().unwrap()))
}

/// A number of 7-bit groups, the high first, each but the last with its top bit set.
fn encint(d: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    loop {
        let b = *d.get(*p)?;
        *p += 1;
        v = v.checked_mul(128)? | (b & 0x7f) as u64;
        if b < 0x80 {
            return Some(v);
        }
    }
}

/// Whether `d` starts with a CHM file's header.
pub fn starts(d: &[u8]) -> bool {
    d.len() >= 0x60 && &d[..4] == MAGIC && u32_at(d, 4) == Some(3) && u32_at(d, 8) == Some(0x60)
}

/// The CHM file `d`: None where its header or directory are not in it. Its LZX section is None
/// where its control data or reset table are not as this takes them.
pub fn parse(d: &[u8]) -> Option<Chm> {
    if !starts(d) {
        return None;
    }
    let dir = usize::try_from(u64_at(d, 0x48)?).ok()?;
    let data = usize::try_from(u64_at(d, 0x58)?).ok()?;
    if d.get(dir..dir + 4)? != b"ITSP" {
        return None;
    }
    let header = u32_at(d, dir + 8)? as usize;
    let chunk = u32_at(d, dir + 0x10)? as usize;
    let chunks = u32_at(d, dir + 0x2c)? as usize;
    if chunk < 20 {
        return None;
    }
    let mut entries = Vec::new();
    for c in 0..chunks {
        let at = dir.checked_add(header)?.checked_add(c.checked_mul(chunk)?)?;
        let ch = d.get(at..at.checked_add(chunk)?)?;
        if &ch[..4] != b"PMGL" {
            continue;
        }
        let free = u32_at(ch, 4)? as usize;
        let end = chunk.checked_sub(free)?;
        let mut p = 20;
        while p < end {
            let n = usize::try_from(encint(ch, &mut p)?).ok()?;
            let name = ch.get(p..p.checked_add(n)?)?.to_vec();
            p += n;
            let (section, offset, len) = (encint(ch, &mut p)?, encint(ch, &mut p)?, encint(ch, &mut p)?);
            entries.push(Entry { name, section, offset, len });
        }
    }
    let mut chm = Chm { entries, data, lzx: None };
    chm.lzx = lzx(d, &chm);
    Some(chm)
}

/// The LZX section of `c`: control data of version 2 (sizes in frames), a reset table of version
/// 2 telling every frame's start.
fn lzx(d: &[u8], c: &Chm) -> Option<Lzx> {
    let span = |name| c.entry(name).filter(|e| e.section == 0).and_then(|e| c.span(e));
    let content = span(CONTENT)?;
    let control = d.get(span(CONTROL)?)?;
    let table = d.get(span(RESET_TABLE)?)?;
    if content.end > d.len() || control.get(4..8)? != b"LZXC" || u32_at(control, 8)? != 2 {
        return None;
    }
    let (reset_frames, window) = (u32_at(control, 12)? as usize, u32_at(control, 16)? as usize);
    if !window.is_power_of_two() || reset_frames == 0 {
        return None;
    }
    let window_bits = (window.trailing_zeros() + 15) as u8;
    if !(15..=21).contains(&window_bits) {
        return None;
    }
    let (version, n, size, header) = (u32_at(table, 0)?, u32_at(table, 4)?, u32_at(table, 8)?, u32_at(table, 12)?);
    let (len, clen, frame) = (u64_at(table, 16)?, u64_at(table, 24)?, u64_at(table, 32)?);
    let len = usize::try_from(len).ok()?;
    // (the content may run past the compressed bytes: those after them are not the stream's)
    if version != 2 || size != 8 || frame != 0x8000 || clen > content.len() as u64 || n as usize != len.div_ceil(0x8000)
    {
        return None;
    }
    let content = content.start..content.start + clen as usize;
    let frames = (0..n as usize)
        .map(|i| usize::try_from(u64_at(table, header as usize + 8 * i)?).ok())
        .collect::<Option<Vec<usize>>>()?;
    Some(Lzx { content, window_bits, reset_frames, len, frames })
}
