//! `git-pack-recons` and `git-pack-index-recons` as part modules ([`layout::build_stream`] and
//! [`module::index_stream`]: the inputs pulled in pieces, the output put in pieces) against the
//! whole decoders and git's own bytes, past the pieces and the windows, and on damaged, cut and
//! extended inputs.

use aqr_api::stream::{Pull, Put, Slice};
use aqr_common::rng::Rng;
use aqr_formula::{Formula, Node};
use aqr_git::index::{self, IDX_MAGIC, REV_MAGIC};
use aqr_git::layout::{self, Entries};
use aqr_git::module::{self, WITH_REV};
use aqr_git::pack::{self, BLOB, ID, OFS_DELTA, REF_DELTA};

/// A [`Pull`] giving at most `k` bytes at a time.
struct Few<'a>(&'a [u8], usize);

impl Pull for Few<'_> {
    fn pull(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.len().min(buf.len()).min(self.1);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        n
    }
}

/// A [`Put`] with room for `.1` bytes, taking them `.2` at a time at most.
struct Capped(Vec<u8>, usize);

impl Put for Capped {
    fn put(&mut self, d: &[u8]) -> Option<()> {
        (self.0.len() + d.len() <= self.1).then(|| self.0.extend_from_slice(d))
    }
}

fn decode(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &aqr_formula::Apply, ins: &[&[u8]]| {
        aqr_git::formula::apply(a, ins, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    })
}

/// The inputs of the application `a` of the module in `f`.
fn inputs_of(f: &Formula, a: &aqr_formula::Apply) -> Vec<Vec<u8>> {
    let mut g = f.clone();
    g.root = Node::Concat(a.inputs[..2].to_vec());
    g.shared.truncate(1);
    let flate = decode(&g).unwrap();
    let streams = g.shared[0].sizes[0] as usize;
    let mut inputs = vec![flate[..streams].to_vec(), flate[streams..].to_vec()];
    g.shared.clear();
    for n in &a.inputs[2..] {
        g.root = n.clone();
        inputs.push(decode(&g).unwrap());
    }
    inputs
}

/// Text of `n` bytes, or noise where `noisy`.
fn data(rng: &mut Rng, n: usize, noisy: bool) -> Vec<u8> {
    if noisy {
        return rng.bytes(n);
    }
    let words: [&[u8]; 5] = [b"pack ", b"entry ", b"tree ", b"blob ", b"delta "];
    let mut t = Vec::new();
    while t.len() < n {
        t.extend_from_slice(words[rng.below(5) as usize]);
        let k = rng.below(3) as usize;
        t.extend(rng.bytes(k));
    }
    t.truncate(n);
    t
}

/// A pack of `sizes.len()` entries of data of these sizes (the types, the zlib settings and
/// the headers varying with the entry: deltas of both kinds, a header written long, a wrong
/// Adler-32), the entries from `noisy` on of noise; the trailer right or not, `rest` after it.
fn make(sizes: &[usize], noisy: usize, trailer: bool, rest: &[u8], seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut pack = b"PACK\0\0\0\x02".to_vec();
    pack.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    let mut starts = Vec::new();
    for (i, &n) in sizes.iter().enumerate() {
        let d = data(&mut rng, n, i >= noisy);
        let t = match i % 5 {
            3 if !starts.is_empty() => OFS_DELTA,
            4 => REF_DELTA,
            _ => BLOB,
        };
        let at = pack.len();
        starts.push(at);
        if i % 11 == 7 {
            // a header written longer than needed
            let (h, k) = pack::header(t, n as u64);
            let mut h = h[..k].to_vec();
            *h.last_mut().unwrap() |= 0x80;
            h.push(0);
            pack.extend_from_slice(&h);
        } else {
            pack::put_header(t, n as u64, &mut pack);
        }
        match t {
            OFS_DELTA => {
                // the distance back to an earlier entry, as git writes it
                let mut dist = (at - starts[rng.below(starts.len() as u64 - 1) as usize]) as u64;
                let mut b = vec![(dist & 0x7f) as u8];
                while dist >= 0x80 {
                    dist = (dist >> 7) - 1;
                    b.push(0x80 | (dist & 0x7f) as u8);
                }
                b.reverse();
                pack.extend_from_slice(&b);
            }
            REF_DELTA => pack.extend(rng.bytes(ID)),
            _ => {}
        }
        let (level, header) = [(1, [0x78, 0x01]), (6, [0x78, 0x9c]), (9, [0x78, 0xda])][(i / 3) % 3];
        pack.extend_from_slice(&header);
        pack.extend(aqr_flate::zlib::compress(&d, aqr_flate::zlib::Params { level, ..Default::default() }).unwrap());
        let mut adler = aqr_common::adler32::hash(&d);
        if i % 13 == 12 {
            adler ^= 1;
        }
        pack.extend_from_slice(&adler.to_be_bytes());
    }
    let sum = pack::sha1(&pack);
    match trailer {
        true => pack.extend_from_slice(&sum),
        false => pack.extend_from_slice(&[7; ID]),
    }
    pack.extend_from_slice(rest);
    pack
}

