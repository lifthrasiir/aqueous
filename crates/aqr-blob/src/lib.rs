//! Blobs: native libraries digested into just what it takes to place them in another image
//! (`aqr-ld blob`, `aqr-ld blobs`): the image laid out from the first section, its base
//! relocations as a list, the RVAs of its entries, and its imports from the other libraries of the
//! set it was digested with, bound by name ahead ([`Bind`]). `aqr-ld merge` places blobs into a
//! stub executable as sections (the self-extracting programs); [`load`] places them in memory at
//! run time (Windows x86-64), for a program that carries them as data (aqr, the interpreter blob).

use std::fmt;

#[cfg(feature = "runner")]
pub mod interp;
#[cfg(all(windows, target_arch = "x86_64"))]
mod load;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use load::{Image, Placed, load};

pub type Result<T> = std::result::Result<T, String>;

/// An import bound to another blob of the set: the RVA of the slot of the import address table
/// (in the importing blob), the library imported from (its name, lowercase) and the RVA of the
/// export there. Whoever places the blobs writes the export's address into the slot.
#[derive(Debug, Clone)]
pub struct Bind {
    pub slot: u32,
    pub lib: String,
    pub rva: u32,
}

/// A native library digested. RVAs are the library's own (over its image base); the image holds
/// the bytes of `[first, first + image.len())`, indexed by `rva - first`. A shared library (no
/// entry of a module) has `apply_rva` 0. The import directory (`import_dir_rva`) is the imports
/// from outside the set only (of system libraries, bound where the blob is placed).
#[derive(Clone)]
pub struct Blob {
    pub apply_rva: u32,
    pub info_rva: u32,
    pub pdata_rva: u32,
    pub pdata_count: u32,
    pub import_dir_rva: u32,
    pub first: u32,
    pub image_base: u64,
    /// Its own name as a library (lowercase), what the others' imports name; empty where it
    /// exports nothing.
    pub name: String,
    pub binds: Vec<Bind>,
    pub relocs: Vec<u32>,
    pub image: Vec<u8>,
}

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Blob({:?}, {} bytes, {} binds)", self.name, self.image.len(), self.binds.len())
    }
}

pub const MAGIC: &[u8; 4] = b"AQB2";

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Blob {
    /// The libraries it binds imports from, each once.
    pub fn libs(&self) -> Vec<String> {
        let mut libs: Vec<String> = Vec::new();
        for b in &self.binds {
            if !libs.contains(&b.lib) {
                libs.push(b.lib.clone());
            }
        }
        libs
    }

    /// The blob serialized: the header, its name, the libraries it binds imports from, the
    /// binds (slot, library by index, RVA), the relocations, then the image.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let put = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let put_str = |b: &mut Vec<u8>, s: &str| {
            put(b, s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        };
        b.extend_from_slice(MAGIC);
        for v in [self.apply_rva, self.info_rva, self.pdata_rva, self.pdata_count, self.import_dir_rva, self.first] {
            put(&mut b, v);
        }
        b.extend_from_slice(&self.image_base.to_le_bytes());
        put_str(&mut b, &self.name);
        let libs = self.libs();
        put(&mut b, libs.len() as u32);
        for l in &libs {
            put_str(&mut b, l);
        }
        put(&mut b, self.binds.len() as u32);
        for bind in &self.binds {
            put(&mut b, bind.slot);
            put(&mut b, libs.iter().position(|l| *l == bind.lib).unwrap() as u32);
            put(&mut b, bind.rva);
        }
        put(&mut b, self.relocs.len() as u32);
        for &r in &self.relocs {
            put(&mut b, r);
        }
        put(&mut b, self.image.len() as u32);
        b.extend_from_slice(&self.image);
        b
    }

    /// A blob parsed from bytes.
    pub fn from_bytes(b: &[u8]) -> Result<Blob> {
        if b.len() < 36 || &b[0..4] != MAGIC {
            return Err("not a module blob (of this version)".into());
        }
        let mut r = Reader { b, at: 36 };
        let name = r.string()?;
        let libs = (0..r.word()?).map(|_| r.string()).collect::<Result<Vec<_>>>()?;
        let mut binds = Vec::new();
        for _ in 0..r.word()? {
            let (slot, lib, rva) = (r.word()?, r.word()? as usize, r.word()?);
            let lib = libs.get(lib).ok_or("a bind of no library")?.clone();
            binds.push(Bind { slot, lib, rva });
        }
        let relocs = (0..r.word()?).map(|_| r.word()).collect::<Result<Vec<_>>>()?;
        let n = r.word()? as usize;
        let image = r.take(n)?.to_vec();
        Ok(Blob {
            apply_rva: u32_at(b, 4),
            info_rva: u32_at(b, 8),
            pdata_rva: u32_at(b, 12),
            pdata_count: u32_at(b, 16),
            import_dir_rva: u32_at(b, 20),
            first: u32_at(b, 24),
            image_base: u64::from_le_bytes(b[28..36].try_into().unwrap()),
            name,
            binds,
            relocs,
            image,
        })
    }
}

/// The bytes of a blob read in order.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n).ok_or("a module blob cut short")?);
        let s = s.ok_or("a module blob cut short")?;
        self.at += n;
        Ok(s)
    }
    fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.word()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        let b = Blob {
            apply_rva: 0x1010,
            info_rva: 0x1020,
            pdata_rva: 0x3000,
            pdata_count: 2,
            import_dir_rva: 0x2000,
            first: 0x1000,
            image_base: 0x1_8000_0000,
            name: "aqr_dll_rt.dll".into(),
            binds: vec![
                Bind { slot: 0x2100, lib: "a.dll".into(), rva: 0x1234 },
                Bind { slot: 0x2108, lib: "b.dll".into(), rva: 0x5678 },
                Bind { slot: 0x2110, lib: "a.dll".into(), rva: 0x9abc },
            ],
            relocs: vec![0x1100, 0x1108],
            image: (0..0x3000u32).map(|i| i as u8).collect(),
        };
        let c = Blob::from_bytes(&b.to_bytes()).unwrap();
        assert_eq!(c.to_bytes(), b.to_bytes());
        assert_eq!(c.libs(), ["a.dll", "b.dll"]);
        assert!(Blob::from_bytes(&b.to_bytes()[..100]).is_err());
    }
}
