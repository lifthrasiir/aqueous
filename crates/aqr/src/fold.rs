//! Names folded as file systems taking names case aside (and their normalization) could take
//! them, more coarsely than any: two names that one of them takes as one fold alike ([`fold`]).
//! Used to find entries of a directory that a file system could take as one name, and links
//! whose targets could name another entry than they seem to (spec, "Extraction").
//!
//! How that was checked (`scripts/name-fold-pairs.py`, the ignored test `name_fold_pairs`):
//!
//! - Every simple case mapping of every version of Unicode (1.1.5 to 17: tables of file
//!   systems, NTFS's `$UpCase` of each volume, come of the version of their time), every case
//!   folding of Unicode 17 (but the Turkic one), and its canonical decompositions.
//! - Probed as black boxes, the names they take as one: APFS (case- and normalization-
//!   insensitive) and HFS+ (its decomposition, case folding and code points ignored) as macOS
//!   27 makes them, and NTFS as Windows 11 makes it (`fold/ntfs-win11.txt`, the test
//!   `ntfs_pairs`). HFS+ took one mapping of no current Unicode as one: Georgian Mkhedruli as
//!   Asomtavruli (Unicode 1.1 to 2.1), hence [`legacy`]. The decompositions of earlier versions
//!   corrected since were probed too, and taken by neither.
//! - exFAT (its up-case table) as macOS 27 and Windows 11 make it, and FAT32 (long names) as
//!   macOS 27 makes it, probed of the characters that have case (`--cased`: all but those of no
//!   case, categories Lo, Cn, Co and Cs, that no case mapping of any version has).

use std::ffi::OsStr;

use unicode_normalization::UnicodeNormalization;

