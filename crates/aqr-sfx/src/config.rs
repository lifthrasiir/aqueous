//! The settings of a self-extracting program (docs/sfx-config-design.md): what it does once run,
//! compiled from the `.aqr/sfx/sfx.toml` of its archive when it is made ([`compile`], feature
//! `encode`) and carried as a record of CBOR between the bundle of blobs and the archive
//! ([`Config::record`]), which the stub reads ([`Config::before`], [`Config::at`]); it never reads
//! the toml. What the settings make of the program's own image (icon, `VERSIONINFO`, manifest,
//! subsystem) is [`compile`]'s [`Pe`], which [`crate::pe`] writes into the stub.
//!
//! ```text
//! record = "AQRSFXC1" le64:n cbor[n] le64:n "AQRSFXC1"
//! ```
//!
//! The magic and the length at both ends, so that the stub finds the record after the bundle
//! (reading forward) or right before an archive it found by scanning (reading back). The CBOR
//! is one map of negative integer keys, as the archive's optional metadata (spec, "Optional
//! metadata"); keys not known are ignored, and a [`VERSION`] not known fails:
//!
//! ```cddl
//! config = {
//!   &(version: -1) => 1,
//!   ? &(title: -2) => text,
//!   ? &(dest: -3) => tstr,        ; "{cwd}" where absent
//!   ? &(subdir: -4) => bool,      ; false
//!   ? &(overwrite: -5) => 0..3,   ; ask, always, never, newer; absent: a file there fails
//!   ? &(prompt: -6) => text,
//!   ? &(finish: -7) => text,
//!   ? &(quiet: -8) => bool,       ; false
//!   ? &(run: -9) => run,
//!   ? &(license: -10) => tstr,    ; a file of .aqr/sfx/, `license.txt`: `license.ko.txt` for ko
//!   * int => any,
//! }
//! text = tstr / { + tstr => tstr }  ; by language tag ("" of none)
//! run = {
//!   &(program: -1) => [+ tstr],   ; the first a path of the archive, by /; {dest} in the
//!                                 ; others (and in env's values) where the files went
//!   ? &(cwd: -2) => tstr,         ; "."
//!   ? &(window: -3) => 0..2,      ; normal, hidden, minimized
//!   ? &(wait: -4) => bool,        ; true
//!   ? &(cleanup: -5) => bool,     ; false
//!   ? &(exit-code: -6) => bool,   ; false
//!   ? &(env: -7) => { * tstr => tstr },  ; set for it
//!   * int => any,
//! }
//! ```

use aqr_archive::cbor::{self, Cbor};

/// The magic at both ends of a record.
pub const MAGIC: &[u8; 8] = b"AQRSFXC1";

/// The version of the settings this stub reads.
pub const VERSION: i128 = 1;

/// The places a destination starts at, as the stub tells them as it runs: a new directory of
/// the system's temporary directory (removed with what was written, where the program is
/// waited for and cleaned up after), the program's directory, the working directory, the
/// directory of programs (`%ProgramFiles%`).
pub const PLACES: [&str; 4] = ["{temp}", "{here}", "{cwd}", "{programs}"];

/// A text of the settings, in one language or several: each its language's tag (`ko`,
/// `en-US`; empty for the one of no language) and the text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Text(pub Vec<(String, String)>);

impl Text {
    /// A text of no language.
    pub fn plain(s: &str) -> Text {
        Text(vec![(String::new(), s.into())])
    }

    /// The text for language `lang` (a tag, `ko-KR`): of that tag, else of its first subtag
    /// (`ko`, `ko-KP`), else of no language, else English, else the first.
    pub fn pick(&self, lang: &str) -> &str {
        let primary = |t: &str| t.split('-').next().unwrap_or("").to_ascii_lowercase();
        let of = |f: &dyn Fn(&str) -> bool| self.0.iter().find(|(l, _)| f(l)).map(|(_, t)| &t[..]);
        of(&|l| l.eq_ignore_ascii_case(lang))
            .or_else(|| of(&|l| !l.is_empty() && primary(l) == primary(lang)))
            .or_else(|| of(&|l| l.is_empty()))
            .or_else(|| of(&|l| primary(l) == "en"))
            .or_else(|| self.0.first().map(|(_, t)| &t[..]))
            .unwrap_or("")
    }
}

/// The names of the file `name` of `.aqr/sfx/` for language `lang` in the order they are
/// looked for (`license.txt` for `ko-KR`: `license.ko-KR.txt`, `license.ko.txt`, `license.txt`).
pub fn names_for(name: &str, lang: &str) -> Vec<String> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !s.ends_with('/') => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    let primary = lang.split('-').next().unwrap_or("");
    let mut names = Vec::new();
    for l in [lang, primary] {
        let n = format!("{stem}.{l}{ext}");
        if !l.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    names.push(name.into());
    names
}

/// What a self-extracting program does once run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The title of its windows (the program's name where none).
    pub title: Option<Text>,
    /// Where the files go: one of [`PLACES`], and a relative path under it by `/`.
    pub dest: String,
    /// The files under a directory of the program's name (`foo.exe`: `foo`) there.
    pub subdir: bool,
    pub overwrite: Overwrite,
    /// Asked before anything is written (yes or no).
    pub prompt: Option<Text>,
    /// Told once done.
    pub finish: Option<Text>,
    /// The license agreed to before anything is written: a file of `.aqr/sfx/` in the
    /// archive's first segment, of its language where there is one ([`names_for`]).
    pub license: Option<String>,
    /// Nothing asked nor told (as the command line's `-y`).
    pub quiet: bool,
    /// The program run once the files are written.
    pub run: Option<Run>,
}

/// What is done with a file where the archive's file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    /// Fails (the program with no settings).
    Fail,
    Ask,
    Always,
    Never,
    /// Written over where the archive's file is newer (by its modification time).
    Newer,
}

