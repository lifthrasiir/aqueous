//! `--serve`: this program serving another (a shell extension browsing archives, through
//! `aqr-win --serve`: `docs/win-shell-ext-design.md`) the entries of archives and the bytes of
//! their files, through its standard input and output: requests, each a JSON object on a line,
//! answered as they are done (not in their order: each request has an `id`, which its answers
//! carry).
//!
//! ```text
//! > {"hello":1}
//! < {"hello":1}
//! > {"id":1,"open":"D:\\x.aqr"}
//! < {"id":1,"archive":7,"identity":"...","single":false}
//! > {"id":2,"list":7,"dir":"a/b"}
//! < {"id":2,"entries":[{"name":"c.txt","kind":"file","size":123,"mtime":[1700000000,0]},...]}
//! > {"id":3,"read":7,"path":"a/b/c.txt","offset":0,"len":1048576}
//! < {"id":3,"data":1048576}            then the 1048576 bytes as they are
//! > {"id":4,"close":7}
//! < {"id":4,"closed":7}
//! ```
//!
//! - `open` (a path): the archive opened, by its metadata blocks alone, as a handle (`archive`)
//!   that the other requests name. `identity` tells the file opened (its volume and number, its
//!   size and modification time): the same path opened again while it is open, and the same
//!   file, is the same handle (closed as many times); a file there now that is not the one open
//!   is opened anew (`changed` true), the handle before staying of the file it was. `single`: an
//!   archive of a single file (no metadata block), whose one entry at the root is the name the
//!   archive is extracted as. An encrypted archive is opened of `passphrase` given with `open`
//!   (decrypted as it is read); without it, an `error` with `encrypted` true, and `wrong` too
//!   where the passphrase given does not open it.
//! - `list` (a handle, and `dir`: names from the root joined with `/`, empty for the root): the
//!   entries of the directory, in answers of at most [`LISTED`] entries, each but the last with
//!   `more` true. An entry has its `name` (as `aqr -d` writes it: [`crate::names`]), its `kind`
//!   (`dir`, `file`, `link`, `other`) and what is known of it: a file's `size`, `mtime` (seconds
//!   and nanoseconds since the epoch; `local` true where it is a local time of an unknown time
//!   zone; `crtime`, `atime`, `ctime` alike), `attrs` (Windows' attributes), as `-t -v` shows
//!   them `owner`, `mode`, `win`, `mac` and `flags`, its `comment`, a link's `target`. With
//!   `verbose` true, a file's `data`, `overhead`, `share`, `ratio` and `segment` too (as `-t -v`'s:
//!   all of the archive's blocks read for them, once).
//! - `stat` (a handle, and `path`): the entry at the path, as `entry` (the root's of no name).
//! - `read` (a handle, `path`, `offset`, `len`): bytes of the file at `path` from `offset`, at
//!   most `len` and [`CHUNK`]: `data` tells how many, and that many bytes follow the line as
//!   they are (fewer than asked only at the end of the file, none past it). The first read of a
//!   file evaluates its leaf, all of whose files are kept ([`Cache`]) for the reads after.
//! - `close` (a handle): the handle given back; once all of a file's are, the archive is let go
//!   of (the file no longer mapped) and its files kept dropped.
//! - `progress` (a handle): how far the evaluation of its leaves is (`decompressed` and
//!   `evaluated`, `[done, total]` bytes of what is evaluated at once, as the archive last told
//!   it: of the evaluations at work of several requests, any of them), and `evaluating`, how many
//!   leaves are.
//! - `trim`: the files kept dropped (`trimmed`: their bytes), as a frontend idle a while asks.
//!
//! A request that fails is answered by `error` (a message). The program ends once its standard
//! input is closed; what it spilled to files is gone with it.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use aqr_archive::{Archive, EntryNode, Extra, Zone, name_bytes};
use serde_json::{Value, json};

use crate::names::Names;
use crate::{Args, Decoder};

/// The version of the protocol.
pub const VERSION: u64 = 1;

