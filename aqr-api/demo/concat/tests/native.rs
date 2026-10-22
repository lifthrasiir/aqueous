use aqr_api::emu::{Failure, apply};
use aqr_demo_concat::concat::ENTRY;

#[test]
fn concat() {
    let out = apply(&ENTRY, &[b"in0", b"input1"], &[b"res"], b"|", 13).unwrap();
    assert_eq!(out, b"resin0|input1");
}

#[test]
fn large() {
    let in0: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
    let out = apply(&ENTRY, &[&in0, b""], &[b""], b"", 100_000).unwrap();
    assert_eq!(out, in0);
}

#[test]
fn wrong_size() {
    let Err(Failure { what, .. }) = apply(&ENTRY, &[b"in0", b"input1"], &[b"res"], b"|", 12) else { panic!() };
    assert!(what.starts_with("trap: assertion"), "{what}");
}

#[test]
fn wrong_arity() {
    assert!(apply(&ENTRY, &[b"in0"], &[b"res"], b"|", 7).is_err());
}