/// The program run once the files are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Its arguments, the first a path of the archive (by `/`) to the program.
    pub program: Vec<String>,
    /// Its working directory, relative to where the files went (by `/`).
    pub cwd: String,
    pub window: Window,
    /// Waited for (else the self-extracting program ends as it starts).
    pub wait: bool,
    /// The files written removed once it ends (waited for).
    pub cleanup: bool,
    /// Its exit code the self-extracting program's (waited for).
    pub exit_code: bool,
    /// The environment variables set for it (its own those of the self-extracting program),
    /// each its name and value.
    pub env: Vec<(String, String)>,
}

/// What `{dest}` stands for in the arguments of the program run and the values of its
/// environment ([`Run::program`], [`Run::env`]): the directory the files went to.
pub const DEST: &str = "{dest}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Normal,
    Hidden,
    Minimized,
}

impl Default for Config {
    /// The program with no settings: the files under the working directory, nothing there
    /// written over.
    fn default() -> Config {
        Config {
            title: None,
            dest: "{cwd}".into(),
            subdir: false,
            overwrite: Overwrite::Fail,
            prompt: None,
            finish: None,
            license: None,
            quiet: false,
            run: None,
        }
    }
}

// keys
const K_VERSION: i128 = -1;
const K_TITLE: i128 = -2;
const K_DEST: i128 = -3;
const K_SUBDIR: i128 = -4;
const K_OVERWRITE: i128 = -5;
const K_PROMPT: i128 = -6;
const K_FINISH: i128 = -7;
const K_QUIET: i128 = -8;
const K_RUN: i128 = -9;
const K_LICENSE: i128 = -10;
const R_PROGRAM: i128 = -1;
const R_CWD: i128 = -2;
const R_WINDOW: i128 = -3;
const R_WAIT: i128 = -4;
const R_CLEANUP: i128 = -5;
const R_EXIT_CODE: i128 = -6;
const R_ENV: i128 = -7;

/// CBOR's simple values false and true.
const FALSE: u8 = 20;
const TRUE: u8 = 21;