/// The code points HFS+ ignores in names (TN1150, "Unicode Subtleties"): joiners, marks of
/// direction and the byte order mark.
fn ignored(c: char) -> bool {
    matches!(c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}

/// `c` (lowercase, as [`fold`] has it) as one of the characters that earlier versions of Unicode
/// took as its case: Georgian Mkhedruli as Nuskhuri, which Asomtavruli lowercases to now (to
/// Mkhedruli in Unicode 1.1 to 2.1; HFS+ takes them as one), `ɘ` as `ǝ` (`Ǝ`'s lowercase in 1.1),
/// `ϲ` as `σ` (uppercase `Σ` to 3.2) and `ʔ` as `ɂ` (uppercase `Ɂ` in 4.1).
fn legacy(c: char) -> char {
    match c {
        '\u{10d0}'..='\u{10f5}' => char::from_u32(c as u32 - 0x10d0 + 0x2d00).unwrap(),
        '\u{258}' => '\u{1dd}',
        '\u{3f2}' => 'σ',
        '\u{294}' => '\u{242}',
        _ => c,
    }
}

/// `name` folded as file systems taking names by case but normalization aside could take it
/// (macOS's case-sensitive APFS and HFSX): the code points HFS+ ignores left out, decomposed
/// (NFD). What is not Unicode is U+FFFD, as [`fold`] has it.
pub fn normalized(name: &OsStr) -> String {
    let s = name.to_string_lossy();
    if s.is_ascii() {
        return s.into_owned();
    }
    s.chars().filter(|&c| !ignored(c)).nfd().collect()
}

/// `name` folded ([module](self)): the code points HFS+ ignores left out, decomposed (NFD), and
/// each character's uppercase then lowercase (full mappings, by character) taken until nothing
/// changes, decomposed again each time. What is not Unicode (lone surrogates, bytes of no
/// UTF-8) is U+FFFD: names of it fold alike, more coarsely than any file system takes them.
pub fn fold(name: &OsStr) -> String {
    let s = name.to_string_lossy();
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut x: String = s.chars().filter(|&c| !ignored(c)).nfd().collect();
    // (as many rounds as `ẞ` takes, `ß` then `ss`, and a few more)
    for _ in 0..8 {
        let y: String = x.chars().flat_map(char::to_uppercase).flat_map(char::to_lowercase).nfd().collect();
        if y == x {
            break;
        }
        x = y;
    }
    // (case mappings of earlier versions of Unicode, which tables of file systems keep)
    if x.chars().any(|c| legacy(c) != c) {
        x = x.chars().map(legacy).collect();
    }
    // (`İ` of its simple lowercase mapping too, `i`: the dot above an `i` left out)
    let mut base = '\0';
    x.retain(|c| {
        let mark = unicode_normalization::char::canonical_combining_class(c) != 0;
        if !mark {
            base = c;
        }
        !(mark && base == 'i' && c == '\u{307}')
    });
    x
}

#[cfg(test)]
mod tests {
    use super::fold;

    fn alike(a: &str, b: &str) -> bool {
        fold(a.as_ref()) == fold(b.as_ref())
    }

    #[test]
    fn folds() {
        assert!(alike("Makefile", "makefile"));
        assert!(alike("ẞ", "ß") && alike("ß", "ss") && alike("ẞ", "SS"));
        assert!(alike("ı", "I") && alike("ı", "i"));
        assert!(alike("İ", "i") && alike("I\u{307}", "i") && alike("i\u{323}\u{307}", "I\u{323}"));
        assert!(alike("ſ", "S"));
        assert!(alike("\u{212a}", "k") && alike("\u{2126}", "ω") && alike("\u{212b}", "å"));
        assert!(alike("ϐ", "β") && alike("ς", "σ") && alike("\u{345}", "ι"));
        assert!(alike("ꭰ", "Ꭰ"));
        assert!(alike("ა", "Ⴀ") && alike("ა", "ⴀ") && alike("ა", "Ა"));
        assert!(alike("Ǝ", "ɘ") && alike("ϲ", "Σ") && alike("Ϲ", "σ") && alike("ʔ", "Ɂ"));
        assert!(alike("é", "e\u{301}") && alike("É", "e\u{301}"));
        assert!(alike("한", "\u{1112}\u{1161}\u{11ab}"));
        assert!(alike(".g\u{200c}it", ".GIT") && alike("a\u{feff}", "A"));
        assert!(!alike("a", "b") && !alike("é", "e") && !alike("ss", "s"));
    }

    /// The names that NTFS took as one (`ntfs-win11.txt`: `scripts/name-fold-pairs.py probe` on
    /// Windows 11 10.0.26300, each line the names without `p` and `q`, of code points in hex),
    /// folded alike.
    #[test]
    fn ntfs_pairs() {
        let hex = |s: &str| -> String {
            s.split(' ').map(|x| char::from_u32(u32::from_str_radix(x, 16).unwrap()).unwrap()).collect()
        };
        let mut n = 0;
        for line in include_str!("fold/ntfs-win11.txt").lines() {
            let (a, b) = line.split_once(" = ").unwrap();
            assert!(alike(&hex(a), &hex(b)), "{line}");
            n += 1;
        }
        assert_eq!(n, 1467);
    }

    /// The pairs of `scripts/name-fold-pairs.py` (the JSON files of `cache/fold`, or those of
    /// `AQR_FOLD_PAIRS`, separated by commas): each pair folds alike.
    #[test]
    #[ignore = "of pairs made by scripts/name-fold-pairs.py"]
    fn name_fold_pairs() {
        let files: Vec<std::path::PathBuf> = match std::env::var_os("AQR_FOLD_PAIRS") {
            Some(v) => v.to_string_lossy().split(',').map(Into::into).collect(),
            None => {
                let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cache/fold");
                let mut f: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().path()).collect();
                f.retain(|p| p.extension().is_some_and(|x| x == "json"));
                f.sort();
                f
            }
        };
        assert!(!files.is_empty(), "no pairs");
        let mut wrong = Vec::new();
        for f in &files {
            let pairs: Vec<(String, String, String)> =
                serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            let n = pairs.iter().filter(|(a, b, _)| !alike(a, b)).count();
            eprintln!("{}: {} pairs, {n} folded apart", f.display(), pairs.len());
            for (a, b, what) in pairs.into_iter().filter(|(a, b, _)| !alike(a, b)) {
                let hex = |s: &str| s.chars().map(|c| format!("{:04X}", c as u32)).collect::<Vec<_>>().join(" ");
                wrong.push(format!(
                    "{} ~ {} ({what}): {} / {}",
                    hex(&a),
                    hex(&b),
                    hex(&fold(a.as_ref())),
                    hex(&fold(b.as_ref()))
                ));
            }
        }
        assert!(wrong.is_empty(), "{} pairs folded apart:\n{}", wrong.len(), wrong.join("\n"));
    }
}