/// The index and reverse index per `gitformat-pack`, written plainly.
fn reference(pack: &[u8], starts: &[u64], ids: &[u8], rev: bool) -> Vec<u8> {
    let n = starts.len() - 1;
    let trailer = &pack[starts[n] as usize..][..ID];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| ids[a * ID..][..ID].cmp(&ids[b * ID..][..ID]).then(a.cmp(&b)));
    let mut idx = IDX_MAGIC.to_vec();
    for b in 0..256usize {
        let c = (0..n).filter(|&j| ids[j * ID] as usize <= b).count() as u32;
        idx.extend_from_slice(&c.to_be_bytes());
    }
    for &j in &order {
        idx.extend_from_slice(&ids[j * ID..][..ID]);
    }
    for &j in &order {
        let e = &pack[starts[j] as usize..starts[j + 1] as usize];
        idx.extend_from_slice(&aqr_common::crc32::hash(e).to_be_bytes());
    }
    let mut big = Vec::new();
    for &j in &order {
        match starts[j] {
            s if s > 0x7fff_ffff => {
                idx.extend_from_slice(&(0x8000_0000 | (big.len() as u32 / 8)).to_be_bytes());
                big.extend_from_slice(&s.to_be_bytes());
            }
            s => idx.extend_from_slice(&(s as u32).to_be_bytes()),
        }
    }
    idx.extend_from_slice(&big);
    idx.extend_from_slice(trailer);
    let sum = pack::sha1(&idx);
    idx.extend_from_slice(&sum);
    if rev {
        let mut r = REV_MAGIC.to_vec();
        for j in 0..n {
            r.extend_from_slice(&(order.iter().position(|&x| x == j).unwrap() as u32).to_be_bytes());
        }
        r.extend_from_slice(trailer);
        let sum = pack::sha1(&r);
        r.extend_from_slice(&sum);
        idx.extend_from_slice(&r);
    }
    idx
}

fn lens4(i: &[Vec<u8>]) -> [u64; 4] {
    [0, 1, 2, 3].map(|k| i[k].len() as u64)
}

/// The pack through the part code, `k` bytes at a time, into room for `room` bytes.
fn build(i: &[Vec<u8>], len: u64, k: usize, room: usize) -> Option<Vec<u8>> {
    let mut out = Capped(Vec::new(), room);
    let mut s: Vec<Few> = i.iter().map(|d| Few(d, k)).collect();
    let [a, b, c, d] = &mut s[..] else { unreachable!() };
    layout::build_stream([a, b, c, d], lens4(i), len, &mut out, None, u64::MAX).ok()?;
    Some(out.0)
}

