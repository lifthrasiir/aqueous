//! .NET (CLI) assemblies: the section holding the metadata of an image, rebuilt bit for bit
//! from streams. The metadata tables go column by column, a byte plane of each column at a
//! time, sorted and list columns as deltas, and the RVAs of method bodies as indices into the
//! bodies; the heaps go to streams of their own; the IL of the method bodies is split by
//! operand, its tokens through a move-to-front cache of each table, and their row numbers
//! that miss it as deltas. What is neither (the CLR header, resources, native code, padding)
//! is kept in a stream of its own, in order.
//!
//! Splitting the IL by operand alone archives no smaller under zstd (the tokens repeat with
//! their opcodes); the tokens' cache and deltas take 29% off them, and the tables' columns
//! 30% off the tables (see FORMAT.md).

use alloc::{vec, vec::Vec};

macro_rules! define_streams {
    ($($i:ident),* $(,)?) => (
        define_streams! { @0 $($i)* }
        /// Names of the streams, in order.
        pub const STREAM_NAMES: [&str; NUM_STREAMS] = [$(stringify!($i)),*];
    );

    (@$c:tt $i:ident $($t:tt)*) => (
        #[allow(dead_code)] const $i: usize = $c;
        define_streams! { @(1 + $c) $($t)* }
    );

    (@$c:tt) => (
        /// Number of the streams; all of them are always present, possibly empty.
        pub const NUM_STREAMS: usize = $c;
    );
}

define_streams! {
    REST,
    META,
    STRINGS, US, BLOB, GUID,
    TABLES, COLFLAGS, RVAX,
    BODYPOS, HDR, EH,
    OP, I1, I2, I4, I8, BR1, BR4,
    TOKT, TOKI, TOKR_STR, TOKR_METHOD, TOKR_FIELD, TOKR_TYPE, TOKR_OTHER,
}

/// The heaps of the metadata kept in streams of their own, by name.
const HEAPS: [(&[u8], usize); 4] = [(b"#Strings", STRINGS), (b"#US", US), (b"#Blob", BLOB), (b"#GUID", GUID)];

/// Entries of the move-to-front cache of each table's tokens.
const CACHE: usize = 255;

/// A column of a metadata table (ECMA-335 §II.22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    U2,
    U4,
    Str,
    Guid,
    Blob,
    /// A simple index into a table.
    Idx(u8),
    /// A coded index: its tag bits and tables (`NONE` for tags not in use).
    Coded(u8, &'static [u8]),
}

const NONE: u8 = 0xff;

const TYPE_DEF_OR_REF: Col = Col::Coded(2, &[0x02, 0x01, 0x1b]);
const HAS_CONSTANT: Col = Col::Coded(2, &[0x04, 0x08, 0x17]);
const HAS_CUSTOM_ATTRIBUTE: Col = Col::Coded(
    5,
    &[
        0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0a, 0x00, 0x0e, 0x17, 0x14, 0x11, 0x1a, 0x1b, 0x20, 0x23, 0x26, 0x27,
        0x28, 0x2a, 0x2c, 0x2b,
    ],
);
const HAS_FIELD_MARSHAL: Col = Col::Coded(1, &[0x04, 0x08]);
const HAS_DECL_SECURITY: Col = Col::Coded(2, &[0x02, 0x06, 0x20]);
const MEMBER_REF_PARENT: Col = Col::Coded(3, &[0x02, 0x01, 0x1a, 0x06, 0x1b]);
const HAS_SEMANTICS: Col = Col::Coded(1, &[0x14, 0x17]);
const METHOD_DEF_OR_REF: Col = Col::Coded(1, &[0x06, 0x0a]);
const MEMBER_FORWARDED: Col = Col::Coded(1, &[0x04, 0x06]);
const IMPLEMENTATION: Col = Col::Coded(2, &[0x26, 0x23, 0x27]);
const CUSTOM_ATTRIBUTE_TYPE: Col = Col::Coded(3, &[NONE, NONE, 0x06, 0x0a, NONE]);
const RESOLUTION_SCOPE: Col = Col::Coded(2, &[0x00, 0x1a, 0x23, 0x01]);
const TYPE_OR_METHOD_DEF: Col = Col::Coded(1, &[0x02, 0x06]);

