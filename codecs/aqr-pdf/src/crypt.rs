//! The ciphers of the Standard security handler (ISO 32000-1 §7.6, ISO 32000-2 §7.6): what the
//! module needs to encrypt a stream again, written from the specifications' descriptions of
//! MD5 (RFC 1321), RC4 and AES (FIPS 197) in CBC mode, with PKCS#5 padding done by the caller.
//! Nothing here decrypts: the encoder has the `aes` crate for that ([`crate::standard`]).
//!
//! A file's cipher is the module's parameters: one byte for the method, then the file key
//! (FORMAT.md, *Parameters*).

use crate::{Error, Result};

/// The methods: RC4 (V2 crypt filters, revisions 2 to 4), AES-128 in CBC mode with a random
/// IV (AESV2, revision 4) and AES-256 (AESV3, revisions 5 and 6).
pub const RC4: u8 = 1;
pub const AES128: u8 = 2;
pub const AES256: u8 = 3;

/// A file's cipher: a method and the file encryption key.
#[derive(Clone, PartialEq, Eq)]
pub struct Crypt {
    method: u8,
    key: [u8; 32],
    len: usize,
}

impl std::fmt::Debug for Crypt {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Crypt(method {}, {} byte key)", self.method, self.len)
    }
}

impl Crypt {
    /// The cipher of `method` with file key `key`: 5 to 16 bytes for RC4, 16 for AES-128, 32
    /// for AES-256.
    pub fn new(method: u8, key: &[u8]) -> Option<Crypt> {
        let ok = match method {
            RC4 => (5..=16).contains(&key.len()),
            AES128 => key.len() == 16,
            AES256 => key.len() == 32,
            _ => false,
        };
        let mut k = [0; 32];
        k[..key.len().min(32)].copy_from_slice(&key[..key.len().min(32)]);
        ok.then_some(Crypt { method, key: k, len: key.len() })
    }

    /// The cipher the parameters of the module tell; none for no parameters.
    pub fn from_params(params: &[u8]) -> Result<Option<Crypt>> {
        match params.split_first() {
            None => Ok(None),
            Some((&m, key)) => Crypt::new(m, key).map(Some).ok_or(Error::Corrupt),
        }
    }

    /// The parameters of the module that tell this cipher.
    pub fn to_params(&self) -> Vec<u8> {
        let mut p = vec![self.method];
        p.extend_from_slice(&self.key[..self.len]);
        p
    }

    pub fn method(&self) -> u8 {
        self.method
    }

    /// Bytes of the IV that starts an encrypted stream.
    pub fn iv_len(&self) -> usize {
        if self.method == RC4 { 0 } else { 16 }
    }

    /// The key of object `num` `gen` (ISO 32000-1, algorithm 1; the file key itself for AES-256).
    pub fn object_key(&self, num: u32, gen_: u16) -> ([u8; 32], usize) {
        if self.method == AES256 {
            return (self.key, 32);
        }
        let mut b = [0u8; 32 + 5 + 4];
        b[..self.len].copy_from_slice(&self.key[..self.len]);
        b[self.len..self.len + 3].copy_from_slice(&num.to_le_bytes()[..3]);
        b[self.len + 3..self.len + 5].copy_from_slice(&gen_.to_le_bytes());
        let mut n = self.len + 5;
        if self.method == AES128 {
            b[n..n + 4].copy_from_slice(b"sAlT");
            n += 4;
        }
        let h = md5(&b[..n]);
        let mut k = [0; 32];
        let m = (self.len + 5).min(16);
        k[..m].copy_from_slice(&h[..m]);
        (k, m)
    }

    /// Encrypts `body` of object `num` `gen_` in place: RC4 as it is, AES-CBC after `iv` for a
    /// body of whole blocks (padded by the caller).
    pub fn seal(&self, num: u32, gen_: u16, iv: &[u8], body: &mut [u8]) -> Result<()> {
        let (k, n) = self.object_key(num, gen_);
        if self.method == RC4 {
            rc4(&k[..n], body);
            return Ok(());
        }
        let iv: [u8; 16] = iv.try_into().map_err(|_| Error::Corrupt)?;
        if !body.len().is_multiple_of(16) {
            return Err(Error::Corrupt);
        }
        let aes = Aes::new(&k[..n]);
        let mut prev = iv;
        for b in body.as_chunks_mut::<16>().0 {
            b.iter_mut().zip(&prev).for_each(|(x, p)| *x ^= p);
            aes.encrypt(b);
            prev = *b;
        }
        Ok(())
    }
}

