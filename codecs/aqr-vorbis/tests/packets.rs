//! A setup made here (ordered and sparse codebook lengths, lookups of both types, floor classes
//! with and without subclasses, residues of the three types, coupling, two submaps) and packets
//! of random values: parsed to their values and written back bit for bit, whole, cut short, and
//! with padding of any bits.

mod common;

use aqr_vorbis::bits::Writer;
use aqr_vorbis::packet::{self, Packet};
use aqr_vorbis::setup::Setup;
use common::{IDENT, codebook, random_packet, setup_bytes};

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
