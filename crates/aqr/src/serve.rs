//! The archives browsed (`--ipc`'s requests `open`, `list`, `stat`, `read`, `close`, `journal`,
//! `progress`, `trim`; `man/aqripc.7`): a shell extension's (through `aqr-win --serve`), a
//! mount's (aqr-mount). Each request is served on the reader's thread, but those that evaluate or
//! map an archive (`open`, `read`), which have one of their own.
//!
//! ```text
//! > {"q":1,"op":"open","path":"D:\\x.aqr"}
//! < {"r":1,"archive":7,"identity":"...","single":false}
//! > {"q":2,"op":"list","archive":7,"dir":"a/b"}
//! < {"r":2,"entries":[{"name":"c.txt","kind":"file","size":123,"mtime":[1700000000,0]},...]}
//! > {"q":3,"op":"read","archive":7,"path":"a/b/c.txt","offset":0,"len":3000000}
//! < {"r":3,"more":true,"blob":1048576}   then the 1048576 bytes as they are, and so on
//! > {"q":4,"op":"close","archive":7}
//! < {"r":4,"closed":7}
//! ```
//!
//! - `open` (`path`): the archive opened, by its metadata blocks alone, as a handle (`archive`)
//!   that the other requests name. `identity` tells the file opened (its volume and number, its
//!   size and modification time): the same path opened again while it is open, and the same
//!   file, is the same handle (closed as many times); a file there now that is not the one open
//!   is opened anew (`changed` true), the handle before staying of the file it was. `single`: an
//!   archive of a single file (no metadata block), whose one entry at the root is the name the
//!   archive is extracted as. An encrypted archive is opened of `passphrase` given with `open`
//!   (decrypted as it is read); without it, the error `encrypted`, and `wrong-passphrase` where
//!   the passphrase given does not open it.
//! - `list` (`archive`, and `dir`: names from the root joined with `/`, empty for the root): the
//!   entries of the directory, in replies of at most [`LISTED`] entries. An entry has its `name`
//!   (as `aqr -d` writes it: [`crate::names`]), its `kind` (`dir`, `file`, `link`, `other`) and
//!   what is known of it: a file's `size`, `mtime` (seconds and nanoseconds since the epoch;
//!   `local` true where it is a local time of an unknown time zone; `crtime`, `atime`, `ctime`
//!   alike), `attrs` (Windows' attributes), as `-t -v` shows them `owner`, `mode`, `win`, `mac`
//!   and `flags`, its `comment`, a link's `target`. With `verbose` true, a file's `data`,
//!   `overhead`, `share`, `ratio`, `segment`, `formula` and `nodes` (its instructions) too (as
//!   `-t -v`'s: all of the archive's blocks read for them, once).
//! - `stat` (`archive`, `path`): the entry at the path, as `entry` (the root's of no name).
//! - `read` (`archive`, `path`, `offset`, `len`): bytes of the file at `path` from `offset`, at
//!   most `len` (fewer only at the end of the file, none past it), as the blobs of the replies,
//!   each at most the hello's chunk. The first read of a file evaluates its leaf, all of whose
//!   files are kept ([`Cache`]) for the reads after. `cancel` of it stops it between replies.
//! - `close` (`archive`): the handle given back; once all of a file's are, the archive is let go
//!   of (the file no longer mapped) and its files kept dropped.
//! - `journal` (`archive`): its journal points (named versions, `--tag` and `-j`), in their
//!   order: `metadata` (what `open` takes as `at`), `time` (milliseconds since the epoch), `shown`
//!   (in local time, as `aqr` shows it) and `label`. `open` with `at` (a point's `metadata`)
//!   opens the archive as it was at that point, as `--at` does: a handle of its own.
//! - `progress` (`archive`): how far the evaluation of its leaves is (`decompressed` and
//!   `evaluated`, `[done, total]` bytes of what is evaluated at once, as the archive last told
//!   it: of the evaluations at work of several requests, any of them), and `evaluating`, how many
//!   leaves are.
//! - `trim`: the files kept dropped (`trimmed`: their bytes), as a frontend idle a while asks.
//!
//! An archive of another format that is of a tree (ZIP, 7z, RAR, ALZ, EGG, tar under its
//! compressors: [`aqr_unpack::by_name`]) is served as well, of no journal point nor what
//! `-t -v` tells of blocks: its entries as aqr-unpack reads them (by its listing; a tar
//! archive's by reading it all once, as it is opened), a file read decoding it alone (where its
//! format has it apart: of a tar archive, all of its files at once, kept as one leaf). An
//! encrypted one (ZIP's, 7z's) is opened of `passphrase` as an Aqueous archive is. `open` tells
//! of such an archive its `fields`, what its format may have of an entry ([`fields`]): the
//! others are never told (of an Aqueous archive, all may be: no `fields`).
//! A single file compressed (`.gz`, `.xz`, ...) is served as an archive of a single file, its
//! name without the compressor's extension.
//!
//! Errors are of the kinds `bad-request`, `no-archive`, `not-found`, `not-a-directory`,
//! `not-a-file`, `encrypted`, `wrong-passphrase`, `cancelled` and `failed`. What was spilled to
//! files is gone with the process.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use aqr_archive::{Archive, EntryNode, Extra, Zone, name_bytes};
use aqr_ipc::wire::{self, kind};
use serde_json::{Value, json};

use crate::names::Names;
use crate::{Args, Decoder, ipc};

/// The most entries an answer to `list` has.
pub const LISTED: usize = 4096;

/// The most bytes kept by default: an eighth of the system's memory, at most this.
const CACHE_MAX: u64 = 1 << 30;

aqr_common::zopt! {
    /// The most bytes of the files evaluated kept in memory ([`Cache`]); a file over it is
    /// spilled to a file of its own.
    static CACHE: Opt<Option<u64>> = Opt::value(
        "serve:cache",
        None,
        "with --serve, the most bytes of the files evaluated kept in memory (by default an eighth of the memory, at most 1 GiB)"
    );
}

