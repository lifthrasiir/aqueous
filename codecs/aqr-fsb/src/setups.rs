//! The Vorbis setup database (FORMAT.md §3), assembling a setup from it (§4) and finding one by its CRC-32 (§5).

use aqr_common::crc32;
use aqr_common::varint;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::OnceLock;

/// Why a database was rejected, or a setup could not be assembled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The database breaks §3.
    Malformed(&'static str),
    /// A skeleton and ends that do not make a setup (§4.2).
    Assemble(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(s) => write!(f, "malformed Vorbis setup database: {s}"),
            Error::Assemble(s) => write!(f, "cannot assemble a Vorbis setup: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<aqr_common::Corrupt> for Error {
    fn from(e: aqr_common::Corrupt) -> Error {
        Error::Malformed(e.0)
    }
}

const MAGIC: &[u8; 4] = b"AQVS";
const VERSION: u8 = 1;
/// Kinds of parts, in the order of the database: codebooks, floors, residues, mappings, modes.
const KINDS: usize = 5;
const RESIDUE: usize = 2;
/// Bits a residue part has up to and including its partition size (§4.1).
const RESIDUE_MIN_BITS: u64 = 88;
/// Residues' ends are below this (§3).
const END_LIMIT: u64 = 1 << 24;
/// The search's largest `end` (§5.2); it has bits 0..=11.
const SEARCH_MAX_END: u32 = 2048;
const SEARCH_END_BITS: usize = 12;

/// A run of bits, §1's way: `bytes` holds the bit string little-endian.
#[derive(Debug)]
struct Part {
    bits: usize,
    bytes: Vec<u8>,
}

/// Which parts a setup is made of: ids into each kind's part list.
#[derive(Debug)]
pub struct Skeleton {
    ids: [Vec<u32>; KINDS],
}

impl Skeleton {
    /// The number of residues, hence of `end`s a setup of this skeleton needs.
    pub fn residues(&self) -> usize {
        self.ids[RESIDUE].len()
    }
}

/// A CRC-32 and the setup it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub crc: u32,
    pub skeleton: usize,
    pub ends: Vec<u32>,
}

/// A setup that may have the CRC-32 sought: a skeleton and its ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub skeleton: usize,
    pub ends: Vec<u32>,
}

/// The parsed database.
#[derive(Debug)]
pub struct Setups {
    parts: [Vec<Part>; KINDS],
    skeletons: Vec<Skeleton>,
    rows: Vec<Row>,
}

/// The built-in database, parsed on first use.
pub fn database() -> Result<&'static Setups, Error> {
    static DB: OnceLock<Result<Setups, Error>> = OnceLock::new();
    DB.get_or_init(|| Setups::parse(include_bytes!("../data/vorbis-setups.bin"))).as_ref().map_err(Clone::clone)
}

fn get_count(src: &[u8], at: &mut usize) -> Result<usize, Error> {
    // Every counted item takes at least a byte, so a count above the rest of the file is bad.
    let n = varint::get(src, at)?;
    if n > (src.len() - *at) as u64 {
        return Err(Error::Malformed("count past the end"));
    }
    Ok(n as usize)
}

fn parse_parts(src: &[u8], at: &mut usize, kind: usize) -> Result<Vec<Part>, Error> {
    let count = get_count(src, at)?;
    let mut parts = Vec::with_capacity(count);
    let mut seen = HashSet::new();
    for _ in 0..count {
        let n = varint::get(src, at)?;
        if n == 0 {
            return Err(Error::Malformed("empty part"));
        }
        let len = n / 8 + (n % 8 != 0) as u64;
        if len > (src.len() - *at) as u64 {
            return Err(Error::Malformed("part past the end"));
        }
        let len = len as usize;
        let bytes = &src[*at..*at + len];
        *at += len;
        if n % 8 != 0 && bytes[len - 1] >> (n % 8) != 0 {
            return Err(Error::Malformed("part has bits set beyond its length"));
        }
        if kind == RESIDUE && (n < RESIDUE_MIN_BITS || bytes[5..8] != [0; 3]) {
            return Err(Error::Malformed("residue part too short or with an end"));
        }
        if !seen.insert((n, bytes)) {
            return Err(Error::Malformed("duplicate part"));
        }
        parts.push(Part { bits: n as usize, bytes: bytes.to_vec() });
    }
    Ok(parts)
}

