//! PDB files made here (an MSF container of a DBI stream and modules' streams of symbol records),
//! rebuilt by `pdb-msf` and `pdb-symbols`.

use aqr_pdb::{msf, symbols};

/// A record of `kind` and `payload`, padded to 4 bytes with `pad`.
fn record(kind: u16, payload: &[u8], pad: u8) -> Vec<u8> {
    let mut r = Vec::new();
    let n = (payload.len() + 4).next_multiple_of(4);
    r.extend_from_slice(&((n - 2) as u16).to_le_bytes());
    r.extend_from_slice(&kind.to_le_bytes());
    r.extend_from_slice(payload);
    r.resize(n, pad);
    r
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A module's symbol substream: procedures holding inline sites (nested), locals, blocks and
/// records of other kinds; `k` varies it.
fn module(k: u32) -> Vec<u8> {
    let mut m = 4u32.to_le_bytes().to_vec();
    let mut obj = u32s(&[0]);
    obj.extend_from_slice(format!("module{k}.obj\0").as_bytes());
    m.extend(record(0x1101, &obj, 0));
    for p in 0..6 + k % 3 {
        // S_GPROC32: parent, end and next set after
        let at = m.len();
        let mut proc = u32s(&[0, 0, 0, 0x40 + p, 4, 0x3c, 0x1000 + p, 0x2000 + 0x40 * p]);
        proc.extend_from_slice(&[1, 0, 0]);
        proc.extend_from_slice(format!("function_{k}_{p}\0").as_bytes());
        m.extend(record(0x1110, &proc, 0));
        let mut local = u32s(&[0x74]);
        local.extend_from_slice(&[1, 0]);
        local.extend_from_slice(b"x\0");
        m.extend(record(0x113e, &local, 0));
        m.extend(record(0x1012, &[7; 28], 0));
        // inline sites, the second within the first
        let mut sites = Vec::new();
        for i in 0..2 {
            let site = m.len();
            let parent = if i == 0 { at } else { sites[0] };
            let mut s = u32s(&[parent as u32, 0, 0x1000 + (p * 7 + i) % 5]);
            // annotations: code offset, code length, line offset (of 2 bytes), file
            s.extend_from_slice(&[3, 4 + i as u8, 4, 0x81, 0x22, 6, 4, 5, 0x18]);
            m.extend(record(0x114d, &s, 0));
            sites.push(site);
        }
        for &site in sites.iter().rev() {
            let end = m.len() as u32;
            m[site + 8..site + 12].copy_from_slice(&end.to_le_bytes());
            m.extend(record(0x114e, &[], 0));
        }
        // a block
        let block = m.len();
        let mut b = u32s(&[at as u32, 0, 8, 0x2010]);
        b.extend_from_slice(&[1, 0, 0]);
        m.extend(record(0x1103, &b, 0));
        let end = m.len() as u32;
        m[block + 8..block + 12].copy_from_slice(&end.to_le_bytes());
        m.extend(record(0x0006, &[], 0));
        // S_INLINEES, and annotations that are not as written (kept as they are)
        m.extend(record(0x1168, &u32s(&[2, 0x1000, 0x1001]), 0));
        let end = m.len() as u32;
        m[at + 8..at + 12].copy_from_slice(&end.to_le_bytes());
        m.extend(record(0x0006, &[], 0));
    }
    let odd_at = m.len();
    let mut odd = u32s(&[0, 0, 0x1003]);
    odd.extend_from_slice(&[0x80, 0x05, 9]);
    m.extend(record(0x114d, &odd, 0));
    let end = m.len() as u32;
    m[odd_at + 8..odd_at + 12].copy_from_slice(&end.to_le_bytes());
    m.extend(record(0x114e, &[], 0));
    // a record that does not fit its layout (a name without its NUL), and one of no layout
    m.extend(record(0x113e, &[1, 2, 3, 4, 5, 6, 7, 8], 9));
    m.extend(record(0x1234, b"anything", 0));
    m
}

/// A DBI stream naming module streams `streams` of symbol bytes `sizes`.
fn dbi(mods: &[(u16, usize)]) -> Vec<u8> {
    let mut recs = Vec::new();
    for (k, &(stream, size)) in mods.iter().enumerate() {
        let mut r = vec![0u8; 64];
        r[34..36].copy_from_slice(&stream.to_le_bytes());
        r[36..40].copy_from_slice(&(size as u32).to_le_bytes());
        r.extend_from_slice(format!("module{k}.obj\0module{k}.obj\0").as_bytes());
        r.resize(r.len().next_multiple_of(4), 0);
        recs.extend(r);
    }
    let mut d = vec![0u8; 64];
    d[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    d[24..28].copy_from_slice(&(recs.len() as u32).to_le_bytes());
    d.extend(recs);
    d
}

/// An MSF file of block size `bs` holding `streams` (`None` a stream of none), their blocks in
/// order but where `shuffle`, free blocks of `stale` bytes among them.
fn msf_file(bs: usize, streams: &[Option<Vec<u8>>], shuffle: bool, stale: u8) -> Vec<u8> {
    let blocks_of = |n: usize| n.div_ceil(bs);
    // blocks: 0 superblock, 1-2 free page maps, 3 the directory's list, then the rest
    let mut next = 4u32;
    let mut lists = Vec::new();
    for s in streams {
        let n = s.as_ref().map_or(0, |s| blocks_of(s.len()));
        let mut l: Vec<u32> = (next..next + n as u32).collect();
        if shuffle {
            l.reverse();
        }
        next += n as u32 + 1; // a free block after each
        lists.push(l);
    }
    let mut dir = (streams.len() as u32).to_le_bytes().to_vec();
    for s in streams {
        dir.extend_from_slice(&s.as_ref().map_or(u32::MAX, |s| s.len() as u32).to_le_bytes());
    }
    for l in &lists {
        dir.extend(u32s(l));
    }
    let dir_blocks: Vec<u32> = (next..next + blocks_of(dir.len()) as u32).collect();
    let nb = next as usize + dir_blocks.len();
    let mut d = vec![stale; nb * bs];
    d[..bs].fill(0);
    d[..32].copy_from_slice(msf::MAGIC);
    for (k, v) in [bs as u32, 1, nb as u32, dir.len() as u32, 0, 3].into_iter().enumerate() {
        d[32 + 4 * k..36 + 4 * k].copy_from_slice(&v.to_le_bytes());
    }
    d[bs..3 * bs].fill(0xff);
    d[3 * bs..4 * bs].fill(0);
    d[3 * bs..3 * bs + 4 * dir_blocks.len()].copy_from_slice(&u32s(&dir_blocks));
    for (chunk, &b) in dir.chunks(bs).zip(&dir_blocks) {
        d[b as usize * bs..][..bs].fill(0);
        d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
    }
    for (s, l) in streams.iter().zip(&lists) {
        let Some(s) = s else { continue };
        for (chunk, &b) in s.chunks(bs).zip(l) {
            d[b as usize * bs..][..bs].fill(0);
            d[b as usize * bs..][..chunk.len()].copy_from_slice(chunk);
        }
    }
    d
}

fn pdb(shuffle: bool) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mods: Vec<Vec<u8>> = (0..5).map(module).collect();
    let mut streams: Vec<Option<Vec<u8>>> = vec![Some(vec![0; 60]), Some(b"pdb info".to_vec()), Some(vec![1; 56])];
    let refs: Vec<(u16, usize)> = mods.iter().enumerate().map(|(k, m)| (5 + k as u16, m.len())).collect();
    streams.push(Some(dbi(&refs)));
    streams.push(None);
    for m in &mods {
        // the symbols, then lines and global references
        let mut s = m.clone();
        s.extend_from_slice(&[0xf2, 0, 0, 0, 8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0]);
        streams.push(Some(s));
    }
    (msf_file(512, &streams, shuffle, 0x5a), mods)
}

#[test]
fn msf_round_trip() {
    for shuffle in [false, true] {
        let (d, _) = pdb(shuffle);
        let inputs = msf::encode(&d).expect("taken");
        let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
        assert_eq!(msf::decode(&refs, d.len() as u64).unwrap(), d);
        assert_eq!(aqr_pdb::module::msf(&[], &refs, d.len() as u64).unwrap(), d);
    }
}

#[test]
fn msf_not_taken() {
    let (d, _) = pdb(false);
    // a byte more than the blocks
    let mut longer = d.clone();
    longer.push(0);
    assert!(msf::encode(&longer).is_none());
    // a block used twice: the second stream's first block made the first's
    let mut twice = d.clone();
    let dir_at = u32::from_le_bytes(d[3 * 512..3 * 512 + 4].try_into().unwrap()) as usize * 512;
    // (10 streams: the first's block, then the second's)
    let first = 4 + 4 * 10;
    let b0 = twice[dir_at + first..dir_at + first + 4].to_vec();
    twice[dir_at + first + 4..dir_at + first + 8].copy_from_slice(&b0);
    assert!(msf::encode(&twice).is_none());
}

#[test]
fn symbols_round_trip() {
    let (_, mods) = pdb(false);
    // (each module's stream its symbols and lines after them)
    let whole: Vec<Vec<u8>> = mods.iter().map(|m| [&m[..], b"lines"].concat()).collect();
    let info = |m: &Vec<u8>| aqr_pdb::dbi::Module { stream: 0, symbols: m.len(), c11: 0, c13: 0 };
    let subs: Vec<(&[u8], aqr_pdb::dbi::Module)> = whole.iter().zip(&mods).map(|(w, m)| (&w[..], info(m))).collect();
    let (params, st) = symbols::encode(&subs).expect("taken");
    let refs: Vec<&[u8]> = st.iter().map(|v| &v[..]).collect();
    assert_eq!(symbols::decode(&params, &refs).unwrap(), whole.concat());
    // (two blocks within a procedure and two inline sites, the second within the first, in it)
    assert_eq!(params, [3]);
    // fewer scopes open at once than there are
    assert!(symbols::decode(&[2], &refs).is_err());
    // every parent and end derived
    let scope = symbols::STREAM_NAMES.iter().position(|&n| n == "scope").unwrap();
    assert!(st[scope].iter().all(|&b| b == 0));
    // through the module, packed
    let packed: Vec<Vec<u8>> = symbols::INPUTS
        .iter()
        .map(|(_, which)| aqr_pdb::pack(&which.iter().map(|&k| &st[k][..]).collect::<Vec<_>>()))
        .collect();
    let p: Vec<&[u8]> = packed.iter().map(|v| &v[..]).collect();
    let len = whole.iter().map(Vec::len).sum::<usize>() as u64;
    assert_eq!(aqr_pdb::module::symbols(&params, &p, len).unwrap(), whole.concat());
    assert!(aqr_pdb::module::symbols(&params, &p, len + 1).is_err());
    assert!(aqr_pdb::module::symbols(&[], &p, len).is_err());
}

#[test]
fn symbols_corrupt() {
    let (_, mods) = pdb(false);
    let info = |m: &Vec<u8>| aqr_pdb::dbi::Module { stream: 0, symbols: m.len(), c11: 0, c13: 0 };
    let subs: Vec<(&[u8], aqr_pdb::dbi::Module)> = mods.iter().map(|m| (&m[..], info(m))).collect();
    let (params, st) = symbols::encode(&subs).unwrap();
    for k in 0..st.len() {
        if st[k].is_empty() {
            continue;
        }
        let mut s = st.clone();
        s[k].pop();
        let refs: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
        assert!(symbols::decode(&params, &refs).is_err(), "stream {k} cut");
    }
    for seed in 0..300usize {
        let mut s = st.clone();
        let k = seed % s.len();
        if !s[k].is_empty() {
            let i = seed * 7919 % s[k].len();
            s[k][i] ^= 1 << (seed % 8);
            let refs: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
            let _ = symbols::decode(&params, &refs);
        }
    }
}

#[test]
fn formula() {
    let (d, _) = pdb(true);
    // (too few symbols for the modules to pay)
    assert!(aqr_pdb::formula::encode(&d, 1).is_none());
}

/// The modules of this crate, natively.
struct Native;

impl aqr_formula::Modules for Native {
    fn apply(&self, a: &aqr_formula::Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
        let r = if a.module == aqr_pdb::formula::MODULE {
            aqr_pdb::formula::decode(&a.params, inputs, a.len)
        } else if a.module == aqr_pdb::formula::SYMBOLS {
            aqr_pdb::formula::decode_symbols(&a.params, inputs, a.len)
        } else if a.module == aqr_pdb::formula::TYPES {
            aqr_pdb::formula::decode_types(&a.params, inputs, a.len)
        } else {
            return Err(aqr_formula::Error::new(format!("no module {}", a.module)));
        };
        r.map_err(|e| aqr_formula::Error::new(e.to_string()))
    }

    /// The outputs of `pdb-types`, which are its output cut.
    fn apply_shared(&self, s: &aqr_formula::Shared, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<Vec<u8>>> {
        let a = s.apply().expect("an application");
        if a.module != aqr_pdb::formula::TYPES {
            return Ok(vec![self.apply(a, inputs)?]);
        }
        let all = self.apply(a, inputs)?;
        let mut rest = &all[..];
        Ok(s.sizes.iter().map(|&n| rest.split_off(..n as usize).unwrap().to_vec()).collect())
    }
}

#[test]
fn formula_symbols() {
    // modules of more symbols than the modules cost, their streams shuffled
    let mods: Vec<Vec<u8>> = (0..400).map(module).collect();
    let refs: Vec<(u16, usize)> = mods.iter().enumerate().map(|(k, m)| (5 + k as u16, m.len())).collect();
    let mut streams: Vec<Option<Vec<u8>>> = vec![Some(vec![0; 60]), Some(b"pdb info".to_vec()), Some(vec![1; 56])];
    streams.push(Some(dbi(&refs)));
    streams.push(None);
    for m in &mods {
        let mut s = m.clone();
        s.extend_from_slice(&[0, 0, 0, 0]);
        streams.push(Some(s));
    }
    let d = msf_file(4096, &streams, true, 0);
    let f = aqr_pdb::formula::encode(&d, 1).expect("taken");
    let modules: Vec<_> = f.applies().iter().map(|a| a.module.name).collect();
    assert_eq!(modules, ["pdb-msf", "pdb-symbols"]);
    assert_eq!(f.eval(&Native).unwrap(), d);
}

// ---------------------------------------------------------------------------------------------
// The type records (TPI and IPI streams) and their hash streams

fn pad_to(r: &mut Vec<u8>, align: bool) {
    // (the length counts from the kind; the compilers pad the record with its length to 4)
    while align && !(r.len() + 2).is_multiple_of(4) {
        let left = 4 - (r.len() + 2) % 4;
        r.push(0xf0 | left as u8);
    }
}

/// A type record of `kind` and `body`, padded (`align`) as the compilers do, with its length.
fn trec(kind: u16, body: &[u8], align: bool) -> Vec<u8> {
    let mut r = kind.to_le_bytes().to_vec();
    r.extend_from_slice(body);
    pad_to(&mut r, align);
    let mut out = (r.len() as u16).to_le_bytes().to_vec();
    out.extend(r);
    out
}

fn le16(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// A numeric leaf of `v` in the canonical form, or `code` and `v`'s bytes as they are.
fn numeric(v: i64) -> Vec<u8> {
    match v {
        0..=0x7fff => le16(v as u16),
        0x8000..=0xffff => [le16(0x8002), le16(v as u16)].concat(),
        0x1_0000..=0xffff_ffff => [le16(0x8004), (v as u32).to_le_bytes().to_vec()].concat(),
        i64::MIN..=-0x8000_0001 => [le16(0x8009), v.to_le_bytes().to_vec()].concat(),
        -0x8000_0000..=-0x8001 => [le16(0x8003), (v as i32).to_le_bytes().to_vec()].concat(),
        -0x8000..=-0x81 => [le16(0x8001), (v as i16).to_le_bytes().to_vec()].concat(),
        -0x80..=-1 => [le16(0x8000), vec![v as i8 as u8]].concat(),
        _ => [le16(0x800a), (v as u64).to_le_bytes().to_vec()].concat(),
    }
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// A field list member: the leaf's `kind` and `body`, padded.
fn member(kind: u16, body: &[u8], at: usize) -> Vec<u8> {
    let mut m = kind.to_le_bytes().to_vec();
    m.extend_from_slice(body);
    // (the padding counts the record's length and kind before it)
    while !(at + 4 + m.len()).is_multiple_of(4) {
        let left = 4 - (at + 4 + m.len()) % 4;
        m.push(0xf0 | left as u8);
    }
    m
}

/// A field list holding members of all kinds.
fn field_list(me: u32) -> Vec<u8> {
    let mut body = Vec::new();
    let mut add = |kind: u16, b: &[u8]| {
        let m = member(kind, b, body.len());
        body.extend(m);
    };
    let t = |v: u32| v.to_le_bytes().to_vec();
    add(0x1400, &[le16(0x1), t(0x1000), numeric(0)].concat());
    add(0x1401, &[le16(0x1), t(0x1000), t(0x74), numeric(8), numeric(16)].concat());
    add(0x1402, &[le16(0x1), t(0x1000), t(0x74), numeric(0x9000), numeric(70000)].concat());
    for (k, off) in [0i64, 8, 0x7fff, 0x8000, 70000, 5_000_000_000].into_iter().enumerate() {
        add(0x150d, &[le16(3), t(0x74 + k as u32), numeric(off), cstr(&format!("field{k}"))].concat());
    }
    for (k, v) in [0i64, 255, -1, -300, -70000, -5_000_000_000, 0x8000].into_iter().enumerate() {
        add(0x1502, &[le16(3), numeric(v), cstr(&format!("VALUE_{k}"))].concat());
    }
    // a value in a form that is not the shortest, and one that is not whole
    add(0x1502, &[le16(3), le16(0x8004), 5u32.to_le_bytes().to_vec(), cstr("LONG_FIVE")].concat());
    add(0x1502, &[le16(3), le16(0x8006), 1.5f64.to_le_bytes().to_vec(), cstr("A_DOUBLE")].concat());
    add(0x150e, &[le16(3), t(0x74), cstr("s_static")].concat());
    add(0x150f, &[le16(2), t(me - 1), cstr("overloaded")].concat());
    add(0x1510, &[le16(0), t(0x1001), cstr("Nested")].concat());
    add(0x1512, &[le16(3), t(0x1001), cstr("NestedEx")].concat());
    add(0x1511, &[le16(0x0b), t(0x1002), cstr("method")].concat());
    // (introducing a virtual function: 4 in bits 2 to 4 of the attributes)
    add(0x1511, &[le16(0x13), t(0x1002), 8u32.to_le_bytes().to_vec(), cstr("virt")].concat());
    add(0x1409, &[le16(0), t(0x1003)].concat());
    add(0x1404, &[le16(0), t(me + 3)].concat());
    trec(0x1203, &body, false)
}

/// The type records: every kind with a layout, fillers to make the index, a record of a kind with
/// none, one that does not fit its kind's, and one referring forward.
fn type_records(fill: usize) -> Vec<Vec<u8>> {
    let t = |v: u32| v.to_le_bytes().to_vec();
    let base = 0x1000u32;
    let mut r: Vec<Vec<u8>> = vec![
        trec(0x1201, &t(0), true),
        trec(0x1201, &[t(2), t(0x74), t(base)].concat(), true),
        trec(0x1008, &[t(3), vec![0, 0], le16(2), t(base + 1)].concat(), false),
        trec(0x1001, &[t(0x74), le16(1)].concat(), true),
        trec(0x1002, &[t(base + 3), t(0x1000c)].concat(), true),
        // a pointer to a member function: its class and form
        trec(0x1002, &[t(base + 2), t(0x1000c | 3 << 5), t(base + 3), le16(2)].concat(), true),
        trec(0x000a, &[le16(3), vec![0x11, 0x01]].concat(), true),
        trec(0x1205, &[t(0x74), vec![3, 1]].concat(), true),
    ];
    r.push(field_list(base + r.len() as u32));
    r.push(trec(0x1206, &[le16(0x0b), le16(0), t(base + 2), le16(0x13), le16(0), t(base + 2), t(4)].concat(), false));
    let me = base + r.len() as u32;
    let tag = |kind: u16, prop: u16, name: &str, unique: Option<&str>| {
        let mut b = [le16(0), le16(prop)].concat();
        match kind {
            0x1507 => b.extend([t(0x74), t(base + 8)].concat()),
            0x1506 => b.extend([t(base + 8), numeric(0x20)].concat()),
            _ => b.extend([t(base + 8), t(0), t(0), numeric(0x20000)].concat()),
        }
        b.extend(cstr(name));
        if let Some(u) = unique {
            b.extend(cstr(u));
        }
        trec(kind, &b, true)
    };
    r.push(tag(0x1504, 0x280, "A", Some(".?AVA@@")));
    r.push(tag(0x1505, 0x200, "B::C", Some(".?AUC@B@@")));
    r.push(tag(0x1506, 0x300, "U<int>", Some(".?ATU@@")));
    r.push(tag(0x1507, 0x200, "E", Some(".?AW4E@@")));
    r.push(tag(0x1507, 0x300, "S::E", Some(".?AW4E@S@@")));
    r.push(tag(0x1505, 0x200, "<unnamed-tag>", Some(".?AU<unnamed-tag>@@")));
    r.push(tag(0x1505, 0x0, "NoUnique", None));
    r.push(trec(0x1503, &[t(0x74), t(0x23), numeric(40), cstr("")].concat(), true));
    r.push(trec(
        0x1009,
        &[t(3), t(me), t(me + 1), vec![0, 0], le16(1), t(base + 1), 0i32.to_le_bytes().to_vec()].concat(),
        false,
    ));
    // a kind of no layout, and records that do not fit theirs
    r.push(trec(0x1234, b"anything at all", false));
    r.push(trec(0x1001, &[t(0x74), le16(1), vec![9]].concat(), false));
    r.push(trec(0x1002, &[t(0x74)].concat(), false));
    // a pointer to a record after it
    let ahead = base + r.len() as u32 + fill as u32 + 4;
    r.push(trec(0x1002, &[t(ahead), t(0x1000c)].concat(), true));
    for k in 0..fill {
        let name = format!("filler::Type{k}<int, filler::Other{}>", k % 7);
        r.push(tag(0x1505, 0x200, &name, Some(&format!(".?AU?$Type{k}@H@filler@@"))));
    }
    r
}

/// The records of an IPI stream.
fn id_records(fill: usize) -> Vec<Vec<u8>> {
    let t = |v: u32| v.to_le_bytes().to_vec();
    let mut r: Vec<Vec<u8>> = vec![
        trec(0x1605, &[t(0), cstr("")].concat(), true),
        trec(0x1605, &[t(0), cstr("C:\\src\\a.cpp")].concat(), true),
        trec(0x1604, &[t(2), t(0x1000), t(0x1001)].concat(), true),
        trec(0x1605, &[t(0x1002), cstr("-c -Zi")].concat(), true),
        trec(0x1603, &[le16(2), t(0x1001), t(0x1003)].concat(), true),
        // functions: with the 8 bytes some compilers write after the name, and without
        trec(0x1601, &[t(0), t(0x1002), cstr("f"), vec![1, 2, 3, 4, 5, 6, 7, 8]].concat(), true),
        trec(0x1601, &[t(0x1000), t(0x1003), cstr("g")].concat(), true),
        trec(0x1602, &[t(0x1004), t(0x1005), cstr("m"), vec![9; 8]].concat(), true),
        trec(0x1606, &[t(0x1010), t(0x1001), t(77)].concat(), true),
        trec(0x1607, &[t(0x1011), t(0x1001), t(78), le16(3)].concat(), false),
    ];
    for k in 0..fill {
        r.push(trec(
            0x1601,
            &[t(0), t(0x1010 + k as u32), cstr(&format!("fn_{k}")), (k as u64).to_le_bytes().to_vec()].concat(),
            true,
        ));
        r.push(trec(0x1607, &[t(0x1020 + k as u32), t(0x1001), t(k as u32), le16(1)].concat(), false));
    }
    r
}

/// Independent versions of the hashes, for the test to check the module's against.
fn test_crc(d: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg());
        }
    }
    c
}

fn test_string_hash(s: &[u8]) -> u32 {
    let mut r = 0u32;
    let mut at = 0;
    while at + 4 <= s.len() {
        r ^= u32::from_le_bytes(s[at..at + 4].try_into().unwrap());
        at += 4;
    }
    if s.len() - at >= 2 {
        r ^= u16::from_le_bytes(s[at..at + 2].try_into().unwrap()) as u32;
        at += 2;
    }
    if at < s.len() {
        r ^= s[at] as u32;
    }
    r |= 0x2020_2020;
    r ^= r >> 11;
    r ^ (r >> 16)
}

/// The hash value of record `r` (with its length) by the kind.
fn test_hash(r: &[u8]) -> u32 {
    let kind = u16::from_le_bytes([r[2], r[3]]);
    let name_at = |at: usize| -> (u16, Vec<u8>, Option<Vec<u8>>) {
        let prop = u16::from_le_bytes([r[6], r[7]]);
        let mut at = at;
        if kind != 0x1507 {
            at += 2 + match u16::from_le_bytes([r[at], r[at + 1]]) {
                0..=0x7fff => 0,
                0x8002 => 2,
                0x8004 => 4,
                _ => panic!("numeric"),
            };
        }
        let n = r[at..].iter().position(|&b| b == 0).unwrap();
        let name = r[at..at + n].to_vec();
        let unique = (prop & 0x200 != 0).then(|| {
            let u = &r[at + n + 1..];
            u[..u.iter().position(|&b| b == 0).unwrap()].to_vec()
        });
        (prop, name, unique)
    };
    let by_record = test_crc(r);
    match kind {
        0x1606 | 0x1607 => test_string_hash(&r[4..8]),
        0x1504..=0x1507 => {
            let (prop, name, unique) = name_at(match kind {
                0x1506 => 12,
                0x1507 => 16,
                _ => 20,
            });
            if prop & 0x80 != 0 || name.ends_with(b"<unnamed-tag>") {
                by_record
            } else {
                test_string_hash(if prop & 0x100 != 0 { unique.as_ref().unwrap_or(&name) } else { &name })
            }
        }
        _ => by_record,
    }
}

/// A TPI or IPI stream of `records` and its hash stream, the header written as Microsoft's link
/// (`lld` false) or lld does, the index of the rule `rule` (0: the first record after each 8 KiB,
/// 1: the record that crosses it).
fn type_streams(records: &[Vec<u8>], hash_stream: u16, rule: u8, lld: bool) -> (Vec<u8>, Vec<u8>) {
    let body: Vec<u8> = records.concat();
    let mut hv = Vec::new();
    let mut idx = Vec::new();
    let (mut off, mut next) = (0usize, 0usize);
    for (k, r) in records.iter().enumerate() {
        hv.extend_from_slice(&(test_hash(r) % 262143).to_le_bytes());
        let end = off + r.len();
        let has = match rule {
            0 => off >= next,
            _ => k == 0 || end / 8192 > off / 8192,
        };
        if has {
            idx.extend_from_slice(&(0x1000 + k as u32).to_le_bytes());
            idx.extend_from_slice(&(off as u32).to_le_bytes());
            next = (off / 8192 + 1) * 8192;
        }
        off = end;
    }
    let mut h = Vec::new();
    for v in [
        20040203u32,
        56,
        0x1000,
        0x1000 + records.len() as u32,
        body.len() as u32,
        hash_stream as u32 | 0xffff << 16,
        4,
        262143,
        0,
        hv.len() as u32,
        hv.len() as u32,
        idx.len() as u32,
        (hv.len() + if lld { 0 } else { idx.len() }) as u32,
        0,
    ] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    (h.into_iter().chain(body).collect(), hv.into_iter().chain(idx).collect())
}

/// The streams of a file with these type streams: 0 and 1 filled, 2 the TPI, 3 a DBI of no
/// modules, 4 the IPI, 5 and 6 their hash streams.
fn type_pdb(rule: u8, lld: bool, shuffle: bool, edit: impl Fn(&mut Vec<u8>, &mut Vec<u8>)) -> (Vec<u8>, Vec<u8>) {
    let (tpi, mut tpi_hash) = type_streams(&type_records(300), 5, rule, lld);
    let (ipi, mut ipi_hash) = type_streams(&id_records(700), 6, rule, lld);
    edit(&mut tpi_hash, &mut ipi_hash);
    let streams = vec![
        Some(vec![0; 60]),
        Some(b"pdb info".to_vec()),
        Some(tpi),
        Some(dbi(&[])),
        Some(ipi),
        Some(tpi_hash),
        Some(ipi_hash),
    ];
    (msf_file(512, &streams, shuffle, 0x5a), streams.into_iter().flatten().collect::<Vec<_>>().concat())
}

#[test]
fn types_round_trip() {
    use aqr_pdb::types;
    for (rule, lld) in [(0, false), (1, false), (0, true), (1, true)] {
        let (d, _) = type_pdb(rule, lld, false, |_, _| {});
        let (inputs, spans) = msf::encode_streams(&d).expect("taken");
        let e = types::encode(&inputs[0], &spans).expect("types taken");
        // both streams, with their hash streams derived
        let config = types::Config::get(&e.params).unwrap();
        assert_eq!(config.modes, [(types::FULL, rule | (lld as u8) << 1); 2], "{rule} {lld}");
        assert_eq!(e.places.len(), 4);
        // some records kept as they are
        assert!(e.streams[0].iter().filter(|&&c| c == 255).count() >= 3);
        // the hashes by the kinds are derived, but the names the test made anonymous
        let ins: Vec<&[u8]> = e.streams.iter().map(|v| &v[..]).collect();
        let out = aqr_pdb::module::types(&e.params, &ins, e.sizes.iter().sum()).unwrap();
        let want: Vec<u8> = e.places.iter().flat_map(|p| inputs[0][p.bytes.clone()].iter().copied()).collect();
        let mut got = Vec::new();
        for p in &e.places {
            let start: u64 = e.sizes[..p.outputs[0]].iter().sum();
            let size: u64 = p.outputs.iter().map(|&o| e.sizes[o]).sum();
            got.extend_from_slice(&out[start as usize..(start + size) as usize]);
        }
        assert_eq!(got, want);
        // the lengths are not read as the sizes: another length fails
        assert!(aqr_pdb::module::types(&e.params, &ins, e.sizes.iter().sum::<u64>() + 1).is_err());
    }
}

#[test]
fn types_hash_exceptions() {
    use aqr_pdb::types;
    // a hash that is not the derived one is kept as it is; an index that is not any rule's makes
    // the stream's hashes as they are
    let (d, _) = type_pdb(0, false, false, |tpi, _| tpi[4 * 7] ^= 1);
    let (inputs, spans) = msf::encode_streams(&d).unwrap();
    let e = types::encode(&inputs[0], &spans).expect("taken");
    assert_eq!(types::Config::get(&e.params).unwrap().modes[0].0, types::FULL);
    let hdr = e.streams[8].len();
    let (d2, _) = type_pdb(0, false, false, |_, _| {});
    let (inputs2, spans2) = msf::encode_streams(&d2).unwrap();
    let e2 = types::encode(&inputs2[0], &spans2).unwrap();
    assert!(hdr > e2.streams[8].len(), "an exception is in hdr");
    let (d3, _) = type_pdb(0, false, false, |tpi, _| {
        let n = tpi.len();
        tpi[n - 1] ^= 1;
    });
    let (inputs3, spans3) = msf::encode_streams(&d3).unwrap();
    let e3 = types::encode(&inputs3[0], &spans3).expect("taken");
    let c = types::Config::get(&e3.params).unwrap();
    assert_eq!(c.modes[0].0, types::RECORDS);
    assert_eq!(c.modes[1].0, types::FULL);
    assert_eq!(e3.places.len(), 3);
}

#[test]
fn types_corrupt() {
    use aqr_pdb::types;
    let (d, _) = type_pdb(1, true, false, |_, _| {});
    let (inputs, spans) = msf::encode_streams(&d).unwrap();
    let e = types::encode(&inputs[0], &spans).unwrap();
    let len: u64 = e.sizes.iter().sum();
    for k in 0..e.streams.len() {
        if e.streams[k].is_empty() {
            continue;
        }
        let mut s = e.streams.clone();
        s[k].pop();
        let ins: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
        assert!(aqr_pdb::module::types(&e.params, &ins, len).is_err(), "stream {k} cut");
    }
    for seed in 0..600usize {
        let mut s = e.streams.clone();
        let k = seed % s.len();
        if !s[k].is_empty() {
            let i = seed * 7919 % s[k].len();
            s[k][i] ^= 1 << (seed % 8);
            let ins: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
            let _ = aqr_pdb::module::types(&e.params, &ins, len);
        }
    }
}

#[test]
fn formula_types() {
    for (rule, lld, shuffle) in [(0, false, false), (1, true, true)] {
        let (d, _) = type_pdb(rule, lld, shuffle, |_, _| {});
        let f = aqr_pdb::formula::encode(&d, 1).expect("taken");
        let modules: Vec<_> = f.applies().iter().map(|a| a.module.name).collect();
        assert_eq!(modules, ["pdb-msf", "pdb-types"]);
        assert_eq!(f.eval(&Native).unwrap(), d);
    }
}
