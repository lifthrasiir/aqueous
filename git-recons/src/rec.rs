//! What both sides share about files and object records: kinds, the record layout, pack
//! indexes.

use crate::obj::{self, Id};
use crate::rd::{Rd, put_uv};
use crate::{Result, corrupt};

pub const K_RAW: u8 = 0;
pub const K_SAME: u8 = 1;
pub const K_BLOB: u8 = 2;
pub const K_BLOB_CRLF: u8 = 3;
pub const K_LOOSE: u8 = 4;
pub const K_PACK: u8 = 5;
pub const K_IDX: u8 = 6;
pub const K_REV: u8 = 7;
pub const K_GIT: u8 = 8;

/// Type (0: whole entry verbatim; 1-4; 6 ofs-delta; 7 ref-delta).
pub const H_TYPE: u32 = 7;
/// zlib mode: written by zlib at the file's default level (a pack's; a loose object's: 1),
/// or at a level of its own (Z_LEVEL); or the zlib stream as is (Z_VERBATIM, loose objects
/// only: a pack entry kept as is is type 0).
pub const H_ZSHIFT: u32 = 3;
pub const Z_DEFAULT: u8 = 0;
pub const Z_LEVEL: u8 = 1;
pub const Z_VERBATIM: u8 = 3;
/// The table index is explicit (an id seen before).
pub const H_DUP: u32 = 1 << 5;
/// The delta's base size is explicit (not the base's claimed size).
pub const H_XBASE: u32 = 1 << 6;
/// The payload is verbatim (a delta not in git's canonical encoding).
pub const H_PLAIN: u32 = 1 << 7;
/// The delta's literals are plain although its chain is a tree, commit or tag: the base did
/// not resolve.
pub const H_UNTYPED: u32 = 1 << 8;

/// Streams a record has lengths for: ops through text.
pub const REC_FIRST: usize = crate::S_OPS;
pub const REC_N: usize = crate::NSTREAMS - REC_FIRST;

/// An object record (a pack entry or a loose object).
#[derive(Clone, Debug, Default)]
pub struct Rec {
    pub h: u32,
    /// `zz(j - next new index)` with H_DUP.
    pub dup: i64,
    /// The size the object claims: non-delta its size, delta its target size (0 if the
    /// delta does not parse). Not for type 0.
    pub size: u64,
    /// ofs-delta: entries back to the base. Type 0: for an ofs-delta on an entry start,
    /// that + 1; for a ref-delta on an id in the table, `zz(base index - own index) + 1`;
    /// else 0.
    pub dist: u64,
    pub base_size: u64,
    pub level: u8,
    pub lens: [u64; REC_N],
}

impl Rec {
    pub fn typ(&self) -> u8 {
        (self.h & H_TYPE) as u8
    }
    pub fn zmode(&self) -> u8 {
        (self.h >> H_ZSHIFT & 3) as u8
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        put_uv(out, self.h as u64);
        if self.h & H_DUP != 0 {
            recons_common::varint::put_signed(out, self.dup);
        }
        if self.typ() == 0 {
            put_uv(out, self.dist);
        } else {
            put_uv(out, self.size);
            if self.typ() == obj::OFS {
                put_uv(out, self.dist);
            }
            if self.h & H_XBASE != 0 {
                put_uv(out, self.base_size);
            }
            if self.zmode() == Z_LEVEL {
                out.push(self.level);
            }
        }
        let mask = (0..REC_N).fold(0u8, |m, i| m | ((self.lens[i] != 0) as u8) << i);
        out.push(mask);
        for &l in &self.lens {
            if l != 0 {
                put_uv(out, l);
            }
        }
    }

    pub fn read(r: &mut Rd) -> Result<Rec> {
        let mut c = Rec { h: u32::try_from(r.uv()?).map_err(|_| corrupt())?, ..Default::default() };
        if c.h >> 9 != 0 || matches!(c.typ(), 5) {
            return Err(corrupt());
        }
        if c.h & H_DUP != 0 {
            c.dup = r.sv()?;
        }
        if c.typ() == 0 {
            c.dist = r.uv()?;
        } else {
            c.size = r.uv()?;
            if c.typ() == obj::OFS {
                c.dist = r.uv()?;
            }
            if c.h & H_XBASE != 0 {
                c.base_size = r.uv()?;
            }
            match c.zmode() {
                Z_LEVEL => {
                    c.level = r.u8()?;
                    if !(1..=9).contains(&c.level) {
                        return Err(corrupt());
                    }
                }
                2 => return Err(corrupt()),
                _ => {}
            }
        }
        let mask = r.u8()?;
        for i in 0..REC_N {
            if mask & 1 << i != 0 {
                c.lens[i] = r.uv()?;
            }
        }
        Ok(c)
    }
}