/// What tells a file apart from another at the same path, or from itself changed: its volume and
/// its number on it, its size and its modification time (nanoseconds since the epoch).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Identity {
    volume: u64,
    file: u64,
    len: u64,
    mtime: i128,
}

impl Identity {
    fn of(f: &File) -> std::io::Result<Identity> {
        let m = f.metadata()?;
        let mtime = match m.modified()?.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        let (volume, file) = file_id(f, &m)?;
        Ok(Identity { volume, file, len: m.len(), mtime })
    }

    fn shown(&self) -> String {
        format!("{:x}:{:x}:{:x}:{:x}", self.volume, self.file, self.len, self.mtime)
    }
}

#[cfg(windows)]
fn file_id(f: &File, _: &std::fs::Metadata) -> std::io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    // SAFETY: a handle of a file open, and a structure of the call's to fill
    let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut i) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((i.dwVolumeSerialNumber as u64, (i.nFileIndexHigh as u64) << 32 | i.nFileIndexLow as u64))
}

#[cfg(unix)]
fn file_id(_: &File, m: &std::fs::Metadata) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((m.dev(), m.ino()))
}

#[cfg(not(any(windows, unix)))]
fn file_id(_: &File, _: &std::fs::Metadata) -> std::io::Result<(u64, u64)> {
    Ok((0, 0))
}

/// The modules run for every archive served: those of the archives' leaves evaluated.
static DECODER: OnceLock<Decoder> = OnceLock::new();

/// An archive open: its file mapped (for as long as it is open), and the archive of it.
struct Opened {
    handle: u64,
    path: PathBuf,
    identity: Identity,
    /// The journal point it is opened as of (the offset of its segment's metadata block), where
    /// it is not of all of its segments.
    at: Option<usize>,
    // (dropped before what it borrows: fields are dropped in their order)
    source: Source,
    _leaked: Leaked,
    view: View,
    /// How far the evaluation of its leaves is, as the archive last told it, and how many are
    /// being evaluated.
    progress: Arc<Progress>,
    /// What `-t -v` tells of its contents ([`aqr_archive::Listed`]), by content: all of its
    /// blocks read for it, once it is first asked for (`verbose`).
    listed: OnceLock<Result<Vec<aqr_archive::Listed>, String>>,
}

#[derive(Default)]
struct Progress {
    reached: Mutex<Option<aqr_archive::Reached>>,
    evaluating: std::sync::atomic::AtomicUsize,
}

/// What an archive open borrows, leaked for it to (its lifetime `'static`), and let go of after
/// it: the mapping of its file, and of an encrypted one, its payload decrypted as it is read.
struct Leaked {
    map: &'static memmap2::Mmap,
    sealed: Option<&'static aqr_archive::envelope::Decrypting<'static>>,
}

impl Drop for Leaked {
    fn drop(&mut self) {
        // SAFETY: leaked of boxes by `Opened::open`, borrowed by nothing left (the archive dropped
        // before), the payload (which borrows the mapping) before the mapping
        unsafe {
            if let Some(d) = self.sealed.take() {
                drop(Box::from_raw(
                    d as *const aqr_archive::envelope::Decrypting as *mut aqr_archive::envelope::Decrypting,
                ));
            }
            drop(Box::from_raw(self.map as *const memmap2::Mmap as *mut memmap2::Mmap));
        }
    }
}

/// What an archive open is: an Aqueous archive, or one of another format (aqr-unpack's).
#[allow(clippy::large_enum_variant, reason = "one for each archive open")]
enum Source {
    Aqr(Archive<'static, 'static>),
    Foreign(Foreign),
}

/// An archive of another format, of a tree.
struct Foreign {
    format: aqr_unpack::Format,
    opened: aqr_unpack::Opened<'static>,
    tree: aqr_archive::Tree,
    /// By content, its bytes, and of the archive's data (where its listing tells).
    sizes: Vec<Option<u64>>,
    packed: Vec<Option<u64>>,
    /// Whether it is decoded whole (a tar archive: no listing to find a file by), its files
    /// one leaf (0) of all of them, by content; else each file a leaf of its own (its content).
    whole: bool,
}

impl Progress {
    fn tell(&self, s: aqr_archive::Step) {
        match s {
            aqr_archive::Step::Reached(r) => *self.reached.lock().unwrap() = Some(r),
            aqr_archive::Step::Started(_) => _ = self.evaluating.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            aqr_archive::Step::Ended(_) => _ = self.evaluating.fetch_sub(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// What an entry of an archive of format `f` may have, of the fields an Aqueous archive's has
/// beyond its name, kind and size (as the frontend's columns are named): its times, Windows'
/// attributes, the permissions and owner of Unix, a link's target, and of a format compressing
/// its files apart (all but tar's) a file's `data` (its compressed bytes; of a solid block, its
/// share by its size) and `ratio`. The comment, macOS' flags, `-t`'s flags and what else
/// `-t -v` tells of blocks (`overhead`, `segment`) are Aqueous's alone.
fn fields(f: aqr_unpack::Format) -> &'static [&'static str] {
    use aqr_unpack::Format::*;
    match f {
        Zip => &["modified", "created", "accessed", "attributes", "mode", "owner", "target", "data", "ratio"],
        SevenZip | Rar => &["modified", "created", "accessed", "attributes", "mode", "target", "data", "ratio"],
        Alz => &["modified", "attributes", "data", "ratio"],
        Egg => &["modified", "attributes", "mode", "owner", "data", "ratio"],
        Tar(_) => &["modified", "accessed", "changed", "mode", "owner", "target"],
        Single(_) => &[],
    }
}

/// The view of tree `t`: its names as `aqr -d` writes them, and its directories' entries.
fn view_of(t: &aqr_archive::Tree, a: &Args) -> View {
    let names = Names::new(&a.names, false);
    names.decide(t);
    let names: Vec<String> = t.entries.iter().map(|e| names.name(&e.name).os.to_string_lossy().into_owned()).collect();
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    let mut by_name = HashMap::new();
    for (i, e) in t.entries.iter().enumerate() {
        children.entry(e.parent).or_default().push(i);
        // (of two names written the same, the first)
        by_name.entry((e.parent, names[i].clone())).or_insert(i);
    }
    View::Tree { names, children, by_name }
}

/// What a sink of aqr-unpack keeps of the contents it is given: those of `wanted` (none: all),
/// held as a leaf's are (spilled once what is in memory would pass `cap`).
struct Keeping<'p> {
    wanted: Option<usize>,
    cap: u64,
    held: Mutex<(HashMap<usize, Held>, u64, Option<String>)>,
    progress: &'p Progress,
}

impl aqr_unpack::Sink for Keeping<'_> {
    fn entries(&self, _: &aqr_archive::Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        if self.wanted.is_some_and(|w| w != c) {
            return Ok(());
        }
        let mut h = self.held.lock().unwrap();
        let n = d.len() as u64;
        let held = match h.1 + n <= self.cap {
            true => {
                h.1 += n;
                Held::Memory(d)
            }
            false => Held::of(d, 0).map_err(|e| format!("spilling: {e}"))?,
        };
        h.0.insert(c, held);
        Ok(())
    }

    fn note(&self, what: &str) {
        let mut h = self.held.lock().unwrap();
        if h.2.is_none() {
            h.2 = Some(what.to_owned());
        }
    }

    fn step(&self, s: aqr_archive::Step) {
        self.progress.tell(s);
    }
}

/// What an archive's entries are, as they are served.
enum View {
    /// An archive of a tree: each entry's name, and by directory (none: the root) its entries in
    /// order and by name.
    Tree {
        names: Vec<String>,
        children: HashMap<Option<usize>, Vec<usize>>,
        by_name: HashMap<(Option<usize>, String), usize>,
    },
    /// An archive of a single file, its name and size.
    Single { name: String, size: u64 },
}

/// What a path names in an archive.
#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Root,
    Entry(usize),
    /// The file of an archive of a single file.
    Single,
}

