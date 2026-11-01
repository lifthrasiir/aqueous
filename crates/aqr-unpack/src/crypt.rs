//! The encryptions of the archives read: ZIP's traditional one (PKWARE's APPNOTE §6.1), WinZip's
//! AES (AE-1 and AE-2: PBKDF2-HMAC-SHA1, AES-CTR of a little-endian counter, HMAC-SHA1) and
//! 7-Zip's AES-256 (a key of iterated SHA-256 over the salt and the passphrase in UTF-16LE,
//! CBC). Written from the formats' published descriptions.

use std::collections::HashMap;
use std::sync::Mutex;

use aes::cipher::{BlockDecryptMut, KeyIvInit, StreamCipher};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// CRC-32's table, a byte at a time (as the traditional encryption steps its keys).
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

    pub fn decrypt(&mut self, d: &mut [u8]) {
        for b in d {
            let t = (self.0[2] | 2) & 0xffff;
            *b ^= (t.wrapping_mul(t ^ 1) >> 8) as u8;
            self.update(*b);
        }
    }

    /// The data of `body` (its 12 bytes of header first) by `passphrase`, `None` where the
    /// header's last byte is not one of `checks` (a wrong passphrase).
    pub fn open(passphrase: &[u8], body: &[u8], checks: &[u8]) -> Option<Vec<u8>> {
        let mut z = ZipCrypto::new(passphrase);
        let mut head: [u8; 12] = body.get(..12)?.try_into().unwrap();
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

/// The data of `body` encrypted by WinZip's AES of `strength` (1 to 3: 128 to 256 bits) by
/// `passphrase`: its salt, the passphrase's verifier, the data and its authentication code.
pub fn winzip_aes(passphrase: &[u8], strength: u8, body: &[u8]) -> Result<Vec<u8>, Fail> {
    let key = match strength {
        1 => 16,
        2 => 24,
        3 => 32,
        s => return Err(Fail::Bad(format!("AES of strength {s}"))),
    };
    let salt = key / 2;
    if body.len() < salt + 2 + 10 {
        return Err(Fail::Bad("cut short".into()));
    }
    let mut k = vec![0u8; 2 * key + 2];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, &body[..salt], 1000, &mut k);
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
    let mut iv = [0u8; 16];
    iv[0] = 1;
    match key {
        16 => ctr::Ctr128LE::<aes::Aes128>::new(k[..16].into(), &iv.into()).apply_keystream(&mut out),
        24 => ctr::Ctr128LE::<aes::Aes192>::new(k[..24].into(), &iv.into()).apply_keystream(&mut out),
        _ => ctr::Ctr128LE::<aes::Aes256>::new(k[..32].into(), &iv.into()).apply_keystream(&mut out),
    }
    Ok(out)
}

/// Whether `passphrase` matches the verifier of `body` encrypted by WinZip's AES of `strength`
/// (by 2 bytes: one in 65,536 wrong ones does too).
pub fn winzip_aes_verifies(passphrase: &[u8], strength: u8, body: &[u8]) -> bool {
    let key = 8 + 8 * strength as usize;
    let salt = key / 2;
    if !(1..=3).contains(&strength) || body.len() < salt + 2 {
        return false;
    }
    let mut k = vec![0u8; 2 * key + 2];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, &body[..salt], 1000, &mut k);
    k[2 * key..] == body[salt..salt + 2]
}

/// The keys of 7-Zip's AES derived so far, by passphrase, salt and power of iterations: each
/// takes 2^19 rounds of SHA-256 by default, and every folder of an archive the same.
static KEYS_7Z: Mutex<Option<HashMap<KeyOf, [u8; 32]>>> = Mutex::new(None);

/// What a key of 7-Zip's AES is derived of: the passphrase (in UTF-16LE), the salt, the power.
type KeyOf = (Vec<u8>, Vec<u8>, u8);

/// The data of `input` encrypted by 7-Zip's AES-256 of properties `props` by `passphrase` (of
/// any length of a multiple of 16: the padding past the coder's output is the caller's to cut).
pub fn sevenz_aes(passphrase: &[u8], props: &[u8], input: &[u8]) -> Result<Vec<u8>, String> {
    let b0 = *props.first().ok_or("AES: no properties")?;
    let power = b0 & 0x3f;
    let (mut salt, mut iv) = (Vec::new(), [0u8; 16]);
    if b0 & 0xc0 != 0 {
        let b1 = *props.get(1).ok_or("AES: bad properties")?;
        let salt_len = (b0 >> 7 & 1) as usize + (b1 >> 4) as usize;
        let iv_len = (b0 >> 6 & 1) as usize + (b1 & 15) as usize;
        let rest = props.get(2..2 + salt_len + iv_len).ok_or("AES: bad properties")?;
        salt = rest[..salt_len].to_vec();
        iv[..iv_len].copy_from_slice(&rest[salt_len..]);
    }
    if power > 24 && power != 0x3f {
        return Err(format!("AES: 2^{power} rounds of its key"));
    }
    // the passphrase in UTF-16LE (of its UTF-8, as typed)
    let pass: Vec<u8> = String::from_utf8_lossy(passphrase).encode_utf16().flat_map(u16::to_le_bytes).collect();
    let id = (pass.clone(), salt.clone(), power);
    let known = KEYS_7Z.lock().unwrap().as_ref().and_then(|k| k.get(&id).copied());
    let key = match known {
        Some(k) => k,
        None => {
            let mut key = [0u8; 32];
            if power == 0x3f {
                let raw: Vec<u8> = salt.iter().chain(&pass).copied().take(32).collect();
                key[..raw.len()].copy_from_slice(&raw);
            } else {
                let mut h = Sha256::new();
                let mut round = Vec::with_capacity(salt.len() + pass.len() + 8);
                for i in 0..1u64 << power {
                    round.clear();
                    round.extend_from_slice(&salt);
                    round.extend_from_slice(&pass);
                    round.extend_from_slice(&i.to_le_bytes());
                    h.update(&round);
                }
                key.copy_from_slice(&h.finalize());
            }
            KEYS_7Z.lock().unwrap().get_or_insert_default().insert(id, key);
            key
        }
    };
    if !input.len().is_multiple_of(16) {
        return Err("AES: not of whole blocks".into());
    }
    let mut out = input.to_vec();
    let mut d = cbc::Decryptor::<aes::Aes256>::new(&key.into(), &iv.into());
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
            let t = (z.0[2] | 2) & 0xffff;
            let p = *b;
            *b ^= (t.wrapping_mul(t ^ 1) >> 8) as u8;
            z.update(p);
        }
        assert_eq!(ZipCrypto::open(pass, &c, b"b").unwrap(), &plain[12..]);
        assert_eq!(ZipCrypto::open(b"secreT", &c, b"b").filter(|d| d == &plain[12..]), None);
    }
}
