use sha2::{Digest, Sha512_256};

/// Each module built is of its hash, and named once.
#[test]
fn hashes() {
    for (i, k) in aqr_modules::KNOWN.iter().enumerate() {
        let wasm = k.read_wasm().unwrap();
        assert_eq!(<[u8; 32]>::from(Sha512_256::digest(&wasm)), k.sha512_256, "{}", k.name);
        assert!(wasm.starts_with(b"\0asm\x01\0\0\0"), "{}", k.name);
        assert!(aqr_modules::KNOWN[..i].iter().all(|o| o.name != k.name), "{}", k.name);
    }
}
