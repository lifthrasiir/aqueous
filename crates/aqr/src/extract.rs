//! Extracting a tree of entries into a directory ([`Extractor`]), whether the tree is an
//! archive's own or one read of an archive of another format (aqr-unpack): the entries told as
//! they are known, a batch at a time, and the contents of their files as they are decoded.
//!
//! The directories, the files (empty, or hard links to the first of their link group), FIFOs and
//! devices are made as their entries are told; the contents of the files and their forks are
//! written as they are decoded (on any thread); then, once all is told ([`Extractor::finish`]),
//! the links (but those reaching out of the directory), the metadata of all but the
//! directories, and that of the directories, the deepest first (spec, "Extraction").
//!
//! Where asked ([`Extractor::new`]'s `fold`, for archives of other formats), a tree of a single
//! directory at its top, of the name of the directory extracted into (case aside on Windows and
//! macOS), is extracted into that directory, not into a directory of the same name in it: what
//! is in it at the top, and its metadata the directory's (as 7-Zip's `-spe` eliminates the
//! duplication of the root folder). As a tree read as it is decoded is not known whole, that is
//! decided by the entries first told; where another entry at the top comes later, what was made
//! is moved into a directory of the first one's name.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, RwLock};

use aqr_formula::aqr::{EntryNode, Extra, Tree, name_bytes, name_from};

use crate::meta::{self, Kind, os_from};
use crate::{create, stats};

/// Extracts the entries told into a directory ([module](self)).
pub struct Extractor<'a> {
    out: &'a Path,
    force: bool,
    ap: &'a meta::Applier,
    seen: &'a stats::Seen,
    s: RwLock<State>,
    /// The forks of links, written once the links are: the entry, the fork's index, the data.
    later: Mutex<Vec<(usize, usize, Vec<u8>)>>,
    /// By content: its bytes, once written.
    written: Mutex<Vec<Option<u64>>>,
    /// The files written and their bytes.
    files: AtomicUsize,
    bytes: AtomicU64,
}

#[derive(Default)]
struct State {
    /// By entry: its path, and what it was made as (its node's kind) where it was.
    paths: Vec<PathBuf>,
    made: Vec<Option<Kind>>,
    /// By entry: the file it is a hard link to (the first of its link group).
    linked: Vec<Option<usize>>,
    /// By entry: the content of the file it was told as.
    content: Vec<Option<usize>>,
    /// By entry: of a file made as its content is written (on the thread writing it, which
    /// applies its metadata then too: one file opened once or twice, and not on one thread),
    /// its metadata; and whether that was done. Files of forks (written as their own contents
    /// are) and of link groups (others linked to them as they are told) are made as they are
    /// told instead, and their metadata applied at the end.
    soon: Vec<Option<Extra>>,
    applied: Vec<AtomicBool>,
    /// By content: the files it is written to (not hard links), and how many files are of it.
    files: Vec<(Vec<usize>, usize)>,
    /// By content: the forks of it.
    forks: Vec<Vec<Fork>>,
    /// By link group: its files.
    groups: HashMap<i128, Vec<usize>>,
    /// Whether the root's forks are known.
    root: bool,
    /// Of an extractor folding: not decided yet (`None`), the directory extracted as the root
    /// (`Some(Some(i))`), or none (`Some(None)`).
    fold: Option<Option<usize>>,
}

/// A fork of a content: the entry (`None`: the root), the fork's index and name, and the kind
/// of the entry.
type Fork = (Option<usize>, usize, Vec<u8>, Kind);

/// What `node` is made as.
fn kind_of(node: &EntryNode) -> Kind {
    match node {
        EntryNode::Dir => Kind::Dir,
        EntryNode::File(_) => Kind::File,
        EntryNode::Link(_) => Kind::Link,
        _ => Kind::Special,
    }
}

