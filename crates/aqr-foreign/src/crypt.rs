//! The encryptions of archives: ZIP's traditional one (PKWARE's APPNOTE §6.1; read only),
//! WinZip's AES (AE-1 and AE-2: PBKDF2-HMAC-SHA1 of 1000 rounds, AES-CTR of a little-endian
//! counter from 1, HMAC-SHA1 cut to 10 bytes; written as AE-2 of AES-256) and 7-Zip's AES-256
//! (a key of iterated SHA-256 over the salt and the passphrase in UTF-16LE, CBC). Written from
//! the formats' published descriptions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// CRC-32's table, a byte at a time, as the traditional encryption steps its keys (on their
/// bare state, which `aqr_common::crc32` does not give).
const CRC: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn crc_byte(c: u32, b: u8) -> u32 {
    (c >> 8) ^ CRC[((c ^ b as u32) & 0xff) as usize]
}

/// ZIP's traditional encryption ("ZipCrypto"), its keys as of the bytes taken so far.
#[derive(Clone)]
pub struct ZipCrypto([u32; 3]);

impl ZipCrypto {
    pub fn new(passphrase: &[u8]) -> ZipCrypto {
        let mut z = ZipCrypto([0x1234_5678, 0x2345_6789, 0x3456_7890]);
        passphrase.iter().for_each(|&b| z.update(b));
        z
    }

    fn update(&mut self, b: u8) {
        let k = &mut self.0;
        k[0] = crc_byte(k[0], b);
        k[1] = k[1].wrapping_add(k[0] & 0xff).wrapping_mul(134_775_813).wrapping_add(1);
        k[2] = crc_byte(k[2], (k[1] >> 24) as u8);
    }

    fn key_byte(&self) -> u8 {
        let t = (self.0[2] | 2) & 0xffff;
        (t.wrapping_mul(t ^ 1) >> 8) as u8
    }

    pub fn decrypt(&mut self, d: &mut [u8]) {
        for b in d {
            *b ^= self.key_byte();
            self.update(*b);
        }
    }

    /// The data of `body` (its 12 bytes of header first) by `passphrase`, `None` where the
    /// header's last byte is not one of `checks` (a wrong passphrase).
    pub fn open(passphrase: &[u8], body: &[u8], checks: &[u8]) -> Option<Vec<u8>> {
        let mut z = ZipCrypto::new(passphrase);
        let mut head: [u8; 12] = *body.first_chunk()?;
        z.decrypt(&mut head);
        if !checks.contains(&head[11]) {
            return None;
        }
        let mut d = body[12..].to_vec();
        z.decrypt(&mut d);
        Some(d)
    }
}

/// Why encrypted data does not open: the passphrase (as the data tells it), or damage.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Wrong,
    Bad(String),
}

/// Bytes of WinZip's AES-256 salt (that of the encryption written), and those its encryption
/// adds to the data (the salt, the verifier and the authentication code).
pub const WINZIP_SALT: usize = 16;
pub const WINZIP_ADDED: usize = WINZIP_SALT + 2 + 10;

/// WinZip's keys of `passphrase` and `salt`, for AES keys of `key` bytes (16, 24 or 32): the
/// AES key, the HMAC key (as long), and 2 bytes of the passphrase's verifier.
fn winzip_keys(passphrase: &[u8], salt: &[u8], key: usize) -> Vec<u8> {
    let mut k = vec![0u8; 2 * key + 2];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, salt, 1000, &mut k);
    k
}

/// WinZip's counter: little-endian, from 1.
fn winzip_iv() -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[0] = 1;
    iv
}

