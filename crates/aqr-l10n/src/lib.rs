//! The frontends' messages (aqr-win, aqr-win-install, aqr-mac, and aqr-ipc's for them) in the
//! languages of `messages/*.toml`, `en.toml` the source and the fallback. README.md has the
//! format and how a language is added.
//!
//! Each section of the catalogs is a module, each message a constant ([`Msg`]) and a function
//! of it, whose parameters are its placeholders: `common::intact(name)`, `types::ZIP`. They
//! speak the language chosen by [`init`] (English until then), for the whole process.
//!
//! What the system shows itself is made from the same catalogs: Windows' string table ([`RC`],
//! the sections `types` and `shell`, which the registry names by [`Msg::resource`]) and the
//! `.strings` of the macOS bundle ([`LPROJ`], written by this crate's program for `bundle.sh`).

use std::fmt::{Display, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("OUT_DIR"), "/messages.rs"));

/// CLDR's plural categories.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plural {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// The category of `n` in language `tag`; `None` for a language whose rule is not known here
/// (a test checks that every catalog's is).
pub fn plural(tag: &str, n: u64) -> Option<Plural> {
    Some(match tag {
        "en" => {
            if n == 1 {
                Plural::One
            } else {
                Plural::Other
            }
        }
        "ko" => Plural::Other,
        _ => return None,
    })
}

/// A message of the catalogs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Msg(u16);

/// The language chosen, an index of [`LANGS`].
static LANG: AtomicUsize = AtomicUsize::new(0);

/// The language chosen by [`init`].
pub fn lang() -> &'static str {
    LANGS[LANG.load(Ordering::Relaxed)]
}

/// Chooses the language of the messages: that of `AQR_LANG` where it is one of the catalogs',
/// else the first of `preferred` (BCP 47 tags or POSIX locales, the most wanted first) that is,
/// else English. The tag chosen.
pub fn init<S: AsRef<str>>(preferred: &[S]) -> &'static str {
    let env = std::env::var("AQR_LANG").ok();
    let chosen = choose(env.iter().map(String::as_str)).or_else(|| choose(preferred.iter().map(|s| s.as_ref())));
    LANG.store(chosen.unwrap_or(0), Ordering::Relaxed);
    lang()
}

/// The language of the catalogs that the first of `preferred` it can matches: the tag itself, or
/// a catalog's that it begins with (`zh-Hans` of `zh-Hans-CN`), or one of the same language
/// (`ko` of `ko-KR`, `en` of `en_GB.UTF-8`).
pub fn choose<'a>(preferred: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    for p in preferred {
        // (POSIX's `ll_CC.charset@modifier`)
        let p = p.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
        if p.is_empty() || p == "c" || p == "posix" {
            continue;
        }
        let tags = || LANGS.iter().map(|t| t.to_ascii_lowercase()).enumerate();
        let primary = |t: &str| t.split('-').next().unwrap_or("").to_string();
        let found = tags()
            .find(|(_, t)| *t == p)
            .or_else(|| tags().filter(|(_, t)| p.starts_with(&format!("{t}-"))).max_by_key(|(_, t)| t.len()))
            .or_else(|| tags().find(|(_, t)| primary(t) == primary(&p)));
        if let Some((i, _)) = found {
            return Some(i);
        }
    }
    None
}

/// The user's languages, the most wanted first, as the system tells them: Windows' preferred UI
/// languages, else `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` and `LANG`. (Of a macOS bundle, its
/// localizations as `NSBundle` prefers them are better: aqr-mac gives those to [`init`].)
pub fn system_languages() -> Vec<String> {
    #[cfg(windows)]
    {
        let v = windows_languages();
        if !v.is_empty() {
            return v;
        }
    }
    let mut v: Vec<String> = Vec::new();
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(s) = std::env::var(var) {
            v.extend(s.split(':').filter(|s| !s.is_empty()).map(String::from));
        }
    }
    v
}

