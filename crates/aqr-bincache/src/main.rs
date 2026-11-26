//! `cargo aqr` and `cargo aqr-win` (aliases in `.cargo/config.toml`, the program's name the first
//! argument): builds and runs the release binary of a program, keeping each binary built by a key
//! over everything it was built from.
//!
//! - `cargo PROGRAM <args>…` runs the binary of the working copy, building it first (`cargo build
//!   -r -p PROGRAM`) unless one with the same key is kept.
//! - `cargo PROGRAM +` lists the program's kept binaries: id, when built, when last used, commit.
//! - `cargo PROGRAM +<id> <args>…` runs the kept binary whose id starts with `<id>`.
//!
//! The programs are those of [`PROGRAMS`]: `aqr`, and `aqr-win` (Windows), which runs the `aqr`
//! beside it: its kept binary has the kept `aqr` of the working copy beside it (a hard link, else
//! a copy), the key of that `aqr` in its own, so `cargo aqr-win --register` points Explorer's
//! verbs at a pair that stays as it is; its shell extension (`aqr_shell.dll`, crates/aqr-win-shell)
//! is built and kept with it, beside it. A binary run with the arguments that tell the system of
//! it (`--register`) is pinned, never evicted, until it is run with those that take it back
//! (`--unregister`; `cache/pinned/PROGRAM`).
//!
//! The key is a git hash over the toolchain (`rustc -vV`), the environment that changes the build
//! (`RUSTFLAGS`, `CARGO_PROFILE_*`, …, `$CARGO_HOME/config.toml`), the program (and the keys of
//! those beside it) and the git blob id of every file in a manifest. A manifest is the dep-info
//! of a build (`target/release/PROGRAM.d`: the sources of the workspace's crates,
//! `include_bytes!`ed files among them) with the `Cargo.toml`s of their crates, the root
//! `Cargo.toml`, `Cargo.lock` (for registry crates) and `.cargo/config.toml`. A lookup tries each
//! kept manifest: if every file in one is unchanged, so is the set of files the build reads, the
//! way ccache's manifests work.
//!
//! The binaries are in `crates/aqr-bincache/cache/` (gitignored), the `AQR_BINCACHE_MAX` (16)
//! least recently used ones of all programs kept (and the pinned ones). Debug info is not kept:
//! on macOS it stays in `target/`'s object files, so backtraces of a kept binary lose their lines
//! once `target/` is cleaned.
//!
//! Before the binary of `aqr` is looked up, the Wasm modules and the SFX pack are made again where
//! what they are made of changed ([`modules`]: build.py and pack.py run as needed). The pack is no
//! part of aqr's build: the binary kept has it written in (Windows x86-64: [`modules::embed`]),
//! its `pack.id` in the manifest (`-` where there is no pack).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod modules;

type Result<T> = std::result::Result<T, String>;

/// A program kept: the binary of its package of the same name.
struct Program {
    name: &'static str,
    /// Of the Wasm modules and the SFX pack ([`modules`]): they are made again before it is looked
    /// up, and the pack is written into the binary kept.
    modules: bool,
    /// The programs whose kept binaries go beside it (it runs them as the ones beside itself).
    beside: &'static [&'static str],
    /// The libraries built and kept with it, beside it: each a package and its library's name
    /// (aqr-win's shell extension, which it registers as the one beside itself).
    libraries: &'static [(&'static str, &'static str)],
    /// The first argument of a run that, where it succeeds, pins the binary run (what the system
    /// was told of it: aqr-win's verbs in the registry), and that of one unpinning it.
    pin: Option<(&'static str, &'static str)>,
}

const PROGRAMS: &[Program] = &[
    Program { name: "aqr", modules: true, beside: &[], libraries: &[], pin: None },
    Program {
        name: "aqr-win",
        modules: false,
        beside: &["aqr"],
        libraries: &[("aqr-win-shell", "aqr_shell")],
        pin: Some(("--register", "--unregister")),
    },
];

