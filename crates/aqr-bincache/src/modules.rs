//! The modules and the SFX pack made again before aqr is built, where what they are made of
//! changed, so that no one has to remember `crates/aqr-modules/build.sh` and
//! `crates/aqr-sfx/pack.sh` (both build the same bytes on any host).
//!
//! - The modules: a stamp (`cache/modules.stamp`) holds the git blob ids of what the last
//!   build.sh read (the dep-info of each module's binary and of aqr-ld, which links them, with
//!   their crates' manifests, build.sh and the toolchain's file) and of what it wrote
//!   (`wasm/*.wasm`, `wasm/*.block`, `src/known.rs`), with the environment and the binaries of
//!   `src/bin`. Where any of it differs, build.sh runs (a minute or so even with nothing to do,
//!   hence the stamp), and the modules whose hashes it changed are told.
//! - The pack (Windows x86-64 alone, where pack.sh runs, and where it is `target/aqr-sfx`, not
//!   `AQR_SFX_PACK`'s): where there is one and its `modules.txt` is not of the modules of
//!   known.rs, pack.sh runs. Where there is none it is left so (pack.sh needs the nightly of
//!   `nightly-toolchain` and build-std); where pack.sh fails, it is not run again for the same
//!   known.rs.
//!
//! `AQR_BINCACHE_MODULES=0` leaves both as they are. One `cargo aqr` at a time does this (a lock
//! on `cache/modules.lock`), as the scripts write the working copy and `target/`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use super::{Result, blobs, cache, dep_info, environment, git, root};

const STAMP: &str = "aqr-bincache modules 1\n";
const BUILD: &str = "crates/aqr-modules/build.sh";
const PACK: &str = "crates/aqr-sfx/pack.sh";
const KNOWN: &str = "crates/aqr-modules/src/known.rs";

/// The modules, and the pack, made again where they are not of their sources.
pub fn refresh() -> Result<()> {
    if std::env::var_os("AQR_BINCACHE_MODULES").is_some_and(|v| v == "0") {
        return Ok(());
    }
    fs::create_dir_all(cache()).map_err(|e| format!("{}: {e}", cache().display()))?;
    let lock = cache().join("modules.lock");
    let lock = fs::File::create(&lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    if lock.try_lock().is_err() {
        eprintln!("aqr-bincache: waiting for another cargo aqr making the modules");
        lock.lock().map_err(|e| format!("modules.lock: {e}"))?;
    }
    modules()?;
    pack()
}

fn modules() -> Result<()> {
    let file = cache().join("modules.stamp");
    let old = fs::read_to_string(&file).ok();
    if let Some(old) = &old {
        let paths = old.lines().filter_map(|l| l.strip_prefix("< ")?.split_once(' ')).map(|(_, p)| p.to_string());
        if stamp(&paths.collect())? == *old {
            return Ok(());
        }
    }
    let why = if old.is_some() { "what the modules are made of changed" } else { "no build of the modules seen yet" };
    eprintln!("aqr-bincache: {why}; running {BUILD}");
    let before = known()?;
    let start = SystemTime::now();
    sh(BUILD)?;
    tell(&before, &known()?);
    let paths = inputs()?.ok_or_else(|| format!("{BUILD} left no dep-info of the modules"))?;
    let changed = paths.iter().find(|p| modified_since(&root().join(p), start));
    match changed {
        // (the stamp left as it was: the next run makes the modules again)
        Some(p) => eprintln!("aqr-bincache: {p} changed while the modules were made; they will be made again"),
        None => fs::write(&file, stamp(&paths)?).map_err(|e| format!("{}: {e}", file.display()))?,
    }
    Ok(())
}

fn modified_since(path: &Path, t: SystemTime) -> bool {
    fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|m| m >= t)
}

/// The modules' binaries (`src/bin/*.rs`), by name.
fn bins() -> Result<BTreeSet<String>> {
    let dir = root().join("crates/aqr-modules/src/bin");
    let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(entries.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".rs").map(String::from)).collect())
}

