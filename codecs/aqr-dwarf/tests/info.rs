//! `.debug_info` of units made here, of most forms, rebuilt by `dwarf-info`.

use std::time::Duration;

use aqr_dwarf::info::{INPUTS, NUM_STREAMS, decode, encode};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// What damaged input reserves stays far below this.
const SMALL: usize = 1 << 24;
const LIMIT: Duration = Duration::from_secs(30);

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
            u.extend_from_slice(&(sub.wrapping_sub(start) as u32).wrapping_add(7).to_le_bytes());
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

fn round_trip(d: &[u8], abbrev: &[u8], strs: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let (params, st) = encode(d, abbrev, strs).expect("taken");
    let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    assert_eq!(decode(&params, &inputs, d.len() as u64).unwrap(), d);
    // through the module: the streams packed
    let mut packed = vec![abbrev.to_vec(), strs.to_vec()];
    for (_, which) in INPUTS {
        packed.push(aqr_dwarf::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>()));
    }
    let p: Vec<&[u8]> = packed.iter().map(|v| &v[..]).collect();
    assert_eq!(aqr_dwarf::module::info(&params, &p, d.len() as u64).unwrap(), d);
    (params, st)
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
    let (_, st) = round_trip(&d, &abbrev, &strs);
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

/// Units of version 5 whose names are indices of `form` (DW_FORM_strx2 or strx3), `width` bytes
/// each: every unit is coded, none kept as it is.
fn indexed_names(form: u64, width: usize) {
    let abbrev = abbrev_table(&[(1, 0x4a, true, &[(NAME, form), (LOW_PC, ADDR)]), (2, 0x34, false, &[(NAME, form)])]);
    let mut u = vec![0, 0, 0, 0, 5, 0, 4, 8, 0, 0, 0, 0];
    u.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    u.extend_from_slice(&[1]);
    u.extend_from_slice(&[7, 0, 0x55][..width]);
    u.extend_from_slice(&0x4000u64.to_le_bytes());
    for i in 1..40u8 {
        u.push(2);
        u.extend_from_slice(&[i, 1, 0x55][..width]);
    }
    u.push(0);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    let (_, st) = round_trip(&u, &abbrev, &[]);
    let raw = aqr_dwarf::info::STREAM_NAMES.iter().position(|&n| n == "raw").unwrap();
    assert!(st[raw].is_empty());
}

#[test]
fn strx2() {
    indexed_names(0x26, 2);
}

#[test]
fn strx3() {
    indexed_names(0x27, 3);
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
    let (_, st) = round_trip(&d, &abbrev, &strs);
    let raw = aqr_dwarf::info::STREAM_NAMES.iter().position(|&n| n == "raw").unwrap();
    assert_eq!(st[raw].len(), u.len() + 2);
}

