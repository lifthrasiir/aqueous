//! Attributes given to files by their paths (docs/hints-design.md §2): files of the syntax and
//! meaning of gitattributes(5), read in the order of their precedence ([`Attributes`]), and what
//! they say of a path ([`Attributes::resolve`]), of a file of the tree or of a member of a
//! container in it (`>`, [`Origin::Aqr`] only).
//!
//! Each line is a pattern and attributes: `PATTERN attr -attr !attr attr=value`. The patterns
//! are gitignore's: of no `/` (but a trailing one), one matches the base name at any depth;
//! else the path from the file's directory (a leading `/` taken off), `*` and `?` not matching
//! `/`, `**` any directories where it is a component of its own. A pattern ending in `/` matches
//! directories alone, so no file. A pattern may be quoted as C's strings are (`"a b.bin"`).
//! There are no negative patterns (`!`): their lines are left out, as git does. `[attr]NAME …`
//! defines a macro, where the file may ([`Attributes::add`]); `binary` is one of git's
//! (`-diff -merge -text`). Names and paths are matched as they are, case and all, on every
//! system.
//!
//! For a path, each attribute is what the line of the most precedence that tells of it says
//! (unspecified by `!attr`): of the files, those given first come first ([`Origin`]), then
//! those of deeper directories, then the later ones; of the lines of a file, the later ones;
//! within a line, the later attributes, and those of a macro where it is set, after it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where a file of attributes comes from, in the order of their precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// A file given (`--attributes`).
    Given,
    /// `.aqr/attributes` at the root of an input: its patterns may step into containers (`>`).
    Aqr,
    /// A `.gitattributes`: `>` is a character of names as any other.
    Git,
}

/// What a line says of an attribute: set (`attr`), unset (`-attr`) or a value (`attr=value`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Set,
    Unset,
    Value(String),
}

/// An attribute of a line: its name and what it says, None where it is made unspecified
/// (`!attr`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Assign {
    name: String,
    state: Option<State>,
}

/// A part of a pattern, between `>`s: matched against a path from a directory (of the file, or
/// the root of a container).
#[derive(Debug, Clone)]
struct Segment {
    /// The pattern, escapes and all (a leading `/` taken off).
    glob: Vec<u8>,
    /// Whether it is matched against the whole path, else the base name alone.
    anchored: bool,
    /// Whether it matches directories alone (a trailing `/`), so no file.
    dir_only: bool,
}

impl Segment {
    fn of(s: &[u8]) -> Segment {
        let (s, dir_only) = match s.strip_suffix(b"/") {
            Some(t) if !t.is_empty() => (t, true),
            _ => (s, false),
        };
        let anchored = s.contains(&b'/');
        let glob = s.strip_prefix(b"/").unwrap_or(s).to_vec();
        Segment { glob, anchored, dir_only }
    }

    /// Whether it matches path `p` (of `/` separators, from its directory; empty for the one
    /// content of what holds no names).
    fn matches(&self, p: &[u8]) -> bool {
        if self.glob.is_empty() || p.is_empty() {
            return self.glob.is_empty() && p.is_empty() && !self.dir_only;
        }
        if self.dir_only {
            return false;
        }
        match self.anchored {
            true => wildmatch(&self.glob, p),
            false => wildmatch(&self.glob, p.rsplit(|&b| b == b'/').next().unwrap_or(p)),
        }
    }
}

/// A line of a file: its pattern (its segments, one for each step into a container) and its
/// attributes.
#[derive(Debug)]
struct Line {
    segments: Vec<Segment>,
    assigns: Vec<Assign>,
    /// Its number in its file (from 1).
    number: usize,
    /// Whether a path was matched by it ([`Attributes::unmatched`]).
    used: AtomicBool,
}

/// A file of attributes, as [`Attributes::add`] takes it.
#[derive(Debug)]
struct File {
    origin: Origin,
    /// Its directory, as a path of the archive from the root (of `/` separators, no trailing
    /// one; empty at the root).
    base: Vec<u8>,
    /// What its values naming files are relative to ([`Attributes::data_dir`]).
    dir: Option<PathBuf>,
    /// What it is called in warnings.
    name: String,
    lines: Vec<Line>,
}