fn put_text(out: &mut Vec<u8>, s: &str) {
    cbor::head(out, 3, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn put_texts(out: &mut Vec<u8>, t: &Text) {
    match &t.0[..] {
        [(l, s)] if l.is_empty() => put_text(out, s),
        all => {
            cbor::head(out, 5, all.len() as u64);
            for (l, s) in all {
                put_text(out, l);
                put_text(out, s);
            }
        }
    }
}

fn put_bool(out: &mut Vec<u8>, b: bool) {
    out.push(0xe0 | if b { TRUE } else { FALSE });
}

impl Config {
    /// The settings as CBOR (the record's).
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pairs: Vec<(i128, Vec<u8>)> = Vec::new();
        let mut put = |k: i128, f: &dyn Fn(&mut Vec<u8>)| {
            let mut v = Vec::new();
            f(&mut v);
            pairs.push((k, v));
        };
        put(K_VERSION, &|v| cbor::int(v, VERSION));
        if let Some(t) = &self.title {
            put(K_TITLE, &|v| put_texts(v, t));
        }
        put(K_DEST, &|v| put_text(v, &self.dest));
        put(K_SUBDIR, &|v| put_bool(v, self.subdir));
        let overwrite = match self.overwrite {
            Overwrite::Fail => None,
            Overwrite::Ask => Some(0),
            Overwrite::Always => Some(1),
            Overwrite::Never => Some(2),
            Overwrite::Newer => Some(3),
        };
        if let Some(o) = overwrite {
            put(K_OVERWRITE, &|v| cbor::int(v, o));
        }
        if let Some(t) = &self.prompt {
            put(K_PROMPT, &|v| put_texts(v, t));
        }
        if let Some(t) = &self.finish {
            put(K_FINISH, &|v| put_texts(v, t));
        }
        if let Some(l) = &self.license {
            put(K_LICENSE, &|v| put_text(v, l));
        }
        put(K_QUIET, &|v| put_bool(v, self.quiet));
        if let Some(r) = &self.run {
            put(K_RUN, &|v| {
                cbor::head(v, 5, 7);
                cbor::int(v, R_PROGRAM);
                cbor::head(v, 4, r.program.len() as u64);
                r.program.iter().for_each(|a| put_text(v, a));
                cbor::int(v, R_CWD);
                put_text(v, &r.cwd);
                cbor::int(v, R_WINDOW);
                cbor::int(v, r.window as i128);
                for (k, b) in [(R_WAIT, r.wait), (R_CLEANUP, r.cleanup), (R_EXIT_CODE, r.exit_code)] {
                    cbor::int(v, k);
                    put_bool(v, b);
                }
                cbor::int(v, R_ENV);
                cbor::head(v, 5, r.env.len() as u64);
                for (n, x) in &r.env {
                    put_text(v, n);
                    put_text(v, x);
                }
            });
        }
        cbor::head(&mut out, 5, pairs.len() as u64);
        for (k, v) in pairs {
            cbor::int(&mut out, k);
            out.extend(v);
        }
        out
    }

    /// The settings of CBOR `d` ([`Config::to_cbor`]).
    pub fn from_cbor(d: &[u8]) -> Result<Config, String> {
        let bad = |what: &str| format!("the program's settings: {what}");
        let item = cbor::whole_item(d).map_err(|e| bad(&e.to_string()))?;
        let Cbor::Map(m) = item else { return Err(bad("not a map")) };
        let text = |v: &Cbor, what: &str| match v {
            Cbor::Text(t) => Ok(String::from_utf8_lossy(t).into_owned()),
            _ => Err(bad(&format!("{what} not a text"))),
        };
        let texts = |v: &Cbor, what: &str| match v {
            Cbor::Map(m) => {
                let t: Result<Vec<_>, String> =
                    m.pairs().map(|(l, s)| Ok((text(&l, what)?, text(&s, what)?))).collect();
                Ok(Text(t?)).and_then(|t| if t.0.is_empty() { Err(bad(&format!("{what} of no text"))) } else { Ok(t) })
            }
            v => Ok(Text::plain(&text(v, what)?)),
        };
        let boolean = |v: &Cbor, what: &str| match v {
            Cbor::Simple(FALSE) => Ok(false),
            Cbor::Simple(TRUE) => Ok(true),
            _ => Err(bad(&format!("{what} not a boolean"))),
        };
        let int = |v: &Cbor, what: &str, max: i128| match v {
            Cbor::Int(i) if (0..=max).contains(i) => Ok(*i),
            _ => Err(bad(&format!("{what} not one of those known"))),
        };
        let mut c = Config::default();
        let mut version = None;
        for (k, v) in m.pairs() {
            let Cbor::Int(k) = k else { continue };
            match k {
                K_VERSION => version = Some(v),
                K_TITLE => c.title = Some(texts(&v, "the title")?),
                K_DEST => c.dest = text(&v, "the destination")?,
                K_SUBDIR => c.subdir = boolean(&v, "subdir")?,
                K_OVERWRITE => {
                    c.overwrite = [Overwrite::Ask, Overwrite::Always, Overwrite::Never, Overwrite::Newer]
                        [int(&v, "overwrite", 3)? as usize]
                }
                K_PROMPT => c.prompt = Some(texts(&v, "the prompt")?),
                K_FINISH => c.finish = Some(texts(&v, "the finish")?),
                K_LICENSE => c.license = Some(text(&v, "the license")?),
                K_QUIET => c.quiet = boolean(&v, "quiet")?,
                K_RUN => {
                    let Cbor::Map(r) = v else { return Err(bad("run not a map")) };
                    let mut run = Run {
                        program: Vec::new(),
                        cwd: ".".into(),
                        window: Window::Normal,
                        wait: true,
                        cleanup: false,
                        exit_code: false,
                        env: Vec::new(),
                    };
                    for (k, v) in r.pairs() {
                        let Cbor::Int(k) = k else { continue };
                        match k {
                            R_PROGRAM => {
                                let Cbor::Array(a) = v else { return Err(bad("the program not an array")) };
                                run.program = a.iter().map(|a| text(&a, "an argument")).collect::<Result<_, _>>()?;
                            }
                            R_CWD => run.cwd = text(&v, "the working directory")?,
                            R_WINDOW => {
                                run.window = [Window::Normal, Window::Hidden, Window::Minimized]
                                    [int(&v, "the window", 2)? as usize]
                            }
                            R_WAIT => run.wait = boolean(&v, "wait")?,
                            R_CLEANUP => run.cleanup = boolean(&v, "cleanup")?,
                            R_EXIT_CODE => run.exit_code = boolean(&v, "exit-code")?,
                            R_ENV => {
                                let Cbor::Map(e) = v else { return Err(bad("env not a map")) };
                                run.env = e
                                    .pairs()
                                    .map(|(n, x)| Ok((text(&n, "a variable's name")?, text(&x, "a variable")?)))
                                    .collect::<Result<_, String>>()?;
                            }
                            _ => {}
                        }
                    }
                    if run.program.is_empty() {
                        return Err(bad("no program to run"));
                    }
                    c.run = Some(run);
                }
                _ => {}
            }
        }
        match version {
            Some(Cbor::Int(VERSION)) => Ok(c),
            Some(Cbor::Int(v)) => Err(bad(&format!("of version {v}, not known to this program (of {VERSION})"))),
            _ => Err(bad("no version")),
        }
    }

    /// The record of the settings, as the program carries it.
    pub fn record(&self) -> Vec<u8> {
        let c = self.to_cbor();
        let n = (c.len() as u64).to_le_bytes();
        [&MAGIC[..], &n, &c, &n, MAGIC].concat()
    }

    /// The settings of the record at the start of `d` and its length, if one is there.
    pub fn at(d: &[u8]) -> Option<Result<(Config, usize), String>> {
        let n = d.strip_prefix(MAGIC)?.first_chunk::<8>()?;
        let n = usize::try_from(u64::from_le_bytes(*n)).ok()?;
        let len = n.checked_add(32)?;
        let r = d.get(..len)?;
        if &r[len - 8..] != MAGIC || r[len - 16..len - 8] != r[8..16] {
            return None;
        }
        Some(Config::from_cbor(&r[16..16 + n]).map(|c| (c, len)))
    }

    /// The settings of the record that ends at `end` of `d` and where it starts, if one is there.
    pub fn before(d: &[u8], end: usize) -> Option<Result<(Config, usize), String>> {
        let d = d.get(..end)?;
        let n = d.strip_suffix(MAGIC)?.last_chunk::<8>()?;
        let start = end.checked_sub(usize::try_from(u64::from_le_bytes(*n)).ok()?.checked_add(32)?)?;
        Some(Config::at(&d[start..])?.map(|(c, _)| (c, start)))
    }
}

/// The path under which the files go, by [`Config::dest`] and the places as `place` tells them
/// ([`PLACES`], the braces left out).
pub fn destination(
    dest: &str,
    place: &mut dyn FnMut(&str) -> Result<std::path::PathBuf, String>,
) -> Result<std::path::PathBuf, String> {
    let (name, rest) = split_place(dest).ok_or_else(|| format!("{dest}: not of a place known"))?;
    let mut p = place(name)?;
    rest.split('/').filter(|s| !s.is_empty()).for_each(|s| p.push(crate::sanitized(s.as_bytes())));
    Ok(p)
}

/// Of a destination, the name of its place ([`PLACES`], no braces) and the path after it.
fn split_place(dest: &str) -> Option<(&str, &str)> {
    let p = PLACES.iter().find(|p| dest.starts_with(*p))?;
    let rest = &dest[p.len()..];
    (rest.is_empty() || rest.starts_with('/')).then(|| (&p[1..p.len() - 1], rest))
}

// ---- the files of .aqr/sfx/

/// The path of the directory of the settings in an archive's root.
pub const DIR: [&[u8]; 2] = [b".aqr", b"sfx"];

/// The entry of the directory `.aqr/sfx/` of the root of `t` ([`DIR`]), if there is one.
pub fn dir_of(t: &aqr_archive::Tree) -> Option<usize> {
    let mut at = None;
    for name in DIR {
        at = Some(
            t.entries.iter().position(|e| e.parent == at && e.name == name && e.node == aqr_archive::EntryNode::Dir)?,
        );
    }
    at
}

/// The path (by `/`) of entry `i` of `t`, under entry `under` where given (else the root).
pub fn path_of(t: &aqr_archive::Tree, mut i: usize, under: Option<usize>) -> String {
    let mut parts = Vec::new();
    while Some(i) != under {
        let e = &t.entries[i];
        parts.push(String::from_utf8_lossy(&e.name).into_owned());
        match e.parent {
            Some(p) => i = p,
            None => break,
        }
    }
    parts.reverse();
    parts.join("/")
}

/// The files under `.aqr/sfx/` of `t`, each its path there (by `/`) and its content.
pub fn files_of(t: &aqr_archive::Tree) -> Vec<(String, usize)> {
    let Some(dir) = dir_of(t) else { return Vec::new() };
    let mut files = Vec::new();
    for (i, e) in t.entries.iter().enumerate() {
        let aqr_archive::EntryNode::File(c) = e.node else { continue };
        let mut p = e.parent;
        while let Some(q) = p {
            if q == dir {
                files.push((path_of(t, i, Some(dir)), c));
                break;
            }
            p = t.entries[q].parent;
        }
    }
    files
}

/// The files of `.aqr/sfx/` that `wanted` names (by their paths there) of the first segment of
/// archive `c` (docs/sfx-config-design.md, 2.1: where the compressor puts them, and where alone
/// they are looked for, so that a segment appended does not change them), decoded; those not
/// there left out.
pub fn read_files(
    c: &[u8],
    runner: &dyn aqr_archive::Runner,
    threads: usize,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<std::collections::HashMap<String, Vec<u8>>, String> {
    let err = |e: aqr_archive::Failure| format!(".aqr/sfx/: {e:?}");
    let all = aqr_archive::open(c, runner).map_err(err)?;
    let Some(&first) = all.segment_metadata().map_err(err)?.first() else { return Ok(Default::default()) };
    drop(all);
    // (decoded whole: the first segment has the files of .aqr/sfx/ alone, and so the stub uses
    // no other way of decoding than the one it extracts by)
    let a = aqr_archive::open_upto(c, runner, first).map_err(err)?;
    let aqr_archive::Value::Tree(t, mut contents) = a.value(threads).map_err(err)? else {
        return Ok(Default::default());
    };
    let files = files_of(&t).into_iter().filter(|(n, _)| wanted(n));
    Ok(files.filter_map(|(n, c)| Some((n, std::mem::take(contents.get_mut(c)?)))).collect())
}

// ---- compiling sfx.toml (aqr)

/// What the settings make of the program's own image ([`crate::pe::apply`]).
#[cfg(feature = "encode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pe {
    /// Of the subsystem of windows (2), not of the console (3).
    pub gui: bool,
    /// An icon (`.ico`'s bytes, checked).
    pub icon: Option<Vec<u8>>,
    /// The manifest (made, or `manifest.xml`'s).
    pub manifest: Vec<u8>,
    pub version: Option<VersionInfo>,
}

/// What `VERSIONINFO` tells.
#[cfg(feature = "encode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub numbers: [u16; 4],
    /// The strings of the table, each its name and value.
    pub strings: Vec<(String, String)>,
}

