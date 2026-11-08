//! Encrypted archives (the spec's "Encrypted archives"): an archive sealed with a passphrase as
//! age's scrypt recipient seals a file, its age header kept in binary ([`Sealer`]), and opened
//! again ([`Envelope`]).
//!
//! The payload is age's STREAM: chunks of 64 KiB of the archive, each sealed by
//! ChaCha20-Poly1305 under a key derived from the file key and the payload's nonce, the last one
//! flagged as such. The file key is wrapped by a key that scrypt derives from the passphrase and
//! a salt, and the header (as age's text) authenticated by an HMAC under the file key.
//!
//! A sealed archive is written as it is made: every chunk is sealed as the next begins, but
//! the first, which holds the archive's size hint block (that the spec requires of the payload)
//! and is sealed last, once the footer gives the size (the encoder writes the block with a
//! placeholder, [`super::Encoder::with_size_hint`]).

use std::io::{self, Seek, SeekFrom, Write};

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use xxhash_rust::xxh64::xxh64;

use super::{Decoded, Failure, HEADER_MAGIC, OVERLAY_HEADER_MAGIC, SIZE_HINT, invalid};

pub const ENCRYPTED_HEADER_MAGIC: [u8; 8] = [0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a, 0x01, 0x00];
pub const ENCRYPTED_MIRRORED_HEADER_MAGIC: [u8; 8] = [0xf9, 0xca, 0xf2, 0xa6, 0xd0, 0x2a, 0x02, 0x00];
const ENCRYPTED_MIRROR_MAGIC: [u8; 8] = [0x78, 0xd7, 0x3f, 0x69, 0x0a, 0xff, 0x44, 0x83];

/// The header fields: the work factor, scrypt's salt, the file key wrapped, the header's MAC
/// and the payload's nonce.
const FIELDS: usize = 1 + 16 + 32 + 32 + 16;
/// A mirrored header: its magic, index, the header fields and checksum.
const MIRROR: usize = 8 + 4 + FIELDS + 8;
/// The plaintext of a chunk (but the last), and the chunk sealed.
const CHUNK: usize = 64 << 10;
const SEALED: usize = CHUNK + 16;

/// The work factor (lg N of scrypt) sealing by default: age's, about a second and 256 MiB.
pub const WORK_FACTOR: u8 = 18;
/// The most work factor opened (age's too): 4 GiB of scrypt's memory.
pub const WORK_FACTOR_LIMIT: u8 = 22;

/// Whether `c` is an encrypted archive (by its magic).
pub fn is_encrypted(c: &[u8]) -> bool {
    c.starts_with(&ENCRYPTED_HEADER_MAGIC) || c.starts_with(&ENCRYPTED_MIRRORED_HEADER_MAGIC)
}

/// The length of the header of an archive `mirrored` or not.
fn header_len(mirrored: bool) -> usize {
    8 + FIELDS + if mirrored { 8 } else { 0 }
}

/// The mirrored header (its index from 1) preceding chunk `m`, where one does.
fn mirror_before(m: u64) -> Option<u32> {
    let n = m + 1;
    match n {
        2 | 4 | 8 | 16 | 32 => Some(n.trailing_zeros()),
        _ if n.is_multiple_of(64) => Some(5 + (n / 64) as u32),
        _ => None,
    }
}

/// The mirrored headers preceding chunk `m` and those before it.
#[cfg(test)]
fn mirrors_to(m: u64) -> u64 {
    let n = m + 1;
    if n >= 64 { 5 + n / 64 } else { (63 - n.leading_zeros()) as u64 }
}

/// Where chunk `m` starts, in an archive `mirrored` or not.
#[cfg(test)]
fn chunk_at(m: u64, mirrored: bool) -> u64 {
    let mirrors = if mirrored { mirrors_to(m) * MIRROR as u64 } else { 0 };
    header_len(mirrored) as u64 + m * SEALED as u64 + mirrors
}

/// Base64 without padding (RFC 4648 §4), as age writes its header.
fn base64(d: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

/// The fields of a header, as they are stored.
#[derive(Clone, Copy)]
struct Fields([u8; FIELDS]);

impl Fields {
    fn work_factor(&self) -> u8 {
        self.0[0]
    }

    fn salt(&self) -> &[u8] {
        &self.0[1..17]
    }

    fn wrapped(&self) -> &[u8] {
        &self.0[17..49]
    }

    fn mac(&self) -> &[u8] {
        &self.0[49..81]
    }

    fn nonce(&self) -> &[u8] {
        &self.0[81..97]
    }

    /// The age header that the MAC is of: up to its `---`.
    fn age_header(&self) -> String {
        format!(
            "age-encryption.org/v1\n-> scrypt {} {}\n{}\n---",
            base64(self.salt()),
            self.work_factor(),
            base64(self.wrapped())
        )
    }

    /// The key wrapping the file key, derived by scrypt from `passphrase`.
    fn wrapping(&self, passphrase: &[u8]) -> Key {
        let salt = [&b"age-encryption.org/v1/scrypt"[..], self.salt()].concat();
        let params = scrypt::Params::new(self.work_factor(), 8, 1, 32).expect("a work factor in range");
        let mut key = Key::default();
        scrypt::scrypt(passphrase, &salt, &params, &mut key).expect("a key of 32 bytes");
        key
    }

    /// The MAC of the header under `file_key`.
    fn mac_of(&self, file_key: &[u8; 16]) -> [u8; 32] {
        let mut k = [0; 32];
        Hkdf::<Sha256>::new(None, file_key).expand(b"header", &mut k).expect("32 bytes");
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&k).expect("any key");
        m.update(self.age_header().as_bytes());
        m.finalize().into_bytes().into()
    }

    /// The cipher of the payload under `file_key`.
    fn payload(&self, file_key: &[u8; 16]) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&self.payload_key(file_key))
    }

    /// The key of the payload under `file_key`.
    fn payload_key(&self, file_key: &[u8; 16]) -> Key {
        let mut k = Key::default();
        Hkdf::<Sha256>::new(Some(self.nonce()), file_key).expand(b"payload", &mut k).expect("32 bytes");
        k
    }

    /// The header of an archive `mirrored` or not.
    fn header(&self, mirrored: bool) -> Vec<u8> {
        let magic = if mirrored { ENCRYPTED_MIRRORED_HEADER_MAGIC } else { ENCRYPTED_HEADER_MAGIC };
        let mut h = [&magic[..], &self.0].concat();
        if mirrored {
            h.extend_from_slice(&xxh64(&h, 0).to_le_bytes());
        }
        h
    }

    /// The `k`-th mirrored header.
    fn mirror(&self, k: u32) -> Vec<u8> {
        let mut h = [&ENCRYPTED_MIRROR_MAGIC[..], &k.to_le_bytes(), &self.0].concat();
        h.extend_from_slice(&xxh64(&h, 0).to_le_bytes());
        h
    }
}