/// The files of attributes read for the inputs, in the order of their precedence, and the
/// macros they define.
#[derive(Debug, Default)]
pub struct Attributes {
    files: Vec<File>,
    /// By name: the attributes of each macro, and the file defining it (of the most
    /// precedence, as [`Attributes::add`] takes them).
    macros: HashMap<String, (Vec<Assign>, Option<usize>)>,
    /// The files by precedence, most first.
    order: Vec<usize>,
}

/// What the files say of a path ([`Attributes::resolve`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Resolved {
    /// The attributes told (not those unspecified), by name, each with the origin of the file
    /// that tells it and the file (an index of [`Attributes`], for [`Attributes::data_dir`]).
    pub attrs: Vec<(String, State, Origin, usize)>,
    /// Whether a line may match what a container at the path holds (a pattern of more steps,
    /// its first ones matching the path).
    pub descends: bool,
}

impl Resolved {
    /// What attribute `name` is, where told.
    pub fn get(&self, name: &str) -> Option<&State> {
        self.attrs.iter().find(|a| a.0 == name).map(|a| &a.1)
    }

    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty() && !self.descends
    }
}

impl Attributes {
    pub fn new() -> Attributes {
        let mut a = Attributes::default();
        let binary = ["-diff", "-merge", "-text"].iter().map(|s| parse_assign(s).unwrap()).collect();
        a.macros.insert("binary".into(), (binary, None));
        a
    }

    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|f| f.lines.is_empty())
    }

    /// Adds `text`, a file of `origin` in directory `base` (a path of the archive from its root,
    /// of `/` separators), called `name` in warnings, its values naming files relative to `dir`.
    /// Macros are defined where `macros` (git takes those of the top-level `.gitattributes` alone),
    /// the first file to define one keeping it: the files are to be added in the order of their
    /// precedence, most first, within each origin. Returns warnings of the lines left out.
    pub fn add(
        &mut self,
        origin: Origin,
        base: &[u8],
        text: &str,
        name: &str,
        dir: Option<PathBuf>,
        macros: bool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let index = self.files.len();
        let mut lines = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let number = n + 1;
            let mut warn = |w: &str| warnings.push(format!("{name}:{number}: {w}"));
            let line = line.trim_start_matches([' ', '\t', '\r']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (pattern, rest) = match pattern_of(line) {
                Ok(p) => p,
                Err(e) => {
                    warn(e);
                    continue;
                }
            };
            let mut assigns = Vec::new();
            for a in rest.split([' ', '\t', '\r']).filter(|a| !a.is_empty()) {
                match parse_assign(a) {
                    Some(a) => assigns.push(a),
                    None => warn(&format!("{a}: not an attribute")),
                }
            }
            if let Some(m) = pattern.strip_prefix(b"[attr]") {
                match std::str::from_utf8(m).ok().filter(|m| valid_name(m)) {
                    _ if !macros => warn("a macro is defined in the top-level file alone"),
                    Some(m) => {
                        self.macros.entry(m.to_owned()).or_insert((assigns, Some(index)));
                    }
                    None => warn("not the name of a macro"),
                }
                continue;
            }
            if pattern.first() == Some(&b'!') {
                warn("negative patterns are left out");
                continue;
            }
            let segments = match origin {
                Origin::Git => vec![Segment::of(&pattern)],
                _ => split_unescaped(&pattern, b'>').into_iter().map(Segment::of).collect(),
            };
            lines.push(Line { segments, assigns, number, used: AtomicBool::new(false) });
        }
        let base = base.strip_suffix(b"/").unwrap_or(base).to_vec();
        self.files.push(File { origin, base, dir, name: name.to_owned(), lines });
        // (by origin, then deeper directories, then those added later)
        let mut order: Vec<usize> = (0..self.files.len()).collect();
        let depth = |f: &File| if f.base.is_empty() { 0 } else { 1 + f.base.iter().filter(|&&b| b == b'/').count() };
        order.sort_by_key(|&i| (self.files[i].origin, std::cmp::Reverse(depth(&self.files[i])), std::cmp::Reverse(i)));
        self.order = order;
        warnings
    }

    /// What the files say of `chain`: a path of the archive (from its root, of `/` separators),
    /// then a path in what it holds for each step into a container (empty for the one content of
    /// what holds no names, as gzip's data).
    pub fn resolve(&self, chain: &[&[u8]]) -> Resolved {
        let mut out = Resolved::default();
        let Some((&path, inner)) = chain.split_first() else { return out };
        // (by name: decided, as None where made unspecified)
        let mut decided: HashMap<&str, Option<(&State, Origin, usize)>> = HashMap::new();
        for &i in &self.order {
            let file = &self.files[i];
            let Some(rel) = relative(path, &file.base) else { continue };
            for line in file.lines.iter().rev() {
                let n = chain.len();
                if line.segments.len() < n || !line.segments[0].matches(rel) {
                    continue;
                }
                if !line.segments[1..n].iter().zip(inner).all(|(s, p)| s.matches(p)) {
                    continue;
                }
                if line.segments.len() > n {
                    out.descends = true;
                    continue;
                }
                line.used.store(true, Ordering::Relaxed);
                self.fill(&line.assigns, file.origin, i, &mut decided, 0);
            }
        }
        let mut attrs: Vec<_> = decided
            .into_iter()
            .filter_map(|(name, d)| d.map(|(s, origin, i)| (name.to_owned(), s.clone(), origin, i)))
            .collect();
        attrs.sort();
        out.attrs = attrs;
        out
    }

    /// Decides the attributes of `assigns` not decided yet, the later first, and those of the
    /// macros set among them after each (as deep as macros go, at most 16).
    fn fill<'a>(
        &'a self,
        assigns: &'a [Assign],
        origin: Origin,
        file: usize,
        decided: &mut HashMap<&'a str, Option<(&'a State, Origin, usize)>>,
        depth: u32,
    ) {
        for a in assigns.iter().rev() {
            if decided.contains_key(&*a.name) {
                continue;
            }
            decided.insert(&a.name, a.state.as_ref().map(|s| (s, origin, file)));
            if a.state == Some(State::Set)
                && depth < 16
                && let Some((m, _)) = self.macros.get(&a.name)
            {
                self.fill(m, origin, file, decided, depth + 1);
            }
        }
    }

    /// The directory that the values naming files of file `i` are relative to.
    pub fn data_dir(&self, i: usize) -> Option<&std::path::Path> {
        self.files.get(i)?.dir.as_deref()
    }

    /// The lines that matched no path resolved so far, as `NAME:LINE: PATTERN` (for `--stats`).
    pub fn unmatched(&self) -> Vec<String> {
        let mut out = Vec::new();
        for f in &self.files {
            for l in f.lines.iter().filter(|l| !l.used.load(Ordering::Relaxed)) {
                let p: Vec<String> = l.segments.iter().map(|s| String::from_utf8_lossy(&s.glob).into_owned()).collect();
                out.push(format!("{}:{}: {}", f.name, l.number, p.join(">")));
            }
        }
        out
    }
}