impl Opened {
    /// Archive `path` opened, as handle `handle`.
    fn open(handle: u64, path: &Path, passphrase: Option<&str>, at: Option<usize>, a: &Args) -> Result<Opened, Value> {
        let err = |e: &dyn std::fmt::Display| wire::error(kind::FAILED, format!("{}: {e}", path.display()));
        let file = File::open(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => wire::error(kind::NOT_FOUND, format!("{}: {e}", path.display())),
            _ => err(&e),
        })?;
        let identity = Identity::of(&file).map_err(|e| err(&e))?;
        // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
        drop(file);
        let encrypted = aqr_archive::envelope::is_encrypted(&map) || aqr_archive::envelope::is_damaged_encrypted(&map);
        let mut leaked = Leaked { map: Box::leak(Box::new(map)), sealed: None };
        let map = leaked.map;
        // (an encrypted one of the passphrase given: its payload decrypted as it is read)
        if encrypted {
            let Some(p) = passphrase else {
                return Err(wire::error(kind::ENCRYPTED, format!("{}: encrypted", path.display())));
            };
            let (e, _) = aqr_archive::envelope::open_damaged(map).map_err(|e| err(&e))?;
            match e.unlock(p.as_bytes()) {
                Ok(u) => leaked.sealed = Some(Box::leak(Box::new(u.decrypting()))),
                Err(aqr_archive::Failure::Decryption(_)) => {
                    let m = format!("{}: encrypted, and the passphrase does not open it", path.display());
                    return Err(wire::error(kind::WRONG_PASSPHRASE, m));
                }
                Err(f) => return Err(err(&f)),
            }
        }
        // (of another format: as aqr-unpack reads it)
        let name = path.file_name().map(crate::meta::os_bytes).unwrap_or_default();
        if let Some((f, _)) = aqr_unpack::by_name(&name) {
            let (source, view, progress) = Opened::foreign(f, map, path, passphrase, a)?;
            return Ok(Opened {
                handle,
                path: path.to_owned(),
                identity,
                at: None,
                source,
                _leaked: leaked,
                view,
                progress,
                listed: OnceLock::new(),
            });
        }
        // (from here, what is leaked let go of with the archive, or here where it does not open)
        let opened = (|| -> Result<_, Value> {
            let runner: &'static Decoder = DECODER.get().expect("made as serving begins");
            let (payload, bytes) = match leaked.sealed {
                Some(d) => (aqr_archive::Payload::Sealed(d), &[][..]),
                None => {
                    let bytes: &'static [u8] = crate::located(map, path, true);
                    (aqr_archive::Payload::Plain(bytes), bytes)
                }
            };
            let mut archive = match (payload.of_tree(), leaked.sealed) {
                (true, _) => match at {
                    Some(m) => aqr_archive::open_indexed_upto(payload, runner, a.opt.threads, m),
                    None => aqr_archive::open_indexed(payload, runner, a.opt.threads),
                },
                (false, Some(d)) => aqr_archive::open_decrypting(d, runner),
                (false, None) => aqr_archive::open(bytes, runner),
            }
            .map_err(|e| err(&e))?;
            archive.set_budget(aqr_common::budget::global());
            let view = match archive.tree() {
                Some(t) => view_of(t, a),
                None => {
                    let size = archive.listing(&|_| None).map_err(|e| err(&e))?.first().map_or(0, |l| l.size);
                    View::Single { name: single_name(path), size }
                }
            };
            let progress = Arc::new(Progress::default());
            let told = progress.clone();
            archive.on_progress(Some(Arc::new(move |s| told.tell(s))));
            Ok((archive, view, progress))
        })();
        let (archive, view, progress) = opened?;
        Ok(Opened {
            handle,
            path: path.to_owned(),
            identity,
            at,
            source: Source::Aqr(archive),
            _leaked: leaked,
            view,
            progress,
            listed: OnceLock::new(),
        })
    }

    /// Archive `map` (at `path`) of format `f`, of another format: its tree as its listing has
    /// it, or as it is read whole (a tar archive's).
    fn foreign(
        f: aqr_unpack::Format,
        map: &'static [u8],
        path: &Path,
        passphrase: Option<&str>,
        a: &Args,
    ) -> Result<(Source, View, Arc<Progress>), Value> {
        let err = |e: &dyn std::fmt::Display| wire::error(kind::FAILED, format!("{}: {e}", path.display()));
        let mut opened = aqr_unpack::Opened::open_with(f, map, &a.names.encoding).map_err(|e| err(&e))?;
        if opened.encrypted() {
            let Some(p) = passphrase else {
                return Err(wire::error(kind::ENCRYPTED, format!("{}: encrypted", path.display())));
            };
            if !opened.unlock(p.as_bytes()).map_err(|e| err(&e))? {
                let m = format!("{}: encrypted, and the passphrase does not open it", path.display());
                return Err(wire::error(kind::WRONG_PASSPHRASE, m));
            }
        }
        let progress = Arc::new(Progress::default());
        // (a single file compressed: its name without the compressor's extension, its size of
        // reading it once)
        if !opened.is_tree() {
            let threads = a.opt.threads;
            let mut n = 0u64;
            aqr_common::pool::run(|| {
                opened.decompress(
                    threads,
                    &mut |d| {
                        n += d.len() as u64;
                        Ok(())
                    },
                    &|_| {},
                )
            })
            .map_err(|e| err(&e))?;
            let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let cut = aqr_unpack::by_name(name.as_bytes()).map_or(0, |(_, k)| k);
            let name = name[..name.len() - cut.min(name.len())].to_owned();
            let f = Foreign {
                format: f,
                opened,
                tree: aqr_archive::Tree::default(),
                sizes: vec![Some(n)],
                packed: Vec::new(),
                whole: true,
            };
            return Ok((Source::Foreign(f), View::Single { name, size: n }, progress));
        }
        let (tree, sizes, whole) = match (opened.tree().cloned(), opened.sizes()) {
            (Some(t), Some(s)) => (t, s, false),
            _ => {
                // (read all once: the tree, and the sizes of its files)
                let sizes = Mutex::new(Vec::new());
                struct Sizes<'s>(&'s Mutex<Vec<Option<u64>>>);
                impl aqr_unpack::Sink for Sizes<'_> {
                    fn entries(&self, _: &aqr_archive::Tree, _: &[usize]) -> Result<(), String> {
                        Ok(())
                    }
                    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
                        let mut s = self.0.lock().unwrap();
                        if s.len() <= c {
                            s.resize(c + 1, None);
                        }
                        s[c] = Some(d.len() as u64);
                        Ok(())
                    }
                    fn note(&self, _: &str) {}
                }
                let threads = a.opt.threads;
                let done = aqr_common::pool::run(|| opened.unpack(threads, &Sizes(&sizes))).map_err(|e| err(&e))?;
                (done.tree, sizes.into_inner().unwrap(), true)
            }
        };
        let view = view_of(&tree, a);
        let packed = opened.packed().unwrap_or_default();
        Ok((Source::Foreign(Foreign { format: f, opened, tree, sizes, packed, whole }), view, progress))
    }

    /// Its tree (none: of a single file).
    fn tree(&self) -> Option<&aqr_archive::Tree> {
        match &self.source {
            Source::Aqr(a) => a.tree(),
            Source::Foreign(f) => f.opened.is_tree().then_some(&f.tree),
        }
    }

    /// What `path` (names joined with `/`) names.
    fn at(&self, path: &str) -> Result<At, Value> {
        let none = || wire::error(kind::NOT_FOUND, format!("{path}: not in the archive"));
        match &self.view {
            View::Single { name, .. } => match path {
                "" => Ok(At::Root),
                p if p == name => Ok(At::Single),
                _ => Err(none()),
            },
            View::Tree { by_name, .. } => {
                let mut at: Option<usize> = None;
                for name in path.split('/').filter(|n| !n.is_empty()) {
                    // (a name under what is not a directory is not there)
                    if let Some(i) = at
                        && self.tree().unwrap().entries[i].node != EntryNode::Dir
                    {
                        return Err(none());
                    }
                    at = Some(*by_name.get(&(at, name.to_owned())).ok_or_else(none)?);
                }
                Ok(at.map_or(At::Root, At::Entry))
            }
        }
    }

    /// What `-t -v` tells of content `c` (of the file of an archive of a single file, 0).
    fn listed(&self, c: usize) -> Option<&aqr_archive::Listed> {
        // (the modules named as -t -v names them)
        let name = |wasm: &[u8]| aqr_decompress::identify(wasm).map(|n| n.split('@').next().unwrap_or(&n).to_owned());
        let Source::Aqr(archive) = &self.source else { return None };
        let l = self.listed.get_or_init(|| archive.listing(&name).map_err(|e| e.to_string()));
        l.as_ref().ok()?.get(c)
    }

    /// Of `-t -v`'s, what `v` (of content `c`) has: its data blocks' bytes, the other blocks', its
    /// share of them and its ratio to its size, its segment (from 1).
    fn verbose(&self, v: &mut Value, c: usize) {
        if let Some(l) = self.listed(c) {
            v["data"] = l.data.into();
            v["overhead"] = l.overhead.into();
            v["share"] = l.share.into();
            v["ratio"] = crate::list::ratio(l.share, l.size).into();
            v["segment"] = (l.segment + 1).into();
            v["formula"] = l.formula.clone().into();
            v["nodes"] = l.nodes.into();
        }
    }

    /// The entry at `at`, as it is served (with what `-t -v` tells too, where `verbose`).
    fn entry(&self, at: At, verbose: bool) -> Value {
        match (&self.view, at) {
            (_, At::Root) => json!({"name": "", "kind": "dir"}),
            (View::Single { name, size }, _) => {
                let mut v = json!({"name": name, "kind": "file", "size": size});
                if verbose {
                    self.verbose(&mut v, 0);
                }
                v
            }
            (View::Tree { names, .. }, At::Entry(i)) => {
                let e = &self.tree().unwrap().entries[i];
                let mut v = json!({"name": names[i]});
                v["kind"] = match &e.node {
                    EntryNode::Dir => "dir",
                    EntryNode::File(_) => "file",
                    EntryNode::Link(_) => "link",
                    _ => "other",
                }
                .into();
                match &e.node {
                    &EntryNode::File(c) => {
                        let size = match &self.source {
                            Source::Aqr(a) => a.content_size(c),
                            Source::Foreign(f) => f.sizes.get(c).copied().flatten(),
                        };
                        v["size"] = size.unwrap_or(0).into();
                        // (of another format: its compressed bytes, where told)
                        if let Source::Foreign(f) = &self.source
                            && let Some(p) = f.packed.get(c).copied().flatten()
                        {
                            v["data"] = p.into();
                            v["share"] = p.into();
                            v["ratio"] = crate::list::ratio(p, size.unwrap_or(0)).into();
                        }
                        if verbose {
                            self.verbose(&mut v, c);
                        }
                    }
                    EntryNode::Link(to) => v["target"] = String::from_utf8_lossy(name_bytes(to).0).into_owned().into(),
                    _ => {}
                }
                extra(&mut v, &e.extra, matches!(e.node, EntryNode::File(_)));
                v
            }
            (View::Tree { .. }, At::Single) => unreachable!("of an archive of a single file"),
        }
    }
}

