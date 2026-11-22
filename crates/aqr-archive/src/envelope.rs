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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

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
fn mirrors_to(m: u64) -> u64 {
    let n = m + 1;
    if n >= 64 { 5 + n / 64 } else { (63 - n.leading_zeros()) as u64 }
}

/// Where chunk `m` starts, in an archive `mirrored` or not.
fn chunk_at(m: u64, mirrored: bool) -> u64 {
    let mirrors = if mirrored { mirrors_to(m) * MIRROR as u64 } else { 0 };
    header_len(mirrored) as u64 + m * SEALED as u64 + mirrors
}

/// Where chunk `m` starts with the mirrored header before it, if any.
fn chunk_with_mirror_at(m: u64, mirrored: bool) -> u64 {
    let before = mirrored && mirror_before(m).is_some();
    chunk_at(m, mirrored) - if before { MIRROR as u64 } else { 0 }
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
        #[cfg(fuzzing)]
        return self.kept(passphrase);
        #[cfg(not(fuzzing))]
        self.derived(passphrase)
    }

    /// [`Fields::wrapping`] fuzzed: scrypt's keys kept, so that the seeds' are derived once, not
    /// for every input.
    #[cfg(fuzzing)]
    fn kept(&self, passphrase: &[u8]) -> Key {
        /// The keys kept at most: those of mutated salts come and go.
        const KEPT: usize = 64;
        type Kept = Vec<((Vec<u8>, u8, Vec<u8>), Key)>;
        static KEYS: Mutex<Kept> = Mutex::new(Vec::new());
        let of = (passphrase.to_vec(), self.work_factor(), self.salt().to_vec());
        if let Some((_, k)) = KEYS.lock().unwrap().iter().find(|(o, _)| *o == of) {
            return *k;
        }
        let k = self.derived(passphrase);
        let mut keys = KEYS.lock().unwrap();
        keys.truncate(KEPT - 1);
        keys.insert(0, (of, k));
        k
    }

    /// [`Fields::wrapping`], scrypt run.
    fn derived(&self, passphrase: &[u8]) -> Key {
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
    /// Where the payload is being written at offsets ([`Sealer::place`]).
    placed: Option<Arc<Placed>>,
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
        // (fuzzed, a salt of its own, so that scrypt's key is derived once: `Fields::kept`)
        let salt = if cfg!(fuzzing) { [0; 16] } else { random::<16>()? };
        fields.0[1..17].copy_from_slice(&salt);
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
            placed: None,
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
        self.placed = None;
        self.begin()
    }

    /// Where nothing is written yet: the payload to be written at offsets of the archive (its
    /// plaintext) through what is returned, its chunks written to the file through `at`, until
    /// [`Sealer::placed`]. None where something is written.
    pub fn place(&mut self, at: Arc<dyn At>) -> Option<Arc<Placed>> {
        if self.done || self.placed.is_some() || self.m != 1 || !self.first.is_empty() || !self.chunk.is_empty() {
            return None;
        }
        let state = Placing::new(self.fields, self.cipher.clone());
        let p = Arc::new(Placed { at, mirrored: self.mirrored, file_key: self.file_key, state: Mutex::new(state) });
        self.placed = Some(p.clone());
        Some(p)
    }

    /// Goes on after the `len` bytes written through [`Sealer::place`], all of them there: the
    /// chunks sealed by it and the payload nonce it may have changed taken over, the first and
    /// the last held as [`Write`] holds them.
    pub fn placed(&mut self, len: u64) -> io::Result<()> {
        let p = self.placed.take().ok_or_else(|| io::Error::other("a sealer not placed"))?;
        let mut s = p.state.lock().unwrap();
        let c = CHUNK as u64;
        // (the chunk the bytes end in, but the first)
        let last = if len > c { (len - 1) / c } else { 0 };
        let filled = |s: &Placing, m: u64| s.open.get(&m).map_or(0, |o| o.1 as u64);
        let whole = s.end == len
            && s.sealed.len() as u64 == last.saturating_sub(1)
            && filled(&s, 0) == len.min(c)
            && (last == 0 || filled(&s, last) == len - last * c)
            && s.open.len() == if last == 0 { (len > 0) as usize } else { 2 };
        if !whole {
            return Err(io::Error::other("a sealed archive placed with bytes missing"));
        }
        let mut take = |m: u64| s.open.remove(&m).map(|(b, n)| b[..n].to_vec()).unwrap_or_default();
        self.first = take(0);
        self.chunk = if last == 0 { Vec::new() } else { take(last) };
        self.m = last.max(1);
        // (the last bytes, of the footer: of the chunk before too where the last is short)
        let before = match last {
            0 => Vec::new(),
            1 => self.first.clone(),
            _ => s.tail.clone().filter(|t| t.0 == last - 1).map(|t| t.1).unwrap_or_default(),
        };
        let now = if last == 0 { &self.first } else { &self.chunk };
        let mut tail = [&before[..], &now[..]].concat();
        tail.drain(..tail.len().saturating_sub(32));
        self.tail = tail;
        self.fields = s.fields;
        self.cipher = s.cipher.clone();
        drop(s);
        self.out.seek(SeekFrom::Start(chunk_with_mirror_at(self.m, self.mirrored)))?;
        Ok(())
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
        if self.placed.is_some() {
            return Err(io::Error::other("a sealer still placed"));
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
        self.opener().unseal(c)
    }

    /// What unseals the archive sealed here ([`Opener::unseal`]), apart from the sealer.
    pub fn opener(&self) -> Opener {
        Opener { fields: self.fields, cipher: self.cipher.clone(), key: self.fields.payload_key(&self.file_key) }
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
        if self.placed.is_some() {
            return Err(io::Error::other("a sealer placed written from its start"));
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

/// The archive a [`Sealer`] sealed, unlocked by its file key ([`Sealer::opener`]).
pub struct Opener {
    fields: Fields,
    cipher: ChaCha20Poly1305,
    key: Key,
}

impl Opener {
    /// Archive `c` unlocked: the one sealed (as its header fields tell), else invalid.
    pub fn unseal<'c>(&self, c: &'c [u8]) -> Decoded<Unlocked<'c>> {
        let e = open(c)?;
        if e.fields.0 != self.fields.0 {
            return invalid("not the archive sealed");
        }
        Ok(Unlocked { cipher: self.cipher.clone(), key: self.key, chunks: e.chunks })
    }
}

/// Where the chunks of a sealed archive written at offsets are written ([`Sealer::place`]): the
/// file, read back where they are moved ([`Placed::moved_back`]).
pub trait At: Send + Sync {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()>;
    fn read_at(&self, at: u64, d: &mut [u8]) -> io::Result<()>;
}

/// The payload of a [`Sealer`] written at offsets of the archive (its plaintext), as
/// [`super::sink::Place`]: each chunk sealed and written once all of its bytes are there and some
/// after it (the last chunk is flagged as such), but the first, held as the sealer holds it. A
/// chunk is sealed at most once under a payload nonce (spec, "Encrypted archives"): a write into
/// one sealed fails, and the bytes moved ([`Placed::moved_back`]) are sealed again under a new
/// nonce. The bytes written are not to overlap (they are counted, not marked).
pub struct Placed {
    at: Arc<dyn At>,
    mirrored: bool,
    file_key: [u8; 16],
    state: Mutex<Placing>,
}

/// What [`Placed`] holds.
struct Placing {
    fields: Fields,
    cipher: ChaCha20Poly1305,
    /// The chunks not sealed, and how many bytes are written into them so far.
    open: HashMap<u64, (Box<[u8]>, usize)>,
    /// The chunks sealed (written).
    sealed: HashSet<u64>,
    /// The chunks whole, but with no byte after them yet (but the first).
    whole: BTreeSet<u64>,
    /// The end of the bytes written.
    end: u64,
    /// The last bytes of the last chunk sealed, and its index.
    tail: Option<(u64, Vec<u8>)>,
}

impl Placing {
    fn new(fields: Fields, cipher: ChaCha20Poly1305) -> Placing {
        let (open, sealed, whole) = (HashMap::new(), HashSet::new(), BTreeSet::new());
        Placing { fields, cipher, open, sealed, whole, end: 0, tail: None }
    }

    /// Bytes `d` put at `at`: the chunks then to be sealed, by their index.
    fn put(&mut self, mut at: u64, mut d: &[u8]) -> io::Result<Vec<(u64, Box<[u8]>)>> {
        let c = CHUNK as u64;
        self.end = self.end.max(at + d.len() as u64);
        while !d.is_empty() {
            let m = at / c;
            if self.sealed.contains(&m) {
                return Err(io::Error::other(format!("chunk {m} of a sealed archive written again once sealed")));
            }
            let o = (at % c) as usize;
            let n = (CHUNK - o).min(d.len());
            let (b, filled) = self.open.entry(m).or_insert_with(|| (vec![0; CHUNK].into_boxed_slice(), 0));
            b[o..o + n].copy_from_slice(&d[..n]);
            *filled += n;
            if *filled > CHUNK {
                return Err(io::Error::other(format!("bytes of chunk {m} of a sealed archive written twice")));
            }
            if *filled == CHUNK && m != 0 {
                self.whole.insert(m);
            }
            at += n as u64;
            d = &d[n..];
        }
        let mut ready = Vec::new();
        while let Some(&m) = self.whole.first()
            && (m + 1) * c < self.end
        {
            self.whole.pop_first();
            let (b, _) = self.open.remove(&m).expect("a chunk whole is open");
            self.sealed.insert(m);
            if self.tail.as_ref().is_none_or(|t| t.0 < m) {
                self.tail = Some((m, b[CHUNK - 32..].to_vec()));
            }
            ready.push((m, b));
        }
        Ok(ready)
    }
}

impl Placed {
    /// Chunk `m` (not the last) of plaintext `d` sealed under `fields` and `cipher`, and written
    /// with the mirrored header before it.
    fn write_sealed(&self, fields: &Fields, cipher: &ChaCha20Poly1305, m: u64, d: &[u8]) -> io::Result<()> {
        let mut c = Vec::with_capacity(MIRROR + SEALED);
        if self.mirrored
            && let Some(k) = mirror_before(m)
        {
            c.extend_from_slice(&fields.mirror(k));
        }
        let from = c.len();
        c.extend_from_slice(d);
        let tag = cipher.encrypt_in_place_detached(&chunk_nonce(m, false), &[], &mut c[from..]).expect("a chunk");
        c.extend_from_slice(&tag);
        self.at.write_at(chunk_with_mirror_at(m, self.mirrored), &c)
    }

    /// Moves the bytes `from..end` written, all of them, back by `by` bytes: under a new payload
    /// nonce (the header written again), every chunk sealed again as it is whole, from the end
    /// on, those sealed read back (each before its place is written again). Fails where other
    /// bytes are written, or bytes of the range are not.
    pub fn moved_back(&self, from: u64, end: u64, by: u64) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        let c = CHUNK as u64;
        let written = s.sealed.len() as u64 * c + s.open.values().map(|o| o.1 as u64).sum::<u64>();
        if from >= end || s.end != end || written != end - from {
            return Err(io::Error::other("bytes of a sealed archive moved but those written"));
        }
        let mut fields = s.fields;
        fields.0[81..97].copy_from_slice(&random::<16>()?);
        let cipher = fields.payload(&self.file_key);
        let old = std::mem::replace(&mut *s, Placing::new(fields, cipher));
        s.end = end + by;
        self.at.write_at(0, &fields.header(self.mirrored))?;
        for m in (from / c..=(end - 1) / c).rev() {
            let d: Box<[u8]> = match old.open.get(&m) {
                Some((b, _)) => b.clone(),
                None => {
                    let mut b = vec![0; SEALED];
                    self.at.read_at(chunk_at(m, self.mirrored), &mut b)?;
                    let (d, tag) = b.split_at_mut(CHUNK);
                    let tag = Tag::clone_from_slice(tag);
                    old.cipher
                        .decrypt_in_place_detached(&chunk_nonce(m, false), &[], d, &tag)
                        .map_err(|_| io::Error::other(format!("chunk {m} of a sealed archive not read back")))?;
                    b.truncate(CHUNK);
                    b.into_boxed_slice()
                }
            };
            let (lo, hi) = ((m * c).max(from), ((m + 1) * c).min(end));
            for (k, b) in s.put(lo + by, &d[(lo - m * c) as usize..(hi - m * c) as usize])? {
                self.write_sealed(&s.fields, &s.cipher, k, &b)?;
            }
        }
        Ok(())
    }
}

