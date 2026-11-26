//! The columns of a folder of an archive: all that `aqr -t -v` tells of an entry but its
//! formula. Those of the system's properties (name, dates, type, size, attributes, owner,
//! comment, a link's target) are keyed so, named and shown in the user's language by the shell;
//! the others (the archive's: its data and other blocks, ratio, segment; permissions, macOS'
//! flags, the flags of `-t`, the time of a change of metadata) by properties of ours
//! ([`OURS`]), named by aqr-l10n. Name, date modified, type and size are shown by default; the
//! others are chosen in the view's "More…".

use serde_json::Value;
use windows::Win32::Foundation::{FILETIME, PROPERTYKEY, SysAllocStringLen};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Variant::{InitVariantFromFileTime, VARIANT};
use windows::Win32::UI::Shell::Common::{
    SHCOLSTATE, SHCOLSTATE_ONBYDEFAULT, SHCOLSTATE_SECONDARYUI, SHCOLSTATE_TYPE_DATE, SHCOLSTATE_TYPE_INT,
    SHCOLSTATE_TYPE_STR,
};
use windows::Win32::UI::Shell::{
    SHFILEINFOW, SHFormatDateTimeW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, StrCmpLogicalW,
    StrFormatKBSizeW,
};
use windows::core::{GUID, PCWSTR};

use crate::pidl::{Item, Kind};

/// The storage properties (`FMTID_Storage`).
const STORAGE: GUID = GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac);
/// `PKEY_FileOwner`'s.
const OWNER: GUID = GUID::from_u128(0x9b174b34_40ff_11d2_a27e_00c04fc30871);
/// `PKEY_Comment`'s (the summary information).
const SUMMARY: GUID = GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9);
/// `PKEY_Link_TargetParsingPath`'s.
const LINK: GUID = GUID::from_u128(0xb9b4b3fc_2b51_4a42_b5d8_324146afcf25);
/// The properties of ours.
pub const OURS: GUID = GUID::from_u128(0xd973fccd_add6_4146_a7d7_5d7c05ef84f0);

/// What a column tells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum What {
    Name,
    Modified,
    Type,
    Size,
    Data,
    Overhead,
    Ratio,
    Segment,
    Owner,
    Mode,
    Attributes,
    Mac,
    Flags,
    Created,
    Accessed,
    Changed,
    Comment,
    Target,
}

/// What a column takes of an entry: its item ID alone, what the server tells of it, or that and
/// what `-t -v` tells (all of the archive's blocks read for it).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Need {
    Item,
    Entry,
    Verbose,
}

pub struct Column {
    pub what: What,
    pub key: PROPERTYKEY,
    pub state: SHCOLSTATE,
    pub width: i32,
    pub right: bool,
    pub title: fn() -> &'static str,
}

const fn key(fmtid: GUID, pid: u32) -> PROPERTYKEY {
    PROPERTYKEY { fmtid, pid }
}

const fn state(kind: SHCOLSTATE, shown: bool) -> SHCOLSTATE {
    SHCOLSTATE(kind.0 | if shown { SHCOLSTATE_ONBYDEFAULT.0 } else { SHCOLSTATE_SECONDARYUI.0 })
}

use aqr_l10n::folder as l;