/// `data` encrypted by WinZip's AES-256 with `passphrase` and `salt`: the salt, the
/// passphrase's verifier, the data encrypted and its authentication code.
pub fn winzip_aes_encrypt(passphrase: &[u8], salt: &[u8; WINZIP_SALT], data: &[u8]) -> Vec<u8> {
    let k = winzip_keys(passphrase, salt, 32);
    let mut out = Vec::with_capacity(data.len() + WINZIP_ADDED);
    out.extend_from_slice(salt);
    out.extend_from_slice(&k[64..]);
    out.extend_from_slice(data);
    let body = &mut out[WINZIP_SALT + 2..];
    ctr::Ctr128LE::<aes::Aes256>::new(k[..32].into(), &winzip_iv().into()).apply_keystream(body);
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&k[32..64]).expect("any key");
    mac.update(body);
    let code = mac.finalize().into_bytes();
    out.extend_from_slice(&code[..10]);
    out
}

/// The bytes of the AES key of WinZip's `strength` (1 to 3: 128 to 256 bits).
fn winzip_key(strength: u8) -> Option<usize> {
    (1..=3).contains(&strength).then_some(8 + 8 * strength as usize)
}

/// The data of `body` encrypted by WinZip's AES of `strength` (1 to 3: 128 to 256 bits) by
/// `passphrase`: its salt, the passphrase's verifier, the data and its authentication code.
pub fn winzip_aes_decrypt(passphrase: &[u8], strength: u8, body: &[u8]) -> Result<Vec<u8>, Fail> {
    let key = winzip_key(strength).ok_or_else(|| Fail::Bad(format!("AES of strength {strength}")))?;
    let salt = key / 2;
    if body.len() < salt + 2 + 10 {
        return Err(Fail::Bad("cut short".into()));
    }
    let k = winzip_keys(passphrase, &body[..salt], key);
    if k[2 * key..] != body[salt..salt + 2] {
        return Err(Fail::Wrong);
    }
    let (data, code) = body[salt + 2..].split_at(body.len() - salt - 2 - 10);
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&k[key..2 * key]).expect("any key");
    mac.update(data);
    if mac.finalize().into_bytes()[..10] != *code {
        // (2 bytes of verifier may match by chance: told by the code as well)
        return Err(Fail::Wrong);
    }
    let mut out = data.to_vec();
    let iv = winzip_iv().into();
    match key {
        16 => ctr::Ctr128LE::<aes::Aes128>::new(k[..16].into(), &iv).apply_keystream(&mut out),
        24 => ctr::Ctr128LE::<aes::Aes192>::new(k[..24].into(), &iv).apply_keystream(&mut out),
        _ => ctr::Ctr128LE::<aes::Aes256>::new(k[..32].into(), &iv).apply_keystream(&mut out),
    }
    Ok(out)
}

/// Whether `passphrase` matches the verifier of `body` encrypted by WinZip's AES of `strength`
/// (by 2 bytes: one in 65,536 wrong ones does too).
pub fn winzip_aes_verifies(passphrase: &[u8], strength: u8, body: &[u8]) -> bool {
    let Some(key) = winzip_key(strength) else {
        return false;
    };
    let salt = key / 2;
    body.len() >= salt + 2 && winzip_keys(passphrase, &body[..salt], key)[2 * key..] == body[salt..salt + 2]
}

/// The power of 7-Zip's rounds of SHA-256 for a key that is written (its default).
pub const SEVENZ_POWER: u8 = 19;

/// The power that stands for no rounds: the key is the salt and the passphrase themselves.
const SEVENZ_RAW: u8 = 0x3f;

/// The properties of 7-Zip's AES coder: the power of the rounds of its key, its salt and its
/// initialization vector (of up to 16 bytes, zeros after).
#[derive(Debug, PartialEq)]
pub struct SevenzProps {
    pub power: u8,
    pub salt: Vec<u8>,
    pub iv: [u8; 16],
}