/// The nonce of chunk `m`, the last or not.
fn chunk_nonce(m: u64, last: bool) -> Nonce {
    let mut n = Nonce::default();
    n[3..11].copy_from_slice(&m.to_be_bytes());
    n[11] = last as u8;
    n
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0; N];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(format!("no random bytes: {e}")))?;
    Ok(b)
}

/// An archive sealed as it is written to `W`: [`Sealer::finish`] once it is all written. The
/// archive has to begin with a size hint block of a padded LEB128 (12 bytes after the header
/// magic), which is filled in from its footer.
pub struct Sealer<W: Write + Seek> {
    out: W,
    mirrored: bool,
    file_key: [u8; 16],
    fields: Fields,
    cipher: ChaCha20Poly1305,
    /// The first chunk, sealed last.
    first: Vec<u8>,
    /// The chunk being written after it, and its index.
    chunk: Vec<u8>,
    m: u64,
    /// The last bytes written (of the footer).
    tail: Vec<u8>,
    done: bool,
}

impl<W: Write + Seek> Sealer<W> {
    /// Sealing with `passphrase` at work factor `work_factor` (`WORK_FACTOR`) into `out`, at its
    /// start, with mirrored headers or not; scrypt is run here.
    pub fn new(out: W, passphrase: &[u8], work_factor: u8, mirrored: bool) -> io::Result<Sealer<W>> {
        if !(12..0x3f).contains(&work_factor) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a work factor out of [12, 63)"));
        }
        let file_key: [u8; 16] = random()?;
        let mut fields = Fields([0; FIELDS]);
        fields.0[0] = work_factor;
        fields.0[1..17].copy_from_slice(&random::<16>()?);
        let mut body = file_key.to_vec();
        let wrap = ChaCha20Poly1305::new(&fields.wrapping(passphrase));
        let tag = wrap.encrypt_in_place_detached(&Nonce::default(), &[], &mut body).expect("a short key");
        fields.0[17..33].copy_from_slice(&body);
        fields.0[33..49].copy_from_slice(&tag);
        let mac = fields.mac_of(&file_key);
        fields.0[49..81].copy_from_slice(&mac);
        let mut s = Sealer {
            cipher: fields.payload(&file_key),
            out,
            mirrored,
            file_key,
            fields,
            first: Vec::with_capacity(CHUNK),
            chunk: Vec::with_capacity(CHUNK),
            m: 1,
            tail: Vec::new(),
            done: false,
        };
        s.begin()?;
        Ok(s)
    }

    /// A new nonce for the payload, and its header written (the first chunk's room left).
    fn begin(&mut self) -> io::Result<()> {
        self.fields.0[81..97].copy_from_slice(&random::<16>()?);
        self.cipher = self.fields.payload(&self.file_key);
        self.out.write_all(&self.fields.header(self.mirrored))?;
        self.out.seek(SeekFrom::Current(SEALED as i64))?;
        Ok(())
    }

    /// Starts over, of a new payload nonce, the output having been emptied by `empty` (to be
    /// written again from its start).
    pub fn restart(&mut self, empty: impl FnOnce(&mut W) -> io::Result<()>) -> io::Result<()> {
        empty(&mut self.out)?;
        self.out.seek(SeekFrom::Start(0))?;
        self.first.clear();
        self.chunk.clear();
        self.m = 1;
        self.tail.clear();
        self.done = false;
        self.begin()
    }

    /// Chunk `m` of `d` sealed, the last or not.
    fn seal(&self, m: u64, last: bool, d: &[u8]) -> Vec<u8> {
        let mut c = Vec::with_capacity(d.len() + 16);
        c.extend_from_slice(d);
        let tag = self.cipher.encrypt_in_place_detached(&chunk_nonce(m, last), &[], &mut c).expect("a chunk");
        c.extend_from_slice(&tag);
        c
    }

    /// Writes chunk `self.m` (`self.chunk`), with the mirrored header before it.
    fn put_chunk(&mut self, last: bool) -> io::Result<()> {
        if self.mirrored
            && let Some(k) = mirror_before(self.m)
        {
            self.out.write_all(&self.fields.mirror(k))?;
        }
        let c = self.seal(self.m, last, &self.chunk);
        self.out.write_all(&c)?;
        self.chunk.clear();
        self.m += 1;
        Ok(())
    }

    /// Seals what is left: the last chunk, then the first (with its size hint).
    pub fn finish(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("not an archive to seal: {m}"));
        // the size hint, from the footer
        let t = &self.tail;
        let n = t.len().checked_sub(2).map(|e| u16::from_le_bytes([t[e], t[e + 1]]) as usize);
        let fields = n.and_then(|n| t.get(t.len().checked_sub(2 + n)?..t.len() - 2)).ok_or(bad("no footer"))?;
        let hint = aqr_common::varint::get(fields, &mut 0).map_err(|_| bad("no footer"))?;
        let at = HEADER_MAGIC.len();
        let header = self.first.get(..at).ok_or(bad("too short"))?;
        if header != HEADER_MAGIC && header != OVERLAY_HEADER_MAGIC
            || self.first.get(at..at + 2) != Some(&[SIZE_HINT as u8, 10][..])
        {
            return Err(bad("no size hint block of 10 bytes first"));
        }
        let mut v = hint;
        for (i, b) in self.first[at + 2..at + 12].iter_mut().enumerate() {
            *b = (v & 0x7f) as u8 | if i < 9 { 0x80 } else { 0 };
            v >>= 7;
        }
        let only = self.m == 1 && self.chunk.is_empty();
        if !only {
            self.put_chunk(true)?;
        }
        let first = self.seal(0, only, &self.first);
        let h = header_len(self.mirrored) as u64;
        let end = if only { h + first.len() as u64 } else { self.out.stream_position()? };
        self.out.seek(SeekFrom::Start(h))?;
        self.out.write_all(&first)?;
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        self.done = true;
        Ok(())
    }

    /// Archive `c` sealed here, unlocked by the file key (rather than the passphrase).
    pub fn unseal<'c>(&self, c: &'c [u8]) -> Decoded<Unlocked<'c>> {
        let e = open(c)?;
        if e.fields.0 != self.fields.0 {
            return invalid("not the archive sealed");
        }
        Ok(Unlocked { cipher: self.cipher.clone(), key: self.fields.payload_key(&self.file_key), chunks: e.chunks })
    }

    pub fn get_ref(&self) -> &W {
        &self.out
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    /// The output, [`Sealer::finish`]ed.
    pub fn into_inner(mut self) -> io::Result<W> {
        self.finish()?;
        Ok(self.out)
    }
}

