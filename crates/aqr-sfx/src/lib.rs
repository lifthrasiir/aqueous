//! The self-extracting apply logic, shared by the stub (`aqr-sfx-shim`) and the tool that builds
//! self-extracting archives (`aqr --format sfx`): [`Sfx`] finds the blobs merged into the program
//! through the dispatch table `aqr-ld merge` wrote into the anchor -- the native module blobs
//! (aqr-dll), each named by its module's hash ([`native_name`]), and the interpreter blob
//! (`aqr-interp`, [`INTERP`]) for any other module -- and is the [`Runner`] the decoder runs each
//! module through; [`Sfx::load`] takes the same blobs from a [`bundle`] (what `aqr --format sfx`
//! appends to the stub), placing them in memory; [`extract`] writes a decoded archive's files
//! out. The blobs are Windows x86-64 only; elsewhere [`Sfx::find`] finds none.

pub mod bundle;
pub mod config;
mod native;
#[cfg(feature = "encode")]
pub mod pe;
#[cfg(windows)]
pub mod resource;

use aqr_archive::{Decoded, Failure, Loaded, Runner};
use aqr_blob::interp::Interpreter;
#[cfg(windows)]
use aqr_blob::interp::Vtable;
#[cfg(windows)]
use native::Fns;
pub use native::native_name;
use native::{Native, NativeLoaded};

/// The name of the interpreter blob in the dispatch table.
pub const INTERP: &str = "aqr-interp";

/// What the name of a shared library that native blobs link against (crates/aqr-dll/layout.py)
/// starts with in the dispatch table: its code is reached through the blobs' imports alone (the
/// merge binds them), its exception table registered as theirs are.
pub const LIB: &str = "lib:";

/// The runner of a self-extracting program: the modules with native blobs merged into it run
/// natively, any other through the interpreter blob, where it is merged.
pub struct Sfx {
    interp: Option<Interpreter>,
    natives: Vec<Native>,
    /// The threads the native blobs decode whole inputs on.
    threads: usize,
}

impl Sfx {
    /// The threads the native blobs decode whole inputs on, as many as the machine has.
    pub fn threads(&self) -> usize {
        self.threads
    }
}

impl Runner for Sfx {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        if let Some(hash) = native::stand_in_hash(wasm) {
            let n = self.natives.iter().find(|n| n.hash == hash);
            let n = n.ok_or_else(|| Failure::Resource("a native module blob not merged".into()))?;
            return Ok(Box::new(NativeLoaded { native: n.fns, threads: self.threads }));
        }
        let interp = self.interp.as_ref().ok_or_else(|| {
            Failure::Resource("a module with no native blob merged, and no interpreter blob either".into())
        })?;
        interp.load(wasm)
    }

    fn registered(&self, hash: &[u8; 32]) -> Decoded<Vec<u8>> {
        match self.natives.iter().find(|n| n.hash == *hash) {
            Some(n) => Ok(native::stand_in(hash, &n.slots)),
            None => Err(Failure::Resource("a registered module with no native blob merged".into())),
        }
    }
}

// ---- finding the merged blobs

#[used]
#[unsafe(no_mangle)]
static AQR_PEMERGE_ANCHOR: aqr_blob::dispatch::Anchor = aqr_blob::dispatch::Anchor::EMPTY;