impl SevenzProps {
    /// The properties of the coder's bytes `p`.
    pub fn parse(p: &[u8]) -> Result<SevenzProps, String> {
        let b0 = *p.first().ok_or("AES: no properties")?;
        let mut props = SevenzProps { power: b0 & 0x3f, salt: Vec::new(), iv: [0; 16] };
        // (b0: bit 7 a salt given, bit 6 an IV given; b1: their lengths less one, in its nibbles)
        if b0 & 0xc0 != 0 {
            let b1 = *p.get(1).ok_or("AES: bad properties")?;
            let salt_len = (b0 >> 7 & 1) as usize + (b1 >> 4) as usize;
            let iv_len = (b0 >> 6 & 1) as usize + (b1 & 15) as usize;
            let rest = p.get(2..2 + salt_len + iv_len).ok_or("AES: bad properties")?;
            props.salt = rest[..salt_len].to_vec();
            props.iv[..iv_len].copy_from_slice(&rest[salt_len..]);
        }
        if props.power > 24 && props.power != SEVENZ_RAW {
            return Err(format!("AES: 2^{} rounds of its key", props.power));
        }
        Ok(props)
    }

    /// The coder's bytes of the properties of initialization vector `iv` (16 bytes), no salt
    /// and [`SEVENZ_POWER`].
    pub fn written(iv: &[u8; 16]) -> Vec<u8> {
        let mut p = vec![SEVENZ_POWER | 0x40, 15];
        p.extend_from_slice(iv);
        p
    }
}

/// `passphrase` (UTF-8, as typed) in UTF-16LE, as 7-Zip takes it.
fn utf16le(passphrase: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(passphrase).encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// 7-Zip's AES-256 key of `passphrase` (UTF-8, taken in UTF-16LE), `salt` and 2^`power`
/// rounds (or none, [`SEVENZ_RAW`]).
pub fn sevenz_key(passphrase: &[u8], salt: &[u8], power: u8) -> [u8; 32] {
    let pass = utf16le(passphrase);
    let mut key = [0u8; 32];
    if power == SEVENZ_RAW {
        let raw: Vec<u8> = salt.iter().chain(&pass).copied().take(32).collect();
        key[..raw.len()].copy_from_slice(&raw);
        return key;
    }
    let mut h = Sha256::new();
    let mut round = Vec::with_capacity(salt.len() + pass.len() + 8);
    for i in 0..1u64 << power {
        round.clear();
        round.extend_from_slice(salt);
        round.extend_from_slice(&pass);
        round.extend_from_slice(&i.to_le_bytes());
        h.update(&round);
    }
    key.copy_from_slice(&h.finalize());
    key
}

/// 7-Zip's AES-256 in CBC, carried over the pieces of a stream: the whole blocks of each piece
/// encrypted, the rest kept for the next (the end padded with zeros).
pub struct SevenzEncryptor {
    enc: cbc::Encryptor<aes::Aes256>,
    carry: Vec<u8>,
}

impl SevenzEncryptor {
    pub fn new(key: &[u8; 32], iv: &[u8; 16]) -> SevenzEncryptor {
        SevenzEncryptor { enc: cbc::Encryptor::new(key.into(), iv.into()), carry: Vec::new() }
    }

    /// The encryption of the blocks `data` completes.
    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(data);
        let whole = self.carry.len() / 16 * 16;
        let mut out: Vec<u8> = self.carry.drain(..whole).collect();
        out.as_chunks_mut::<16>().0.iter_mut().for_each(|b| self.enc.encrypt_block_mut(b.into()));
        out
    }

    /// The last block, padded.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.carry.is_empty() {
            return Vec::new();
        }
        self.carry.resize(16, 0);
        let mut b = std::mem::take(&mut self.carry);
        self.enc.encrypt_block_mut(b.as_mut_slice().into());
        b
    }
}

/// `data` encrypted by 7-Zip's AES-256 of `key` and `iv`, padded with zeros to whole blocks.
pub fn sevenz_aes_encrypt(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut e = SevenzEncryptor::new(key, iv);
    let mut out = e.feed(data);
    out.extend(e.finish());
    out
}

/// The keys of 7-Zip's AES derived so far in this process (each takes 2^19 rounds of SHA-256
/// by default, and every folder of an archive the same), by a hash of what they are derived of
/// (the passphrase is not kept): each derived once, those wanted at once waiting for it.
static SEVENZ_KEYS: Mutex<Option<HashMap<[u8; 32], Derived>>> = Mutex::new(None);

/// A key of 7-Zip's AES, once derived.
type Derived = Arc<OnceLock<[u8; 32]>>;