impl Setups {
    /// Parses and validates a database (§3).
    pub fn parse(src: &[u8]) -> Result<Setups, Error> {
        if src.len() < 5 || &src[..4] != MAGIC || src[4] != VERSION {
            return Err(Error::Malformed("bad magic or version"));
        }
        let mut at = 5;
        let mut parts: [Vec<Part>; KINDS] = Default::default();
        for (kind, list) in parts.iter_mut().enumerate() {
            *list = parse_parts(src, &mut at, kind)?;
        }
        let nskel = get_count(src, &mut at)?;
        let mut skeletons = Vec::with_capacity(nskel);
        for _ in 0..nskel {
            let mut ids: [Vec<u32>; KINDS] = Default::default();
            for (kind, list) in ids.iter_mut().enumerate() {
                let max = if kind == 0 { 256 } else { 64 };
                let count = varint::get(src, &mut at)?;
                if count == 0 || count > max {
                    return Err(Error::Malformed("id count out of range"));
                }
                for _ in 0..count {
                    let id = varint::get(src, &mut at)?;
                    if id >= parts[kind].len() as u64 {
                        return Err(Error::Malformed("part id out of range"));
                    }
                    list.push(id as u32);
                }
            }
            skeletons.push(Skeleton { ids });
        }
        let nrows = get_count(src, &mut at)?;
        let mut rows: Vec<Row> = Vec::with_capacity(nrows);
        for _ in 0..nrows {
            let crc = src.get(at..at + 4).ok_or(Error::Malformed("row past the end"))?;
            let crc = u32::from_le_bytes(crc.try_into().unwrap());
            at += 4;
            if rows.last().is_some_and(|r| r.crc >= crc) {
                return Err(Error::Malformed("rows not sorted"));
            }
            let skeleton = varint::get(src, &mut at)?;
            let skel = skeletons.get(skeleton as usize).ok_or(Error::Malformed("skeleton number out of range"))?;
            let mut ends = Vec::with_capacity(skel.residues());
            for _ in 0..skel.residues() {
                let end = varint::get(src, &mut at)?;
                if end >= END_LIMIT {
                    return Err(Error::Malformed("end out of range"));
                }
                ends.push(end as u32);
            }
            rows.push(Row { crc, skeleton: skeleton as usize, ends });
        }
        if at != src.len() {
            return Err(Error::Malformed("bytes after the last row"));
        }
        Ok(Setups { parts, skeletons, rows })
    }

    pub fn skeletons(&self) -> &[Skeleton] {
        &self.skeletons
    }

    /// The rows, sorted by CRC-32.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row of a CRC-32 (§5.1).
    pub fn row(&self, crc: u32) -> Option<&Row> {
        self.rows.binary_search_by_key(&crc, |r| r.crc).ok().map(|i| &self.rows[i])
    }

    /// Assembles the setup header of a skeleton and its ends (§4.2).
    pub fn assemble(&self, skeleton: usize, ends: &[u32]) -> Result<Vec<u8>, Error> {
        self.assemble_at(skeleton, ends).map(|(bytes, _)| bytes)
    }

    /// As `assemble`, with the bit position at which each residue part begins.
    fn assemble_at(&self, skeleton: usize, ends: &[u32]) -> Result<(Vec<u8>, Vec<usize>), Error> {
        let skel = self.skeletons.get(skeleton).ok_or(Error::Assemble("skeleton out of range"))?;
        if ends.len() != skel.residues() {
            return Err(Error::Assemble("wrong number of ends"));
        }
        if ends.iter().any(|&e| u64::from(e) >= END_LIMIT) {
            return Err(Error::Assemble("end out of range"));
        }
        let mut w = BitWriter::default();
        w.put(5, 8);
        for &b in b"vorbis" {
            w.put(b.into(), 8);
        }
        let mut residue_at = Vec::new();
        for (kind, ids) in skel.ids.iter().enumerate() {
            if kind == 0 {
                w.put(ids.len() as u64 - 1, 8);
            } else {
                if kind == 1 {
                    // The time-domain placeholder, before the floors.
                    w.put(0, 6);
                    w.put(0, 16);
                }
                w.put(ids.len() as u64 - 1, 6);
            }
            for (i, &id) in ids.iter().enumerate() {
                let part = &self.parts[kind][id as usize];
                if kind == RESIDUE {
                    residue_at.push(w.len);
                    let mut bytes = part.bytes.clone();
                    bytes[5..8].copy_from_slice(&ends[i].to_le_bytes()[..3]);
                    w.put_bytes(&bytes, part.bits);
                } else {
                    w.put_bytes(&part.bytes, part.bits);
                }
            }
        }
        w.put(1, 1);
        Ok((w.buf, residue_at))
    }