impl super::sink::Place for Placed {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
        let (ready, fields, cipher) = {
            let mut s = self.state.lock().unwrap();
            let ready = s.put(at, d)?;
            (ready, s.fields, s.cipher.clone())
        };
        for (m, b) in ready {
            self.write_sealed(&fields, &cipher, m, &b)?;
        }
        Ok(())
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
    if mirrored && !crate::sum_holds(&xxh64(&header[..h - 8], 0).to_le_bytes()[..], &header[h - 8..]) {
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
    /// The chunks sealed, each its bytes and tag (for the fuzz targets' mutator).
    #[cfg(any(fuzzing, test))]
    pub(crate) fn sealed_chunks(&self) -> &[&'c [u8]] {
        &self.chunks
    }

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
        self.decrypt_into_then(out, threads, &|_| {})
    }

    /// [`Unlocked::decrypt_into`], `read` told of the bytes of the archive decrypted (several
    /// chunks, and the mirrored headers between them) as it goes: to let go of a mapping's pages.
    pub fn decrypt_into_then(&self, out: &mut [u8], threads: usize, read: &(dyn Fn(&[u8]) + Sync)) -> Decoded<()> {
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
                if !self.opens(m, m == last, d, tag) {
                    return invalid(format!("encrypted chunk {m} does not authenticate"));
                }
            }
            let (a, b) = (self.chunks[t * TASK], self.chunks[(t * TASK + TASK).min(last + 1) - 1]);
            let n = b.as_ptr() as usize + b.len() - a.as_ptr() as usize;
            // SAFETY: the chunks are of the archive's bytes, one after another
            read(unsafe { std::slice::from_raw_parts(a.as_ptr(), n) });
            Ok(())
        });
        r.into_iter().collect()
    }

    /// The payload decrypted into `out` (of [`Unlocked::len`] bytes) as [`Unlocked::decrypt_into`]
    /// does, but of chunks that do not authenticate too: decrypted without it (their bytes not to
    /// be trusted, spec "Repair"), the last one tried as not the last too (the archive cut short
    /// after it). Returns the chunks that do not authenticate, and whether the last one
    /// authenticates only as not the last: the payload cut short after it, what came after it
    /// lost, though all of what is there is as it was sealed.
    pub fn decrypt_lossy(&self, out: &mut [u8], threads: usize) -> (Vec<usize>, bool) {
        assert_eq!(out.len(), self.len());
        const TASK: usize = 64;
        let last = self.chunks.len() - 1;
        let parts: Vec<std::sync::Mutex<&mut [u8]>> = out.chunks_mut(CHUNK * TASK).map(std::sync::Mutex::new).collect();
        let cut = std::sync::atomic::AtomicBool::new(false);
        let failed = aqr_common::pool::par_map(parts.len(), threads, |t| {
            let mut part = parts[t].lock().unwrap();
            let mut failed = Vec::new();
            for (i, d) in part.chunks_mut(CHUNK).enumerate() {
                let m = t * TASK + i;
                let c = self.chunks[m];
                let (body, tag) = (&c[..c.len() - 16], Tag::from_slice(&c[c.len() - 16..]));
                let opens = |is_last: bool, d: &mut [u8]| {
                    d.copy_from_slice(body);
                    self.opens(m, is_last, d, tag)
                };
                let authentic = match m == last {
                    true if opens(true, d) => true,
                    true if opens(false, d) => {
                        cut.store(true, std::sync::atomic::Ordering::Relaxed);
                        true
                    }
                    true => false,
                    false => opens(false, d),
                };
                if !authentic {
                    d.copy_from_slice(body);
                    self.unauthenticated(m, m == last, d);
                    failed.push(m);
                }
            }
            failed
        });
        (failed.into_iter().flatten().collect(), cut.into_inner())
    }

    /// Chunk `m` of sealed bytes `d` and tag `tag` (the last or not) decrypted in place: whether
    /// it authenticates (`d` as it was where not).
    ///
    /// Fuzzed (`cfg(fuzzing)`, as the checksums are skipped), a chunk that does not authenticate
    /// is decrypted without it, and taken to where its tag's first byte is even: a mutation of
    /// its bytes is then one of the payload's, at the same place, and the chunks taken not to
    /// authenticate are those of tags of an odd first byte.
    fn opens(&self, m: usize, last: bool, d: &mut [u8], tag: &Tag) -> bool {
        if self.cipher.decrypt_in_place_detached(&chunk_nonce(m as u64, last), &[], d, tag).is_ok() {
            return true;
        }
        if cfg!(fuzzing) && tag[0].is_multiple_of(2) {
            self.unauthenticated(m, last, d);
            return true;
        }
        false
    }

    /// Chunk `m` of sealed bytes `d` (the last or not) decrypted in place without its tag.
    fn unauthenticated(&self, m: usize, last: bool, d: &mut [u8]) {
        use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
        // (ChaCha20-Poly1305's keystream: from the second block, the first the MAC's key)
        let mut k = chacha20::ChaCha20::new(&self.key, &chunk_nonce(m as u64, last));
        k.seek(64u64);
        k.apply_keystream(d);
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
        // (zeroed pages of its own, mapped as they are touched, so that the chunks not read take
        // no memory, and given back as they are let go of: [`Decrypting::release`])
        let buf = aqr_common::pages::map_own(len)
            .ok()
            .and_then(std::ptr::NonNull::new)
            .unwrap_or_else(|| std::alloc::handle_alloc_error(std::alloc::Layout::array::<u8>(len.max(1)).unwrap()));
        let done = (0..self.chunks.len()).map(|_| Mutex::new(Chunk::Not)).collect();
        let decrypted = std::sync::atomic::AtomicUsize::new(0);
        Decrypting { unlocked: self, buf, len, done, decrypted, let_go: false }
    }
}

