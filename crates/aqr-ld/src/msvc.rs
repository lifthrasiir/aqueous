//! As the MSVC linker of crates/aqr-dll's no-TLS target (`CARGO_TARGET_X86_64_AQR_WINDOWS_MSVC_LINKER`,
//! build.sh): the shared libraries of the modules (Rust dylibs, crates/aqr-dll/layout.py) export
//! every Rust symbol of their crates, which keeps all of their code; where `AQR_LD_EXPORTS` names
//! a directory holding `LIBRARY.txt` (the symbols that the others import from it, `aqr-ld exports`)
//! for the library linked, the export list rustc gives (`/DEF:`) is cut to those, so that the
//! linker (`/OPT:REF`) leaves out the rest. Then `link.exe` links as asked.
//!
//! `aqr-ld exports --out DIR LIBRARY.dll...` writes those lists: for each library of the set that
//! the others import from, the symbols they import, a line each.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

/// Whether `args` are those of a link by MSVC's `link.exe` (options `/...`, or a response file).
pub fn is_link(args: &[std::ffi::OsString]) -> bool {
    args.iter().filter_map(|a| a.to_str()).any(|a| a.starts_with('@') || a.starts_with("/OUT:") || a == "/NOLOGO")
}

/// The arguments as `link.exe` reads them: a response file's (`@FILE`: UTF-16 with its byte order
/// mark, else UTF-8; an argument a line, quoted where it has spaces) in its place.
fn expanded(args: &[std::ffi::OsString]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for a in args {
        let a = a.to_str().ok_or("a linker argument not UTF-8")?;
        let Some(file) = a.strip_prefix('@') else {
            out.push(a.to_owned());
            continue;
        };
        let b = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        let text = match b.strip_prefix(&[0xff, 0xfe]) {
            Some(w) => String::from_utf16_lossy(
                &w.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>(),
            ),
            None => String::from_utf8_lossy(&b).into_owned(),
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let line = line.strip_prefix('"').and_then(|l| l.strip_suffix('"')).unwrap_or(line);
            out.push(line.replace("\\\"", "\""));
        }
    }
    Ok(out)
}