/// The size an entry claims, from its bytes: non-delta the header's, delta the target size
/// (0 if unreadable).
pub fn claimed_raw(b: &[u8]) -> u64 {
    let Some((t, size, mut p)) = obj::entry_header_any(b) else { return 0 };
    match t {
        obj::OFS => match obj::parse_ofs(&b[p..]) {
            Some((_, l)) => p += l,
            None => return 0,
        },
        obj::REF => p += 20,
        _ => return size,
    }
    let Some(z) = b.get(p..) else { return 0 };
    obj::delta_sizes(&obj::inflate_prefix(z, 20)).map_or(0, |s| s.1)
}

/// The version 2 index git writes for a pack with these entries (in pack order).
pub fn gen_idx(ids: &[Id], offs: &[u64], crcs: &[u32], trailer: &[u8]) -> Vec<u8> {
    let n = ids.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| ids[a].cmp(&ids[b]));
    let mut out = Vec::with_capacity(8 + 1024 + n * 28 + 40);
    out.extend_from_slice(&[0xff, 0x74, 0x4f, 0x63, 0, 0, 0, 2]);
    let mut fan = [0u32; 256];
    for id in ids {
        fan[id[0] as usize] += 1;
    }
    let mut acc = 0;
    for f in fan {
        acc += f;
        out.extend_from_slice(&acc.to_be_bytes());
    }
    for &k in &order {
        out.extend_from_slice(&ids[k]);
    }
    for &k in &order {
        out.extend_from_slice(&crcs[k].to_be_bytes());
    }
    let mut large = vec![];
    for &k in &order {
        let o = offs[k];
        let v = if o >= 0x8000_0000 {
            large.extend_from_slice(&o.to_be_bytes());
            0x8000_0000 | (large.len() / 8 - 1) as u32
        } else {
            o as u32
        };
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(&large);
    out.extend_from_slice(trailer);
    let h = obj::sha1(&[&out]);
    out.extend_from_slice(&h);
    out
}

/// The reverse index git writes for a pack with these ids (in pack order).
pub fn gen_rev(ids: &[Id], trailer: &[u8]) -> Vec<u8> {
    let n = ids.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| ids[a].cmp(&ids[b]));
    let mut pos = vec![0u32; n];
    for (i, &k) in order.iter().enumerate() {
        pos[k] = i as u32;
    }
    let mut out = Vec::with_capacity(12 + n * 4 + 40);
    out.extend_from_slice(b"RIDX\0\0\0\x01\0\0\0\x01");
    for p in pos {
        out.extend_from_slice(&p.to_be_bytes());
    }
    out.extend_from_slice(trailer);
    let h = obj::sha1(&[&out]);
    out.extend_from_slice(&h);
    out
}

/// `pack-<hex>.pack` (or .idx, .rev): the hex as an id.
pub fn pack_name_id(path: &str) -> Option<Id> {
    let name = path.rsplit('/').next()?;
    let hex = name.strip_prefix("pack-")?.split('.').next()?;
    obj::parse_hex(hex.as_bytes())
}

/// Whether a path lies in a git directory (a `.git` component before the last).
pub fn in_git_dir(path: &str) -> bool {
    let mut it = path.split('/');
    it.next_back();
    it.any(|c| c == ".git")
}

/// A loose object's path: the directory part (through `objects/`) and the id.
pub fn loose_path(path: &str) -> Option<(&str, Id)> {
    if !in_git_dir(path) {
        return None;
    }
    let b = path.as_bytes();
    if b.len() < 49 || b[b.len() - 39] != b'/' {
        return None;
    }
    let dir = &path[..path.len() - 41];
    if !dir.ends_with("/objects/") {
        return None;
    }
    let mut hex = [0u8; 40];
    hex[..2].copy_from_slice(&b[b.len() - 41..b.len() - 39]);
    hex[2..].copy_from_slice(&b[b.len() - 38..]);
    Some((dir, obj::parse_hex(&hex)?))
}

pub fn loose_path_of(dir: &str, id: &Id) -> String {
    let h = obj::hex(id);
    format!("{dir}{}/{}", &h[..2], &h[2..])
}
