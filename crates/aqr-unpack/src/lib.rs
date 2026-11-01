//! Archives of other formats than Aqueous read for extraction: ZIP, 7z, RAR, ALZ, EGG and tar (alone, or under
//! gzip, bzip2, xz, LZMA or Zstandard), and those compressors' streams of a single file. An
//! archive is read into a [`Tree`] as Aqueous's own are (the metadata it keeps made the tree's:
//! times, permissions, ownership, attributes, extended attributes, links), told to a [`Sink`]
//! as it is known, with the contents of its files as they are decoded, on the pool's threads:
//! the program extracting Aqueous archives extracts these the same way.
//!
//! What cannot be extracted safely or as the archive has it (paths reaching out of the
//! directory, encrypted entries without their passphrase, methods not known) is left out and
//! told ([`Sink::note`]). Encrypted ZIP entries and 7z folders (and headers) are decrypted by
//! the passphrase taken by [`Opened::unlock`].

use aqr_formula::aqr::Step;
pub use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree};

mod alzip;
mod build;
mod codec;
mod crypt;
mod lzma;
mod rar;
mod sevenz;
mod stream;
mod sys;
mod tar;
mod zip;

/// A compressor of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Zstd,
}

/// What an archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    SevenZip,
    /// A RAR archive: RAR5, or RAR 1.5–4.x.
    Rar,
    /// An ALZ archive (ALZip's).
    Alz,
    /// An EGG archive (ALZip's).
    Egg,
    /// A tar archive, under a compressor where given.
    Tar(Option<Filter>),
    /// A compressor's stream of a single file.
    Single(Filter),
}

/// The format of an archive by its name (case aside), and the bytes of the name that tell it:
/// what is left of the name names what is extracted.
pub fn by_name(name: &[u8]) -> Option<(Format, usize)> {
    use Filter::*;
    use Format::*;
    const NAMES: &[(&str, Format)] = &[
        (".tar.gz", Tar(Some(Gzip))),
        (".tgz", Tar(Some(Gzip))),
        (".taz", Tar(Some(Gzip))),
        (".tar.bz2", Tar(Some(Bzip2))),
        (".tbz2", Tar(Some(Bzip2))),
        (".tbz", Tar(Some(Bzip2))),
        (".tb2", Tar(Some(Bzip2))),
        (".tar.xz", Tar(Some(Xz))),
        (".txz", Tar(Some(Xz))),
        (".tar.lzma", Tar(Some(Lzma))),
        (".tlz", Tar(Some(Lzma))),
        (".tar.zst", Tar(Some(Zstd))),
        (".tzst", Tar(Some(Zstd))),
        (".tar", Tar(None)),
        (".zip", Zip),
        (".jar", Zip),
        (".apk", Zip),
        (".xpi", Zip),
        (".cbz", Zip),
        (".whl", Zip),
        (".7z", SevenZip),
        (".cb7", SevenZip),
        (".rar", Rar),
        (".cbr", Rar),
        (".alz", Alz),
        (".egg", Egg),
        (".gz", Single(Gzip)),
        (".bz2", Single(Bzip2)),
        (".xz", Single(Xz)),
        (".lzma", Single(Lzma)),
        (".zst", Single(Zstd)),
    ];
    let lower = name.to_ascii_lowercase();
    NAMES.iter().find(|(s, _)| lower.len() > s.len() && lower.ends_with(s.as_bytes())).map(|&(s, f)| (f, s.len()))
}

/// What an archive is extracted to: its tree as it is known and the contents of its files, as
/// they are decoded.
pub trait Sink: Sync {
    /// Entries `new` of `t` (new ones, or ones told before and changed since: to be made again)
    /// are known, each after its parent: told before the contents of their files. Never at
    /// once with [`Sink::content`].
    fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String>;
    /// The content `c` of the files told (on any thread).
    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String>;
    /// What of the archive is not extracted, or not as it has it.
    fn note(&self, what: &str);
    /// How far it is: the units of [`Opened::units`] started and ended, and the bytes of the
    /// archive decompressed and of the files made.
    fn step(&self, _: Step) {}
}

/// An archive extracted: its tree (whole), and the entries left out (told to [`Sink::note`]).
pub struct Done {
    pub tree: Tree,
    pub skipped: usize,
}

/// An archive opened ([`Opened::open`]): its listing read, where it has one.
pub struct Opened<'d> {
    d: &'d [u8],
    inner: Inner<'d>,
}