fn program(name: &str) -> Result<&'static Program> {
    PROGRAMS.iter().find(|p| p.name == name).ok_or_else(|| {
        let names: Vec<&str> = PROGRAMS.iter().map(|p| p.name).collect();
        format!("usage: aqr-bincache {} [+[ID]] ARGS…", names.join("|"))
    })
}

/// The file name of a program's binary.
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The file name of a library's binary (of a `cdylib`).
fn dll(name: &str) -> String {
    format!("{}{name}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX)
}

/// The binary of a program to run, and its key where it is kept.
struct Built {
    exe: PathBuf,
    key: Option<String>,
}

/// A kept binary of a program beside another's.
struct Beside {
    name: &'static str,
    key: String,
    exe: PathBuf,
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let name = args.next().unwrap_or_default();
    let args: Vec<OsString> = args.collect();
    let result = program(&name.to_string_lossy()).and_then(|p| {
        match args.first().and_then(|a| a.to_str()).and_then(|a| a.strip_prefix('+')) {
            Some("") if args.len() == 1 => list(p),
            Some("") => Err(format!("usage: cargo {} +", p.name)),
            Some(id) => find(p, id).and_then(|dir| run(p, &dir.join(exe(p.name)), &args[1..])),
            None => current(p).and_then(|built| run(p, &built.exe, &args)),
        }
    });
    match result {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("aqr-bincache: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace's root, spelled as Cargo spells the paths of dep-info (not canonicalized, which
/// on Windows gives a `\\?\` path no dep-info path starts with).
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

fn cache() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("cache")
}

fn target() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from).join("release")
}

/// The kept binary of the working copy, built and kept first if there is none (the one built,
/// with the pack, where it is not kept), those beside it first.
fn current(p: &Program) -> Result<Built> {
    if p.modules {
        modules::refresh()?;
    }
    let mut beside = Vec::new();
    for &name in p.beside {
        let built = current(program(name)?)?;
        let key = built.key.ok_or_else(|| format!("{name} was not kept, and {} needs it beside it", p.name))?;
        beside.push(Beside { name, key, exe: built.exe });
    }
    let env = environment()? + &program_env(p, &beside);
    if let Some(key) = kept(&env)? {
        return Ok(Built { exe: cache().join("bin").join(&key).join(exe(p.name)), key: Some(key) });
    }
    build(p, &env, &beside)
}

/// The key of the kept binary of the working copy, where there is one (nothing built or made).
fn current_key(p: &Program) -> Result<Option<String>> {
    let mut beside = Vec::new();
    for &name in p.beside {
        let Some(key) = current_key(program(name)?)? else { return Ok(None) };
        beside.push(Beside { name, key, exe: PathBuf::new() });
    }
    kept(&(environment()? + &program_env(p, &beside)))
}

/// The key of a kept binary of the environment `env` whose manifest's files are as they are.
fn kept(env: &str) -> Result<Option<String>> {
    let manifests = manifests()?;
    let blobs = blobs(manifests.values().flatten())?;
    for paths in manifests.values() {
        let key = key(env, paths, &blobs)?;
        if cache().join("bin").join(&key).join("info").exists() {
            return Ok(Some(key));
        }
    }
    Ok(None)
}

/// What the key has of the program besides [`environment`]: nothing for `aqr` (its keys as they
/// were before there were other programs).
fn program_env(p: &Program, beside: &[Beside]) -> String {
    if p.name == "aqr" {
        return String::new();
    }
    let mut text = format!("program={}\n", p.name);
    for b in beside {
        text += &format!("beside {}={}\n", b.name, b.key);
    }
    for (_, l) in p.libraries {
        text += &format!("library {l}\n");
    }
    text
}

/// Builds the program and keeps it with those beside it, giving the binary to run.
fn build(p: &Program, env: &str, beside: &[Beside]) -> Result<Built> {
    let start = SystemTime::now();
    let cargo = std::env::var_os("CARGO").unwrap_or("cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .args(["build", "-r", "-p", p.name, "--bin", p.name])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }
    let mut built = target().join(exe(p.name));
    let mut paths = dep_info(&target().join(format!("{}.d", p.name)))?;
    for (package, name) in p.libraries {
        let status = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()))
            .current_dir(root())
            .args(["build", "-r", "-p", package, "--lib"])
            .status()
            .map_err(|e| format!("cargo: {e}"))?;
        if !status.success() {
            return Err(format!("cargo build of {package} failed"));
        }
        paths.extend(dep_info(&target().join(format!("{name}.d")))?);
    }
    if p.modules {
        // (the pack written into a copy in the target directory, the one run where the binary is
        // not kept: a file just written in the cache, which a sync client such as Dropbox may be
        // reading, is not opened for the update)
        let packed = target().join(exe(&format!("{}-packed", p.name)));
        fs::copy(&built, &packed).map_err(|e| format!("{}: {e}", built.display()))?;
        modules::embed(&packed)?;
        paths.extend(modules::pack_id());
        built = packed;
    }
    let unkept = || unkept(p, &built, beside).map(|exe| Built { exe, key: None });
    let made = target().parent().map_or_else(target, Path::to_path_buf);
    if let Some(path) = changed_since(&paths, &root(), &made, start) {
        eprintln!("aqr-bincache: {path} changed during the build; running the binary without keeping it");
        return unkept();
    }
    let blobs = blobs(&paths)?;
    let key = key(env, &paths, &blobs)?;
    let bin = cache().join("bin");
    let dir = bin.join(&key);
    let kept = || Built { exe: dir.join(exe(p.name)), key: Some(key.clone()) };
    if dir.join("info").exists() {
        return Ok(kept());
    }
    let text: String = paths.iter().map(|p| format!("{p}\n")).collect();
    let manifest = hash(text.as_bytes())?;
    let tmp = cache().join(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    fs::create_dir_all(cache().join("manifests")).map_err(|e| e.to_string())?;
    let dest = cache().join("manifests").join(&manifest);
    if !dest.exists() {
        fs::write(tmp.join("manifest"), &text).map_err(|e| e.to_string())?;
        fs::rename(tmp.join("manifest"), &dest).map_err(|e| e.to_string())?;
    }
    fs::copy(&built, tmp.join(exe(p.name))).map_err(|e| format!("{}: {e}", built.display()))?;
    for (_, name) in p.libraries {
        let from = target().join(dll(name));
        fs::copy(&from, tmp.join(dll(name))).map_err(|e| format!("{}: {e}", from.display()))?;
    }
    let mut info = format!("created={}\nmanifest={manifest}\nprogram={}\n", now(), p.name);
    for b in beside {
        link_or_copy(&b.exe, &tmp.join(exe(b.name)))?;
        info += &format!("beside.{}={}\n", b.name, b.key);
    }
    info += &commit(&paths, &blobs)?;
    fs::write(tmp.join("info"), info).map_err(|e| e.to_string())?;
    let moved = keep(&tmp, &dir, KEEP_WAITS, |from, to| fs::rename(from, to));
    evict()?;
    match moved {
        Ok(_) => Ok(kept()),
        Err(e) => {
            eprintln!("aqr-bincache: {} not kept ({e}); running the binary without keeping it", dir.display());
            unkept()
        }
    }
}

/// The binary `built` of `p` to run where it is not kept: itself, or where others go beside it,
/// a copy beside copies of theirs (in `target/release/PROGRAM-unkept/`).
fn unkept(p: &Program, built: &Path, beside: &[Beside]) -> Result<PathBuf> {
    if beside.is_empty() && p.libraries.is_empty() {
        return Ok(built.to_path_buf());
    }
    let dir = target().join(format!("{}-unkept", p.name));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let all = std::iter::once((built.to_path_buf(), exe(p.name)))
        .chain(beside.iter().map(|b| (b.exe.clone(), exe(b.name))))
        .chain(p.libraries.iter().map(|(_, l)| (target().join(dll(l)), dll(l))));
    for (from, name) in all {
        fs::copy(&from, dir.join(name)).map_err(|e| format!("{} to {}: {e}", from.display(), dir.display()))?;
    }
    Ok(dir.join(exe(p.name)))
}

/// A hard link to `from` at `to`, else a copy (another volume, a file system of no links).
fn link_or_copy(from: &Path, to: &Path) -> Result<()> {
    fs::hard_link(from, to).or_else(|_| fs::copy(from, to).map(drop)).map_err(|e| format!("{}: {e}", from.display()))
}

/// The first of `paths` (relative to `root`, or absolute) modified since `start`, but those under
/// `built` (the target directory): what the build made itself, such as a build script's files
/// in its `OUT_DIR`, is no source changed during it.
fn changed_since<'a>(paths: &'a BTreeSet<String>, root: &Path, built: &Path, start: SystemTime) -> Option<&'a String> {
    paths
        .iter()
        .map(|p| (p, root.join(p)))
        .filter(|(_, path)| !path.starts_with(built))
        .find_map(|(p, path)| fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|t| t >= start).then_some(p))
}