/// The tables there are, 0x00 to 0x2C, by their columns.
const SCHEMA: [&[Col]; 0x2d] = {
    use Col::*;
    [
        &[U2, Str, Guid, Guid, Guid],                              // 00 Module
        &[RESOLUTION_SCOPE, Str, Str],                             // 01 TypeRef
        &[U4, Str, Str, TYPE_DEF_OR_REF, Idx(0x04), Idx(0x06)],    // 02 TypeDef
        &[Idx(0x04)],                                              // 03 FieldPtr
        &[U2, Str, Blob],                                          // 04 Field
        &[Idx(0x06)],                                              // 05 MethodPtr
        &[U4, U2, U2, Str, Blob, Idx(0x08)],                       // 06 MethodDef
        &[Idx(0x08)],                                              // 07 ParamPtr
        &[U2, U2, Str],                                            // 08 Param
        &[Idx(0x02), TYPE_DEF_OR_REF],                             // 09 InterfaceImpl
        &[MEMBER_REF_PARENT, Str, Blob],                           // 0A MemberRef
        &[U2, HAS_CONSTANT, Blob],                                 // 0B Constant
        &[HAS_CUSTOM_ATTRIBUTE, CUSTOM_ATTRIBUTE_TYPE, Blob],      // 0C CustomAttribute
        &[HAS_FIELD_MARSHAL, Blob],                                // 0D FieldMarshal
        &[U2, HAS_DECL_SECURITY, Blob],                            // 0E DeclSecurity
        &[U2, U4, Idx(0x02)],                                      // 0F ClassLayout
        &[U4, Idx(0x04)],                                          // 10 FieldLayout
        &[Blob],                                                   // 11 StandAloneSig
        &[Idx(0x02), Idx(0x14)],                                   // 12 EventMap
        &[Idx(0x14)],                                              // 13 EventPtr
        &[U2, Str, TYPE_DEF_OR_REF],                               // 14 Event
        &[Idx(0x02), Idx(0x17)],                                   // 15 PropertyMap
        &[Idx(0x17)],                                              // 16 PropertyPtr
        &[U2, Str, Blob],                                          // 17 Property
        &[U2, Idx(0x06), HAS_SEMANTICS],                           // 18 MethodSemantics
        &[Idx(0x02), METHOD_DEF_OR_REF, METHOD_DEF_OR_REF],        // 19 MethodImpl
        &[Str],                                                    // 1A ModuleRef
        &[Blob],                                                   // 1B TypeSpec
        &[U2, MEMBER_FORWARDED, Str, Idx(0x1a)],                   // 1C ImplMap
        &[U4, Idx(0x04)],                                          // 1D FieldRVA
        &[U4, U4],                                                 // 1E EncLog
        &[U4],                                                     // 1F EncMap
        &[U4, U2, U2, U2, U2, U4, Blob, Str, Str],                 // 20 Assembly
        &[U4],                                                     // 21 AssemblyProcessor
        &[U4, U4, U4],                                             // 22 AssemblyOS
        &[U2, U2, U2, U2, U4, Blob, Str, Str, Blob],               // 23 AssemblyRef
        &[U4, Idx(0x23)],                                          // 24 AssemblyRefProcessor
        &[U4, U4, U4, Idx(0x23)],                                  // 25 AssemblyRefOS
        &[U4, Str, Blob],                                          // 26 File
        &[U4, U4, Str, Str, IMPLEMENTATION],                       // 27 ExportedType
        &[U4, U4, Str, IMPLEMENTATION],                            // 28 ManifestResource
        &[Idx(0x02), Idx(0x02)],                                   // 29 NestedClass
        &[U2, U2, TYPE_OR_METHOD_DEF, Str],                        // 2A GenericParam
        &[METHOD_DEF_OR_REF, Blob],                                // 2B MethodSpec
        &[Idx(0x2a), TYPE_DEF_OR_REF],                             // 2C GenericParamConstraint
    ]
};

/// The table of method bodies' RVAs, and its column.
const METHOD_DEF: usize = 0x06;

/// The header of the tables stream (`#~` or `#-`), and the layout it gives them.
struct Tables {
    /// Bytes of the header: through the row counts, and the extra 4 bytes after them where
    /// `HeapSizes` has bit 0x40.
    header_len: usize,
    /// Row counts, by table.
    rows: [u32; 64],
    /// Column widths, by table (empty for tables not present).
    widths: Vec<Vec<usize>>,
    /// Bytes of the rows of every table, in order.
    rows_len: usize,
}

impl Tables {
    /// The layout of the tables whose header starts `h`; `None` where it is cut short, or has
    /// tables above 0x2C.
    fn parse(h: &[u8]) -> Option<Tables> {
        let heap_sizes = *h.get(6)?;
        let valid = u64::from_le_bytes(h.get(8..16)?.try_into().unwrap());
        if valid >> SCHEMA.len() != 0 {
            return None;
        }
        let mut rows = [0u32; 64];
        let mut p = 24;
        for (t, r) in rows.iter_mut().enumerate() {
            if valid >> t & 1 != 0 {
                *r = u32::from_le_bytes(h.get(p..p + 4)?.try_into().unwrap());
                p += 4;
            }
        }
        if heap_sizes & 0x40 != 0 {
            p += 4;
        }
        let heap = |bit: u8| if heap_sizes & bit != 0 { 4 } else { 2 };
        let width = |c: Col| match c {
            Col::U2 => 2,
            Col::U4 => 4,
            Col::Str => heap(0x01),
            Col::Guid => heap(0x02),
            Col::Blob => heap(0x04),
            Col::Idx(t) => {
                if rows[t as usize] < 1 << 16 {
                    2
                } else {
                    4
                }
            }
            Col::Coded(bits, ts) => {
                let max = ts.iter().filter(|&&t| t != NONE).map(|&t| rows[t as usize]).max().unwrap_or(0);
                if max < 1 << (16 - bits) {
                    2
                } else {
                    4
                }
            }
        };
        let mut widths = Vec::with_capacity(SCHEMA.len());
        let mut rows_len = 0usize;
        for (t, cols) in SCHEMA.iter().enumerate() {
            if valid >> t & 1 == 0 {
                widths.push(Vec::new());
                continue;
            }
            let w: Vec<usize> = cols.iter().map(|&c| width(c)).collect();
            rows_len = rows_len.checked_add(w.iter().sum::<usize>().checked_mul(rows[t] as usize)?)?;
            widths.push(w);
        }
        Some(Tables { header_len: p, rows, widths, rows_len })
    }
}

/// A stream of the metadata root: its offset from the root, size and name.
struct MetaStream<'a> {
    offset: u32,
    size: u32,
    name: &'a [u8],
}

