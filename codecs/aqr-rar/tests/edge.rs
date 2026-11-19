//! Edge cases: how `read` groups files into streams (split, encrypted, stored between solid
//! members, unknown sizes, a dictionary changing), made by synthetic headers over the fixtures'
//! packed data; large sizes, encrypted headers, service headers, unsupported versions, Unicode
//! names; damaged archives, and damaged data and index of the modules.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_common::{crc32, varint};
use aqr_formula::{Apply, Formula};
use aqr_rar::{formula, rar4, rar5, recons3, recons5};
use aqr_testkit::{Refusing, damaged};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| formula::apply(a, inputs, 2).map_err(aqr_formula::Error::new)).unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}.rar", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// What an archive makes of `d`: no panic, and a formula, if any, rebuilds it.
fn check(d: &[u8]) -> Option<formula::Encoded> {
    let _ = formula::extent(d);
    let e = formula::encode_with(d, &formula::Options { threads: 2, ..Default::default() })?;
    assert!(eval(&e.formula) == d, "formula does not rebuild");
    Some(e)
}

fn vint(v: u64) -> Vec<u8> {
    let mut o = Vec::new();
    varint::put(&mut o, v);
    o
}

// ---- RAR5

#[derive(Clone)]
struct R5 {
    name: Vec<u8>,
    ci: u64,
    size: Option<u64>,
    crc: u32,
    packed: Vec<u8>,
    /// header flags (bit 3: split before, 4: split after)
    hflags: u64,
    extra: Vec<u8>,
    service: bool,
}

/// A header of `kind`: its fields, extra area and the data's size.
fn h5(kind: u64, flags: u64, fields: &[u8], extra: &[u8], data: usize) -> Vec<u8> {
    let mut f = flags;
    if !extra.is_empty() {
        f |= 1;
    }
    if data != 0 {
        f |= 2;
    }
    let mut body = [vint(kind), vint(f)].concat();
    if !extra.is_empty() {
        body.extend(vint(extra.len() as u64));
    }
    if data != 0 {
        body.extend(vint(data as u64));
    }
    body.extend_from_slice(fields);
    body.extend_from_slice(extra);
    let h = [vint(body.len() as u64), body].concat();
    [crc32::hash(&h).to_le_bytes().to_vec(), h].concat()
}

fn file5(r: &R5) -> Vec<u8> {
    let ff = 4 | if r.size.is_none() { 8 } else { 0 };
    let mut f = [vint(ff), vint(r.size.unwrap_or(0)), vint(0)].concat();
    f.extend_from_slice(&r.crc.to_le_bytes());
    f.extend(vint(r.ci));
    f.extend(vint(0));
    f.extend(vint(r.name.len() as u64));
    f.extend_from_slice(&r.name);
    let h = h5(if r.service { 3 } else { 2 }, r.hflags, &f, &r.extra, r.packed.len());
    [h, r.packed.clone()].concat()
}

fn end5() -> Vec<u8> {
    h5(5, 0, &vint(0), &[], 0)
}

fn build5(rs: &[R5]) -> Vec<u8> {
    let mut d = rar5::SIGNATURE.to_vec();
    d.extend(h5(1, 0, &vint(0), &[], 0));
    for r in rs {
        d.extend(file5(r));
    }
    d.extend(end5());
    d
}

/// The records of the compressed files of fixture `name`.
fn recs5(name: &str) -> Vec<R5> {
    let d = fixture(name);
    let a = rar5::parse(&d).unwrap();
    a.entries
        .iter()
        .filter(|e| !e.service && !e.dir && !e.data.is_empty())
        .map(|e| {
            let n = e.dict.trailing_zeros() as u64 - 17;
            let mut ci = n << 10 | (e.method as u64) << 7 | (e.solid as u64) << 6;
            if e.version == 1 {
                ci |= 1;
            }
            R5 {
                name: d[e.name.clone()].to_vec(),
                ci,
                size: e.size,
                crc: e.crc.unwrap_or(0),
                packed: d[e.data.clone()].to_vec(),
                hflags: 0,
                extra: Vec::new(),
                service: false,
            }
        })
        .collect()
}