fn err(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

impl<'a> Extractor<'a> {
    /// An extractor into `out`, made here (or there already, where `force`); folding a tree of a
    /// single directory at its top where `fold` ([module](self)).
    pub fn new(
        out: &'a Path,
        force: bool,
        fold: bool,
        ap: &'a meta::Applier,
        seen: &'a stats::Seen,
    ) -> Result<Self, String> {
        match fs::create_dir(out) {
            Err(e) if !(force && e.kind() == std::io::ErrorKind::AlreadyExists && out.is_dir()) => {
                return Err(err(out, e));
            }
            _ => {}
        }
        Ok(Extractor {
            out,
            force,
            ap,
            seen,
            s: RwLock::new(State { fold: (!fold).then_some(None), ..State::default() }),
            later: Mutex::default(),
            written: Mutex::default(),
            files: AtomicUsize::new(0),
            bytes: AtomicU64::new(0),
        })
    }

    /// Makes entries `new` of `t` (new ones, or ones told before and changed since: made again),
    /// each after its parent. Not to be called while a content is written.
    pub fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String> {
        let mut s = self.s.write().unwrap();
        let s = &mut *s;
        let n = t.entries.len();
        s.paths.resize(n, PathBuf::new());
        s.made.resize(n, None);
        s.linked.resize(n, None);
        s.content.resize(n, None);
        s.soon.resize(n, None);
        s.applied.resize_with(n, AtomicBool::default);
        if !s.root {
            s.root = true;
            for (k, f) in t.root.forks.iter().enumerate() {
                grow(&mut s.forks, f.1).push((None, k, f.0.clone(), Kind::Dir));
            }
        }
        self.fold(s, t, new)?;
        // the link groups of the files told, consistent where all their files are of a content
        let mut touched = Vec::new();
        for &i in new {
            let e = &t.entries[i];
            if let (Some(g), EntryNode::File(_)) = (e.extra.link_group, &e.node) {
                let files = s.groups.entry(g).or_default();
                if !files.contains(&i) {
                    files.push(i);
                }
                touched.push(i);
            }
        }
        for i in touched {
            let files = &s.groups[&t.entries[i].extra.link_group.unwrap()];
            let first = files[0];
            if first != i && files.iter().all(|&j| t.entries[j].node == t.entries[first].node) {
                s.linked[i] = Some(first);
            }
        }
        for &i in new {
            let e = &t.entries[i];
            let kind = kind_of(&e.node);
            if s.paths[i].as_os_str().is_empty() {
                let parent = e.parent.map_or(self.out, |p| &s.paths[p]);
                s.paths[i] = parent.join(self.fs_name(parent, &e.name));
            }
            let path = s.paths[i].clone();
            // (the directory folded: the root, made already)
            if s.fold == Some(Some(i)) {
                s.made[i] = Some(Kind::Dir);
                continue;
            }
            // told again: a file of the content it had kept as it is (now in a link group)
            if let (Some(Kind::File), EntryNode::File(c)) = (s.made[i], &e.node)
                && s.content[i] == Some(*c)
            {
                continue;
            }
            // else what was made of it undone (a directory kept as it is)
            if let Some(was) = s.made[i].take() {
                if let Some(c) = s.content[i].take() {
                    let f = &mut s.files[c];
                    f.0.retain(|&j| j != i);
                    f.1 -= 1;
                }
                s.forks.iter_mut().for_each(|f| f.retain(|g| g.0 != Some(i)));
                match (was, kind) {
                    (Kind::Dir, Kind::Dir) => {
                        s.made[i] = Some(Kind::Dir);
                        continue;
                    }
                    (Kind::Dir, _) | (_, Kind::Dir) => {
                        return Err(format!("{}: a directory and a file of one name", path.display()));
                    }
                    // (a file made as its content is written, not written yet: not there)
                    _ => match fs::remove_file(&path) {
                        Err(x) if !(x.kind() == std::io::ErrorKind::NotFound && s.soon[i].is_some()) => {
                            return Err(err(&path, x));
                        }
                        _ => {}
                    },
                }
                s.soon[i] = None;
                *s.applied[i].get_mut() = false;
            }
            match &e.node {
                EntryNode::Dir => match fs::create_dir(&path) {
                    Err(x) if !(self.force && path.is_dir() && !path.is_symlink()) => return Err(err(&path, x)),
                    _ => {}
                },
                EntryNode::File(c) => {
                    grow(&mut s.files, *c).1 += 1;
                    s.content[i] = Some(*c);
                    match s.linked[i] {
                        Some(j) => {
                            if self.force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
                                fs::remove_file(&path).map_err(|x| err(&path, x))?;
                            }
                            fs::hard_link(&s.paths[j], &path).map_err(|x| err(&path, x))?;
                            // (of a content written already: counted now)
                            if let Some(&Some(n)) = self.written.lock().unwrap().get(*c) {
                                self.files.fetch_add(1, Relaxed);
                                self.bytes.fetch_add(n, Relaxed);
                            }
                        }
                        None => {
                            if e.extra.link_group.is_none() && e.extra.forks.is_empty() {
                                s.soon[i] = Some(e.extra.clone());
                            } else {
                                create(&path, self.force)?;
                            }
                            s.files[*c].0.push(i);
                        }
                    }
                }
                // (made once all else is)
                EntryNode::Link(_) => {}
                EntryNode::Fifo | EntryNode::Device { .. } => {
                    if self.force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
                        fs::remove_file(&path).map_err(|x| err(&path, x))?;
                    }
                    if !self.ap.special(&path, &e.node)? {
                        continue;
                    }
                }
                EntryNode::Special(k) => {
                    self.ap.tell(&path, &format!("a special entry (kind {k}), not created"));
                    continue;
                }
            }
            if s.linked[i].is_none() {
                for (k, f) in e.extra.forks.iter().enumerate() {
                    grow(&mut s.forks, f.1).push((Some(i), k, f.0.clone(), kind));
                }
            }
            // (a link made once all else is)
            s.made[i] = (kind != Kind::Link).then_some(kind);
        }
        Ok(())
    }

    /// Decides whether to fold ([module](self)) as entries are first told, and undoes it where
    /// an entry of `new` is at the top beside the directory folded: the directory made, what is in
    /// it moved there.
    fn fold(&self, s: &mut State, t: &Tree, new: &[usize]) -> Result<(), String> {
        let top = |i: &&usize| t.entries[**i].parent.is_none();
        match s.fold {
            None if !new.is_empty() => {
                let mut tops = new.iter().filter(top);
                let first = tops.next().copied();
                s.fold = Some(match (first, tops.next()) {
                    (Some(i), None) if t.entries[i].node == EntryNode::Dir && self.out_named(&t.entries[i].name) => {
                        s.paths[i] = self.out.to_path_buf();
                        Some(i)
                    }
                    _ => None,
                });
                Ok(())
            }
            Some(Some(i)) if new.iter().filter(top).any(|&j| j != i) => {
                let dir = self.out.join(self.fs_name(self.out, &t.entries[i].name));
                fs::create_dir(&dir).map_err(|x| err(&dir, x))?;
                let moved: Vec<usize> =
                    (0..t.entries.len()).filter(|&k| t.entries[k].parent == Some(i) && s.made[k].is_some()).collect();
                for k in moved {
                    let to = dir.join(s.paths[k].file_name().unwrap());
                    fs::rename(&s.paths[k], &to).map_err(|x| err(&to, x))?;
                }
                // (every path so far is of the directory folded: under the root)
                for p in s.paths.iter_mut().filter(|p| !p.as_os_str().is_empty()) {
                    *p = dir.join(p.strip_prefix(self.out).unwrap());
                }
                s.paths[i] = dir;
                s.fold = Some(None);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Whether entry name `name` is the name of the directory extracted into, as this system
    /// takes names (on Windows and macOS, case aside).
    fn out_named(&self, name: &[u8]) -> bool {
        let Some(out) = self.out.file_name() else { return false };
        let name = os_from(name_bytes(name).0);
        if cfg!(any(windows, target_os = "macos")) {
            let fold = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
            fold(&name) == fold(out)
        } else {
            name == out
        }
    }

    /// The name of `name` (an entry's) on this system, in directory `dir`. On Windows, the
    /// characters it cannot have (and a dot or a space at the end) are `_`, and a device's name
    /// takes `_` before it: told where that is so (spec, "Extraction").
    fn fs_name(&self, dir: &Path, name: &[u8]) -> OsString {
        let (bytes, _) = name_bytes(name);
        let os = os_from(bytes);
        #[cfg(windows)]
        if let Some(sub) = windows_name(&os) {
            self.ap.tell(
                &dir.join(&os),
                &format!("not a name this system takes, written as {}", Path::new(&sub).display()),
            );
            return sub;
        }
        let _ = dir;
        os
    }

    /// Writes content `c` of the entries told, `d`: to its files and forks.
    pub fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        self.seen.saw(c, &d, d.len() as u64);
        let _s = aqr_common::trace::span("write");
        let s = self.s.read().unwrap();
        let (to, count) = s.files.get(c).map_or((&[][..], 0), |f| (&f.0[..], f.1));
        for &i in to {
            let p = &s.paths[i];
            match &s.soon[i] {
                Some(x) => {
                    let mut f = create(p, self.force)?;
                    f.write_all(&d).map_err(|e| err(p, e))?;
                    self.ap.apply_file(&f, p, x)?;
                    drop(f);
                    s.applied[i].store(true, Relaxed);
                }
                // (the file made empty already)
                None => {
                    let mut f = fs::OpenOptions::new().write(true).open(p).map_err(|e| err(p, e))?;
                    f.write_all(&d).map_err(|e| err(p, e))?;
                }
            }
        }
        for (i, k, name, kind) in s.forks.get(c).map_or(&[][..], |f| f) {
            match (kind, *i) {
                (Kind::Link, Some(i)) => self.later.lock().unwrap().push((i, *k, d.clone())),
                (_, None) => self.ap.fork(self.out, *kind, name, &d),
                (_, Some(i)) if s.made[i].is_some() => self.ap.fork(&s.paths[i], *kind, name, &d),
                _ => {}
            }
        }
        self.files.fetch_add(count, Relaxed);
        self.bytes.fetch_add(count as u64 * d.len() as u64, Relaxed);
        *grow(&mut self.written.lock().unwrap(), c) = Some(d.len() as u64);
        Ok(())
    }

    /// Makes the links of `t` (the whole tree, all of it told) and applies the metadata of what
    /// was made. Returns the number of files written and their bytes.
    pub fn finish(self, t: &Tree) -> Result<(usize, u64), String> {
        let mut s = self.s.into_inner().unwrap();
        let by_name: HashMap<(Option<usize>, &[u8]), usize> =
            t.entries.iter().enumerate().map(|(i, e)| ((e.parent, &e.name[..]), i)).collect();
        for (i, e) in t.entries.iter().enumerate() {
            let EntryNode::Link(target) = &e.node else { continue };
            if s.paths.get(i).is_none_or(|p| p.as_os_str().is_empty()) {
                continue;
            }
            let path = &s.paths[i];
            // (folded: reaching out of the directory folded is reaching out)
            let within = s.fold.flatten().map(|f| &t.entries[f].name[..]);
            let Some(to) = resolve_link(t, &by_name, i, within) else {
                self.ap.tell(path, &format!("a link reaching out of {}, not created", self.out.display()));
                continue;
            };
            if self.force && fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
                fs::remove_file(path).map_err(|x| err(path, x))?;
            }
            // (on Windows, of the kind recorded, else by what it leads to; spec, "Extraction")
            let dir = match e.extra.win_link_type {
                Some(k) => k != 0,
                None => to.is_some_and(|j| t.entries[j].node == EntryNode::Dir),
            };
            symlink(&os_from(name_bytes(target).0), path, dir).map_err(|x| err(path, x))?;
            s.made[i] = Some(Kind::Link);
        }
        for (i, k, d) in self.later.into_inner().unwrap() {
            if s.made[i].is_some() {
                self.ap.fork(&s.paths[i], Kind::Link, &t.entries[i].extra.forks[k].0, &d);
            }
        }
        // the metadata of the directories once nothing more is written in them, the deepest first
        for (i, e) in t.entries.iter().enumerate() {
            if let Some(k) = s.made[i].filter(|&k| k != Kind::Dir)
                && s.linked[i].is_none()
                && !*s.applied[i].get_mut()
            {
                self.ap.apply(&s.paths[i], k, &e.extra)?;
            }
        }
        for (i, e) in t.entries.iter().enumerate().rev() {
            if s.made[i] == Some(Kind::Dir) {
                self.ap.apply(&s.paths[i], Kind::Dir, &e.extra)?;
            }
        }
        self.ap.apply(self.out, Kind::Dir, &t.root)?;
        Ok((self.files.into_inner(), self.bytes.into_inner()))
    }
}

