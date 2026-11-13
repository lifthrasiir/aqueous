//! Media made here from setups of aqr-vorbis's database: their stripped setups read and made
//! again, and their formulas (the setup found by the table of ids or by a skeleton) giving them
//! back through aqr-vorbis's module.

use aqr_formula::Apply;
use aqr_vorbis::setup::{Ident, Setup};
use aqr_vorbis::setups;
use aqr_wwise::{formula, stripped, wem};

/// Writes a packet of random values of `setup`, as the walk asks for them (floors mostly used).
struct Random<'a> {
    setup: &'a Setup,
    rng: aqr_common::rng::Rng,
    w: aqr_vorbis::bits::Writer,
}

impl aqr_vorbis::packet::Source for Random<'_> {
    fn bits(&mut self, f: aqr_vorbis::packet::Field, n: u32) -> Option<u32> {
        use aqr_vorbis::packet::Field;
        let v = match f {
            Field::Type | Field::Window => panic!("not in stripped packets"),
            Field::Mode => self.rng.below(self.setup.modes.len() as u64) as u32,
            Field::Nonzero { .. } => (self.rng.below(4) > 0) as u32,
            _ => self.rng.below(1 << n) as u32,
        };
        self.w.bits(v, n);
        Some(v)
    }

    fn entry(&mut self, _: aqr_vorbis::packet::Field, book: usize) -> Option<u32> {
        let b = &self.setup.codebooks[book];
        let used: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        let e = used[self.rng.below(used.len() as u64) as usize];
        b.write(e, &mut self.w);
        Some(e)
    }
}

/// The stripped setup header of database skeleton `k` with ends `ends`, its codebooks given ids
/// `ids`, of a stream of `channels`; and the setup header it stands for.
fn stripped_setup(k: usize, ends: &[u32], ids: &[u16], channels: usize) -> (Vec<u8>, Vec<u8>) {
    let db = setups::database().unwrap();
    let setup = db.assemble(k, ends).unwrap();
    let books: usize = db.skeletons()[k].codebooks().iter().map(|&b| db.codebook(b as usize).1).sum();
    let bit = |i: usize| setup[i / 8] >> (i % 8) & 1;
    // (after the packet type, `vorbis` and the codebook count)
    let rest: Vec<u8> = (8 * 8 + books..setup.len() * 8).map(bit).collect();
    (stripped::strip(ids, &rest, channels).unwrap(), setup)
}

/// A medium (`fmt ` of 0x42 bytes) of stripped setup `setup` and `packets` random stripped
/// packets of Vorbis setup `full`, the last with `extra` zero bytes after its values; cut `cut`
/// bytes short.
fn medium(setup: &[u8], full: &[u8], ident: Ident, packets: u64, extra: usize, cut: usize) -> Vec<u8> {
    let s = Setup::parse(full, ident).unwrap();
    let seek = [0x5a; 8];
    let mut data = seek.to_vec();
    data.extend_from_slice(&(setup.len() as u16).to_le_bytes());
    data.extend_from_slice(setup);
    let audio = data.len();
    for seed in 0..packets {
        let mut r = Random { setup: &s, rng: aqr_common::rng::Rng::new(seed), w: aqr_vorbis::bits::Writer::new() };
        aqr_vorbis::packet::walk(&s, &mut r).unwrap();
        let mut p = r.w.finish();
        if seed + 1 == packets {
            p.resize(p.len() + extra, 0);
        }
        data.extend_from_slice(&(p.len() as u16).to_le_bytes());
        data.extend_from_slice(&p);
    }
    data.truncate(data.len() - cut);
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0xFFFFu16.to_le_bytes());
    fmt.extend_from_slice(&(ident.channels as u16).to_le_bytes());
    fmt.extend_from_slice(&48000u32.to_le_bytes());
    fmt.extend_from_slice(&[0; 8]);
    fmt.extend_from_slice(&0x30u16.to_le_bytes());
    fmt.extend_from_slice(&[0; 6]);
    // the vorb fields: samples, mod_signal, then the setup's and first audio packet's offsets
    fmt.extend_from_slice(&(packets as u32 * 1024).to_le_bytes());
    fmt.extend_from_slice(&0xE0u32.to_le_bytes());
    fmt.extend_from_slice(&[0; 8]);
    fmt.extend_from_slice(&(seek.len() as u32).to_le_bytes());
    fmt.extend_from_slice(&(audio as u32).to_le_bytes());
    fmt.extend_from_slice(&[0; 12]);
    fmt.extend_from_slice(&0x1234_5678u32.to_le_bytes());
    fmt.push(ident.blocksizes[0].trailing_zeros() as u8);
    fmt.push(ident.blocksizes[1].trailing_zeros() as u8);
    assert_eq!(fmt.len(), 0x42);
    let mut d = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    d.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    d.extend_from_slice(&fmt);
    d.extend_from_slice(b"data");
    d.extend_from_slice(&(data.len() as u32).to_le_bytes());
    d.extend_from_slice(&data);
    let size = (d.len() - 8) as u32;
    d[4..8].copy_from_slice(&size.to_le_bytes());
    d
}

