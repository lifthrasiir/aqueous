//! Mach-O images, thin or universal ("fat"): the image of each architecture, and the
//! sections of each that hold instructions.
//!
//! Only little-endian images are looked into (x86, x86-64, ARM, ARM64); big-endian ones
//! (PowerPC) are images without sections. A universal file's header is big-endian, as is
//! that of a Java class file, which starts with the same magic: a universal file has fewer
//! than 45 architectures (a class file's major version is at least 45), each of them an
//! image.

use alloc::vec::Vec;
use core::ops::Range;

pub const CPU_TYPE_X86: u32 = 7;
pub const CPU_TYPE_X86_64: u32 = 0x0100_0007;
pub const CPU_TYPE_ARM: u32 = 12;
pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;

const MH_MAGIC: u32 = 0xfeed_face;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
/// Fewer architectures than a class file's lowest major version.
const FAT_MAX_ARCHS: u32 = 45;

const LC_SEGMENT: u32 = 0x1;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1d;

const SECTION_TYPE: u32 = 0xff;
const S_ZEROFILL: u32 = 0x1;
const S_GB_ZEROFILL: u32 = 0xc;
const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x400;

/// An architecture's image: the whole file when thin, a slice of a universal file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Where the image is in the file, cut to the file.
    pub range: Range<usize>,
    /// `CPU_TYPE_*`, as the image's header says (or the universal header, for an image that
    /// is not looked into).
    pub cputype: u32,
    /// `(virtual address, file offset, size)` of each section with instructions, cut to the
    /// image, in the order of the load commands. File offsets are in the file, not the image.
    pub sections: Vec<(u64, usize, usize)>,
}

impl Image {
    /// Whether the image holds x86 or x86-64 code.
    pub fn is_x86(&self) -> bool {
        matches!(self.cputype, CPU_TYPE_X86 | CPU_TYPE_X86_64)
    }

    pub fn is64(&self) -> bool {
        self.cputype & 0x0100_0000 != 0
    }
}

fn u32le(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

fn u64le(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_le_bytes(*d.get(p..)?.first_chunk()?))
}

fn u32be(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

fn u64be(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_be_bytes(*d.get(p..)?.first_chunk()?))
}

/// Whether `d` starts with a Mach-O header, of either byte order.
fn thin_magic(d: &[u8]) -> bool {
    [u32le(d, 0), u32be(d, 0)].into_iter().flatten().any(|m| m == MH_MAGIC || m == MH_MAGIC_64)
}

/// The architectures of the universal file `d`, as `(cputype, offset, size)`, if it is one:
/// each starts within `d` with a Mach-O header (or past the end of `d`, which may be cut).
pub fn fat_archs(d: &[u8]) -> Option<Vec<(u32, u64, u64)>> {
    let wide = match u32be(d, 0)? {
        FAT_MAGIC => false,
        FAT_MAGIC_64 => true,
        _ => return None,
    };
    let n = u32be(d, 4)?;
    if n == 0 || n >= FAT_MAX_ARCHS {
        return None;
    }
    let entry = if wide { 32 } else { 20 };
    let mut archs = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let e = 8 + i * entry;
        let cputype = u32be(d, e)?;
        let (offset, size) = if wide {
            (u64be(d, e + 8)?, u64be(d, e + 16)?)
        } else {
            (u32be(d, e + 8)? as u64, u32be(d, e + 12)? as u64)
        };
        let header_end = (8 + n as usize * entry) as u64;
        if offset < header_end || size < 28 {
            return None;
        }
        if let Some(at) = usize::try_from(offset).ok().filter(|&o| o + 4 <= d.len()) {
            if !thin_magic(&d[at..]) {
                return None;
            }
        }
        archs.push((cputype, offset, size));
    }
    Some(archs)
}

/// Whether `d` starts like a Mach-O file: a little-endian image's header with its load
/// commands in `d`, or a universal header ([`fat_archs`]).
pub fn starts(d: &[u8]) -> bool {
    if fat_archs(d).is_some() {
        return true;
    }
    let header = match u32le(d, 0) {
        Some(MH_MAGIC) => 28,
        Some(MH_MAGIC_64) => 32,
        _ => return false,
    };
    let (Some(ncmds), Some(size)) = (u32le(d, 16), u32le(d, 20)) else { return false };
    ncmds > 0 && size as u64 >= ncmds as u64 * 8 && header + size as usize <= d.len()
}

/// The images of `d`, a thin or universal Mach-O file, cut to `d`; `None` if `d` is neither.
pub fn images(d: &[u8]) -> Option<Vec<Image>> {
    if let Some(archs) = fat_archs(d) {
        return Some(
            archs
                .into_iter()
                .filter_map(|(cputype, offset, size)| {
                    let start = usize::try_from(offset).ok().filter(|&o| o < d.len())?;
                    let end = usize::try_from(offset.saturating_add(size)).map_or(d.len(), |e| e.min(d.len()));
                    Some(thin(d, start..end).unwrap_or(Image { range: start..end, cputype, sections: Vec::new() }))
                })
                .collect(),
        );
    }
    starts(d).then(|| thin(d, 0..d.len())).flatten().map(|i| alloc::vec![i])
}