/// The length of the metadata root starting `m`, through its stream headers, and those;
/// `None` where it is cut short.
fn parse_root(m: &[u8]) -> Option<(usize, Vec<MetaStream<'_>>)> {
    let version_len = u32::from_le_bytes(m.get(12..16)?.try_into().unwrap()) as usize;
    let mut p = 16usize.checked_add(version_len)?;
    let count = u16::from_le_bytes(m.get(p + 2..p + 4)?.try_into().unwrap());
    p += 4;
    let mut streams = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = u32::from_le_bytes(m.get(p..p + 4)?.try_into().unwrap());
        let size = u32::from_le_bytes(m.get(p + 4..p + 8)?.try_into().unwrap());
        p += 8;
        let n = m.get(p..)?.iter().position(|&b| b == 0)?;
        streams.push(MetaStream { offset, size, name: &m[p..p + n] });
        p += (n + 4) & !3;
    }
    m.get(..p)?;
    Some((p, streams))
}

/// The operand of an IL opcode (ECMA-335 §III), by what follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operand {
    None,
    /// To stream `I1`, `I2`, `I4` or `I8` (`float32` with the 8-byte ones), of that many bytes.
    Imm(usize, usize),
    Token,
    Br1,
    Br4,
    Switch,
}

fn operand(op: u8) -> Operand {
    use Operand::*;
    match op {
        0x0e..=0x13 | 0x1f => Imm(I1, 1),
        0x20 => Imm(I4, 4),
        0x21 | 0x23 => Imm(I8, 8),
        0x22 => Imm(I8, 4),
        0x27 | 0x28 | 0x29 | 0x6f..=0x75 | 0x79 | 0x7b..=0x81 | 0x8c | 0x8d | 0x8f | 0xa3..=0xa5 | 0xc2 | 0xc6
        | 0xd0 => Token,
        0x2b..=0x37 | 0xde => Br1,
        0x38..=0x44 | 0xdd => Br4,
        0x45 => Switch,
        _ => None,
    }
}

/// The operand of `FE op`.
fn operand_fe(op: u8) -> Operand {
    use Operand::*;
    match op {
        0x06 | 0x07 | 0x15 | 0x16 | 0x1c => Token,
        0x09..=0x0e => Imm(I2, 2),
        0x12 | 0x19 => Imm(I1, 1),
        _ => None,
    }
}

/// The stream of the row numbers of a table's tokens that miss its cache.
fn rid_stream(table: u8) -> usize {
    match table {
        0x70 => TOKR_STR,
        0x06 | 0x0a | 0x2b => TOKR_METHOD,
        0x04 => TOKR_FIELD,
        0x01 | 0x02 | 0x1b => TOKR_TYPE,
        _ => TOKR_OTHER,
    }
}

/// The tokens' move-to-front caches, a row number of each entry, and the last row number
/// of each table to miss its cache.
struct Tokens {
    cache: Vec<Vec<u32>>,
    last: [u32; 256],
}

impl Tokens {
    fn new() -> Tokens {
        Tokens { cache: vec![Vec::new(); 256], last: [0; 256] }
    }

    fn insert(&mut self, table: u8, rid: u32) {
        let c = &mut self.cache[table as usize];
        c.insert(0, rid);
        c.truncate(CACHE);
    }
}


#[cfg(feature = "encode")]
mod encode {
    use super::*;
    use core::array;
    use core::ops::Range;

    fn zigzag(v: i32) -> u32 {
        ((v << 1) ^ (v >> 31)) as u32
    }

    /// The size of the method body whose header starts `b`: the header's, the code's and, with
    /// its data sections, the end of the last of them; `None` where it is not one whole within `b`
    /// (a fat header of other than 12 bytes, a data section shorter than its header).
    fn body_size(b: &[u8]) -> Option<(usize, usize, usize)> {
        let h = *b.first()?;
        match h & 3 {
            2 => {
                let code = (h >> 2) as usize;
                b.get(..1 + code)?;
                Some((1, code, 1 + code))
            }
            3 => {
                let flags = u16::from_le_bytes(b.get(..2)?.try_into().unwrap());
                if flags >> 12 != 3 {
                    return None;
                }
                let code = u32::from_le_bytes(b.get(4..8)?.try_into().unwrap()) as usize;
                let mut end = 12usize.checked_add(code)?;
                b.get(..end)?;
                if flags & 8 != 0 {
                    loop {
                        end = end.checked_add(3)? & !3;
                        let kind = *b.get(end)?;
                        let n = if kind & 0x40 != 0 {
                            u32::from_le_bytes([*b.get(end + 1)?, *b.get(end + 2)?, *b.get(end + 3)?, 0]) as usize
                        } else {
                            *b.get(end + 1)? as usize
                        };
                        if n < 4 {
                            return None;
                        }
                        end = end.checked_add(n)?;
                        b.get(..end)?;
                        if kind & 0x80 == 0 {
                            break;
                        }
                    }
                }
                Some((12, code, end))
            }
            _ => None,
        }
    }

    /// The streams of a section, as [`encode`] gives them.
    #[derive(Debug)]
    pub struct EncodedStreams {
        streams: [Vec<u8>; NUM_STREAMS],
        rest: Vec<Range<usize>>,
    }

    impl EncodedStreams {
        pub fn streams(&self) -> &[Vec<u8>; NUM_STREAMS] {
            &self.streams
        }