/// What build.sh read, by the dep-info its builds left: `None` where some is missing.
fn inputs() -> Result<Option<BTreeSet<String>>> {
    // (build.sh's paths: it builds in `target/` whatever CARGO_TARGET_DIR is)
    let target = root().join("target");
    let wasm = target.join("wasm32-unknown-unknown/release-wasm");
    let mut infos: Vec<PathBuf> = bins()?.iter().map(|b| wasm.join(format!("{b}.d"))).collect();
    infos.push(target.join("release/aqr-ld.d"));
    let mut paths = BTreeSet::new();
    for d in infos {
        if !d.exists() {
            return Ok(None);
        }
        paths.extend(dep_info(&d)?);
    }
    paths.extend([BUILD, "rust-toolchain.toml"].map(String::from));
    // (not Cargo.lock, which pack.sh writes after build.sh: the registry crates' versions are in
    // the paths of their sources)
    paths.remove("Cargo.lock");
    Ok(Some(paths))
}

/// What build.sh writes and the stamp follows (not the spec, which other changes touch too).
fn outputs() -> Result<BTreeSet<String>> {
    let dir = root().join("crates/aqr-modules/wasm");
    let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out: BTreeSet<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.ends_with(".wasm") || n.ends_with(".block"))
        .map(|n| format!("crates/aqr-modules/wasm/{n}"))
        .collect();
    out.insert(KNOWN.into());
    Ok(out)
}

/// The stamp of `inputs` (the lines `< ID PATH`) and of the outputs (`> ID PATH`).
fn stamp(inputs: &BTreeSet<String>) -> Result<String> {
    let outputs = outputs()?;
    let ids = blobs(inputs.iter().chain(&outputs))?;
    let mut text = format!("{STAMP}{}bins {}\n", environment()?, Vec::from_iter(bins()?).join(" "));
    for (mark, set) in [('<', inputs), ('>', &outputs)] {
        for p in set {
            text += &format!("{mark} {} {p}\n", ids[p]);
        }
    }
    Ok(text)
}

/// The modules of known.rs: name to hash.
fn known() -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(root().join(KNOWN)).map_err(|e| format!("{KNOWN}: {e}"))?;
    Ok(parse_known(&text))
}

fn parse_known(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut name = None;
    for line in text.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("name: \"").and_then(|l| l.strip_suffix("\",")) {
            name = Some(n.to_string());
        } else if let Some(h) = line.strip_prefix("sha512_256: hex(\"").and_then(|l| l.strip_suffix("\"),")) {
            map.extend(name.take().map(|n| (n, h.to_string())));
        }
    }
    map
}

/// Tells the modules build.sh changed, to be committed.
fn tell(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) {
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let changed: Vec<&str> = names.into_iter().filter(|n| before.get(*n) != after.get(*n)).map(|n| &n[..]).collect();
    if changed.is_empty() {
        eprintln!("aqr-bincache: no module changed");
    } else {
        eprintln!(
            "aqr-bincache: modules changed (commit crates/aqr-modules/wasm, {KNOWN} and spec/index.bs): {}",
            changed.join(" ")
        );
    }
}

