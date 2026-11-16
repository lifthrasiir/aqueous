//! aqr-ld as Cargo's `RUSTC_WORKSPACE_WRAPPER` (crates/aqr-modules/build.py): rustc run with the
//! `-C metadata` that Cargo gives each crate of the workspace replaced by one of what does not
//! depend on the host.
//!
//! Cargo hashes a path package by its path relative to the workspace, separators and all
//! (`crates\aqr-api` on Windows, `crates/aqr-api` elsewhere), so the metadata of every crate of
//! the workspace, and of what depends on one, differs between hosts. The metadata makes the
//! crates' disambiguators in the symbols, and those order the modules' data (the strings merged)
//! and some of their functions: a quarter of the modules built on Windows differed from those
//! built on macOS. Here it is a hash of the crate's name, types, cfgs, target, edition, codegen
//! options and the names of its externs (none of them paths), which tell the crates of one build
//! apart as Cargo's metadata does.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Whether aqr-ld is run as a wrapper of rustc: Cargo gives the path of rustc first.
pub fn is_wrapper(args: &[OsString]) -> bool {
    args.first().and_then(|a| Path::new(a).file_stem()).is_some_and(|s| s == "rustc")
}

/// FNV-1a of 64 bits, each key ended by a byte that no UTF-8 string has.
fn fnv(h: &mut u64, s: &str) {
    for b in s.bytes().chain([0xff]) {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

/// `args` with the metadata replaced (as `-C metadata=…` or `-Cmetadata=…`), and the keys hashed.
fn rewrite(mut args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut keys = Vec::new();
    let mut at = None;
    let mut i = 0;
    while i < args.len() {
        const LONG: [&str; 6] = ["--crate-name", "--crate-type", "--edition", "--target", "--cfg", "--extern"];
        let a = args[i].as_str();
        let (opt, value, step) = if a == "-C" || LONG.contains(&a) {
            (a.to_string(), args.get(i + 1).cloned().unwrap_or_default(), 2)
        } else if let Some((o, v)) = a.split_once('=').filter(|(o, _)| LONG.contains(o)) {
            (o.to_string(), v.to_string(), 1)
        } else if let Some(v) = a.strip_prefix("-C").filter(|v| !v.is_empty()) {
            ("-C".to_string(), v.to_string(), 1)
        } else {
            i += 1;
            continue;
        };
        match opt.as_str() {
            // (the name alone: the path is the host's)
            "--extern" => keys.push(format!("--extern {}", value.split('=').next().unwrap_or_default())),
            "-C" if value.starts_with("metadata=") => at = Some((i, step)),
            // (paths, or Cargo's own of the metadata)
            "-C" if ["extra-filename=", "incremental=", "linker="].iter().any(|p| value.starts_with(p)) => {}
            _ => keys.push(format!("{opt} {value}")),
        }
        i += step;
    }
    if let Some((i, step)) = at {
        keys.sort();
        let mut h = 0xcbf2_9ce4_8422_2325;
        for k in &keys {
            fnv(&mut h, k);
        }
        let m = format!("metadata={h:016x}");
        if step == 2 {
            args[i + 1] = m;
        } else {
            args[i] = format!("-C{m}");
        }
    }
    (args, keys)
}

/// Runs rustc (`args[0]`) on the rest of `args`, its metadata replaced, and exits as it does.
pub fn wrap(args: &[OsString]) -> Result<(), String> {
    let rest: Vec<String> = args[1..]
        .iter()
        .map(|a| a.clone().into_string().map_err(|a| format!("an argument not in UTF-8: {a:?}")))
        .collect::<Result<_, _>>()?;
    let (rest, _) = rewrite(rest);
    let status = Command::new(&args[0]).args(&rest).status().map_err(|e| format!("{}: {e}", args[0].display()))?;
    std::process::exit(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    fn meta(a: &[String]) -> &str {
        a.iter().find_map(|a| a.strip_prefix("metadata=").or_else(|| a.strip_prefix("-Cmetadata="))).unwrap()
    }

    /// The same metadata from either host's paths, other metadata for other features.
    #[test]
    fn metadata_of_no_path() {
        let win = args(
            "--crate-name aqr_api --edition=2024 crates\\aqr-api\\src\\lib.rs --crate-type lib -C opt-level=3 \
             -C metadata=b86165225ec0cb5c -C extra-filename=-b86165225ec0cb5c --out-dir D:\\t\\deps \
             --target wasm32-unknown-unknown -L dependency=D:\\t\\deps --extern aqr_common=D:\\t\\libaqr_common-1.rlib",
        );
        let mac = args(
            "--crate-name aqr_api --edition=2024 crates/aqr-api/src/lib.rs --crate-type lib -C opt-level=3 \
             -C metadata=137b4b150916cec2 -C extra-filename=-137b4b150916cec2 --out-dir /t/deps \
             --target wasm32-unknown-unknown -L dependency=/t/deps --extern aqr_common=/t/libaqr_common-2.rlib",
        );
        let (w, keys) = rewrite(win.clone());
        let (m, _) = rewrite(mac);
        assert_eq!(meta(&w), meta(&m));
        assert!(keys.iter().all(|k| !k.contains('\\') && !k.contains('/')), "{keys:?}");
        // (Cargo's own extra filename left)
        assert!(w.iter().any(|a| a == "extra-filename=-b86165225ec0cb5c"));
        let mut feat = win.clone();
        feat.extend(args("--cfg feature=\"wasm\""));
        assert_ne!(meta(&rewrite(feat).0), meta(&w));
        let mut other = win.clone();
        other[1] = "aqr_common".into();
        assert_ne!(meta(&rewrite(other).0), meta(&w));
    }

    /// The options written joined (`-Copt-level=3`, `--edition=2024`) are the same keys.
    #[test]
    fn metadata_joined() {
        let (a, ka) = rewrite(args("--crate-name x --edition=2024 -Cmetadata=0123 -Copt-level=3"));
        let (b, kb) = rewrite(args("--crate-name=x --edition 2024 -C metadata=4567 -C opt-level=3"));
        assert_eq!(ka, kb);
        assert_eq!(meta(&a), meta(&b));
        assert!(a.iter().any(|x| x.starts_with("-Cmetadata=")));
        let (c, _) = rewrite(args("--crate-name x --edition=2021 -Cmetadata=0123 -Copt-level=3"));
        assert_ne!(meta(&a), meta(&c));
    }

    #[test]
    fn rustc_recognised() {
        assert!(is_wrapper(&["C:\\x\\rustc.exe".into(), "-vV".into()]));
        assert!(is_wrapper(&["/usr/bin/rustc".into()]));
        assert!(!is_wrapper(&["merge".into()]));
        assert!(!is_wrapper(&["-flavor".into()]));
    }
}