        /// The ranges of the section in [`REST`] (stream 0), in order: what is not otherwise
        /// taken.
        pub fn rest(&self) -> &[Range<usize>] {
            &self.rest
        }
    }

    fn varint(mut v: u32, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// Whether `code` is whole instructions, the last ending with it.
    fn parses(code: &[u8]) -> bool {
        let mut i = 0;
        while i < code.len() {
            let op = code[i];
            i += 1;
            let k = if op == 0xfe {
                let Some(&op) = code.get(i) else { return false };
                i += 1;
                operand_fe(op)
            } else {
                operand(op)
            };
            let n = match k {
                Operand::None => 0,
                Operand::Imm(_, n) => n,
                Operand::Token | Operand::Br4 => 4,
                Operand::Br1 => 1,
                Operand::Switch => {
                    let Some(c) = code.get(i..i + 4) else { return false };
                    let c = u32::from_le_bytes(c.try_into().unwrap()) as usize;
                    match c.checked_mul(4).and_then(|n| n.checked_add(4)) {
                        Some(n) => n,
                        None => return false,
                    }
                }
            };
            i = match i.checked_add(n) {
                Some(i) if i <= code.len() => i,
                _ => return false,
            };
        }
        true
    }

    fn split(code: &[u8], st: &mut [Vec<u8>; NUM_STREAMS], tokens: &mut Tokens) {
        let mut i = 0;
        while i < code.len() {
            let op = code[i];
            st[OP].push(op);
            i += 1;
            let k = if op == 0xfe {
                st[OP].push(code[i]);
                i += 1;
                operand_fe(code[i - 1])
            } else {
                operand(op)
            };
            match k {
                Operand::None => {}
                Operand::Imm(s, n) => {
                    st[s].extend_from_slice(&code[i..i + n]);
                    i += n;
                }
                Operand::Br1 => {
                    st[BR1].push(code[i]);
                    i += 1;
                }
                Operand::Br4 => {
                    st[BR4].extend_from_slice(&code[i..i + 4]);
                    i += 4;
                }
                Operand::Switch => {
                    let n = 4 + 4 * u32::from_le_bytes(code[i..i + 4].try_into().unwrap()) as usize;
                    st[BR4].extend_from_slice(&code[i..i + n]);
                    i += n;
                }
                Operand::Token => {
                    let t = u32::from_le_bytes(code[i..i + 4].try_into().unwrap());
                    i += 4;
                    let (table, rid) = ((t >> 24) as u8, t & 0xff_ffff);
                    st[TOKT].push(table);
                    let c = &mut tokens.cache[table as usize];
                    match c.iter().position(|&r| r == rid) {
                        Some(k) => {
                            st[TOKI].push(k as u8 + 1);
                            c.remove(k);
                            c.insert(0, rid);
                        }
                        None => {
                            st[TOKI].push(0);
                            let last = &mut tokens.last[table as usize];
                            varint(zigzag(rid.wrapping_sub(*last) as i32), &mut st[rid_stream(table)]);
                            *last = rid;
                            tokens.insert(table, rid);
                        }
                    }
                }
            }
        }
    }

    /// The streams of `section`, whose first byte is at RVA `origin`, with the metadata root
    /// at offset `meta`; `None` where the metadata is not as [`decode`] reads it, or its
    /// parts overlap or are not within the section.
    pub fn encode(section: &[u8], origin: u32, meta: usize) -> Option<EncodedStreams> {
        let mut st: [Vec<u8>; NUM_STREAMS] = array::from_fn(|_| Vec::new());
        let m = section.get(meta..)?;
        if m.get(..4)? != b"BSJB" {
            return None;
        }
        let (root_len, streams) = parse_root(m)?;
        st[META].extend_from_slice(&m[..root_len]);
        let mut taken = Vec::new();
        taken.push(meta..meta + root_len);

        // the heaps, and the tables
        let mut seen = [false; HEAPS.len()];
        let mut tables = None;
        for s in &streams {
            let start = meta.checked_add(s.offset as usize)?;
            let end = start.checked_add(s.size as usize)?;
            if let Some(h) = HEAPS.iter().position(|h| h.0 == s.name) {
                if !seen[h] {
                    seen[h] = true;
                    st[HEAPS[h].1].extend_from_slice(section.get(start..end)?);
                    taken.push(start..end);
                }
            } else if tables.is_none() && (s.name == b"#~" || s.name == b"#-") {
                let t = Tables::parse(section.get(start..end)?)?;
                let rows_end = start.checked_add(t.header_len)?.checked_add(t.rows_len)?;
                if rows_end > end {
                    return None;
                }
                st[META].extend_from_slice(&section[start..start + t.header_len]);
                taken.push(start..rows_end);
                tables = Some((start + t.header_len, t));
            }
        }
        taken.sort_by_key(|r| r.start);
        if taken.windows(2).any(|w| w[0].end > w[1].start) {
            return None;
        }

        // the method bodies, of every RVA of the MethodDef table in the section
        let mut bodies = Vec::new();
        if let Some((at, t)) = &tables {
            let w = &t.widths[METHOD_DEF];
            if !w.is_empty() {
                let row: usize = w.iter().sum();
                let start = at + table_offset(t, METHOD_DEF);
                let mut rvas: Vec<u32> = (0..t.rows[METHOD_DEF] as usize)
                    .map(|r| u32::from_le_bytes(section[start + r * row..][..4].try_into().unwrap()))
                    .filter(|&rva| rva != 0)
                    .collect();
                rvas.sort_unstable();
                rvas.dedup();
                bodies = rvas.into_iter().filter_map(|rva| Some(rva.checked_sub(origin)? as usize)).collect();
            }
        }
        // (each after the last taken, clear of the metadata)
        let mut tokens = Tokens::new();
        let mut taken_bodies = Vec::new();
        let mut body_ranges = Vec::new();
        let (mut at, mut next) = (0, 0);
        for b in bodies {
            if b < at || b >= section.len() {
                continue;
            }
            let Some((hl, code, n)) = body_size(&section[b..]) else { continue };
            while taken.get(next).is_some_and(|r| r.end <= b) {
                next += 1;
            }
            if taken.get(next).is_some_and(|r| r.start < b + n) || !parses(&section[b + hl..][..code]) {
                continue;
            }
            varint((b - at) as u32, &mut st[BODYPOS]);
            st[HDR].extend_from_slice(&section[b..b + hl]);
            split(&section[b + hl..][..code], &mut st, &mut tokens);
            st[EH].extend_from_slice(&section[b + hl + code..b + n]);
            taken_bodies.push(b);
            body_ranges.push(b..b + n);
            at = b + n;
        }
        taken.extend(body_ranges);

        // the tables' columns
        if let Some((at, t)) = &tables {
            let mut p = *at;
            let mut last_body: i64 = -1;
            for (table, w) in t.widths.iter().enumerate() {
                let row: usize = w.iter().sum();
                let nr = t.rows[table] as usize;
                let mut off = 0;
                for (c, &cw) in w.iter().enumerate() {
                    let mut vals: Vec<u32> = (0..nr)
                        .map(|r| {
                            let v = &section[p + r * row + off..][..cw];
                            if cw == 2 {
                                u16::from_le_bytes(v.try_into().unwrap()) as u32
                            } else {
                                u32::from_le_bytes(v.try_into().unwrap())
                            }
                        })
                        .collect();
                    let width = if table == METHOD_DEF && c == 0 {
                        for v in &mut vals {
                            let body = v.checked_sub(origin).and_then(|o| taken_bodies.binary_search(&(o as usize)).ok());
                            *v = match body {
                                _ if *v == 0 => 0,
                                Some(k) => {
                                    let d = k as i64 - (last_body + 1);
                                    last_body = k as i64;
                                    2 + zigzag(d as i32)
                                }
                                None => {
                                    st[RVAX].extend_from_slice(&v.to_le_bytes());
                                    1
                                }
                            };
                        }
                        4
                    } else {
                        let sorted = nr > 1 && vals.windows(2).all(|v| v[0] <= v[1]);
                        st[COLFLAGS].push(sorted as u8);
                        if sorted {
                            for r in (1..nr).rev() {
                                vals[r] -= vals[r - 1];
                            }
                        }
                        cw
                    };
                    for b in 0..width {
                        st[TABLES].extend(vals.iter().map(|v| (v >> (8 * b)) as u8));
                    }
                    off += cw;
                }
                p += row * nr;
            }
        }

        taken.sort_by_key(|r| r.start);
        let mut rest = Vec::new();
        let mut at = 0;
        for r in taken {
            if r.start > at {
                rest.push(at..r.start);
            }
            at = r.end;
        }
        if at < section.len() {
            rest.push(at..section.len());
        }
        for r in &rest {
            st[REST].extend_from_slice(&section[r.clone()]);
        }
        Some(EncodedStreams { streams: st, rest })
    }

    /// The offset of table `table`'s rows from the first table's.
    fn table_offset(t: &Tables, table: usize) -> usize {
        (0..table).map(|u| t.widths[u].iter().sum::<usize>() * t.rows[u] as usize).sum()
    }
}

#[cfg(feature = "encode")]
pub use encode::*;

#[cfg(feature = "decode")]
mod decode {
    use super::*;