/// Where a chunk of [`Decrypting`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Chunk {
    /// Not decrypted (or let go of since).
    Not,
    Authentic,
    /// Decrypted, but it does not authenticate.
    Bad,
}

/// The payload of an encrypted archive, decrypted a chunk at a time as its bytes are first read
/// ([`Decrypting::get`]), each chunk authenticated: a file read alone decrypts its blocks' chunks
/// and those of the blocks that tell where they are, not the whole archive. Chunks read may be
/// let go of ([`Decrypting::release`]), to be decrypted again where they are read again.
pub struct Decrypting<'c> {
    unlocked: Unlocked<'c>,
    /// The payload's bytes, of each chunk once it is decrypted ([`Decrypting::done`]): pages of
    /// its own ([`aqr_common::pages::map_own`]).
    buf: std::ptr::NonNull<u8>,
    len: usize,
    /// By chunk: whether it is decrypted, and authenticates.
    done: Vec<Mutex<Chunk>>,
    /// The chunks decrypted so far (again where they were let go of).
    decrypted: std::sync::atomic::AtomicUsize,
    /// Whether the encrypted chunks' pages are let go of as they are decrypted
    /// ([`Decrypting::let_go_read`]).
    let_go: bool,
}

// SAFETY: the bytes of a chunk are written by the one holding its lock in `done`, while it is not
// decrypted: before any reference to them is made, or after those made are no longer used (as
// `release` is told); otherwise only read.
unsafe impl Send for Decrypting<'_> {}
unsafe impl Sync for Decrypting<'_> {}