/// `path` relative to directory `base` (both from the root), where it is within it.
fn relative<'a>(path: &'a [u8], base: &[u8]) -> Option<&'a [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    path.strip_prefix(base)?.strip_prefix(b"/")
}

/// The pattern of `line` (unquoted where quoted) and the rest of the line.
fn pattern_of(line: &str) -> Result<(Vec<u8>, &str), &'static str> {
    let b = line.as_bytes();
    if b[0] != b'"' {
        let end = b.iter().position(|&c| c == b' ' || c == b'\t' || c == b'\r').unwrap_or(b.len());
        return Ok((b[..end].to_vec(), &line[end..]));
    }
    let mut out = Vec::new();
    let mut i = 1;
    loop {
        match b.get(i) {
            None => return Err("an unterminated quoted pattern"),
            Some(b'"') => break,
            Some(b'\\') => {
                let c = *b.get(i + 1).ok_or("an unterminated quoted pattern")?;
                i += 2;
                match c {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'v' => out.push(11),
                    b'f' => out.push(12),
                    b'r' => out.push(b'\r'),
                    b'"' => out.push(b'"'),
                    // (kept escaped: the pattern's own escapes, `\*`, `\>`)
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    b'0'..=b'3' => {
                        let o = b.get(i..i + 2).filter(|o| o.iter().all(|d| (b'0'..=b'7').contains(d)));
                        let o = o.ok_or("a bad octal escape")?;
                        out.push((c - b'0') * 64 + (o[0] - b'0') * 8 + (o[1] - b'0'));
                        i += 2;
                    }
                    c => out.extend_from_slice(&[b'\\', c]),
                }
            }
            Some(&c) => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok((out, &line[i + 1..]))
}

/// Whether `name` is an attribute's name as git takes them: of `-`, `.`, `_` and ASCII letters
/// and digits, not starting with `-`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_'))
}