pub const COLUMNS: [Column; 18] = [
    Column {
        what: What::Name,
        key: key(STORAGE, 10),
        state: state(SHCOLSTATE_TYPE_STR, true),
        width: 30,
        right: false,
        title: l::col_name,
    },
    Column {
        what: What::Modified,
        key: key(STORAGE, 14),
        state: state(SHCOLSTATE_TYPE_DATE, true),
        width: 18,
        right: false,
        title: l::col_modified,
    },
    Column {
        what: What::Type,
        key: key(STORAGE, 4),
        state: state(SHCOLSTATE_TYPE_STR, true),
        width: 18,
        right: false,
        title: l::col_type,
    },
    Column {
        what: What::Size,
        key: key(STORAGE, 12),
        state: state(SHCOLSTATE_TYPE_INT, true),
        width: 10,
        right: true,
        title: l::col_size,
    },
    Column {
        what: What::Data,
        key: key(OURS, 2),
        state: state(SHCOLSTATE_TYPE_INT, false),
        width: 10,
        right: true,
        title: l::col_data,
    },
    Column {
        what: What::Overhead,
        key: key(OURS, 3),
        state: state(SHCOLSTATE_TYPE_INT, false),
        width: 10,
        right: true,
        title: l::col_overhead,
    },
    Column {
        what: What::Ratio,
        key: key(OURS, 4),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 7,
        right: true,
        title: l::col_ratio,
    },
    Column {
        what: What::Segment,
        key: key(OURS, 5),
        state: state(SHCOLSTATE_TYPE_INT, false),
        width: 5,
        right: true,
        title: l::col_segment,
    },
    Column {
        what: What::Owner,
        key: key(OWNER, 4),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 16,
        right: false,
        title: l::col_owner,
    },
    Column {
        what: What::Mode,
        key: key(OURS, 6),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 10,
        right: false,
        title: l::col_mode,
    },
    Column {
        what: What::Attributes,
        key: key(STORAGE, 13),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 6,
        right: false,
        title: l::col_attributes,
    },
    Column {
        what: What::Mac,
        key: key(OURS, 7),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 12,
        right: false,
        title: l::col_mac,
    },
    Column {
        what: What::Flags,
        key: key(OURS, 8),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 7,
        right: false,
        title: l::col_flags,
    },
    Column {
        what: What::Created,
        key: key(STORAGE, 15),
        state: state(SHCOLSTATE_TYPE_DATE, false),
        width: 18,
        right: false,
        title: l::col_created,
    },
    Column {
        what: What::Accessed,
        key: key(STORAGE, 16),
        state: state(SHCOLSTATE_TYPE_DATE, false),
        width: 18,
        right: false,
        title: l::col_accessed,
    },
    Column {
        what: What::Changed,
        key: key(OURS, 9),
        state: state(SHCOLSTATE_TYPE_DATE, false),
        width: 18,
        right: false,
        title: l::col_changed,
    },
    Column {
        what: What::Comment,
        key: key(SUMMARY, 6),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 20,
        right: false,
        title: l::col_comment,
    },
    Column {
        what: What::Target,
        key: key(LINK, 2),
        state: state(SHCOLSTATE_TYPE_STR, false),
        width: 20,
        right: false,
        title: l::col_target,
    },
];

/// The column of property `key`.
pub fn of_key(key: &PROPERTYKEY) -> Option<&'static Column> {
    COLUMNS.iter().find(|c| c.key.fmtid == key.fmtid && c.key.pid == key.pid)
}

impl What {
    pub fn needs(self) -> Need {
        match self {
            What::Name | What::Modified | What::Type | What::Size | What::Attributes => Need::Item,
            What::Data | What::Overhead | What::Ratio | What::Segment => Need::Verbose,
            _ => Need::Entry,
        }
    }
}

/// What a column tells of an entry.
#[derive(Clone, PartialEq, Debug)]
pub enum Cell {
    None,
    Text(String),
    Bytes(u64),
    Number(u64),
    /// A `FILETIME`.
    Date(i64),
    /// A share of a size, as shown.
    Ratio(u64, u64, String),
}

/// A time of `entry` (`[seconds, nanoseconds]` since the epoch) as a `FILETIME`.
fn time_of(v: &Value) -> Option<i64> {
    let (s, n) = (v[0].as_i64()?, v[1].as_u64()?);
    Some(((s as i128 + 11_644_473_600) * 10_000_000 + n as i128 / 100).clamp(1, i64::MAX as i128) as i64)
}