    fn unzigzag(z: u32) -> i32 {
        ((z >> 1) as i32) ^ -((z & 1) as i32)
    }

    fn take<'a>(s: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (a, b) = (s.get(..n)?, s.get(n..)?);
        *s = b;
        Some(a)
    }

    fn byte(s: &mut &[u8]) -> Option<u8> {
        Some(take(s, 1)?[0])
    }

    fn varint(s: &mut &[u8]) -> Option<u32> {
        let mut v = 0u64;
        for k in 0..5 {
            let b = byte(s)?;
            v |= ((b & 0x7f) as u64) << (7 * k);
            if b & 0x80 == 0 {
                return u32::try_from(v).ok();
            }
        }
        None
    }

    /// Copies `n` bytes from `s` to `out` at `at`.
    fn copy(s: &mut &[u8], out: &mut [u8], at: usize, n: usize) -> Option<()> {
        out.get_mut(at..at.checked_add(n)?)?.copy_from_slice(take(s, n)?);
        Some(())
    }

    /// Rebuilds the code of `n` bytes at `at` of `out`.
    fn join(s: &mut [&[u8]; NUM_STREAMS], out: &mut [u8], at: usize, n: usize, tokens: &mut Tokens) -> Option<()> {
        let end = at.checked_add(n)?;
        let code = out.get_mut(at..end)?;
        let mut i = 0;
        let put = |code: &mut [u8], i: &mut usize, b: &[u8]| -> Option<()> {
            code.get_mut(*i..*i + b.len())?.copy_from_slice(b);
            *i += b.len();
            Some(())
        };
        while i < n {
            let op = byte(&mut s[OP])?;
            put(code, &mut i, &[op])?;
            let k = if op == 0xfe {
                let op = byte(&mut s[OP])?;
                put(code, &mut i, &[op])?;
                operand_fe(op)
            } else {
                operand(op)
            };
            match k {
                Operand::None => {}
                Operand::Imm(st, n) => put(code, &mut i, take(&mut s[st], n)?)?,
                Operand::Br1 => put(code, &mut i, take(&mut s[BR1], 1)?)?,
                Operand::Br4 => put(code, &mut i, take(&mut s[BR4], 4)?)?,
                Operand::Switch => {
                    let c = take(&mut s[BR4], 4)?;
                    put(code, &mut i, c)?;
                    let c = (u32::from_le_bytes(c.try_into().unwrap()) as usize).checked_mul(4)?;
                    put(code, &mut i, take(&mut s[BR4], c)?)?;
                }
                Operand::Token => {
                    let table = byte(&mut s[TOKT])?;
                    let k = byte(&mut s[TOKI])? as usize;
                    let rid = if k == 0 {
                        let d = unzigzag(varint(&mut s[rid_stream(table)])?);
                        let last = &mut tokens.last[table as usize];
                        let rid = last.wrapping_add(d as u32);
                        if rid >> 24 != 0 {
                            return None;
                        }
                        *last = rid;
                        tokens.insert(table, rid);
                        rid
                    } else {
                        let c = &mut tokens.cache[table as usize];
                        if k > c.len() {
                            return None;
                        }
                        let rid = c.remove(k - 1);
                        c.insert(0, rid);
                        rid
                    };
                    put(code, &mut i, &((table as u32) << 24 | rid).to_le_bytes())?;
                }
            }
        }
        Some(())
    }