/// The solid fixture's records, at least 3 of them, the first not solid, the others solid.
fn solid5() -> Vec<R5> {
    let rs = recs5("r5_solid");
    assert!(rs.len() >= 3 && rs[0].ci & 0x40 == 0 && rs[1..].iter().all(|r| r.ci & 0x40 != 0));
    rs
}

fn rebuilt5(rs: &[R5]) -> Vec<bool> {
    let d = build5(rs);
    let a = rar5::parse(&d).unwrap();
    assert_eq!(a.entries.len(), rs.len());
    check(&d).map_or(vec![false; rs.len()], |e| e.rebuilt)
}

#[test]
fn rar5_builder_matches_the_fixtures() {
    for name in ["r5_m3", "r5_solid"] {
        let rs = recs5(name);
        let d = build5(&rs);
        let (a, o) = (rar5::parse(&d).unwrap(), rar5::parse(&fixture(name)).unwrap());
        let key = |e: &rar5::Entry| (e.method, e.dict, e.solid, e.version, e.size);
        let o: Vec<_> = o.entries.iter().filter(|e| !e.service && !e.dir && !e.data.is_empty()).map(key).collect();
        assert_eq!(a.entries.iter().map(key).collect::<Vec<_>>(), o, "{name}");
        assert!(rebuilt5(&rs).iter().all(|&b| b), "{name}");
    }
}

#[test]
fn rar5_groups_are_taken_whole_or_not_at_all() {
    let base = solid5();
    let n = base.len();
    // a member split from the next volume, or encrypted: the whole group is kept
    for k in 0..n {
        for how in 0..3 {
            let mut rs = base.clone();
            match how {
                0 => rs[k].hflags |= 0x08,
                1 => rs[k].hflags |= 0x10,
                // an encryption record: size, type 1, flags, KDF count, salt, IV, check
                _ => {
                    let rec = [vec![1, 0, 15], vec![0; 16], vec![0; 16], vec![0; 12]].concat();
                    rs[k].extra = [vint(rec.len() as u64), rec].concat();
                }
            }
            let r = rebuilt5(&rs);
            assert!(r.iter().all(|&b| !b), "how {how} on {k}: {r:?}");
        }
    }
    // an unknown size
    for k in 0..n {
        let mut rs = base.clone();
        rs[k].size = None;
        assert!(rebuilt5(&rs).iter().all(|&b| !b), "unknown size on {k}");
    }
    // a dictionary changing within the group
    let mut rs = base.clone();
    rs[1].ci += 1 << 10;
    assert!(rebuilt5(&rs).iter().all(|&b| !b), "dictionary");
    // a stored file between solid members ends the stream: the first file alone may be taken,
    // the ones after it (solid, with no stream open) are not
    let mut rs = base.clone();
    let stored = R5 { ci: 0, packed: vec![7; 30], size: Some(30), crc: 0, name: b"s".to_vec(), ..rs[0].clone() };
    rs.insert(1, stored);
    let r = rebuilt5(&rs);
    assert!(r[1], "the stored file is its contents");
    assert!(r[2..].iter().all(|&b| !b), "{r:?}");
}

#[test]
fn rar5_headers_and_service_records() {
    let base = solid5();
    // a service header (a comment) between files
    let mut rs = base.clone();
    let cmt =
        R5 { service: true, ci: 0, size: Some(3), packed: b"CMT".to_vec(), name: b"CMT".to_vec(), ..rs[0].clone() };
    rs.insert(1, cmt);
    let d = build5(&rs);
    let a = rar5::parse(&d).unwrap();
    assert_eq!(a.entries.iter().filter(|e| e.service).count(), 1);
    check(&d);
    // extra records that overrun, or are empty
    for extra in
        [vec![200, 3, 0], vec![0], vec![2, 3], vec![3, 5, 1, 0xff, 0xff], vec![5, 3, 0, 0, 0xff], vec![1, 3, 9]]
    {
        let mut rs = base.clone();
        aqr_testkit::case(&format!("extra {extra:?}"));
        rs[0].extra = extra;
        check(&build5(&rs));
    }
    // an encrypted-headers block (type 4) after the first file: the archive ends there
    let first = build5(&base[..1]);
    let mut d = first[..first.len() - end5().len()].to_vec();
    let end = d.len();
    d.extend(h5(4, 0, &[vint(0), vec![0; 49]].concat(), &[], 0));
    d.extend(file5(&base[1]));
    assert_eq!(rar5::parse(&d).unwrap().end, end);
    check(&d);
    // encrypted headers before any file: not an archive of files
    let mut e = rar5::SIGNATURE.to_vec();
    e.extend(h5(1, 0, &vint(0), &[], 0));
    e.extend(h5(4, 0, &[vint(0), vec![0; 49]].concat(), &[], 0));
    e.extend(file5(&base[0]));
    assert!(rar5::parse(&e).is_none() && formula::encode(&e, 2).is_none());
    // a size past anything, and data cut short
    let mut big = base[0].clone();
    big.size = Some(u64::MAX);
    aqr_testkit::case("size u64::MAX");
    check(&build5(&[big]));
    let mut d = build5(&[base[0].clone()]);
    aqr_testkit::case("data cut short");
    let at = d.len() - end5().len() - base[0].packed.len();
    d.drain(at..at + 100);
    check(&d);
}