/// Of `x`, what an entry served tells.
fn extra(v: &mut Value, x: &Extra, file: bool) {
    if let Some(t) = x.mtime {
        v["mtime"] = json!([t.secs, t.nanos]);
        if t.zone == Zone::Local {
            v["local"] = true.into();
        }
    }
    for (name, t) in [("crtime", x.crtime), ("atime", x.atime), ("ctime", x.ctime)] {
        if let Some(t) = t {
            v[name] = json!([t.secs, t.nanos]);
        }
    }
    if let Some(a) = x.win_attrs {
        v["attrs"] = a.into();
    }
    // (as `-t -v` shows them)
    for (name, shown) in [
        ("owner", crate::list::owner(x)),
        ("mode", crate::list::mode(x)),
        ("win", crate::list::win(x)),
        ("mac", crate::list::mac(x)),
    ] {
        if shown != "-" {
            v[name] = shown.into();
        }
    }
    v["flags"] = crate::list::flags(x, Some(file)).into();
    if let Some(c) = &x.comment {
        v["comment"] = String::from_utf8_lossy(c).into_owned().into();
    }
}

/// The name an archive of a single file at `path` is extracted as: its own without `.aqr`.
fn single_name(path: &Path) -> String {
    let named = match path.extension().is_some_and(|e| e.eq_ignore_ascii_case("aqr")) {
        true => path.file_stem(),
        false => path.file_name(),
    };
    named.map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned())
}

