//! Reads `messages/*.toml` (`en.toml` the source) and writes `messages.rs` into `OUT_DIR`: the
//! texts of every language, a module of each section with a constant and a function of each
//! message (its placeholders its parameters), Windows' string table (`RC`) and macOS' `.strings`
//! (`LPROJ`). The format is in README.md; anything amiss in a catalog fails the build.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::Path;

/// CLDR's plural categories, as tables of a plural message have them.
const PLURALS: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// The sections in Windows' string table (`RC`), read by Explorer itself (indirect strings in the
/// registry): no placeholders.
const RC_SECTIONS: [&str; 2] = ["types", "shell"];

/// The first resource id of the string table; a message's is this and its index.
const RESOURCE_BASE: u16 = 1000;

/// Windows' language (`LANGUAGE` of a resource script: primary and sub language) of each
/// language of the catalogs.
const RC_LANGUAGES: [(&str, &str); 2] = [("en", "0x09, 0x01"), ("ko", "0x12, 0x01")];

/// The sections written as `.strings` of the macOS bundle, and their files: keyed by the English
/// text, as Info.plist has it.
const LPROJ: [(&str, &str); 2] = [("types", "InfoPlist.strings"), ("services", "ServicesMenu.strings")];

const KEYWORDS: [&str; 38] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
];

struct Message {
    section: String,
    key: String,
    /// The forms of each language (indexed as the languages): (category, text); empty where the
    /// language has none.
    forms: Vec<Vec<(String, String)>>,
    plural: bool,
    /// The placeholders of the English text, in order.
    params: Vec<String>,
}

fn main() {
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("messages");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut langs: Vec<(String, toml::Table)> = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let tag = path.file_stem().unwrap().to_str().unwrap().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let table = text.parse::<toml::Table>().unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
        langs.push((tag, table));
    }
    // (English first, the others by their tags)
    langs.sort_by(|a, b| (a.0 != "en", &a.0).cmp(&(b.0 != "en", &b.0)));
    if langs.first().is_none_or(|(t, _)| t != "en") {
        fail("messages/en.toml is not there");
    }
    let messages = read(&langs);
    let tags: Vec<&str> = langs.iter().map(|(t, _)| t.as_str()).collect();
    let mut out = String::new();
    rust(&mut out, &tags, &messages);
    let rc = rc(&tags, &messages);
    writeln!(out, "/// Windows' string table of the sections {RC_SECTIONS:?}, a resource script (UTF-8).").unwrap();
    writeln!(out, "pub const RC: &str = {rc:?};").unwrap();
    writeln!(out, "/// The `.strings` of the macOS bundle: (folder, file, contents).").unwrap();
    writeln!(out, "pub const LPROJ: &[(&str, &str, &str)] = &[").unwrap();
    for (lang, tag) in tags.iter().enumerate() {
        for (section, file) in LPROJ {
            let strings = strings(lang, section, &messages);
            writeln!(out, "    ({:?}, {file:?}, {strings:?}),", format!("{tag}.lproj")).unwrap();
        }
    }
    writeln!(out, "];").unwrap();
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("messages.rs"), out).unwrap();
}

fn fail(e: &str) -> ! {
    panic!("aqr-l10n: {e}");
}

/// The messages of English's sections and keys, with the forms of each language.
fn read(langs: &[(String, toml::Table)]) -> Vec<Message> {
    let en = &langs[0].1;
    let mut messages = Vec::new();
    for (section, table) in en {
        ident(section);
        let toml::Value::Table(table) = table else { fail(&format!("en: {section} is not a section")) };
        for (key, value) in table {
            ident(key);
            let at = format!("en: {section}.{key}");
            let english = forms(&at, value);
            let plural = matches!(value, toml::Value::Table(_));
            let mut params = Vec::new();
            for (_, text) in &english {
                for p in placeholders(&at, text) {
                    if !params.contains(&p) {
                        params.push(p);
                    }
                }
            }
            if RC_SECTIONS.contains(&section.as_str()) && (plural || !params.is_empty()) {
                fail(&format!("{at}: a message of Windows' string table takes no placeholders"));
            }
            messages.push(Message { section: section.clone(), key: key.clone(), forms: vec![english], plural, params });
        }
    }
    for (tag, table) in &langs[1..] {
        for (section, t) in table {
            let toml::Value::Table(t) = t else { fail(&format!("{tag}: {section} is not a section")) };
            for key in t.keys() {
                if !messages.iter().any(|m| m.section == *section && m.key == *key) {
                    fail(&format!("{tag}: {section}.{key} is not in en.toml"));
                }
            }
        }
        for m in &mut messages {
            let at = format!("{tag}: {}.{}", m.section, m.key);
            let forms = match table.get(&m.section).and_then(|t| t.get(&m.key)) {
                None => Vec::new(),
                Some(v @ toml::Value::Table(_)) if !m.plural => {
                    fail(&format!("{at}: a plural of a message that is not ({v})"))
                }
                Some(v) => forms(&at, v),
            };
            for (_, text) in &forms {
                for p in placeholders(&at, text) {
                    if !m.params.contains(&p) {
                        fail(&format!("{at}: {{{p}}} is not a placeholder of the English text"));
                    }
                }
            }
            m.forms.push(forms);
        }
    }
    messages
}