/// The little-endian image at `range` of `d`, with its sections; `None` where its header or
/// load commands do not hold.
fn thin(d: &[u8], range: Range<usize>) -> Option<Image> {
    let m = &d[range.clone()];
    let (header, wide) = match u32le(m, 0)? {
        MH_MAGIC => (28, false),
        MH_MAGIC_64 => (32, true),
        _ => return None,
    };
    let cputype = u32le(m, 4)?;
    let ncmds = u32le(m, 16)?;
    let cmds_end = header + u32le(m, 20)? as usize;
    let mut sections = Vec::new();
    let mut p = header;
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(m, p)?, u32le(m, p + 4)? as usize);
        if size < 8 || p + size > cmds_end {
            return None;
        }
        let (seg_header, sect_size) = match cmd {
            LC_SEGMENT if !wide => (56, 68),
            LC_SEGMENT_64 if wide => (72, 80),
            _ => {
                p += size;
                continue;
            }
        };
        let nsects = u32le(m, p + seg_header - 8)? as usize;
        if seg_header + nsects * sect_size > size {
            return None;
        }
        for s in (p + seg_header..).step_by(sect_size).take(nsects) {
            let (addr, len, off, flags) = if wide {
                (u64le(m, s + 32)?, u64le(m, s + 40)?, u32le(m, s + 48)?, u32le(m, s + 64)?)
            } else {
                (u32le(m, s + 32)? as u64, u32le(m, s + 36)? as u64, u32le(m, s + 40)?, u32le(m, s + 56)?)
            };
            let zerofill = matches!(flags & SECTION_TYPE, S_ZEROFILL | S_GB_ZEROFILL | S_THREAD_LOCAL_ZEROFILL);
            let code = flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0;
            let off = off as usize;
            if !code || zerofill || off == 0 || off >= m.len() || len == 0 {
                continue;
            }
            let len = usize::try_from(len).map_or(m.len() - off, |l| l.min(m.len() - off));
            sections.push((addr, range.start + off, len));
        }
        p += size;
    }
    Some(Image { range, cputype, sections })
}