/// The settings and what they make of the image.
#[cfg(feature = "encode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub config: Config,
    pub pe: Pe,
}

/// What is at a path of the archive ([`compile`]).
#[cfg(feature = "encode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// The settings `.aqr/sfx/sfx.toml` tells, checked: `file` gives a file of `.aqr/sfx/` by its
/// path there (by `/`), `kind` tells what is at a path of the archive. A key not known, a file
/// it names not there, a program not in the archive fail here, not when the program runs.
#[cfg(feature = "encode")]
pub fn compile(
    files: &std::collections::HashMap<String, Vec<u8>>,
    kind: &dyn Fn(&str) -> Option<Kind>,
) -> Result<Compiled, String> {
    let file = &|n: &str| files.get(n).cloned();
    let toml = file("sfx.toml").ok_or(".aqr/sfx/sfx.toml: not there")?;
    let toml = String::from_utf8(toml).map_err(|_| ".aqr/sfx/sfx.toml: not UTF-8".to_string())?;
    let table: toml::Table = toml.parse().map_err(|e| format!(".aqr/sfx/sfx.toml: {e}"))?;
    let mut t = Toml::new(&table, "");

    let mode = t.text("mode")?;
    let mut ex = t.section("extract")?;
    let mut run = t.section("run")?;
    let mut ui = t.section("ui")?;
    let mut pe = t.section("pe")?;
    let title = t.texts("title")?;
    t.done()?;

    let running = match mode.as_deref() {
        Some("run") => true,
        Some("extract") => false,
        None => run.table.contains_key("program"),
        Some(m) => return Err(format!("mode: {m:?}, not \"extract\" nor \"run\"")),
    };
    // extract
    let dest = ex.text("dest")?.unwrap_or_else(|| if running { "{temp}" } else { "{cwd}" }.into());
    check_dest(&dest)?;
    let subdir = ex.boolean("subdir")?.unwrap_or(false);
    let overwrite = match ex.text("overwrite")?.as_deref() {
        None if running => Overwrite::Always,
        None | Some("ask") => Overwrite::Ask,
        Some("always") => Overwrite::Always,
        Some("never") => Overwrite::Never,
        Some("newer") => Overwrite::Newer,
        Some(o) => return Err(format!("extract.overwrite: {o:?}, not \"ask\", \"always\", \"never\" nor \"newer\"")),
    };
    if ex.boolean("ask")?.is_some() {
        return Err("extract.ask: not yet (a path asked for comes with the program's dialogs)".into());
    }
    ex.done()?;

    // run
    let program = run.strings("program")?;
    let cwd = run.text("cwd")?;
    let window = match run.text("window")?.as_deref() {
        None | Some("normal") => Window::Normal,
        Some("hidden") => Window::Hidden,
        Some("minimized") => Window::Minimized,
        Some(w) => return Err(format!("run.window: {w:?}, not \"normal\", \"hidden\" nor \"minimized\"")),
    };
    let wait = run.boolean("wait")?;
    let cleanup = run.boolean("cleanup")?;
    let exit_code = run.boolean("exit-code")?;
    let env = run.strings_by_name("env")?;
    for (n, _) in &env {
        if n.is_empty() || n.contains(['=', '\0']) {
            return Err(format!("run.env: {n:?}: not a name of an environment variable"));
        }
    }
    let any = !run.table.is_empty();
    run.done()?;
    let run = match (running, program) {
        (false, _) if any => return Err("[run] with mode = \"extract\" (nothing is run)".into()),
        (false, _) => None,
        (true, None) => return Err("mode = \"run\" with no run.program".into()),
        (true, Some(program)) => {
            check_program(&program, kind)?;
            let cwd = cwd.unwrap_or_else(|| ".".into());
            if cwd != "." {
                check_relative(&cwd).map_err(|e| format!("run.cwd: {cwd}: {e}"))?;
                if kind(&cwd) != Some(Kind::Dir) {
                    return Err(format!("run.cwd: {cwd}: not a directory of the archive"));
                }
            }
            let wait = wait.unwrap_or(true);
            let (cleanup, exit_code) = (cleanup.unwrap_or(wait), exit_code.unwrap_or(wait));
            if !wait && (cleanup || exit_code) {
                let what = if cleanup { "cleanup" } else { "exit-code" };
                return Err(format!("run.{what} with run.wait = false (the program is not waited for)"));
            }
            Some(Run { program, cwd, window, wait, cleanup, exit_code, env })
        }
    };

    // ui
    if ui.table.contains_key("banner") {
        return Err("ui.banner: not yet (docs/sfx-config-design.md, 9)".into());
    }
    let license = match ui.text("license")? {
        Some(name) => {
            // (each of its languages' files too: text, as it is shown)
            for (n, d) in std::iter::once(name.clone())
                .chain(language_files(files, &name))
                .filter_map(|n| Some((n.clone(), sfx_file(file, "ui.license", &n).ok()?)))
            {
                if std::str::from_utf8(&d).is_err() {
                    return Err(format!("ui.license: {n}: not UTF-8"));
                }
            }
            sfx_file(file, "ui.license", &name)?;
            Some(name)
        }
        None => None,
    };
    let prompt = ui.texts("prompt")?;
    let finish = ui.texts("finish")?;
    let quiet = ui.boolean("quiet")?.unwrap_or(false);
    ui.done()?;

    // pe
    let gui = match pe.text("subsystem")?.as_deref() {
        None | Some("cli") => false,
        Some("gui") => true,
        Some(s) => return Err(format!("pe.subsystem: {s:?}, not \"gui\" nor \"cli\"")),
    };
    let icon = match pe.text("icon")? {
        Some(name) => {
            let d = sfx_file(file, "pe.icon", &name)?;
            crate::pe::icon_images(&d).map_err(|e| format!("pe.icon: {name}: {e}"))?;
            Some(d)
        }
        None => None,
    };
    let admin = pe.boolean("admin")?;
    let manifest = match file("manifest.xml") {
        Some(_) if admin.is_some() => {
            return Err("pe.admin with .aqr/sfx/manifest.xml (whose requestedExecutionLevel tells)".into());
        }
        Some(m) => m,
        None => crate::pe::manifest(admin.unwrap_or(false), gui).into_bytes(),
    };
    let numbers = match pe.text("version")? {
        Some(v) => Some((
            parse_version(&v).ok_or_else(|| format!("pe.version: {v:?}, not of 1 to 4 numbers of 0 to 65535"))?,
            v,
        )),
        None => None,
    };
    let mut strings = Vec::new();
    if let Some((_, v)) = &numbers {
        strings.push(("FileVersion".to_string(), v.clone()));
        strings.push(("ProductVersion".to_string(), v.clone()));
    }
    for (key, name) in [
        ("product", "ProductName"),
        ("company", "CompanyName"),
        ("description", "FileDescription"),
        ("copyright", "LegalCopyright"),
    ] {
        if let Some(v) = pe.text(key)? {
            strings.push((name.to_string(), v));
        }
    }
    pe.done()?;
    let version = (!strings.is_empty()).then(|| VersionInfo { numbers: numbers.map_or([0; 4], |(n, _)| n), strings });

    let config = Config { title, dest, subdir, overwrite, prompt, finish, license, quiet, run };
    Ok(Compiled { config, pe: Pe { gui, icon, manifest, version } })
}