    /// The section of `len` bytes at RVA `origin`, its metadata root at offset `meta`, from
    /// its `streams`; `None` where they do not make one, or are not all read.
    pub fn decode(streams: [&[u8]; NUM_STREAMS], origin: u32, meta: usize, len: usize) -> Option<Vec<u8>> {
        let mut s = streams;
        let mut out = vec![0u8; len];
        let mut taken = Vec::new();

        // the metadata root, the heaps and the tables' header
        let (root_len, streams) = parse_root(s[META])?;
        let root = take(&mut s[META], root_len)?;
        out.get_mut(meta..meta.checked_add(root_len)?)?.copy_from_slice(root);
        taken.push((meta, meta + root_len));
        let mut seen = [false; HEAPS.len()];
        let mut tables = None;
        for m in &streams {
            let start = meta.checked_add(m.offset as usize)?;
            let end = start.checked_add(m.size as usize)?;
            if let Some(h) = HEAPS.iter().position(|h| h.0 == m.name) {
                if !seen[h] {
                    seen[h] = true;
                    copy(&mut s[HEAPS[h].1], &mut out, start, m.size as usize)?;
                    taken.push((start, end));
                }
            } else if tables.is_none() && (m.name == b"#~" || m.name == b"#-") {
                let t = Tables::parse(s[META])?;
                let rows_end = start.checked_add(t.header_len)?.checked_add(t.rows_len)?;
                if rows_end > end {
                    return None;
                }
                copy(&mut s[META], &mut out, start, t.header_len)?;
                taken.push((start, rows_end));
                tables = Some((start + t.header_len, t));
            }
        }

        // the method bodies
        let mut tokens = Tokens::new();
        let mut bodies = Vec::new();
        let mut at = 0usize;
        while !s[BODYPOS].is_empty() {
            let b = at.checked_add(varint(&mut s[BODYPOS])? as usize)?;
            let h = *s[HDR].first()?;
            let (hl, code) = match h & 3 {
                2 => {
                    copy(&mut s[HDR], &mut out, b, 1)?;
                    (1, (h >> 2) as usize)
                }
                3 => {
                    copy(&mut s[HDR], &mut out, b, 12)?;
                    if u16::from_le_bytes(out[b..b + 2].try_into().unwrap()) >> 12 != 3 {
                        return None;
                    }
                    (12, u32::from_le_bytes(out[b + 4..b + 8].try_into().unwrap()) as usize)
                }
                _ => return None,
            };
            join(&mut s, &mut out, b + hl, code, &mut tokens)?;
            // the data sections, each aligned to 4 bytes
            let mut end = b + hl + code;
            if hl == 12 && out[b] & 8 != 0 {
                loop {
                    let pad = (end.checked_add(3)? & !3) - end;
                    copy(&mut s[EH], &mut out, end, pad)?;
                    end += pad;
                    let kind = *s[EH].first()?;
                    let n = if kind & 0x40 != 0 {
                        let h = s[EH].get(..4)?;
                        u32::from_le_bytes([h[1], h[2], h[3], 0]) as usize
                    } else {
                        *s[EH].get(1)? as usize
                    };
                    if n < 4 {
                        return None;
                    }
                    copy(&mut s[EH], &mut out, end, n)?;
                    end += n;
                    if kind & 0x80 == 0 {
                        break;
                    }
                }
            }
            bodies.push(b);
            taken.push((b, end));
            at = end;
        }

        // the tables' columns
        if let Some((mut p, t)) = tables {
            let mut last_body: i64 = -1;
            let mut vals = Vec::new();
            for (table, w) in t.widths.iter().enumerate() {
                let row: usize = w.iter().sum();
                let nr = t.rows[table] as usize;
                let mut off = 0;
                for (c, &cw) in w.iter().enumerate() {
                    let rva = table == METHOD_DEF && c == 0;
                    let width = if rva { 4 } else { cw };
                    vals.clear();
                    vals.resize(nr, 0u32);
                    for b in 0..width {
                        for (v, &x) in vals.iter_mut().zip(take(&mut s[TABLES], nr)?) {
                            *v |= (x as u32) << (8 * b);
                        }
                    }
                    if rva {
                        for v in &mut vals {
                            *v = match *v {
                                0 => 0,
                                1 => u32::from_le_bytes(take(&mut s[RVAX], 4)?.try_into().unwrap()),
                                z => {
                                    let k = last_body.checked_add(1 + unzigzag(z - 2) as i64)?;
                                    last_body = k;
                                    let b = *bodies.get(usize::try_from(k).ok()?)?;
                                    origin.wrapping_add(u32::try_from(b).ok()?)
                                }
                            };
                        }
                    } else {
                        match byte(&mut s[COLFLAGS])? {
                            0 => {}
                            1 => {
                                let mask = if cw == 2 { 0xffff } else { u32::MAX };
                                for r in 1..nr {
                                    vals[r] = vals[r].wrapping_add(vals[r - 1]) & mask;
                                }
                            }
                            _ => return None,
                        }
                    }
                    for (r, v) in vals.iter().enumerate() {
                        let at = p + r * row + off;
                        out.get_mut(at..at + cw)?.copy_from_slice(&v.to_le_bytes()[..cw]);
                    }
                    off += cw;
                }
                p += row * nr;
            }
        }

        // the rest, between what is taken
        taken.sort_unstable();
        let mut at = 0;
        for (start, end) in taken {
            if start < at || end > len {
                return None;
            }
            copy(&mut s[REST], &mut out, at, start - at)?;
            at = end;
        }
        copy(&mut s[REST], &mut out, at, len - at)?;
        s.iter().all(|s| s.is_empty()).then_some(out)
    }
}