// ---- RAR 1.5-4.x

#[derive(Clone)]
struct R4 {
    name: Vec<u8>,
    flags: u16,
    version: u8,
    method: u8,
    size: u64,
    crc: u32,
    packed: Vec<u8>,
    high: Option<(u32, u32)>,
    service: bool,
}

fn h4(kind: u8, flags: u16, rest: &[u8]) -> Vec<u8> {
    let mut h = vec![0, 0, kind];
    h.extend_from_slice(&flags.to_le_bytes());
    h.extend_from_slice(&((rest.len() + 7) as u16).to_le_bytes());
    h.extend_from_slice(rest);
    let c = crc32::hash(&h[2..]) as u16;
    h[..2].copy_from_slice(&c.to_le_bytes());
    h
}

fn file4(r: &R4) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&(r.packed.len() as u32).to_le_bytes());
    f.extend_from_slice(&(r.size as u32).to_le_bytes());
    f.push(2);
    f.extend_from_slice(&r.crc.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&[r.version, r.method]);
    f.extend_from_slice(&(r.name.len() as u16).to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    let mut flags = r.flags;
    if let Some((p, s)) = r.high {
        flags |= 0x0100;
        f.extend_from_slice(&p.to_le_bytes());
        f.extend_from_slice(&s.to_le_bytes());
    }
    f.extend_from_slice(&r.name);
    let h = h4(if r.service { 0x7a } else { 0x74 }, flags, &f);
    [h, r.packed.clone()].concat()
}

fn build4(rs: &[R4], main_flags: u16) -> Vec<u8> {
    let mut d = rar4::SIGNATURE.to_vec();
    d.extend(h4(0x73, main_flags, &[0; 6]));
    for r in rs {
        d.extend(file4(r));
    }
    d.extend(h4(0x7b, 0, &[]));
    d
}

fn recs4(name: &str) -> Vec<R4> {
    let d = fixture(name);
    let a = rar4::parse(&d).unwrap();
    a.entries
        .iter()
        .filter(|e| !e.service && !e.dir && !e.data.is_empty())
        .map(|e| {
            let n = if e.method == 0x30 { 3 } else { e.dict.trailing_zeros() as u16 - 16 };
            R4 {
                name: d[e.name.clone()].to_vec(),
                flags: n << 5 | (e.solid as u16) << 4,
                version: e.version,
                method: e.method,
                size: e.size,
                crc: e.crc,
                packed: d[e.data.clone()].to_vec(),
                high: None,
                service: false,
            }
        })
        .collect()
}

fn solid4() -> Vec<R4> {
    let rs = recs4("r4_solid");
    assert!(rs.len() >= 3 && rs[0].flags & 0x10 == 0 && rs[1..].iter().all(|r| r.flags & 0x10 != 0));
    rs
}

fn rebuilt4(rs: &[R4]) -> Vec<bool> {
    let d = build4(rs, 0x0008);
    let a = rar4::parse(&d).unwrap();
    assert_eq!(a.entries.len(), rs.len());
    check(&d).map_or(vec![false; rs.len()], |e| e.rebuilt)
}

