//! The files the installer carries, recompressed as Aqueous would (aqr-exe's filters, then
//! zstd), though without Aqueous: the code sections of a PE image of x86 or x86-64 go through
//! the x86 disfilter (`exe-recons/x86`), its base relocations through `exe-recons/pe-reloc`,
//! and each of their streams and the rest of the file is a zstd frame of its own.
//!
//! ```text
//! payload  = "AQRWINS1" u8:count file*
//! file     = u8:n name[n] (UTF-8) le64:size u8:count region* frame:rest
//! region   = u8:kind le64:offset le64:len params frame*
//!            kind 1, x86: params le64:origin u8:sentinel u8:is64; 13 frames (x86::STREAM_NAMES)
//!            kind 2, base relocations: params u8:align; 2 frames (reloc::STREAM_NAMES)
//! frame    = le32:raw le32:n zstd[n]
//! ```
//!
//! The regions are in the order of their offsets, apart, within the file; `rest` is the file
//! without them.

use aqr_exe::arch::x86;
use aqr_exe::reloc;

pub const MAGIC: &[u8; 8] = b"AQRWINS1";

pub const X86: u8 = 1;
pub const RELOC: u8 = 2;

/// A file of the payload.
pub struct File {
    pub name: String,
    pub data: Vec<u8>,
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("the payload is cut short".into());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn usize(&mut self) -> Result<usize, String> {
        usize::try_from(self.u64()?).map_err(|_| "too large".into())
    }

    fn frame(&mut self) -> Result<Vec<u8>, String> {
        let raw = self.u32()? as usize;
        let n = self.u32()? as usize;
        let z = self.take(n)?;
        let out = zstd::bulk::decompress(z, raw).map_err(|e| format!("zstd: {e}"))?;
        if out.len() != raw {
            return Err("a frame of another length".into());
        }
        Ok(out)
    }
}

/// The files of `payload`.
pub fn unpack(payload: &[u8]) -> Result<Vec<File>, String> {
    let mut r = Reader(payload);
    if r.take(8)? != MAGIC {
        return Err("not a payload of this installer".into());
    }
    let count = r.u8()?;
    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let n = r.u8()? as usize;
        let name = String::from_utf8(r.take(n)?.to_vec()).map_err(|_| "a name not of UTF-8")?;
        let size = r.usize()?;
        let mut regions = Vec::new();
        for _ in 0..r.u8()? {
            let (kind, off, len) = (r.u8()?, r.usize()?, r.usize()?);
            let data = match kind {
                X86 => {
                    let (origin, sentinel, is64) = (r.u64()?, r.u8()?, r.u8()? != 0);
                    let streams: Vec<Vec<u8>> = (0..x86::NUM_STREAMS).map(|_| r.frame()).collect::<Result<_, _>>()?;
                    let streams = std::array::from_fn(|i| &streams[i][..]);
                    let st = x86::DecodedStreams::new(origin, sentinel, streams);
                    x86::decode_with(&st, is64, &x86::Opcodes::BUILTIN, len)
                }
                RELOC => {
                    let align = r.u8()?;
                    let (rvas, types) = (r.frame()?, r.frame()?);
                    reloc::decode(&rvas, &types, align, len)
                }
                _ => return Err(format!("a region of kind {kind}")),
            };
            match data {
                Some(d) if d.len() == len => regions.push((off, d)),
                _ => return Err(format!("{name}: a region at {off:#x} not decoded")),
            }
        }
        let rest = r.frame()?;
        let mut data = Vec::with_capacity(size);
        let mut at = 0;
        for (off, d) in regions {
            let Some(gap) = off.checked_sub(data.len()).filter(|&g| at + g <= rest.len()) else {
                return Err(format!("{name}: regions out of order"));
            };
            data.extend_from_slice(&rest[at..at + gap]);
            at += gap;
            data.extend_from_slice(&d);
        }
        data.extend_from_slice(&rest[at..]);
        if data.len() != size {
            return Err(format!("{name}: of {} bytes, not {size}", data.len()));
        }
        files.push(File { name, data });
    }
    Ok(files)
}