/// A section of sfx.toml read, its keys taken off as they are read ([`Toml::done`]: those left
/// are not known).
#[cfg(feature = "encode")]
struct Toml<'a> {
    table: &'a toml::Table,
    at: &'a str,
    /// The keys read.
    taken: Vec<String>,
}

#[cfg(feature = "encode")]
impl<'a> Toml<'a> {
    fn new(table: &'a toml::Table, at: &'a str) -> Toml<'a> {
        Toml { table, at, taken: Vec::new() }
    }

    fn name(&self, key: &str) -> String {
        if self.at.is_empty() { key.into() } else { format!("{}.{key}", self.at) }
    }

    fn take(&mut self, key: &str) -> Option<&'a toml::Value> {
        let v = self.table.get(key)?;
        self.taken.push(key.into());
        Some(v)
    }

    fn text(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{}: not a string", self.name(key))),
        }
    }

    /// A table of strings, each its key and value (none where it is not there).
    fn strings_by_name(&mut self, key: &str) -> Result<Vec<(String, String)>, String> {
        let name = self.name(key);
        match self.take(key) {
            None => Ok(Vec::new()),
            Some(toml::Value::Table(t)) => t
                .iter()
                .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())).ok_or(format!("{name}.{k}: not a string")))
                .collect(),
            Some(_) => Err(format!("{name}: not a table of strings")),
        }
    }

    /// A text, or a table of them by language tag ([`Text`]).
    fn texts(&mut self, key: &str) -> Result<Option<Text>, String> {
        let name = self.name(key);
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(Text::plain(s))),
            Some(toml::Value::Table(t)) if !t.is_empty() => {
                let mut all = Vec::new();
                for (l, v) in t {
                    let tag = !l.is_empty()
                        && l.split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()));
                    match v.as_str() {
                        Some(s) if tag => all.push((l.clone(), s.to_string())),
                        _ => return Err(format!("{name}.{l}: not a string of a language tag (en, ko, pt-BR)")),
                    }
                }
                Ok(Some(Text(all)))
            }
            Some(_) => Err(format!("{name}: not a string, nor a table of them by language")),
        }
    }

    fn boolean(&mut self, key: &str) -> Result<Option<bool>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Boolean(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("{}: not true nor false", self.name(key))),
        }
    }

    fn strings(&mut self, key: &str) -> Result<Option<Vec<String>>, String> {
        let name = self.name(key);
        let not = || format!("{name}: not an array of strings, one at least");
        match self.take(key) {
            None => Ok(None),
            Some(toml::Value::Array(a)) if !a.is_empty() => {
                a.iter().map(|v| v.as_str().map(String::from).ok_or_else(not)).collect::<Result<_, _>>().map(Some)
            }
            Some(_) => Err(not()),
        }
    }

    /// The section `key` (an empty one where there is none).
    fn section(&mut self, key: &'a str) -> Result<Toml<'a>, String> {
        static EMPTY: std::sync::OnceLock<toml::Table> = std::sync::OnceLock::new();
        match self.take(key) {
            None => Ok(Toml::new(EMPTY.get_or_init(toml::Table::new), key)),
            Some(toml::Value::Table(t)) => Ok(Toml::new(t, key)),
            Some(_) => Err(format!("{key}: not a section")),
        }
    }

    /// Fails on a key of the section not read.
    fn done(&self) -> Result<(), String> {
        match self.table.keys().find(|k| !self.taken.contains(k)) {
            Some(k) => Err(format!(".aqr/sfx/sfx.toml: {}: not a setting known", self.name(k))),
            None => Ok(()),
        }
    }
}