/// An output of a leaf evaluated: in memory, or spilled to a file of its own (gone once closed)
/// where it is longer than all that is kept in memory.
enum Held {
    Memory(Vec<u8>),
    Spilled(File, u64),
    /// Not decoded (of an archive of another format, left out: why).
    Missing(String),
}

impl Held {
    fn len(&self) -> u64 {
        match self {
            Held::Memory(d) => d.len() as u64,
            Held::Spilled(_, n) => *n,
            Held::Missing(_) => 0,
        }
    }

    /// Its `n` bytes from `at`.
    fn read(&self, at: u64, n: usize) -> std::io::Result<Vec<u8>> {
        match self {
            Held::Memory(d) => Ok(d[at as usize..at as usize + n].to_vec()),
            Held::Spilled(f, _) => {
                let mut d = vec![0; n];
                aqr_common::fsio::read_exact_at(f, &mut d, at)?;
                Ok(d)
            }
            Held::Missing(why) => Err(std::io::Error::other(why.clone())),
        }
    }

    /// `d` held: spilled where it is longer than `cap`.
    fn of(d: Vec<u8>, cap: u64) -> std::io::Result<Held> {
        if d.len() as u64 <= cap {
            return Ok(Held::Memory(d));
        }
        let f = aqr_common::fsio::temp_file("aqr-serve")?;
        aqr_common::fsio::write_all_at(&f, &d, 0)?;
        Ok(Held::Spilled(f, d.len() as u64))
    }
}

/// An output written a piece at a time: in memory up to `cap` bytes, then spilled.
struct Growing {
    held: Held,
    cap: u64,
}

impl Growing {
    fn new(cap: u64) -> Growing {
        Growing { held: Held::Memory(Vec::new()), cap }
    }

    fn push(&mut self, d: &[u8]) -> std::io::Result<()> {
        let spilled = match &mut self.held {
            Held::Memory(m) if (m.len() + d.len()) as u64 <= self.cap => {
                m.extend_from_slice(d);
                None
            }
            Held::Memory(m) => {
                let f = aqr_common::fsio::temp_file("aqr-serve")?;
                aqr_common::fsio::write_all_at(&f, m, 0)?;
                aqr_common::fsio::write_all_at(&f, d, m.len() as u64)?;
                Some(Held::Spilled(f, (m.len() + d.len()) as u64))
            }
            Held::Spilled(f, n) => {
                aqr_common::fsio::write_all_at(f, d, *n)?;
                *n += d.len() as u64;
                None
            }
            Held::Missing(_) => unreachable!("written into"),
        };
        if let Some(h) = spilled {
            self.held = h;
        }
        Ok(())
    }

    /// The leaf of it alone.
    fn leaf(self) -> Leaf {
        let bytes = if let Held::Memory(m) = &self.held { m.len() as u64 } else { 0 };
        Leaf { outputs: vec![self.held], bytes }
    }
}

/// A leaf evaluated: its outputs, and the bytes of them in memory.
struct Leaf {
    outputs: Vec<Held>,
    bytes: u64,
}

impl Leaf {
    fn of(outputs: Vec<Vec<u8>>, cap: u64) -> Result<Leaf, String> {
        let outputs = outputs.into_iter().map(|d| Held::of(d, cap)).collect::<std::io::Result<Vec<_>>>();
        let outputs = outputs.map_err(|e| format!("spilling: {e}"))?;
        let bytes = outputs.iter().map(|h| if let Held::Memory(d) = h { d.len() as u64 } else { 0 }).sum();
        Ok(Leaf { outputs, bytes })
    }
}

type Evaluated = Arc<OnceLock<Result<Arc<Leaf>, String>>>;