/// Links as `link.exe` with `args`, the export list cut first where there is a list for it.
pub fn link(args: &[std::ffi::OsString]) -> Result<()> {
    if let Some(dir) = std::env::var_os("AQR_LD_NEEDED") {
        // (what rustc's object of symbols references, which no import table shows: kept)
        let all = expanded(args)?;
        let out = all.iter().find_map(|a| a.strip_prefix("/OUT:").map(PathBuf::from));
        let symbols = all.iter().find(|a| a.ends_with("\\symbols.o") || a.ends_with("/symbols.o"));
        if let (Some(out), Some(symbols)) = (out, symbols) {
            let obj = std::fs::read(symbols).map_err(|e| format!("{symbols}: {e}"))?;
            // (but what it exports itself, which its object of symbols references to keep)
            let own: BTreeSet<String> = match all.iter().find_map(|a| a.strip_prefix("/DEF:")) {
                Some(def) => std::fs::read_to_string(def)
                    .map_err(|e| format!("{def}: {e}"))?
                    .lines()
                    .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
                    .collect(),
                None => BTreeSet::new(),
            };
            let text: String = undefined(&obj)?.into_iter().filter(|s| !own.contains(s)).map(|s| s + "\n").collect();
            let name = out.file_name().ok_or("/OUT: of no name")?.to_string_lossy().to_lowercase();
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let to = Path::new(&dir).join(format!("{name}.txt"));
            std::fs::write(&to, text).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    if let Some(dir) = std::env::var_os("AQR_LD_EXPORTS") {
        let all = expanded(args)?;
        let opt = |p: &str| all.iter().find_map(|a| a.strip_prefix(p).map(PathBuf::from));
        if let (Some(def), Some(out)) = (opt("/DEF:"), opt("/OUT:"))
            && all.iter().any(|a| a == "/DLL")
        {
            let name = out.file_name().ok_or("/OUT: of no name")?.to_string_lossy().to_lowercase();
            let list = Path::new(&dir).join(format!("{name}.txt"));
            if list.exists() {
                let full = std::fs::read(&def).map_err(|e| format!("{}: {e}", def.display()))?;
                cut(&def, &list)?;
                run("link.exe", args)?;
                // the import library of every export still, as the libraries linking against it
                // reference what objects of theirs reference, even where their code is left out
                // (what they import in the end is what the list has)
                let implib = opt("/IMPLIB:").ok_or("/DLL with no /IMPLIB:")?;
                let whole = def.with_extension("whole.def");
                std::fs::write(&whole, full).map_err(|e| format!("{}: {e}", whole.display()))?;
                let (d, o, n) = (whole.display(), implib.display(), out.file_name().unwrap().to_string_lossy());
                let lib_args =
                    ["/NOLOGO", "/MACHINE:X64", &format!("/DEF:{d}"), &format!("/OUT:{o}"), &format!("/NAME:{n}")];
                return run("lib.exe", &lib_args.map(std::ffi::OsString::from));
            }
        }
    }
    run("link.exe", args)
}

fn run(tool: &str, args: &[std::ffi::OsString]) -> Result<()> {
    let status = Command::new(tool).args(args).status().map_err(|e| format!("{tool}: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("{tool} failed with {status}")),
    }
}

/// The external symbols that COFF object `obj` references but does not define (regular or
/// `/bigobj` headers).
fn undefined(obj: &[u8]) -> Result<Vec<String>> {
    let u16_at = |o: usize| obj.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| obj.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let bad = || "symbols.o: not a COFF object".to_string();
    // (a big object: signature 0, 0xffff, then a version and the machine)
    let big = u16_at(0) == Some(0) && u16_at(2) == Some(0xffff);
    let (table, count, size) = match big {
        true => (u32_at(48).ok_or_else(bad)? as usize, u32_at(52).ok_or_else(bad)? as usize, 20),
        false => (u32_at(8).ok_or_else(bad)? as usize, u32_at(12).ok_or_else(bad)? as usize, 18),
    };
    let strings = table + count * size;
    let mut out = Vec::new();
    let mut i = 0;
    while i < count {
        let s = table + i * size;
        let entry = obj.get(s..s + size).ok_or_else(bad)?;
        let (section, class, aux) = match big {
            true => (u32_at(s + 12).ok_or_else(bad)?, entry[18], entry[19]),
            false => (u16_at(s + 12).ok_or_else(bad)? as u32, entry[16], entry[17]),
        };
        if section == 0 && class == 2 {
            let name = match u32_at(s).ok_or_else(bad)? {
                0 => {
                    let at = strings + u32_at(s + 4).ok_or_else(bad)? as usize;
                    let end = obj[at..].iter().position(|&b| b == 0).ok_or_else(bad)?;
                    &obj[at..at + end]
                }
                _ => {
                    let n = entry[..8].iter().position(|&b| b == 0).unwrap_or(8);
                    &entry[..n]
                }
            };
            out.push(String::from_utf8_lossy(name).into_owned());
        }
        i += 1 + aux as usize;
    }
    Ok(out)
}

/// Cuts module-definition file `def` (`LIBRARY`, `EXPORTS`, then a symbol a line, `DATA` after
/// some) to the symbols of `list`.
fn cut(def: &Path, list: &Path) -> Result<()> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let keep: BTreeSet<String> = read(list)?.lines().map(str::to_owned).collect();
    let text = read(def)?;
    let mut out = String::new();
    let mut exports = false;
    for line in text.lines() {
        let symbol = line.split_whitespace().next().unwrap_or("");
        if !exports || keep.contains(symbol) || symbol.is_empty() {
            out.push_str(line);
            out.push('\n');
        }
        exports |= symbol == "EXPORTS";
    }
    std::fs::write(def, out).map_err(|e| format!("{}: {e}", def.display()))
}

/// `aqr-ld exports --out DIR LIBRARY.dll...`.
pub fn exports_main(args: &[std::ffi::OsString]) -> Result<()> {
    let (mut out, mut needed) = (None, None);
    let mut dlls = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--out") => out = Some(PathBuf::from(it.next().ok_or("--out: no directory")?)),
            Some("--needed") => needed = Some(PathBuf::from(it.next().ok_or("--needed: no directory")?)),
            _ => dlls.push(PathBuf::from(a)),
        }
    }
    let out = out.ok_or("exports: no --out")?;
    // what the links' objects of symbols referenced (AQR_LD_NEEDED), kept in each library that
    // exports it
    let mut always = BTreeSet::new();
    if let Some(dir) = needed {
        for e in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            always.extend(text.lines().map(str::to_owned));
        }
    }
    let mut names = BTreeSet::new();
    let mut imported: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for d in &dlls {
        let buf = std::fs::read(d).map_err(|e| format!("{}: {e}", d.display()))?;
        names.insert(d.file_name().ok_or("a library of no name")?.to_string_lossy().to_lowercase());
        for (lib, symbol) in crate::pemerge::imports(buf).map_err(|e| format!("{}: {e}", d.display()))? {
            imported.entry(lib).or_default().insert(symbol);
        }
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (lib, symbols) in imported.iter().filter(|(l, _)| names.contains(*l)) {
        let text: String = symbols.union(&always).map(|s| format!("{s}\n")).collect();
        let to = out.join(format!("{lib}.txt"));
        std::fs::write(&to, text).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module-definition file cut to a list: the symbols listed kept, `DATA` and all, the
    /// others left out, the header as it is.
    #[test]
    fn cut_to_the_list() {
        let dir = std::env::temp_dir().join(format!("aqr-ld-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (def, list) = (dir.join("lib.def"), dir.join("list.txt"));
        std::fs::write(&def, "LIBRARY\nEXPORTS\n  _RNvA\n  _RNvB DATA\n  _RNvC\n").unwrap();
        std::fs::write(&list, "_RNvB\n_RNvC\n_RNvD\n").unwrap();
        cut(&def, &list).unwrap();
        assert_eq!(std::fs::read_to_string(&def).unwrap(), "LIBRARY\nEXPORTS\n  _RNvB DATA\n  _RNvC\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
