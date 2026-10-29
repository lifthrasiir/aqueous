//! Streams of packets (each after its 16-bit length) of the setup made here, through the record
//! and back: whole packets, packets cut short, padding of any bits, bytes past a packet's end;
//! where the stream ends; records that are not one.

mod common;

use aqr_common::rng::Rng;
use aqr_vorbis::codec;
use aqr_vorbis::setup::Setup;
use common::{IDENT, random_packet, setup_bytes};

/// A stream of `n` packets, some cut short and some with bytes after them.
fn stream(s: &Setup, n: u64, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut d = Vec::new();
    for i in 0..n {
        let (mut p, _) = random_packet(s, seed * 1000 + i);
        match rng.below(8) {
            0 => p.truncate(1 + rng.below(p.len() as u64) as usize),
            1 => p.extend((0..rng.below(3)).map(|_| rng.below(256) as u8)),
            _ => {}
        }
        d.extend_from_slice(&(p.len() as u16).to_le_bytes());
        d.extend_from_slice(&p);
    }
    d
}

#[test]
fn streams_round_trip() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    for seed in 0..20 {
        let d = stream(&s, 1 + seed * 7, seed);
        let (used, record) = codec::encode(&s, &d).unwrap();
        assert_eq!(used, d.len());
        assert_eq!(codec::decode(&s, &record, used).unwrap(), d);
    }
}

#[test]
fn stream_ends() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    let d = stream(&s, 30, 99);
    // at a length of 0, and what follows is not taken
    let mut e = d.clone();
    e.extend_from_slice(&[0, 0, 0xaa, 0xbb]);
    let (used, record) = codec::encode(&s, &e).unwrap();
    assert_eq!(used, d.len());
    assert_eq!(codec::decode(&s, &record, used).unwrap(), d);
    // a lone byte after the packets is not a length
    let mut e = d.clone();
    e.push(7);
    assert_eq!(codec::encode(&s, &e).unwrap().0, d.len());
    // no packets
    let (used, record) = codec::encode(&s, &[0, 0]).unwrap();
    assert_eq!(used, 0);
    assert_eq!(codec::decode(&s, &record, 0).unwrap(), b"");
    // a packet past the end, or not an audio packet
    assert!(codec::encode(&s, &d[..d.len() - 1]).is_none());
    assert!(codec::encode(&s, &[1, 0, 1]).is_none());
}

#[test]
fn bad_records_fail() {
    let s = Setup::parse(&setup_bytes(), IDENT).unwrap();
    let d = stream(&s, 20, 5);
    let (used, record) = codec::encode(&s, &d).unwrap();
    // another length
    assert!(codec::decode(&s, &record, used + 1).is_err());
    assert!(codec::decode(&s, &record, used - 1).is_err());
    // records cut or changed decode to something else or fail, without panicking
    let mut rng = Rng::new(1);
    for i in 0..300 {
        let mut r = record.clone();
        if i % 2 == 0 {
            r.truncate(rng.below(r.len() as u64) as usize);
        } else {
            let at = rng.below(r.len() as u64) as usize;
            r[at] ^= 1 << rng.below(8);
        }
        if let Ok(out) = codec::decode(&s, &r, used) {
            assert_eq!(out.len(), used);
        }
    }
}
