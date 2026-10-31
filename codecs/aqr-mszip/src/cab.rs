//! CAB files (MS-CAB) read: the header, the folders and their data blocks, the files.

use std::ops::Range;

/// The signature and the version (1.3) of every CAB file seen.
pub const MAGIC: &[u8; 4] = b"MSCF";

/// Compression types (the low 4 bits of a folder's `typeCompress`).
pub const NONE: u16 = 0;
pub const MSZIP: u16 = 1;
pub const QUANTUM: u16 = 2;
pub const LZX: u16 = 3;

/// Header flags: a cabinet before, after, and reserved fields.
const PREV: u16 = 1;
const NEXT: u16 = 2;
const RESERVE: u16 = 4;

/// The bytes of a data block's header but its reserved field.
pub const DATA_HEADER: usize = 8;

/// A data block (`CFDATA`): where its header starts, where its compressed bytes are, and its
/// bytes of data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Data {
    pub start: usize,
    pub payload: Range<usize>,
    pub uncomp: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub type_compress: u16,
    pub blocks: Vec<Data>,
}

impl Folder {
    /// The compression type.
    pub fn method(&self) -> u16 {
        self.type_compress & 15
    }

    /// The bytes of data.
    pub fn size(&self) -> usize {
        self.blocks.iter().map(|b| b.uncomp).sum()
    }

    /// The bytes of the file that its data blocks span, one after another.
    pub fn span(&self) -> Option<Range<usize>> {
        Some(self.blocks.first()?.start..self.blocks.last()?.payload.end)
    }
}

/// A file (`CFFILE`): its name, its folder (as `iFolder` tells it, the continued ones too) and
/// where its contents are in the folder's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub folder: u16,
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cab {
    /// `cbCabinet`.
    pub size: usize,
    pub flags: u16,
    pub folders: Vec<Folder>,
    pub files: Vec<File>,
    /// The span of a signature that the header's reserved field points at (Authenticode's,
    /// after the cabinet).
    pub signature: Option<Range<usize>>,
}

impl Cab {
    /// The bytes of the file: the cabinet and the signature after it.
    pub fn extent(&self) -> usize {
        self.signature.as_ref().map_or(self.size, |s| s.end.max(self.size))
    }

    /// Whether folder `i` goes on in another cabinet (before or after this one), so that its
    /// data is not all here.
    pub fn continued(&self, i: usize) -> bool {
        (i == 0 && self.flags & PREV != 0) || (i + 1 == self.folders.len() && self.flags & NEXT != 0)
    }

    /// The files of folder `i`, as `iFolder` and the continued ones tell them.
    pub fn files_of(&self, i: usize) -> impl Iterator<Item = &File> {
        let last = self.folders.len().wrapping_sub(1);
        self.files.iter().filter(move |f| match f.folder {
            0xfffd => i == 0,
            0xfffe | 0xffff => i == last,
            k => k as usize == i,
        })
    }
}

fn u16_at(d: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(p..p + 2)?.try_into().unwrap()))
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(p..p + 4)?.try_into().unwrap()))
}

/// A string ending with a zero byte at `*p` (taken past it).
fn sz(d: &[u8], p: &mut usize) -> Option<Vec<u8>> {
    let n = d.get(*p..)?.iter().position(|&b| b == 0)?;
    let s = d[*p..*p + n].to_vec();
    *p += n + 1;
    Some(s)
}

/// Whether `d` starts with a CAB file's header.
pub fn starts(d: &[u8]) -> bool {
    d.len() >= 36 && &d[..4] == MAGIC && u32_at(d, 4) == Some(0) && d[24] == 3 && d[25] == 1
}

/// The CAB file at the start of `d`, as far as `d` holds it: None where its header, folders or
/// files are not in it. A data block past the end of `d` ends its folder's blocks there.
pub fn parse(d: &[u8]) -> Option<Cab> {
    if !starts(d) {
        return None;
    }
    let size = u32_at(d, 8)? as usize;
    let files_at = u32_at(d, 16)? as usize;
    let (folders, files, flags) = (u16_at(d, 26)?, u16_at(d, 28)?, u16_at(d, 30)?);
    let mut p = 36;
    let (mut folder_res, mut data_res, mut signature) = (0, 0, None);
    if flags & RESERVE != 0 {
        let header_res = u16_at(d, p)? as usize;
        folder_res = *d.get(p + 2)? as usize;
        data_res = *d.get(p + 3)? as usize;
        p += 4;
        let res = d.get(p..p + header_res)?;
        // (Authenticode's: 0x00100000, the signature's offset and size, 8 bytes of zeros)
        if header_res == 20 && u32_at(res, 0) == Some(0x0010_0000) {
            let (at, n) = (u32_at(res, 4)? as usize, u32_at(res, 8)? as usize);
            signature = Some(at..at.checked_add(n)?).filter(|s| s.start >= size);
        }
        p += header_res;
    }
    if flags & PREV != 0 {
        sz(d, &mut p)?;
        sz(d, &mut p)?;
    }
    if flags & NEXT != 0 {
        sz(d, &mut p)?;
        sz(d, &mut p)?;
    }
    let mut cab = Cab { size, flags, signature, ..Cab::default() };
    for _ in 0..folders {
        let (start, n, type_compress) = (u32_at(d, p)? as usize, u16_at(d, p + 4)?, u16_at(d, p + 6)?);
        p += 8 + folder_res;
        let mut blocks = Vec::with_capacity(n as usize);
        let mut q = start;
        for _ in 0..n {
            let (Some(cb), Some(uncomp)) = (u16_at(d, q + 4), u16_at(d, q + 6)) else { break };
            let payload = q + DATA_HEADER + data_res..q + DATA_HEADER + data_res + cb as usize;
            if payload.end > d.len() {
                break;
            }
            blocks.push(Data { start: q, payload: payload.clone(), uncomp: uncomp as usize });
            q = payload.end;
        }
        cab.folders.push(Folder { type_compress, blocks });
    }
    let mut p = files_at;
    for _ in 0..files {
        let (size, offset, folder) = (u32_at(d, p)?, u32_at(d, p + 4)?, u16_at(d, p + 8)?);
        p += 16;
        let name = sz(d, &mut p)?;
        cab.files.push(File { name, folder, offset, size });
    }
    Some(cab)
}