impl<W: Write + Seek> Write for Sealer<W> {
    fn write(&mut self, mut d: &[u8]) -> io::Result<usize> {
        if self.done {
            return Err(io::Error::other("an archive sealed already"));
        }
        let n = d.len();
        if d.len() >= 32 {
            self.tail.clear();
            self.tail.extend_from_slice(&d[d.len() - 32..]);
        } else {
            self.tail.extend_from_slice(d);
            let over = self.tail.len().saturating_sub(32);
            self.tail.drain(..over);
        }
        let room = (CHUNK - self.first.len()).min(d.len());
        self.first.extend_from_slice(&d[..room]);
        d = &d[room..];
        while !d.is_empty() {
            // (a chunk full is sealed as more follows it: the last one is flagged)
            if self.chunk.len() == CHUNK {
                self.put_chunk(false)?;
            }
            let room = (CHUNK - self.chunk.len()).min(d.len());
            self.chunk.extend_from_slice(&d[..room]);
            d = &d[room..];
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// An encrypted archive parsed: its header fields and its chunks sealed.
pub struct Envelope<'c> {
    fields: Fields,
    chunks: Vec<&'c [u8]>,
}

/// An encrypted archive whose file key is unwrapped: its payload to be decrypted.
pub struct Unlocked<'c> {
    cipher: ChaCha20Poly1305,
    /// The payload's key, for chunks that do not authenticate ([`Unlocked::decrypt_lossy`]).
    key: Key,
    chunks: Vec<&'c [u8]>,
}

/// Encrypted archive `c` parsed, its header checked (but for the passphrase).
pub fn open(c: &[u8]) -> Decoded<Envelope<'_>> {
    let mirrored = c.starts_with(&ENCRYPTED_MIRRORED_HEADER_MAGIC);
    if !mirrored && !c.starts_with(&ENCRYPTED_HEADER_MAGIC) {
        return invalid("not an encrypted archive");
    }
    let h = header_len(mirrored);
    let Some(header) = c.get(..h) else { return invalid("truncated encrypted header") };
    if mirrored && xxh64(&header[..h - 8], 0).to_le_bytes() != header[h - 8..] {
        return invalid("bad encrypted header checksum");
    }
    let fields = Fields(header[8..8 + FIELDS].try_into().unwrap());
    if !(12..0x3f).contains(&fields.work_factor()) {
        return invalid("bad work factor");
    }
    let (chunks, cut) = chunks_of(c, mirrored);
    if cut {
        return invalid("truncated encrypted chunk");
    }
    envelope(fields, chunks)
}

/// The envelope of `fields` and `chunks`, checked.
fn envelope(fields: Fields, chunks: Vec<&[u8]>) -> Decoded<Envelope<'_>> {
    if chunks.is_empty() {
        return invalid("no payload");
    }
    if fields.work_factor() > WORK_FACTOR_LIMIT {
        return Err(Failure::Resource(format!("a work factor of {} (over {WORK_FACTOR_LIMIT})", fields.work_factor())));
    }
    Ok(Envelope { fields, chunks })
}

