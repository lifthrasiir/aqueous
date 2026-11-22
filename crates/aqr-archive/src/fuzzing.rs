//! What the fuzz targets' mutator takes of an archive (built only under `cfg(fuzzing)`, and its
//! tests): its
//! blocks by the generic structure, the data of each as it is decoded, and the block made again
//! of other data, so that a mutation of what a block holds (a formula, a metadata tree) gets
//! past its Zstandard coding. Blocks of a dictionary's data are left out (their data is not
//! decoded without it). An encrypted archive's payload is decrypted for it, and put back
//! ([`unsealed`]).

use std::ops::Range;

use super::*;
use crate::locate::frame_at;
use crate::parse::{decoded, leb};

/// A block of an archive, of data that decodes by itself.
#[derive(Debug, Clone)]
pub struct Block {
    /// Where it is (with its padding).
    pub at: Range<usize>,
    pub ty: u64,
    /// Its data: the bytes before what is decoded (a formula block's number of outputs), and the
    /// rest.
    head: Range<usize>,
    coded: Range<usize>,
    zstd: bool,
    checked: bool,
}

/// The blocks of archive `c` from its header on, as far as they frame, of data that decodes by
/// itself.
pub fn blocks(c: &[u8]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut at = HEADER_MAGIC.len();
    if !c.starts_with(&HEADER_MAGIC) && !c.starts_with(&OVERLAY_HEADER_MAGIC) {
        return out;
    }
    while let Some((ty, data, end)) = frame_at(c, at) {
        // (bits as `parse::blocks` reads them)
        let coding = match ty {
            0x10..=0x17 => Some((0, ty & 0x04 != 0, true)),
            0x20..=0x37 if ty & 0x10 == 0 => Some((0, ty & 0x08 != 0, ty & 0x04 != 0)),
            0x40..=0x4b if ty & 0x08 == 0 => Some((0, ty & 0x04 != 0, true)),
            0x51..=0x57 | 0x61..=0x67 => Some((0, true, true)),
            0x70..=0x77 if ty & 0x04 == 0 => {
                let mut p = data.start;
                leb(c, &mut p).ok().map(|_| (p - data.start, true, true))
            }
            _ => None,
        };
        if let Some((h, zstd, checked)) = coding
            && h <= data.len()
        {
            let head = data.start..data.start + h;
            let coded = head.end..data.end;
            out.push(Block { at: at..end, ty, head, coded, zstd, checked });
        }
        at = end;
    }
    out
}

impl Block {
    /// Its data decoded (of archive `c`), where it decodes.
    pub fn data(&self, c: &[u8]) -> Option<Vec<u8>> {
        decoded(&c[self.coded.clone()], self.zstd, self.checked).ok()
    }

    /// The block again (of archive `c`) of data `d`, coded as it is: Zstandard data at level 1,
    /// or of raw Zstandard blocks where that is shorter, its checksum where it has one. Its size
    /// is of the width of its type, or wider where it has to be.
    pub fn of(&self, c: &[u8], d: &[u8]) -> Vec<u8> {
        let mut data = c[self.head.clone()].to_vec();
        match self.zstd {
            true => {
                let at = always_compressed(&mut zstd_safe::CCtx::create(), &[d], 1, 1).unwrap_or_default();
                let raw = stored_external(&[d]);
                let mut z = if at.is_empty() || raw.len() <= at.len() { raw } else { at };
                if !self.checked {
                    z.truncate(z.len() - 8);
                }
                data.extend_from_slice(&z);
            }
            false => {
                data.extend_from_slice(d);
                if self.checked {
                    data.extend_from_slice(&checksum(d).to_le_bytes());
                }
            }
        }
        let n = data.len() as u64;
        let ss = (0..4).find(|&s| s >= self.ty % 4 && (s == 3 || n < 1 << (8 << s))).unwrap();
        framed(self.ty & !3 | ss, &data)
    }
}

/// A block of type `ty` and data `d`, padded.
fn framed(ty: u64, d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + 10);
    aqr_common::varint::put(&mut out, ty);
    out.extend_from_slice(&(d.len() as u64).to_le_bytes()[..1 << (ty % 4)]);
    out.extend_from_slice(d);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// The payload of an encrypted archive decrypted, to be mutated and put back ([`Sealed::put`]).
pub struct Sealed {
    pub payload: Vec<u8>,
    /// By chunk: where its sealed bytes are in the archive, and how many (its tag after them).
    at: Vec<(usize, usize)>,
}

/// Encrypted archive `c` unlocked by `passphrase`, where its work factor is `work_factor` (no
/// other, for scrypt's time and memory), its payload decrypted (of the chunks that do not
/// authenticate too, as fuzzed: [`super::envelope::Unlocked::decrypt_lossy`]).
pub fn unsealed(c: &[u8], passphrase: &[u8], work_factor: u8) -> Option<Sealed> {
    let (e, _) = super::envelope::open_damaged(c).ok()?;
    if e.work_factor() != work_factor {
        return None;
    }
    let u = e.unlock(passphrase).ok()?;
    let mut payload = vec![0; u.len()];
    u.decrypt_lossy(&mut payload, 1);
    let at = u.sealed_chunks().iter().map(|s| (s.as_ptr() as usize - c.as_ptr() as usize, s.len() - 16)).collect();
    Some(Sealed { payload, at })
}