#[cfg(windows)]
fn windows_languages() -> Vec<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserPreferredUILanguages(flags: u32, count: *mut u32, buffer: *mut u16, len: *mut u32) -> i32;
    }
    /// `MUI_LANGUAGE_NAME`: tags (`ko-KR`) rather than ids.
    const NAME: u32 = 8;
    let (mut count, mut len) = (0u32, 0u32);
    // SAFETY: the length asked for first, then room of it; the counts written to locals
    unsafe {
        if GetUserPreferredUILanguages(NAME, &mut count, std::ptr::null_mut(), &mut len) == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(NAME, &mut count, buf.as_mut_ptr(), &mut len) == 0 {
            return Vec::new();
        }
        // (each ended by a NUL, the list by another)
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

impl Msg {
    /// The text in the language chosen (its form "other"; English where it has none).
    pub fn text(self) -> &'static str {
        self.form(LANG.load(Ordering::Relaxed), None)
    }

    /// The text in language `lang` (an index of [`LANGS`]), of the category of `n` where it is a
    /// plural; English's where the language has none.
    fn form(self, lang: usize, n: Option<u64>) -> &'static str {
        let (lang, forms) = match TEXTS[lang][self.0 as usize] {
            [] => (0, TEXTS[0][self.0 as usize]),
            forms => (lang, forms),
        };
        let category = n.and_then(|n| plural(LANGS[lang], n)).unwrap_or(Plural::Other);
        let find = |c| forms.iter().find(|(k, _)| *k == c).map(|(_, t)| *t);
        find(category).or_else(|| find(Plural::Other)).unwrap_or("")
    }

    /// Its id in Windows' string table ([`RC`]; only those of the sections `types` and `shell`
    /// are there), the one that `messages/ids.toml` gives it: it does not change as messages are
    /// added. Panics for a message that is not in the table.
    pub fn resource(self) -> u16 {
        match RESOURCE_IDS[self.0 as usize] {
            0 => panic!("not a message of Windows' string table"),
            id => id,
        }
    }

    /// The text in the language chosen, of `n`'s category where it is a plural, its placeholders
    /// filled by `args` (by name).
    #[doc(hidden)]
    pub fn fill(self, n: Option<u64>, args: &[(&str, &dyn Display)]) -> String {
        fill(self.form(LANG.load(Ordering::Relaxed), n), args)
    }
}

/// A count (of files, of archives) in the language chosen: `1234`. Every number the frontends
/// show goes through this, [`decimal`], [`percent`] and [`size`] (the plurals' `{n}` too), so
/// that the language's own forms (CLDR's grouping and decimal marks) are written in one place;
/// they are the same in every language for now.
pub fn integer(n: u64) -> String {
    integer_in(LANG.load(Ordering::Relaxed), n)
}

/// A number of `decimals` decimals in the language chosen: `2.35`.
pub fn decimal(x: f64, decimals: usize) -> String {
    decimal_in(LANG.load(Ordering::Relaxed), x, decimals)
}

/// A share (0 to 1) as a whole percentage in the language chosen: `42%`.
pub fn percent(share: f64) -> String {
    percent_in(LANG.load(Ordering::Relaxed), share)
}

/// Bytes of a size in the language chosen, of decimal units as `aqr` tells them: `999 B`,
/// `1.5 KB`, `2.35 MB`, `1.20 GB`.
pub fn size(bytes: f64) -> String {
    size_in(LANG.load(Ordering::Relaxed), bytes)
}

fn integer_in(_lang: usize, n: u64) -> String {
    n.to_string()
}

fn decimal_in(_lang: usize, x: f64, decimals: usize) -> String {
    format!("{x:.decimals$}")
}

fn percent_in(lang: usize, share: f64) -> String {
    decimal_in(lang, 100.0 * share, 0) + "%"
}

fn size_in(lang: usize, n: f64) -> String {
    let (x, decimals, unit) = match n {
        n if n < 1e3 => (n, 0, "B"),
        n if n < 1e6 => (n / 1e3, 1, "KB"),
        n if n < 1e9 => (n / 1e6, 2, "MB"),
        n => (n / 1e9, 2, "GB"),
    };
    format!("{} {unit}", decimal_in(lang, x, decimals))
}

/// A time of `seconds` in the language chosen, of the parts of [`units`]: under a minute as
/// seconds of `decimals` decimals (`2.35 s`, `2.35초`), else of whole hours, minutes and seconds,
/// those of zero left out (`1 h 2 min 3 s`, `1 min`; `1시간 2분 3초`). ICU is not linked, so its
/// duration formats are not at hand; this is what the catalogs need of them.
pub fn duration(seconds: f64, decimals: usize) -> String {
    duration_in(LANG.load(Ordering::Relaxed), seconds, decimals)
}

/// [`duration`] in language `lang` (an index of [`LANGS`]).
fn duration_in(lang: usize, seconds: f64, decimals: usize) -> String {
    let part = |m: Msg, n: &str| fill(m.form(lang, None), &[("n", &n)]);
    let t = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
    // (what rounds to a minute is one)
    if format!("{t:.decimals$}").parse::<f64>().is_ok_and(|s| s < 60.0) {
        return part(units::SECONDS, &decimal_in(lang, t, decimals));
    }
    let t = t.round() as u64;
    let parts = [(t / 3600, units::HOURS), (t / 60 % 60, units::MINUTES), (t % 60, units::SECONDS)];
    let parts: Vec<String> =
        parts.iter().filter(|(n, _)| *n > 0).map(|&(n, m)| part(m, &integer_in(lang, n))).collect();
    parts.join(" ")
}