/// The chunks sealed of encrypted archive `c` (`mirrored` or not), at the places the spec
/// tells; and whether the last was too short to hold anything (left out).
fn chunks_of(c: &[u8], mirrored: bool) -> (Vec<&[u8]>, bool) {
    let h = header_len(mirrored);
    let mut chunks = Vec::with_capacity(c.len().saturating_sub(h) / SEALED + 1);
    let mut at = h;
    while at < c.len() {
        let m = chunks.len() as u64;
        if mirrored && mirror_before(m).is_some() {
            at += MIRROR;
        }
        let rest = c.len().saturating_sub(at);
        if rest <= 16 {
            return (chunks, true);
        }
        chunks.push(&c[at..at + rest.min(SEALED)]);
        at += rest.min(SEALED);
    }
    (chunks, false)
}

/// Where the `k`-th mirrored header (from 1) is in its archive (spec, "Encrypted archives").
fn mirror_at(k: u32) -> u64 {
    let m = if k <= 6 { (1u64 << k) - 1 } else { 64 * (k as u64 - 5) - 1 };
    header_len(true) as u64 + SEALED as u64 * m + MIRROR as u64 * (k as u64 - 1)
}

/// The header fields of an intact mirrored header of `c` telling that its archive starts where
/// `c` does (found by its magic), and its index.
fn mirrored_header(c: &[u8]) -> Option<(Fields, u32)> {
    let mut from = 0;
    while let Some(i) = c.get(from..)?.windows(8).position(|w| w == ENCRYPTED_MIRROR_MAGIC) {
        let at = from + i;
        from = at + 1;
        let Some(h) = c.get(at..at + MIRROR) else { break };
        let k = u32::from_le_bytes(h[8..12].try_into().unwrap());
        if k == 0 || xxh64(&h[..MIRROR - 8], 0).to_le_bytes() != h[MIRROR - 8..] || mirror_at(k) != at as u64 {
            continue;
        }
        return Some((Fields(h[12..12 + FIELDS].try_into().unwrap()), k));
    }
    None
}

/// Whether `c` is an encrypted archive whose header magic is damaged: one not starting with a
/// header magic of any kind, of an intact mirrored header telling that its archive starts where
/// `c` does.
pub fn is_damaged_encrypted(c: &[u8]) -> bool {
    !is_encrypted(c)
        && !c.starts_with(&HEADER_MAGIC)
        && !c.starts_with(&OVERLAY_HEADER_MAGIC)
        && mirrored_header(c).is_some()
}

/// What [`open_damaged`] did of an encrypted archive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Opened {
    /// The mirrored header (its index) whose fields were taken, the header not intact.
    pub header_from: Option<u32>,
    /// Whether the archive is cut short: the bytes after its last chunk whole too few to hold
    /// anything (left out).
    pub cut: bool,
}

/// Encrypted archive `c` parsed as [`open`] does, but damaged (spec, "Repair"): the header fields
/// of an intact mirrored header where its header is not intact (its magic or its checksum
/// damaged), and its last chunk left out where it is too short to hold anything.
pub fn open_damaged(c: &[u8]) -> Decoded<(Envelope<'_>, Opened)> {
    let mirrored = c.starts_with(&ENCRYPTED_MIRRORED_HEADER_MAGIC);
    let h = header_len(mirrored);
    let intact = (mirrored || c.starts_with(&ENCRYPTED_HEADER_MAGIC))
        && c.get(..h).is_some_and(|header| !mirrored || xxh64(&header[..h - 8], 0).to_le_bytes() == header[h - 8..])
        && (12..0x3f).contains(&c[8]);
    let (fields, mirrored, header_from) = match intact {
        true => (Fields(c[8..8 + FIELDS].try_into().unwrap()), mirrored, None),
        false => match mirrored_header(c) {
            Some((f, k)) => (f, true, Some(k)),
            None => return invalid("an encrypted header damaged, and no intact mirrored header"),
        },
    };
    if !(12..0x3f).contains(&fields.work_factor()) {
        return invalid("bad work factor");
    }
    let (chunks, cut) = chunks_of(c, mirrored);
    Ok((envelope(fields, chunks)?, Opened { header_from, cut }))
}

impl<'c> Envelope<'c> {
    /// The work factor of scrypt (lg N).
    pub fn work_factor(&self) -> u8 {
        self.fields.work_factor()
    }

    /// scrypt's salt and the payload's nonce, as stored.
    pub fn salt_and_nonce(&self) -> (&[u8], &[u8]) {
        (self.fields.salt(), self.fields.nonce())
    }

    /// The number of chunks sealed.
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }

    /// The payload unlocked by `passphrase`: a decryption failure where it does not unwrap the
    /// file key (scrypt is run here).
    pub fn unlock(&self, passphrase: &[u8]) -> Decoded<Unlocked<'c>> {
        let f = &self.fields;
        let mut key = [0u8; 16];
        key.copy_from_slice(&f.wrapped()[..16]);
        let wrap = ChaCha20Poly1305::new(&f.wrapping(passphrase));
        let tag = Tag::from_slice(&f.wrapped()[16..]);
        if wrap.decrypt_in_place_detached(&Nonce::default(), &[], &mut key, tag).is_err() {
            return Err(Failure::Decryption("a wrong passphrase".into()));
        }
        // (compared in constant time, as the MAC is a secret's)
        let mac = f.mac_of(&key);
        if mac.iter().zip(f.mac()).fold(0, |d, (a, b)| d | (a ^ b)) != 0 {
            return invalid("bad header MAC");
        }
        Ok(Unlocked { cipher: f.payload(&key), key: f.payload_key(&key), chunks: self.chunks.clone() })
    }
}

