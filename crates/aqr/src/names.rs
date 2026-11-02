//! Names of entries as this system writes them (`docs/names-design.md`): names of no declared
//! encoding (`raw`) decoded or kept as bytes (`-E`), Unicode names normalized (`--normalize`),
//! and what this system cannot write replaced or failed (`--bad-names`).
//!
//! Each name is mapped alone, the same each time ([`Names::name`]): the extractor takes names
//! as one, resolves links and makes files by the names mapped, so that a link reaches the name
//! its target's is mapped to. Of an archive, how its names of no declared encoding are decoded
//! is decided once, by those of the entries first told ([`Names::decide`]).

use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

use aqr_foreign::charset::{self, Charset, Choice, Decision};
use aqr_formula::aqr::{EntryNode, Tree, name_bytes};
use unicode_normalization::UnicodeNormalization;

/// How Unicode names are normalized (`--normalize`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Normalize {
    /// Composed ([`compose`]) but on macOS, where they are kept.
    #[default]
    Auto,
    Nfc,
    Nfd,
    Keep,
}

/// What is done with a name this system cannot write as it is (`--bad-names`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Bad {
    /// Kept where names of bytes are taken, else replaced.
    #[default]
    Auto,
    Keep,
    Replace,
    Fail,
}

/// What is asked of the names of archives extracted.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Opts {
    pub encoding: Choice,
    pub normalize: Normalize,
    pub bad: Bad,
}

impl Opts {
    pub fn normalize(s: &str) -> Result<Normalize, String> {
        Ok(match s {
            "auto" => Normalize::Auto,
            "nfc" => Normalize::Nfc,
            "nfd" => Normalize::Nfd,
            "keep" => Normalize::Keep,
            _ => return Err(format!("--normalize takes auto, nfc, nfd or keep: {s}")),
        })
    }

    pub fn bad(s: &str) -> Result<Bad, String> {
        Ok(match s {
            "auto" => Bad::Auto,
            "keep" => Bad::Keep,
            "replace" => Bad::Replace,
            "fail" => Bad::Fail,
            _ => return Err(format!("--bad-names takes auto, keep, replace or fail: {s}")),
        })
    }
}

/// A name mapped: as this system writes it, and what was done.
#[derive(Clone, Debug, Default)]
pub struct Mapped {
    pub os: OsString,
    /// Of no declared encoding, decoded.
    pub decoded: bool,
    /// Normalized otherwise than it was.
    pub normalized: bool,
    /// Not written as it is: bytes, a lone surrogate, characters this system does not take,
    /// replaced.
    pub replaced: bool,
}

/// The names of an archive as this system writes them ([module](self)).
pub struct Names {
    opts: Opts,
    /// Whether names of bytes (not Unicode) are written as they are: where this system takes
    /// them and they are not to be replaced.
    bytes: bool,
    decision: OnceLock<Decision>,
    /// The names decoded, and normalized (of the entries made).
    counts: Mutex<(usize, usize)>,
}

impl Names {
    /// The names as `opts` asks, written into a file system of names taken as they are (as
    /// Linux's mostly are, which take names of bytes) where `exact`.
    pub fn new(opts: &Opts, exact: bool) -> Names {
        let takes = cfg!(unix) && !cfg!(target_os = "macos") && exact;
        let bytes = takes && matches!(opts.bad, Bad::Auto | Bad::Keep);
        Names { opts: opts.clone(), bytes, decision: OnceLock::new(), counts: Mutex::default() }
    }

    /// Decides how the names of no declared encoding are decoded, by those of `t` (the entries
    /// told first, and their links' targets): where they can be kept as bytes, they are with
    /// `-E auto`; else they are decoded, of the set guessed where not given.
    pub fn decide(&self, t: &Tree) {
        self.decision.get_or_init(|| {
            let mut raw: Vec<&[u8]> = Vec::new();
            for e in &t.entries {
                if let (b, true) = name_bytes(&e.name) {
                    raw.push(b);
                }
                if let EntryNode::Link(target) = &e.node
                    && let (b, true) = name_bytes(target)
                {
                    raw.extend(b.split(|&c| c == b'/').filter(|n| !n.is_empty()));
                }
            }
            if raw.is_empty() {
                return Decision::default();
            }
            charset::decide(&self.opts.encoding, &raw, !self.bytes, Charset::system(false), Charset::likely())
        });
    }