/// The leaves evaluated, by archive (its handle) and leaf, the least recently read dropped once
/// they are over `cap` bytes; a leaf asked for again while it is evaluated waits for that.
struct Cache {
    cap: u64,
    used: u64,
    tick: u64,
    slots: HashMap<(u64, usize), Slot>,
}

struct Slot {
    leaf: Evaluated,
    last: u64,
    /// The bytes it holds, counted in [`Cache::used`] once it is evaluated.
    bytes: Option<u64>,
}

impl Cache {
    /// Leaf `key`, evaluated by `evaluate` where it is not kept.
    fn get(
        this: &Mutex<Cache>,
        key: (u64, usize),
        evaluate: impl FnOnce() -> Result<Leaf, String>,
    ) -> Result<Arc<Leaf>, String> {
        let leaf = {
            let mut c = this.lock().unwrap();
            c.tick += 1;
            let tick = c.tick;
            let s = c.slots.entry(key).or_insert_with(|| Slot { leaf: Arc::default(), last: tick, bytes: None });
            s.last = tick;
            s.leaf.clone()
        };
        let r = leaf.get_or_init(|| evaluate().map(Arc::new)).clone();
        let mut c = this.lock().unwrap();
        let ours = c.slots.get(&key).is_some_and(|s| Arc::ptr_eq(&s.leaf, &leaf));
        match &r {
            // (to be evaluated again when it is asked for again)
            Err(_) if ours => _ = c.slots.remove(&key),
            Ok(l) if ours => {
                let s = c.slots.get_mut(&key).unwrap();
                if s.bytes.is_none() {
                    s.bytes = Some(l.bytes);
                    c.used += l.bytes;
                    c.shrink(Some(key));
                }
            }
            _ => {}
        }
        r
    }

    /// The least recently read dropped (but `keep`) while over the bound.
    fn shrink(&mut self, keep: Option<(u64, usize)>) {
        while self.used > self.cap {
            let oldest =
                self.slots.iter().filter(|(k, s)| Some(**k) != keep && s.bytes.is_some()).min_by_key(|(_, s)| s.last);
            let Some((&k, _)) = oldest else { break };
            self.drop_slot(k);
        }
    }

    fn drop_slot(&mut self, k: (u64, usize)) {
        if let Some(s) = self.slots.remove(&k) {
            self.used -= s.bytes.unwrap_or(0);
        }
    }

    /// All of archive `handle`'s dropped.
    fn forget(&mut self, handle: u64) {
        let keys: Vec<_> = self.slots.keys().filter(|k| k.0 == handle).copied().collect();
        keys.into_iter().for_each(|k| self.drop_slot(k));
    }

    /// All that is evaluated dropped: the bytes it held.
    fn trim(&mut self) -> u64 {
        let (used, keys): (u64, Vec<_>) =
            (self.used, self.slots.iter().filter(|(_, s)| s.bytes.is_some()).map(|(k, _)| *k).collect());
        keys.into_iter().for_each(|k| self.drop_slot(k));
        used - self.used
    }
}

/// The archives open, by handle, each with the times it was opened and not closed.
#[derive(Default)]
struct Handles {
    next: u64,
    open: HashMap<u64, (Arc<Opened>, usize)>,
}

struct Server {
    /// The options of the command line.
    args: &'static Args,
    handles: Mutex<Handles>,
    cache: Mutex<Cache>,
}

/// The server, once `--ipc` has begun ([`init`]).
static SERVER: OnceLock<Server> = OnceLock::new();

/// The reads cancelled (`cancel` of them), until they stop.
static CANCELLED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// A failure of kind `failed`.
fn failed(m: impl Into<String>) -> Value {
    wire::error(kind::FAILED, m)
}

/// The server made, of `a`, the options of the command line.
pub fn init(a: &'static Args) {
    _ = DECODER.set(Decoder::new(a.opt.threads));
    let cap = CACHE.get().unwrap_or_else(|| aqr_common::budget::total().map_or(CACHE_MAX, |t| (t / 8).min(CACHE_MAX)));
    _ = SERVER.set(Server {
        args: a,
        handles: Mutex::default(),
        cache: Mutex::new(Cache { cap, used: 0, tick: 0, slots: HashMap::new() }),
    });
}

/// Request `v` (numbered `q`) served: on a thread of its own where it evaluates or maps an
/// archive, else at once.
pub fn request(q: u64, v: Value) {
    let Some(s) = SERVER.get() else { return ipc::fail(q, kind::FAILED, "not served") };
    let long = v["op"] == "open" || v["op"] == "read";
    let work = move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.answer(q, &v)));
        if r.is_err() {
            ipc::fail(q, kind::FAILED, "the request failed (a panic)");
        }
    };
    if !long {
        return work();
    }
    if std::thread::Builder::new().name("serve".into()).spawn(work).is_err() {
        ipc::fail(q, kind::BUSY, "no thread for the request");
    }
}

/// Read `of` cancelled, where it is one at work: it stops before its next reply.
pub fn cancel(of: u64) {
    CANCELLED.lock().unwrap().get_or_insert_default().insert(of);
}

/// Whether read `q` is cancelled (forgotten once told).
fn cancelled(q: u64) -> bool {
    CANCELLED.lock().unwrap().as_mut().is_some_and(|c| c.remove(&q))
}

/// The last reply of `q`: `r`, or its error.
fn done(q: u64, r: Result<Value, Value>) {
    match r {
        Ok(v) => ipc::reply(q, v, false, &[]),
        Err(e) => ipc::send(&json!({"r": q, "error": e}), &[]),
    }
}

impl Server {
    fn opened(&self, v: &Value) -> Result<Arc<Opened>, Value> {
        let h = v["archive"].as_u64().ok_or_else(|| wire::error(kind::BAD_REQUEST, "archive: not a handle"))?;
        let handles = self.handles.lock().unwrap();
        handles
            .open
            .get(&h)
            .map(|(o, _)| o.clone())
            .ok_or_else(|| wire::error(kind::NO_ARCHIVE, format!("{h}: no archive open so")))
    }