/// `d` through its formula and aqr-vorbis's module.
fn round_trip(d: &[u8]) -> Vec<u8> {
    let f = formula::encode(d, 1).expect("a formula");
    assert_eq!(f.applies().len(), 1);
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, aqr_vorbis::formula::MODULE);
        aqr_vorbis::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
    .unwrap()
}

/// Rows of the database for media: (skeleton, ends, channels), a mono and a stereo one (of
/// coupling, which a mono stream's setup does not parse).
fn cases() -> Vec<(usize, Vec<u32>, usize)> {
    let db = setups::database().unwrap();
    let parses = |r: &setups::Row, channels| {
        Setup::parse(&db.assemble_row(r).unwrap(), Ident { channels, blocksizes: [256, 2048], stripped: true }).is_ok()
    };
    let stereo = db.rows().iter().find(|r| !parses(r, 1) && parses(r, 2)).unwrap();
    let mono = db.rows().iter().find(|r| parses(r, 1)).unwrap();
    vec![(mono.skeleton, mono.ends.clone(), 1), (stereo.skeleton, stereo.ends.clone(), 2)]
}

#[test]
fn stripped_setups_read_back() {
    let db = setups::database().unwrap();
    for (k, ends, channels) in cases() {
        let n = db.skeletons()[k].codebooks().len();
        let ids: Vec<u16> = (0..n as u16).map(|i| 1000 - i).collect();
        let (s, full) = stripped_setup(k, &ends, &ids, channels);
        let p = stripped::parse(&s, channels).unwrap();
        assert_eq!(p.ids, ids);
        assert_eq!(p.ends, ends);
        assert_eq!(stripped::strip(&p.ids, &p.rest, channels).unwrap(), s);
        // the setup it stands for, from the skeleton's codebooks
        let books: Vec<_> = db.skeletons()[k].codebooks().iter().map(|&b| db.codebook(b as usize)).collect();
        assert_eq!(p.assemble(&books), full);
    }
    assert!(stripped::parse(&[3], 1).is_none());
}

#[test]
fn media_through_their_formula() {
    let db = setups::database().unwrap();
    let mut by_table = 0;
    for (k, ends, channels) in cases() {
        let ident = Ident { channels, blocksizes: [256, 2048], stripped: true };
        let parts = db.skeletons()[k].codebooks();
        // ids the table does not know, the setup found by its skeleton; and the table's ids,
        // where it has them all
        let unknown: Vec<u16> = (0..parts.len() as u16).map(|i| 1000 - i).collect();
        let table: Option<Vec<u16>> = parts.iter().map(|&p| aqr_wwise::ids::id(p)).collect();
        by_table += table.is_some() as usize;
        for ids in [Some(unknown), table].into_iter().flatten() {
            let (s, full) = stripped_setup(k, &ends, &ids, channels);
            for (packets, extra, cut) in [(30, 0, 0), (30, 3, 0), (20, 0, 5), (1, 0, 0)] {
                let d = medium(&s, &full, ident, packets, extra, cut);
                let w = wem::parse(&d).unwrap();
                assert_eq!(w.cut, cut > 0);
                assert_eq!(w.packets.len() as u64, packets - (cut > 0) as u64);
                assert_eq!(wem::extent(&d), Some(d.len()));
                if packets == 1 && ids[0] == 1000 {
                    // no packet to find the skeleton by
                    assert!(formula::encode(&d, 1).is_none());
                    continue;
                }
                assert_eq!(round_trip(&d), d);
            }
        }
    }
    assert!(by_table > 0, "the table knows no case's codebooks");
    assert!(formula::encode(b"RIFF", 1).is_none());
}
