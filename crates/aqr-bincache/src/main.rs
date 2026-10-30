//! `cargo aqr` (an alias in `.cargo/config.toml`): builds and runs the release `aqr`, keeping each
//! binary built by a key over everything it was built from.
//!
//! - `cargo aqr <args>…` runs the binary of the working copy, building it first (`cargo build -r
//!   -p aqr`) unless one with the same key is kept.
//! - `cargo aqr +` lists the kept binaries: id, when built, when last used, commit.
//! - `cargo aqr +<id> <args>…` runs the kept binary whose id starts with `<id>`.
//!
//! The key is a git hash over the toolchain (`rustc -vV`), the environment that changes the build
//! (`RUSTFLAGS`, `CARGO_PROFILE_*`, …, `$CARGO_HOME/config.toml`) and the git blob id of every
//! file in a manifest. A manifest is the dep-info of a build (`target/release/aqr.d`: the sources
//! of the workspace's crates, `include_bytes!`ed files among them) with the `Cargo.toml`s of
//! their crates, the root `Cargo.toml`, `Cargo.lock` (for registry crates) and
//! `.cargo/config.toml`. A lookup tries each kept manifest: if every file in one is unchanged, so
//! is the set of files the build reads, the way ccache's manifests work.
//!
//! The binaries are in `crates/aqr-bincache/cache/` (gitignored), the `AQR_BINCACHE_MAX` (16)
//! least recently used ones kept. Debug info is not kept: on macOS it stays in `target/`'s object
//! files, so backtraces of a kept binary lose their lines once `target/` is cleaned.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let result = match args.first().and_then(|a| a.to_str()).and_then(|a| a.strip_prefix('+')) {
        Some("") if args.len() == 1 => list(),
        Some("") => Err("usage: cargo aqr +".into()),
        Some(id) => find(id).and_then(|dir| run(&dir, &args[1..])),
        None => current().and_then(|dir| run(&dir, &args)),
    };
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

/// The kept binary of the working copy, built and kept first if there is none.
fn current() -> Result<PathBuf> {
    let manifests = manifests()?;
    let env = environment()?;
    let blobs = blobs(manifests.values().flatten())?;
    for paths in manifests.values() {
        let dir = cache().join("bin").join(key(&env, paths, &blobs)?);
        if dir.join("info").exists() {
            return Ok(dir);
        }
    }
    build(&env)
}

fn build(env: &str) -> Result<PathBuf> {
    let start = SystemTime::now();
    let cargo = std::env::var_os("CARGO").unwrap_or("cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .args(["build", "-r", "-p", "aqr", "--bin", "aqr"])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("cargo build failed".into());
    }
    let exe = target().join(format!("aqr{EXE}"));
    let paths = dep_info(&target().join("aqr.d"))?;
    let changed =
        paths.iter().find(|p| fs::metadata(root().join(p)).and_then(|m| m.modified()).is_ok_and(|t| t >= start));
    if let Some(path) = changed {
        eprintln!("aqr-bincache: {path} changed during the build; running the binary without keeping it");
        return Ok(target());
    }
    let blobs = blobs(&paths)?;
    let key = key(env, &paths, &blobs)?;
    let bin = cache().join("bin");
    let dir = bin.join(&key);
    if dir.join("info").exists() {
        return Ok(dir);
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
    fs::copy(&exe, tmp.join(format!("aqr{EXE}"))).map_err(|e| format!("{}: {e}", exe.display()))?;
    let info = format!("created={}\nmanifest={manifest}\n{}", now(), commit(&paths, &blobs)?);
    fs::write(tmp.join("info"), info).map_err(|e| e.to_string())?;
    let kept = keep(&tmp, &dir, |from, to| fs::rename(from, to));
    evict()?;
    Ok(kept.unwrap_or_else(|| {
        eprintln!("aqr-bincache: {} not kept; running the binary without keeping it", dir.display());
        target()
    }))
}

/// Moves the binary made in `tmp` to `dir` by `rename`, giving `dir`; tried again for a while
/// where it fails (on Windows, something holding the directory open for a moment, such as
/// Dropbox or an antivirus). Gives `dir` where another session kept the same key meanwhile
/// (theirs is as good), and nothing where it is not there.
fn keep(tmp: &Path, dir: &Path, rename: impl Fn(&Path, &Path) -> std::io::Result<()>) -> Option<PathBuf> {
    let kept = || dir.join("info").exists();
    for wait in [0, 50, 200, 1000] {
        std::thread::sleep(Duration::from_millis(wait));
        if rename(tmp, dir).is_ok() {
            return Some(dir.to_path_buf());
        }
        if kept() {
            break;
        }
    }
    let _ = fs::remove_dir_all(tmp);
    kept().then(|| dir.to_path_buf())
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
            matches!(&k[..], "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "RUSTC" | "RUSTC_WRAPPER")
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
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    writer.join().unwrap().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// The `commit=`, `dirty=` and `subject=` lines of an info file: HEAD, and whether any file
/// differs from it.
fn commit(paths: &BTreeSet<String>, blobs: &BTreeMap<String, String>) -> Result<String> {
    let head = git(&["log", "-1", "--format=%h%n%s"], b"")?;
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
            let info = text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.into(), v.into())).collect();
            let used = fs::metadata(e.path().join("used")).or_else(|_| fs::metadata(e.path().join("info")));
            let used = used.and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
            Some(Entry { key: e.file_name().to_string_lossy().into_owned(), dir: e.path(), info, used })
        })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.info.get("created").and_then(|c| c.parse::<u64>().ok())));
    entries
}