/// The files of `.aqr/sfx/` of other languages of `name` there (`license.ko.txt` of
/// `license.txt`): those of a language tag between its stem and its extension.
#[cfg(feature = "encode")]
fn language_files(files: &std::collections::HashMap<String, Vec<u8>>, name: &str) -> Vec<String> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    let mut of: Vec<String> = (files.keys())
        .filter(|n| {
            let tag = n.strip_prefix(stem).and_then(|r| r.strip_prefix('.')).and_then(|r| r.strip_suffix(&ext[..]));
            tag.is_some_and(|t| {
                !t.is_empty() && t.split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
            })
        })
        .cloned()
        .collect();
    of.sort();
    of
}

/// A file of `.aqr/sfx/` that setting `what` names.
#[cfg(feature = "encode")]
fn sfx_file(file: &dyn Fn(&str) -> Option<Vec<u8>>, what: &str, name: &str) -> Result<Vec<u8>, String> {
    check_relative(name).map_err(|e| format!("{what}: {name}: {e}"))?;
    file(name).ok_or_else(|| format!("{what}: {name}: not a file of .aqr/sfx/"))
}

/// Checks a path of the archive by `/`: relative, of names (no `.`, `..`, nor empty ones), no `\`.
#[cfg(feature = "encode")]
fn check_relative(p: &str) -> Result<(), &'static str> {
    if p.contains('\\') {
        return Err("a path by / (not \\)");
    }
    if p.split('/').any(|s| s.is_empty() || s == "." || s == "..") || p.contains(':') {
        return Err("not a path relative to the archive's root, of names alone");
    }
    Ok(())
}

#[cfg(feature = "encode")]
fn check_dest(dest: &str) -> Result<(), String> {
    let bad = || format!("extract.dest: {dest}: not of one of {} and a relative path after it", PLACES.join(" "));
    let (_, rest) = split_place(dest).ok_or_else(bad)?;
    match rest.strip_prefix('/') {
        None | Some("") => Ok(()),
        Some(r) => check_relative(r.trim_end_matches('/')).map_err(|_| bad()),
    }
}

#[cfg(feature = "encode")]
fn check_program(program: &[String], kind: &dyn Fn(&str) -> Option<Kind>) -> Result<(), String> {
    let p = &program[0];
    check_relative(p).map_err(|e| format!("run.program: {p}: {e}"))?;
    if p == ".aqr" || p.starts_with(".aqr/") {
        return Err(format!("run.program: {p}: in .aqr/ (not written where a program is run)"));
    }
    let ext = p.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("exe" | "com")) {
        return Err(format!("run.program: {p}: not a program (.exe): no shell runs it (.bat, .cmd, .msi, ...)"));
    }
    if kind(p) != Some(Kind::File) {
        return Err(format!("run.program: {p}: not a file of the archive"));
    }
    Ok(())
}