    fn decision(&self) -> &Decision {
        self.decision.get_or_init(Decision::default)
    }

    /// Entry name `name` (encoded) mapped.
    pub fn name(&self, name: &[u8]) -> Mapped {
        let (b, raw) = name_bytes(name);
        self.part(b, raw)
    }

    /// Name `b` (of no declared encoding where `raw`, else WTF-8) mapped.
    pub fn part(&self, b: &[u8], raw: bool) -> Mapped {
        let mut m = Mapped::default();
        let text = match raw {
            false => Some(b.to_vec()),
            true => self.decision().decode(b).map(String::into_bytes),
        };
        let os = match text {
            None if self.bytes => bytes_os(b),
            None => {
                m.replaced = true;
                OsString::from(escaped(b))
            }
            Some(t) => {
                m.decoded = raw;
                let n = normalized(&t, self.form());
                m.normalized = n != t;
                self.unicode(&n, &mut m)
            }
        };
        #[cfg(windows)]
        if let Some(sub) = crate::extract::windows_name(&os) {
            m.replaced = true;
            m.os = sub;
            return m;
        }
        m.os = os;
        m
    }

    /// The form names are normalized to (`None`: kept; `Some(false)`: composed alone).
    fn form(&self) -> Option<Form> {
        match self.opts.normalize {
            Normalize::Auto if cfg!(target_os = "macos") => None,
            Normalize::Auto => Some(Form::Composed),
            Normalize::Nfc => Some(Form::Nfc),
            Normalize::Nfd => Some(Form::Nfd),
            Normalize::Keep => None,
        }
    }

    /// WTF-8 name `w` as this system writes it: on Windows as it is (lone surrogates and all),
    /// elsewhere as its bytes, its lone surrogates U+FFFD but where names of bytes are kept.
    fn unicode(&self, w: &[u8], m: &mut Mapped) -> OsString {
        if cfg!(windows) || self.bytes || std::str::from_utf8(w).is_ok() {
            return crate::meta::os_from(w);
        }
        m.replaced = true;
        let s: String = units(w).into_iter().map(|u| u.unwrap_or('\u{fffd}')).collect();
        OsString::from(s)
    }

    /// The target of link `target` (encoded) as this system writes it: each of its names mapped
    /// (on Windows, `\` between them); and whether a name was replaced.
    pub fn link(&self, target: &[u8]) -> (OsString, bool) {
        let (b, raw) = name_bytes(target);
        let (mut out, mut replaced) = (OsString::new(), false);
        for (k, part) in b.split(|&c| c == b'/').enumerate() {
            if k > 0 {
                out.push(if cfg!(windows) { "\\" } else { "/" });
            }
            match part {
                b"" | b"." | b".." => out.push(crate::meta::os_from(part)),
                _ => {
                    let m = self.part(part, raw);
                    replaced |= m.replaced;
                    out.push(m.os);
                }
            }
        }
        (out, replaced)
    }

    /// Whether `m` (of an entry made) fails the extraction (`--bad-names fail`).
    pub fn fails(&self, m: &Mapped) -> bool {
        self.opts.bad == Bad::Fail && m.replaced
    }

    /// Counts `m`, of an entry made.
    pub fn count(&self, m: &Mapped) {
        let mut c = self.counts.lock().unwrap();
        c.0 += m.decoded as usize;
        c.1 += m.normalized as usize;
    }

    /// What is told of the names of the entries made: of no declared encoding, decoded as
    /// guessed; normalized.
    pub fn told(&self) -> Vec<String> {
        let (decoded, normalized) = *self.counts.lock().unwrap();
        let mut out = Vec::new();
        if decoded > 0
            && let Some(t) = self.decision().told(decoded)
        {
            out.push(t);
        }
        if normalized > 0 {
            let how = match self.form() {
                Some(Form::Nfd) => "decomposed (NFD)",
                _ => "composed (NFC)",
            };
            let names = if normalized == 1 { "name" } else { "names" };
            out.push(format!("{normalized} {names} {how}"));
        }
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// NFC, but of the characters that NFC changes alone (singletons as CJK compatibility
    /// ideographs and the sign of ohm, and those excluded from composition) kept.
    Composed,
    Nfc,
    Nfd,
}

/// Bytes `b` as a name of this system (on Unix, as they are).
fn bytes_os(b: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b).to_owned()
    }
    #[cfg(not(unix))]
    OsString::from(escaped(b))
}

