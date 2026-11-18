//! The self-extracting apply logic, shared by the stub (`aqr-sfx-shim`) and the tool that builds
//! self-extracting archives (`aqr --format sfx`): [`Sfx`] finds the blobs merged into the program
//! through the dispatch table `aqr-ld merge` wrote into the anchor -- the native module blobs
//! (aqr-dll), each named by its module's hash ([`native_name`]), and the interpreter blob
//! (`aqr-interp`, [`INTERP`]) for any other module -- and is the [`Runner`] the decoder runs each
//! module through; [`Sfx::load`] takes the same blobs from a [`bundle`] (what `aqr --format sfx`
//! appends to the stub), placing them in memory; [`extract`] writes a decoded archive's files
//! out. The blobs are Windows x86-64 only; elsewhere [`Sfx::find`] finds none.

pub mod bundle;
mod native;
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

use std::path::Path;

use aqr_archive::{EntryNode, Value};

/// Writes the files of `value` under `dest`, returning how many were written. Nothing there is
/// written over or through: a file there already fails, and so does a link (or junction) where a
/// directory of the archive goes, which would lead out of `dest`.
pub fn extract(value: Value, dest: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    match value {
        Value::File(data) => {
            write(&dest.join("aqr-sfx-output"), &data)?;
            Ok(1)
        }
        Value::Tree(tree, contents) => {
            let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(tree.entries.len());
            let mut files = 0;
            for e in &tree.entries {
                // the entries come each after its parent, so the parent's path is known
                let base = match e.parent {
                    Some(p) => paths[p].clone(),
                    None => dest.to_path_buf(),
                };
                let path = base.join(sanitized(&e.name));
                match &e.node {
                    EntryNode::Dir => dir(&path)?,
                    EntryNode::File(c) => {
                        let data = contents.get(*c).ok_or_else(|| format!("{}: no content", path.display()))?;
                        write(&path, data)?;
                        files += 1;
                    }
                    // links and special entries are left out of this first stub
                    _ => {}
                }
                paths.push(path);
            }
            Ok(files)
        }
    }
}

/// Makes directory `path` (its parent there), or takes the one there: not a link to one.
fn dir(path: &Path) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(err(&"there already, not a directory (or a link), not written through")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(path).map_err(|e| err(&e)),
        Err(e) => Err(err(&e)),
    }
}

/// Writes `data` to `path` (its directory made already), a new file: not over one there, nor
/// through a link there.
fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{}: there already, not written over", path.display()),
        _ => format!("{}: {e}", path.display()),
    };
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(err)?;
    f.write_all(data).map_err(err)
}

/// An entry name (WTF-8) as a path component, lossily, as a Windows name cannot escape its
/// directory: separators, `..` and the characters a name cannot have (`<>:"|?*`, controls: a
/// `C:x` would replace the directory, `x:y` name a stream) as `_`, a dot or a space at its end
/// as `_`, and `_` before a device's name (`CON`, `NUL.txt`, ...; as `aqr`'s own extraction).
fn sanitized(name: &[u8]) -> String {
    let bad = |c: char| c < ' ' || "<>:\"|?*/\\".contains(c);
    let mut s: String = String::from_utf8_lossy(name).replace(bad, "_").replace("..", "_");
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

    #[test]
    fn names_stay_in_their_directory() {
        for (name, want) in [
            ("a.txt", "a.txt"),
            ("C:evil", "C_evil"),
            ("x:ads", "x_ads"),
            ("\\\\host\\share", "__host_share"),
            ("..\\..\\x", "____x"),
            ("a/b", "a_b"),
            ("what?", "what_"),
            ("dots..", "dots_"),
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