    fn open(&self, path: &Path, passphrase: Option<&str>, at: Option<usize>) -> Result<Value, Value> {
        // (as given: an archive opened by another path is opened again, to be closed apart)
        let now = File::open(path).and_then(|f| Identity::of(&f));
        let mut changed = false;
        {
            let mut handles = self.handles.lock().unwrap();
            for (o, refs) in handles.open.values_mut() {
                if o.path == path && o.at == at {
                    match &now {
                        Ok(i) if *i == o.identity => {
                            *refs += 1;
                            return Ok(self.opened_value(o, false));
                        }
                        _ => changed = true,
                    }
                }
            }
        }
        let handle = {
            let mut handles = self.handles.lock().unwrap();
            handles.next += 1;
            handles.next
        };
        let o = Arc::new(Opened::open(handle, path, passphrase, at, self.args)?);
        let v = self.opened_value(&o, changed);
        self.handles.lock().unwrap().open.insert(handle, (o, 1));
        Ok(v)
    }

    fn opened_value(&self, o: &Opened, changed: bool) -> Value {
        let mut v = json!({
            "archive": o.handle,
            "identity": o.identity.shown(),
            "single": matches!(o.view, View::Single { .. }),
        });
        if changed {
            v["changed"] = true.into();
        }
        if let Source::Foreign(f) = &o.source {
            v["fields"] = fields(f.format).into();
        }
        v
    }

    /// The entries of `dir`, a [`LISTED`] at a time (each reply but the last of `more`).
    fn list(&self, v: &Value) -> Result<Vec<Value>, Value> {
        let o = self.opened(v)?;
        let dir = v["dir"].as_str().unwrap_or("");
        let verbose = v["verbose"] == true;
        let entries: Vec<Value> = match (o.at(dir)?, &o.view) {
            (At::Root, View::Single { .. }) => vec![o.entry(At::Single, verbose)],
            (At::Root, View::Tree { children, .. }) => {
                children.get(&None).into_iter().flatten().map(|&i| o.entry(At::Entry(i), verbose)).collect()
            }
            (At::Entry(d), View::Tree { children, .. }) if o.tree().unwrap().entries[d].node == EntryNode::Dir => {
                children.get(&Some(d)).into_iter().flatten().map(|&i| o.entry(At::Entry(i), verbose)).collect()
            }
            _ => return Err(wire::error(kind::NOT_A_DIRECTORY, format!("{dir}: not a directory"))),
        };
        let mut answers: Vec<Value> = entries.chunks(LISTED).map(|c| json!({"entries": c})).collect();
        if answers.is_empty() {
            answers.push(json!({"entries": []}));
        }
        Ok(answers)
    }

    fn stat(&self, v: &Value) -> Result<Value, Value> {
        let o = self.opened(v)?;
        let at = o.at(v["path"].as_str().unwrap_or(""))?;
        Ok(json!({"entry": o.entry(at, v["verbose"] == true)}))
    }

    /// Read `q` (`v`): its replies sent, each of a chunk, the last where it fails.
    fn read(&self, q: u64, v: &Value) -> Result<(), Value> {
        let o = self.opened(v)?;
        let path = v["path"].as_str().unwrap_or("");
        let offset = v["offset"].as_u64().unwrap_or(0);
        let chunk = ipc::get().map_or(wire::CHUNK, |i| i.chunk) as u64;
        let len = v["len"].as_u64().unwrap_or(chunk);
        let (key, output, size) = match (o.at(path)?, &o.view) {
            (At::Single, View::Single { size, .. }) => ((o.handle, 0), 0, *size),
            (At::Entry(i), _) => match o.tree().unwrap().entries[i].node {
                EntryNode::File(c) => match &o.source {
                    Source::Aqr(a) => {
                        let (leaf, output, size) = a.content_at(c).expect("a content of the tree");
                        ((o.handle, leaf), output, size)
                    }
                    // (of a tar archive, all its files one leaf; else a leaf each)
                    Source::Foreign(f) => {
                        let size = f.sizes.get(c).copied().flatten().unwrap_or(0);
                        match f.whole {
                            true => ((o.handle, 0), c, size),
                            false => ((o.handle, c), 0, size),
                        }
                    }
                },
                _ => return Err(wire::error(kind::NOT_A_FILE, format!("{path}: not a file"))),
            },
            _ => return Err(wire::error(kind::NOT_A_FILE, format!("{path}: not a file"))),
        };
        let err = |e: &dyn std::fmt::Display| failed(format!("{}: {path}: {e}", o.path.display()));
        let leaf = Cache::get(&self.cache, key, || self.evaluate(&o, key.1)).map_err(|e| err(&e))?;
        let held = leaf.outputs.get(output).ok_or_else(|| err(&"a file of an output its leaf does not have"))?;
        // (of another format, a file not decoded: told why)
        if let Held::Missing(why) = held {
            return Err(err(why));
        }
        if held.len() != size {
            return Err(err(&"a file of another size than its entry's"));
        }
        let end = offset.saturating_add(len.min(size.saturating_sub(offset)));
        let mut at = offset;
        loop {
            if cancelled(q) {
                return Err(wire::error(kind::CANCELLED, format!("{path}: read cancelled")));
            }
            let n = (end - at).min(chunk) as usize;
            let d = match n {
                0 => Vec::new(),
                n => held.read(at, n).map_err(|e| err(&e))?,
            };
            at += n as u64;
            ipc::reply(q, json!({}), at < end, &d);
            if at >= end {
                return Ok(());
            }
        }
    }