#[test]
fn packs_through_pieces() {
    let cases: [(&str, Vec<usize>, usize, bool, &[u8]); 7] = [
        ("one", vec![100], 9, true, b""),
        ("small", (0..40).map(|i| 30 + 17 * i).collect(), 20, true, b""),
        ("noise", (0..30).map(|i| 5000 + 3001 * (i % 7)).collect(), 0, true, b"after the pack"),
        // entries past the pieces of 64 KiB, and several of them in a stream
        ("big", vec![200_000, 10, 1_000_000, 70_000, 65_535, 65_536, 65_537], 1, true, b""),
        ("many", (0..4000).map(|i| 20 + i % 90).collect(), 9999, true, b""),
        ("trailer not derived", (0..30).map(|i| 100 + 31 * i).collect(), 5, false, b"x"),
        ("empty rest", vec![0, 1, 2], 3, true, b""),
    ];
    for (name, sizes, noisy, trailer, rest) in cases {
        let pack = make(&sizes, noisy, trailer, rest, 1);
        let f = aqr_git::formula::encode(&pack, 1).unwrap_or_else(|| panic!("{name}: taken"));
        let Node::Apply(a) = &f.root else { panic!("{name}: an application") };
        let ins = inputs_of(&f, a);
        let len = pack.len() as u64;
        let refs: Vec<&[u8]> = ins.iter().map(|d| &d[..]).collect();
        assert!(module::decode(&[], &refs, len, 1).unwrap() == pack, "{name}: whole");
        for k in [1, 7, 4093, 1 << 20] {
            let got = build(&ins, len, k, pack.len()).unwrap_or_else(|| panic!("{name}: {k} at a time"));
            assert!(got == pack, "{name}: {k} at a time");
        }
        // room for one byte less, or more than the pack: the length is the declared one
        assert!(build(&ins, len, 4093, pack.len() - 1).is_none(), "{name}: less room");
        assert!(build(&ins, len + 1, 4093, pack.len() + 1).is_none(), "{name}: declared longer");
        assert!(build(&ins, len - 1, 4093, pack.len()).is_none(), "{name}: declared shorter");
        // inputs cut short or extended
        for k in 0..4 {
            let mut cut = ins.clone();
            if cut[k].pop().is_some() {
                assert!(build(&cut, len, 4093, pack.len()).is_none(), "{name}: input {k} cut");
            }
            let mut more = ins.clone();
            more[k].push(0);
            assert!(build(&more, len, 4093, pack.len()).is_none(), "{name}: input {k} extended");
        }
        // inputs damaged: another pack of the length, or none
        let mut rng = Rng::new(5);
        for k in 0..4 {
            for _ in 0..40.min(ins[k].len()) {
                let mut bad = ins.clone();
                let at = rng.below(bad[k].len() as u64) as usize;
                bad[k][at] ^= 1 << rng.below(8);
                let refs: Vec<&[u8]> = bad.iter().map(|d| &d[..]).collect();
                let whole = module::decode(&[], &refs, len, 1).ok();
                assert!(build(&bad, len, 4093, pack.len()) == whole, "{name}: input {k} damaged at {at}");
            }
        }
        assert!(module::decode(&[1], &refs, len, 1).is_err(), "{name}: params");
        assert!(module::decode(&[], &refs[..3], len, 1).is_err(), "{name}: inputs");
    }
}

#[test]
fn layout_checks() {
    let pack = make(&(0..50).map(|i| 50 + 13 * i).collect::<Vec<_>>(), 10, true, b"rest", 2);
    let f = aqr_git::formula::encode(&pack, 1).unwrap();
    let Node::Apply(a) = &f.root else { panic!() };
    let ins = inputs_of(&f, a);
    assert!(layout::check(&ins[2], &ins[1], &ins[3], &pack));
    let mut bad = pack.clone();
    bad[40] ^= 1;
    assert!(!layout::check(&ins[2], &ins[1], &ins[3], &bad));
    assert!(!layout::check(&ins[2], &ins[1], &ins[3], &pack[..pack.len() - 1]));
    let tags = layout::tags(&ins[2]).unwrap();
    assert_eq!(tags.len(), 50);
    assert!(layout::trailer_derived(&ins[2]));
}