/// Column `what` of `item` (and of `entry`, as the server tells it, where the column needs it).
pub fn cell(item: &Item, entry: Option<&Value>, what: What) -> Cell {
    let text = |k: &str| entry.and_then(|e| e[k].as_str()).map_or(Cell::None, |s| Cell::Text(s.to_owned()));
    let number = |k: &str| entry.and_then(|e| e[k].as_u64());
    let date = |k: &str| entry.and_then(|e| time_of(&e[k])).map_or(Cell::None, Cell::Date);
    let file = item.kind == Kind::File;
    match what {
        What::Name => Cell::Text(item.name.clone()),
        What::Modified if item.mtime != 0 => Cell::Date(item.mtime),
        What::Modified => Cell::None,
        What::Type => Cell::Text(type_text(item)),
        What::Size if file => Cell::Bytes(item.size),
        What::Data if file => number("data").map_or(Cell::None, Cell::Bytes),
        What::Overhead if file => number("overhead").map_or(Cell::None, Cell::Bytes),
        What::Ratio if file => match (number("share"), entry.and_then(|e| e["ratio"].as_str())) {
            (Some(share), Some(r)) => Cell::Ratio(share, item.size, r.to_owned()),
            _ => Cell::None,
        },
        What::Segment if file => number("segment").map_or(Cell::None, Cell::Number),
        What::Size | What::Data | What::Overhead | What::Ratio | What::Segment => Cell::None,
        What::Owner => text("owner"),
        What::Mode => text("mode"),
        // (as Explorer shows them, and of `-t -v`'s `T` and `I`)
        What::Attributes => {
            let a: String = [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x20, 'A'), (0x100, 'T'), (0x2000, 'I')]
                .iter()
                .filter(|(b, _)| item.attrs & b != 0)
                .map(|(_, c)| *c)
                .collect();
            if a.is_empty() { Cell::None } else { Cell::Text(a) }
        }
        What::Mac => text("mac"),
        What::Flags => text("flags"),
        What::Created => date("crtime"),
        What::Accessed => date("atime"),
        What::Changed => date("ctime"),
        What::Comment => text("comment"),
        What::Target => text("target"),
    }
}

impl Cell {
    /// As it is shown in the user's language.
    pub fn shown(&self) -> String {
        match self {
            Cell::None => String::new(),
            Cell::Text(t) | Cell::Ratio(_, _, t) => t.clone(),
            Cell::Number(n) => n.to_string(),
            Cell::Bytes(n) => {
                let mut buf = [0u16; 64];
                // SAFETY: a buffer of its length
                match unsafe { StrFormatKBSizeW(*n as i64, &mut buf) } {
                    p if p.is_null() => n.to_string(),
                    _ => String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]),
                }
            }
            Cell::Date(t) => {
                let mut buf = [0u16; 128];
                // FDTF_SHORTTIME | FDTF_SHORTDATE | FDTF_NOAUTOREADINGORDER (no marks of direction)
                let mut flags = 0x1 | 0x2 | 0x400;
                // SAFETY: a FILETIME and a buffer of its length
                unsafe { SHFormatDateTimeW(&filetime(*t), Some(&mut flags), &mut buf) };
                String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])
            }
        }
    }

    /// As a property's value (`what`'s: the attributes as their bits).
    pub fn variant(&self, item: &Item, what: What) -> windows::core::Result<VARIANT> {
        Ok(match (self, what) {
            (_, What::Attributes) => variant(19, item.attrs as u64), // VT_UI4
            (Cell::None, _) => variant(0, 0),
            (Cell::Text(t) | Cell::Ratio(_, _, t), _) => variant_text(t),
            (Cell::Bytes(n) | Cell::Number(n), _) => variant(21, *n), // VT_UI8
            // SAFETY: a FILETIME
            (Cell::Date(t), _) => unsafe { InitVariantFromFileTime(&filetime(*t))? },
        })
    }

    /// Which of two is first (none last, numbers and times as such, text as Explorer sorts it).
    pub fn compare(&self, other: &Cell) -> i32 {
        use std::cmp::Ordering;
        let o = match (self, other) {
            (Cell::None, Cell::None) => Ordering::Equal,
            (Cell::None, _) => Ordering::Greater,
            (_, Cell::None) => Ordering::Less,
            (Cell::Bytes(a) | Cell::Number(a), Cell::Bytes(b) | Cell::Number(b)) => a.cmp(b),
            (Cell::Date(a), Cell::Date(b)) => a.cmp(b),
            (Cell::Ratio(a, x, _), Cell::Ratio(b, y, _)) => (*a as u128 * *y as u128).cmp(&(*b as u128 * *x as u128)),
            (a, b) => return logical(&a.shown(), &b.shown()),
        };
        o as i32
    }
}