/// RC4 (a stream cipher: this also decrypts), in place.
pub fn rc4(key: &[u8], data: &mut [u8]) {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    for b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

/// MD5 of `data`.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8,
        0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340,
        0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87,
        0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039,
        0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92,
        0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    fn block(s: &mut [u32; 4], c: &[u8; 64]) {
        let m: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes(c[4 * i..4 * i + 4].try_into().unwrap()));
        let [mut a, mut b, mut cc, mut d] = *s;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & cc) | (!b & d), i),
                1 => ((d & b) | (!d & cc), (5 * i + 1) % 16),
                2 => (b ^ cc ^ d, (3 * i + 5) % 16),
                _ => (cc ^ (b | !d), (7 * i) % 16),
            };
            let t = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            (a, d, cc) = (d, cc, b);
            b = b.wrapping_add(t.rotate_left(S[i / 16 * 4 + i % 4]));
        }
        for (x, y) in s.iter_mut().zip([a, b, cc, d]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut s = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let (blocks, rest) = data.as_chunks::<64>();
    for c in blocks {
        block(&mut s, c);
    }
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let n = if rest.len() < 56 { 64 } else { 128 };
    tail[n - 8..n].copy_from_slice(&(data.len() as u64).wrapping_mul(8).to_le_bytes());
    for c in tail[..n].as_chunks::<64>().0 {
        block(&mut s, c);
    }
    let mut out = [0; 16];
    for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(s) {
        o.copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// AES's S-box: the multiplicative inverse in GF(2^8) followed by the affine map.
const SBOX: [u8; 256] = {
    let mut s = [0u8; 256];
    let (mut p, mut q) = (1u8, 1u8);
    loop {
        // p times 3, q divided by 3
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
};

/// An AES key (128 or 256 bits), expanded for encryption.
pub struct Aes {
    rk: [u8; 240],
    rounds: usize,
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

impl Aes {
    /// The key schedule of a key of 16, 24 or 32 bytes.
    pub fn new(key: &[u8]) -> Aes {
        let nk = key.len() / 4;
        let rounds = nk + 6;
        let mut rk = [0u8; 240];
        rk[..key.len()].copy_from_slice(key);
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t: [u8; 4] = rk[4 * (i - 1)..4 * i].try_into().unwrap();
            if i % nk == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| SBOX[b as usize]);
            }
            for k in 0..4 {
                rk[4 * i + k] = rk[4 * (i - nk) + k] ^ t[k];
            }
        }
        Aes { rk, rounds }
    }

    /// Encrypts a block.
    pub fn encrypt(&self, b: &mut [u8; 16]) {
        let add =
            |b: &mut [u8; 16], r: usize| b.iter_mut().zip(&self.rk[16 * r..16 * r + 16]).for_each(|(x, k)| *x ^= k);
        add(b, 0);
        for r in 1..=self.rounds {
            let mut t = [0u8; 16];
            // SubBytes and ShiftRows: row `i` of column `c` comes from column `c + i`.
            for c in 0..4 {
                for i in 0..4 {
                    t[4 * c + i] = SBOX[b[4 * ((c + i) % 4) + i] as usize];
                }
            }
            if r < self.rounds {
                for c in t.as_chunks_mut::<4>().0 {
                    let (a0, a1, a2, a3) = (c[0], c[1], c[2], c[3]);
                    let all = a0 ^ a1 ^ a2 ^ a3;
                    c[0] = a0 ^ all ^ xtime(a0 ^ a1);
                    c[1] = a1 ^ all ^ xtime(a1 ^ a2);
                    c[2] = a2 ^ all ^ xtime(a2 ^ a3);
                    c[3] = a3 ^ all ^ xtime(a3 ^ a0);
                }
            }
            *b = t;
            add(b, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn md5_vectors() {
        // RFC 1321, A.5 (and the sine table it is built from)
        let h = |s: &[u8]| md5(s).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(h(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(h(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            h(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        assert_eq!(h(&[b'a'; 56]), "3b0c8ac703f828b04c6c197006d17218");
        assert_eq!(h(&[b'a'; 64]), "014842d480b571495a4a0363793f7367");
    }

    #[test]
    fn md5_constants() {
        // (the table is what floor(2^32 abs(sin(i + 1))) gives)
        let src = include_str!("crypt.rs");
        let table = src.split("const K: [u32; 64] = [").nth(1).unwrap().split("];").next().unwrap();
        let k: Vec<u32> =
            table.split(',').filter_map(|t| u32::from_str_radix(t.trim().strip_prefix("0x")?, 16).ok()).collect();
        assert_eq!(k.len(), 64);
        for (i, &v) in k.iter().enumerate() {
            assert_eq!(v, ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32, "{i}");
        }
    }

    #[test]
    fn rc4_vector() {
        // RFC 6229, key 0102030405 (40 bits), offset 0
        let mut z = [0u8; 16];
        rc4(&[1, 2, 3, 4, 5], &mut z);
        assert_eq!(z.to_vec(), hex("b2396305f03dc027ccc3524a0a1118a8"));
    }

    #[test]
    fn aes_vectors() {
        // FIPS 197, appendix C
        let pt: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        for (key, ct) in [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "8ea2b7ca516745bfeafc49904b496089"),
        ] {
            let mut b = pt;
            Aes::new(&hex(key)).encrypt(&mut b);
            assert_eq!(b.to_vec(), hex(ct));
        }
    }

    #[test]
    fn aes_against_the_crate() {
        use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
        let mut x = 12345u32;
        let mut rnd = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect()
        };
        for klen in [16, 32] {
            for _ in 0..50 {
                let (key, mut blk) = (rnd(klen), rnd(16));
                let mut want = GenericArray::clone_from_slice(&blk);
                if klen == 16 {
                    aes::Aes128::new_from_slice(&key).unwrap().encrypt_block(&mut want);
                } else {
                    aes::Aes256::new_from_slice(&key).unwrap().encrypt_block(&mut want);
                }
                let b: &mut [u8; 16] = (&mut blk[..]).try_into().unwrap();
                Aes::new(&key).encrypt(b);
                assert_eq!(&b[..], &want[..]);
            }
        }
    }
}