/// The waits (ms) before each try of [`keep`]: 12.75 s in all.
const KEEP_WAITS: &[u64] = &[0, 50, 100, 200, 400, 800, 1600, 3200, 6400];

/// Moves the binary made in `tmp` to `dir` by `rename`, giving `dir`; tried after each of
/// `waits` where it fails (on Windows, a file in it held open for a moment by what reads a file
/// just written, such as Dropbox or an antivirus: 0.4 s to over 1.25 s for aqr.exe in a Dropbox
/// folder). Gives `dir` where another session kept the same key meanwhile (theirs is as good),
/// and the last failure where it is not there.
fn keep(
    tmp: &Path,
    dir: &Path,
    waits: &[u64],
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    let kept = || dir.join("info").exists();
    let mut failure = None;
    for &wait in waits {
        std::thread::sleep(Duration::from_millis(wait));
        match rename(tmp, dir) {
            Ok(()) => return Ok(dir.to_path_buf()),
            Err(e) => failure = Some(e),
        }
        if kept() {
            break;
        }
    }
    let _ = fs::remove_dir_all(tmp);
    if kept() { Ok(dir.to_path_buf()) } else { Err(failure.unwrap()) }
}

/// The paths (relative to the root where under it) in a dep-info file, with the manifests of
/// their crates and the files every build reads.
fn dep_info(path: &Path) -> Result<BTreeSet<String>> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = root();
    let mut paths = BTreeSet::new();
    let add = |paths: &mut BTreeSet<String>, p: &Path| {
        let p = p.strip_prefix(&root).unwrap_or(p);
        paths.insert(p.to_string_lossy().replace('\\', "/"));
    };
    for dep in deps(&text) {
        add(&mut paths, Path::new(&dep));
    }
    let crates: BTreeSet<PathBuf> = paths
        .iter()
        .filter_map(|p| {
            let p = root.join(p);
            p.ancestors()
                .skip(1)
                .take_while(|a| a.starts_with(&root))
                .find(|a| a.join("Cargo.toml").exists())
                .map(Path::to_path_buf)
        })
        .collect();
    for dir in crates {
        add(&mut paths, &dir.join("Cargo.toml"));
    }
    for p in ["Cargo.toml", "Cargo.lock", ".cargo/config.toml"] {
        add(&mut paths, Path::new(p));
    }
    Ok(paths)
}