fn parse_assign(a: &str) -> Option<Assign> {
    let (name, state) = if let Some(n) = a.strip_prefix('-') {
        (n, Some(State::Unset))
    } else if let Some(n) = a.strip_prefix('!') {
        (n, None)
    } else if let Some((n, v)) = a.split_once('=') {
        (n, Some(State::Value(v.to_owned())))
    } else {
        (a, Some(State::Set))
    };
    valid_name(name).then(|| Assign { name: name.to_owned(), state })
}

/// `s` split at each `sep` not escaped by a backslash.
fn split_unescaped(s: &[u8], sep: u8) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let (mut from, mut i) = (0, 0);
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            c if c == sep => {
                out.push(&s[from..i]);
                i += 1;
                from = i;
            }
            _ => i += 1,
        }
    }
    out.push(&s[from.min(s.len())..]);
    out
}

/// Whether glob `p` matches all of `t`, as git's wildmatch does with `WM_PATHNAME`: `*` and `?`
/// match no `/`, `**` any directories where it is a component of its own (`**/`, `/**/`,
/// `/**`), else as `*`; `[…]` a byte of a class (`!` or `^` first for its complement, ranges,
/// `[:alpha:]` and the like), `\` escapes a byte.
pub(crate) fn wildmatch(p: &[u8], t: &[u8]) -> bool {
    matches_at(p, 0, t, 0)
}

