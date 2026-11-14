//! The Standard security handler read by the encoder (ISO 32000-1 §7.6.3 and ISO 32000-2
//! §7.6.4), for files whose user password is empty: the file encryption key from the
//! `/Encrypt` dictionary and the file's `/ID`, checked against the dictionary's `/U` entry (so
//! that a wrong key is never used), and the streams decrypted. Public-key security, other
//! handlers and non-empty user passwords are not read: those files keep their streams as they
//! are.
//!
//! Strings are not decrypted: they stay in the skeleton.

use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::crypt::{AES128, AES256, Aes, Crypt, RC4, md5, rc4};
use crate::lex::{Obj, Parser, string_value};
use crate::objects::Objects;

/// The padding string that completes a password to 32 bytes (algorithm 2).
const PAD: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01, 0x08, 0x2e, 0x2e, 0x00,
    0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53, 0x69, 0x7a,
];

/// What the encoder knows of a file's encryption.
#[derive(Debug, Clone)]
pub struct Standard {
    pub crypt: Crypt,
    /// Whether the document's metadata stream is encrypted too (`/EncryptMetadata`).
    pub metadata: bool,
}

/// The encryption of `file` with the empty user password, if it has any that is understood and
/// whose streams are encrypted by a cipher.
pub fn detect(file: &[u8], objs: &mut Objects) -> Option<Standard> {
    let mut dicts: Vec<Obj> = vec![];
    for i in memchr::memmem::rfind_iter(file, b"/Encrypt") {
        let d = match Parser::new(file, i + 8).object() {
            Some(Obj::Ref(n, _)) => objs.get(n),
            Some(d @ Obj::Dict(_)) => Some(d),
            _ => None,
        };
        if let Some(d) = d
            && !dicts.contains(&d)
        {
            dicts.push(d);
        }
    }
    if dicts.is_empty() {
        return None;
    }
    // (the last /ID first; a file with an update has several, normally the same first element)
    let mut ids: Vec<Vec<u8>> = vec![];
    for i in memchr::memmem::rfind_iter(file, b"/ID") {
        if let Some(Obj::Array(a)) = Parser::new(file, i + 3).object()
            && let Some(Obj::Str(s)) = a.first()
        {
            let id = string_value(s);
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids.push(vec![]);
    dicts.iter().find_map(|d| ids.iter().find_map(|id| derive(d, id)))
}

/// The key of the file with the encryption dictionary `d` and `id`, if the empty user password
/// opens it.
fn derive(d: &Obj, id: &[u8]) -> Option<Standard> {
    if d.get(b"Filter")?.as_name()? != b"Standard" {
        return None;
    }
    let int = |k: &[u8]| d.get(k).and_then(Obj::as_int);
    let string = |k: &[u8]| match d.get(k)? {
        Obj::Str(s) => Some(string_value(s)),
        _ => None,
    };
    let (v, r) = (int(b"V").unwrap_or(0), int(b"R")?);
    let metadata = !matches!(d.get(b"EncryptMetadata"), Some(Obj::Bool(false)));
    // The method of streams: V 1 and 2 are RC4; V 4 and 5 name a crypt filter (/StmF).
    let bits = int(b"Length").unwrap_or(40);
    let (method, klen) = match v {
        1 => (RC4, 5),
        2 | 3 => (RC4, usize::try_from(bits / 8).ok()?),
        4 | 5 => {
            let stm = d.get(b"StmF").and_then(Obj::as_name).unwrap_or(b"Identity");
            if stm == b"Identity" {
                return None;
            }
            let cf = d.get(b"CF")?.get(stm)?;
            match cf.get(b"CFM")?.as_name()? {
                b"V2" if v == 4 => (RC4, if d.get(b"Length").is_some() { usize::try_from(bits / 8).ok()? } else { 16 }),
                b"AESV2" if v == 4 => (AES128, 16),
                b"AESV3" if v == 5 => (AES256, 32),
                _ => return None,
            }
        }
        _ => return None,
    };
    let (o, u) = (string(b"O")?, string(b"U")?);
    let key = match r {
        2..=4 => {
            let klen = if r == 2 { 5 } else { klen };
            if !(5..=16).contains(&klen) {
                return None;
            }
            let p = int(b"P")? as u32;
            standard_key(&o, &u, p, id, r, klen, metadata)?
        }
        5 | 6 => {
            if method != AES256 {
                return None;
            }
            long_key(&u, string(b"UE")?.as_slice(), r)?
        }
        _ => return None,
    };
    Some(Standard { crypt: Crypt::new(method, &key)?, metadata })
}

/// Algorithm 2 with the empty password, then 4 or 5 (the check against `/U`).
fn standard_key(o: &[u8], u: &[u8], p: u32, id: &[u8], r: i64, n: usize, metadata: bool) -> Option<Vec<u8>> {
    let mut b = PAD.to_vec();
    b.extend_from_slice(o.get(..32)?);
    b.extend_from_slice(&p.to_le_bytes());
    b.extend_from_slice(id);
    if r >= 4 && !metadata {
        b.extend_from_slice(&[0xff; 4]);
    }
    let mut h = md5(&b);
    if r >= 3 {
        for _ in 0..50 {
            h = md5(&h[..n]);
        }
    }
    let key = h[..n].to_vec();
    if r == 2 {
        let mut x = PAD;
        rc4(&key, &mut x);
        return (u.get(..32)? == x).then_some(key);
    }
    let mut b = PAD.to_vec();
    b.extend_from_slice(id);
    let mut x = md5(&b);
    for i in 0..20u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        rc4(&k, &mut x);
    }
    (u.get(..16)? == x).then_some(key)
}

/// The file key of revision 5 or 6 (ISO 32000-2 algorithm 2.A, 2.B) with the empty password,
/// checked against `/U`.
fn long_key(u: &[u8], ue: &[u8], r: i64) -> Option<Vec<u8>> {
    let (hash, vsalt, ksalt) = (u.get(..32)?, u.get(32..40)?, u.get(40..48)?);
    let h = |salt: &[u8]| if r == 5 { Sha256::digest(salt).to_vec() } else { hash_2b(salt) };
    if h(vsalt) != hash {
        return None;
    }
    let mut key = ue.get(..32)?.to_vec();
    // AES-256 in CBC mode without padding, with a zero IV
    cbc_decrypt(&aes::Aes256::new_from_slice(&h(ksalt)).ok()?, [0; 16], &mut key);
    Some(key)
}

/// Decrypts the whole blocks of `d` in CBC mode, `iv` first.
fn cbc_decrypt(aes: &impl BlockDecrypt, mut prev: [u8; 16], d: &mut [u8]) {
    for c in d.as_chunks_mut::<16>().0 {
        let ct = *c;
        aes.decrypt_block(GenericArray::from_mut_slice(c));
        c.iter_mut().zip(&prev).for_each(|(x, p)| *x ^= p);
        prev = ct;
    }
}

/// Algorithm 2.B of the empty password: a hash of `salt` by rounds of AES-128 and the SHA-2
/// family.
fn hash_2b(salt: &[u8]) -> Vec<u8> {
    let mut k = Sha256::digest(salt).to_vec();
    let mut round = 0usize;
    loop {
        let k1 = k.repeat(64);
        let aes = Aes::new(&k[..16]);
        let mut e = k1;
        let mut prev: [u8; 16] = k[16..32].try_into().unwrap();
        for c in e.as_chunks_mut::<16>().0 {
            c.iter_mut().zip(&prev).for_each(|(x, p)| *x ^= p);
            aes.encrypt(c);
            prev = *c;
        }
        k = match e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && *e.last().unwrap() as usize <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

impl Standard {
    /// The plain bytes of the stream `data` of object `num` `gen_`, if they encrypt back to it
    /// exactly: for AES, the IV first and whole blocks of CBC with canonical padding.
    pub fn open(&self, num: u32, gen_: u16, data: &[u8]) -> Option<Vec<u8>> {
        let (k, n) = self.crypt.object_key(num, gen_);
        if self.crypt.method() == RC4 {
            let mut d = data.to_vec();
            rc4(&k[..n], &mut d);
            return Some(d);
        }
        if data.len() < 32 || !data.len().is_multiple_of(16) {
            return None;
        }
        let (iv, body) = data.split_at(16);
        let mut d = body.to_vec();
        let prev: [u8; 16] = iv.try_into().ok()?;
        match n {
            16 => cbc_decrypt(&aes::Aes128::new_from_slice(&k[..16]).unwrap(), prev, &mut d),
            _ => cbc_decrypt(&aes::Aes256::new_from_slice(&k[..32]).unwrap(), prev, &mut d),
        }
        let pad = *d.last()? as usize;
        if !(1..=16).contains(&pad) || d[d.len() - pad..].iter().any(|&b| b as usize != pad) {
            return None;
        }
        d.truncate(d.len() - pad);
        Some(d)
    }

    /// Whether `data` (of an AES stream) ends in canonical padding, without decrypting the rest.
    pub fn plausible(&self, num: u32, gen_: u16, data: &[u8]) -> bool {
        if self.crypt.method() == RC4 {
            return true;
        }
        if data.len() < 32 || !data.len().is_multiple_of(16) {
            return false;
        }
        self.open(num, gen_, &data[data.len() - 32..]).is_some()
    }
}