/// The prerequisites of the first rule of a dep-info file, unescaped.
fn deps(text: &str) -> Vec<String> {
    let deps = text.lines().next().and_then(|l| l.split_once(": ")).map_or("", |(_, d)| d);
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = deps.chars();
    while let Some(c) = chars.next() {
        match c {
            // Cargo escapes only spaces; any other backslash is a Windows path separator.
            '\\' if chars.as_str().starts_with(' ') => word.extend(chars.next()),
            ' ' => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// What changes a build besides the files: the toolchain and the environment.
fn environment() -> Result<String> {
    let rustc = std::env::var_os("RUSTC").unwrap_or("rustc".into());
    let out = Command::new(rustc).current_dir(root()).arg("-vV").output().map_err(|e| format!("rustc: {e}"))?;
    let mut text = String::from("aqr-bincache 2\n");
    text += &String::from_utf8_lossy(&out.stdout);
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| {
            // (and the pack written into the binary kept, modules::embed)
            matches!(&k[..], "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "RUSTC" | "RUSTC_WRAPPER" | "AQR_SFX_PACK")
                || ["CARGO_PROFILE_", "CARGO_BUILD_", "CARGO_TARGET_"].iter().any(|p| k.starts_with(p))
                    && !k.ends_with("TARGET_DIR")
        })
        .collect();
    vars.sort();
    for (k, v) in vars {
        text += &format!("{k}={v}\n");
    }
    let home = std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| Path::new(&h).join(".cargo"))
    });
    if let Some(config) = home.and_then(|h| fs::read_to_string(h.join("config.toml")).ok()) {
        text += "$CARGO_HOME/config.toml\n";
        text += &config;
    }
    Ok(text)
}