#[cfg(windows)]
impl Sfx {
    /// The blobs merged into this program, as the dispatch table names them: the interpreter
    /// blob ([`INTERP`], the RVA of its `aqr_interp_vtable`) and the native module blobs
    /// ([`native_name`], the RVA of each `aqr_dll_apply`), each one's image registered here
    /// (`aqr_blob::register_image`: its panics' type information is relative to its RVA 0). Their
    /// exception data is in this program's own directory (the merge put it there: the unwinder
    /// reads no other for the addresses of an image).
    pub fn find() -> Sfx {
        unsafe extern "C" {
            static __ImageBase: u8;
        }
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        let mut sfx = Sfx {
            interp: None,
            natives: Vec::new(),
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        };
        // (the merged blobs' imports from the C runtime are bound to this program's)
        #[cfg(target_env = "msvc")]
        aqr_blob::keep_crt();
        let base = &raw const __ImageBase as usize;
        let Some((rva, len)) = AQR_PEMERGE_ANCHOR.table() else { return sfx };
        let table = unsafe { std::slice::from_raw_parts((base + rva as usize) as *const u8, len as usize) };
        REGISTERED.call_once(|| {
            for e in aqr_blob::dispatch::entries(table) {
                aqr_blob::register_image(base + e.image_rva as usize, e.image_len as usize, base + e.mod_base as usize);
            }
        });
        for e in aqr_blob::dispatch::entries(table) {
            let info = if e.info_rva == 0 { 0 } else { base + e.info_rva as usize };
            // SAFETY: the merge wrote the RVAs of the blob's entries
            unsafe { sfx.add(e.name, base + e.apply_rva as usize, info) };
        }
        sfx
    }

    /// The blobs of bundle `bundle` ([`bundle`]: what `aqr --format sfx` appends to the stub)
    /// placed in memory (`aqr_blob::load`: relocated, their imports bound, their exception tables
    /// and images registered) and taken as [`Sfx::find`] takes the merged ones.
    #[cfg(target_arch = "x86_64")]
    pub fn load(&mut self, bundle: &[u8]) -> Result<(), String> {
        #[cfg(target_env = "msvc")]
        aqr_blob::keep_crt();
        let blobs = bundle::unpack(bundle)?;
        let set: Vec<&[u8]> = blobs.iter().map(|(_, b)| &b[..]).collect();
        let image = aqr_blob::load(&set)?;
        for ((name, _), p) in blobs.iter().zip(&image.blobs) {
            // SAFETY: the blob's entries, placed (a module blob's aqr_dll_apply and aqr_dll_info,
            // the interpreter blob's aqr_interp_vtable)
            unsafe { self.add(name.as_bytes(), p.at(p.apply_rva), p.at(p.info_rva)) };
        }
        Ok(())
    }

    /// None: blobs are placed on x86-64 alone.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn load(&mut self, _: &[u8]) -> Result<(), String> {
        Err("the blobs of a self-extracting program run on x86-64 alone".into())
    }

    /// Takes the blob of name `name` in the program (as [`Sfx::find`] tells) whose entries are
    /// at `apply` and `info` (0 for none): the interpreter blob, a native module blob, or
    /// neither (a shared library: nothing to take).
    ///
    /// # Safety
    /// `apply` and `info` are the blob's entries, as its name tells.
    unsafe fn add(&mut self, name: &[u8], apply: usize, info: usize) {
        if name == INTERP.as_bytes() {
            // SAFETY: the RVA of aqr_interp_vtable, which returns the static vtable
            let vtable_fn: extern "C" fn() -> *const Vtable = unsafe { std::mem::transmute(apply) };
            self.interp = Some(unsafe { Interpreter::new(vtable_fn()) });
        } else if let Some((hash, slots)) = native::parse_native_name(name)
            && apply != 0
            && info != 0
        {
            // SAFETY: the blob's aqr_dll_apply and aqr_dll_info
            let apply: aqr_api::dll::ApplyFn = unsafe { std::mem::transmute(apply) };
            let info: aqr_api::dll::InfoFn = unsafe { std::mem::transmute(info) };
            // (a blob of another version of the interface left out: its info is not this)
            let info = unsafe { info() };
            if info.version == aqr_api::dll::VERSION {
                let fns = Fns {
                    apply,
                    apply_part: info.apply_part,
                    run: info.run,
                    resources: info.resources != 0,
                    decode: info.decode,
                    streamed: info.streamed,
                };
                self.natives.push(Native { hash, slots, fns });
            }
        }
    }
}

#[cfg(not(windows))]
impl Sfx {
    /// None: the merged blobs are Windows x86-64 only.
    pub fn find() -> Sfx {
        Sfx { interp: None, natives: Vec::new(), threads: std::thread::available_parallelism().map_or(1, |n| n.get()) }
    }