/// The most bytes a `read` is answered with: a long read is several, so that one does not hold
/// the others' answers back long.
pub const CHUNK: usize = 1 << 20;

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
    // (dropped before what it borrows: fields are dropped in their order)
    archive: Archive<'static, 'static>,
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
    fn open(handle: u64, path: &Path, passphrase: Option<&str>, a: &Args) -> Result<Opened, Value> {
        let err = |e: &dyn std::fmt::Display| json!({"error": format!("{}: {e}", path.display())});
        let file = File::open(path).map_err(|e| err(&e))?;
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
                return Err(json!({"error": format!("{}: encrypted", path.display()), "encrypted": true}));
            };
            let (e, _) = aqr_archive::envelope::open_damaged(map).map_err(|e| err(&e))?;
            match e.unlock(p.as_bytes()) {
                Ok(u) => leaked.sealed = Some(Box::leak(Box::new(u.decrypting()))),
                Err(aqr_archive::Failure::Decryption(_)) => {
                    let m = format!("{}: encrypted, and the passphrase does not open it", path.display());
                    return Err(json!({"error": m, "encrypted": true, "wrong": true}));
                }
                Err(f) => return Err(err(&f)),
            }
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
                (true, _) => aqr_archive::open_indexed(payload, runner, a.opt.threads),
                (false, Some(d)) => aqr_archive::open_decrypting(d, runner),
                (false, None) => aqr_archive::open(bytes, runner),
            }
            .map_err(|e| err(&e))?;
            archive.set_budget(aqr_common::budget::global());
            let view = match archive.tree() {
                Some(t) => {
                    let names = Names::new(&a.names, false);
                    names.decide(t);
                    let names: Vec<String> =
                        t.entries.iter().map(|e| names.name(&e.name).os.to_string_lossy().into_owned()).collect();
                    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
                    let mut by_name = HashMap::new();
                    for (i, e) in t.entries.iter().enumerate() {
                        children.entry(e.parent).or_default().push(i);
                        // (of two names written the same, the first)
                        by_name.entry((e.parent, names[i].clone())).or_insert(i);
                    }
                    View::Tree { names, children, by_name }
                }
                None => {
                    let size = archive.listing(&|_| None).map_err(|e| err(&e))?.first().map_or(0, |l| l.size);
                    View::Single { name: single_name(path), size }
                }
            };
            let progress = Arc::new(Progress::default());
            let told = progress.clone();
            archive.on_progress(Some(Arc::new(move |s| match s {
                aqr_archive::Step::Reached(r) => *told.reached.lock().unwrap() = Some(r),
                aqr_archive::Step::Started(_) => _ = told.evaluating.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                aqr_archive::Step::Ended(_) => _ = told.evaluating.fetch_sub(1, std::sync::atomic::Ordering::Relaxed),
            })));
            Ok((archive, view, progress))
        })();
        let (archive, view, progress) = opened?;
        Ok(Opened {
            handle,
            path: path.to_owned(),
            identity,
            archive,
            _leaked: leaked,
            view,
            progress,
            listed: OnceLock::new(),
        })
    }

    /// What `path` (names joined with `/`) names.
    fn at(&self, path: &str) -> Result<At, String> {
        let none = || format!("{path}: not in the archive");
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
                        && self.archive.tree().unwrap().entries[i].node != EntryNode::Dir
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
        let l = self.listed.get_or_init(|| self.archive.listing(&|_| None).map_err(|e| e.to_string()));
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
                let e = &self.archive.tree().unwrap().entries[i];
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
                        v["size"] = self.archive.content_size(c).unwrap_or(0).into();
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
}