/// [`sevenz_key`], derived once in this process ([`SEVENZ_KEYS`]).
fn sevenz_key_once(passphrase: &[u8], salt: &[u8], power: u8) -> [u8; 32] {
    let mut id = Sha256::new();
    for part in [passphrase, salt] {
        id.update((part.len() as u64).to_le_bytes());
        id.update(part);
    }
    id.update([power]);
    let id: [u8; 32] = id.finalize().into();
    let cell = {
        let mut keys = SEVENZ_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        keys.get_or_insert_default().entry(id).or_default().clone()
    };
    *cell.get_or_init(|| sevenz_key(passphrase, salt, power))
}

/// The data of `input` encrypted by 7-Zip's AES-256 of properties `props` by `passphrase` (of
/// any length of a multiple of 16: the padding past the coder's output is the caller's to cut).
pub fn sevenz_aes_decrypt(passphrase: &[u8], props: &[u8], input: &[u8]) -> Result<Vec<u8>, String> {
    let p = SevenzProps::parse(props)?;
    let key = sevenz_key_once(passphrase, &p.salt, p.power);
    if !input.len().is_multiple_of(16) {
        return Err("AES: not of whole blocks".into());
    }
    let mut out = input.to_vec();
    let mut d = cbc::Decryptor::<aes::Aes256>::new(&key.into(), &p.iv.into());
    for b in out.as_chunks_mut::<16>().0 {
        d.decrypt_block_mut(b.into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zipcrypto_round_trips_by_its_keys() {
        // (encrypting is the same steps, the keys updated by the plain byte)
        let (pass, plain) = (b"secret", b"0123456789ab and the data");
        let mut z = ZipCrypto::new(pass);
        let mut c = plain.to_vec();
        for b in &mut c {
            let p = *b;
            *b ^= z.key_byte();
            z.update(p);
        }
        assert_eq!(ZipCrypto::open(pass, &c, b"b").unwrap(), &plain[12..]);
        assert_eq!(ZipCrypto::open(b"secreT", &c, b"b").filter(|d| d == &plain[12..]), None);
    }

    #[test]
    fn winzip_aes_round_trips() {
        let data = b"the data of an entry, encrypted".repeat(5);
        let body = winzip_aes_encrypt(b"pass", &[7; WINZIP_SALT], &data);
        assert_eq!(body.len(), data.len() + WINZIP_ADDED);
        assert!(winzip_aes_verifies(b"pass", 3, &body));
        assert_eq!(winzip_aes_decrypt(b"pass", 3, &body), Ok(data));
        assert_eq!(winzip_aes_decrypt(b"Pass", 3, &body), Err(Fail::Wrong));
        let mut bad = body.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(winzip_aes_decrypt(b"pass", 3, &bad), Err(Fail::Wrong));
    }

    #[test]
    fn sevenz_aes_round_trips() {
        let iv = [3; 16];
        let props = SevenzProps::written(&iv);
        assert_eq!(SevenzProps::parse(&props), Ok(SevenzProps { power: SEVENZ_POWER, salt: Vec::new(), iv }));
        let data = b"the data of a folder";
        let c = sevenz_aes_encrypt(&sevenz_key("p\u{e4}ss".as_bytes(), &[], SEVENZ_POWER), &iv, data);
        assert_eq!(c.len(), 32);
        let d = sevenz_aes_decrypt("p\u{e4}ss".as_bytes(), &props, &c).unwrap();
        assert_eq!(&d[..data.len()], data);
        assert_ne!(&sevenz_aes_decrypt(b"pass", &props, &c).unwrap()[..data.len()], data);
        // (in pieces, the same)
        let mut e = SevenzEncryptor::new(&sevenz_key("p\u{e4}ss".as_bytes(), &[], SEVENZ_POWER), &iv);
        let mut pieces: Vec<u8> = data.chunks(7).flat_map(|p| e.feed(p)).collect();
        pieces.extend(e.finish());
        assert_eq!(pieces, c);
    }
}