/// The forms of a message: a string (the category "other"), or a table of categories.
fn forms(at: &str, v: &toml::Value) -> Vec<(String, String)> {
    match v {
        toml::Value::String(s) => vec![("other".into(), s.clone())],
        toml::Value::Table(t) => {
            let mut forms = Vec::new();
            for (category, text) in t {
                if !PLURALS.contains(&category.as_str()) {
                    fail(&format!("{at}: {category} is not a plural category ({PLURALS:?})"));
                }
                let toml::Value::String(text) = text else { fail(&format!("{at}.{category} is not a string")) };
                forms.push((category.clone(), text.clone()));
            }
            if !t.contains_key("other") {
                fail(&format!("{at}: a plural without \"other\""));
            }
            forms
        }
        _ => fail(&format!("{at} is not a string or a plural")),
    }
}

/// Fails where `s` is not a name for Rust (of lowercase letters, digits and `_`).
fn ident(s: &str) {
    let ok = s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.contains(&s);
    if !ok {
        fail(&format!("{s:?} is not a name (lowercase letters, digits and _, not a keyword of Rust)"));
    }
}

/// The placeholders of `text`, in order (`{{` and `}}` being braces).
fn placeholders(at: &str, text: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => _ = chars.next(),
            '}' if chars.peek() == Some(&'}') => _ = chars.next(),
            '{' => {
                let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                ident(&name);
                v.push(name);
            }
            '}' => fail(&format!("{at}: a lone }} in {text:?}")),
            _ => {}
        }
    }
    v
}

/// `{{` and `}}` of `text` as braces.
fn unescape(text: &str) -> String {
    text.replace("{{", "{").replace("}}", "}")
}

