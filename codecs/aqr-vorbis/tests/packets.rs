//! A setup made here (ordered and sparse codebook lengths, lookups of both types, floor classes
//! with and without subclasses, residues of the three types, coupling, two submaps) and packets
//! of random values: parsed to their values and written back bit for bit, whole, cut short, and
//! with padding of any bits.

use aqr_common::rng::Rng;
use aqr_vorbis::bits::Writer;
use aqr_vorbis::packet::{self, End, Field, Packet, Source};
use aqr_vorbis::setup::{Ident, Setup, lookup1_values};

const IDENT: Ident = Ident { channels: 2, blocksizes: [64, 256] };

/// A codebook: `lengths` (0 unused) stored sparse, or ordered where `ordered` (lengths then
/// nondecreasing and all used), and a lookup of `lookup` type (0 for none).
fn codebook(w: &mut Writer, dims: u32, lengths: &[u32], ordered: bool, lookup: u32) {
    w.bits(0x564342, 24);
    w.bits(dims, 16);
    w.bits(lengths.len() as u32, 24);
    w.bits(ordered as u32, 1);
    if ordered {
        w.bits(lengths[0] - 1, 5);
        let (mut at, mut len) = (0, lengths[0]);
        while at < lengths.len() {
            let n = lengths[at..].iter().take_while(|&&l| l == len).count();
            w.bits(n as u32, aqr_vorbis::bits::ilog((lengths.len() - at) as u32));
            at += n;
            len += 1;
        }
    } else {
        w.bits(1, 1);
        for &l in lengths {
            w.bits((l > 0) as u32, 1);
            if l > 0 {
                w.bits(l - 1, 5);
            }
        }
    }
    w.bits(lookup, 4);
    if lookup > 0 {
        w.bits(0x6080_0000, 32);
        w.bits(0x6040_0000, 32);
        w.bits(1, 4);
        w.bits(0, 1);
        let n = if lookup == 1 { lookup1_values(lengths.len(), dims as usize) } else { lengths.len() * dims as usize };
        for i in 0..n {
            w.bits(i as u32 & 3, 2);
        }
    }
}

fn residue(w: &mut Writer, kind: u32, end: u32, partition: u32) {
    w.bits(kind, 16);
    w.bits(0, 24);
    w.bits(end, 24);
    w.bits(partition - 1, 24);
    w.bits(1, 6); // 2 classifications
    w.bits(1, 8); // classbook 1
    // class 0: pass 0 (book 2); class 1: passes 0 and 2 (books 3 and 2)
    for cascade in [0b001u32, 0b101] {
        w.bits(cascade & 7, 3);
        w.bits(0, 1);
    }
    w.bits(2, 8);
    w.bits(3, 8);
    w.bits(2, 8);
}

fn setup_bytes() -> Vec<u8> {
    let mut w = Writer::new();
    w.bits(5, 8);
    b"vorbis".iter().for_each(|&c| w.bits(c as u32, 8));
    w.bits(3, 8); // 4 codebooks
    codebook(&mut w, 1, &[3; 8], false, 0);
    codebook(&mut w, 2, &[2; 4], false, 0);
    codebook(&mut w, 2, &[2, 0, 3, 3, 3, 0, 4, 4, 3], false, 1);
    codebook(&mut w, 4, &[4; 16], true, 2);
    w.bits(0, 6);
    w.bits(0, 16);
    // a floor: partition classes 0 and 1; class 0 of 2 values from book 0, class 1 of 3 values,
    // a master book (1) choosing between no book and book 0
    w.bits(0, 6);
    w.bits(1, 16);
    w.bits(2, 5);
    w.bits(0, 4);
    w.bits(1, 4);
    w.bits(1, 3);
    w.bits(0, 2);
    w.bits(1, 8);
    w.bits(2, 3);
    w.bits(1, 2);
    w.bits(1, 8);
    w.bits(0, 8);
    w.bits(1, 8);
    w.bits(1, 2); // multiplier 2
    w.bits(7, 4);
    for x in [64, 32, 96, 16, 80] {
        w.bits(x, 7);
    }
    w.bits(2, 6); // 3 residues
    residue(&mut w, 2, 96, 8);
    residue(&mut w, 0, 64, 16);
    residue(&mut w, 1, 50, 8);
    w.bits(1, 6); // 2 mappings
    // mapping 0: one submap, channels 0 and 1 coupled, floor 0, residue 0 (type 2)
    w.bits(0, 16);
    w.bits(0, 1);
    w.bits(1, 1);
    w.bits(0, 8);
    w.bits(0, 1);
    w.bits(1, 1);
    w.bits(0, 2);
    w.bits(0, 8);
    w.bits(0, 8);
    w.bits(0, 8);
    // mapping 1: two submaps, a channel each, residues 1 (type 0) and 2 (type 1)
    w.bits(0, 16);
    w.bits(1, 1);
    w.bits(1, 4);
    w.bits(0, 1);
    w.bits(0, 2);
    w.bits(0, 4);
    w.bits(1, 4);
    for res in [1, 2] {
        w.bits(0, 8);
        w.bits(0, 8);
        w.bits(res, 8);
    }
    w.bits(1, 6); // 2 modes
    for (flag, mapping) in [(0, 0), (1, 1)] {
        w.bits(flag, 1);
        w.bits(0, 32);
        w.bits(mapping, 8);
    }
    w.bits(1, 1);
    w.finish()
}