/// The git blob ids of the files, `-` for the missing ones.
fn blobs<'a>(paths: impl IntoIterator<Item = &'a String>) -> Result<BTreeMap<String, String>> {
    let root = root();
    let mut blobs = BTreeMap::new();
    let present: Vec<&String> = paths
        .into_iter()
        .filter(|p| {
            let ok = root.join(p).is_file();
            if !ok {
                blobs.insert(p.to_string(), "-".into());
            }
            ok
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let input: String = present.iter().map(|p| format!("{p}\n")).collect();
    let out = git(&["hash-object", "--stdin-paths"], input.as_bytes())?;
    let ids: Vec<&str> = out.lines().collect();
    if ids.len() != present.len() {
        return Err("git hash-object: unexpected output".into());
    }
    blobs.extend(present.into_iter().zip(ids).map(|(p, id)| (p.clone(), id.to_string())));
    Ok(blobs)
}

fn key(env: &str, paths: &BTreeSet<String>, blobs: &BTreeMap<String, String>) -> Result<String> {
    let mut text = env.to_string();
    for p in paths {
        text += &format!("{} {p}\n", blobs[p]);
    }
    hash(text.as_bytes())
}

fn hash(data: &[u8]) -> Result<String> {
    Ok(git(&["hash-object", "--stdin"], data)?.trim().to_string())
}

fn git(args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(root())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    writer.join().unwrap().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), err.trim_end()));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// The `commit=`, `dirty=` and `subject=` lines of an info file: HEAD, and whether any file
/// differs from it. Outside a git working copy (sources from `git archive`), no commit and
/// dirty: the blob ids are still git's, which `git hash-object` gives anywhere.
fn commit(paths: &BTreeSet<String>, blobs: &BTreeMap<String, String>) -> Result<String> {
    let Ok(head) = git(&["log", "-1", "--format=%h%n%s"], b"") else {
        return Ok("commit=-------\ndirty=1\nsubject=(not a git working copy)\n".into());
    };
    let (hash, subject) = head.trim_end().split_once('\n').unwrap_or((head.trim_end(), ""));
    let tree = git(&["ls-tree", "-r", "HEAD"], b"")?;
    let committed: BTreeMap<&str, &str> = tree
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(meta, path)| Some((path, meta.split(' ').nth(2)?)))
        .collect();
    let dirty =
        paths.iter().any(|p| Path::new(p).is_absolute() || committed.get(&p[..]).is_none_or(|id| *id != blobs[p]));
    Ok(format!("commit={hash}\ndirty={}\nsubject={subject}\n", dirty as u8))
}