    /// None: the blobs are Windows x86-64 only.
    pub fn load(&mut self, _: &[u8]) -> Result<(), String> {
        Err("the blobs of a self-extracting program run on Windows x86-64 alone".into())
    }
}

// ---- writing the decoded files out

use std::path::{Path, PathBuf};

use aqr_archive::{EntryNode, Value};
pub use config::Overwrite;

/// What is answered where a file is there already and the program asks ([`Overwrite::Ask`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Written over.
    Yes,
    /// Left as it is.
    No,
    /// Written over, and every other there too.
    All,
    /// Left, and every other there too.
    None,
    /// Nothing more written: the extraction fails.
    Quit,
}

/// How [`extract_with`] writes the files.
pub struct Extracting<'a> {
    pub overwrite: Overwrite,
    /// Asked of a path there already, where [`Overwrite::Ask`].
    pub ask: &'a mut dyn FnMut(&Path) -> Answer,
    /// The directory `.aqr` of the archive's root left out (what makes the program again: not
    /// written where a program is run, docs/sfx-config-design.md, 6).
    pub skip_aqr: bool,
    /// What was made, in order (directories before what is in them; the directories of the
    /// destination made among them): what [`clean`] removes. Kept where the extraction fails.
    pub made: Vec<PathBuf>,
    /// Of the files, those left as they were there.
    pub left: usize,
}

impl Extracting<'_> {
    /// As [`extract`] writes: nothing there written over.
    pub fn new(ask: &mut dyn FnMut(&Path) -> Answer) -> Extracting<'_> {
        Extracting { overwrite: Overwrite::Fail, ask, skip_aqr: false, made: Vec::new(), left: 0 }
    }
}

/// Writes the files of `value` under `dest`, returning how many were written. Nothing there is
/// written over or through: a file there already fails, and so does a link (or junction) where a
/// directory of the archive goes, which would lead out of `dest`.
pub fn extract(value: Value, dest: &Path) -> Result<usize, String> {
    extract_with(value, dest, &mut Extracting::new(&mut |_| Answer::Quit))
}

/// [`extract`] as `x` tells: a file there written over or not by [`Extracting::overwrite`] (but a
/// directory, or a link where a directory of the archive goes, still fails), the files' times
/// of modification set where the archive has them.
pub fn extract_with(value: Value, dest: &Path, x: &mut Extracting) -> Result<usize, String> {
    // (the directories of the destination made noted, outermost first)
    let mut missing: Vec<PathBuf> =
        dest.ancestors().take_while(|p| !p.as_os_str().is_empty() && !p.exists()).map(Path::to_path_buf).collect();
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    missing.reverse();
    x.made.extend(missing);
    match value {
        Value::File(data) => {
            let wrote = write(&dest.join("aqr-sfx-output"), &data, None, x)?;
            Ok(wrote as usize)
        }
        Value::Tree(tree, contents) => {
            // each entry's path, `None` where it is left out
            let mut paths: Vec<Option<PathBuf>> = Vec::with_capacity(tree.entries.len());
            let mut files = 0;
            for e in &tree.entries {
                // the entries come each after its parent, so the parent's path is known
                let base = match e.parent {
                    Some(p) => paths[p].clone(),
                    None if x.skip_aqr && e.name == b".aqr" => None,
                    None => Some(dest.to_path_buf()),
                };
                let Some(path) = base.map(|b| b.join(sanitized(&e.name))) else {
                    paths.push(None);
                    continue;
                };
                match &e.node {
                    EntryNode::Dir => {
                        if dir(&path)? {
                            x.made.push(path.clone());
                        }
                    }
                    EntryNode::File(c) => {
                        let data = contents.get(*c).ok_or_else(|| format!("{}: no content", path.display()))?;
                        let mtime = e.extra.mtime.as_ref().map(system_time);
                        if write(&path, data, mtime, x)? {
                            files += 1;
                        }
                    }
                    // links and special entries are left out of this first stub
                    _ => {}
                }
                paths.push(Some(path));
            }
            Ok(files)
        }
    }
}

