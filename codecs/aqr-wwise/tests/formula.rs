//! Media made here from setups of aqr-vorbis's database: their stripped setups read and made
//! again, and their formulas (the setup found by the table of ids or by a skeleton) giving them
//! back through aqr-vorbis's module.

use aqr_archive::Evaluate;
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
            Field::Type => 0,
            Field::Window => self.rng.below(2) as u32,
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
    medium_of(setup, full, ident, packets, extra, cut, 0xE0)
}

/// [`medium`] with the vorb field `mod_signal` (0x4A, 0x4B, 0x69 and 0x70: packets whole).
fn medium_of(setup: &[u8], full: &[u8], ident: Ident, packets: u64, extra: usize, cut: usize, signal: u32) -> Vec<u8> {
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
    fmt.extend_from_slice(&signal.to_le_bytes());
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

/// `d` through its formula (whatever its size: random packets code to more than their bytes)
/// and aqr-vorbis's module.
fn round_trip(d: &[u8]) -> Vec<u8> {
    let f = formula::formula_of(d).expect("a formula");
    // (`encode` gives it only where its streams but the setup are fewer bytes than `d`)
    let own: usize = f.streams.iter().filter(|s| s.class != "vorbis/setup").map(|s| s.data.len()).sum();
    assert_eq!(formula::encode(d, 1).is_some(), own < d.len());
    assert!(formula::is(&f));
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
                    assert!(formula::formula_of(&d).is_none());
                    continue;
                }
                assert_eq!(round_trip(&d), d);
            }
        }
    }
    assert!(by_table > 0, "the table knows no case's codebooks");
    assert!(formula::encode(b"RIFF", 1).is_none());
}

/// `d` through its formula and back, if it has one (the module's failing, a wrong result).
fn back(d: &[u8]) -> Option<Vec<u8>> {
    formula::formula_of(d)?
        .eval(&|a: &Apply, inputs: &[&[u8]]| {
            aqr_vorbis::formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
        })
        .ok()
}

/// A stream's medium of the first case (a mono one) with `packets` packets and the vorb field
/// `signal`, and what it needs to be made again.
fn mono_medium(packets: u64, signal: u32) -> Vec<u8> {
    let (k, ends, channels) = cases().remove(0);
    let n = setups::database().unwrap().skeletons()[k].codebooks().len();
    let ids: Vec<u16> = (0..n as u16).map(|i| 1000 - i).collect();
    let (s, full) = stripped_setup(k, &ends, &ids, channels);
    let ident = Ident { channels, blocksizes: [256, 2048], stripped: signal == 0xE0 };
    medium_of(&s, &full, ident, packets, 0, 0, signal)
}

/// Packets that carry their type and window flags (a `mod_signal` of 0x4A, 0x4B, 0x69, 0x70)
/// are read, and coded as Vorbis's own.
#[test]
fn whole_packets_through_their_formula() {
    for signal in [0x4A, 0x4B, 0x69, 0x70] {
        let d = mono_medium(30, signal);
        assert!(!wem::parse(&d).unwrap().mod_packets);
        assert_eq!(round_trip(&d), d, "{signal:#x}");
    }
    assert!(wem::parse(&mono_medium(3, 0xE0)).unwrap().mod_packets);
}

/// The medium with the bytes of chunk `tag` replaced (a chunk is its tag, its size and its bytes).
fn with_chunk(d: &[u8], tag: &[u8; 4], f: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let at = d.windows(4).position(|w| w == tag).unwrap();
    let size = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
    let mut body = d[at + 8..at + 8 + size].to_vec();
    f(&mut body);
    let mut e = d[..at + 4].to_vec();
    e.extend_from_slice(&(body.len() as u32).to_le_bytes());
    e.extend_from_slice(&body);
    e.extend_from_slice(&d[at + 8 + size..]);
    let n = (e.len() - 8) as u32;
    e[4..8].copy_from_slice(&n.to_le_bytes());
    e
}

#[test]
fn chunks_and_fields_out_of_place() {
    let d = mono_medium(20, 0xE0);
    let ok = round_trip(&d);
    assert_eq!(ok, d);
    // chunks the medium does not use, before and after its data, of odd sizes (RIFF pads them)
    let data_at = d.windows(4).position(|w| w == b"data").unwrap();
    for junk in [&b"JUNK   abcd"[..], &b"cue     "[..], &b"LIST   abc "[..]] {
        let mut e = d[..data_at].to_vec();
        e.extend_from_slice(junk);
        e.extend_from_slice(&d[data_at..]);
        e.extend_from_slice(junk);
        let n = (e.len() - 8) as u32;
        e[4..8].copy_from_slice(&n.to_le_bytes());
        // (the offsets of the vorb field count from the data's start, so these still hold)
        assert_eq!(back(&e).as_deref(), Some(&e[..]), "{junk:?}");
        assert!(wem::parse(&e).is_ok(), "{junk:?}");
    }
    // a vorb field of another size
    for len in [0usize, 0x28, 0x2C, 0x32, 0x34, 0x2B] {
        let mut e = d.clone();
        // (the medium has no vorb chunk: its fmt of 0x42 bytes holds it)
        e = with_chunk(&e, b"fmt ", |b| {
            b.truncate(0x18);
            b.extend(std::iter::repeat_n(0, len));
        });
        assert!(wem::parse(&e).is_err() || len == 0x2A, "{len}");
    }
    // offsets of the setup and of the audio past the data
    for (field, v) in [(0x10usize, u32::MAX), (0x10, 1 << 20), (0x14, u32::MAX), (0x14, 1 << 20)] {
        let e = with_chunk(&d, b"fmt ", |b| b[0x18 + field..0x18 + field + 4].copy_from_slice(&v.to_le_bytes()));
        assert!(wem::parse(&e).is_err(), "{field:#x} {v:#x}");
        assert!(formula::formula_of(&e).is_none());
    }
    // sizes past the file's end: the last chunk takes what there is
    let mut e = d.clone();
    let at = e.windows(4).position(|w| w == b"data").unwrap();
    e[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let w = wem::parse(&e).unwrap();
    assert!(w.cut || !w.packets.is_empty());
    assert_eq!(wem::extent(&e), Some(e.len().min(8 + u32::from_le_bytes(e[4..8].try_into().unwrap()) as usize)));
    // no channels, many
    for ch in [0u16, 255, 256, 0xFFFF] {
        let e = with_chunk(&d, b"fmt ", |b| b[2..4].copy_from_slice(&ch.to_le_bytes()));
        assert!(back(&e).is_none_or(|o| o == e), "{ch}");
    }
}

/// Damaged media: no panic, no hang, no allocation the data cannot hold; and a formula made
/// gives the medium back.
#[test]
fn damaged_media() {
    #[global_allocator]
    static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);
    for signal in [0xE0, 0x4A] {
        let d = mono_medium(6, signal);
        aqr_testkit::damaged(&d, 40, 0x80, std::time::Duration::from_secs(60), |v| {
            let _ = wem::extent(v);
            assert!(back(v).is_none_or(|o| o == v));
        });
    }
}