enum Inner<'d> {
    Zip(zip::Listing<'d>),
    SevenZip(sevenz::Listing<'d>),
    /// A 7z archive whose header is encrypted, read once a passphrase decrypts it.
    SevenZipSealed,
    Rar(rar::Listing<'d>),
    Alz(alzip::Listing<'d>),
    Egg(alzip::Listing<'d>),
    Tar(Option<Filter>),
    Single(Filter),
}

impl<'d> Opened<'d> {
    /// Archive `d` of format `f`: a ZIP, 7z, RAR, ALZ or EGG archive's listing read (its
    /// directory, its header, its headers, its records).
    pub fn open(f: Format, d: &'d [u8]) -> Result<Opened<'d>, String> {
        let inner = match f {
            Format::Zip => Inner::Zip(zip::Listing::read(d)?),
            Format::SevenZip => match sevenz::Listing::read(d, None)? {
                Some(l) => Inner::SevenZip(l),
                None => Inner::SevenZipSealed,
            },
            Format::Rar => Inner::Rar(rar::Listing::read(d)?),
            Format::Alz => Inner::Alz(alzip::Listing::read_alz(d)?),
            Format::Egg => Inner::Egg(alzip::Listing::read_egg(d)?),
            Format::Tar(f) => Inner::Tar(f),
            Format::Single(f) => Inner::Single(f),
        };
        Ok(Opened { d, inner })
    }

    /// Whether it is encrypted, in part (its entries) or whole (its listing too): a passphrase
    /// to give it ([`Opened::unlock`]).
    pub fn encrypted(&self) -> bool {
        match &self.inner {
            Inner::Zip(l) => l.encrypted(),
            Inner::SevenZip(l) => l.encrypted(),
            Inner::SevenZipSealed => true,
            _ => false,
        }
    }

    /// Whether its listing is encrypted and not decrypted yet: it fails to unpack.
    pub fn sealed(&self) -> bool {
        matches!(self.inner, Inner::SevenZipSealed)
    }

    /// Whether `pass` decrypts it, taken for it where it does (as far as can be told without
    /// decoding it all: an entry not decoding by it is left out, told). Without one, the
    /// entries encrypted are left out, and an archive encrypted whole fails.
    pub fn unlock(&mut self, pass: &[u8]) -> Result<bool, String> {
        match &mut self.inner {
            Inner::Zip(l) => Ok(l.unlock(pass)),
            Inner::SevenZip(l) => Ok(l.unlock(pass)),
            Inner::SevenZipSealed => match sevenz::Listing::read(self.d, Some(pass))? {
                Some(l) => {
                    self.inner = Inner::SevenZip(l);
                    Ok(true)
                }
                None => Ok(false),
            },
            _ => Ok(false),
        }
    }

    /// Whether it is a tree of entries (else a single file: [`Opened::decompress`]).
    pub fn is_tree(&self) -> bool {
        !matches!(self.inner, Inner::Single(_))
    }

    /// The units it is decoded by, as [`Sink::step`] tells them: what each is shown as.
    pub fn units(&self) -> Vec<String> {
        match &self.inner {
            Inner::Zip(l) => l.units(),
            Inner::SevenZip(l) => l.units(),
            Inner::SevenZipSealed => Vec::new(),
            Inner::Rar(l) => l.units(),
            Inner::Alz(l) | Inner::Egg(l) => l.units(),
            Inner::Tar(_) | Inner::Single(_) => vec![String::new()],
        }
    }

    /// Extracts the tree to `sink`, on up to `threads` threads.
    pub fn unpack(&self, threads: usize, sink: &dyn Sink) -> Result<Done, String> {
        match &self.inner {
            Inner::Zip(l) => l.unpack(threads, sink),
            Inner::SevenZip(l) => l.unpack(threads, sink),
            Inner::SevenZipSealed => Err("7z: its header encrypted, and no passphrase given".into()),
            Inner::Rar(l) => l.unpack(threads, sink),
            Inner::Alz(l) => l.unpack("ALZ", threads, sink),
            Inner::Egg(l) => l.unpack("EGG", threads, sink),
            &Inner::Tar(f) => tar::unpack(f, self.d, threads, sink),
            Inner::Single(_) => Err("a single file, not a tree".into()),
        }
    }

    /// Decompresses a single file, giving its data to `write` as it is decoded; returns its
    /// bytes.
    pub fn decompress(
        &self,
        threads: usize,
        write: &mut dyn FnMut(&[u8]) -> Result<(), String>,
        step: &dyn Fn(Step),
    ) -> Result<u64, String> {
        let &Inner::Single(f) = &self.inner else { return Err("not a single file".into()) };
        let total = stream::estimate(Some(f), self.d);
        let (mut n, mut failed) = (0u64, None);
        step(Step::Started(0));
        stream::decode(Some(f), self.d, threads, &mut |v, start| {
            n += (v.len() - start) as u64;
            step(stream::reached(n, total, self.d.len() as u64));
            match write(&v[start..]) {
                Ok(()) => true,
                Err(e) => {
                    failed = Some(e);
                    false
                }
            }
        })?;
        step(Step::Ended(0));
        failed.map_or(Ok(n), Err)
    }
}