/// Removes what `made` names ([`Extracting::made`]): the files, then the directories innermost
/// first where they are empty. What cannot be removed (a file still open) is left; returns how
/// many were.
pub fn clean(made: &[PathBuf]) -> usize {
    let mut left = 0;
    for p in made.iter().rev() {
        let gone = match std::fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => std::fs::remove_dir(p),
            Ok(_) => std::fs::remove_file(p),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        left += gone.is_err() as usize;
    }
    left
}

fn system_time(t: &aqr_archive::Time) -> std::time::SystemTime {
    let at = std::time::Duration::new(t.secs.unsigned_abs(), 0);
    let base = match t.secs >= 0 {
        true => std::time::UNIX_EPOCH.checked_add(at),
        false => std::time::UNIX_EPOCH.checked_sub(at),
    };
    base.and_then(|b| b.checked_add(std::time::Duration::from_nanos(t.nanos.into()))).unwrap_or(std::time::UNIX_EPOCH)
}

/// Makes directory `path` (its parent there), or takes the one there: not a link to one. Whether
/// it was made.
fn dir(path: &Path) -> Result<bool, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(false),
        Ok(_) => Err(err(&"there already, not a directory (or a link), not written through")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map(|_| true).map_err(|e| err(&e))
        }
        Err(e) => Err(err(&e)),
    }
}

/// Writes `data` to `path` (its directory made already), a new file: not through a link there,
/// and over a file there only as `x` tells (the link itself replaced). Whether it was written.
fn write(path: &Path, data: &[u8], mtime: Option<std::time::SystemTime>, x: &mut Extracting) -> Result<bool, String> {
    use std::io::Write;
    let err = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{}: there already, not written over", path.display()),
        _ => format!("{}: {e}", path.display()),
    };
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if m.is_dir() {
            return Err(format!("{}: a directory there, not written over", path.display()));
        }
        let over = match x.overwrite {
            Overwrite::Fail => return Err(err(std::io::ErrorKind::AlreadyExists.into())),
            Overwrite::Always => true,
            Overwrite::Never => false,
            Overwrite::Newer => mtime.zip(m.modified().ok()).is_some_and(|(new, old)| new > old),
            Overwrite::Ask => match (x.ask)(path) {
                Answer::Yes => true,
                Answer::No => false,
                Answer::All => {
                    x.overwrite = Overwrite::Always;
                    true
                }
                Answer::None => {
                    x.overwrite = Overwrite::Never;
                    false
                }
                Answer::Quit => return Err(format!("{}: there already; stopped", path.display())),
            },
        };
        if !over {
            x.left += 1;
            return Ok(false);
        }
        // (a link to a directory is one to remove as a directory, on Windows)
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path)).map_err(err)?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(err)?;
    x.made.push(path.to_path_buf());
    f.write_all(data).map_err(err)?;
    if let Some(t) = mtime {
        f.set_modified(t).map_err(err)?;
    }
    Ok(true)
}