#[test]
fn rar4_builder_matches_the_fixtures() {
    for name in ["r4_m3", "r4_solid", "r4_ppmd", "r4_audio"] {
        let rs = recs4(name);
        let d = build4(&rs, 0x0008);
        let a = rar4::parse(&d).unwrap();
        let o = rar4::parse(&fixture(name)).unwrap();
        let key = |e: &rar4::Entry| (e.method, e.dict, e.solid, e.version, e.size);
        let o: Vec<_> = o.entries.iter().filter(|e| !e.service && !e.dir && !e.data.is_empty()).map(key).collect();
        assert_eq!(a.entries.iter().map(key).collect::<Vec<_>>(), o, "{name}");
        assert!(rebuilt4(&rs).iter().all(|&b| b), "{name}");
    }
}

#[test]
fn rar4_groups_are_taken_whole_or_not_at_all() {
    let base = solid4();
    let n = base.len();
    for k in 0..n {
        for flag in [0x0001, 0x0002, 0x0004] {
            let mut rs = base.clone();
            rs[k].flags |= flag;
            let r = rebuilt4(&rs);
            assert!(r.iter().all(|&b| !b), "flag {flag} on {k}: {r:?}");
        }
        // the algorithms of RAR 1.5 and 2.0, and unknown ones
        for v in [15, 20, 30, 0] {
            let mut rs = base.clone();
            rs[k].version = v;
            let r = rebuilt4(&rs);
            assert!(r.iter().all(|&b| !b), "version {v} on {k}: {r:?}");
        }
    }
    // a dictionary changing within the group
    let mut rs = base.clone();
    rs[1].flags += 1 << 5;
    assert!(rebuilt4(&rs).iter().all(|&b| !b));
    // a stored file between solid members
    let mut rs = base.clone();
    let stored = R4 { method: 0x30, version: 20, packed: vec![7; 30], size: 30, name: b"s".to_vec(), ..rs[0].clone() };
    rs.insert(1, R4 { flags: 3 << 5, ..stored });
    let r = rebuilt4(&rs);
    assert!(r[1] && r[2..].iter().all(|&b| !b), "{r:?}");
}

#[test]
fn rar4_large_headers_and_blocks() {
    let base = solid4();
    // LHD_LARGE with zero high parts: the same sizes
    let mut rs = base.clone();
    rs.iter_mut().for_each(|r| r.high = Some((0, 0)));
    assert!(rebuilt4(&rs).iter().all(|&b| b));
    // high parts that the data cannot hold: the archive ends before the file
    for high in [(1, 0), (0x8000_0000, 0), (u32::MAX, u32::MAX), (0, 1), (0, u32::MAX)] {
        let mut rs = base.clone();
        aqr_testkit::case(&format!("high {high:?}"));
        rs[1].high = Some(high);
        let d = build4(&rs, 0x0008);
        let _ = rar4::parse(&d);
        check(&d);
    }
    // an encrypted main header block: no files
    let d = build4(&base, 0x0080 | 0x0008);
    assert!(rar4::parse(&d).is_none() && formula::encode(&d, 2).is_none());
    // a service block (a comment) between files, with its own packed data
    let mut rs = base.clone();
    let cmt =
        R4 { service: true, method: 0x30, size: 3, packed: b"CMT".to_vec(), name: b"CMT".to_vec(), ..rs[0].clone() };
    rs.insert(1, cmt);
    let d = build4(&rs, 0x0008);
    assert_eq!(rar4::parse(&d).unwrap().entries.iter().filter(|e| e.service).count(), 1);
    check(&d);
    // an unknown block with a data size past the file
    let mut d = build4(&base[..1], 0x0008);
    let end = d.len() - 7;
    d.truncate(end);
    d.extend(h4(0x79, 0x8000, &u32::MAX.to_le_bytes()));
    d.extend(file4(&base[1]));
    check(&d);
}