    /// Assembles the setup of a row.
    pub fn assemble_row(&self, row: &Row) -> Result<Vec<u8>, Error> {
        self.assemble(row.skeleton, &row.ends)
    }

    /// The setups that may have this CRC-32: its row if it has one, else the search's candidates (§5).
    pub fn lookup(&self, crc: u32) -> Vec<Candidate> {
        match self.row(crc) {
            Some(r) => vec![Candidate { skeleton: r.skeleton, ends: r.ends.clone() }],
            None => self.search(crc),
        }
    }

    /// Searches all skeletons for setups with this CRC-32 (§5.2), ignoring the rows.
    pub fn search(&self, crc: u32) -> Vec<Candidate> {
        let mut out = Vec::new();
        for skeleton in 0..self.skeletons.len() {
            self.search_skeleton(skeleton, crc, &mut out);
        }
        out
    }

    fn search_skeleton(&self, skeleton: usize, crc: u32, out: &mut Vec<Candidate>) {
        let skel = &self.skeletons[skeleton];
        let (x0, at) = self.assemble_at(skeleton, &vec![0; skel.residues()]).expect("validated skeleton");
        let zero = vec![0u8; x0.len()];
        let crc_zero = crc32::hash(&zero);
        // crc(setup) = crc(x0) ^ D_0(e_0) ^ D_1(e_1) ^ ..., each D_i linear in e_i.
        let target = crc ^ crc32::hash(&x0);
        // Per residue: the candidate ends with their D.
        let lists: Vec<Vec<(u32, u32)>> = skel.ids[RESIDUE]
            .iter()
            .zip(&at)
            .map(|(&id, &start)| {
                let bytes = &self.parts[RESIDUE][id as usize].bytes;
                let psize = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], 0]) + 1;
                let mut basis = [0u32; SEARCH_END_BITS];
                let mut m = zero.clone();
                for (b, d) in basis.iter_mut().enumerate() {
                    let bit = start + 40 + b;
                    m[bit / 8] = 1 << (bit % 8);
                    *d = crc32::hash(&m) ^ crc_zero;
                    m[bit / 8] = 0;
                }
                (0..=SEARCH_MAX_END / psize)
                    .map(|k| {
                        let e = k * psize;
                        let d = (0..SEARCH_END_BITS).filter(|b| e >> b & 1 != 0).fold(0, |a, b| a ^ basis[b]);
                        (e, d)
                    })
                    .collect()
            })
            .collect();
        // Meet in the middle: the first half of the residues against the rest.
        let (left, right) = lists.split_at(lists.len() / 2);
        let mut table: HashMap<u32, Vec<Vec<u32>>> = HashMap::new();
        for (d, es) in combinations(right) {
            table.entry(d).or_default().push(es);
        }
        for (d, les) in combinations(left) {
            for res in table.get(&(target ^ d)).into_iter().flatten() {
                let mut ends = les.clone();
                ends.extend_from_slice(res);
                out.push(Candidate { skeleton, ends });
            }
        }
    }
}

/// All choices of one (end, D) per list, as the XOR of the Ds and the ends.
fn combinations(lists: &[Vec<(u32, u32)>]) -> Vec<(u32, Vec<u32>)> {
    let mut acc = vec![(0u32, Vec::new())];
    for list in lists {
        acc = acc
            .iter()
            .flat_map(|(d, es)| {
                list.iter().map(move |&(e, de)| {
                    let mut es = es.clone();
                    es.push(e);
                    (d ^ de, es)
                })
            })
            .collect();
    }
    acc
}

/// Bit strings, least significant bit first.
#[derive(Default)]
struct BitWriter {
    buf: Vec<u8>,
    len: usize,
}

impl BitWriter {
    /// Appends the low `n` bits of `v`.
    fn put(&mut self, mut v: u64, mut n: usize) {
        while n > 0 {
            let off = self.len % 8;
            if off == 0 {
                self.buf.push(0);
            }
            let take = (8 - off).min(n);
            *self.buf.last_mut().unwrap() |= ((v & ((1 << take) - 1)) as u8) << off;
            v >>= take;
            n -= take;
            self.len += take;
        }
    }