#[test]
fn corrupt_inputs() {
    let (d, abbrev, strs) = section(2);
    let (params, st) = round_trip(&d, &abbrev, &strs);
    let run = |st: &[Vec<u8>], len: usize| {
        let inputs: Vec<&[u8]> = [&abbrev[..], &strs[..]].into_iter().chain(st.iter().map(|s| &s[..])).collect();
        decode(&params, &inputs, len as u64)
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

/// `encode` takes `d` and then decodes to it, or does not take it: never a panic, and nothing
/// large reserved. Whether it was taken.
fn invariant(d: &[u8], abbrev: &[u8], strs: &[u8]) -> bool {
    aqr_testkit::reset();
    let r = encode(d, abbrev, strs);
    assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
    let Some((params, st)) = r else { return false };
    let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    assert!(decode(&params, &inputs, d.len() as u64).unwrap() == d, "taken, not rebuilt");
    true
}

/// Whether `d` is coded as units (none kept as it is), by `encode`; it also must round-trip.
fn coded(d: &[u8], abbrev: &[u8], strs: &[u8]) -> bool {
    aqr_testkit::reset();
    let r = encode(d, abbrev, strs);
    let Some((params, st)) = r else { return false };
    let inputs: Vec<&[u8]> = [abbrev, strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    assert!(decode(&params, &inputs, d.len() as u64).unwrap() == d, "taken, not rebuilt");
    let raw = aqr_dwarf::info::STREAM_NAMES.iter().position(|&n| n == "raw").unwrap();
    st[raw].is_empty()
}

/// A unit of `version` (2 to 4) with a header of address size `addr`, of the DIEs `body`.
fn unit_v(version: u16, addr: u8, body: &[u8]) -> Vec<u8> {
    let mut u = vec![0, 0, 0, 0];
    u.extend_from_slice(&version.to_le_bytes());
    u.extend_from_slice(&[0, 0, 0, 0, addr]);
    u.extend_from_slice(body);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    u
}

#[test]
fn versions_2_and_3() {
    // (the reference form of the unit's own section: an offset in 3 and 4, an address in 2)
    let abbrev =
        abbrev_table(&[(1, 0x11, true, &[(NAME, STRP), (LOW_PC, ADDR)]), (2, 0x0d, false, &[(TYPE, REF_ADDR)])]);
    let (strs, _) = strings(2);
    for (version, addr, refsize) in [(2, 8, 8), (2, 4, 4), (3, 8, 4), (3, 4, 4), (4, 8, 4)] {
        let mut body = vec![1];
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&vec![0x10; addr as usize]);
        for i in 0..20u8 {
            body.push(2);
            body.extend_from_slice(&vec![i; refsize]);
        }
        body.push(0);
        assert!(coded(&unit_v(version, addr, &body), &abbrev, &strs), "v{version} addr {addr}");
    }
    // other versions are kept as they are
    for version in [0, 1, 6, 0xffff] {
        let u = unit_v(version, 8, &[0]);
        assert!(!coded(&u, &abbrev, &strs), "v{version}");
    }
    // an address size that no machine has
    for addr in [0, 1, 2, 3, 5, 16, 255] {
        invariant(&unit_v(4, addr, &[1, 0, 0, 0, 0]), &abbrev, &strs);
    }
}

#[test]
fn units_of_64_bits_and_odd_lengths() {
    let (strs, _) = strings(2);
    let abbrev = abbrev_table(&[(1, 0x11, false, &[(NAME, STRP)])]);
    // DWARF64: 0xffffffff, a length of 8 bytes, and offsets of 8 bytes
    let mut u = vec![0xff; 4];
    u.extend_from_slice(&(2u64 + 8 + 1 + 1 + 8).to_le_bytes());
    u.extend_from_slice(&[4, 0]);
    u.extend_from_slice(&0u64.to_le_bytes());
    u.push(8);
    u.push(1);
    u.extend_from_slice(&0u64.to_le_bytes());
    invariant(&u, &abbrev, &strs);
    // lengths of the reserved range, past the section, zero, and units cut short
    for len in [0u32, 1, 6, 7, 0xffff_fff0, 0xffff_fffe, 0xffff_ffff, 0x7fff_ffff] {
        let mut u = unit_v(4, 8, &[1, 0, 0, 0, 0, 0]);
        u[..4].copy_from_slice(&len.to_le_bytes());
        invariant(&u, &abbrev, &strs);
    }
    let u = unit_v(4, 8, &[1, 0, 0, 0, 0, 0]);
    for cut in 0..u.len() {
        invariant(&u[..cut], &abbrev, &strs);
    }
    // two units, the second cut
    let two = [u.clone(), u.clone()].concat();
    assert!(invariant(&two, &abbrev, &strs));
    for cut in u.len()..two.len() {
        invariant(&two[..cut], &abbrev, &strs);
    }
    invariant(&[], &abbrev, &strs);
}

/// A version 5 unit of `unit_type`, its header's extra bytes `extra`, of `body`.
fn unit5(unit_type: u8, extra: &[u8], body: &[u8]) -> Vec<u8> {
    let mut u = vec![0, 0, 0, 0, 5, 0, unit_type, 8, 0, 0, 0, 0];
    u.extend_from_slice(extra);
    u.extend_from_slice(body);
    let len = (u.len() - 4) as u32;
    u[..4].copy_from_slice(&len.to_le_bytes());
    u
}

#[test]
fn unit_types_of_version_5() {
    let abbrev = abbrev_table(&[(1, 0x11, true, &[(NAME, STRX1), (LOW_PC, ADDR)]), (2, 0x34, false, &[(NAME, STRX1)])]);
    let mut body = vec![1, 0];
    body.extend_from_slice(&0x4000u64.to_le_bytes());
    for i in 1..30u8 {
        body.extend_from_slice(&[2, i]);
    }
    body.push(0);
    // (compile, type, partial, skeleton, split compile, split type, and types that do not exist)
    for (t, extra) in [(1u8, 0), (2, 12), (3, 0), (4, 8), (5, 8), (6, 12), (0, 0), (7, 0), (0x80, 0), (0xff, 8)] {
        let u = unit5(t, &vec![0x5a; extra], &body);
        let taken = coded(&u, &abbrev, &[]);
        assert_eq!(taken, (1..=6).contains(&t), "unit type {t}");
        // the header cut inside its extra bytes
        for cut in 12..12 + extra {
            invariant(&u[..cut], &abbrev, &[]);
        }
    }
    // extra bytes of a unit type that has none, as if of another
    invariant(&unit5(1, &[0; 12], &body), &abbrev, &[]);
    // an address size of 3
    let mut bad = unit5(1, &[], &body);
    bad[7] = 3;
    invariant(&bad, &abbrev, &[]);
}

#[test]
fn abbreviation_tables() {
    let (strs, _) = strings(4);
    let unit = |code: u8| unit_v(4, 8, &[code, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    // duplicate codes, codes that start at 0 and skip, a code of 2^16 and of 2^40, the table
    // unterminated or cut inside an entry
    let dup = abbrev_table(&[(1, 0x11, false, &[(NAME, STRP)]), (1, 0x11, false, &[(NAME, DATA4)])]);
    let skip = abbrev_table(&[(5, 0x11, false, &[(NAME, STRP)])]);
    let high = abbrev_table(&[(1 << 16, 0x11, false, &[(NAME, STRP)])]);
    let higher = abbrev_table(&[(1 << 16 | 1, 0x11, false, &[(NAME, STRP)])]);
    let huge = abbrev_table(&[(1 << 40, 0x11, false, &[(NAME, STRP)])]);
    let zero = abbrev_table(&[(0, 0x11, false, &[(NAME, STRP)])]);
    let ok = abbrev_table(&[(1, 0x11, false, &[(NAME, STRP)])]);
    for (t, code) in [(&dup, 1u8), (&skip, 5), (&high, 0x80), (&higher, 0x81), (&huge, 0x80), (&zero, 0), (&ok, 1)] {
        let body = unit(code);
        invariant(&body, t, &strs);
        for cut in 0..t.len() {
            invariant(&body, &t[..cut], &strs);
        }
    }
    // an attribute of an unknown form, and forms in a loop (indirect)
    for form in [0u64, 0x7f, 0x1f01, 0x20, 0x2d, 1 << 40, INDIRECT] {
        let t = abbrev_table(&[(1, 0x11, false, &[(NAME, form), (LOW_PC, form)])]);
        invariant(&unit(1), &t, &strs);
    }
    // a table of very many attributes, and units naming offsets all over the table (each its
    // own table to parse)
    let attrs: Vec<(u64, u64)> = (0..5000).map(|i| (0x2000 + i, DATA1)).collect();
    let t = abbrev_table(&[(1, 0x11, false, &attrs)]);
    invariant(&unit(1), &t, &strs);
    let mut many = Vec::new();
    for off in 0..t.len().min(2000) {
        let mut u = unit_v(4, 8, &[1, 0, 0, 0, 0, 0]);
        u[6..10].copy_from_slice(&(off as u32).to_le_bytes());
        many.extend_from_slice(&u);
    }
    aqr_testkit::within(LIMIT, || invariant(&many, &t, &strs)).unwrap();
}

#[test]
fn forms_past_their_data() {
    let (strs, _) = strings(4);
    // each form's value cut short by the end of the unit
    let forms = (0..0x2du64).chain([0x1f01, 0x1f02]);
    for form in forms {
        let t = abbrev_table(&[(1, 0x11, false, &[(NAME, form)]), (2, 0x34, false, &[(NAME, form)])]);
        for fill in [0u8, 0x7f, 0x80, 0xff] {
            for n in 0..14 {
                let mut body = vec![1];
                body.extend_from_slice(&vec![fill; n]);
                invariant(&unit_v(4, 8, &body), &t, &strs);
                invariant(&unit5(1, &[], &body), &t, &strs);
            }
        }
    }
}

#[test]
fn damaged_info() {
    let (d, abbrev, strs) = section(2);
    aqr_testkit::damaged(&d, 300, 64, LIMIT, |v| {
        invariant(v, &abbrev, &strs);
    });
    aqr_testkit::damaged(&abbrev, 300, 64, LIMIT, |v| {
        invariant(&d, v, &strs);
    });
    aqr_testkit::damaged(&strs, 100, 8, LIMIT, |v| {
        invariant(&d, &abbrev, v);
    });
}

#[test]
fn damaged_streams() {
    let (d, abbrev, strs) = section(2);
    let (params, st) = round_trip(&d, &abbrev, &strs);
    let run = |params: &[u8], st: &[Vec<u8>], len: u64| {
        let inputs: Vec<&[u8]> = [&abbrev[..], &strs[..]].into_iter().chain(st.iter().map(|s| &s[..])).collect();
        aqr_testkit::reset();
        if let Ok(o) = decode(params, &inputs, len) {
            assert_eq!(o.len() as u64, len);
        }
        assert!(aqr_testkit::most() < SMALL, "{} bytes reserved", aqr_testkit::most());
    };
    aqr_testkit::damaged(&params, 100, 64, LIMIT, |v| run(v, &st, d.len() as u64));
    for k in 0..st.len() {
        aqr_testkit::damaged(&st[k], 100, 32, LIMIT, |v| {
            let mut s = st.clone();
            s[k] = v.to_vec();
            run(&params, &s, d.len() as u64);
        });
    }
    for len in [0, 1, u64::MAX, 1 << 40, 1 << 33, d.len() as u64 * 100] {
        aqr_testkit::case(&format!("length {len}"));
        run(&params, &st, len);
    }
}

/// A DWARF64 unit (offsets of 8 bytes) of a DIE of the abbreviation 1 and a string offset.
fn unit64(version: u16, name: u64) -> Vec<u8> {
    let mut u = vec![0xff; 4];
    let mut rest = Vec::new();
    rest.extend_from_slice(&version.to_le_bytes());
    if version >= 5 {
        rest.extend_from_slice(&[1, 8]);
        rest.extend_from_slice(&0u64.to_le_bytes());
    } else {
        rest.extend_from_slice(&0u64.to_le_bytes());
        rest.push(8);
    }
    rest.push(1);
    rest.extend_from_slice(&name.to_le_bytes());
    u.extend_from_slice(&(rest.len() as u64).to_le_bytes());
    u.extend(rest);
    u
}

/// DWARF64 units are kept as they are (the raw stream holds them whole) among units coded, and
/// round-trip alone and with others around them.
#[test]
fn dwarf64_units_are_kept_raw() {
    let raw = aqr_dwarf::info::STREAM_NAMES.iter().position(|&n| n == "raw").unwrap();
    let (strs, _) = strings(4);
    let abbrev = abbrev_table(&[(1, 0x11, false, &[(NAME, STRP)])]);
    for version in [3u16, 4, 5] {
        let big = unit64(version, 3);
        // (a unit of 32 bits, of version 4, naming string 5 of the same table)
        let small = unit_v(4, 8, &[1, 5, 0, 0, 0]);
        for d in [
            [big.clone(), big.clone()].concat(),
            [small.clone(), big.clone()].concat(),
            [big.clone(), small.clone()].concat(),
            [small.clone(), big.clone(), small.clone()].concat(),
        ] {
            let (params, streams) = encode(&d, &abbrev, &strs).expect("taken");
            let inputs: Vec<&[u8]> =
                [&abbrev[..], &strs[..]].into_iter().chain(streams.iter().map(|s| &s[..])).collect();
            assert_eq!(decode(&params, &inputs, d.len() as u64).unwrap(), d, "version {version}");
            assert!(streams[raw].len() >= big.len(), "version {version}: the 64-bit unit is raw");
        }
        // a lone 64-bit unit is taken and kept whole
        let (params, streams) = encode(&big, &abbrev, &strs).expect("taken as it is");
        assert_eq!(streams[raw], big, "version {version}");
        let inputs: Vec<&[u8]> = [&abbrev[..], &strs[..]].into_iter().chain(streams.iter().map(|s| &s[..])).collect();
        assert_eq!(decode(&params, &inputs, big.len() as u64).unwrap(), big);
    }
}

/// Split units (skeleton and split compile and type units of version 5, and the split compile
/// units of the `.dwo` sections' kind in version 4 that carry no unit type) round-trip, coded or
/// kept; sections named `.dwo` are not DWARF sections of this crate (kept as the container's).
#[test]
fn split_units_round_trip() {
    let abbrev = abbrev_table(&[(1, 0x11, true, &[(NAME, STRX1)]), (2, 0x34, false, &[(NAME, STRX1)])]);
    let body = [1, 0, 2, 1, 2, 2, 0];
    for t in [4u8, 5, 6] {
        let u = unit5(t, &[0x11; 12][..if t == 6 { 12 } else { 8 }], &body);
        let d = [u.clone(), u.clone(), u].concat();
        assert!(invariant(&d, &abbrev, &[]), "unit type {t}");
    }
    // version 4 split units: DW_AT_GNU_dwo_id in the unit's DIE, no unit type
    let abbrev4 = abbrev_table(&[(1, 0x11, false, &[(0x2131, 0x07)])]);
    let mut u = vec![1];
    u.extend_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
    let u = unit_v(4, 8, &u);
    let d = [u.clone(), u.clone()].concat();
    assert!(invariant(&d, &abbrev4, &[]));
    for name in
        [&b".debug_info.dwo"[..], b".debug_abbrev.dwo", b".debug_str.dwo", b"__debug_info.dwo", b".debug_cu_index"]
    {
        assert!(aqr_dwarf::Section::by_name(name).is_none(), "{}", String::from_utf8_lossy(name));
    }
    assert_eq!(aqr_dwarf::Section::by_name(b".debug_info"), Some(aqr_dwarf::Section::Info));
}