/// The Rust: the languages, the texts, and the sections' modules.
fn rust(out: &mut String, tags: &[&str], messages: &[Message]) {
    let n = messages.len();
    writeln!(out, "// Written by aqr-l10n's build.rs from messages/*.toml.\n").unwrap();
    writeln!(out, "/// The languages of the catalogs, English first.").unwrap();
    writeln!(out, "pub const LANGS: [&str; {}] = {tags:?};", tags.len()).unwrap();
    writeln!(out, "/// The first resource id of Windows' string table.").unwrap();
    writeln!(out, "pub const RESOURCE_BASE: u16 = {RESOURCE_BASE};").unwrap();
    writeln!(out, "/// The forms of each message of each language (none: not translated).").unwrap();
    writeln!(out, "static TEXTS: [[&[(Plural, &str)]; {n}]; {}] = [", tags.len()).unwrap();
    for lang in 0..tags.len() {
        writeln!(out, "    [").unwrap();
        for m in messages {
            let plain = !m.plural && m.params.is_empty();
            let forms: Vec<String> = m.forms[lang]
                .iter()
                .map(|(c, t)| {
                    let t = if plain { unescape(t) } else { t.clone() };
                    format!("(Plural::{}, {t:?})", capitalize(c))
                })
                .collect();
            writeln!(out, "        &[{}],", forms.join(", ")).unwrap();
        }
        writeln!(out, "    ],").unwrap();
    }
    writeln!(out, "];\n").unwrap();
    let sections: Vec<&str> =
        messages.iter().map(|m| m.section.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
    for section in sections {
        writeln!(out, "pub mod {section} {{").unwrap();
        writeln!(out, "    #![allow(clippy::too_many_arguments)]").unwrap();
        writeln!(out, "    use super::Msg;").unwrap();
        writeln!(out, "    #[allow(unused_imports)]\n    use std::fmt::Display;").unwrap();
        for (i, m) in messages.iter().enumerate().filter(|(_, m)| m.section == section) {
            let english: Vec<String> = m.forms[0].iter().map(|(c, t)| format!("{c}: {t:?}")).collect();
            let doc = english.join("; ").replace('\n', "\\n");
            let name = &m.key;
            writeln!(out, "    /// {doc}").unwrap();
            writeln!(out, "    pub const {}: Msg = Msg({i});", name.to_uppercase()).unwrap();
            writeln!(out, "    /// {doc}").unwrap();
            if !m.plural && m.params.is_empty() {
                writeln!(out, "    pub fn {name}() -> &'static str {{ {}.text() }}", name.to_uppercase()).unwrap();
                continue;
            }
            let others: Vec<&String> = m.params.iter().filter(|p| !(m.plural && *p == "n")).collect();
            let mut params: Vec<String> = others.iter().map(|p| format!("{p}: impl Display")).collect();
            let mut args: Vec<String> = others.iter().map(|p| format!("({p:?}, &{p})")).collect();
            if m.plural {
                params.insert(0, "n: u64".into());
                args.insert(0, "(\"n\", &n)".into());
            }
            let n = if m.plural { "Some(n)" } else { "None" };
            writeln!(
                out,
                "    pub fn {name}({}) -> String {{ {}.fill({n}, &[{}]) }}",
                params.join(", "),
                name.to_uppercase(),
                args.join(", ")
            )
            .unwrap();
        }
        writeln!(out, "}}\n").unwrap();
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
}

/// The string table: a block of each language of [`RC_LANGUAGES`], of the messages of
/// [`RC_SECTIONS`] (English's where the language has none).
fn rc(tags: &[&str], messages: &[Message]) -> String {
    let mut out = String::from(
        "// Written by aqr-l10n's build.rs from messages/*.toml: what Explorer reads itself (indirect\n\
         // strings, \"@aqr-win.exe,-ID\"), each message at RESOURCE_BASE and its index. All ASCII: what\n\
         // is not is escaped (UTF-16), as rc.exe, windres and llvm-rc all read it so.\n",
    );
    for (lang, tag) in tags.iter().enumerate() {
        let Some((_, id)) = RC_LANGUAGES.iter().find(|(t, _)| t == tag) else {
            fail(&format!("{tag}: no Windows language in build.rs's RC_LANGUAGES"))
        };
        writeln!(out, "\nLANGUAGE {id}\nSTRINGTABLE\nBEGIN").unwrap();
        for (i, m) in messages.iter().enumerate().filter(|(_, m)| RC_SECTIONS.contains(&m.section.as_str())) {
            let text = m.forms[lang].first().or(m.forms[0].first()).map(|(_, t)| unescape(t)).unwrap();
            writeln!(out, "    {}, L\"{}\"", RESOURCE_BASE as usize + i, rc_string(&text)).unwrap();
        }
        writeln!(out, "END").unwrap();
    }
    out
}

/// `text` in a wide string of a resource script: `"` doubled, `\\` and what is not printable ASCII
/// escaped (each UTF-16 unit as `\\x` and four digits, which rc.exe takes at most).
fn rc_string(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '"' => out += "\"\"",
            '\\' => out += "\\\\",
            ' '..='~' => out.push(c),
            _ => {
                for u in c.encode_utf16(&mut [0; 2]) {
                    write!(out, "\\x{u:04x}").unwrap();
                }
            }
        }
    }
    out
}

/// The `.strings` of `section` in language `lang`: each English text and its translation
/// (English where there is none).
fn strings(lang: usize, section: &str, messages: &[Message]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"));
    let mut out = String::from("/* Written by aqr-l10n from its catalogs (messages). */\n");
    for m in messages.iter().filter(|m| m.section == section) {
        let english = unescape(&m.forms[0][0].1);
        let text = m.forms[lang].first().map_or(english.clone(), |(_, t)| unescape(t));
        writeln!(out, "{} = {};", quote(&english), quote(&text)).unwrap();
    }
    out
}