#[cfg(feature = "decode")]
pub use decode::decode;

#[cfg(test)]
mod tests {
    use super::*;
    use core::array;

    const ORIGIN: u32 = 0x2000;

    fn roundtrip(section: &[u8], meta: usize) -> EncodedStreams {
        let e = encode(section, ORIGIN, meta).expect("encoded");
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        assert_eq!(decode(s, ORIGIN, meta, section.len()).as_deref(), Some(section));
        e
    }

    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// A section with three method bodies (tiny, fat with an exception table, and one cut
    /// short), the metadata root at 0x200, and MethodDef rows for them, one abstract and one
    /// pointing elsewhere.
    fn section() -> (Vec<u8>, usize, [usize; 3]) {
        let mut d = vec![0u8; 0x400];
        d[..8].copy_from_slice(b"CLRHDR!!");
        // tiny: ldstr, call, call (the same token: a cache hit), ret
        let a = 0x10;
        let code_a = [0x72, 1, 0, 0, 0x70, 0x28, 2, 0, 0, 0x0a, 0x28, 2, 0, 0, 0x0a, 0x2a];
        d[a] = (code_a.len() << 2 | 2) as u8;
        d[a + 1..a + 1 + code_a.len()].copy_from_slice(&code_a);
        // fat, with more sections: ldc.i4, switch, br.s, ldloc, initobj, leave.s, ret
        let b = 0x24;
        let mut code_b = vec![0x20, 0x78, 0x56, 0x34, 0x12, 0x45];
        code_b.extend(le32(2).into_iter().chain(le32(5)).chain(le32(0xffff_fffe)));
        code_b.extend([0x2b, 0x00, 0xfe, 0x0c, 0x01, 0x00, 0xfe, 0x15, 3, 0, 0, 0x01, 0xde, 0x00, 0x2a]);
        d[b..b + 2].copy_from_slice(&0x301bu16.to_le_bytes());
        d[b + 2..b + 4].copy_from_slice(&8u16.to_le_bytes());
        d[b + 4..b + 8].copy_from_slice(&le32(code_b.len() as u32));
        d[b + 8..b + 12].copy_from_slice(&le32(0x1100_0001));
        d[b + 12..b + 12 + code_b.len()].copy_from_slice(&code_b);
        let eh = (b + 12 + code_b.len() + 3) & !3;
        d[eh - 1] = 0xcc; // padding, not zero
        d[eh..eh + 4].copy_from_slice(&[0x01, 16, 0, 0]);
        d[eh + 4..eh + 16].copy_from_slice(&[0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        // cut short: ldc.i4 with two bytes of its four
        let c = 0x80;
        d[c..c + 4].copy_from_slice(&[3 << 2 | 2, 0x20, 1, 2]);

        // the metadata root and its streams
        let meta = 0x200;
        let mut m = Vec::new();
        m.extend(b"BSJB");
        m.extend([1, 0, 1, 0, 0, 0, 0, 0]);
        m.extend(le32(12));
        m.extend(b"v4.0.30319\0\0");
        m.extend([0, 0, 5, 0]);
        let names: [&[u8]; 5] = [b"#~", b"#Strings", b"#US", b"#GUID", b"#Blob"];
        let header_len = m.len() + names.iter().map(|n| 8 + ((n.len() + 4) & !3)).sum::<usize>();

        let mut tables = vec![0, 0, 0, 0, 2, 0, 0, 1];
        tables.extend(((1u64 << 0) | (1 << 2) | (1 << 6)).to_le_bytes());
        tables.extend(0u64.to_le_bytes());
        tables.extend(le32(1).into_iter().chain(le32(2)).chain(le32(5)));
        // Module: generation, name, mvid, encid, encbaseid
        tables.extend([0, 0, 1, 0, 1, 0, 0, 0, 0, 0]);
        // TypeDef: flags, name, namespace, extends, field list, method list
        for (name, methods) in [(1u16, 1u16), (5, 3)] {
            tables.extend(le32(0x100001));
            for v in [name, 0, 0, 1, methods] {
                tables.extend(v.to_le_bytes());
            }
        }
        // MethodDef: RVA, impl flags, flags, name, signature, param list
        for rva in [ORIGIN + a as u32, ORIGIN + b as u32, 0, ORIGIN + c as u32, 0x9999] {
            tables.extend(le32(rva));
            for v in [0u16, 0x86, 5, 1, 1] {
                tables.extend(v.to_le_bytes());
            }
        }
        while tables.len() % 4 != 0 {
            tables.push(0);
        }
        let heaps: [&[u8]; 5] = [&tables, b"\0Foo\0Bar\0\0\0\0", b"\0\x05h\0i\0\0\0", &[0x5a; 16], b"\0\x03abc\0\0\0"];
        let mut at = header_len;
        for (name, h) in names.iter().zip(heaps) {
            m.extend(le32(at as u32));
            m.extend(le32(h.len() as u32));
            m.extend(*name);
            m.extend(vec![0; ((name.len() + 4) & !3) - name.len()]);
            at += h.len();
        }
        assert_eq!(m.len(), header_len);
        for h in heaps {
            m.extend(h);
        }
        d[meta..meta + m.len()].copy_from_slice(&m);
        (d, meta, [a, b, c])
    }

    #[test]
    fn known_metadata() {
        let (d, meta, [a, b, c]) = section();
        let e = roundtrip(&d, meta);
        let s = e.streams();
        // the tiny and the fat body taken, the one cut short left
        assert_eq!(s[BODYPOS], [a as u8, (b - (a + 17)) as u8]);
        assert_eq!(s[HDR][0], 16 << 2 | 2);
        assert!(e.rest().iter().any(|r| r.contains(&c)));
        assert!(e.rest().iter().all(|r| !r.contains(&a) && !r.contains(&b)));
        // tokens: a string, a member twice (the second from the cache), a type
        assert_eq!(s[TOKT], [0x70, 0x0a, 0x0a, 0x01]);
        assert_eq!(s[TOKI], [0, 0, 1, 0]);
        assert_eq!((&s[TOKR_STR][..], &s[TOKR_METHOD][..], &s[TOKR_TYPE][..]), (&[2][..], &[4][..], &[6][..]));
        assert_eq!(s[BR4].len(), 12);
        assert_eq!(s[I2], [1, 0]);
        // the padding before the exception table, as it is, then the table
        assert_eq!(s[EH].len(), 3 + 16);
        assert_eq!(s[EH][..7], [0, 0, 0xcc, 0x01, 16, 0, 0]);
        // the RVAs (after Module's 10 bytes and TypeDef's 28): bodies 0 and 1, none, then two
        // kept as they are
        assert_eq!(s[TABLES][38..43], [2, 2, 0, 1, 1]);
        assert!(s[TABLES][43..58].iter().all(|&b| b == 0));
        assert_eq!(s[RVAX], [le32(ORIGIN + c as u32), le32(0x9999)].concat());
        // sorted columns as deltas, but those of one row (Module's) and MethodDef's RVAs
        assert_eq!(s[COLFLAGS], [[0; 5], [1; 5]].concat().into_iter().chain([1; 6]).collect::<Vec<_>>());
    }

    #[test]
    fn corrupt_streams_fail() {
        let (d, meta, _) = section();
        let e = encode(&d, ORIGIN, meta).unwrap();
        let s: [&[u8]; NUM_STREAMS] = array::from_fn(|i| &e.streams()[i][..]);
        assert_eq!(decode(s, ORIGIN, meta, d.len() - 1), None);
        assert_eq!(decode(s, ORIGIN, meta, d.len() + 1), None);
        // every stream a byte short, a byte too long, or with a byte changed: no panic, and
        // whatever comes out is not taken for the section unless it is it
        for i in 0..NUM_STREAMS {
            let v = &e.streams()[i];
            let mut variants = vec![[&v[..], &[0]].concat()];
            if !v.is_empty() {
                variants.push(v[..v.len() - 1].to_vec());
                for k in (0..v.len()).step_by(v.len().div_ceil(16)) {
                    let mut w = v.clone();
                    w[k] ^= 0x81;
                    variants.push(w);
                }
            }
            for w in &variants {
                let mut t = s;
                t[i] = w;
                if let Some(out) = decode(t, ORIGIN, meta, d.len()) {
                    assert_ne!(out, d, "stream {}", STREAM_NAMES[i]);
                }
            }
        }
    }

    #[test]
    fn not_metadata() {
        let (mut d, meta, _) = section();
        assert!(encode(&d, ORIGIN, meta + 4).is_none());
        // tables above 0x2C
        let (_, streams) = parse_root(&d[meta..]).unwrap();
        let t = meta + streams[0].offset as usize;
        d[t + 13] = 0x40;
        assert!(encode(&d, ORIGIN, meta).is_none());
    }
}
