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
        } else {
            return Err(aqr_formula::Error::new(format!("no module {}", a.module)));
        };
        r.map_err(|e| aqr_formula::Error::new(e.to_string()))
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
