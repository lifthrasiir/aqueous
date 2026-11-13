//! What Unreal's readers share: a cursor over a table, and its `FString`s.

use super::{u32le, u64le, utf16_lossy};

/// A reader of a table.
pub(super) struct R<'a> {
    pub(super) d: &'a [u8],
    pub(super) at: usize,
}

impl R<'_> {
    pub(super) fn u8(&mut self) -> Option<u8> {
        let b = *self.d.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    pub(super) fn u32(&mut self) -> Option<u32> {
        let v = u32le(self.d, self.at)?;
        self.at += 4;
        Some(v)
    }

    pub(super) fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    pub(super) fn u64(&mut self) -> Option<u64> {
        let v = u64le(self.d, self.at)?;
        self.at += 8;
        Some(v)
    }

    pub(super) fn skip(&mut self, n: usize) -> Option<()> {
        self.at = self.at.checked_add(n).filter(|&e| e <= self.d.len())?;
        Some(())
    }

    /// An `FString`, as UTF-8 (lossy, of UTF-16 ones), up to its NUL.
    pub(super) fn string(&mut self) -> Option<Vec<u8>> {
        let n = self.i32()?;
        let s = match n {
            0.. => {
                let b = self.d.get(self.at..self.at.checked_add(n as usize)?)?;
                self.at += n as usize;
                b.to_vec()
            }
            _ => {
                let n = (n as i64).unsigned_abs() as usize;
                let b = self.d.get(self.at..self.at.checked_add(n.checked_mul(2)?)?)?;
                self.at += 2 * n;
                utf16_lossy(b).into_bytes()
            }
        };
        Some(s.split(|&b| b == 0).next().unwrap_or(&[]).to_vec())
    }
}
