//! The item IDs of entries of an archive: one `SHITEMID` for each, under the file system's ID
//! list of the archive file, the path inside the archive the chain of them. Item IDs stay in
//! favourites, recent items and jump lists, so their layout is fixed and versioned:
//!
//! ```text
//! u16 cb | u32 magic 'AQRI' | u8 version | u8 kind (dir, file, link, other)
//! | u64 size | i64 mtime (FILETIME) | u32 attributes (Windows')
//! | u16 name length | name (UTF-16, a name of the path)
//! ```
//!
//! An entry is told by its name alone (a content's index changes as the archive is appended to);
//! the size and time are copies, to fill the columns at once.

use serde_json::Value;
use windows::Win32::System::Com::CoTaskMemAlloc;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;

/// `AQRI`, little-endian.
pub const MAGIC: u32 = u32::from_le_bytes(*b"AQRI");
pub const VERSION: u8 = 1;

/// The bytes before the name (`cb` included).
const HEAD: usize = 2 + 4 + 1 + 1 + 8 + 8 + 4 + 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dir = 0,
    File = 1,
    Link = 2,
    Other = 3,
}

/// An entry, as an item ID holds it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub kind: Kind,
    pub size: u64,
    /// A `FILETIME` (0: none told).
    pub mtime: i64,
    pub attrs: u32,
    pub name: String,
}

impl Item {
    /// The entry `v` as `aqr --serve` tells it (`list`, `stat`).
    pub fn of(v: &Value) -> Item {
        let kind = match v["kind"].as_str() {
            Some("dir") => Kind::Dir,
            Some("file") => Kind::File,
            Some("link") => Kind::Link,
            _ => Kind::Other,
        };
        let mtime = match (v["mtime"][0].as_i64(), v["mtime"][1].as_u64()) {
            (Some(s), Some(n)) => {
                ((s as i128 + 11_644_473_600) * 10_000_000 + n as i128 / 100).clamp(0, i64::MAX as i128) as i64
            }
            _ => 0,
        };
        Item {
            kind,
            size: v["size"].as_u64().unwrap_or(0),
            mtime,
            attrs: v["attrs"].as_u64().unwrap_or(0) as u32,
            name: v["name"].as_str().unwrap_or("").to_owned(),
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    /// Its `SHITEMID`'s bytes, `cb` first.
    pub fn encode(&self) -> Vec<u8> {
        let name: Vec<u16> = self.name.encode_utf16().collect();
        let n = HEAD + 2 * name.len();
        let mut b = Vec::with_capacity(n);
        b.extend_from_slice(&(n as u16).to_le_bytes());
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.push(VERSION);
        b.push(self.kind as u8);
        b.extend_from_slice(&self.size.to_le_bytes());
        b.extend_from_slice(&self.mtime.to_le_bytes());
        b.extend_from_slice(&self.attrs.to_le_bytes());
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        name.iter().for_each(|u| b.extend_from_slice(&u.to_le_bytes()));
        b
    }

    /// The item of `SHITEMID` bytes `b` (`cb` first), where it is one of these.
    pub fn decode(b: &[u8]) -> Option<Item> {
        if b.len() < HEAD || u16::from_le_bytes([b[0], b[1]]) as usize != b.len() {
            return None;
        }
        let u32_at = |k: usize| u32::from_le_bytes(b[k..k + 4].try_into().unwrap());
        let u64_at = |k: usize| u64::from_le_bytes(b[k..k + 8].try_into().unwrap());
        if u32_at(2) != MAGIC || b[6] != VERSION {
            return None;
        }
        let kind = match b[7] {
            0 => Kind::Dir,
            1 => Kind::File,
            2 => Kind::Link,
            _ => Kind::Other,
        };
        let n = u16::from_le_bytes([b[28], b[29]]) as usize;
        if b.len() != HEAD + 2 * n {
            return None;
        }
        let name: Vec<u16> = b[HEAD..].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
        Some(Item {
            kind,
            size: u64_at(8),
            mtime: u64_at(16) as i64,
            attrs: u32_at(24),
            name: String::from_utf16(&name).ok()?,
        })
    }
}

/// The `SHITEMID`s of ID list `p` (each with its `cb`), up to its end.
///
/// # Safety
///
/// `p` is null or an ID list, ended by a `cb` of 0.
pub unsafe fn ids<'a>(p: *const ITEMIDLIST) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    if p.is_null() {
        return out;
    }
    let mut at = p as *const u8;
    // SAFETY: an ID list, each item's first two bytes its length, the last 0
    unsafe {
        loop {
            let cb = u16::from_le_bytes([*at, *at.add(1)]) as usize;
            if cb < 2 {
                return out;
            }
            out.push(std::slice::from_raw_parts(at, cb));
            at = at.add(cb);
        }
    }
}

/// The items of ID list `p` (relative, of this namespace alone): none where any is not one.
///
/// # Safety
///
/// As [`ids`].
pub unsafe fn items(p: *const ITEMIDLIST) -> Option<Vec<Item>> {
    // SAFETY: as the caller's
    unsafe { ids(p) }.into_iter().map(Item::decode).collect()
}

/// The first item of ID list `p`, where it is one of these.
///
/// # Safety
///
/// As [`ids`].
pub unsafe fn first(p: *const ITEMIDLIST) -> Option<Item> {
    // SAFETY: as the caller's
    unsafe { ids(p) }.first().and_then(|b| Item::decode(b))
}

/// An ID list of the bytes of `ids` (each an `SHITEMID`), of COM's memory: the caller's to free.
pub fn alloc(ids: &[&[u8]]) -> *mut ITEMIDLIST {
    let n: usize = ids.iter().map(|b| b.len()).sum::<usize>() + 2;
    // SAFETY: memory of COM's of `n` bytes, filled with the items and the end
    unsafe {
        let p = CoTaskMemAlloc(n) as *mut u8;
        if p.is_null() {
            return std::ptr::null_mut();
        }
        let mut at = 0;
        for b in ids {
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(at), b.len());
            at += b.len();
        }
        *p.add(at) = 0;
        *p.add(at + 1) = 0;
        p as *mut ITEMIDLIST
    }
}

/// An ID list of `items`, of COM's memory.
pub fn of_items(items: &[Item]) -> *mut ITEMIDLIST {
    let enc: Vec<Vec<u8>> = items.iter().map(Item::encode).collect();
    alloc(&enc.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let i = Item {
            kind: Kind::File,
            size: 1 << 40,
            mtime: 133_000_000_000_000_000,
            attrs: 0x21,
            name: "한글 a.txt".into(),
        };
        let b = i.encode();
        assert_eq!(Item::decode(&b), Some(i.clone()));
        let p = of_items(&[i.clone(), Item { name: "b".into(), kind: Kind::Dir, ..i.clone() }]);
        // SAFETY: an ID list just made
        let got = unsafe { items(p) }.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].name.as_str(), got[1].name.as_str(), got[1].kind), ("한글 a.txt", "b", Kind::Dir));
        // SAFETY: of CoTaskMemAlloc
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p as *const _)) };
        // (another namespace's, or damaged)
        assert_eq!(Item::decode(&b[..b.len() - 2]), None);
        let mut other = b.clone();
        other[2] = b'X';
        assert_eq!(Item::decode(&other), None);
    }

    #[test]
    fn of_served() {
        let v = serde_json::json!({"name": "c.txt", "kind": "file", "size": 5, "mtime": [0, 0], "attrs": 1});
        let i = Item::of(&v);
        assert_eq!((i.kind, i.size, i.mtime, i.attrs), (Kind::File, 5, 116_444_736_000_000_000, 1));
    }
}