fn manifests() -> Result<BTreeMap<String, BTreeSet<String>>> {
    let Ok(dir) = fs::read_dir(cache().join("manifests")) else { return Ok(BTreeMap::new()) };
    let mut manifests = BTreeMap::new();
    for entry in dir.flatten() {
        let text = fs::read_to_string(entry.path()).map_err(|e| e.to_string())?;
        manifests.insert(entry.file_name().to_string_lossy().into_owned(), text.lines().map(String::from).collect());
    }
    Ok(manifests)
}

struct Entry {
    key: String,
    /// The program (`aqr` where the info has none, as those kept before there were others).
    program: String,
    dir: PathBuf,
    info: BTreeMap<String, String>,
    used: SystemTime,
}

fn entries() -> Vec<Entry> {
    let Ok(dir) = fs::read_dir(cache().join("bin")) else { return Vec::new() };
    let mut entries: Vec<Entry> = dir
        .flatten()
        .filter_map(|e| {
            let text = fs::read_to_string(e.path().join("info")).ok()?;
            let info: BTreeMap<String, String> =
                text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
            let used = fs::metadata(e.path().join("used")).or_else(|_| fs::metadata(e.path().join("info")));
            let used = used.and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
            let program = info.get("program").map_or("aqr", String::as_str).to_string();
            Some(Entry { key: e.file_name().to_string_lossy().into_owned(), program, dir: e.path(), info, used })
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.info.get("created").and_then(|c| c.parse::<u64>().ok())));
    entries
}

/// The kept binaries of a program.
fn entries_of(p: &Program) -> Vec<Entry> {
    entries().into_iter().filter(|e| e.program == p.name).collect()
}

fn find(p: &Program, id: &str) -> Result<PathBuf> {
    let found: Vec<Entry> = entries_of(p).into_iter().filter(|e| e.key.starts_with(id)).collect();
    match &found[..] {
        [entry] => Ok(entry.dir.clone()),
        [] => Err(format!("no kept binary +{id} of {} (cargo {} + lists them)", p.name, p.name)),
        _ => Err(format!("+{id} is ambiguous")),
    }
}

fn list(p: &Program) -> Result<ExitCode> {
    let entries = entries_of(p);
    let current = current_key(p)?;
    let pinned = pinned();
    let mut out = std::io::stdout().lock();
    let written = (|| {
        for e in &entries {
            let get = |k: &str| e.info.get(k).map_or("", String::as_str);
            let created = get("created").parse().unwrap_or(0);
            let used = e.used.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mark = if current.as_ref() == Some(&e.key) { '*' } else { ' ' };
            let pin = if pinned.contains(&e.key) { "  (pinned)" } else { "" };
            let dirty = if get("dirty") == "1" { "+" } else { " " };
            writeln!(
                out,
                "{mark} {}  {}  used {}  {}{dirty} {}{pin}",
                &e.key[..12],
                local_time(created),
                local_time(used),
                get("commit"),
                get("subject"),
            )?;
        }
        if entries.is_empty() {
            writeln!(out, "(no kept binaries)")?;
        }
        out.flush()
    })();
    match written {
        // (read only in part, as by `head`: not an error)
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(format!("writing the list: {e}")),
        _ => Ok(ExitCode::SUCCESS),
    }
}

/// How old a `tmp-*` directory of the cache is (by its modification) once it is no build's.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// The keys of the pinned binaries (`cache/pinned/PROGRAM`: the key of the program's).
fn pinned() -> BTreeSet<String> {
    let Ok(dir) = fs::read_dir(cache().join("pinned")) else { return BTreeSet::new() };
    dir.flatten().filter_map(|e| fs::read_to_string(e.path()).ok()).map(|k| k.trim().to_string()).collect()
}