impl Drop for Decrypting<'_> {
    fn drop(&mut self) {
        // SAFETY: mapped so in `Unlocked::decrypting`, no reference to it left
        unsafe { aqr_common::pages::unmap_own(self.buf.as_ptr(), self.len) };
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

    /// Lets go of the pages of the encrypted chunks as they are decrypted.
    ///
    /// # Safety
    ///
    /// The archive unlocked is a mapping, read only, of a file (pages let go of read as zeros
    /// otherwise).
    pub unsafe fn let_go_read(&mut self) {
        self.let_go = true;
    }

    /// Chunk `m` decrypted where it is not yet: whether it authenticates.
    fn chunk(&self, m: usize) -> bool {
        let mut s = self.done[m].lock().unwrap();
        if *s == Chunk::Not {
            let c = self.unlocked.chunks[m];
            let n = c.len() - 16;
            // SAFETY: the bytes of chunk `m`, within the mapping; written under its lock alone,
            // with no reference to them in use (none made yet, or let go of)
            let d = unsafe { std::slice::from_raw_parts_mut(self.buf.as_ptr().add(m * CHUNK), n) };
            d.copy_from_slice(&c[..n]);
            self.decrypted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tag = Tag::from_slice(&c[n..]);
            let last = m + 1 == self.done.len();
            let ok = self.unlocked.opens(m, last, d, tag);
            if self.let_go {
                // SAFETY: of a file mapped read only (as `let_go_read` was told)
                unsafe { aqr_common::pages::let_go(c.as_ptr(), c.len()) };
            }
            *s = if ok { Chunk::Authentic } else { Chunk::Bad };
        }
        *s == Chunk::Authentic
    }

    /// Lets go of the chunks wholly within bytes `r` of the payload: their pages given back, each
    /// decrypted again where it is read again.
    ///
    /// # Safety
    ///
    /// No reference to the bytes of those chunks ([`Decrypting::get`]'s, [`Decrypting::unread`]'s)
    /// is used after, until they are got again.
    pub unsafe fn release(&self, r: std::ops::Range<usize>) {
        let first = r.start.div_ceil(CHUNK);
        let end = if r.end >= self.len { self.done.len() } else { r.end / CHUNK };
        for m in first..end {
            let mut s = self.done[m].lock().unwrap();
            if *s != Chunk::Not {
                let n = CHUNK.min(self.len - m * CHUNK);
                // SAFETY: the chunk's pages, of the mapping of its own; not read until written
                // again (as the caller tells), under its lock
                unsafe { aqr_common::pages::forget(self.buf.as_ptr().add(m * CHUNK), n) };
                *s = Chunk::Not;
            }
        }
    }

    /// The chunks decrypted and not let go of since.
    #[cfg(test)]
    fn held(&self) -> usize {
        self.done.iter().filter(|c| *c.lock().unwrap() != Chunk::Not).count()
    }

    /// Where bytes `d` are in the payload, where they are of it.
    pub fn range_of(&self, d: &[u8]) -> Option<std::ops::Range<usize>> {
        let at = (d.as_ptr() as usize).checked_sub(self.buf.as_ptr() as usize)?;
        (at + d.len() <= self.len).then_some(at..at + d.len())
    }

    /// All of the payload's bytes, whether decrypted or not.
    ///
    /// # Safety
    ///
    /// No bytes of it are read but those got ([`Decrypting::get`]) and not let go of since.
    pub unsafe fn unread(&self) -> &[u8] {
        // SAFETY: of the mapping, of `len` bytes (as the caller reads them)
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr(), self.len) }
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
    use aqr_formula::Formula;

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

    /// The strict opener (no damage tolerated): a payload cut at the bound of a chunk, chunks
    /// swapped, doubled or dropped, and work factors out of 12..=22 are refused, never opened
    /// as another payload.
    #[test]
    fn refuses_rearranged_chunks() {
        let a = archive(3 * CHUNK);
        let invalid = |d: &[u8], what: &str| {
            let r = opened(d, b"pass");
            assert!(matches!(r, Err(Failure::Invalid(_))), "{what}: {:?}", r.map(|p| p.len()));
        };
        for mirrored in [false, true] {
            let c = sealed(&a, b"pass", mirrored);
            let chunks = open(&c).unwrap().chunks() as u64;
            assert!(chunks >= 4);
            // cut where a chunk (with its mirrored header) starts, and a byte or a tag's width after
            for m in 1..chunks {
                let at = chunk_with_mirror_at(m, mirrored) as usize;
                for cut in [at, at + 1, at + 16, at + 17, chunk_at(m, mirrored) as usize] {
                    invalid(&c[..cut], &format!("mirrored {mirrored}: cut at chunk {m} + {}", cut - at));
                }
            }
        }
        // without mirrors, the chunks are at regular places
        let c = sealed(&a, b"pass", false);
        let h = header_len(false);
        let chunk = |m: usize| h + m * SEALED..(h + (m + 1) * SEALED).min(c.len());
        let n = open(&c).unwrap().chunks();
        let with = |order: &[usize]| {
            let mut d = c[..h].to_vec();
            order.iter().for_each(|&m| d.extend_from_slice(&c[chunk(m)]));
            d
        };
        assert_eq!(opened(&with(&(0..n).collect::<Vec<_>>()), b"pass").unwrap().len(), a.len());
        invalid(&with(&[1, 0, 2, 3]), "the first two swapped");
        invalid(&with(&[0, 2, 1, 3]), "the middle two swapped");
        invalid(&with(&[0, 1, 2, 3, 2, 3]), "the tail twice");
        invalid(&with(&[0, 1, 1, 2, 3]), "a chunk doubled");
        invalid(&with(&[0, 2, 3]), "one dropped");
        invalid(&with(&[1, 2, 3]), "the first dropped");
        invalid(&with(&[0, 1, 2]), "the last dropped");
        invalid(&with(&[0, 1, 2, 2]), "the last chunk's place taken by the one before");
        invalid(&with(&[3, 1, 2, 0]), "the first and the last swapped");
        // the header alone has no payload
        assert!(matches!(open(&c[..h]).map(|_| ()), Err(Failure::Invalid(_))));
        // work factors: under 12, over the largest known, over the limit
        for wf in [0u8, 1, 11, 0x3f, 0x40, 0xff] {
            for mirrored in [false, true] {
                let mut d = sealed(&a, b"pass", mirrored);
                d[8] = wf;
                if mirrored {
                    // (the checksum of the header made again)
                    let sum = xxh64(&d[..header_len(true) - 8], 0).to_le_bytes();
                    let h = header_len(true);
                    d[h - 8..h].copy_from_slice(&sum);
                }
                assert!(matches!(open(&d).map(|_| ()), Err(Failure::Invalid(m)) if m == "bad work factor"), "{wf}");
            }
        }
        for wf in [WORK_FACTOR_LIMIT + 1, 0x3e] {
            let mut d = c.clone();
            d[8] = wf;
            assert!(matches!(open(&d).map(|_| ()), Err(Failure::Resource(_))), "{wf}");
        }
        // and one sealed by the least work factor is opened
        let mut s = Sealer::new(io::Cursor::new(Vec::new()), b"pass", 12, false).unwrap();
        s.write_all(&a).unwrap();
        let d = s.into_inner().unwrap().into_inner();
        assert_eq!(open(&d).unwrap().work_factor(), 12);
        assert_eq!(opened(&d, b"pass").unwrap().len(), a.len());
    }

    /// An archive of a file sealed, opened decrypting its data blocks as they are read
    /// ([`super::super::open_decrypting`]): the file given back, by its blocks and a module's
    /// applications, the chunks of the data blocks let go of once read, all but those a block
    /// shares with the next (and its head); and again, the chunks decrypted again.
    #[test]
    fn opened_decrypting() {
        use super::super::open_decrypting;
        use super::super::tests::{REV, natives};
        use aqr_formula::Node;
        let parts: Vec<Vec<u8>> = (0..12).map(|k| aqr_common::rng::noise((1 << 20) + k)).collect();
        let mut f = Formula::default();
        let mut nodes = Vec::new();
        for (k, p) in parts.iter().enumerate() {
            nodes.push(match k % 3 {
                2 => {
                    let s = f.push("r", p.iter().rev().copied().collect::<Vec<u8>>());
                    Node::apply(REV, vec![1, 2, 3], vec![s], p.len() as u64)
                }
                _ => f.push("p", p.clone()),
            });
        }
        f.root = Node::concat(nodes);
        let want = parts.concat();
        let a = Encoder::new(Level::Zstd(1)).with_size_hint(true).encode(&f, None).unwrap();
        let c = sealed(&a, b"p", true);
        let d = open(&c).unwrap().unlock(b"p").unwrap().decrypting();
        let natives = natives();
        for _ in 0..2 {
            let archive = open_decrypting(&d, &natives).unwrap();
            assert!(archive.parsed().unwrap().non_leaf.len() > 4);
            let mut got = Vec::new();
            let n = archive
                .write_file(2, &mut |b: &[u8]| {
                    got.extend_from_slice(b);
                    Ok(())
                })
                .unwrap();
            assert!(n == want.len() as u64 && got == want);
            assert!(d.held() * 4 < d.chunks(), "{} of {} chunks held", d.held(), d.chunks());
        }
        assert!(d.decrypted() > d.chunks(), "decrypted again");
    }

    /// An encrypted archive of 24 files (of 3 chunks or so each: in parcels of their own, two by
    /// two of a stream they share, or of a module's application), sealed: its contents as
    /// they were, and those taken alone of a few chunks of it decrypted, not all.
    #[test]
    fn decrypted_as_read() {
        use super::super::tests::{REV, natives};
        use super::super::{ContentAt, Entry, EntryNode, Extra, Leaf, Payload, SegmentWriter, Tree, open_indexed};
        use aqr_formula::Node;
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
            a.extend_from_slice(batch.blocks());
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
            let (failed, cut) = u.decrypt_lossy(&mut d, 2);
            let ranges: Vec<_> = failed.iter().map(|&m| u.chunk_range(m)).collect();
            (d, (failed, cut), o, ranges)
        };
        let (d, failed, o, _) = lossy(&c);
        assert!(d == a && failed == (vec![], false) && o == Opened::default());
        // cut at the end of a chunk: all of it authenticates, the last as not the last, told
        let (d, failed, o, _) = lossy(&c[..chunk_at(5, true) as usize]);
        assert!(!o.cut && failed == (vec![], true) && d[..] == a[..5 * CHUNK]);
        // (chunk 2: after the mirrored headers before chunks 1 and 3)
        let at = chunk_at(2, true) as usize + 1000;
        let mut x = c.clone();
        x[at] ^= 0x40;
        let (d, failed, _, ranges) = lossy(&x);
        assert!(failed == (vec![2], false) && ranges.len() == 1 && ranges[0] == (2 * CHUNK..3 * CHUNK));
        let diff: Vec<usize> = (0..a.len()).filter(|&i| d[i] != a[i]).collect();
        assert_eq!(diff, [2 * CHUNK + 1000]);
        // the header magic and its checksum damaged: from the first mirrored header
        let mut x = c.clone();
        x[0] ^= 1;
        x[50] ^= 1;
        assert!(is_damaged_encrypted(&x) && !is_damaged_encrypted(&c));
        let (d, failed, o, _) = lossy(&x);
        assert!(d == a && failed == (vec![], false) && o.header_from == Some(1));
        // cut in the last chunk: what is left of it, too short, left out, the chunk before it
        // taken as the last
        let cut = chunk_at(5, true) as usize + 10;
        let (d, failed, o, _) = lossy(&c[..cut]);
        assert!(o.cut && failed == (vec![], true) && d[..] == a[..5 * CHUNK]);
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

    /// A file of bytes in memory, written from a position (a sealer's output) and at offsets
    /// (its chunks placed), both at once.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>, u64);

    impl Shared {
        fn put(&self, at: u64, d: &[u8]) {
            let mut v = self.0.lock().unwrap();
            let (at, end) = (at as usize, at as usize + d.len());
            if v.len() < end {
                v.resize(end, 0);
            }
            v[at..end].copy_from_slice(d);
        }
    }

    impl Write for Shared {
        fn write(&mut self, d: &[u8]) -> io::Result<usize> {
            self.put(self.1, d);
            self.1 += d.len() as u64;
            Ok(d.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for Shared {
        fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
            self.1 = match p {
                SeekFrom::Start(n) => n,
                SeekFrom::Current(n) => self.1.checked_add_signed(n).unwrap(),
                SeekFrom::End(n) => (self.0.lock().unwrap().len() as u64).checked_add_signed(n).unwrap(),
            };
            Ok(self.1)
        }
    }

    impl At for Shared {
        fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
            self.put(at, d);
            Ok(())
        }

        fn read_at(&self, at: u64, d: &mut [u8]) -> io::Result<()> {
            let v = self.0.lock().unwrap();
            let s = v.get(at as usize..at as usize + d.len()).ok_or(io::ErrorKind::UnexpectedEof)?;
            d.copy_from_slice(s);
            Ok(())
        }
    }

    /// An archive written at offsets ([`Sealer::place`]): the bytes after a room at its start in
    /// pieces from the end on, maybe written nearer the start and moved back to their place
    /// ([`Placed::moved_back`], under a new nonce), then the room, then the last bytes from the
    /// start on ([`Sealer::placed`]). It opens to the archive as sealed from its start does, mirrored
    /// or not; a chunk sealed is not written again.
    #[test]
    fn placed_at_offsets() {
        use super::super::sink::Place;
        for (n, mirrored) in [(100, false), (CHUNK - 30, true), (3 * CHUNK, false), (70 * CHUNK + 3, true)] {
            let a = archive(n);
            let room = a.len() / 3;
            for (by, after) in [(0, 0), (0, 37), (room / 2, 0), (CHUNK + 9, 37)] {
                if by >= room || after >= a.len() - room {
                    continue;
                }
                let out = Shared::default();
                let mut s = Sealer::new(out.clone(), b"p", 12, mirrored).unwrap();
                let p = s.place(Arc::new(out.clone())).unwrap();
                assert!(s.place(Arc::new(out.clone())).is_none());
                let end = a.len() - after;
                let mut at = end;
                for k in (1..).map(|i| i * 7919 % 100_003) {
                    let from = at.saturating_sub(k).max(room);
                    p.write_at((from - by) as u64, &a[from..at]).unwrap();
                    at = from;
                    if at == room {
                        break;
                    }
                }
                if by > 0 {
                    let old = s.fields.nonce().to_vec();
                    p.moved_back((room - by) as u64, (end - by) as u64, by as u64).unwrap();
                    assert!(p.state.lock().unwrap().fields.nonce() != old);
                }
                if n > 4 * CHUNK {
                    let sealed = (room / CHUNK + 2) * CHUNK;
                    assert!(p.write_at(sealed as u64 + 1, &[0]).is_err(), "a chunk sealed written again");
                }
                p.write_at(0, &a[..room]).unwrap();
                s.placed(end as u64).unwrap();
                s.write_all(&a[end..]).unwrap();
                s.finish().unwrap();
                let c = out.0.lock().unwrap().clone();
                let want = opened(&sealed(&a, b"p", mirrored), b"p").unwrap();
                assert!(opened(&c, b"p").unwrap() == want, "{n} bytes, {by} moved, {after} after");
            }
        }
    }

    /// Bytes missing as a sealer placed goes on fail.
    #[test]
    fn placed_whole() {
        use super::super::sink::Place;
        let a = archive(3 * CHUNK);
        let out = Shared::default();
        let mut s = Sealer::new(out.clone(), b"p", 12, true).unwrap();
        let p = s.place(Arc::new(out.clone())).unwrap();
        p.write_at(0, &a[..CHUNK]).unwrap();
        p.write_at(CHUNK as u64 + 1, &a[CHUNK + 1..]).unwrap();
        assert!(s.placed(a.len() as u64).is_err());
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