/// An entry name (WTF-8) as a path component, lossily, as a Windows name cannot escape its
/// directory: separators and the characters a name cannot have (`<>:"|?*`, controls: a
/// `C:x` would replace the directory, `x:y` name a stream) as `_`, a dot or a space at its end
/// as `_`, and `_` before a device's name (`CON`, `NUL.txt`, ...; as `aqr`'s own extraction).
pub fn sanitized(name: &[u8]) -> String {
    let bad = |c: char| c < ' ' || "<>:\"|?*/\\".contains(c);
    let mut s: String = String::from_utf8_lossy(name).replace(bad, "_");
    if s.ends_with(['.', ' ']) {
        s.pop();
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p).is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']) || ["¹", "²", "³"].contains(&n))
    };
    if matches!(&stem[..], "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT") {
        s.insert(0, '_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{extract, sanitized};
    use aqr_archive::{Entry, EntryNode, Extra, Tree, Value};

    /// A tree of a directory `sub` holding a file `x`.
    fn tree() -> Value {
        let e = |parent, name: &str, node| Entry { parent, name: name.into(), node, extra: Extra::default() };
        let t = Tree {
            entries: vec![e(None, "sub", EntryNode::Dir), e(Some(0), "x", EntryNode::File(0))],
            ..Tree::default()
        };
        Value::Tree(Box::new(t), vec![b"new".to_vec()])
    }

    /// What is there already is not written over, nor written through where it is a link.
    #[test]
    fn nothing_written_over_or_through() {
        let d = std::env::temp_dir().join(format!("aqr-sfx-extract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (dest, out) = (d.join("dest"), d.join("out"));
        std::fs::create_dir_all(dest.join("sub")).unwrap();
        std::fs::write(dest.join("sub/x"), b"old").unwrap();
        let e = extract(tree(), &dest).unwrap_err();
        assert!(e.contains("not written over"), "{e}");
        assert_eq!(std::fs::read(dest.join("sub/x")).unwrap(), b"old");
        // (a link where the directory goes, out of the destination: where links can be made)
        std::fs::remove_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&out, dest.join("sub")).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&out, dest.join("sub")).is_ok();
        if linked {
            let e = extract(tree(), &dest).unwrap_err();
            assert!(e.contains("not written through"), "{e}");
            assert!(!out.join("x").exists());
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A file there is written over or left as the policy tells (asked: as answered); `.aqr`
    /// left out where told; [`clean`] removes what was made and nothing else.
    #[test]
    fn overwrite_policies_and_clean() {
        use super::{Answer, Extracting, Overwrite, clean, extract_with};
        use std::path::Path;
        let d = std::env::temp_dir().join(format!("aqr-sfx-overwrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let at = |secs, name: &[u8], c| {
            let mut e = entry(None, name, EntryNode::File(c));
            e.extra.mtime = Some(aqr_archive::Time { secs, nanos: 0, zone: aqr_archive::Zone::Utc, unit: 1 });
            e
        };
        // (of 2001 and of 2100: older and newer than the files there, written now)
        let t = Tree {
            entries: vec![
                at(1_000_000_000, b"old", 0),
                at(4_102_444_800, b"new", 1),
                entry(None, b".aqr", EntryNode::Dir),
                entry(Some(2), b"sfx", EntryNode::File(0)),
            ],
            ..Tree::default()
        };
        let v = || Value::Tree(Box::new(t.clone()), vec![b"archive's".to_vec(), b"archive's".to_vec()]);
        let read = |n: &str| String::from_utf8(std::fs::read(d.join(n)).unwrap()).unwrap();
        let there = || {
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            ["old", "new"].iter().for_each(|n| std::fs::write(d.join(n), b"there").unwrap());
        };
        let mut never = |_: &Path| -> Answer { panic!("asked") };
        for (policy, want) in [
            (Overwrite::Always, ["archive's", "archive's"]),
            (Overwrite::Never, ["there", "there"]),
            (Overwrite::Newer, ["there", "archive's"]),
        ] {
            there();
            let mut x = Extracting { overwrite: policy, skip_aqr: true, ..Extracting::new(&mut never) };
            let n = extract_with(v(), &d, &mut x).unwrap();
            assert_eq!([read("old"), read("new")], want.map(String::from), "{policy:?}");
            assert_eq!(n + x.left, 2, "{policy:?}");
            assert!(!d.join(".aqr").exists());
        }
        // asked: a file at a time, or all of them at once
        for (answers, want) in [
            (vec![Answer::No, Answer::Yes], ["there", "archive's"]),
            (vec![Answer::All], ["archive's", "archive's"]),
            (vec![Answer::None], ["there", "there"]),
        ] {
            there();
            let mut answers = answers.into_iter();
            let mut ask = |_: &Path| answers.next().expect("asked once more");
            let mut x = Extracting { overwrite: Overwrite::Ask, ..Extracting::new(&mut ask) };
            extract_with(v(), &d, &mut x).unwrap();
            assert_eq!([read("old"), read("new")], want.map(String::from));
            assert_eq!(read(".aqr/sfx"), "archive's");
        }
        there();
        let mut quit = |_: &Path| Answer::Quit;
        let e = extract_with(v(), &d, &mut Extracting { overwrite: Overwrite::Ask, ..Extracting::new(&mut quit) });
        assert!(e.unwrap_err().contains("stopped"));

        // what was made removed, the destination's directories among them; nothing else
        there();
        let dest = d.join("a/b");
        let mut x = Extracting::new(&mut never);
        assert_eq!(extract_with(v(), &dest, &mut x), Ok(3));
        assert_eq!(x.made.len(), 2 + 4);
        assert_eq!(clean(&x.made), 0);
        assert!(!d.join("a").exists() && d.join("old").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    fn entry(parent: Option<usize>, name: &[u8], node: EntryNode) -> Entry {
        Entry { parent, name: name.to_vec(), node, extra: Extra::default() }
    }

    /// Names that collide once sanitised (`a:b`, `a_b`) are a failure to write the second, not
    /// one written over the first; the first stays. Links and special entries are left out (as
    /// this first stub has it), not counted, and what is after them is written.
    #[test]
    fn collisions_and_links() {
        let d = std::env::temp_dir().join(format!("aqr-sfx-collide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = Tree {
            entries: vec![
                entry(None, b"a:b", EntryNode::File(0)),
                entry(None, b"link", EntryNode::Link(b"a:b".to_vec())),
                entry(None, b"special", EntryNode::Special(1)),
                entry(None, b"fine", EntryNode::File(1)),
                entry(None, b"a_b", EntryNode::File(1)),
            ],
            ..Tree::default()
        };
        let v = || Value::Tree(Box::new(t.clone()), vec![b"first".to_vec(), b"second".to_vec()]);
        let e = extract(v(), &d).unwrap_err();
        assert!(e.contains("a_b") && e.contains("there already, not written over"), "{e}");
        assert_eq!(std::fs::read(d.join("a_b")).unwrap(), b"first");
        assert_eq!(std::fs::read(d.join("fine")).unwrap(), b"second");
        assert!(!d.join("link").exists() && !d.join("special").exists());
        // (without the collision: two files, and the link and special entry not counted)
        let _ = std::fs::remove_dir_all(&d);
        let mut t2 = t.clone();
        t2.entries.pop();
        assert_eq!(extract(Value::Tree(Box::new(t2), vec![b"first".to_vec(), b"second".to_vec()]), &d), Ok(2));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A name that is nothing but what is replaced (a dot, a space, `:`) still has a name, and a
    /// device's name as a directory makes a directory of the replaced name that files go in.
    #[test]
    fn names_that_sanitize_to_little() {
        for (name, want) in [(" ", "_"), (".", "_"), (":", "_"), ("::", "__"), ("\u{1}", "_"), ("CON ", "CON_")] {
            let got = sanitized(name.as_bytes());
            assert_eq!(got, want, "{name:?}");
            assert!(!got.is_empty() && got != "." && got != "..");
        }
        let d = std::env::temp_dir().join(format!("aqr-sfx-device-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = Tree {
            entries: vec![
                entry(None, b"CON", EntryNode::Dir),
                entry(Some(0), b"x:y", EntryNode::File(0)),
                entry(None, b" ", EntryNode::File(0)),
            ],
            ..Tree::default()
        };
        assert_eq!(extract(Value::Tree(Box::new(t), vec![b"c".to_vec()]), &d), Ok(2));
        assert_eq!(std::fs::read(d.join("_CON/x_y")).unwrap(), b"c");
        assert_eq!(std::fs::read(d.join("_")).unwrap(), b"c");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn names_stay_in_their_directory() {
        for (name, want) in [
            ("a.txt", "a.txt"),
            ("C:evil", "C_evil"),
            ("x:ads", "x_ads"),
            ("\\\\host\\share", "__host_share"),
            ("..\\..\\x", ".._.._x"),
            ("a/b", "a_b"),
            ("what?", "what_"),
            ("dots..", "dots._"),
            ("con", "_con"),
            ("NUL.tar.gz", "_NUL.tar.gz"),
            ("COM1", "_COM1"),
            ("COM10", "COM10"),
            ("console", "console"),
        ] {
            assert_eq!(sanitized(name.as_bytes()), want, "{name}");
        }
    }
}
