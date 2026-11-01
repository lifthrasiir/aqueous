//! The version of the binaries, for their build scripts (a build dependency).
//!
//! The version is, the first that is there:
//!
//! - `AQR_VERSION` in the environment (the release workflow sets it to the tag's), without a
//!   leading `v`;
//! - `git describe --tags --match 'v[0-9]*'` of the working copy, without the `v`: the tag's
//!   (`0.0.20261031`) at a tag, `0.0.20261031-5-gf646b70` five commits on; `0.0.0-gf646b70`
//!   where no tag is reachable (a shallow clone);
//! - `0.0.0` (no git, as in a source archive).
//!
//! Changes in the working copy are not told (no `--dirty`): the build script would have to run
//! on every build to see them.
//!
//! [`emit`] gives it to the crate as `env!("AQR_VERSION")`; [`write_rc`] writes `version.rc` in
//! `OUT_DIR`, Windows' `VERSIONINFO`, for the crate's `.rc` to `#include` (embed-resource passes
//! `OUT_DIR` to rc.exe as an include directory).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The version, given to the crate being built as `AQR_VERSION` and its build script rerun as
/// it changes.
pub fn emit() -> String {
    println!("cargo:rerun-if-env-changed=AQR_VERSION");
    let version = match std::env::var("AQR_VERSION") {
        Ok(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(&v).to_string(),
        _ => {
            for path in git_paths() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            match git(&["describe", "--tags", "--match", "v[0-9]*", "--always"]) {
                Some(d) => d.strip_prefix('v').map_or_else(|| format!("0.0.0-g{d}"), str::to_string),
                None => "0.0.0".to_string(),
            }
        }
    };
    println!("cargo:rustc-env=AQR_VERSION={version}");
    version
}

fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The files of git that `git describe` changes with: `HEAD`, the branch it is on, the packed
/// refs and the tags; only those that are there, as Cargo reruns the script every time for one
/// missing.
fn git_paths() -> Vec<PathBuf> {
    let Some(dirs) = git(&["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"]) else {
        return Vec::new();
    };
    let mut lines = dirs.lines().map(PathBuf::from);
    let (Some(git_dir), Some(common)) = (lines.next(), lines.next()) else { return Vec::new() };
    let mut paths = vec![git_dir.join("HEAD"), common.join("packed-refs"), common.join("refs/tags")];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push(common.join(branch));
    }
    paths.retain(|p| p.exists());
    paths
}

/// The four numbers of Windows' `FILEVERSION` (16 bits each) for `version`: `a.b.c` gives
/// `a,b,c,0`, but a date `c` (`0.0.20261031`) gives `a,b,YYYY,MMDD`. Anything after a `-` or
/// `+` is left out.
pub fn numbers(version: &str) -> [u16; 4] {
    let core = version.split(['-', '+']).next().unwrap_or("");
    let n: Vec<u64> = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let at = |i: usize| n.get(i).copied().unwrap_or(0);
    let clamp = |x: u64| x.min(u16::MAX as u64) as u16;
    let (c, d) = match at(2) {
        date @ 0x1_0000..1_0000_0000 => (date / 10000, date % 10000),
        c => (c, at(3)),
    };
    [clamp(at(0)), clamp(at(1)), clamp(c), clamp(d)]
}

/// Writes `version.rc` into `OUT_DIR`: the `VERSIONINFO` of the program `exe` (its file name,
/// as `aqr.exe`) at `version`, described by `description` (what Explorer and Task Manager show).
pub fn write_rc(version: &str, description: &str, exe: &str) {
    let [a, b, c, d] = numbers(version);
    // (VS_FF_PRERELEASE for a build that is not a tag's)
    let flags = if version.contains(['-', '+']) { 2 } else { 0 };
    let s = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let internal = exe.strip_suffix(".exe").unwrap_or(exe);
    let rc = format!(
        "// Written by aqr-version's write_rc.\n\
         1 VERSIONINFO\n\
         FILEVERSION {a},{b},{c},{d}\n\
         PRODUCTVERSION {a},{b},{c},{d}\n\
         FILEFLAGSMASK 0x3F\n\
         FILEFLAGS {flags:#x}\n\
         FILEOS 0x40004\n\
         FILETYPE 0x1\n\
         FILESUBTYPE 0x0\n\
         BEGIN\n\
         \x20   BLOCK \"StringFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       BLOCK \"040904B0\"\n\
         \x20       BEGIN\n\
         \x20           VALUE \"FileDescription\", {description}\n\
         \x20           VALUE \"FileVersion\", {version}\n\
         \x20           VALUE \"InternalName\", {internal}\n\
         \x20           VALUE \"OriginalFilename\", {exe}\n\
         \x20           VALUE \"ProductName\", \"Aqueous\"\n\
         \x20           VALUE \"ProductVersion\", {version}\n\
         \x20       END\n\
         \x20   END\n\
         \x20   BLOCK \"VarFileInfo\"\n\
         \x20   BEGIN\n\
         \x20       VALUE \"Translation\", 0x409, 1200\n\
         \x20   END\n\
         END\n",
        description = s(description),
        version = s(version),
        internal = s(internal),
        exe = s(exe),
    );
    let out = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is not set")).join("version.rc");
    // (not rewritten unchanged, so that rc.exe's input keeps its time)
    if std::fs::read_to_string(&out).ok().as_deref() != Some(&rc) {
        std::fs::write(&out, rc).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::numbers;

    #[test]
    fn numbers_of_versions() {
        assert_eq!(numbers("0.0.20261031"), [0, 0, 2026, 1031]);
        assert_eq!(numbers("0.0.20261031-5-gf646b70"), [0, 0, 2026, 1031]);
        assert_eq!(numbers("1.2.3"), [1, 2, 3, 0]);
        assert_eq!(numbers("1.2.3.4"), [1, 2, 3, 4]);
        assert_eq!(numbers("0.0.0-gf646b70"), [0, 0, 0, 0]);
        assert_eq!(numbers("0.0.0"), [0, 0, 0, 0]);
    }
}