/// Writes a packet of random values, as the walk asks for them.
struct Random<'a> {
    setup: &'a Setup,
    rng: Rng,
    w: Writer,
    values: Vec<u32>,
}

impl Source for Random<'_> {
    fn bits(&mut self, f: Field, n: u32) -> Option<u32> {
        let v = match f {
            Field::Type => 0,
            Field::Mode => self.rng.below(self.setup.modes.len() as u64) as u32,
            // mostly used floors
            Field::Nonzero { .. } => (self.rng.below(4) > 0) as u32,
            _ => self.rng.below(1 << n) as u32,
        };
        self.w.bits(v, n);
        self.values.push(v);
        Some(v)
    }

    fn entry(&mut self, _: Field, book: usize) -> Option<u32> {
        let b = &self.setup.codebooks[book];
        let used: Vec<u32> = (0..b.entries() as u32).filter(|&e| b.used(e)).collect();
        let e = used[self.rng.below(used.len() as u64) as usize];
        b.write(e, &mut self.w);
        self.values.push(e);
        Some(e)
    }
}

/// A random packet (padded with random bits), and its values.
fn random_packet(setup: &Setup, seed: u64) -> (Vec<u8>, Vec<u32>) {
    let mut r = Random { setup, rng: Rng::new(seed), w: Writer::new(), values: Vec::new() };
    assert_eq!(packet::walk(setup, &mut r), Ok(End::Whole));
    while !r.w.pos().is_multiple_of(8) {
        let b = r.rng.below(2) as u32;
        r.w.bit(b);
    }
    (r.w.finish(), r.values)
}

#[test]
fn setup_parses() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    assert_eq!(s.codebooks.len(), 4);
    assert_eq!(s.codebooks[3].lengths, [4; 16]);
    assert_eq!(s.codebooks[2].lengths, [2, 0, 3, 3, 3, 0, 4, 4, 3]);
    // codewords in entry order, each the lowest free one of its length (Vorbis I §3.2.1)
    assert_eq!(s.codebooks[2].codewords, [0b00, 0, 0b010, 0b011, 0b100, 0, 0b1010, 0b1011, 0b110]);
    assert_eq!(s.codebooks[2].lookup.as_ref().unwrap().multiplicands.len(), 3);
    assert_eq!(s.codebooks[3].lookup.as_ref().unwrap().multiplicands.len(), 64);
    assert_eq!(s.floors[0].xs, [0, 128, 64, 32, 96, 16, 80]);
    assert_eq!(s.residues.iter().map(|r| r.kind).collect::<Vec<_>>(), [2, 0, 1]);
    assert_eq!(s.mappings[0].coupling, [(0, 1)]);
    assert_eq!(s.mappings[1].mux, [0, 1]);
    assert_eq!(s.modes.iter().map(|m| m.blockflag).collect::<Vec<_>>(), [false, true]);
}

#[test]
fn setup_rejects_what_it_should() {
    let d = setup_bytes();
    // cut anywhere
    for n in 0..d.len() {
        assert!(Setup::parse(&d[..n], IDENT).is_err(), "cut at {n}");
    }
    // a bad sync, a codebook overspecified
    let mut e = d.clone();
    e[8] ^= 1;
    assert!(Setup::parse(&e, IDENT).is_err());
    let mut w = Writer::new();
    w.bits(5, 8);
    b"vorbis".iter().for_each(|&c| w.bits(c as u32, 8));
    w.bits(0, 8);
    codebook(&mut w, 1, &[1, 1, 1], false, 0);
    assert_eq!(Setup::parse(&w.finish(), IDENT).unwrap_err().0, "codebook overspecified");
}

#[test]
fn packets_round_trip() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    let mut short = 0;
    for seed in 0..2000 {
        let (d, values) = random_packet(&s, seed);
        let p = packet::parse(&s, &d).unwrap();
        assert_eq!(p.values, values);
        assert!(!p.short && p.tail.len() < 8);
        assert_eq!(packet::write(&s, &p).unwrap(), d);
        // cut short: what is there, written back
        let cut = &d[..(seed as usize % d.len().max(1))];
        let p = packet::parse(&s, cut).unwrap();
        short += p.short as usize;
        assert!(values.starts_with(&p.values));
        assert_eq!(packet::write(&s, &p).unwrap(), cut);
    }
    assert!(short > 1000, "{short}");
}

#[test]
fn values_not_of_a_packet_are_refused() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    let (d, _) = random_packet(&s, 7);
    let p = packet::parse(&s, &d).unwrap();
    let with = |f: &dyn Fn(&mut Packet)| {
        let mut q = p.clone();
        f(&mut q);
        packet::write(&s, &q)
    };
    // a value left over, a tail off a byte, an entry without a codeword, a field too wide
    assert!(with(&|q| q.values.push(0)).is_err());
    assert!(with(&|q| q.tail.push(0)).is_err());
    assert!(with(&|q| q.short = true).is_err());
    assert!(with(&|q| q.values[1] = 2).is_err());
    // not an audio packet
    assert!(packet::parse(&s, &[1]).is_err());
    // an empty packet is short at once
    assert_eq!(packet::parse(&s, &[]).unwrap(), Packet { values: vec![], tail: vec![], short: true });
}