impl Held {
    fn len(&self) -> u64 {
        match self {
            Held::Memory(d) => d.len() as u64,
            Held::Spilled(_, n) => *n,
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

struct Server<'a> {
    args: &'a Args,
    handles: Mutex<Handles>,
    cache: Mutex<Cache>,
}

/// What a request is answered with: a line, and the bytes after it.
type Answer = (Value, Vec<u8>);

impl Server<'_> {
    fn opened(&self, v: &Value, key: &str) -> Result<Arc<Opened>, String> {
        let h = v[key].as_u64().ok_or_else(|| format!("{key}: not a handle"))?;
        let handles = self.handles.lock().unwrap();
        handles.open.get(&h).map(|(o, _)| o.clone()).ok_or_else(|| format!("{h}: no archive open so"))
    }

    fn open(&self, path: &str, passphrase: Option<&str>) -> Result<Value, Value> {
        let path = Path::new(path);
        // (as given: an archive opened by another path is opened again, to be closed apart)
        let now = File::open(path).and_then(|f| Identity::of(&f));
        let mut changed = false;
        {
            let mut handles = self.handles.lock().unwrap();
            for (o, refs) in handles.open.values_mut() {
                if o.path == path {
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
        let o = Arc::new(Opened::open(handle, path, passphrase, self.args)?);
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
        v
    }

    /// The entries of `dir`, a [`LISTED`] at a time.
    fn list(&self, v: &Value) -> Result<Vec<Value>, String> {
        let o = self.opened(v, "list")?;
        let dir = v["dir"].as_str().unwrap_or("");
        let verbose = v["verbose"] == true;
        let entries: Vec<Value> = match (o.at(dir)?, &o.view) {
            (At::Root, View::Single { .. }) => vec![o.entry(At::Single, verbose)],
            (At::Root, View::Tree { children, .. }) => {
                children.get(&None).into_iter().flatten().map(|&i| o.entry(At::Entry(i), verbose)).collect()
            }
            (At::Entry(d), View::Tree { children, .. })
                if o.archive.tree().unwrap().entries[d].node == EntryNode::Dir =>
            {
                children.get(&Some(d)).into_iter().flatten().map(|&i| o.entry(At::Entry(i), verbose)).collect()
            }
            _ => return Err(format!("{dir}: not a directory")),
        };
        let mut answers: Vec<Value> = entries.chunks(LISTED).map(|c| json!({"entries": c, "more": true})).collect();
        match answers.last_mut() {
            Some(last) => last["more"] = false.into(),
            None => answers.push(json!({"entries": [], "more": false})),
        }
        Ok(answers)
    }

    fn stat(&self, v: &Value) -> Result<Value, String> {
        let o = self.opened(v, "stat")?;
        let at = o.at(v["path"].as_str().unwrap_or(""))?;
        Ok(json!({"entry": o.entry(at, v["verbose"] == true)}))
    }

    fn read(&self, v: &Value) -> Result<Answer, String> {
        let o = self.opened(v, "read")?;
        let path = v["path"].as_str().unwrap_or("");
        let offset = v["offset"].as_u64().unwrap_or(0);
        let len = v["len"].as_u64().unwrap_or(CHUNK as u64).min(CHUNK as u64);
        let (key, output, size) = match (o.at(path)?, &o.view) {
            (At::Single, View::Single { size, .. }) => ((o.handle, 0), 0, *size),
            (At::Entry(i), _) => match o.archive.tree().unwrap().entries[i].node {
                EntryNode::File(c) => {
                    let (leaf, output, size) = o.archive.content_at(c).expect("a content of the tree");
                    ((o.handle, leaf), output, size)
                }
                _ => return Err(format!("{path}: not a file")),
            },
            _ => return Err(format!("{path}: not a file")),
        };
        let err = |e: &dyn std::fmt::Display| format!("{}: {path}: {e}", o.path.display());
        let leaf = Cache::get(&self.cache, key, || self.evaluate(&o, key.1)).map_err(|e| err(&e))?;
        let held = leaf.outputs.get(output).ok_or_else(|| err(&"a file of an output its leaf does not have"))?;
        if held.len() != size {
            return Err(err(&"a file of another size than its entry's"));
        }
        let n = len.min(size.saturating_sub(offset)) as usize;
        let d = match n {
            0 => Vec::new(),
            n => held.read(offset, n).map_err(|e| err(&e))?,
        };
        Ok((json!({"data": n}), d))
    }

    /// Leaf `leaf` of `o` evaluated.
    fn evaluate(&self, o: &Opened, leaf: usize) -> Result<Leaf, String> {
        let threads = self.args.opt.threads;
        let cap = self.cache.lock().unwrap().cap;
        aqr_common::pool::run(|| match o.view {
            View::Single { .. } => {
                // (written as it is evaluated: spilled once it is over the bound, not held whole)
                let mut held = Held::Memory(Vec::new());
                let mut failed = None;
                let r = o.archive.write_file(threads, &mut |d: &[u8]| {
                    let r = match &mut held {
                        Held::Memory(m) if (m.len() + d.len()) as u64 <= cap => {
                            m.extend_from_slice(d);
                            Ok(None)
                        }
                        Held::Memory(m) => aqr_common::fsio::temp_file("aqr-serve").and_then(|f| {
                            aqr_common::fsio::write_all_at(&f, m, 0)?;
                            aqr_common::fsio::write_all_at(&f, d, m.len() as u64)?;
                            let n = (m.len() + d.len()) as u64;
                            Ok(Some(Held::Spilled(f, n)))
                        }),
                        Held::Spilled(f, n) => aqr_common::fsio::write_all_at(f, d, *n).map(|()| {
                            *n += d.len() as u64;
                            None
                        }),
                    };
                    let r = r.map(|spilled| {
                        if let Some(s) = spilled {
                            held = s;
                        }
                    });
                    r.map_err(|e| {
                        let m = format!("spilling: {e}");
                        failed = Some(m.clone());
                        aqr_archive::Failure::Resource(m)
                    })
                });
                if let Some(m) = failed {
                    return Err(m);
                }
                r.map_err(|e| e.to_string())?;
                let bytes = if let Held::Memory(m) = &held { m.len() as u64 } else { 0 };
                Ok(Leaf { outputs: vec![held], bytes })
            }
            View::Tree { .. } => {
                let mut only = vec![false; o.archive.leaves_len()];
                only[leaf] = true;
                let outputs = Mutex::new(None);
                o.archive
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

    fn close(&self, v: &Value) -> Result<Value, String> {
        let h = v["close"].as_u64().ok_or("close: not a handle")?;
        let mut handles = self.handles.lock().unwrap();
        let Some((_, refs)) = handles.open.get_mut(&h) else { return Err(format!("{h}: no archive open so")) };
        *refs -= 1;
        if *refs == 0 {
            // (let go of once the reads at work are done with it)
            handles.open.remove(&h);
            self.cache.lock().unwrap().forget(h);
        }
        Ok(json!({"closed": h}))
    }

    /// Request `v` done: its answers.
    fn answer(&self, v: &Value) -> Vec<Answer> {
        let plain = |r: Result<Value, String>| vec![(r.unwrap_or_else(|e| json!({"error": e})), Vec::new())];
        if let Some(p) = v.get("open") {
            let r = match p.as_str() {
                Some(p) => self.open(p, v["passphrase"].as_str()),
                None => Err(json!({"error": "open: not a path"})),
            };
            return vec![(r.unwrap_or_else(|e| e), Vec::new())];
        }
        if v.get("list").is_some() {
            return match self.list(v) {
                Ok(l) => l.into_iter().map(|v| (v, Vec::new())).collect(),
                Err(e) => plain(Err(e)),
            };
        }
        if v.get("stat").is_some() {
            return plain(self.stat(v));
        }
        if v.get("read").is_some() {
            return vec![self.read(v).unwrap_or_else(|e| (json!({"error": e}), Vec::new()))];
        }
        if v.get("close").is_some() {
            return plain(self.close(v));
        }
        if v.get("progress").is_some() {
            return plain(self.opened(v, "progress").map(|o| {
                let r = *o.progress.reached.lock().unwrap();
                let mut a = json!({"evaluating": o.progress.evaluating.load(std::sync::atomic::Ordering::Relaxed)});
                if let Some(r) = r {
                    a["decompressed"] = json!([r.decompressed.0, r.decompressed.1]);
                    a["evaluated"] = json!([r.evaluated.0, r.evaluated.1]);
                }
                a
            }));
        }
        if v.get("trim").is_some() {
            return plain(Ok(json!({"trimmed": self.cache.lock().unwrap().trim()})));
        }
        plain(Err("a request not known".into()))
    }
}

/// Answers `a` to request `id`, each line with the bytes after it, at once.
fn send(id: &Value, answers: Vec<Answer>) {
    let mut out = std::io::stdout().lock();
    for (mut v, d) in answers {
        v["id"] = id.clone();
        // (nothing to do where the frontend is gone: its end of standard input closes too)
        if writeln!(out, "{v}").and_then(|_| out.write_all(&d)).is_err() {
            return;
        }
    }
    _ = out.flush();
}

/// Serves the frontend until it is gone.
pub fn serve(a: &Args) -> Result<(), String> {
    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    stdin.read_line(&mut line).map_err(|e| format!("--serve: {e}"))?;
    let hello: Value = serde_json::from_str(&line).map_err(|_| "--serve: no hello from the frontend")?;
    match hello["hello"].as_u64() {
        Some(v) if v >= VERSION => {}
        Some(v) => return Err(format!("--serve: version {v} of the protocol is not known")),
        None => return Err("--serve: no hello from the frontend".into()),
    }
    _ = DECODER.set(Decoder::new(a.opt.threads));
    let cap = CACHE.get().unwrap_or_else(|| aqr_common::budget::total().map_or(CACHE_MAX, |t| (t / 8).min(CACHE_MAX)));
    let server = Server {
        args: a,
        handles: Mutex::default(),
        cache: Mutex::new(Cache { cap, used: 0, tick: 0, slots: HashMap::new() }),
    };
    {
        let mut out = std::io::stdout().lock();
        _ = writeln!(out, "{}", json!({"hello": VERSION})).and_then(|_| out.flush());
    }
    let server = &server;
    std::thread::scope(|s| {
        for line in stdin.lines() {
            let line = match line {
                Ok(line) => line,
                // (a line that is not text is not understood: answered as such)
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(_) => break,
            };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let id = v["id"].clone();
            // (what evaluates or maps an archive on a thread of its own, the rest at once)
            let long = v.get("open").is_some() || v.get("read").is_some();
            let failed = id.clone();
            let run = move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.answer(&v)));
                let answers =
                    r.unwrap_or_else(|_| vec![(json!({"error": "the request failed (a panic)"}), Vec::new())]);
                send(&id, answers);
            };
            if long {
                if std::thread::Builder::new().name("serve".into()).spawn_scoped(s, run).is_err() {
                    send(&failed, vec![(json!({"error": "no thread for the request"}), Vec::new())]);
                }
            } else {
                run();
            }
        }
        // (the frontend gone: the work at it not waited for)
        std::process::exit(0);
    })
}