impl<'c> Unlocked<'c> {
    /// The bytes of the payload.
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|c| c.len() - 16).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The payload decrypted into `out` (of [`Unlocked::len`] bytes), on up to `threads`
    /// threads.
    pub fn decrypt_into(&self, out: &mut [u8], threads: usize) -> Decoded<()> {
        assert_eq!(out.len(), self.len());
        /// Chunks decrypted by a task: 4 MiB.
        const TASK: usize = 64;
        let last = self.chunks.len() - 1;
        let parts: Vec<std::sync::Mutex<&mut [u8]>> = out.chunks_mut(CHUNK * TASK).map(std::sync::Mutex::new).collect();
        let r = aqr_common::pool::par_map(parts.len(), threads, |t| {
            let mut part = parts[t].lock().unwrap();
            for (i, d) in part.chunks_mut(CHUNK).enumerate() {
                let m = t * TASK + i;
                let c = self.chunks[m];
                d.copy_from_slice(&c[..c.len() - 16]);
                let tag = Tag::from_slice(&c[c.len() - 16..]);
                if self.cipher.decrypt_in_place_detached(&chunk_nonce(m as u64, m == last), &[], d, tag).is_err() {
                    return invalid(format!("encrypted chunk {m} does not authenticate"));
                }
            }
            Ok(())
        });
        r.into_iter().collect()
    }

    /// The payload decrypted into `out` (of [`Unlocked::len`] bytes) as [`Unlocked::decrypt_into`]
    /// does, but of chunks that do not authenticate too: decrypted without it (their bytes not to
    /// be trusted, spec "Repair"), the last one tried as not the last too (the archive cut short
    /// after it). Returns the chunks that do not authenticate.
    pub fn decrypt_lossy(&self, out: &mut [u8], threads: usize) -> Vec<usize> {
        use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
        assert_eq!(out.len(), self.len());
        const TASK: usize = 64;
        let last = self.chunks.len() - 1;
        let parts: Vec<std::sync::Mutex<&mut [u8]>> = out.chunks_mut(CHUNK * TASK).map(std::sync::Mutex::new).collect();
        let failed = aqr_common::pool::par_map(parts.len(), threads, |t| {
            let mut part = parts[t].lock().unwrap();
            let mut failed = Vec::new();
            for (i, d) in part.chunks_mut(CHUNK).enumerate() {
                let m = t * TASK + i;
                let c = self.chunks[m];
                let (body, tag) = (&c[..c.len() - 16], Tag::from_slice(&c[c.len() - 16..]));
                let authentic = [m == last, false].into_iter().take(1 + (m == last) as usize).any(|is_last| {
                    d.copy_from_slice(body);
                    self.cipher.decrypt_in_place_detached(&chunk_nonce(m as u64, is_last), &[], d, tag).is_ok()
                });
                if !authentic {
                    // (ChaCha20-Poly1305's keystream: from the second block, the first the MAC's key)
                    d.copy_from_slice(body);
                    let mut k = chacha20::ChaCha20::new(&self.key, &chunk_nonce(m as u64, m == last));
                    k.seek(64u64);
                    k.apply_keystream(d);
                    failed.push(m);
                }
            }
            failed
        });
        failed.into_iter().flatten().collect()
    }

    /// The bytes of the payload of chunk `m`.
    pub fn chunk_range(&self, m: usize) -> std::ops::Range<usize> {
        let start = m * CHUNK;
        start..start + self.chunks[m].len() - 16
    }

    /// The payload decrypted.
    pub fn decrypt(&self, threads: usize) -> Decoded<Vec<u8>> {
        let mut d = vec![0; self.len()];
        self.decrypt_into(&mut d, threads)?;
        Ok(d)
    }

    /// The payload to be decrypted a chunk at a time, as its bytes are read ([`Decrypting`]).
    pub fn decrypting(self) -> Decrypting<'c> {
        let len = self.len();
        let layout = std::alloc::Layout::array::<u8>(len.max(1)).expect("a payload of a file's size");
        // (zeroed: of pages the system maps as they are touched, so that the chunks not read take
        // no memory)
        // SAFETY: a layout of a nonzero size
        let buf = std::ptr::NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        let done = (0..self.chunks.len()).map(|_| std::sync::OnceLock::new()).collect();
        Decrypting { unlocked: self, buf, len, done, decrypted: std::sync::atomic::AtomicUsize::new(0) }
    }
}

/// The payload of an encrypted archive, decrypted a chunk at a time as its bytes are first read
/// ([`Decrypting::get`]), each chunk authenticated: a file read alone decrypts its blocks' chunks
/// and those of the blocks that tell where they are, not the whole archive.
pub struct Decrypting<'c> {
    unlocked: Unlocked<'c>,
    /// The payload's bytes, of each chunk once it is decrypted ([`Decrypting::done`]).
    buf: std::ptr::NonNull<u8>,
    len: usize,
    /// By chunk, once decrypted: whether it authenticates.
    done: Vec<std::sync::OnceLock<bool>>,
    /// The chunks decrypted so far.
    decrypted: std::sync::atomic::AtomicUsize,
}

// SAFETY: the bytes of a chunk are written once, by the one initializing its `done`, before any
// reference to them is made; then only read.
unsafe impl Send for Decrypting<'_> {}
unsafe impl Sync for Decrypting<'_> {}

impl Drop for Decrypting<'_> {
    fn drop(&mut self) {
        let layout = std::alloc::Layout::array::<u8>(self.len.max(1)).unwrap();
        // SAFETY: allocated so in `Unlocked::decrypting`, no reference to it left
        unsafe { std::alloc::dealloc(self.buf.as_ptr(), layout) };
    }
}

impl<'c> Decrypting<'c> {
    /// The bytes of the payload.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of chunks.
    pub fn chunks(&self) -> usize {
        self.done.len()
    }