/// Bytes `b` as text: what is UTF-8 as it is, other bytes as `%XX` (as macOS writes names of HFS
/// that are not UTF-8), and so `%` itself, and `\\` (on Windows a separator; here likely a byte
/// of a character not decoded, as 5C of Shift_JIS's): one name, to be taken back as it was.
pub fn escaped(b: &[u8]) -> String {
    let mut out = String::new();
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '%' | '\\' => out.push_str(&format!("%{:02X}", c as u8)),
                c => out.push(c),
            }
        }
        for c in chunk.invalid() {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// The characters of WTF-8 `w`, a lone surrogate `None`.
fn units(w: &[u8]) -> Vec<Option<char>> {
    let mut out = Vec::new();
    for chunk in w.utf8_chunks() {
        out.extend(chunk.valid().chars().map(Some));
        // (in WTF-8, what is not UTF-8 is lone surrogates: ED A0..BF xx, each 3 bytes, which
        // Rust takes as one byte at a time)
        let n = chunk.invalid().len();
        if n > 0 && chunk.invalid()[0] == 0xed {
            out.push(None);
        }
    }
    out
}

/// WTF-8 name `w` normalized to `form` (`None`: as it is), its lone surrogates as they are.
fn normalized(w: &[u8], form: Option<Form>) -> Vec<u8> {
    let Some(form) = form else { return w.to_vec() };
    let mut out = Vec::with_capacity(w.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u8>| {
        let s: String = match form {
            Form::Composed => compose(run),
            Form::Nfc => run.nfc().collect(),
            Form::Nfd => run.nfd().collect(),
        };
        out.extend_from_slice(s.as_bytes());
        run.clear();
    };
    let mut rest = w;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                run.push_str(s);
                rest = &[];
            }
            Err(e) => {
                let ok = e.valid_up_to();
                run.push_str(std::str::from_utf8(&rest[..ok]).unwrap());
                flush(&mut run, &mut out);
                // (a lone surrogate, 3 bytes)
                let n = 3.min(rest.len() - ok);
                out.extend_from_slice(&rest[ok..ok + n]);
                rest = &rest[ok + n..];
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// `s` composed: NFC, but a character that NFC changes alone kept as it is (between runs of
/// the others, each composed).
pub fn compose(s: &str) -> String {
    if unicode_normalization::is_nfc_quick(s.chars()) == unicode_normalization::IsNormalized::Yes {
        return s.to_owned();
    }
    let (mut out, mut run) = (String::with_capacity(s.len()), String::new());
    for c in s.chars() {
        let mut alone = [0u8; 4];
        let alone = &*c.encode_utf8(&mut alone);
        if alone.nfc().eq(alone.chars()) {
            run.push(c);
        } else {
            out.extend(run.nfc());
            run.clear();
            out.push(c);
        }
    }
    out.extend(run.nfc());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed() {
        assert_eq!(compose("\u{1100}\u{1161}"), "가");
        assert_eq!(compose("e\u{301}"), "é");
        assert_eq!(compose("\u{f900}e\u{301}"), "\u{f900}é");
        assert_eq!(compose("\u{212b}"), "\u{212b}");
        assert_eq!(compose("plain"), "plain");
    }

    #[test]
    fn surrogates_kept_apart() {
        let w = b"e\xcc\x81\xed\xa0\x80e\xcc\x81";
        assert_eq!(normalized(w, Some(Form::Composed)), b"\xc3\xa9\xed\xa0\x80\xc3\xa9");
        assert_eq!(units(b"a\xed\xa0\x80b"), [Some('a'), None, Some('b')]);
        assert_eq!(escaped(b"a\xb0\xa1b"), "a%B0%A1b");
        // (5C, of a character of Shift_JIS not decoded, and `%` escaped too: one name, taken
        // back as it was)
        assert_eq!(escaped(b"\x95\\\x8e\xa6%.txt"), "%95%5C%8E%A6%25.txt");
    }
}
