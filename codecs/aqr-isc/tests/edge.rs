//! Edge cases: chunks decoding to more than a chunk may, chunks of length 0, cabinets cut or
//! damaged anywhere.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_common::rng::Rng;
use aqr_decompress::Native;
use aqr_flate::zlib::{Params, compress};
use aqr_isc::formula::{self, CHUNK, Options};
use aqr_isc::isc;
use aqr_testkit::{Refusing, case, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

fn text(rng: &mut Rng, n: usize) -> Vec<u8> {
    let words: [&[u8]; 5] = [b"install ", b"shield ", b"cabinet ", b"file ", b"\r\n"];
    let mut d = Vec::new();
    while d.len() < n {
        d.extend_from_slice(words[rng.below(5) as usize]);
    }
    d.truncate(n);
    d
}

/// A cabinet of the chunks' data `chunks`, each the DEFLATE stream of zlib at level 6.
fn cabinet(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut d = isc::MAGIC.to_vec();
    d.extend([0xb0, 0x04, 0x00, 0x02]);
    d.extend([0; 4]);
    d.extend(0x200u32.to_le_bytes());
    d.extend([0; 4]);
    d.extend(0x200u32.to_le_bytes());
    d.resize(0x200, 0);
    for c in chunks {
        let z = compress(c, Params { level: 6, ..Params::default() }).unwrap();
        d.extend((z.len() as u16).to_le_bytes());
        d.extend(z);
    }
    d
}

fn check(d: &[u8]) {
    case("isc");
    if let Some(f) = formula::encode(d, 2) {
        assert!(f.eval(&Native::default()).unwrap() == d);
    }
    assert!(most() < 256 << 20);
}

/// A chunk decoding to more than a chunk may (64 KiB) is kept as it is, those around it
/// rebuilt; one of exactly the most is taken.
#[test]
fn chunk_past_max() {
    let mut rng = Rng::new(1);
    for (n, rebuilt) in [(65536, 3), (65537, 2), (1 << 20, 2)] {
        let d = cabinet(&[text(&mut rng, CHUNK), vec![0; n], text(&mut rng, 1000)]);
        let e = formula::encode_with(&d, &Options::default()).expect("the others");
        assert_eq!(e.rebuilt, rebuilt, "{n}");
        assert!(e.formula.eval(&Native::default()).unwrap() == d);
    }
    // alone: nothing to take
    let d = cabinet(&[vec![0; 1 << 20]]);
    assert!(formula::encode(&d, 1).is_none());
}

/// A chunk of length 0 ends the chunks read: what follows is kept as it is.
#[test]
fn chunk_of_length_0() {
    let mut rng = Rng::new(2);
    let mut d = cabinet(&[text(&mut rng, CHUNK), text(&mut rng, 5000)]);
    let cab = isc::parse(&d).unwrap();
    assert_eq!(cab.chunks.len(), 2);
    let end = cab.chunks[0].end;
    // (a length of 0 where the second begins: it and what follows are the tail)
    d[end..end + 2].copy_from_slice(&[0, 0]);
    assert_eq!(isc::parse(&d).unwrap().chunks.len(), 1);
    let e = formula::encode_with(&d, &Options::default()).expect("the first");
    assert_eq!(e.rebuilt, 1);
    assert!(e.formula.eval(&Native::default()).unwrap() == d);
    // a first chunk of length 0: no cabinet
    let mut z = cabinet(&[text(&mut rng, 3000)]);
    z[0x200..0x202].copy_from_slice(&[0, 0]);
    assert!(isc::parse(&z).is_none() && !isc::starts(&z));
    assert!(formula::encode(&z, 1).is_none());
}

/// A cabinet cut at every length from the header's end to its end: no panic, and a formula
/// gives the cut cabinet back.
#[test]
fn truncation_sweep() {
    let mut rng = Rng::new(3);
    let d = cabinet(&[text(&mut rng, CHUNK), text(&mut rng, 3000)]);
    let mut cuts: Vec<usize> = (0..80).collect();
    cuts.extend((0x1f0..0x240).chain(d.len() - 40..d.len()));
    cuts.extend((0..d.len()).step_by(997));
    for cut in cuts {
        check(&d[..cut]);
    }
}

#[test]
fn damaged_cabinets() {
    let mut rng = Rng::new(4);
    let d = cabinet(&[text(&mut rng, CHUNK), text(&mut rng, 300)]);
    // the header and the first chunk's start at every place, the rest by sampling
    damaged(&d, 60, 0x240, LIMIT, check);
}