impl Sealed {
    /// Payload `new` (as long as the payload) put into archive `c` (of which it was unsealed) in
    /// place of the payload: the ChaCha20 ciphertext changed where the bytes are, as the
    /// keystream is the same, and the tag of a chunk changed made to be taken as fuzzed
    /// (its first byte even: `Unlocked::opens`).
    pub fn put(&self, c: &mut [u8], new: &[u8]) {
        assert_eq!(new.len(), self.payload.len());
        let mut p = 0;
        for &(at, n) in &self.at {
            let mut changed = false;
            for i in 0..n {
                let d = self.payload[p + i] ^ new[p + i];
                c[at + i] ^= d;
                changed |= d != 0;
            }
            if changed {
                c[at + n] &= !1;
            }
            p += n;
        }
    }
}

/// A spacer block of `n` bytes in all (even, at least 2), of zeros.
pub fn spacer(n: usize) -> Vec<u8> {
    assert!(n >= 2 && n.is_multiple_of(2));
    // (of the widest size that `n` bytes take: its data the rest)
    let ss = (0..4).rev().find(|&s| n >= 1 + (1 << s) && (s == 3 || n - 1 - (1 << s) < 1 << (8 << s))).unwrap();
    framed(ss, &vec![0; n - 1 - (1 << ss)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::natives;
    use aqr_formula::{Formula, Node};

    /// Archives of compressed and stored blocks: formula blocks, leaf and non-leaf data.
    fn archives() -> Vec<Vec<u8>> {
        let mut f = Formula::default();
        let a = f.push("a", (0..3000u32).map(|i| (i * 7 % 251) as u8).collect::<Vec<u8>>());
        let b = f.push("b", b"hello, hello, hello".to_vec());
        f.root = Node::concat([Node::slice(0, 10, 900), b, a]);
        let mut out = Vec::new();
        for level in [Level::STORED, Level::Zstd(3)] {
            out.push(encode(&f, None, level).unwrap());
            out.push(encode(&Formula::raw("raw", vec![7; 5000]), None, level).unwrap());
        }
        out
    }

    #[test]
    fn blocks_made_again_decode_alike() {
        let n = natives();
        for c in archives() {
            let want = decode_value(&c, &n).unwrap();
            let blocks = blocks(&c);
            assert!(!blocks.is_empty());
            for b in &blocks {
                let d = b.data(&c).unwrap();
                let made = b.of(&c, &d);
                assert!(made.len() <= b.at.len(), "{:#x}: {} for {}", b.ty, made.len(), b.at.len());
                let mut new = c.clone();
                let mut put = made.clone();
                if put.len() < b.at.len() {
                    put.extend_from_slice(&spacer(b.at.len() - put.len()));
                }
                new[b.at.clone()].copy_from_slice(&put);
                assert_eq!(decode_value(&new, &n).unwrap(), want, "block {:#x} at {:?}", b.ty, b.at);
                // (a mutation of its data: the block made again of it decodes so)
                let mut d2 = d.clone();
                d2.push(0);
                assert_eq!(blocks_of(&b.of(&c, &d2)).data(&b.of(&c, &d2)), Some(d2));
            }
        }
    }

    /// The block of `b` (framed as at the start of an archive) as [`blocks`] finds it.
    fn blocks_of(b: &[u8]) -> Block {
        let c = [&HEADER_MAGIC[..], b].concat();
        let mut found = blocks(&c).into_iter().next().unwrap();
        // (its ranges of `b`)
        let back = |r: Range<usize>| r.start - HEADER_MAGIC.len()..r.end - HEADER_MAGIC.len();
        found.at = back(found.at);
        found.head = back(found.head);
        found.coded = back(found.coded);
        found
    }

    /// A payload changed and put back decrypts (without authentication) to what was put, the
    /// chunks changed not authenticating (as not fuzzed).
    #[test]
    fn payloads_put_back() {
        use std::io::Write;
        let d: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 253) as u8).collect();
        let a = Encoder::new(Level::STORED).with_size_hint(true).encode(&Formula::raw("raw", d), None).unwrap();
        for mirrored in [false, true] {
            let mut s =
                super::super::envelope::Sealer::new(std::io::Cursor::new(Vec::new()), b"p", 12, mirrored).unwrap();
            s.write_all(&a).unwrap();
            let mut c = s.into_inner().unwrap().into_inner();
            assert!(unsealed(&c, b"p", 13).is_none());
            let sealed = unsealed(&c, b"p", 12).unwrap();
            // (but for its size hint, filled in)
            assert!(sealed.payload.len() == a.len() && sealed.payload[20..] == a[20..]);
            let mut new = sealed.payload.clone();
            new[70_000] ^= 0x55;
            new[150_000..150_010].fill(7);
            sealed.put(&mut c, &new);
            let u = super::super::envelope::open(&c).unwrap().unlock(b"p").unwrap();
            let mut got = vec![0; u.len()];
            assert_eq!(u.decrypt_lossy(&mut got, 1), (vec![1, 2], false));
            assert!(got == new);
        }
    }

    #[test]
    fn spacers() {
        for n in (2..300_000).step_by(2) {
            let s = spacer(n);
            assert_eq!(s.len(), n);
            assert_eq!(frame_at(&s, 0).map(|f| (f.0 < 4, f.2)), Some((true, n)));
        }
    }
}