/// Runs pack.sh where the pack is there and not of known.rs.
fn pack() -> Result<()> {
    if !cfg!(all(windows, target_arch = "x86_64")) || std::env::var_os("AQR_SFX_PACK").is_some() {
        return Ok(());
    }
    let Ok(list) = fs::read_to_string(root().join("target/aqr-sfx/modules.txt")) else { return Ok(()) };
    if pack_of(&list, &known()?) {
        return Ok(());
    }
    let failed = cache().join("pack.failed");
    let known_id = blobs([&KNOWN.to_string()])?.remove(KNOWN).unwrap_or_default();
    if fs::read_to_string(&failed).is_ok_and(|id| id == known_id) {
        return Ok(());
    }
    eprintln!("aqr-bincache: the SFX pack (target/aqr-sfx) is not of the modules of known.rs; running {PACK}");
    match sh(PACK) {
        Ok(()) => {
            let _ = fs::remove_file(&failed);
            // (the shared libraries' layout, crates/aqr-dll/layout.py's, is committed)
            let layout = git(&["status", "--porcelain", "--", "crates/aqr-dll", "Cargo.lock"], b"")?;
            if !layout.trim().is_empty() {
                eprintln!(
                    "aqr-bincache: {PACK} changed the shared libraries' layout: commit crates/aqr-dll (and Cargo.lock's hunks of aqr-dll-*)"
                );
            }
        }
        Err(e) => {
            eprintln!("aqr-bincache: {e}; the pack is left as it was (its modules not in known.rs run as Wasm)");
            let _ = fs::write(&failed, known_id);
        }
    }
    Ok(())
}

/// Whether the pack's `modules.txt` (`NAME HASH` lines, the binaries' names) has the modules of
/// known.rs (whose names may have `/` for the binaries' `-`), no more and no fewer.
fn pack_of(list: &str, known: &BTreeMap<String, String>) -> bool {
    let have: BTreeSet<(String, String)> =
        list.lines().filter_map(|l| l.split_once(' ')).map(|(n, h)| (n.to_string(), h.trim().to_string())).collect();
    let want: BTreeSet<(String, String)> = known.iter().map(|(n, h)| (n.replace('/', "-"), h.clone())).collect();
    have == want
}

/// Runs a script of the working copy by `sh` from its root, its output on standard error (that of
/// `cargo aqr` is aqr's).
fn sh(script: &str) -> Result<()> {
    let status = Command::new(shell())
        .current_dir(root())
        .arg(script)
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| format!("sh {script}: {e}"))?;
    if !status.success() {
        return Err(format!("{script} failed ({status})"));
    }
    Ok(())
}

/// `sh`: on PATH, or (on Windows, run from PowerShell or cmd) Git for Windows' own.
fn shell() -> PathBuf {
    let on_path = Command::new("sh").args(["-c", ":"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    if on_path.is_ok_and(|s| s.success()) || !cfg!(windows) {
        return "sh".into();
    }
    // (`git --exec-path`: <Git>/mingw64/libexec/git-core; sh is <Git>/usr/bin/sh.exe)
    let exec = Command::new("git").arg("--exec-path").output().ok();
    let git = exec.and_then(|o| String::from_utf8(o.stdout).ok()).map(|p| PathBuf::from(p.trim()));
    git.and_then(|p| Some(p.ancestors().nth(3)?.join("usr/bin/sh.exe")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "sh".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN_RS: &str = r#"pub const KNOWN: &[Known] = &[
    Known {
        name: "bcj-x86",
        version: 1,
        sha512_256: hex("aa"),
        block: include_bytes!("../wasm/bcj-x86.block"),
    },
    Known {
        name: "exe-recons/x86",
        version: 2,
        sha512_256: hex("bb"),
        block: include_bytes!("../wasm/exe-recons-x86.block"),
    },
];
"#;

    #[test]
    fn known_parsed() {
        let k = parse_known(KNOWN_RS);
        assert_eq!(k.len(), 2);
        assert_eq!(k["exe-recons/x86"], "bb");
    }

    /// A pack is of known.rs where it has its modules, by the binaries' names, and no others.
    #[test]
    fn pack_compared() {
        let k = parse_known(KNOWN_RS);
        assert!(pack_of("bcj-x86 aa\nexe-recons-x86 bb\n", &k));
        assert!(pack_of("exe-recons-x86 bb\r\nbcj-x86 aa\r\n", &k));
        assert!(!pack_of("bcj-x86 aa\nexe-recons-x86 cc\n", &k), "stale");
        assert!(!pack_of("bcj-x86 aa\n", &k), "missing");
        assert!(!pack_of("bcj-x86 aa\nexe-recons-x86 bb\nold aa\n", &k), "one more");
    }
}
