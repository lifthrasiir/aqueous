//! `.debug_info` of units made here, of most forms, rebuilt by `dwarf-info`.

use aqr_dwarf::info::{INPUTS, NUM_STREAMS, decode, encode};

fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

// attributes and forms
const SIBLING: u64 = 0x01;
const NAME: u64 = 0x03;
const BYTE_SIZE: u64 = 0x0b;
const LOW_PC: u64 = 0x11;
const HIGH_PC: u64 = 0x12;
const CONST_VALUE: u64 = 0x1c;
const DECL_FILE: u64 = 0x3a;
const DECL_LINE: u64 = 0x3b;
const TYPE: u64 = 0x49;
const LOCATION: u64 = 0x02;
const LINKAGE_NAME: u64 = 0x6e;
const SPECIFICATION: u64 = 0x47;
const RANGES: u64 = 0x55;
const EXTERNAL: u64 = 0x3f;

const ADDR: u64 = 0x01;
const DATA1: u64 = 0x0b;
const DATA2: u64 = 0x05;
const DATA4: u64 = 0x06;
const STRING: u64 = 0x08;
const BLOCK1: u64 = 0x0a;
const SDATA: u64 = 0x0d;
const STRP: u64 = 0x0e;
const UDATA: u64 = 0x0f;
const REF_ADDR: u64 = 0x10;
const REF4: u64 = 0x13;
const INDIRECT: u64 = 0x16;
const SEC_OFFSET: u64 = 0x17;
const EXPRLOC: u64 = 0x18;
const FLAG_PRESENT: u64 = 0x19;
const STRX1: u64 = 0x25;
const IMPLICIT_CONST: u64 = 0x21;

/// `.debug_str` of `n` names, and the offset of each.
fn strings(n: usize) -> (Vec<u8>, Vec<u32>) {
    let mut s = Vec::new();
    let mut at = Vec::new();
    for i in 0..n {
        at.push(s.len() as u32);
        s.extend_from_slice(format!("name_{i}").as_bytes());
        s.push(0);
    }
    (s, at)
}

/// An abbreviation table: (code, tag, children, attributes (attribute, form, implicit constant)).
/// An abbreviation: its code, tag, whether it has children, and its attributes and forms.
type Abbrev<'a> = (u64, u64, bool, &'a [(u64, u64)]);