/// A version of 1 to 4 numbers by dots, the rest 0.
#[cfg(feature = "encode")]
fn parse_version(v: &str) -> Option<[u16; 4]> {
    let mut n = [0; 4];
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n[i] = p.parse().ok()?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_config() -> Config {
        Config {
            title: Some(Text::plain("Example Setup")),
            dest: "{temp}/x".into(),
            subdir: true,
            overwrite: Overwrite::Newer,
            prompt: Some(Text(vec![("en".into(), "Install?".into()), ("ko".into(), "설치할까요?".into())])),
            finish: None,
            license: Some("license.txt".into()),
            quiet: false,
            run: Some(Run {
                program: vec!["bin/setup.exe".into(), "/S".into(), "한글".into()],
                cwd: "bin".into(),
                window: Window::Minimized,
                wait: true,
                cleanup: true,
                exit_code: false,
                env: vec![("APP_DIR".into(), "{dest}/bin".into())],
            }),
        }
    }

    #[test]
    fn record_round_trips() {
        for c in [Config::default(), run_config()] {
            let r = c.record();
            let (back, n) = Config::at(&r).unwrap().unwrap();
            assert_eq!((back, n), (c.clone(), r.len()));
            // (found back from its end, after other bytes)
            let d = [&b"stub and bundle"[..], &r, b"archive"].concat();
            let (back, start) = Config::before(&d, 15 + r.len()).unwrap().unwrap();
            assert_eq!((back, start), (c, 15));
            assert!(Config::at(&d).is_none() && Config::before(&d, d.len()).is_none());
        }
    }

    /// Keys not known are ignored; a version not known fails.
    #[test]
    fn keys_and_versions_not_known() {
        let mut c = run_config().to_cbor();
        // (a map of one more pair: -100 => "x")
        assert!((0xa1..0xb7).contains(&c[0]));
        c[0] += 1;
        c.extend_from_slice(&[0x38, 99, 0x61, b'x']);
        assert_eq!(Config::from_cbor(&c).unwrap(), run_config());
        let mut v = run_config().to_cbor();
        assert_eq!(&v[1..3], &[0x20, 0x01]);
        v[2] = 0x02;
        assert!(Config::from_cbor(&v).unwrap_err().contains("version 2"));
    }

    #[test]
    fn texts_by_language() {
        let t = Text(vec![
            ("de".into(), "Deutsch".into()),
            ("en".into(), "English".into()),
            ("ko".into(), "한국어".into()),
        ]);
        for (lang, want) in
            [("ko-KR", "한국어"), ("ko", "한국어"), ("KO-kr", "한국어"), ("en-GB", "English"), ("fr-FR", "English")]
        {
            assert_eq!(t.pick(lang), want, "{lang}");
        }
        let t =
            Text(vec![("pt-BR".into(), "Brasil".into()), ("pt".into(), "Portugal".into()), ("".into(), "any".into())]);
        assert_eq!((t.pick("pt-BR"), t.pick("pt-PT"), t.pick("ja")), ("Brasil", "Brasil", "any"));
        assert_eq!(Text(vec![("de".into(), "Deutsch".into())]).pick("ja"), "Deutsch");
        assert_eq!(names_for("license.txt", "ko-KR"), ["license.ko-KR.txt", "license.ko.txt", "license.txt"]);
        assert_eq!(names_for("legal/LICENSE", "ko"), ["legal/LICENSE.ko", "legal/LICENSE"]);
        assert_eq!(names_for("license.txt", ""), ["license.txt"]);
    }

    #[test]
    fn destinations() {
        let mut place = |p: &str| Ok(std::path::PathBuf::from(format!("/{p}")));
        assert_eq!(destination("{cwd}", &mut place).unwrap(), std::path::Path::new("/cwd"));
        assert_eq!(destination("{here}/a/b", &mut place).unwrap(), std::path::Path::new("/here/a/b"));
        assert!(destination("{home}", &mut place).is_err());
        assert!(destination("{cwd}x", &mut place).is_err());
    }

    #[cfg(feature = "encode")]
    mod compiling {
        use super::super::*;
        use std::collections::HashMap;

        fn compiled(toml: &str, extra: &[(&str, &[u8])]) -> Result<Compiled, String> {
            let mut files: HashMap<String, Vec<u8>> = HashMap::new();
            files.insert("sfx.toml".into(), toml.as_bytes().to_vec());
            extra.iter().for_each(|(n, d)| drop(files.insert(n.to_string(), d.to_vec())));
            let archive = |p: &str| match p {
                "setup.exe" | "bin/tool.exe" | "readme.txt" | "run.bat" => Some(Kind::File),
                "bin" => Some(Kind::Dir),
                _ => None,
            };
            compile(&files, &archive)
        }

        #[test]
        fn license_and_texts() {
            let toml = "title = { en = \"Setup\", ko = \"설치\" }\n[ui]\nlicense = \"license.txt\"\nprompt = \"Go?\"\n";
            let c = compiled(toml, &[("license.txt", b"MIT"), ("license.ko.txt", "MIT, 한국어".as_bytes())]).unwrap();
            assert_eq!(c.config.license.as_deref(), Some("license.txt"));
            assert_eq!(c.config.title.as_ref().map(|t| t.pick("ko-KR")), Some("설치"));
            assert_eq!(c.config.prompt, Some(Text::plain("Go?")));
            let back = Config::from_cbor(&c.config.to_cbor()).unwrap();
            assert_eq!(back, c.config);
            // (the license there, each of its languages' files text)
            let e = compiled(toml, &[]).unwrap_err();
            assert!(e.contains("ui.license: license.txt: not a file of .aqr/sfx/"), "{e}");
            let e = compiled(toml, &[("license.txt", b"MIT"), ("license.ko.txt", b"\xff\xfe")]).unwrap_err();
            assert!(e.contains("license.ko.txt: not UTF-8"), "{e}");
            let e = compiled("title = { \"en_US\" = \"x\" }", &[]).unwrap_err();
            assert!(e.contains("title.en_US: not a string of a language tag"), "{e}");
            assert!(compiled("title = {}", &[]).is_err());
        }

        #[test]
        fn modes_and_their_defaults() {
            let c = compiled("mode = \"run\"\n[run]\nprogram = [\"setup.exe\", \"/S\"]\n", &[]).unwrap();
            let want = Run {
                program: vec!["setup.exe".into(), "/S".into()],
                cwd: ".".into(),
                window: Window::Normal,
                wait: true,
                cleanup: true,
                exit_code: true,
                env: Vec::new(),
            };
            assert_eq!(c.config.run, Some(want));
            assert_eq!((&c.config.dest[..], c.config.overwrite), ("{temp}", Overwrite::Always));
            assert!(!c.pe.gui && c.pe.icon.is_none() && c.pe.version.is_none());
            assert!(String::from_utf8_lossy(&c.pe.manifest).contains("asInvoker"));
            let e = compiled("title = \"x\"\n", &[]).unwrap();
            assert_eq!((e.config.run, &e.config.dest[..], e.config.overwrite), (None, "{cwd}", Overwrite::Ask));
            // (not waited for: neither cleaned up after nor its exit code)
            let n = compiled("[run]\nprogram = [\"bin/tool.exe\"]\nwait = false\ncwd = \"bin\"\n", &[]).unwrap();
            let r = n.config.run.unwrap();
            assert!(!r.wait && !r.cleanup && !r.exit_code && r.cwd == "bin");
        }

        #[test]
        fn pe_settings() {
            let mut ico = vec![0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0];
            ico.extend_from_slice(&8u32.to_le_bytes());
            ico.extend_from_slice(&22u32.to_le_bytes());
            ico.extend_from_slice(b"\x89PNG\r\n\x1a\n");
            let toml = "[pe]\nicon = \"icon.ico\"\nadmin = true\nsubsystem = \"gui\"\nversion = \"1.2.3\"\nproduct = \"Example\"\n";
            let c = compiled(toml, &[("icon.ico", &ico)]).unwrap();
            assert!(c.pe.gui && c.pe.icon.as_deref() == Some(&ico[..]));
            assert!(String::from_utf8_lossy(&c.pe.manifest).contains("requireAdministrator"));
            let v = c.pe.version.unwrap();
            assert_eq!(v.numbers, [1, 2, 3, 0]);
            assert!(v.strings.contains(&("ProductName".into(), "Example".into())));
            // (a manifest of its own)
            let c = compiled("", &[("manifest.xml", b"<assembly/>")]).unwrap();
            assert_eq!(c.pe.manifest, b"<assembly/>");
        }

        #[test]
        fn checked_when_made() {
            for (toml, want) in [
                ("tilte = \"x\"", "tilte: not a setting known"),
                ("[run]\nprogram = [\"setup.exe\"]\nwiat = true", "run.wiat: not a setting known"),
                ("[run]\nprogram = [\"missing.exe\"]", "not a file of the archive"),
                ("[run]\nprogram = [\"../setup.exe\"]", "relative"),
                ("[run]\nprogram = [\"run.bat\"]", "no shell"),
                ("[run]\nprogram = [\"readme.txt\"]", "not a program"),
                ("[run]\nprogram = [\".aqr/sfx/x.exe\"]", "in .aqr/"),
                ("[run]\nprogram = []", "one at least"),
                ("[run]\nprogram = [\"setup.exe\"]\ncwd = \"nowhere\"", "not a directory"),
                ("[run]\nprogram = [\"setup.exe\"]\nwait = false\ncleanup = true", "run.cleanup with run.wait"),
                ("mode = \"run\"", "no run.program"),
                ("[run]\nprogram = [\"setup.exe\"]\n[run.env]\n\"A=B\" = \"x\"", "run.env: \"A=B\""),
                ("[run]\nprogram = [\"setup.exe\"]\n[run.env]\nA = 1", "run.env.A: not a string"),
                ("mode = \"extract\"\n[run]\nprogram = [\"setup.exe\"]", "nothing is run"),
                ("mode = \"install\"", "mode"),
                ("[extract]\ndest = \"C:/x\"", "extract.dest"),
                ("[extract]\ndest = \"{temp}/../x\"", "extract.dest"),
                ("[extract]\noverwrite = \"maybe\"", "extract.overwrite"),
                ("[extract]\nask = true", "not yet"),
                ("[ui]\nbanner = \"banner.png\"", "not yet"),
                ("[pe]\nicon = \"icon.ico\"", "not a file of .aqr/sfx/"),
                ("[pe]\nversion = \"1.2.3.4.5\"", "pe.version"),
                ("[pe]\nversion = \"1.70000\"", "pe.version"),
                ("[pe]\nsubsystem = \"tui\"", "pe.subsystem"),
                ("title = 3", "title: not a string"),
                ("run = 3", "run: not a section"),
                ("title = ", "sfx.toml"),
            ] {
                let e = compiled(toml, &[]).unwrap_err();
                assert!(e.contains(want), "{toml:?}: {e}");
            }
            let e = compiled("[pe]\nicon = \"icon.ico\"", &[("icon.ico", b"not an icon")]).unwrap_err();
            assert!(e.contains("pe.icon: icon.ico"), "{e}");
            let e = compiled("[pe]\nadmin = true", &[("manifest.xml", b"<assembly/>")]).unwrap_err();
            assert!(e.contains("manifest.xml"), "{e}");
        }
    }
}