    /// Leaf `leaf` of `o` evaluated.
    fn evaluate(&self, o: &Opened, leaf: usize) -> Result<Leaf, String> {
        let threads = self.args.opt.threads;
        let cap = self.cache.lock().unwrap().cap;
        if let Source::Foreign(f) = &o.source {
            return aqr_common::pool::run(|| self.evaluate_foreign(o, f, leaf, cap));
        }
        let Source::Aqr(archive) = &o.source else { unreachable!() };
        aqr_common::pool::run(|| match o.view {
            View::Single { .. } => {
                // (written as it is evaluated: spilled once it is over the bound, not held whole)
                let mut g = Growing::new(cap);
                let mut failed = None;
                let r = archive.write_file(threads, &mut |d: &[u8]| {
                    g.push(d).map_err(|e| {
                        let m = format!("spilling: {e}");
                        failed = Some(m.clone());
                        aqr_archive::Failure::Resource(m)
                    })
                });
                if let Some(m) = failed {
                    return Err(m);
                }
                r.map_err(|e| e.to_string())?;
                Ok(g.leaf())
            }
            View::Tree { .. } => {
                let mut only = vec![false; archive.leaves_len()];
                only[leaf] = true;
                let outputs = Mutex::new(None);
                archive
                    .each_leaf_only(threads, &only, |_, r| {
                        *outputs.lock().unwrap() = Some(r?);
                        Ok(())
                    })
                    .map_err(|e| e.to_string())?;
                let outputs = outputs.into_inner().unwrap().ok_or("the leaf not evaluated")?;
                Leaf::of(outputs, cap)
            }
        })
    }

    /// Leaf `leaf` of `f` (of `o`), an archive of another format, decoded: the file of content
    /// `leaf`, or (decoded whole) all of its files, by content.
    fn evaluate_foreign(&self, o: &Opened, f: &Foreign, leaf: usize, cap: u64) -> Result<Leaf, String> {
        let threads = self.args.opt.threads;
        if !f.opened.is_tree() {
            let mut g = Growing::new(cap);
            let progress = &o.progress;
            f.opened
                .decompress(threads, &mut |d| g.push(d).map_err(|e| format!("spilling: {e}")), &|s| progress.tell(s))?;
            return Ok(g.leaf());
        }
        let keeping = Keeping { wanted: None, cap, held: Mutex::default(), progress: &o.progress };
        match f.whole {
            true => _ = f.opened.unpack(threads, &keeping)?,
            false => {
                let k = Keeping { wanted: Some(leaf), ..keeping };
                let s = select_of(&f.tree, leaf).ok_or("a content of no file")?;
                f.opened.unpack_selected(threads, &k, &s)?;
                let (mut held, bytes, note) = k.held.into_inner().unwrap();
                let h = held.remove(&leaf).unwrap_or_else(|| Held::Missing(note.unwrap_or("not decoded".into())));
                return Ok(Leaf { outputs: vec![h], bytes });
            }
        }
        let (mut held, bytes, note) = keeping.held.into_inner().unwrap();
        let n = f.sizes.len();
        let why = note.unwrap_or("not decoded".into());
        let outputs = (0..n).map(|c| held.remove(&c).unwrap_or_else(|| Held::Missing(why.clone()))).collect();
        Ok(Leaf { outputs, bytes })
    }

    fn close(&self, v: &Value) -> Result<Value, Value> {
        let h = v["archive"].as_u64().ok_or_else(|| wire::error(kind::BAD_REQUEST, "archive: not a handle"))?;
        let mut handles = self.handles.lock().unwrap();
        let Some((_, refs)) = handles.open.get_mut(&h) else {
            return Err(wire::error(kind::NO_ARCHIVE, format!("{h}: no archive open so")));
        };
        *refs -= 1;
        if *refs == 0 {
            // (let go of once the reads at work are done with it)
            handles.open.remove(&h);
            self.cache.lock().unwrap().forget(h);
        }
        Ok(json!({"closed": h}))
    }

    /// Request `v` (numbered `q`) answered: its replies sent.
    fn answer(&self, q: u64, v: &Value) {
        match v["op"].as_str().unwrap_or("") {
            "open" => done(
                q,
                match wire::os_of(&v["path"]) {
                    Some(p) => self.open(Path::new(&p), v["passphrase"].as_str(), v["at"].as_u64().map(|m| m as usize)),
                    None => Err(wire::error(kind::BAD_REQUEST, "open: not a path")),
                },
            ),
            "list" => match self.list(v) {
                Ok(l) => {
                    let n = l.len();
                    for (i, a) in l.into_iter().enumerate() {
                        ipc::reply(q, a, i + 1 < n, &[]);
                    }
                }
                Err(e) => done(q, Err(e)),
            },
            "stat" => done(q, self.stat(v)),
            "read" => {
                if let Err(e) = self.read(q, v) {
                    done(q, Err(e));
                }
                // (forgotten, whether or not it came)
                cancelled(q);
            }
            "close" => done(q, self.close(v)),
            "journal" => done(
                q,
                self.opened(v).map(|o| {
                    let journal = match &o.source {
                        Source::Aqr(a) => a.journal(),
                        Source::Foreign(_) => &[],
                    };
                    let points: Vec<Value> = journal
                        .iter()
                        .map(|p| {
                            json!({
                                "metadata": p.metadata,
                                "time": p.time,
                                "shown": crate::journal::show(p.time),
                                "label": p.label,
                            })
                        })
                        .collect();
                    json!({"points": points})
                }),
            ),
            "progress" => done(
                q,
                self.opened(v).map(|o| {
                    let r = *o.progress.reached.lock().unwrap();
                    let mut a = json!({"evaluating": o.progress.evaluating.load(std::sync::atomic::Ordering::Relaxed)});
                    if let Some(r) = r {
                        a["decompressed"] = json!([r.decompressed.0, r.decompressed.1]);
                        a["evaluated"] = json!([r.evaluated.0, r.evaluated.1]);
                    }
                    a
                }),
            ),
            "trim" => done(q, Ok(json!({"trimmed": self.cache.lock().unwrap().trim()}))),
            op => done(q, Err(wire::error(kind::NOT_KNOWN, format!("{op}: a request not known")))),
        }
    }
}

/// The selection of the file of content `c` of `t` (aqr-unpack's).
fn select_of(t: &aqr_archive::Tree, c: usize) -> Option<aqr_unpack::Select> {
    let mut i = t.entries.iter().position(|e| e.node == EntryNode::File(c))?;
    let mut names = vec![t.entries[i].name.clone()];
    while let Some(p) = t.entries[i].parent {
        names.push(t.entries[p].name.clone());
        i = p;
    }
    names.reverse();
    Some(aqr_unpack::Select::new(vec![names]))
}
