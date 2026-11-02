//! Apple accelerator tables made as LLVM's `AccelTable` lays them out, rebuilt by `dwarf-apple`.

use aqr_dwarf::apple::{decode, djb, encode};

/// `.debug_str` of `names`, and the offset of each.
fn strings(names: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut s = vec![0u8];
    let offsets = names
        .iter()
        .map(|n| {
            let at = s.len() as u32;
            s.extend_from_slice(n.as_bytes());
            s.push(0);
            at
        })
        .collect();
    (s, offsets)
}

/// A table of `buckets` buckets of `entries` (string offset, records), each record the DIE
/// offset and `extra` bytes of other atoms (forms `forms`, after the DIE offset's).
/// An entry: its string offset and records (the DIE offset, the other atoms).
type Entry = (u32, Vec<(u32, Vec<u8>)>);

fn table(strs: &[u8], entries: &[Entry], buckets: u32, forms: &[u16]) -> Vec<u8> {
    let s = |at: u32| {
        let t = &strs[at as usize..];
        &t[..t.iter().position(|&c| c == 0).unwrap()]
    };
    let mut sorted: Vec<_> = entries.iter().map(|e| (djb(s(e.0)) % buckets, djb(s(e.0)), e)).collect();
    sorted.sort_by_key(|x| (x.0, x.1, x.2.0));
    let groups: Vec<_> = sorted.chunk_by(|a, b| a.1 == b.1).collect();
    let mut out = Vec::new();
    let put32 = |o: &mut Vec<u8>, v: u32| o.extend_from_slice(&v.to_le_bytes());
    put32(&mut out, 0x4841_5348);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    put32(&mut out, buckets);
    put32(&mut out, groups.len() as u32);
    put32(&mut out, 8 + 4 * (1 + forms.len() as u32));
    put32(&mut out, 0);
    put32(&mut out, 1 + forms.len() as u32);
    out.extend_from_slice(&[1, 0, 6, 0]);
    for (k, f) in forms.iter().enumerate() {
        out.extend_from_slice(&(3 + k as u16).to_le_bytes());
        out.extend_from_slice(&f.to_le_bytes());
    }
    let mut first = vec![u32::MAX; buckets as usize];
    for (i, g) in groups.iter().enumerate().rev() {
        first[g[0].0 as usize] = i as u32;
    }
    first.iter().for_each(|&b| put32(&mut out, b));
    groups.iter().for_each(|g| put32(&mut out, g[0].1));
    let data_at = out.len() + 4 * groups.len();
    let mut data = Vec::new();
    for g in &groups {
        put32(&mut out, (data_at + data.len()) as u32);
        for (_, _, (strp, recs)) in g.iter() {
            put32(&mut data, *strp);
            put32(&mut data, recs.len() as u32);
            for (die, extra) in recs {
                put32(&mut data, *die);
                data.extend_from_slice(extra);
            }
        }
        put32(&mut data, 0);
    }
    out.extend_from_slice(&data);
    out
}

fn round_trip(strs: &[u8], t: &[u8]) -> (Vec<u8>, [Vec<u8>; 4]) {
    let (params, st) = encode(t, strs).expect("taken");
    let inputs: Vec<&[u8]> = [strs].into_iter().chain(st.iter().map(|s| &s[..])).collect();
    assert_eq!(decode(&params, &inputs, t.len() as u64).unwrap(), t);
    (params, st)
}

#[test]
fn names() {
    let names: Vec<String> = (0..500).map(|i| format!("name_{i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let (strs, at) = strings(&refs);
    let entries: Vec<_> = at
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, (0..1 + i % 4).map(|k| (100 + 37 * i as u32 + 9 * k as u32, vec![])).collect()))
        .collect();
    let t = table(&strs, &entries, 125, &[]);
    round_trip(&strs, &t);
}

#[test]
fn types_with_atoms() {
    let (strs, at) = strings(&["int", "char", "Foo", "Bar", "Baz"]);
    let entries: Vec<_> = at.iter().map(|&s| (s, vec![(s * 3, vec![0x13, 0, 2])])).collect();
    // DW_ATOM_die_tag (data2), DW_ATOM_type_flags (data1)
    let t = table(&strs, &entries, 2, &[0x05, 0x0b]);
    round_trip(&strs, &t);
}

#[test]
fn collisions() {
    // "Ab" and "BA" (and "Ac" and "BB") have the same DJB hash: entries of a group
    assert_eq!(djb(b"Ab"), djb(b"BA"));
    let (strs, at) = strings(&["BA", "Ab", "x", "BB", "Ac"]);
    let entries: Vec<_> = at.iter().map(|&s| (s, vec![(s, vec![])])).collect();
    let t = table(&strs, &entries, 3, &[]);
    round_trip(&strs, &t);
}

#[test]
fn empty() {
    let (strs, _) = strings(&[]);
    let t = table(&strs, &[], 1, &[]);
    round_trip(&strs, &t);
}

#[test]
fn not_taken() {
    let (strs, at) = strings(&["a", "b", "c"]);
    let entries: Vec<_> = at.iter().map(|&s| (s, vec![(s, vec![])])).collect();
    let t = table(&strs, &entries, 2, &[]);
    // a hash not of its string
    let mut bad = t.clone();
    let hashes = 36 + 4 * 2;
    bad[hashes] ^= 1;
    assert!(encode(&bad, &strs).is_none());
    // a trailing byte
    let mut longer = t.clone();
    longer.push(0);
    assert!(encode(&longer, &strs).is_none());
    // another hash function
    let mut other = t;
    other[6] = 1;
    assert!(encode(&other, &strs).is_none());
}

#[test]
fn corrupt_inputs() {
    let (strs, at) = strings(&["a", "b", "c"]);
    let entries: Vec<_> = at.iter().map(|&s| (s, vec![(s, vec![])])).collect();
    let t = table(&strs, &entries, 2, &[]);
    let (params, st) = round_trip(&strs, &t);
    let len = t.len() as u64;
    let inputs = |st: &[Vec<u8>]| -> Vec<Vec<u8>> { [strs.clone()].into_iter().chain(st.iter().cloned()).collect() };
    let run = |i: Vec<Vec<u8>>, len| decode(&params, &i.iter().map(|v| &v[..]).collect::<Vec<_>>(), len);
    assert!(run(inputs(&st), len + 1).is_err());
    let mut s = st.clone();
    s[1] = vec![200];
    assert!(run(inputs(&s), len).is_err());
    let mut s = st.clone();
    s[0].push(100);
    assert!(run(inputs(&s), len).is_err());
    assert!(decode(&params, &[&strs[..]], len).is_err());
}