    /// Appends the first `bits` bits of a little-endian bit string.
    fn put_bytes(&mut self, bytes: &[u8], bits: usize) {
        for (i, &b) in bytes.iter().enumerate() {
            self.put(b.into(), (bits - i * 8).min(8));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn db() -> &'static Setups {
        database().expect("built-in database")
    }

    #[test]
    fn rows_have_their_crc() {
        let db = db();
        assert_eq!(db.rows().len(), 260);
        for row in db.rows() {
            let setup = db.assemble_row(row).unwrap();
            assert_eq!(crc32::hash(&setup), row.crc, "{:08x}", row.crc);
        }
    }

    #[test]
    fn setups_are_structured() {
        let db = db();
        for row in db.rows() {
            let s = db.assemble_row(row).unwrap();
            assert_eq!(&s[..7], b"\x05vorbis");
            // The framing bit is the highest set bit of the last byte, so the last byte is not zero.
            assert_ne!(*s.last().unwrap(), 0);
        }
    }

    #[test]
    fn search_finds_rows() {
        let db = db();
        let t = Instant::now();
        let mut multi = 0;
        for row in db.rows() {
            let found = db.search(row.crc);
            assert!(
                found.contains(&Candidate { skeleton: row.skeleton, ends: row.ends.clone() }),
                "{:08x} not found",
                row.crc
            );
            for c in &found {
                assert_eq!(crc32::hash(&db.assemble(c.skeleton, &c.ends).unwrap()), row.crc);
            }
            multi += (found.len() > 1) as usize;
        }
        println!("rows with several candidates: {multi} of {}; {:?} for 260 searches", db.rows().len(), t.elapsed());
    }

    #[test]
    fn search_miss_is_fast() {
        let db = db();
        let crc = (1u32..).map(|i| i.wrapping_mul(0x9e37_79b9)).find(|&c| db.row(c).is_none()).unwrap();
        let t = Instant::now();
        let found = db.search(crc);
        let dt = t.elapsed();
        println!("miss {crc:08x}: {} candidates in {dt:?}", found.len());
        assert!(dt.as_secs_f64() < 1.0);
        assert_eq!(db.lookup(crc), found);
        let row = &db.rows()[0];
        assert_eq!(db.lookup(row.crc), vec![Candidate { skeleton: row.skeleton, ends: row.ends.clone() }]);
    }

    #[test]
    fn assemble_checks_arguments() {
        let db = db();
        assert!(db.assemble(usize::MAX, &[]).is_err());
        assert!(db.assemble(0, &[]).is_err());
        let n = db.skeletons()[0].residues();
        assert!(db.assemble(0, &vec![1 << 24; n]).is_err());
    }

    #[test]
    fn malformed_is_rejected() {
        let src = include_bytes!("../data/vorbis-setups.bin");
        assert!(Setups::parse(src).is_ok());
        for cut in (0..src.len()).step_by(97).chain([0, 1, 4, 5, 6, 100, src.len() - 1, src.len() - 2]) {
            assert!(Setups::parse(&src[..cut]).is_err(), "cut at {cut}");
        }
        for i in [0, 3, 4] {
            let mut bad = src.to_vec();
            bad[i] ^= 1;
            assert!(Setups::parse(&bad).is_err());
        }
        let mut bad = src.to_vec();
        bad.push(0);
        assert!(Setups::parse(&bad).is_err());
        // Garbage must never panic.
        let mut x = 12345u32;
        for _ in 0..2000 {
            let mut bad = src.to_vec();
            for _ in 0..3 {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                let at = (x >> 8) as usize % bad.len();
                bad[at] = (x >> 3) as u8;
            }
            let _ = Setups::parse(&bad);
        }
    }

    #[test]
    fn id_out_of_range_is_rejected() {
        // A minimal database: one part per kind, one skeleton, no rows.
        let mut d = b"AQVS\x01".to_vec();
        d.extend_from_slice(&[1, 1, 0]); // codebooks: one part of 1 bit
        d.extend_from_slice(&[1, 1, 0]); // floors
        d.extend_from_slice(&[1, 88]); // residues: one part of 88 bits
        d.extend_from_slice(&[0; 11]);
        d.extend_from_slice(&[1, 1, 0]); // mappings
        d.extend_from_slice(&[1, 1, 0]); // modes
        d.push(1); // skeletons
        for _ in 0..5 {
            d.extend_from_slice(&[1, 0]);
        }
        d.push(0); // rows
        assert!(Setups::parse(&d).is_ok());
        let at = d.len() - 2;
        d[at] = 1; // modes id 1
        assert!(Setups::parse(&d).is_err());
    }
}