/// A pack with ids at random, its indexes as the module makes them.
fn with_index(sizes: &[usize], rev: bool, seed: u64) -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>, Vec<u8>) {
    let pack = make(sizes, 4, true, b"", seed);
    let f = aqr_git::formula::encode(&pack, 1).unwrap();
    let Node::Apply(a) = &f.root else { panic!() };
    let mut ins = inputs_of(&f, a);
    let mut starts: Vec<u64> = pack::entries(&pack).unwrap().iter().map(|e| e.header.start as u64).collect();
    starts.push(pack.len() as u64 - ID as u64);
    let mut ids = Rng::new(seed).bytes(sizes.len() * ID);
    // some of the same first bytes, and a few the same ids
    for j in (0..sizes.len()).step_by(5) {
        ids[j * ID] = 0x42;
    }
    if sizes.len() > 8 {
        let (a, b) = ids.split_at_mut(7 * ID);
        b[..ID].copy_from_slice(&a[2 * ID..3 * ID]);
    }
    let want = reference(&pack, &starts, &ids, rev);
    ins.push(ids.clone());
    let mut params = vec![if rev { WITH_REV } else { 0 }];
    aqr_common::varint::put(&mut params, pack.len() as u64);
    (pack, ins, params, want)
}

#[test]
fn indexes_through_pieces() {
    for (name, sizes) in [
        ("one", vec![10]),
        ("few", (0..12).map(|i| 40 + 9 * i).collect()),
        ("many", (0..3000).map(|i| 20 + i % 50).collect()),
        ("big", vec![300_000, 20, 200_000, 70_000, 100]),
    ] {
        for rev in [false, true] {
            let (pack, ins, params, idx) = with_index(&sizes, rev, 3);
            let want = [&pack[..], &idx].concat();
            let len = want.len() as u64;
            let refs: Vec<&[u8]> = ins.iter().map(|d| &d[..]).collect();
            assert!(module::decode_index(&params, &refs, len, 1).unwrap() == want, "{name} {rev}: whole");
            let lens = [0, 1, 2, 3, 4].map(|k| ins[k].len() as u64);
            for k in [1, 7, 4093, 1 << 20] {
                let mut s: Vec<Few> = ins.iter().map(|d| Few(d, k)).collect();
                let [a, b, c, d, e] = &mut s[..] else { unreachable!() };
                let mut out = Capped(Vec::new(), want.len());
                let n = module::index_stream([a, b, c, d, e], lens, pack.len() as u64, rev, &mut out)
                    .unwrap_or_else(|e| panic!("{name} {rev} {k} at a time: {e}"));
                assert!(out.0 == want && n == len, "{name} {rev}: {k} at a time");
            }
            // another length of the output, the declared one, params
            assert!(module::decode_index(&params, &refs, len + 1, 1).is_err(), "{name} {rev}: longer");
            assert!(module::decode_index(&params, &refs, len - 1, 1).is_err(), "{name} {rev}: shorter");
            let mut p = params.clone();
            p[0] ^= WITH_REV;
            assert!(module::decode_index(&p, &refs, len, 1).is_err(), "{name} {rev}: flags");
            // ids of another count, cut, extended
            for k in 0..5 {
                for extend in [false, true] {
                    let mut bad = ins.clone();
                    match extend {
                        true => bad[k].push(0),
                        false if bad[k].pop().is_none() => continue,
                        false => {}
                    }
                    let refs: Vec<&[u8]> = bad.iter().map(|d| &d[..]).collect();
                    assert!(module::decode_index(&params, &refs, len, 1).is_err(), "{name} {rev}: input {k} {extend}");
                }
            }
            // damaged: indexes or none, and of the same length when taken
            let mut rng = Rng::new(8);
            for k in 0..5 {
                for _ in 0..20.min(ins[k].len()) {
                    let mut bad = ins.clone();
                    let at = rng.below(bad[k].len() as u64) as usize;
                    bad[k][at] ^= 1 << rng.below(8);
                    let refs: Vec<&[u8]> = bad.iter().map(|d| &d[..]).collect();
                    if let Ok(out) = module::decode_index(&params, &refs, len, 1) {
                        assert_eq!(out.len() as u64, len);
                    }
                }
            }
        }
    }
}