/// Bytes of the Mach-O file at the start of `d`, as far as its headers tell: the end of its
/// last segment's file data, of its code signature or of its load commands, and of a
/// universal file's last architecture. Not cut to `d`.
pub fn extent(d: &[u8]) -> Option<u64> {
    if let Some(archs) = fat_archs(d) {
        let header = 8 + archs.len() as u64 * if u32be(d, 0)? == FAT_MAGIC_64 { 32 } else { 20 };
        return Some(archs.iter().fold(header, |e, &(_, offset, size)| e.max(offset.saturating_add(size))));
    }
    let (header, wide) = match u32le(d, 0)? {
        MH_MAGIC => (28, false),
        MH_MAGIC_64 => (32, true),
        _ => return None,
    };
    let ncmds = u32le(d, 16)?;
    let cmds_end = header + u32le(d, 20)? as u64;
    let mut end = cmds_end;
    let mut p = header as usize;
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(d, p)?, u32le(d, p + 4)? as usize);
        if size < 8 || (p + size) as u64 > cmds_end {
            return None;
        }
        let file_end = match cmd {
            LC_SEGMENT if !wide => (u32le(d, p + 32)? as u64).saturating_add(u32le(d, p + 36)? as u64),
            LC_SEGMENT_64 if wide => u64le(d, p + 40)?.saturating_add(u64le(d, p + 48)?),
            LC_CODE_SIGNATURE => (u32le(d, p + 8)? as u64).saturating_add(u32le(d, p + 12)? as u64),
            _ => 0,
        };
        end = end.max(file_end);
        p += size;
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A thin 64-bit image of `cputype` with a `__TEXT` segment of `__text` (instructions),
    /// `__const` (none) and `__bss` (zerofill), and a code signature after them.
    pub(crate) fn thin64(cputype: u32, code: &[u8]) -> Vec<u8> {
        let cmds = 72 + 3 * 80 + 16;
        let text = 32 + cmds;
        let mut d = vec![0u8; text];
        d[..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes());
        d[4..8].copy_from_slice(&cputype.to_le_bytes());
        d[12..16].copy_from_slice(&2u32.to_le_bytes()); // MH_EXECUTE
        d[16..20].copy_from_slice(&2u32.to_le_bytes());
        d[20..24].copy_from_slice(&(cmds as u32).to_le_bytes());
        let put32 = |d: &mut Vec<u8>, p: usize, v: u32| d[p..p + 4].copy_from_slice(&v.to_le_bytes());
        let put64 = |d: &mut Vec<u8>, p: usize, v: u64| d[p..p + 8].copy_from_slice(&v.to_le_bytes());
        let seg = 32;
        put32(&mut d, seg, LC_SEGMENT_64);
        put32(&mut d, seg + 4, 72 + 3 * 80);
        d[seg + 8..seg + 14].copy_from_slice(b"__TEXT");
        let size = (text + code.len() + 64) as u64;
        put64(&mut d, seg + 24, 0x1_0000_0000); // vmaddr
        put64(&mut d, seg + 32, size); // vmsize
        put64(&mut d, seg + 48, size); // filesize
        put32(&mut d, seg + 64, 3);
        let sects = [
            (&b"__text"[..], text, code.len(), S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS),
            (b"__const", text + code.len(), 64, 0),
            (b"__bss", 0, 4096, S_ZEROFILL),
        ];
        for (i, (name, off, len, flags)) in sects.into_iter().enumerate() {
            let s = seg + 72 + i * 80;
            d[s..s + name.len()].copy_from_slice(name);
            d[s + 16..s + 22].copy_from_slice(b"__TEXT");
            put64(&mut d, s + 32, 0x1_0000_0000 + off as u64);
            put64(&mut d, s + 40, len as u64);
            put32(&mut d, s + 48, off as u32);
            put32(&mut d, s + 64, flags);
        }
        let sig = seg + 72 + 3 * 80;
        put32(&mut d, sig, LC_CODE_SIGNATURE);
        put32(&mut d, sig + 4, 16);
        put32(&mut d, sig + 8, size as u32);
        put32(&mut d, sig + 12, 32);
        d.extend_from_slice(code);
        d.resize(size as usize + 32, 0xaa);
        d
    }

    /// A universal file of `images`, aligned to 4 KiB.
    pub(crate) fn fat(images: &[(u32, &[u8])]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        d.extend_from_slice(&(images.len() as u32).to_be_bytes());
        let mut at = 4096;
        let mut body = Vec::new();
        for &(cputype, image) in images {
            d.extend_from_slice(&cputype.to_be_bytes());
            d.extend_from_slice(&3u32.to_be_bytes());
            d.extend_from_slice(&(at as u32).to_be_bytes());
            d.extend_from_slice(&(image.len() as u32).to_be_bytes());
            d.extend_from_slice(&12u32.to_be_bytes());
            body.resize(at - 4096, 0);
            body.extend_from_slice(image);
            at = (4096 + body.len()).next_multiple_of(4096);
        }
        d.resize(4096, 0);
        d.extend_from_slice(&body);
        d
    }

    #[test]
    fn thin_sections_and_extent() {
        let code = [0x90u8; 1000];
        let d = thin64(CPU_TYPE_X86_64, &code);
        let all = images(&d).unwrap();
        assert_eq!(all.len(), 1);
        let i = &all[0];
        assert!(i.is_x86() && i.is64());
        let text = 32 + 72 + 3 * 80 + 16;
        assert_eq!(i.sections, [(0x1_0000_0000 + text as u64, text, 1000)]);
        assert_eq!(extent(&d), Some(d.len() as u64));
        let mut longer = d.clone();
        longer.extend_from_slice(b"trailing");
        assert_eq!(extent(&longer), Some(d.len() as u64));
    }

    #[test]
    fn fat_images() {
        let x86 = thin64(CPU_TYPE_X86_64, &[0x90; 3000]);
        let arm = thin64(CPU_TYPE_ARM64, &[0x1f, 0x20, 0x03, 0xd5].repeat(500));
        let d = fat(&[(CPU_TYPE_X86_64, &x86), (CPU_TYPE_ARM64, &arm)]);
        assert!(starts(&d));
        let all = images(&d).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].range, 4096..4096 + x86.len());
        assert_eq!(all[1].range.len(), arm.len());
        assert!(all[0].is_x86() && !all[1].is_x86());
        let text = 32 + 72 + 3 * 80 + 16;
        assert_eq!(all[0].sections[0].1, 4096 + text);
        assert_eq!(all[1].sections[0].1, all[1].range.start + text);
        assert_eq!(extent(&d), Some(d.len() as u64));
        // cut short: the images are cut to the data, the extent still that of the headers
        let cut = &d[..4096 + 500];
        assert_eq!(images(cut).unwrap()[0].range, 4096..4096 + 500);
        assert_eq!(extent(cut), Some(d.len() as u64));
    }

    /// A Java class file starts with the same magic: its version makes it no universal file.
    #[test]
    fn java_class_is_not_fat() {
        let class = b"\xca\xfe\xba\xbe\x00\x00\x00\x34\x00\x1d\x0a\x00\x06\x00\x0f";
        assert!(!starts(class));
        assert_eq!(images(class), None);
        // nor is a universal header whose architectures are not images
        let mut d = fat(&[(CPU_TYPE_X86_64, &thin64(CPU_TYPE_X86_64, &[0x90; 10]))]);
        d[4096 + 3] ^= 1;
        assert!(!starts(&d));
    }

    #[test]
    fn broken_load_commands() {
        let mut d = thin64(CPU_TYPE_X86_64, &[0x90; 100]);
        // a load command running past the commands
        d[32 + 4..32 + 8].copy_from_slice(&10_000u32.to_le_bytes());
        assert_eq!(images(&d).unwrap_or_default().iter().map(|i| i.sections.len()).sum::<usize>(), 0);
        assert_eq!(extent(&d), None);
    }
}
