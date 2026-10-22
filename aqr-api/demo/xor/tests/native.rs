use aqr_api::emu::{Entry, Failure, Kind, apply};
use aqr_demo_xor::xor::ENTRY;

fn xor(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter().enumerate().map(|(i, b)| b ^ key[i % key.len()]).collect()
}

#[test]
fn small() {
    assert_eq!(apply(&ENTRY, &[b"hello"], &[], b"k1", 5).unwrap(), xor(b"hello", b"k1"));
}

#[test]
fn multi_page() {
    let data: Vec<u8> = (0..200_003u32).map(|i| (i * 7) as u8).collect();
    let key = b"some key";
    assert_eq!(apply(&ENTRY, &[&data], &[], key, data.len() as u64).unwrap(), xor(&data, key));
}

#[test]
fn as_whole() {
    // The same code as a whole module may not call the part host functions.
    let entry = Entry { kind: Kind::Whole, ..ENTRY };
    let Err(Failure { what, .. }) = apply(&entry, &[b"hello"], &[], b"k", 5) else { panic!() };
    assert!(what.contains("aqr_request"), "{what}");
}

#[test]
fn empty_key() {
    let failure = apply(&ENTRY, &[b"hello"], &[], b"", 5).unwrap_err();
    assert_eq!(failure.what, "unreachable");
    assert_eq!(failure.log.as_deref(), Some(&b"xor: key of 1 to 256 bytes expected"[..]));
}