/// `v[i]`, `v` grown to have it.
fn grow<T: Default>(v: &mut Vec<T>, i: usize) -> &mut T {
    if v.len() <= i {
        v.resize_with(i + 1, T::default);
    }
    &mut v[i]
}

/// The name `name` takes on Windows where it cannot be as it is: the characters a name cannot
/// have (`<>:"/\|?*` and controls) and a dot or a space at its end as `_`, and `_` before a
/// device's name (`CON`, `NUL.txt`, ...). `None` where it can be.
#[cfg(any(windows, test))]
pub fn windows_name(name: &std::ffi::OsStr) -> Option<OsString> {
    let s = name.to_string_lossy();
    // (a lone surrogate is a name Windows takes: kept as it is)
    if s.contains('\u{fffd}') && name.to_str().is_none() {
        return None;
    }
    let bad = |c: char| (c as u32) < 0x20 || "<>:\"/\\|?*".contains(c);
    let mut out: String = s.chars().map(|c| if bad(c) { '_' } else { c }).collect();
    if out.ends_with(['.', ' ']) {
        out.pop();
        out.push('_');
    }
    let stem = out.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let device = matches!(&stem[..], "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9')
        || (stem.starts_with("COM") || stem.starts_with("LPT")) && ["¹", "²", "³"].contains(&&stem[3..]);
    if device {
        out.insert(0, '_');
    }
    (out != s).then(|| out.into())
}

/// Where link `i` of `t` leads, resolved from the link's directory through the links of the
/// tree (`by_name`: the entries by their directory and name): the entry (`None`: none of the
/// tree); `None` where it reaches out of the directory it is extracted into, by an absolute
/// target or by one going above it (or out of the directory at the top named `within`, where
/// given: the one extracted as the root).
fn resolve_link(
    t: &Tree,
    by_name: &HashMap<(Option<usize>, &[u8]), usize>,
    i: usize,
    within: Option<&[u8]>,
) -> Option<Option<usize>> {
    // the path of an entry, as names from the root
    let path_of = |mut e: Option<usize>| {
        let mut p = Vec::new();
        while let Some(i) = e {
            p.push(t.entries[i].name.clone());
            e = t.entries[i].parent;
        }
        p.reverse();
        p
    };
    let find = |path: &[Vec<u8>]| -> Option<usize> {
        let mut at = None;
        for name in path {
            at = Some(*by_name.get(&(at, &name[..]))?);
        }
        at
    };
    // (the tree's own links followed, as many as there are entries at most)
    let mut budget = t.entries.len() + 1;
    fn resolve(
        t: &Tree,
        dir: Vec<Vec<u8>>,
        target: &[u8],
        budget: &mut usize,
        find: &dyn Fn(&[Vec<u8>]) -> Option<usize>,
        path_of: &dyn Fn(Option<usize>) -> Vec<Vec<u8>>,
    ) -> Option<Vec<Vec<u8>>> {
        let (bytes, _) = name_bytes(target);
        if bytes.starts_with(b"/") || (cfg!(windows) && bytes.get(1) == Some(&b':')) {
            return None;
        }
        let mut at = dir;
        for part in bytes.split(|&b| b == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    at.pop()?;
                }
                name => {
                    at.push(name_from(name));
                    if let Some(j) = find(&at)
                        && let EntryNode::Link(next) = &t.entries[j].node
                    {
                        *budget = budget.checked_sub(1)?;
                        let parent = path_of(t.entries[j].parent);
                        at = resolve(t, parent, next, budget, find, path_of)?;
                    }
                }
            }
        }
        Some(at)
    }
    let EntryNode::Link(target) = &t.entries[i].node else { return Some(None) };
    let p = resolve(t, path_of(t.entries[i].parent), target, &mut budget, &find, &path_of)?;
    if within.is_some_and(|w| p.first().is_none_or(|n| n[..] != *w)) {
        return None;
    }
    Some(find(&p))
}