fn abbrev_table(t: &[Abbrev]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(code, tag, children, attrs) in t {
        uleb(&mut out, code);
        uleb(&mut out, tag);
        out.push(children as u8);
        for &(a, f) in attrs {
            uleb(&mut out, a);
            uleb(&mut out, f);
            if f == IMPLICIT_CONST {
                sleb(&mut out, -3);
            }
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.push(0);
    out
}

const CU: u64 = 1;
const SUB: u64 = 2;
const VAR: u64 = 3;
const TY: u64 = 4;
const MISC: u64 = 5;

fn table() -> Vec<u8> {
    abbrev_table(&[
        (CU, 0x11, true, &[(NAME, STRP), (LOW_PC, ADDR), (HIGH_PC, ADDR), (0x10, SEC_OFFSET)]),
        (
            SUB,
            0x2e,
            true,
            &[
                (SIBLING, REF4),
                (NAME, STRP),
                (LINKAGE_NAME, STRP),
                (DECL_FILE, DATA1),
                (DECL_LINE, DATA2),
                (LOW_PC, ADDR),
                (HIGH_PC, DATA4),
                (EXTERNAL, FLAG_PRESENT),
            ],
        ),
        (
            VAR,
            0x34,
            false,
            &[(NAME, STRING), (TYPE, REF4), (LOCATION, EXPRLOC), (DECL_LINE, UDATA), (CONST_VALUE, SDATA)],
        ),
        (TY, 0x24, false, &[(NAME, STRP), (BYTE_SIZE, DATA1), (0x3e, IMPLICIT_CONST)]),
        (
            MISC,
            0x0d,
            false,
            &[(TYPE, REF_ADDR), (SPECIFICATION, INDIRECT), (CONST_VALUE, BLOCK1), (RANGES, SEC_OFFSET)],
        ),
    ])
}

/// A unit of version 4 at `start` of the section: a compile unit holding `n` subprograms, each of
/// a few variables, types at the start; strings of `str_at`, the first at `first`.
fn unit4(start: usize, n: usize, str_at: &[u32], first: usize, prev_type: Option<u32>) -> Vec<u8> {
    let mut u = vec![0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 8];
    let mut k = first;
    let mut name = || {
        k += 1;
        str_at[(k - 1) % str_at.len()]
    };
    uleb(&mut u, CU);
    u.extend_from_slice(&name().to_le_bytes());
    u.extend_from_slice(&0x1000u64.to_le_bytes());
    u.extend_from_slice(&0x9000u64.to_le_bytes());
    u.extend_from_slice(&(start as u32 * 3).to_le_bytes());
    // the types
    let types: Vec<u32> = (0..3)
        .map(|i| {
            let at = u.len() as u32;
            uleb(&mut u, TY);
            u.extend_from_slice(&name().to_le_bytes());
            u.push(1 << i);
            at
        })
        .collect();
    for s in 0..n {
        let sub = u.len();
        uleb(&mut u, SUB);
        let sib = u.len();
        u.extend_from_slice(&[0; 4]);
        u.extend_from_slice(&name().to_le_bytes());
        u.extend_from_slice(&name().to_le_bytes());
        u.push(1 + (s % 3) as u8);
        u.extend_from_slice(&(10 + 7 * s as u16).to_le_bytes());
        u.extend_from_slice(&(0x1000 + 0x40 * s as u64).to_le_bytes());
        u.extend_from_slice(&0x3cu32.to_le_bytes());
        for v in 0..s % 4 {
            uleb(&mut u, VAR);
            u.extend_from_slice(format!("v{v}\0").as_bytes());
            u.extend_from_slice(&types[v % 3].to_le_bytes());
            u.extend_from_slice(&[2, 0x91, 0x70 - v as u8]);
            uleb(&mut u, 11 + 7 * s as u64 + v as u64);
            sleb(&mut u, -(v as i64) * 1000);
        }
        if s % 5 == 0 {
            uleb(&mut u, MISC);
            let t = prev_type.unwrap_or(start as u32 + types[0]);
            u.extend_from_slice(&t.to_le_bytes());
            uleb(&mut u, DATA4);
            u.extend_from_slice(&((sub - start) as u32 + 7).to_le_bytes());
            u.extend_from_slice(&[3, 1, 2, 3]);
            u.extend_from_slice(&(s as u32 * 16).to_le_bytes());
        }
        u.push(0);
        // the sibling: the end of the subtree, but for every seventh (as if written otherwise)
        let next = if s % 7 == 6 { types[1] } else { u.len() as u32 };
        u[sib..sib + 4].copy_from_slice(&next.to_le_bytes());
    }
    u.push(0);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    u
}

fn round_trip(d: &[u8], abbrev: &[u8], strs: &[u8]) -> Vec<Vec<u8>> {
    let st = encode(d, abbrev, strs).expect("taken");
    let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    assert_eq!(decode(&inputs, d.len() as u64).unwrap(), d);
    // through the module: the streams packed
    let mut packed = vec![abbrev.to_vec(), strs.to_vec()];
    for (_, which) in INPUTS {
        packed.push(aqr_dwarf::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>()));
    }
    let p: Vec<&[u8]> = packed.iter().map(|v| &v[..]).collect();
    assert_eq!(aqr_dwarf::module::info(&[], &p, d.len() as u64).unwrap(), d);
    st
}

fn section(units: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (strs, at) = strings(300);
    let mut d = Vec::new();
    let mut prev_type = None;
    for k in 0..units {
        let start = d.len();
        let u = unit4(start, 12 + k, &at, 17 * k, prev_type);
        // (a reference to the first type of the unit before, of DW_FORM_ref_addr)
        prev_type = Some(start as u32 + 11 + 1 + 4 + 8 + 8 + 4);
        d.extend_from_slice(&u);
    }
    (d, table(), strs)
}

#[test]
fn version4() {
    let (d, abbrev, strs) = section(5);
    let st = round_trip(&d, &abbrev, &strs);
    // every unit coded
    assert!(st.iter().zip(aqr_dwarf::info::STREAM_NAMES).all(|(s, n)| n != "raw" || s.is_empty()));
    assert_eq!(st.len(), NUM_STREAMS);
}

#[test]
fn version5() {
    // a unit of version 5 (a skeleton, of a DWO id), strings by index
    let abbrev = abbrev_table(&[(1, 0x4a, true, &[(NAME, STRX1), (LOW_PC, ADDR)]), (2, 0x34, false, &[(NAME, STRX1)])]);
    let mut u = vec![0, 0, 0, 0, 5, 0, 4, 8, 0, 0, 0, 0];
    u.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    u.extend_from_slice(&[1, 0]);
    u.extend_from_slice(&0x4000u64.to_le_bytes());
    for i in 1..40 {
        u.extend_from_slice(&[2, i]);
    }
    u.push(0);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    round_trip(&u, &abbrev, &[]);
}

#[test]
fn units_kept() {
    let (mut d, abbrev, strs) = section(3);
    // a unit of an LEB128 longer than it needs (kept as it is), and bytes after the units
    let mut u = vec![0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 8, 0x83, 0x00];
    u.extend_from_slice(&0x10u32.to_le_bytes());
    u.extend_from_slice(&[1, 2, 3]);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    d.extend_from_slice(&u);
    d.extend_from_slice(&[0xff, 0xff]);
    let st = round_trip(&d, &abbrev, &strs);
    let raw = aqr_dwarf::info::STREAM_NAMES.iter().position(|&n| n == "raw").unwrap();
    assert_eq!(st[raw].len(), u.len() + 2);
}

#[test]
fn corrupt_inputs() {
    let (d, abbrev, strs) = section(2);
    let st = round_trip(&d, &abbrev, &strs);
    let run = |st: &[Vec<u8>], len: usize| {
        let inputs: Vec<&[u8]> = [&abbrev[..], &strs[..]].into_iter().chain(st.iter().map(|s| &s[..])).collect();
        decode(&inputs, len as u64)
    };
    assert!(run(&st, d.len() + 1).is_err());
    assert!(run(&st, d.len() - 1).is_err());
    for k in 0..st.len() {
        if st[k].is_empty() {
            continue;
        }
        let mut s = st.to_vec();
        s[k].pop();
        assert!(run(&s, d.len()).is_err(), "stream {k} cut");
        let mut s = st.to_vec();
        s[k].push(0x7f);
        assert!(run(&s, d.len()).is_err(), "stream {k} longer");
    }
    // anything, with no panic
    for seed in 0..200u64 {
        let mut s = st.to_vec();
        let k = (seed as usize) % s.len();
        if !s[k].is_empty() {
            let i = (seed as usize * 7919) % s[k].len();
            s[k][i] ^= 1 << (seed % 8);
            let _ = run(&s, d.len());
        }
    }
}