fn evict() -> Result<()> {
    let max = std::env::var("AQR_BINCACHE_MAX").ok().and_then(|m| m.parse().ok()).unwrap_or(16);
    let pinned = pinned();
    let (mut entries, kept): (Vec<Entry>, Vec<Entry>) = entries().into_iter().partition(|e| !pinned.contains(&e.key));
    entries.sort_by_key(|e| std::cmp::Reverse(e.used));
    for e in entries.drain(max.min(entries.len())..) {
        let _ = fs::remove_dir_all(&e.dir);
    }
    entries.extend(kept);
    // binaries made but not kept (their removal failed), but those of builds going on
    let stale = |m: fs::Metadata| m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > STALE);
    for entry in fs::read_dir(cache()).map_err(|e| e.to_string())?.flatten() {
        if entry.file_name().to_string_lossy().starts_with("tmp-") && entry.metadata().is_ok_and(stale) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    let kept: BTreeSet<&str> = entries.iter().filter_map(|e| e.info.get("manifest")).map(String::as_str).collect();
    for entry in fs::read_dir(cache().join("manifests")).map_err(|e| e.to_string())?.flatten() {
        if !kept.contains(&*entry.file_name().to_string_lossy()) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `secs` as local time, through date(1) (BSD's `-r`, else GNU's `-d @`).
fn local_time(secs: u64) -> String {
    let fmt = "+%Y-%m-%d %H:%M";
    let bsd = Command::new("date").args(["-r", &secs.to_string(), fmt]).stderr(Stdio::null()).output();
    let out = bsd.ok().filter(|o| o.status.success()).or_else(|| {
        Command::new("date").args(["-d", &format!("@{secs}"), fmt]).output().ok().filter(|o| o.status.success())
    });
    out.map_or_else(|| secs.to_string(), |o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Runs `exe` (a kept binary of `p`, marked used, or the one just built) with `args`, pinning or
/// unpinning it where they are those of [`Program::pin`] and it succeeds.
fn run(p: &Program, exe: &Path, args: &[OsString]) -> Result<ExitCode> {
    let dir = exe.parent().unwrap_or(exe);
    let kept = dir.join("info").exists();
    if kept {
        let used = fs::File::options().create(true).append(true).open(dir.join("used"));
        let _ = used.and_then(|f| f.set_modified(SystemTime::now()));
    }
    let mut cmd = Command::new(exe);
    cmd.args(args);
    let first = args.first().and_then(|a| a.to_str());
    let pin = p.pin.and_then(|(pin, unpin)| [(pin, true), (unpin, false)].into_iter().find(|&(a, _)| Some(a) == first));
    #[cfg(unix)]
    if pin.is_none() {
        use std::os::unix::process::CommandExt;
        return Err(format!("{}: {}", exe.display(), cmd.exec()));
    }
    let status = cmd.status().map_err(|e| format!("{}: {e}", exe.display()))?;
    if let Some((arg, pinning)) = pin.filter(|_| status.success()) {
        let file = cache().join("pinned").join(p.name);
        let done = match (pinning, kept) {
            (true, true) => fs::create_dir_all(cache().join("pinned"))
                .and_then(|()| fs::write(&file, dir.file_name().unwrap_or_default().as_encoded_bytes())),
            (true, false) => {
                eprintln!("aqr-bincache: {} {arg} with a binary not kept: not pinned", p.name);
                Ok(())
            }
            (false, _) => fs::remove_file(&file).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            }),
        };
        done.map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

#[cfg(test)]
mod tests {
    use super::{KEEP_WAITS, changed_since, deps, keep};
    use std::cell::Cell;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    /// A directory of its own for test `name`, empty.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aqr-bincache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A binary made in `tmp` under `root`, its info `info`.
    fn made(root: &Path, info: &str) -> PathBuf {
        let tmp = root.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("info"), info).unwrap();
        tmp
    }

    /// A rename failing the first `n` times (as one of a directory that something holds open for
    /// a moment, on Windows).
    fn failing(n: usize) -> impl Fn(&Path, &Path) -> io::Result<()> {
        let left = Cell::new(n);
        move |from, to| match left.get() {
            0 => fs::rename(from, to),
            k => {
                left.set(k - 1);
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        }
    }

    /// A rename failing for a moment is tried again; one failing for good keeps nothing, rather
    /// than giving a directory that is not there.
    #[test]
    fn kept_only_where_moved() {
        let root = scratch("moved");
        let (tmp, dir) = (made(&root, "ours"), root.join("bin"));
        assert_eq!(keep(&tmp, &dir, KEEP_WAITS, failing(2)).ok(), Some(dir.clone()));
        assert_eq!(fs::read_to_string(dir.join("info")).unwrap(), "ours");
        assert!(!tmp.exists());

        let root = scratch("not-moved");
        let (tmp, dir) = (made(&root, "ours"), root.join("bin"));
        assert!(keep(&tmp, &dir, &[0, 10], failing(usize::MAX)).is_err());
        assert!(!tmp.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A rename failing for a few seconds (a 30 MB binary just written, which Dropbox or an
    /// antivirus is still reading) is tried again until it is moved.
    #[test]
    fn kept_after_seconds() {
        let root = scratch("seconds");
        let (tmp, dir) = (made(&root, "ours"), root.join("bin"));
        let start = std::time::Instant::now();
        let held = |from: &Path, to: &Path| match start.elapsed().as_millis() {
            ..2000 => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            _ => fs::rename(from, to),
        };
        assert_eq!(keep(&tmp, &dir, KEEP_WAITS, held).ok(), Some(dir.clone()));
        fs::remove_dir_all(&root).unwrap();
    }

    /// Where another session kept the same key meanwhile, its binary is taken.
    #[test]
    fn kept_by_another() {
        let root = scratch("another");
        let dir = root.join("bin");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("info"), "theirs").unwrap();
        let tmp = made(&root, "ours");
        assert_eq!(keep(&tmp, &dir, KEEP_WAITS, |from, to| fs::rename(from, to)).ok(), Some(dir.clone()));
        assert_eq!(fs::read_to_string(dir.join("info")).unwrap(), "theirs");
        assert!(!tmp.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A source written during the build is told; what the build wrote in the target directory
    /// (a build script's `OUT_DIR`, as aqr's `sfx.pack`) is not.
    #[test]
    fn changed_sources_only() {
        let root = scratch("changed");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/release/build/x/out")).unwrap();
        let start = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        fs::write(root.join("target/release/build/x/out/sfx.pack"), "made").unwrap();
        let paths = ["src/lib.rs", "target/release/build/x/out/sfx.pack"].map(String::from).into_iter().collect();
        let built = root.join("target");
        assert_eq!(changed_since(&paths, &root, &built, start), None);
        // (also as an absolute path, as dep-info names what is outside the root)
        let abs = [root.join("target/release/build/x/out/sfx.pack").to_string_lossy().into_owned()].into();
        assert_eq!(changed_since(&abs, &root, &built, start), None);
        fs::write(root.join("src/lib.rs"), "edited").unwrap();
        assert_eq!(changed_since(&paths, &root, &built, start).map(String::as_str), Some("src/lib.rs"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deps_unescape_spaces_only() {
        let text = r"C:\w\target\release\aqr.exe: C:\w\a\src\lib.rs C:\w\my\ dir\b.rs

C:\w\a\src\lib.rs:
";
        assert_eq!(deps(text), [r"C:\w\a\src\lib.rs", r"C:\w\my dir\b.rs"]);
        let text = r"/w/target/release/aqr: /w/a/src/lib.rs /w/my\ dir/b.rs";
        assert_eq!(deps(text), ["/w/a/src/lib.rs", "/w/my dir/b.rs"]);
    }
}
