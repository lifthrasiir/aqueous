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
//!
//! Some paths of an archive may be extracted alone ([`Opened::select`]): what is decoded then
//! is what they need. Of an archive of a listing (ZIP, 7z, RAR, ALZ, EGG), the units holding
//! none of their files are not decoded, and a solid one only as far as the last of them (7z's
//! LZMA2 parts and EGG's pieces of none of them not at all); a tar archive is read through, as
//! its listing is not known before, but the bodies of the members not selected are not kept.

pub use aqr_foreign::charset::Choice;
use aqr_formula::aqr::Step;
pub use aqr_formula::aqr::{Entry, EntryNode, Extra, Time, Tree};
pub use build::Select;

mod alzip;
mod build;
mod codec;
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

/// The notes of a listing told to `sink`, but those of entries not selected by `s`: how many.
fn tell(notes: &[build::Note], s: Option<&Select>, sink: &dyn Sink) -> usize {
    let told = notes.iter().filter(|n| s.is_none_or(|s| s.wants(&n.0)));
    told.map(|n| sink.note(&n.1)).count()
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
    select: Option<Select>,
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
        Opened::open_with(f, d, &Choice::Auto)
    }

    /// As [`Opened::open`], its names of no declared encoding (ZIP's, ALZ's, EGG's and RAR
    /// 1.5–4.x's of none) taken as `names` asks (`docs/names-design.md`).
    pub fn open_with(f: Format, d: &'d [u8], names: &Choice) -> Result<Opened<'d>, String> {
        let inner = match f {
            Format::Zip => Inner::Zip(zip::Listing::read(d, names)?),
            Format::SevenZip => match sevenz::Listing::read(d, None)? {
                Some(l) => Inner::SevenZip(l),
                None => Inner::SevenZipSealed,
            },
            Format::Rar => Inner::Rar(rar::Listing::read(d, names)?),
            Format::Alz => Inner::Alz(alzip::Listing::read_alz(d, names)?),
            Format::Egg => Inner::Egg(alzip::Listing::read_egg(d, names)?),
            Format::Tar(f) => Inner::Tar(f),
            Format::Single(f) => Inner::Single(f),
        };
        Ok(Opened { d, inner, select: None })
    }

    /// Has only the entries of `s` extracted (and what is under them); what they need alone
    /// decoded ([module](self)). The tree told is the whole archive's still (a tar archive's,
    /// what is selected), the contents only those selected.
    pub fn select(&mut self, s: Select) {
        self.select = Some(s);
    }

    /// What its listing tells of the entries of `s` (or under them) left out of the tree, as
    /// it would tell it extracting them (not of a tar archive: told as it is read).
    pub fn left_out(&self, s: &Select) -> Vec<String> {
        let (notes, sealed) = match &self.inner {
            Inner::Zip(l) => (l.notes(), l.sealed_notes()),
            Inner::SevenZip(l) => (l.notes(), Vec::new()),
            Inner::Rar(l) => (l.notes(), Vec::new()),
            Inner::Alz(l) | Inner::Egg(l) => (l.notes(), Vec::new()),
            Inner::SevenZipSealed | Inner::Tar(_) | Inner::Single(_) => return Vec::new(),
        };
        let wanted = |n: &build::Note| !n.0.is_empty() && s.wants(&n.0);
        notes.iter().chain(&sealed).filter(|n| wanted(n)).map(|n| n.1.clone()).collect()
    }

    /// The tree as its listing has it, where it has one read already (not of a tar archive, nor
    /// of a 7z archive whose header is not decrypted yet).
    pub fn tree(&self) -> Option<&Tree> {
        match &self.inner {
            Inner::Zip(l) => Some(l.tree()),
            Inner::SevenZip(l) => Some(l.tree()),
            Inner::Rar(l) => Some(l.tree()),
            Inner::Alz(l) | Inner::Egg(l) => Some(l.tree()),
            Inner::SevenZipSealed | Inner::Tar(_) | Inner::Single(_) => None,
        }
    }

    /// What is told of how its names of no declared encoding were taken, where it was guessed
    /// (`build::Undeclared::told`).
    pub fn names_told(&self) -> Option<String> {
        match &self.inner {
            Inner::Zip(l) => l.undeclared.told("ZIP"),
            Inner::Rar(l) => l.undeclared.told("RAR"),
            Inner::Alz(l) => l.undeclared.told("ALZ"),
            Inner::Egg(l) => l.undeclared.told("EGG"),
            _ => None,
        }
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
        let s = self.select.as_ref();
        match &self.inner {
            Inner::Zip(l) => l.unpack(threads, sink, s),
            Inner::SevenZip(l) => l.unpack(threads, sink, s),
            Inner::SevenZipSealed => Err("7z: its header encrypted, and no passphrase given".into()),
            Inner::Rar(l) => l.unpack(threads, sink, s),
            Inner::Alz(l) => l.unpack("ALZ", threads, sink, s),
            Inner::Egg(l) => l.unpack("EGG", threads, sink, s),
            &Inner::Tar(f) => tar::unpack(f, self.d, threads, sink, s),
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

/// What the readers' tests of selected paths take: a [`Sink`] keeping the contents given.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::{EntryNode, Select, Sink, Tree};

    #[derive(Default)]
    pub struct Kept(pub Mutex<BTreeMap<usize, Vec<u8>>>);

    impl Sink for Kept {
        fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
            Ok(())
        }

        fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
            self.0.lock().unwrap().insert(c, d);
            Ok(())
        }

        fn note(&self, _: &str) {}
    }

    /// The selection of the file of content `c` of `t`.
    pub fn file_of(t: &Tree, c: usize) -> Select {
        let mut i = t.entries.iter().position(|e| e.node == EntryNode::File(c)).expect("a file of it");
        let mut names = vec![t.entries[i].name.clone()];
        while let Some(p) = t.entries[i].parent {
            names.push(t.entries[p].name.clone());
            i = p;
        }
        names.reverse();
        Select::new(vec![names])
    }
}
