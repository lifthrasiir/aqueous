//! The encryptions of the archives written: WinZip's AES (AE-2, AES-256: PBKDF2-HMAC-SHA1 of
//! 1000 rounds, AES-CTR of a little-endian counter from 1, HMAC-SHA1 cut to 10 bytes) and
//! 7-Zip's AES-256 (a key of 2^19 rounds of SHA-256 over the passphrase in UTF-16LE, CBC).
//! Written from the formats' published descriptions, as aqr-unpack's decryption.

use aes::cipher::{BlockEncryptMut, KeyIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Bytes of WinZip's AES-256 salt, and those its encryption adds to the data (the salt, the
/// verifier and the authentication code).
pub const WINZIP_SALT: usize = 16;
pub const WINZIP_ADDED: usize = WINZIP_SALT + 2 + 10;

/// `data` encrypted by WinZip's AES-256 with `passphrase` and `salt`: the salt, the
/// passphrase's verifier, the data encrypted and its authentication code.
pub fn winzip_aes(passphrase: &[u8], salt: &[u8; WINZIP_SALT], data: &[u8]) -> Vec<u8> {
    let mut k = [0u8; 66];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, salt, 1000, &mut k);
    let mut out = Vec::with_capacity(data.len() + WINZIP_ADDED);
    out.extend_from_slice(salt);
    out.extend_from_slice(&k[64..]);
    out.extend_from_slice(data);
    let mut iv = [0u8; 16];
    iv[0] = 1;
    let body = &mut out[WINZIP_SALT + 2..];
    ctr::Ctr128LE::<aes::Aes256>::new(k[..32].into(), &iv.into()).apply_keystream(body);
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&k[32..64]).expect("any key");
    mac.update(body);
    let code = mac.finalize().into_bytes();
    out.extend_from_slice(&code[..10]);
    out
}

/// The power of 7-Zip's rounds of SHA-256 for a key (its default).
pub const SEVENZ_POWER: u8 = 19;

/// 7-Zip's AES-256 key of `passphrase` (UTF-8, taken in UTF-16LE) and `salt`.
pub fn sevenz_key(passphrase: &[u8], salt: &[u8]) -> [u8; 32] {
    let pass: Vec<u8> = String::from_utf8_lossy(passphrase).encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut h = Sha256::new();
    let mut round = Vec::with_capacity(salt.len() + pass.len() + 8);
    for i in 0..1u64 << SEVENZ_POWER {
        round.clear();
        round.extend_from_slice(salt);
        round.extend_from_slice(&pass);
        round.extend_from_slice(&i.to_le_bytes());
        h.update(&round);
    }
    h.finalize().into()
}

/// The properties of 7-Zip's AES coder of initialization vector `iv` (16 bytes) and no salt.
pub fn sevenz_props(iv: &[u8; 16]) -> Vec<u8> {
    // (b0: the power, bit 6 an IV given; b1: its length less one in the low nibble)
    let mut p = vec![SEVENZ_POWER | 0x40, 15];
    p.extend_from_slice(iv);
    p
}

/// `data` encrypted by 7-Zip's AES-256 of `key` and `iv`, padded with zeros to whole blocks.
pub fn sevenz_aes(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    out.resize(data.len().next_multiple_of(16), 0);
    let mut e = cbc::Encryptor::<aes::Aes256>::new(key.into(), iv.into());
    for b in out.as_chunks_mut::<16>().0 {
        e.encrypt_block_mut(b.into());
    }
    out
}