/// Of two names, which is first as Explorer sorts them (the numbers in them as numbers).
pub fn logical(a: &str, b: &str) -> i32 {
    let (wa, wb): (Vec<u16>, Vec<u16>) = (a.encode_utf16().chain([0]).collect(), b.encode_utf16().chain([0]).collect());
    // SAFETY: two strings ended by 0
    unsafe { StrCmpLogicalW(PCWSTR(wa.as_ptr()), PCWSTR(wb.as_ptr())) }.signum()
}

/// What Explorer calls the type of `item` (of its extension, as of a file of that name).
pub fn type_text(item: &Item) -> String {
    let w: Vec<u16> = item.name.encode_utf16().chain([0]).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: a name ended by 0, and a structure of its size
    unsafe {
        SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            file_attributes(item),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
    String::from_utf16_lossy(&info.szTypeName[..n])
}

pub fn file_attributes(item: &Item) -> FILE_FLAGS_AND_ATTRIBUTES {
    match item.is_dir() || item.kind == crate::pidl::Kind::Version {
        true => FILE_ATTRIBUTE_DIRECTORY,
        false => FILE_FLAGS_AND_ATTRIBUTES(FILE_ATTRIBUTE_NORMAL.0 | (item.attrs & 0x7)),
    }
}

fn filetime(t: i64) -> FILETIME {
    FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t as u64 >> 32) as u32 }
}

/// `VARIANT`, as it is laid out.
#[repr(C)]
struct Var {
    vt: u16,
    reserved: [u16; 3],
    value: u64,
    record: usize,
}

fn variant(vt: u16, value: u64) -> VARIANT {
    // SAFETY: of the same layout
    unsafe { std::mem::transmute(Var { vt, reserved: [0; 3], value, record: 0 }) }
}

fn variant_text(s: &str) -> VARIANT {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: a BSTR made of `w`, the VARIANT's
    let b = unsafe { SysAllocStringLen(Some(&w)) };
    variant(8, unsafe { std::mem::transmute::<windows::core::BSTR, usize>(b) } as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells() {
        let item = Item { kind: Kind::File, size: 1000, mtime: 0, attrs: 0x21, name: "a.txt".into() };
        let e = serde_json::json!({"data": 300, "overhead": 20, "share": 320, "ratio": "32.0%", "segment": 1,
            "owner": "u:g", "flags": "-r-----", "ctime": [0, 0]});
        assert_eq!(cell(&item, Some(&e), What::Data), Cell::Bytes(300));
        assert_eq!(cell(&item, Some(&e), What::Ratio), Cell::Ratio(320, 1000, "32.0%".into()));
        assert_eq!(cell(&item, Some(&e), What::Owner), Cell::Text("u:g".into()));
        assert_eq!(cell(&item, Some(&e), What::Attributes), Cell::Text("RA".into()));
        assert_eq!(cell(&item, Some(&e), What::Changed), Cell::Date(116_444_736_000_000_000));
        assert_eq!(cell(&item, None, What::Mode), Cell::None);
        // (a ratio by its share of the size, none last)
        let half = Cell::Ratio(1, 2, String::new());
        assert_eq!(half.compare(&Cell::Ratio(2, 3, String::new())), -1);
        assert_eq!(Cell::None.compare(&half), 1);
        // (every column of a key of its own, and found by it)
        for c in &COLUMNS {
            assert_eq!(of_key(&c.key).map(|f| f.what), Some(c.what));
        }
    }
}
