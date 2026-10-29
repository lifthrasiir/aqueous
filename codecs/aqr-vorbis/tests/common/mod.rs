//! A setup made here (ordered and sparse codebook lengths, lookups of both types, floor classes
//! with and without subclasses, residues of the three types, coupling, two submaps), and packets
//! of random values of it.
#![allow(dead_code)]

use aqr_common::rng::Rng;
use aqr_vorbis::bits::Writer;
use aqr_vorbis::packet::{self, End, Field, Source};
use aqr_vorbis::setup::{Ident, Setup, lookup1_values};

pub const IDENT: Ident = Ident { channels: 2, blocksizes: [64, 256] };

/// A codebook: `lengths` (0 unused) stored sparse, or ordered where `ordered` (lengths then
/// nondecreasing and all used), and a lookup of `lookup` type (0 for none).
pub fn codebook(w: &mut Writer, dims: u32, lengths: &[u32], ordered: bool, lookup: u32) {
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

pub fn setup_bytes() -> Vec<u8> {
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
pub fn random_packet(setup: &Setup, seed: u64) -> (Vec<u8>, Vec<u32>) {
    let mut r = Random { setup, rng: Rng::new(seed), w: Writer::new(), values: Vec::new() };
    assert_eq!(packet::walk(setup, &mut r), Ok(End::Whole));
    while !r.w.pos().is_multiple_of(8) {
        let b = r.rng.below(2) as u32;
        r.w.bit(b);
    }
    (r.w.finish(), r.values)
}