fn find(id: &str) -> Result<PathBuf> {
    let found: Vec<Entry> = entries().into_iter().filter(|e| e.key.starts_with(id)).collect();
    match &found[..] {
        [entry] => Ok(entry.dir.clone()),
        [] => Err(format!("no kept binary +{id} (cargo aqr + lists them)")),
        _ => Err(format!("+{id} is ambiguous")),
    }
}

fn list() -> Result<ExitCode> {
    let entries = entries();
    let current = (|| {
        let manifests = manifests()?;
        let env = environment()?;
        let blobs = blobs(manifests.values().flatten())?;
        manifests.values().map(|paths| key(&env, paths, &blobs)).collect::<Result<BTreeSet<_>>>()
    })()?;
    for e in &entries {
        let get = |k: &str| e.info.get(k).map_or("", String::as_str);
        let created = get("created").parse().unwrap_or(0);
        let used = e.used.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mark = if current.contains(&e.key) { '*' } else { ' ' };
        let dirty = if get("dirty") == "1" { "+" } else { " " };
        println!(
            "{mark} {}  {}  used {}  {}{dirty} {}",
            &e.key[..12],
            local_time(created),
            local_time(used),
            get("commit"),
            get("subject")
        );
    }
    if entries.is_empty() {
        println!("(no kept binaries)");
    }
    Ok(ExitCode::SUCCESS)
}

/// How old a `tmp-*` directory of the cache is (by its modification) once it is no build's.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

fn evict() -> Result<()> {
    let max = std::env::var("AQR_BINCACHE_MAX").ok().and_then(|m| m.parse().ok()).unwrap_or(16);
    let mut entries = entries();
    entries.sort_by_key(|e| std::cmp::Reverse(e.used));
    for e in entries.drain(max.min(entries.len())..) {
        let _ = fs::remove_dir_all(&e.dir);
    }
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

/// Runs the `aqr` in `dir` (a kept binary, or `target/release`) with `args`.
fn run(dir: &Path, args: &[OsString]) -> Result<ExitCode> {
    if dir.join("info").exists() {
        let used = fs::File::options().create(true).append(true).open(dir.join("used"));
        let _ = used.and_then(|f| f.set_modified(SystemTime::now()));
    }
    let mut cmd = Command::new(dir.join(format!("aqr{EXE}")));
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("{}: {}", dir.display(), cmd.exec()))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::{deps, keep};
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
        assert_eq!(keep(&tmp, &dir, failing(2)), Some(dir.clone()));
        assert_eq!(fs::read_to_string(dir.join("info")).unwrap(), "ours");
        assert!(!tmp.exists());

        let root = scratch("not-moved");
        let (tmp, dir) = (made(&root, "ours"), root.join("bin"));
        assert_eq!(keep(&tmp, &dir, failing(usize::MAX)), None);
        assert!(!tmp.exists());
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
        assert_eq!(keep(&tmp, &dir, |from, to| fs::rename(from, to)), Some(dir.clone()));
        assert_eq!(fs::read_to_string(dir.join("info")).unwrap(), "theirs");
        assert!(!tmp.exists());
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