/// `template` with its placeholders filled by `args`, `{{` and `}}` as braces; a placeholder of no
/// argument left as it is.
fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out += &rest[..i];
        let brace = rest.as_bytes()[i] as char;
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix(brace) {
            out.push(brace);
            rest = r;
            continue;
        }
        if brace == '}' {
            out.push('}');
            continue;
        }
        let end = rest.find('}').unwrap_or(rest.len());
        let name = &rest[..end];
        rest = rest.get(end + 1..).unwrap_or("");
        match args.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => _ = write!(out, "{v}"),
            None => _ = write!(out, "{{{name}}}"),
        }
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ko() -> usize {
        LANGS.iter().position(|&t| t == "ko").unwrap()
    }

    #[test]
    fn every_language_has_a_plural_rule() {
        for tag in LANGS {
            assert!(plural(tag, 1).is_some(), "{tag}");
        }
    }

    #[test]
    fn languages_chosen() {
        let ko = Some(ko());
        assert_eq!(choose(["ko-KR"]), ko);
        assert_eq!(choose(["ko_KR.UTF-8"]), ko);
        assert_eq!(choose(["ja-JP", "ko"]), ko);
        assert_eq!(choose(["en-GB", "ko"]), Some(0));
        assert_eq!(choose(["C", "ja"]), None);
        assert_eq!(choose(Vec::<&str>::new()), None);
    }

    #[test]
    fn placeholders_filled() {
        let n: &dyn Display = &3;
        assert_eq!(fill("{n} of {m} {{n}} }}", &[("n", n)]), "3 of {m} {n} }");
    }

    #[test]
    fn plurals_and_fallbacks() {
        let tested = |lang, n| fill(common::TESTED.form(lang, Some(n)), &[("n", &n)]);
        assert!(tested(0, 1).starts_with("1 file,"));
        assert!(tested(0, 2).starts_with("2 files,"));
        assert!(tested(ko(), 1).starts_with("파일 1개"));
        assert_eq!(types::ZIP.form(ko(), None), "ZIP 압축 파일");
        // (the language chosen is English in the tests)
        assert_eq!(common::intact("a.aqr"), "a.aqr is intact");
        assert_eq!(common::all_intact(2), "The 2 archives are intact");
    }

    #[test]
    fn numbers() {
        // (the language chosen is English in the tests)
        assert_eq!(integer(1234), "1234");
        assert_eq!(decimal(2.345, 1), "2.3");
        assert_eq!(percent(0.426), "43%");
        assert_eq!(size(999.0), "999 B");
        assert_eq!(size(1500.0), "1.5 KB");
        assert_eq!(size(2_345_678.0), "2.35 MB");
        assert_eq!(size(1.2e9), "1.20 GB");
        assert_eq!(size_in(ko(), 1500.0), "1.5 KB");
    }

    #[test]
    fn durations() {
        // (the language chosen is English in the tests)
        assert_eq!(duration(0.0, 2), "0.00 s");
        assert_eq!(duration(2.34, 1), "2.3 s");
        assert_eq!(duration(59.4, 0), "59 s");
        assert_eq!(duration(59.996, 2), "1 min");
        assert_eq!(duration(60.0, 2), "1 min");
        assert_eq!(duration(62.6, 2), "1 min 3 s");
        assert_eq!(duration(3600.0, 0), "1 h");
        assert_eq!(duration(3603.0, 0), "1 h 3 s");
        assert_eq!(duration(3723.2, 1), "1 h 2 min 3 s");
        assert_eq!(duration(-1.0, 0), "0 s");
        assert_eq!(duration_in(ko(), 2.34, 1), "2.3초");
        assert_eq!(duration_in(ko(), 3723.0, 0), "1시간 2분 3초");
        assert_eq!(duration_in(ko(), 120.0, 0), "2분");
    }

    /// The string table has the messages of `types` and `shell` in each language, by their ids.
    #[test]
    fn string_table() {
        assert!(RC.is_ascii());
        assert_eq!(RC.matches("STRINGTABLE").count(), LANGS.len());
        let id = shell::COMPRESS.resource();
        assert!(RC.contains(&format!("    {id}, L\"Compress with Aqueous\"\n")));
        assert!(RC.contains(&format!("    {id}, L\"Aqueous\\xb85c \\xc555\\xcd95\"\n")), "{RC}");
        assert!(RC.contains(&format!("    {}, L\"ZIP archive\"\n", types::ZIP.resource())));
        // (the ids of ids.toml: kept for good, each in its section's range)
        assert_eq!((shell::COMPRESS.resource(), shell::EXTRACT_VERSION.resource()), (1200, 1207));
        assert_eq!((types::AQR.resource(), types::ZST.resource()), (1100, 1114));
        assert_eq!(
            RC.matches(
                "
    1"
            )
            .count(),
            24 * LANGS.len()
        );
    }

    #[test]
    fn lproj() {
        let ko = LPROJ.iter().find(|(d, f, _)| *d == "ko.lproj" && *f == "ServicesMenu.strings").unwrap();
        assert!(ko.2.contains("\"Compress with Aqueous\" = \"Aqueous로 압축\";\n"), "{}", ko.2);
        assert_eq!(LPROJ.len(), 2 * LANGS.len());
    }
}