/// Offsets past 2 GiB (64-bit ones in the index) and an index of entries all of one id, written
/// from the entries' offsets and CRC-32s, against the plain writer.
#[test]
fn large_offsets() {
    let n = 64;
    let mut rng = Rng::new(11);
    let ids = rng.bytes(n * ID);
    // offsets of both kinds, some at the limits
    let mut starts: Vec<u64> = (0..=n as u64).map(|j| 12 + j * 0x0300_0000 + rng.below(1000)).collect();
    starts[10] = 0x7fff_ffff;
    starts[11] = 0x8000_0000;
    starts.sort_unstable();
    let crcs: Vec<u32> = (0..n).map(|_| rng.u64() as u32).collect();
    let trailer: [u8; ID] = rng.bytes(ID).try_into().unwrap();
    // (a plain writer wants the pack's bytes: build the CRC-32s by hand)
    let entries = Entries { starts: starts.clone(), crcs: crcs.clone() };
    for rev in [false, true] {
        let mut out = Vec::new();
        let total = index::write(&mut Slice(&ids), &entries, &trailer, rev, &mut out).unwrap();
        assert_eq!(total, out.len() as u64);
        let large = starts[..n].iter().filter(|&&s| s > 0x7fff_ffff).count();
        assert!(large > 10);
        assert_eq!(out.len(), index::idx_len(n, large) + if rev { index::rev_len(n) } else { 0 });
        // against the plain writer: the same bytes once its CRC-32s are the given ones
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| ids[a * ID..][..ID].cmp(&ids[b * ID..][..ID]).then(a.cmp(&b)));
        let crc_at = 8 + 1024 + n * ID;
        for (k, &j) in order.iter().enumerate() {
            assert_eq!(out[crc_at + 4 * k..][..4], crcs[j].to_be_bytes());
        }
        let offs_at = crc_at + 4 * n;
        let mut b = 0u32;
        for (k, &j) in order.iter().enumerate() {
            let w = u32::from_be_bytes(out[offs_at + 4 * k..][..4].try_into().unwrap());
            if starts[j] > 0x7fff_ffff {
                assert_eq!(w, 0x8000_0000 | b);
                let at = offs_at + 4 * n + 8 * b as usize;
                assert_eq!(out[at..][..8], starts[j].to_be_bytes());
                b += 1;
            } else {
                assert_eq!(w as u64, starts[j]);
            }
        }
        let idx = index::idx_len(n, large);
        assert_eq!(out[idx - 2 * ID..idx - ID], trailer);
        assert_eq!(out[idx - ID..idx], pack::sha1(&out[..idx - ID]));
        if rev {
            let r = &out[idx..];
            assert_eq!(&r[..12], &REV_MAGIC);
            for (k, &j) in order.iter().enumerate() {
                assert_eq!(r[12 + 4 * j..][..4], (k as u32).to_be_bytes());
            }
        }
    }
}

/// The reverse index's places are those of `index::build` (the plain reference's) for ids with
/// long cycles of the permutation.
#[test]
fn reverse_places_in_place() {
    for n in [1usize, 2, 3, 50, 1000] {
        let mut rng = Rng::new(n as u64);
        let ids = rng.bytes(n * ID);
        let mut starts: Vec<u64> = (0..=n as u64).map(|j| 12 + 40 * j).collect();
        starts[n] = 12 + 40 * n as u64;
        let pack = vec![0u8; 12 + 40 * n + ID];
        let want = reference(&pack, &starts, &ids, true);
        let s: Vec<usize> = starts.iter().map(|&s| s as usize).collect();
        let mut got = vec![0; index::len(&s, true)];
        index::build(&pack, &s, &ids, true, &mut got).unwrap();
        assert!(got == want, "{n}");
    }
}