/// Makes a link to `target` at `at` (on Windows, to a directory where `dir`).
#[cfg(unix)]
fn symlink(target: &std::ffi::OsStr, at: &Path, _: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn symlink(target: &std::ffi::OsStr, at: &Path, dir: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let slash = |c: u16| if c == b'/' as u16 { b'\\' as u16 } else { c };
    let target = PathBuf::from(OsString::from_wide(&target.encode_wide().map(slash).collect::<Vec<_>>()));
    if dir { std::os::windows::fs::symlink_dir(target, at) } else { std::os::windows::fs::symlink_file(target, at) }
}

#[cfg(test)]
mod tests {
    use super::windows_name;

    #[test]
    fn windows_names() {
        let w = |s: &str| windows_name(s.as_ref()).map(|o| o.into_string().unwrap());
        assert_eq!(w("a.txt"), None);
        assert_eq!(w("12:30.txt").as_deref(), Some("12_30.txt"));
        assert_eq!(w("what?").as_deref(), Some("what_"));
        assert_eq!(w("dots..").as_deref(), Some("dots._"));
        assert_eq!(w("con").as_deref(), Some("_con"));
        assert_eq!(w("NUL.tar.gz").as_deref(), Some("_NUL.tar.gz"));
        assert_eq!(w("COM1").as_deref(), Some("_COM1"));
        assert_eq!(w("COM10"), None);
        assert_eq!(w("console"), None);
        assert_eq!(w("a\\b").as_deref(), Some("a_b"));
    }
}
