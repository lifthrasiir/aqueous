//! The modules and the SFX pack made again before aqr is built, where what they are made of
//! changed, so that no one has to remember `crates/aqr-modules/build.py` and
//! `crates/aqr-sfx/pack.py` (both build the same bytes on any host; Python runs them: `PYTHON`,
//! else the `.venv`'s at the root, else `python3` or `python` on PATH).
//!
//! - The modules: a stamp (`cache/modules.stamp`) holds the git blob ids of what the last
//!   build.py read (the dep-info of each module's binary and of aqr-ld, which links them, with
//!   their crates' manifests, build.py and the toolchain's file) and of what it wrote
//!   (`wasm/*.wasm`, `wasm/*.block`, `src/known.rs`), with the environment and the binaries of
//!   `src/bin`. Where any of it differs, build.py runs (a minute or so even with nothing to do,
//!   hence the stamp), and the modules whose hashes it changed are told.
//! - The pack (Windows x86-64 alone, where pack.py runs, and where it is `target/aqr-sfx`, not
//!   `AQR_SFX_PACK`'s): where there is one and its `modules.txt` is not of the modules of
//!   known.rs, or its `sfx.pack` is missing or of an earlier form, pack.py runs. Where there is
//!   none it is left so (pack.py needs the nightly of `nightly-toolchain` and build-std); where
//!   pack.py fails, it is not run again for the same known.rs. The binary kept has the pack (of
//!   `AQR_SFX_PACK` where that is set) written in by `aqr-sfx-pack --embed` ([`embed`]).
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
const BUILD: &str = "crates/aqr-modules/build.py";
const PACK: &str = "crates/aqr-sfx/pack.py";
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
    python(BUILD)?;
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

/// What build.py read, by the dep-info its builds left: `None` where some is missing.
fn inputs() -> Result<Option<BTreeSet<String>>> {
    // (build.py's paths: it builds in `target/` whatever CARGO_TARGET_DIR is)
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
    // (not Cargo.lock, which pack.py writes after build.py: the registry crates' versions are in
    // the paths of their sources)
    paths.remove("Cargo.lock");
    Ok(Some(paths))
}

/// What build.py writes and the stamp follows (not the spec, which other changes touch too).
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

/// Tells the modules build.py changed, to be committed.
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

/// Runs pack.py where the pack is there and not of known.rs, or not packed as aqr reads it.
fn pack() -> Result<()> {
    if !cfg!(all(windows, target_arch = "x86_64")) || std::env::var_os("AQR_SFX_PACK").is_some() {
        return Ok(());
    }
    let Ok(list) = fs::read_to_string(root().join("target/aqr-sfx/modules.txt")) else { return Ok(()) };
    if pack_of(&list, &known()?) && packed() {
        return Ok(());
    }
    let failed = cache().join("pack.failed");
    let known_id = blobs([&KNOWN.to_string()])?.remove(KNOWN).unwrap_or_default();
    if fs::read_to_string(&failed).is_ok_and(|id| id == known_id) {
        return Ok(());
    }
    eprintln!("aqr-bincache: the SFX pack (target/aqr-sfx) is not of the modules of known.rs; running {PACK}");
    match python(PACK) {
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

/// The directory of the pack written into aqr: `AQR_SFX_PACK`, else `target/aqr-sfx` (Windows
/// x86-64 alone).
fn pack_dir() -> Option<PathBuf> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return None;
    }
    Some(std::env::var_os("AQR_SFX_PACK").map_or_else(|| root().join("target/aqr-sfx"), PathBuf::from))
}

/// The pack's `pack.id`, for the manifest of a binary (relative to the root where under it): a
/// binary is of the pack written into it as of its sources.
pub fn pack_id() -> Option<String> {
    let id = pack_dir()?.join("pack.id");
    Some(id.strip_prefix(root()).unwrap_or(&id).to_string_lossy().replace('\\', "/"))
}

/// Writes the pack into the binary `exe` (`aqr-sfx-pack --embed`), where there is one.
pub fn embed(exe: &Path) -> Result<()> {
    let Some(dir) = pack_dir().filter(|d| d.join("sfx.pack").is_file()) else { return Ok(()) };
    let cargo = std::env::var_os("CARGO").unwrap_or("cargo".into());
    let status = Command::new(cargo)
        .current_dir(root())
        .args(["run", "-q", "--release", "-p", "aqr-sfx", "-F", "encode,xz", "--bin", "aqr-sfx-pack", "--", "--embed"])
        .args([dir.as_os_str(), exe.as_os_str()])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("the pack of {} not written into aqr", dir.display())),
    }
}

/// Whether the pack's `sfx.pack` is there, of the form `aqr-sfx-pack --embed` takes (the magic
/// of aqr-sfx's `aqr-sfx-pack`).
fn packed() -> bool {
    let mut head = [0; 8];
    fs::File::open(root().join("target/aqr-sfx/sfx.pack"))
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
        .is_ok_and(|()| &head == b"AQRSFXP2")
}

/// Whether the pack's `modules.txt` (`NAME HASH` lines, the binaries' names) has the modules of
/// known.rs (whose names may have `/` for the binaries' `-`), no more and no fewer.
fn pack_of(list: &str, known: &BTreeMap<String, String>) -> bool {
    let have: BTreeSet<(String, String)> =
        list.lines().filter_map(|l| l.split_once(' ')).map(|(n, h)| (n.to_string(), h.trim().to_string())).collect();
    let want: BTreeSet<(String, String)> = known.iter().map(|(n, h)| (n.replace('/', "-"), h.clone())).collect();
    have == want
}

/// Runs a Python script of the working copy from its root, its output on standard error (that of
/// `cargo aqr` is aqr's).
fn python(script: &str) -> Result<()> {
    let python = interpreter().ok_or_else(|| format!("{script}: no Python (set PYTHON)"))?;
    let status = Command::new(&python)
        .current_dir(root())
        .args(["-I", script])
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| format!("{} {script}: {e}", python.display()))?;
    if !status.success() {
        return Err(format!("{script} failed ({status})"));
    }
    Ok(())
}

/// Python: `PYTHON`, else the venv's at the root, else `python3` or `python` that runs (Windows'
/// `python3` may be the Store's stand-in).
fn interpreter() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PYTHON") {
        return Some(p.into());
    }
    let venv = [".venv/Scripts/python.exe", ".venv/bin/python"].map(|p| root().join(p));
    if let Some(p) = venv.into_iter().find(|p| p.exists()) {
        return Some(p);
    }
    let runs = |p: &str| {
        let s = Command::new(p).args(["-c", ""]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        s.is_ok_and(|s| s.success())
    };
    ["python3", "python"].into_iter().find(|p| runs(p)).map(PathBuf::from)
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