fn matches_at(p: &[u8], mut pi: usize, t: &[u8], mut ti: usize) -> bool {
    while pi < p.len() {
        match p[pi] {
            b'*' => {
                let mut stars = pi;
                while stars < p.len() && p[stars] == b'*' {
                    stars += 1;
                }
                let double = stars - pi >= 2;
                let starts_component = pi == 0 || p[pi - 1] == b'/';
                let ends_component = stars == p.len() || p[stars] == b'/';
                if double && starts_component && ends_component {
                    if stars == p.len() {
                        // (`**` at the end: anything; `a/**` matches what is within `a` alone)
                        return pi == 0 || ti < t.len();
                    }
                    // `**/`: none or more directories
                    let rest = stars + 1;
                    let mut at = ti;
                    loop {
                        if matches_at(p, rest, t, at) {
                            return true;
                        }
                        match t[at..].iter().position(|&b| b == b'/') {
                            Some(k) => at += k + 1,
                            None => return false,
                        }
                    }
                }
                let rest = stars;
                let mut at = ti;
                loop {
                    if matches_at(p, rest, t, at) {
                        return true;
                    }
                    if at >= t.len() || t[at] == b'/' {
                        return false;
                    }
                    at += 1;
                }
            }
            b'?' => {
                if ti >= t.len() || t[ti] == b'/' {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            b'[' => {
                let Some(&c) = t.get(ti) else { return false };
                match class(p, pi + 1, c) {
                    Some((true, next)) if c != b'/' => {
                        pi = next;
                        ti += 1;
                    }
                    Some(_) => return false,
                    // (no closing `]`: a `[` as it is)
                    None if c == b'[' => {
                        pi += 1;
                        ti += 1;
                    }
                    None => return false,
                }
            }
            b'\\' if pi + 1 < p.len() => {
                if t.get(ti) != Some(&p[pi + 1]) {
                    return false;
                }
                pi += 2;
                ti += 1;
            }
            c => {
                if t.get(ti) != Some(&c) {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == t.len()
}

/// Whether byte `c` is of the class whose body starts at `p[at]` (after its `[`), and where the
/// pattern goes on after its `]`; None where it has no `]`.
fn class(p: &[u8], mut at: usize, c: u8) -> Option<(bool, usize)> {
    let negated = matches!(p.get(at), Some(b'!' | b'^'));
    if negated {
        at += 1;
    }
    let mut hit = false;
    let mut first = true;
    loop {
        let &b = p.get(at)?;
        if b == b']' && !first {
            return Some((hit != negated, at + 1));
        }
        first = false;
        if b == b'['
            && p.get(at + 1) == Some(&b':')
            && let Some(end) = p[at + 2..].windows(2).position(|w| w == b":]")
        {
            let name = &p[at + 2..at + 2 + end];
            hit |= match name {
                b"alnum" => c.is_ascii_alphanumeric(),
                b"alpha" => c.is_ascii_alphabetic(),
                b"blank" => c == b' ' || c == b'\t',
                b"cntrl" => c.is_ascii_control(),
                b"digit" => c.is_ascii_digit(),
                b"graph" => c.is_ascii_graphic(),
                b"lower" => c.is_ascii_lowercase(),
                b"print" => c.is_ascii_graphic() || c == b' ',
                b"punct" => c.is_ascii_punctuation(),
                b"space" => c.is_ascii_whitespace(),
                b"upper" => c.is_ascii_uppercase(),
                b"xdigit" => c.is_ascii_hexdigit(),
                _ => false,
            };
            at += 2 + end + 2;
            continue;
        }
        let (lo, next) = match b {
            b'\\' => (*p.get(at + 1)?, at + 2),
            b => (b, at + 1),
        };
        if p.get(next) == Some(&b'-') && p.get(next + 1).is_some_and(|&e| e != b']') {
            let (hi, after) = match p[next + 1] {
                b'\\' => (*p.get(next + 2)?, next + 3),
                e => (e, next + 2),
            };
            hit |= (lo..=hi).contains(&c);
            at = after;
        } else {
            hit |= lo == c;
            at = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildmatch_as_git() {
        let cases: &[(&str, &str, bool)] = &[
            ("*.bin", "a.bin", true),
            ("*.bin", "dir/a.bin", false),
            ("a?c", "abc", true),
            ("a?c", "a/c", false),
            ("**/a.bin", "a.bin", true),
            ("**/a.bin", "x/y/a.bin", true),
            ("x/**/a.bin", "x/a.bin", true),
            ("x/**/a.bin", "x/y/z/a.bin", true),
            ("x/**/a.bin", "xa.bin", false),
            ("x/**", "x/y/z", true),
            ("x/**", "x", false),
            ("**", "a/b", true),
            ("a**b", "axxb", true),
            ("a**b", "ax/xb", false),
            ("[abc].bin", "b.bin", true),
            ("[!abc].bin", "b.bin", false),
            ("[a-c][0-9]", "b7", true),
            ("[[:digit:]]x", "3x", true),
            ("[[:digit:]]x", "ax", false),
            ("\\*.bin", "*.bin", true),
            ("\\*.bin", "a.bin", false),
            ("a\\>b", "a>b", true),
            ("[]]", "]", true),
            ("[", "[", true),
            ("game/*.pak", "game/x.pak", true),
            ("game/*.pak", "game/sub/x.pak", false),
        ];
        for &(p, t, want) in cases {
            assert_eq!(wildmatch(p.as_bytes(), t.as_bytes()), want, "{p} on {t}");
        }
    }

    fn attrs(r: &Resolved) -> Vec<String> {
        r.attrs
            .iter()
            .map(|(n, s, _, _)| match s {
                State::Set => n.clone(),
                State::Unset => format!("-{n}"),
                State::Value(v) => format!("{n}={v}"),
            })
            .collect()
    }

    fn resolve(a: &Attributes, chain: &[&str]) -> Vec<String> {
        let chain: Vec<&[u8]> = chain.iter().map(|s| s.as_bytes()).collect();
        attrs(&a.resolve(&chain))
    }

    #[test]
    fn patterns_and_precedence() {
        let mut a = Attributes::new();
        let w = a.add(
            Origin::Aqr,
            b"",
            "# comment\n\
             *.bin aqr-format=tensors aqr-dtype=f16\n\
             sub/*.bin aqr-dtype=bf16\n\
             /top.bin -aqr-format\n\
             \"a b.bin\" aqr-kind=compressed\n\
             !neg.bin aqr-kind=text\n\
             x.bin aqr-not=zlib aqr-not=png\n\
             dir/ aqr-kind=text\n",
            "root/.aqr/attributes",
            None,
            true,
        );
        assert_eq!(w, ["root/.aqr/attributes:6: negative patterns are left out"]);
        assert_eq!(resolve(&a, &["deep/w.bin"]), ["aqr-dtype=f16", "aqr-format=tensors"]);
        assert_eq!(resolve(&a, &["sub/w.bin"]), ["aqr-dtype=bf16", "aqr-format=tensors"]);
        assert_eq!(resolve(&a, &["x/sub/w.bin"]), ["aqr-dtype=f16", "aqr-format=tensors"]);
        assert_eq!(resolve(&a, &["top.bin"]), ["aqr-dtype=f16", "-aqr-format"]);
        assert_eq!(resolve(&a, &["d/top.bin"]), ["aqr-dtype=f16", "aqr-format=tensors"]);
        assert_eq!(resolve(&a, &["a b.bin"]), ["aqr-dtype=f16", "aqr-format=tensors", "aqr-kind=compressed"]);
        // (the later of a line wins)
        assert_eq!(resolve(&a, &["x.bin"]), ["aqr-dtype=f16", "aqr-format=tensors", "aqr-not=png"]);
        assert!(resolve(&a, &["dir/f"]).is_empty());
        assert!(resolve(&a, &["neg.bin"]).len() == 2);
    }

    #[test]
    fn origins_and_directories() {
        let mut a = Attributes::new();
        a.add(Origin::Git, b"", "*.dat aqr-kind=text\n*.raw binary\n", ".gitattributes", None, true);
        a.add(Origin::Git, b"sub", "*.dat aqr-kind=table\n", "sub/.gitattributes", None, false);
        a.add(Origin::Aqr, b"", "*.dat !aqr-kind\nx.raw text\n", ".aqr/attributes", None, true);
        a.add(Origin::Given, b"", "y.dat aqr-kind=pixels\n", "given", None, true);
        // (`!` of .aqr/attributes makes it unspecified over .gitattributes)
        assert!(resolve(&a, &["a.dat"]).is_empty());
        assert_eq!(resolve(&a, &["y.dat"]), ["aqr-kind=pixels"]);
        // (the deeper .gitattributes wins; it is not reached under .aqr/attributes's `!`)
        assert!(resolve(&a, &["sub/a.dat"]).is_empty());
        let mut b = Attributes::new();
        b.add(Origin::Git, b"", "*.dat aqr-kind=text\n", ".gitattributes", None, true);
        b.add(Origin::Git, b"sub", "*.dat aqr-kind=table\n[attr]m -text\n", "sub/.gitattributes", None, false);
        assert_eq!(resolve(&b, &["sub/a.dat"]), ["aqr-kind=table"]);
        assert_eq!(resolve(&b, &["a.dat"]), ["aqr-kind=text"]);
        assert_eq!(resolve(&b, &["subx/a.dat"]), ["aqr-kind=text"]);
        // (git's binary macro)
        assert_eq!(resolve(&a, &["q.raw"]), ["binary", "-diff", "-merge", "-text"]);
        assert_eq!(resolve(&a, &["x.raw"]), ["binary", "-diff", "-merge", "text"]);
    }

    #[test]
    fn macros() {
        let mut a = Attributes::new();
        let w = a.add(
            Origin::Aqr,
            b"",
            "[attr]f16-tensors aqr-format=tensors aqr-dtype=f16\n\
             [attr]-bad x\n\
             *_f16.bin f16-tensors\n\
             a_f16.bin -f16-tensors\n",
            "a",
            None,
            true,
        );
        assert_eq!(w, ["a:2: not the name of a macro"]);
        assert_eq!(resolve(&a, &["m/x_f16.bin"]), ["aqr-dtype=f16", "aqr-format=tensors", "f16-tensors"]);
        // (a macro unset is unset alone, as git has it)
        assert_eq!(resolve(&a, &["a_f16.bin"]), ["-f16-tensors"]);
        let mut b = Attributes::new();
        let w = b.add(Origin::Git, b"sub", "[attr]m x\n", "sub/.gitattributes", None, false);
        assert_eq!(w, ["sub/.gitattributes:1: a macro is defined in the top-level file alone"]);
    }

    #[test]
    fn containers() {
        let mut a = Attributes::new();
        a.add(
            Origin::Aqr,
            b"",
            "loupe.zip>*.bin aqr-format=tensors\n\
             game/*.pak>sounds/** aqr-kind=compressed\n\
             backup.tar.zst> aqr-kind=table\n\
             backup.tar.zst>>db/*.dat aqr-format=sqlite\n\
             *.bin aqr-kind=compressed\n\
             a\\>b.bin aqr-kind=text\n",
            ".aqr/attributes",
            None,
            true,
        );
        a.add(Origin::Git, b"", "loupe.zip>*.bin aqr-not=x\nnone.* aqr-not=y\n", ".gitattributes", None, true);
        let r = a.resolve(&[&b"loupe.zip"[..]]);
        assert!(r.attrs.is_empty() && r.descends);
        assert_eq!(resolve(&a, &["loupe.zip", "w/x.bin"]), ["aqr-format=tensors"]);
        // (a pattern of no `>` matches the files of the tree alone)
        assert_eq!(resolve(&a, &["x.bin"]), ["aqr-kind=compressed"]);
        assert!(!a.resolve(&[&b"x.bin"[..]]).descends);
        assert_eq!(resolve(&a, &["game/a.pak", "sounds/x/y.wav"]), ["aqr-kind=compressed"]);
        assert!(resolve(&a, &["game/a.pak", "music/y.wav"]).is_empty());
        assert_eq!(resolve(&a, &["backup.tar.zst", ""]), ["aqr-kind=table"]);
        assert!(a.resolve(&[&b"backup.tar.zst"[..], b""]).descends);
        assert_eq!(resolve(&a, &["backup.tar.zst", "", "db/x.dat"]), ["aqr-format=sqlite"]);
        assert!(resolve(&a, &["backup.tar.zst", "x"]).is_empty());
        assert_eq!(resolve(&a, &["a>b.bin"]), ["aqr-kind=text"]);
        // (in .gitattributes `>` is a character of names)
        assert_eq!(resolve(&a, &["loupe.zip>x.bin"]), ["aqr-kind=compressed", "aqr-not=x"]);
        let mut u = a.unmatched();
        u.sort();
        assert_eq!(u, [".gitattributes:2: none.*"]);
    }

    #[test]
    fn quoting() {
        assert_eq!(pattern_of(r#""a\tb\101\"" x"#).unwrap(), (b"a\tbA\"".to_vec(), " x"));
        assert_eq!(pattern_of(r#""a\*b" x"#).unwrap().0, b"a\\*b");
        assert!(pattern_of(r#""abc"#).is_err());
        assert_eq!(split_unescaped(b"a>b\\>c>", b'>'), [&b"a"[..], b"b\\>c", b""]);
    }
}