    /// The payload unlocked, to be decrypted otherwise ([`Unlocked::decrypt_lossy`]).
    pub fn unlocked(&self) -> &Unlocked<'c> {
        &self.unlocked
    }

    /// The number of chunks decrypted so far.
    pub fn decrypted(&self) -> usize {
        self.decrypted.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Chunk `m` decrypted where it is not yet: whether it authenticates.
    fn chunk(&self, m: usize) -> bool {
        *self.done[m].get_or_init(|| {
            let c = self.unlocked.chunks[m];
            let n = c.len() - 16;
            // SAFETY: the bytes of chunk `m`, within the allocation; written by this one call
            // alone (that of `done[m]`'s initialization), with no reference to them yet
            let d = unsafe { std::slice::from_raw_parts_mut(self.buf.as_ptr().add(m * CHUNK), n) };
            d.copy_from_slice(&c[..n]);
            self.decrypted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tag = Tag::from_slice(&c[n..]);
            let last = m + 1 == self.done.len();
            self.unlocked.cipher.decrypt_in_place_detached(&chunk_nonce(m as u64, last), &[], d, tag).is_ok()
        })
    }

    /// Bytes `r` of the payload, the chunks they are in decrypted where they are not yet: an
    /// invalid archive where `r` is past the payload, or a chunk does not authenticate.
    pub fn get(&self, r: std::ops::Range<usize>) -> Decoded<&[u8]> {
        if r.start > r.end || r.end > self.len {
            return invalid("bytes past the payload");
        }
        if r.is_empty() {
            return Ok(&[]);
        }
        for m in r.start / CHUNK..=(r.end - 1) / CHUNK {
            if !self.chunk(m) {
                return invalid(format!("encrypted chunk {m} does not authenticate"));
            }
        }
        // SAFETY: of chunks decrypted, not to be written again
        Ok(unsafe { std::slice::from_raw_parts(self.buf.as_ptr().add(r.start), r.len()) })
    }

    /// The whole payload, the chunks not decrypted yet decrypted on up to `threads` threads.
    pub fn all(&self, threads: usize) -> Decoded<&[u8]> {
        /// Chunks decrypted by a task: 4 MiB.
        const TASK: usize = 64;
        let n = self.done.len();
        let r = aqr_common::pool::par_map(n.div_ceil(TASK), threads, |t| {
            match (t * TASK..n.min((t + 1) * TASK)).find(|&m| !self.chunk(m)) {
                Some(m) => invalid(format!("encrypted chunk {m} does not authenticate")),
                None => Ok(()),
            }
        });
        r.into_iter().collect::<Decoded<()>>()?;
        self.get(0..self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Encoder, Level, NoModules, decode, open_payload};
    use super::*;
    use crate::Formula;

    /// An archive (with its size hint to be filled in) of `n` bytes of a file of its own.
    fn archive(n: usize) -> Vec<u8> {
        let d: Vec<u8> = (0..n as u64).map(|i| (i.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 56) as u8).collect();
        Encoder::new(Level::STORED).with_size_hint(true).encode(&Formula::raw("raw", d), None).unwrap()
    }

    fn sealed(a: &[u8], pass: &[u8], mirrored: bool) -> Vec<u8> {
        let mut s = Sealer::new(io::Cursor::new(Vec::new()), pass, 12, mirrored).unwrap();
        // (in pieces of every size, across the chunks' bounds)
        let mut rest = a;
        for n in (1..).map(|i| i * 7919 % 100_003) {
            if rest.is_empty() {
                break;
            }
            let (now, after) = rest.split_at(n.min(rest.len()));
            s.write_all(now).unwrap();
            rest = after;
        }
        s.into_inner().unwrap().into_inner()
    }

    /// The payload of `c` by `pass`, the size hint as the footer's.
    fn opened(c: &[u8], pass: &[u8]) -> Decoded<Vec<u8>> {
        open(c)?.unlock(pass)?.decrypt(4)
    }

    #[test]
    fn seals_and_opens() {
        for n in [0, 1, 1000, CHUNK - 40, CHUNK, CHUNK + 1, 3 * CHUNK + 5, 70 * CHUNK] {
            let a = archive(n);
            for mirrored in [false, true] {
                let c = sealed(&a, b"correct horse", mirrored);
                let chunks = a.len().div_ceil(CHUNK) as u64;
                let mirrors = if mirrored { (1..chunks).filter(|&m| mirror_before(m).is_some()).count() } else { 0 };
                assert_eq!(c.len(), header_len(mirrored) + a.len() + 16 * chunks as usize + MIRROR * mirrors);
                let p = opened(&c, b"correct horse").unwrap();
                // (as the file was, but for the size hint filled in)
                assert_eq!(p.len(), a.len());
                assert_eq!((&p[..8], &p[18..]), (&a[..8], &a[18..]));
                assert_ne!(p[8..18], a[8..18]);
                assert_eq!(decode(&p, &NoModules).unwrap().len(), n);
                assert_eq!(open_payload(&p, &NoModules).map(|_| ()), Ok(()));
                assert!(matches!(opened(&c, b"correct horses"), Err(Failure::Decryption(_))));
                assert!(matches!(opened(&c, b""), Err(Failure::Decryption(_))));
                assert!(matches!(super::super::open(&c, &NoModules).map(|_| ()), Err(Failure::Decryption(_))));
            }
        }
    }

    #[test]
    fn takes_an_empty_passphrase() {
        let a = archive(100);
        let c = sealed(&a, b"", true);
        assert_eq!(opened(&c, b"").unwrap().len(), a.len());
        assert!(matches!(opened(&c, b" "), Err(Failure::Decryption(_))));
    }

    #[test]
    fn tells_damage() {
        let a = archive(3 * CHUNK);
        let c = sealed(&a, b"pass", true);
        // the header's checksum, before scrypt
        let mut d = c.clone();
        d[20] ^= 1;
        assert!(matches!(open(&d).map(|_| ()), Err(Failure::Invalid(_))));
        // the header's MAC, unchecked without a checksum
        let c = sealed(&a, b"pass", false);
        let mut d = c.clone();
        d[8 + 60] ^= 1;
        assert!(matches!(opened(&d, b"pass"), Err(Failure::Invalid(_))));
        // a chunk, the last one cut short, one more
        for at in [header_len(false) + 10, c.len() - 1] {
            let mut d = c.clone();
            d[at] ^= 1;
            assert!(matches!(opened(&d, b"pass"), Err(Failure::Invalid(_))));
        }
        assert!(matches!(opened(&c[..c.len() - 100], b"pass"), Err(Failure::Invalid(_))));
        assert!(matches!(opened(&c[..header_len(false) + SEALED], b"pass"), Err(Failure::Invalid(_))));
        assert!(matches!(opened(&[&c[..], &[0; 17]].concat(), b"pass"), Err(Failure::Invalid(_))));
        // too much work
        let mut d = c.clone();
        d[8] = WORK_FACTOR_LIMIT + 1;
        assert!(matches!(open(&d).map(|_| ()), Err(Failure::Resource(_))));
    }

    /// An encrypted archive of 24 files (of 3 chunks or so each: in parcels of their own, two by
    /// two of a stream they share, or of a module's application), sealed: its contents as
    /// they were, and those taken alone of a few chunks of it decrypted, not all.
    #[test]
    fn decrypted_as_read() {
        use super::super::tests::{REV, natives};
        use super::super::{ContentAt, Entry, EntryNode, Extra, Leaf, Payload, SegmentWriter, Tree, open_indexed};
        use crate::Node;
        const N: usize = 24;
        const LEN: usize = 150_000;
        let data: Vec<Vec<u8>> = (0..N).map(|k| aqr_common::rng::noise(LEN + k)).collect();
        let enc = Encoder::new(Level::STORED);
        let mut w = SegmentWriter::with_size_hint(true);
        let mut a = w.head().to_vec();
        let (mut t, mut contents) = (Tree::default(), Vec::new());
        for k in (0..N).step_by(2) {
            let mut f = Formula::default();
            let (x, y) = (&data[k], &data[k + 1]);
            let leaves = match k % 6 {
                // (each of a stream of its own)
                0 => {
                    let (sx, sy) = (f.push("x", x.clone()), f.push("y", y.clone()));
                    vec![Leaf { outputs: vec![sx], sums: vec![None] }, Leaf { outputs: vec![sy], sums: vec![None] }]
                }
                // (both of one stream, the second's leaf after the first's)
                2 => {
                    f.push("xy", [&x[..], y].concat());
                    vec![
                        Leaf { outputs: vec![Node::slice(0, 0, x.len() as u64)], sums: vec![None] },
                        Leaf { outputs: vec![Node::slice(0, x.len() as u64, y.len() as u64)], sums: vec![None] },
                    ]
                }
                // (reversed, by a module)
                _ => {
                    let rx: Vec<u8> = x.iter().rev().copied().collect();
                    let ry: Vec<u8> = y.iter().rev().copied().collect();
                    let (sx, sy) = (f.push("x", rx), f.push("y", ry));
                    let rev = |s, n| Node::apply(REV, vec![1, 2, 3], vec![s], n as u64);
                    vec![
                        Leaf { outputs: vec![rev(sx, x.len())], sums: vec![None] },
                        Leaf { outputs: vec![rev(sy, y.len())], sums: vec![None] },
                    ]
                }
            };
            let batch = w.batch(&enc, &f, &leaves).unwrap();
            w.commit(&batch);
            a.extend_from_slice(&batch.bytes);
            for (i, &leaf) in batch.leaf_at.iter().enumerate() {
                let c = contents.len();
                contents.push(ContentAt { leaf, output: 0, size: data[k + i].len() as u64 });
                let name = format!("f{c}").into_bytes();
                t.entries.push(Entry { parent: None, name, node: EntryNode::File(c), extra: Extra::default() });
            }
        }
        a.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        let c = sealed(&a, b"pass", true);
        let unlocked = || open(&c).unwrap().unlock(b"pass").unwrap().decrypting();
        let natives = natives();
        // all of them
        let d = unlocked();
        let archive = open_indexed(Payload::Sealed(&d), &natives, 2).unwrap();
        let got = std::sync::Mutex::new(vec![None; N]);
        archive
            .contents(2, |c, d| {
                got.lock().unwrap()[c] = Some(d);
                Ok(())
            })
            .unwrap();
        assert!(got.into_inner().unwrap().into_iter().map(Option::unwrap).eq(data.iter().cloned()));
        assert_eq!(d.decrypted(), d.chunks());
        // each alone: its own chunks (3 or so), those of the metadata block and footer, the first
        // (its size hint), and those of the blocks between its formula block and its stream
        for k in 0..N {
            let d = unlocked();
            let archive = open_indexed(Payload::Sealed(&d), &natives, 2).unwrap();
            let mut only = vec![false; N];
            only[k] = true;
            let got = std::sync::Mutex::new(None);
            archive
                .contents_of(2, Some(&only), |c, d| {
                    assert_eq!(c, k);
                    *got.lock().unwrap() = Some(d);
                    Ok(())
                })
                .unwrap();
            assert!(got.into_inner().unwrap().as_ref() == Some(&data[k]), "{k}");
            let (n, all) = (d.decrypted(), d.chunks());
            assert!(n <= 10 && all > 50, "file {k}: {n} of {all} chunks decrypted");
        }
    }

    /// Damaged, an archive is still decrypted: chunks that do not authenticate without it (their
    /// bytes but the damaged ones as they were), the header from a mirrored header, the last
    /// chunk cut short left out (the one before it then the last, as not flagged so).
    #[test]
    fn decrypts_damaged() {
        let a = archive(5 * CHUNK + 100);
        let c = sealed(&a, b"pass", true);
        // (as sealed: its size hint filled in)
        let a = opened(&c, b"pass").unwrap();
        let lossy = |c: &[u8]| {
            let (e, o) = open_damaged(c).unwrap();
            let u = e.unlock(b"pass").unwrap();
            let mut d = vec![0; u.len()];
            let failed = u.decrypt_lossy(&mut d, 2);
            let ranges: Vec<_> = failed.iter().map(|&m| u.chunk_range(m)).collect();
            (d, failed, o, ranges)
        };
        let (d, failed, o, _) = lossy(&c);
        assert!(d == a && failed.is_empty() && o == Opened::default());
        // (chunk 2: after the mirrored headers before chunks 1 and 3)
        let at = chunk_at(2, true) as usize + 1000;
        let mut x = c.clone();
        x[at] ^= 0x40;
        let (d, failed, _, ranges) = lossy(&x);
        assert!(failed == [2] && ranges.len() == 1 && ranges[0] == (2 * CHUNK..3 * CHUNK));
        let diff: Vec<usize> = (0..a.len()).filter(|&i| d[i] != a[i]).collect();
        assert_eq!(diff, [2 * CHUNK + 1000]);
        // the header magic and its checksum damaged: from the first mirrored header
        let mut x = c.clone();
        x[0] ^= 1;
        x[50] ^= 1;
        assert!(is_damaged_encrypted(&x) && !is_damaged_encrypted(&c));
        let (d, failed, o, _) = lossy(&x);
        assert!(d == a && failed.is_empty() && o.header_from == Some(1));
        // cut in the last chunk: what is left of it, too short, left out, the chunk before it
        // taken as the last
        let cut = chunk_at(5, true) as usize + 10;
        let (d, failed, o, _) = lossy(&c[..cut]);
        assert!(o.cut && failed.is_empty() && d[..] == a[..5 * CHUNK]);
    }

    #[test]
    fn restarts_of_a_new_nonce() {
        let a = archive(2 * CHUNK);
        let mut s = Sealer::new(io::Cursor::new(Vec::new()), b"p", 12, true).unwrap();
        s.write_all(&a[..1000]).unwrap();
        s.restart(|c| {
            c.get_mut().clear();
            Ok(())
        })
        .unwrap();
        s.write_all(&a).unwrap();
        let c = s.into_inner().unwrap().into_inner();
        assert_eq!(opened(&c, b"p").unwrap().len(), a.len());
    }

    /// Writes archives for `scripts/age-interop.py` to check against age, into `AQR_AGE_DIR`:
    /// `sealed-N` (mirrored or not) and `plain-N` (its payload), and opens `age-N` there (as the
    /// script seals `plain-N`) where it is.
    #[test]
    #[ignore]
    fn interop_files() {
        let dir = std::path::PathBuf::from(std::env::var_os("AQR_AGE_DIR").expect("AQR_AGE_DIR"));
        for (i, n) in [10, CHUNK, 70 * CHUNK + 3].into_iter().enumerate() {
            let c = sealed(&archive(n), b"aqueous", i != 0);
            std::fs::write(dir.join(format!("sealed-{i}")), &c).unwrap();
            std::fs::write(dir.join(format!("plain-{i}")), opened(&c, b"aqueous").unwrap()).unwrap();
            if let Ok(a) = std::fs::read(dir.join(format!("age-{i}"))) {
                assert_eq!(opened(&a, b"aqueous").unwrap(), opened(&c, b"aqueous").unwrap());
                eprintln!("age-{i}: opened");
            }
        }
    }

    #[test]
    fn seals_only_archives_with_a_size_hint() {
        let a = Encoder::new(Level::STORED).encode(&Formula::raw("raw", vec![1; 100]), None).unwrap();
        let mut s = Sealer::new(io::Cursor::new(Vec::new()), b"p", 12, true).unwrap();
        s.write_all(&a).unwrap();
        assert!(s.finish().is_err());
    }

    #[test]
    fn base64_as_rfc4648() {
        for (d, s) in [("", ""), ("f", "Zg"), ("fo", "Zm8"), ("foo", "Zm9v"), ("foob", "Zm9vYg"), ("fooba", "Zm9vYmE")]
        {
            assert_eq!(base64(d.as_bytes()), s);
        }
    }

    #[test]
    fn mirrors_as_the_spec_places_them() {
        let before: Vec<u64> = (0..300).filter(|&m| mirror_before(m).is_some()).collect();
        assert_eq!(before, [1, 3, 7, 15, 31, 63, 127, 191, 255]);
        for (k, &m) in before.iter().enumerate() {
            assert_eq!(mirror_before(m), Some(k as u32 + 1));
            assert_eq!(mirrors_to(m), k as u64 + 1);
            // the k-th is 113 + 65,552m + 117(k - 1) bytes after the start
            assert_eq!(chunk_at(m, true) - MIRROR as u64, 113 + 65_552 * m + 117 * k as u64);
        }
        assert_eq!(mirrors_to(0), 0);
        assert_eq!(mirrors_to(2), 1);
    }
}