#[test]
fn rar4_unicode_names() {
    let n = |f: &[u8]| String::from_utf8(rar4::name(f, true)).unwrap();
    assert_eq!(n(b"abc"), "abc");
    // plain "abcde", high byte 4: ops 01 00 10 11 = U+0478, 'A', U+1234, then two of the plain name
    let f = [&b"abcde"[..], &[0, 0x04, 0x4b, 0x78, 0x41, 0x34, 0x12, 0x00]].concat();
    assert_eq!(n(&f), "\u{478}A\u{1234}de");
    // truncated or nonsense encodings end the name, never panic
    for k in 0..f.len() {
        let _ = rar4::name(&f[..k], true);
    }
    for b in 0..=255u8 {
        let g = [&b"xyz"[..], &[0, b, b, b, b, b]].concat();
        let _ = rar4::name(&g, true);
        let _ = rar4::name(&[0, b, b], true);
    }
    assert_eq!(rar4::name(b"a\0", true), b"a");
    assert_eq!(rar4::name(b"", true), b"");
    // a Unicode name in a header
    let mut rs = solid4();
    rs[0].name = f.clone();
    rs[0].flags |= 0x0200;
    let d = build4(&rs, 0x0008);
    let a = rar4::parse(&d).unwrap();
    assert!(a.entries[0].unicode);
    check(&d);
}

// ---- damage

#[test]
fn damaged_archives() {
    for name in ["r4_m3", "r4_solid", "r4_ppmd", "r4_stored", "r5_m3", "r5_solid", "r5_stored"] {
        let d = fixture(name);
        damaged(&d, 25, 150, LIMIT, |d| _ = check(d));
    }
}

#[test]
fn damaged_synthetic_archives() {
    let a = build5(&solid5());
    damaged(&a, 25, 200, LIMIT, |d| _ = check(d));
    let a = build4(&solid4(), 0x0008);
    damaged(&a, 25, 200, LIMIT, |d| _ = check(d));
}

/// The modules' three inputs (data, records, index) of a solid group, damaged in each in turn:
/// an error or some output, never a panic or a hang.
#[test]
fn damaged_data_records_and_index() {
    let d = fixture("r5_solid");
    let a = rar5::parse(&d).unwrap();
    let es: Vec<_> = a.entries.iter().filter(|e| !e.service && e.method != 0 && !e.data.is_empty()).collect();
    let parts: Vec<&[u8]> = es.iter().map(|e| &d[e.data.clone()]).collect();
    let sizes: Vec<usize> = es.iter().map(|e| e.size.unwrap() as usize).collect();
    let sp = recons5::split(&parts, &sizes, es[0].version == 1, es[0].dict).unwrap();
    let ix = formula::index(&[(sp.data.len(), sp.record.len())]);
    let total = parts.iter().map(|p| p.len() as u64).sum::<u64>();
    assert!(formula::decode5(&[], &[&sp.data, &sp.record, &ix], total, 2).unwrap() == parts.concat());
    for which in 0..3 {
        let inputs = [sp.data.clone(), sp.record.clone(), ix.clone()];
        damaged(&inputs[which], 40, 60, LIMIT, |x| {
            let mut v = inputs.clone();
            v[which] = x.to_vec();
            let _ = formula::decode5(&[], &[&v[0], &v[1], &v[2]], total, 2);
            let _ = formula::decode5(&[], &[&v[0], &v[1], &v[2]], u64::MAX, 2);
        });
    }

    let d = fixture("r4_solid");
    let a = rar4::parse(&d).unwrap();
    let es: Vec<_> = a.entries.iter().filter(|e| !e.service && e.method != 0x30 && !e.data.is_empty()).collect();
    let parts: Vec<&[u8]> = es.iter().map(|e| &d[e.data.clone()]).collect();
    let sizes: Vec<usize> = es.iter().map(|e| e.size as usize).collect();
    let sp = recons3::split(&parts, &sizes, es[0].dict).unwrap();
    let ix = formula::index(&[(sp.data.len(), sp.record.len())]);
    let params = formula::params3(0);
    let total = parts.iter().map(|p| p.len() as u64).sum::<u64>();
    assert!(formula::decode3(&params, &[&sp.data, &sp.record, &ix], total, 2).unwrap() == parts.concat());
    for which in 0..3 {
        let inputs = [sp.data.clone(), sp.record.clone(), ix.clone()];
        damaged(&inputs[which], 40, 60, LIMIT, |x| {
            let mut v = inputs.clone();
            v[which] = x.to_vec();
            let _ = formula::decode3(&params, &[&v[0], &v[1], &v[2]], total, 2);
            let _ = formula::decode3(&params, &[&v[0], &v[1], &v[2]], u64::MAX, 2);
        });
    }
}
