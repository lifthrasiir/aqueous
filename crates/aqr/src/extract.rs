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
//! Names are taken as the file system extracted into could take them ([`Folding`], [`fold`]):
//! an entry of a name taken as an earlier one's of its directory is a conflict, told and not
//! made (nor what is under it); and a link whose target goes through a name that file systems
//! may take as a link of the tree or not (`a` of link `A`) is told and not made, as where it
//! leads is not known.
//!
//! The entries at the top of the tree go where [`Place`] tells: into a directory of the
//! archive's own, into a directory as it is (the current one: as `tar`, `unzip` and 7-Zip
//! extract), or, with `--auto-dir` ([`Place::Auto`]), a single entry at the top as it is beside
//! the archive where nothing is there of its name, else into a directory of the archive's own.
//! As a tree read as it is decoded is not known whole, that is decided by the entries first
//! told; where another entry at the top comes later, what was made of the first one is moved
//! into the archive's directory.
//!
//! Where some paths of the archive are selected ([`Selection`]), only those entries and what is
//! under them are made: in place, as the whole archive would have them ([`Place::Into`]: the
//! directories on the way made plainly, without their metadata, and the directory extracted
//! into made only where something goes there), or at a path of their own, renamed. The links
//! under a path renamed are made only where they lead within it (a link renamed itself is made
//! as it is), and no metadata of the root is applied.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, RwLock};

use aqr_formula::aqr::{EntryNode, Extra, Tree, name_bytes};

use crate::meta::{self, Kind};
use crate::names::{self, Names};
use crate::{create, fold, stats};

/// Whether output `path` is to be written over where it is there: `None` not, the output
/// skipped (the frontend answering so; [`crate::output`]).
pub type Ask<'a> = dyn Fn(&Path) -> Result<Option<bool>, String> + Sync + 'a;

/// Where the entries at the top of a tree are extracted ([module](self)).
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// Into a directory of the archive's own: made, or there already where forced (asked
    /// before); the root's metadata its.
    Own(&'a Path),
    /// Into a directory as it is, beside what is there: made where it is not (the root's
    /// metadata its then). The current one where empty.
    Into(&'a Path),
    /// A single entry at the top into `base` as it is, where nothing is there of its name or that
    /// is `own`'s; else into `own`, a directory of the archive's own. `ask` is asked of what is
    /// there.
    Auto { base: &'a Path, own: &'a Path, ask: &'a Ask<'a> },
}

/// Paths of an archive's entries extracted alone ([module](self)): each by its names (encoded
/// as the tree's), and where it goes where not in place. None is under another.
#[derive(Clone)]
pub struct Selection {
    pub paths: Vec<(Vec<Vec<u8>>, Option<PathBuf>)>,
}

impl Selection {
    /// The entry of `t` at each path (`None`: none there).
    pub fn find(&self, t: &Tree) -> Vec<Option<usize>> {
        // (each entry after its parent: one pass over the entries a path, rather than a map of
        // them all for a few paths)
        let at = |names: &[Vec<u8>]| {
            let (mut at, mut d) = (None, 0);
            if names.is_empty() {
                return None;
            }
            for (i, e) in t.entries.iter().enumerate() {
                if e.parent == at && e.name == names[d] {
                    (at, d) = (Some(i), d + 1);
                    if d == names.len() {
                        return at;
                    }
                }
            }
            None
        };
        self.paths.iter().map(|(names, _)| at(names)).collect()
    }

    /// Path `k` for people.
    pub fn shown(&self, k: usize) -> String {
        let names: Vec<&[u8]> = self.paths[k].0.iter().map(|n| name_bytes(n).0).collect();
        String::from_utf8_lossy(&names.join(&b'/')).into_owned()
    }

    /// The paths (of those of indices `of`, where given) as names of bytes, as aqr-unpack
    /// selects them.
    pub fn unpacking(&self, of: Option<usize>) -> aqr_unpack::Select {
        let raw = |p: &Vec<Vec<u8>>| p.iter().map(|n| name_bytes(n).0.to_vec()).collect();
        let paths = self.paths.iter().enumerate().filter(|(k, _)| of.is_none_or(|o| o == *k));
        aqr_unpack::Select::new(paths.map(|(_, (p, _))| raw(p)).collect())
    }

    /// An error of the first path not in `t`, where one is not: told why by `left_out` (what
    /// was told of the entries of a path left out of the tree, or nothing), or not there.
    pub fn check(&self, t: &Tree, left_out: &dyn Fn(usize) -> Vec<String>) -> Result<(), String> {
        let Some(k) = self.find(t).iter().position(Option::is_none) else { return Ok(()) };
        match &left_out(k)[..] {
            [] => Err(format!("{}: not in the archive", self.shown(k))),
            [why] => Err(why.clone()),
            [why, more @ ..] => Err(format!("{why} (and {} more under {})", more.len(), self.shown(k))),
        }
    }

    /// By entry of `t`, whether it is selected or under one selected.
    pub fn picked(&self, t: &Tree) -> Vec<bool> {
        let roots = self.find(t);
        let mut picked = vec![false; t.entries.len()];
        for (i, e) in t.entries.iter().enumerate() {
            picked[i] = roots.contains(&Some(i)) || e.parent.is_some_and(|p| picked[p]);
        }
        picked
    }

    /// The contents of `t` (of `n` at least) of the entries selected and those under them,
    /// their forks too: by content, whether it is.
    pub fn contents(&self, t: &Tree, n: usize) -> Vec<bool> {
        let picked = self.picked(t);
        let mut out = vec![false; n];
        for (i, e) in t.entries.iter().enumerate() {
            if !picked[i] {
                continue;
            }
            let file = if let EntryNode::File(c) = e.node { Some(c) } else { None };
            for c in file.into_iter().chain(e.extra.forks.iter().map(|f| f.1)) {
                *grow(&mut out, c) = true;
            }
        }
        out
    }
}

/// What of an entry is made, where some paths are selected.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pick {
    /// Not: neither on the way to a path selected, nor under one.
    Out,
    /// Not, but on the way to paths selected that are renamed only.
    Via,
    /// A directory on the way to a path selected in place: made plainly.
    Above,
    /// Path `k` selected, or an entry under it.
    Root(usize),
    Under(usize),
}

/// Extracts the entries told into a directory ([module](self)).
pub struct Extractor<'a> {
    place: Place<'a>,
    /// The paths selected, where not all.
    sel: Option<Selection>,
    /// Whether what is there is written over (decided as the output is, with [`Place::Auto`]).
    force: AtomicBool,
    /// Whether the output was skipped as asked ([`Place::Auto`]): the error then not one.
    skipped: AtomicBool,
    ap: &'a meta::Applier,
    seen: &'a stats::Seen,
    /// The names as this system writes them ([`names`]).
    names: Names,
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
    /// The directory that the entries at the top go into (`None`: not decided yet), and whether
    /// it is the archive's own (made by it, given its metadata and forks).
    top: Option<PathBuf>,
    ours: bool,
    /// Of [`Place::Auto`], the single entry at the top extracted as it is beside the archive.
    single: Option<usize>,
    /// By entry, what of it is made ([`Pick`]; `None`: not told yet); of those on the way to
    /// paths selected, their names.
    picks: Vec<Option<Pick>>,
    via: HashMap<usize, Vec<Vec<u8>>>,
    /// Whether the directory extracted into was made where something was to go there (where
    /// paths are selected, as it is made only then).
    top_made: bool,
    /// How the file system extracted into takes names as one (decided as the first entries are
    /// made), and the entries of each directory by the key of their names so taken.
    folding: Option<(Folding, bool)>,
    taken: HashMap<(Option<usize>, String), usize>,
    /// By entry, where it is not made as its name is one that the file system takes as an
    /// earlier entry's of its directory: that entry (`Some`), or `None` where one above it is
    /// not made so (spec, "Extraction").
    clashed: Vec<Option<Option<usize>>>,
    /// Where the file system has short names, the entries of each directory by their short
    /// names, where their names may be ones ([`short_key`]); and the first entry of each
    /// directory whose name is none (and so takes a short name made, `~` and digits:
    /// [`made_short`]).
    taken_short: HashMap<(Option<usize>, Vec<u8>), usize>,
    long_named: HashMap<Option<usize>, usize>,
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
    /// An extractor to `place`, what is there written over where `force`; of the paths of `sel`
    /// alone where given (into [`Place::Into`] alone).
    pub fn new(
        place: Place<'a>,
        force: bool,
        ap: &'a meta::Applier,
        seen: &'a stats::Seen,
        sel: Option<Selection>,
        opts: &names::Opts,
    ) -> Result<Self, String> {
        let (top, ours) = match place {
            // (made as something goes there)
            Place::Into(dir) if sel.is_some() => (Some(dir.to_owned()), false),
            _ if sel.is_some() => return Err("paths selected of an archive extracted, not into a directory".into()),
            Place::Own(out) => {
                match fs::create_dir(out) {
                    Err(e) if !(force && e.kind() == std::io::ErrorKind::AlreadyExists && out.is_dir()) => {
                        return Err(err(out, e));
                    }
                    _ => {}
                }
                (Some(out.to_owned()), true)
            }
            Place::Into(dir) if dir.as_os_str().is_empty() || dir.is_dir() => (Some(dir.to_owned()), false),
            Place::Into(dir) => {
                fs::create_dir_all(dir).map_err(|e| err(dir, e))?;
                (Some(dir.to_owned()), true)
            }
            Place::Auto { .. } => (None, false),
        };
        // (of where the entries go, or beside it: decided before any name is mapped)
        let folding = Folding::of(match place {
            Place::Own(d) | Place::Into(d) | Place::Auto { base: d, .. } => d,
        });
        Ok(Extractor {
            place,
            sel,
            force: AtomicBool::new(force),
            skipped: AtomicBool::new(false),
            ap,
            seen,
            names: Names::new(opts, folding.0 == Folding::Exact),
            s: RwLock::new(State { top, ours, folding: Some(folding), ..State::default() }),
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
        s.picks.resize(n, None);
        self.names.decide(t);
        if let Some(&i) = new.iter().find(|&&i| self.names.fails(&self.names.name(&t.entries[i].name))) {
            let shown = names::escaped(name_bytes(&t.entries[i].name).0);
            return Err(format!("{shown}: not a name this system takes as it is (--bad-names fail)"));
        }
        self.place(s, t, new)?;
        // (where paths are selected, the entries made of those told)
        let picked: Vec<usize>;
        let new = match &self.sel {
            None => new,
            Some(sel) => {
                for &i in new {
                    if s.picks[i].is_none() {
                        s.picks[i] = Some(pick(sel, s, t, i));
                    }
                }
                picked = new.iter().copied().filter(|&i| !matches!(s.picks[i], Some(Pick::Out | Pick::Via))).collect();
                &picked[..]
            }
        };
        if s.top.is_none() {
            return Ok(());
        }
        if !s.root && s.ours {
            s.root = true;
            for (k, f) in t.root.forks.iter().enumerate() {
                grow(&mut s.forks, f.1).push((None, k, f.0.clone(), Kind::Dir));
            }
        }
        let top = s.top.clone().unwrap();
        let force = self.force.load(Relaxed);
        // the entries of names the file system takes as an earlier one's, not made (nor what is
        // under them)
        let (folding, short) = *s.folding.get_or_insert_with(|| Folding::of(&top));
        s.clashed.resize(n, None);
        for &i in new {
            let e = &t.entries[i];
            if s.clashed[i].is_some() {
                continue;
            }
            if e.parent.is_some_and(|p| s.clashed[p].is_some()) {
                s.clashed[i] = Some(None);
                continue;
            }
            let renamed =
                matches!((s.picks[i], &self.sel), (Some(Pick::Root(k)), Some(sel)) if sel.paths[k].1.is_some());
            if renamed {
                continue;
            }
            if let Some(key) = folding.key(&self.names.name(&e.name).os) {
                let j = *s.taken.entry((e.parent, key)).or_insert(i);
                if j != i {
                    s.clashed[i] = Some(Some(j));
                }
            }
            // (of short names too: FAT on Windows takes a name as another's short name, by the
            // best fit of the OEM code page, `pÀq` as `paq`; and FAT and NTFS take a short name
            // made, `LONGFI~1.TXT`, as the long name's)
            if short && s.clashed[i].is_none() {
                match short_key(&self.names.name(&e.name).os) {
                    Some(k) => {
                        let long = s.long_named.get(&e.parent).copied().filter(|&j| j != i && made_short(&k));
                        let j = long.unwrap_or_else(|| *s.taken_short.entry((e.parent, k)).or_insert(i));
                        if j != i {
                            s.clashed[i] = Some(Some(j));
                        }
                    }
                    None => {
                        s.long_named.entry(e.parent).or_insert(i);
                    }
                }
            }
        }
        // the link groups of the files told, consistent where all their files are of a content
        let mut touched = Vec::new();
        for &i in new {
            let e = &t.entries[i];
            if s.clashed[i].is_some() {
                continue;
            }
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
            match s.clashed[i] {
                Some(Some(j)) if s.paths[i].as_os_str().is_empty() => {
                    // (told once: its path kept to tell it once)
                    let parent = e.parent.map_or(&top, |p| &s.paths[p]);
                    let path = parent.join(self.names.name(&e.name).os);
                    let what = if e.node == EntryNode::Dir { "nor what is under it " } else { "" };
                    self.ap.tell(
                        &path,
                        &format!(
                            "not extracted, {what}as this file system may take its name as that of {}",
                            s.paths[j].display()
                        ),
                    );
                    s.paths[i] = path;
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            if s.paths[i].as_os_str().is_empty() {
                match (s.picks[i], &self.sel) {
                    (Some(Pick::Root(k)), Some(sel)) if sel.paths[k].1.is_some() => {
                        let to = sel.paths[k].1.clone().unwrap();
                        if let Some(p) = to.parent().filter(|p| !p.as_os_str().is_empty()) {
                            fs::create_dir_all(p).map_err(|x| err(p, x))?;
                        }
                        s.paths[i] = to;
                    }
                    _ => {
                        if e.parent.is_none() && self.sel.is_some() && !s.top_made {
                            if !top.as_os_str().is_empty() {
                                fs::create_dir_all(&top).map_err(|x| err(&top, x))?;
                            }
                            s.top_made = true;
                        }
                        let parent = e.parent.map_or(&top, |p| &s.paths[p]);
                        s.paths[i] = parent.join(self.fs_name(parent, &e.name));
                    }
                }
            }
            let path = s.paths[i].clone();
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
                // (into a directory there already, as it is)
                EntryNode::Dir => match fs::create_dir(&path) {
                    Err(x) if !(path.is_dir() && !path.is_symlink()) => return Err(err(&path, x)),
                    _ => {}
                },
                EntryNode::File(c) => {
                    grow(&mut s.files, *c).1 += 1;
                    s.content[i] = Some(*c);
                    match s.linked[i] {
                        Some(j) => {
                            if force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
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
                                create(&path, force)?;
                            }
                            s.files[*c].0.push(i);
                        }
                    }
                }
                // (made once all else is)
                EntryNode::Link(_) => {}
                EntryNode::Fifo | EntryNode::Device { .. } => {
                    if force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
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
            if s.linked[i].is_none() && s.picks[i] != Some(Pick::Above) {
                for (k, f) in e.extra.forks.iter().enumerate() {
                    grow(&mut s.forks, f.1).push((Some(i), k, f.0.clone(), kind));
                }
            }
            // (a link made once all else is)
            s.made[i] = (kind != Kind::Link).then_some(kind);
        }
        Ok(())
    }

    /// Of [`Place::Auto`], decides where the entries at the top go as entries are first told
    /// ([module](self)), and where an entry of `new` is at the top beside the single one
    /// extracted as it is, moves that into the archive's directory.
    fn place(&self, s: &mut State, t: &Tree, new: &[usize]) -> Result<(), String> {
        let Place::Auto { base, own, ask } = self.place else { return Ok(()) };
        let top = |i: &&usize| t.entries[**i].parent.is_none();
        match s.single {
            None if s.top.is_none() && !new.is_empty() => {
                // (-o, made where it is not)
                if !base.as_os_str().is_empty() {
                    fs::create_dir_all(base).map_err(|e| err(base, e))?;
                }
                let mut tops = new.iter().filter(top);
                if let (Some(&i), None) = (tops.next(), tops.next()) {
                    let name = &t.entries[i].name;
                    let at = base.join(self.fs_name(base, name));
                    // (of the archive's name: the archive's directory, asked of as it is)
                    let mine = own.file_name().is_some_and(|o| alike(&self.names.name(name).os, o));
                    let there = fs::symlink_metadata(&at).is_ok();
                    if !there || mine {
                        if there {
                            match ask(own)? {
                                Some(f) => self.force.store(f, Relaxed),
                                None => return Err(self.skip()),
                            }
                        }
                        tell_output(&at);
                        s.paths[i] = at;
                        s.single = Some(i);
                        s.top = Some(base.to_owned());
                        return Ok(());
                    }
                }
                match self.own(s, own, ask)? {
                    true => Ok(()),
                    false => Err(self.skip()),
                }
            }
            Some(i) if new.iter().filter(top).any(|&j| j != i) => {
                let at = s.paths[i].clone();
                if !self.own(s, own, ask)? {
                    return Err(format!("{} exists, and {} was extracted already", own.display(), at.display()));
                }
                let to = own.join(at.file_name().unwrap());
                if s.made[i].is_some() {
                    fs::rename(&at, &to).map_err(|x| err(&to, x))?;
                }
                // (every path so far is of the single entry: in `base`)
                for p in s.paths.iter_mut().filter(|p| !p.as_os_str().is_empty()) {
                    *p = own.join(p.strip_prefix(base).unwrap());
                }
                s.single = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Of [`Place::Auto`], has the entries at the top go into the archive's directory `own`:
    /// made, or there already as `ask` answers (false: not to be written over).
    fn own(&self, s: &mut State, own: &Path, ask: &Ask<'_>) -> Result<bool, String> {
        let force = match ask(own)? {
            Some(f) => f,
            None => return Ok(false),
        };
        if let Some(p) = own.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(p).map_err(|e| err(p, e))?;
        }
        match fs::create_dir(own) {
            Err(e) if !(force && e.kind() == std::io::ErrorKind::AlreadyExists && own.is_dir()) => {
                return Err(err(own, e));
            }
            _ => {}
        }
        self.force.store(force, Relaxed);
        tell_output(own);
        s.top = Some(own.to_owned());
        s.ours = true;
        Ok(true)
    }

    /// The output skipped as asked: the error that ends the extraction ([`Extractor::skipped`]).
    fn skip(&self) -> String {
        self.skipped.store(true, Relaxed);
        "skipped".into()
    }

    /// Whether the output was skipped as the frontend answered ([`Place::Auto`]): an error of
    /// the extraction then not one, nothing extracted.
    pub fn skipped(&self) -> bool {
        self.skipped.load(Relaxed)
    }

    /// The name of `name` (an entry's, made) on this system, in directory `dir` ([`Names`]):
    /// told where it is not written as it is (spec, "Extraction"), and counted.
    fn fs_name(&self, dir: &Path, name: &[u8]) -> OsString {
        let m = self.names.name(name);
        if m.replaced {
            let was = names::escaped(name_bytes(name).0);
            self.ap.tell(
                &dir.join(&was),
                &format!("not a name this system takes, written as {}", Path::new(&m.os).display()),
            );
        }
        self.names.count(&m);
        m.os
    }

    /// What is told of the names of the entries made ([`Names::told`]).
    pub fn names_told(&self) -> Vec<String> {
        self.names.told()
    }

    /// Takes content `c` as lost (not to be written): its files made so far (empty, or hard links
    /// to one of them) removed, and their metadata not applied.
    pub fn lose(&self, c: usize) -> Result<(), String> {
        let mut s = self.s.write().unwrap();
        let s = &mut *s;
        for i in 0..s.content.len() {
            if s.content[i] != Some(c) {
                continue;
            }
            if s.made[i].take().is_some() {
                match fs::remove_file(&s.paths[i]) {
                    Err(x) if x.kind() != std::io::ErrorKind::NotFound => return Err(err(&s.paths[i], x)),
                    _ => {}
                }
            }
            s.soon[i] = None;
        }
        Ok(())
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
                    let mut f = create(p, self.force.load(Relaxed))?;
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
                (_, None) => self.ap.fork(s.top.as_deref().unwrap(), *kind, name, &d),
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
    /// was made. Returns the number of files written and their bytes, and what was extracted:
    /// the directory, or the single entry extracted as it is ([`Place::Auto`]); `None` where the
    /// output was skipped as asked.
    pub fn finish(self, t: &Tree) -> Result<Option<(usize, u64, PathBuf)>, String> {
        // (nothing told: the archive's directory, empty)
        if let Place::Auto { own, ask, .. } = self.place {
            let mut s = self.s.write().unwrap();
            if s.top.is_none() && !self.own(&mut s, own, ask)? {
                return Ok(None);
            }
        }
        let mut s = self.s.into_inner().unwrap();
        let top = s.top.clone().unwrap();
        let force = self.force.load(Relaxed);
        let renamed = |k: usize| self.sel.as_ref().and_then(|sel| sel.paths[k].1.as_deref());
        let out = match (s.single, &self.sel) {
            (Some(i), _) => s.paths[i].clone(),
            // (a single path selected and renamed: that)
            (None, Some(sel)) if sel.paths.len() == 1 && sel.paths[0].1.is_some() => sel.paths[0].1.clone().unwrap(),
            (None, _) => top.clone(),
        };
        // (made at the first link)
        let mut by_name: Option<ByName> = None;
        for (i, e) in t.entries.iter().enumerate() {
            let EntryNode::Link(target) = &e.node else { continue };
            if s.paths.get(i).is_none_or(|p| p.as_os_str().is_empty()) {
                continue;
            }
            if s.clashed.get(i).is_some_and(Option::is_some) {
                continue;
            }
            let by_name = by_name.get_or_insert_with(|| {
                let mut m = ByName::new();
                for (j, e) in t.entries.iter().enumerate() {
                    if s.clashed.get(j).is_none_or(Option::is_none) {
                        by_name_add(&mut m, e.parent, Name::of(&self.names.name(&e.name).os), j);
                    }
                }
                m
            });
            let path = &s.paths[i];
            // (of a single entry extracted as it is: reaching out of it is reaching out; and of a
            // path renamed, reaching out of it, but where the link is the path renamed itself)
            let (within, of, alone) = match s.picks[i] {
                Some(Pick::Root(k) | Pick::Under(k)) if renamed(k).is_some() => {
                    let names = self.sel.as_ref().unwrap().paths[k].0.clone();
                    (Some(names), renamed(k).unwrap(), s.picks[i] == Some(Pick::Root(k)))
                }
                _ => (s.single.map(|f| vec![t.entries[f].name.clone()]), out.as_path(), false),
            };
            let to = match resolve_link(t, &self.names, by_name, i, within.as_deref()) {
                Ok(to) => to,
                Err(_) if alone => resolve_link(t, &self.names, by_name, i, None).ok().flatten(),
                Err(Refused::Out) => {
                    let of = if of.as_os_str().is_empty() { Path::new(".") } else { of };
                    self.ap.tell(path, &format!("a link reaching out of {}, not created", of.display()));
                    continue;
                }
                Err(Refused::Loop) => {
                    self.ap.tell(path, "a link going round in a loop of links, not created");
                    continue;
                }
                Err(Refused::Unsure(name)) => {
                    self.ap.tell(
                        path,
                        &format!("a link through {name}, which file systems may take as a link of the archive or not, not created"),
                    );
                    continue;
                }
            };
            if force && fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
                fs::remove_file(path).map_err(|x| err(path, x))?;
            }
            // (on Windows, of the kind recorded, else by what it leads to; spec, "Extraction")
            let dir = match e.extra.win_link_type {
                Some(k) => k != 0,
                None => to.is_some_and(|j| t.entries[j].node == EntryNode::Dir),
            };
            let (os, substituted) = self.names.link(target);
            if substituted {
                let shown = String::from_utf8_lossy(name_bytes(target).0);
                self.ap.tell(path, &format!("a link to {shown}, made to {}", Path::new(&os).display()));
            }
            symlink(&os, path, dir).map_err(|x| err(path, x))?;
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
            if s.made[i] == Some(Kind::Dir) && s.picks.get(i) != Some(&Some(Pick::Above)) {
                self.ap.apply(&s.paths[i], Kind::Dir, &e.extra)?;
            }
        }
        if s.ours {
            self.ap.apply(&top, Kind::Dir, &t.root)?;
        }
        Ok(Some((self.files.into_inner(), self.bytes.into_inner(), out)))
    }
}

/// What of entry `i` of `t` is made, of the paths of `sel` ([`Pick`]), its parent's told before.
fn pick(sel: &Selection, s: &mut State, t: &Tree, i: usize) -> Pick {
    let e = &t.entries[i];
    let names = match e.parent.map(|p| (p, s.picks[p])) {
        Some((_, Some(Pick::Root(k) | Pick::Under(k)))) => return Pick::Under(k),
        Some((_, Some(Pick::Out) | None)) => return Pick::Out,
        Some((p, Some(Pick::Via | Pick::Above))) => [&s.via[&p][..], std::slice::from_ref(&e.name)].concat(),
        None => vec![e.name.clone()],
    };
    if let Some(k) = sel.paths.iter().position(|(p, _)| *p == names) {
        return Pick::Root(k);
    }
    let on_way = |renamed: bool| {
        sel.paths.iter().any(|(p, r)| r.is_some() == renamed && p.len() > names.len() && p.starts_with(&names))
    };
    let pick = match (e.node == EntryNode::Dir, on_way(false), on_way(true)) {
        (true, true, _) => Pick::Above,
        (true, false, true) => Pick::Via,
        _ => return Pick::Out,
    };
    s.via.insert(i, names);
    pick
}

/// Tells the frontend what is extracted, as it is decided ([`Place::Auto`]).
fn tell_output(p: &Path) {
    crate::ipc::tell(serde_json::json!({"event": "output", "output": p.display().to_string()}));
}

/// Whether names `a` and `b` are of one file, as this system takes names (on Windows and
/// macOS, case aside).
fn alike(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// `v[i]`, `v` grown to have it.
fn grow<T: Default>(v: &mut Vec<T>, i: usize) -> &mut T {
    if v.len() <= i {
        v.resize_with(i + 1, T::default);
    }
    &mut v[i]
}

/// The name `name` takes on Windows where it cannot be as it is ([`windows_wide`]). `None`
/// where it can be.
#[cfg(windows)]
pub fn windows_name(name: &std::ffi::OsStr) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    windows_wide(&name.encode_wide().collect::<Vec<_>>()).map(|w| OsString::from_wide(&w))
}

/// The name of UTF-16 `name` (lone surrogates and all) on Windows where it cannot be as it is:
/// the characters a name cannot have (`<>:"/\|?*` and controls) and a dot or a space at its end
/// as `_`, and `_` before a device's name (`CON`, `NUL.txt`, ...). `None` where it can be.
#[cfg(any(windows, test))]
fn windows_wide(name: &[u16]) -> Option<Vec<u16>> {
    let bad = |c: u16| c < 0x20 || b"<>:\"/\\|?*".iter().any(|&b| b as u16 == c);
    let mut out: Vec<u16> = name.iter().map(|&c| if bad(c) { b'_' as u16 } else { c }).collect();
    if let Some(last) = out.last_mut()
        && (*last == b'.' as u16 || *last == b' ' as u16)
    {
        *last = b'_' as u16;
    }
    // (devices' names are of ASCII and superscripts, which surrogates do not change)
    let s = String::from_utf16_lossy(&out);
    let stem = s.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let device = matches!(&stem[..], "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9')
        || (stem.starts_with("COM") || stem.starts_with("LPT")) && ["¹", "²", "³"].contains(&&stem[3..]);
    if device {
        out.insert(0, b'_' as u16);
    }
    (out != name).then_some(out)
}

/// How the file system extracted into takes names of a directory as one, as far as it is told
/// apart: entries of names it takes as one are a conflict ([`Extractor::entries`]).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Folding {
    /// As they are.
    Exact,
    /// Normalization aside (macOS's case-sensitive APFS and HFSX; on Linux what a probe finds so).
    #[cfg_attr(windows, allow(dead_code))]
    Normalization,
    /// Case and normalization aside ([`fold::fold`]: macOS's case-insensitive file systems,
    /// Windows; on Linux FAT, exFAT, HFS and what a probe finds so).
    Full,
    /// As [`Folding::Full`], and characters beyond the Basic Multilingual Plane alike
    /// ([`fold::fold_astral`]: Linux's hfsplus).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    FullAstral,
}

impl Folding {
    /// The folding of the file system of directory `dir` (or the nearest one above it there),
    /// and whether it has short names (8.3) that a name could be taken as (FAT; on Windows, any,
    /// as NTFS may have them too).
    fn of(dir: &Path) -> (Folding, bool) {
        let mut d = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        while !d.exists()
            && let Some(p) = d.parent()
        {
            d = if p.as_os_str().is_empty() { Path::new(".") } else { p };
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::ffi::OsStrExt;
            let Ok(c) = std::ffi::CString::new(d.as_os_str().as_bytes()) else { return (Folding::Full, true) };
            // SAFETY: a path of a C string
            let folding = match unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) } {
                1 => Folding::Normalization,
                _ => Folding::Full,
            };
            // SAFETY: plain data, filled by `statfs`
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            // SAFETY: as above
            let fat = unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 || {
                // SAFETY: a NUL-terminated name of the file system's type
                let ty = unsafe { std::ffi::CStr::from_ptr(st.f_fstypename.as_ptr()) };
                ty.to_bytes() == b"msdos"
            };
            return (folding, fat);
        }
        #[cfg(target_os = "linux")]
        return linux_folding(d);
        #[cfg(windows)]
        {
            let _ = d;
            return (Folding::Full, true);
        }
        #[allow(unreachable_code)]
        {
            let _ = d;
            (Folding::Exact, false)
        }
    }

    /// The key of `name` (a name on this system) by which names this file system takes as one
    /// are alike; `None` where it takes them as they are.
    fn key(self, name: &std::ffi::OsStr) -> Option<String> {
        match self {
            Folding::Exact => None,
            Folding::Normalization => Some(fold::normalized(name)),
            Folding::Full => Some(fold::fold(name)),
            Folding::FullAstral => Some(fold::fold_astral(name)),
        }
    }
}

/// [`Folding::of`] directory `d` (there) on Linux: by the type of its file system where that
/// tells (FAT of short names, exFAT, HFS, hfsplus), else by a probe in `d` (files `.aqr-names-*`
/// made and removed: names of another case, or composed and decomposed, found as one), which
/// finds casefolded directories of ext4 and f2fs, ntfs3 of `nocase`, CIFS, and shares of
/// macOS's file systems in virtual machines. Where `d` cannot be written, as they are.
#[cfg(target_os = "linux")]
fn linux_folding(d: &Path) -> (Folding, bool) {
    use std::os::unix::ffi::OsStrExt;
    if let Ok(c) = std::ffi::CString::new(d.as_os_str().as_bytes()) {
        // SAFETY: plain data, filled by `statfs`
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: a path of a C string
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } == 0 {
            match st.f_type as u64 & 0xffff_ffff {
                // MSDOS_SUPER_MAGIC (vfat, msdos)
                0x4d44 => return (Folding::Full, true),
                // EXFAT_SUPER_MAGIC, HFS_SUPER_MAGIC
                0x2011_bab0 | 0x4244 => return (Folding::Full, false),
                // HFSPLUS_SUPER_MAGIC
                0x482b => return (Folding::FullAstral, false),
                _ => {}
            }
        }
    }
    let base = format!(".aqr-names-{}-", std::process::id());
    let probe = |made: &str, other: &str| -> Option<bool> {
        let at = d.join(format!("{base}{made}"));
        fs::OpenOptions::new().write(true).create_new(true).open(&at).ok()?;
        let one = fs::symlink_metadata(d.join(format!("{base}{other}"))).is_ok();
        let _ = fs::remove_file(&at);
        Some(one)
    };
    match (probe("a", "A"), probe("\u{e9}", "e\u{301}")) {
        (Some(true), _) => (Folding::Full, false),
        (_, Some(true)) => (Folding::Normalization, false),
        _ => (Folding::Exact, false),
    }
}

/// The short name (8.3) that FAT on Windows may take `name` (a name on this system) as: as
/// FastFAT looks a name up, uppercased and in the OEM code page, characters it does not have
/// as their best fit (`pÀq` as `PAQ` in code page 949), where that is a valid 8.3 name; and so
/// the short name of another file, `paq`'s (`PAQ`), or one Windows made for a long one
/// (`LONGFI~1.TXT`).
#[cfg(windows)]
fn short_key(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = name.encode_wide().collect();
    // (none of more than 12 units is: each is a byte at least)
    if w.is_empty() || w.len() > 12 {
        return None;
    }
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }
    #[repr(C)]
    struct OemString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u8,
    }
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn RtlUpcaseUnicodeStringToOemString(dest: *mut OemString, src: *const UnicodeString, allocate: u8) -> i32;
    }
    let mut buf = [0u8; 32];
    let src = UnicodeString { length: (w.len() * 2) as u16, maximum_length: (w.len() * 2) as u16, buffer: w.as_ptr() };
    let mut dest = OemString { length: 0, maximum_length: buf.len() as u16, buffer: buf.as_mut_ptr() };
    // SAFETY: `src` of `w`'s units, `dest` of `buf`'s bytes (two for each unit at most), not
    // allocated
    if unsafe { RtlUpcaseUnicodeStringToOemString(&mut dest, &src, 0) } < 0 {
        return None;
    }
    let oem = &buf[..dest.length as usize];
    short_valid(oem).then(|| oem.to_vec())
}

/// The short name (8.3) that FAT may take `name` (a name on this system) as, elsewhere than on
/// Windows: of ASCII names alone, uppercased (macOS's and Linux's FAT take names of no such
/// short name as their long names: [`fold::fold`] has them).
#[cfg(not(windows))]
fn short_key(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    let s = name.to_str().filter(|s| s.is_ascii())?;
    let up = s.to_ascii_uppercase().into_bytes();
    short_valid(&up).then_some(up)
}

/// Whether `oem` (a name in an OEM code page) may be a short name (8.3) of FAT: a base of 1 to 8
/// bytes and an extension of 1 to 3 after a dot, or none. Lenient (the characters FAT does not
/// take aside, but `?`, the best fit of characters of no fit, and `*`), as a name taken as short
/// that is not is a conflict or a link not made more, but one not taken that is could be written
/// through.
fn short_valid(oem: &[u8]) -> bool {
    if oem.iter().any(|&c| c < 0x20 || c == b'?' || c == b'*') {
        return false;
    }
    let (base, ext) = match oem.iter().position(|&c| c == b'.') {
        Some(k) => (&oem[..k], Some(&oem[k + 1..])),
        None => (oem, None),
    };
    (1..=8).contains(&base.len()) && ext.is_none_or(|e| (1..=3).contains(&e.len()) && !e.contains(&b'.'))
}

/// Whether short name `key` is one of those Windows makes for long names (`~` and digits): it
/// may be the short name of any long name of its directory.
fn made_short(key: &[u8]) -> bool {
    key.windows(2).any(|w| w[0] == b'~' && w[1].is_ascii_digit())
}

/// A name (an entry's, or one of a link target) as file systems could take it: folded
/// ([`fold::fold`]), folded as Linux's hfsplus takes it ([`fold::fold_astral`]), and its short
/// name where it may be one ([`short_key`]). Names alike by any of them are taken as one.
#[derive(Clone)]
struct Name {
    fold: String,
    astral: String,
    short: Option<Vec<u8>>,
}

impl Name {
    /// The name of `os` (a name on this system, mapped: [`Names`]).
    fn of(os: &std::ffi::OsStr) -> Name {
        Name { fold: fold::fold(os), astral: fold::fold_astral(os), short: short_key(os) }
    }

    /// The keys of the name in [`ByName`].
    /// The keys of the name in [`ByName`] (each kind of every name, as `x😀`'s `Astral` key is
    /// `x????`'s).
    fn keys(&self) -> impl Iterator<Item = Key> + '_ {
        [Key::Fold(self.fold.clone()), Key::Astral(self.astral.clone())]
            .into_iter()
            .chain(self.short.clone().map(Key::Short))
    }
}

/// A key of [`ByName`]: a name folded, folded as Linux's hfsplus takes it, or a short name.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Fold(String),
    Astral(String),
    Short(Vec<u8>),
}

/// The entries of a tree by their directory and the keys of their names ([`Name`]): those that
/// some file system takes as one name, together.
type ByName = HashMap<(Option<usize>, Key), Vec<usize>>;

/// Adds entry `j` of `parent` and name `n` to `m`.
fn by_name_add(m: &mut ByName, parent: Option<usize>, n: Name, j: usize) {
    for k in n.keys() {
        m.entry((parent, k)).or_default().push(j);
    }
}

/// Why a link is not made ([`resolve_link`]).
enum Refused {
    /// It reaches out of the directory extracted into.
    Out,
    /// Following the tree's links on its way does not end: they go round in a loop (or there
    /// are more of them on the way than the tree has entries).
    Loop,
    /// A name of its target, `.0`, could be a link of the tree as the file system takes names, or
    /// not: what it leads to is not known.
    Unsure(String),
}

/// Where link `i` of `t` leads, resolved from the link's directory through the links of the
/// tree (`by_name`, of the entries not made aside): the entry (`None`: none of the tree). Refused
/// where it reaches out of the directory it is extracted into, by an absolute target or by one
/// going above it (or out of the entry of names `within`, where given: the one extracted as the
/// root); where the links on its way go round in a loop; or where a name of it could be a link as some file system takes names (by case, by
/// normalization, or as a short name of Windows) and not as others: a link is followed only
/// where the name is the link's as it is, and the link the only entry of names taken as one
/// there (spec, "Extraction").
fn resolve_link(
    t: &Tree,
    names: &Names,
    by_name: &ByName,
    i: usize,
    within: Option<&[Vec<u8>]>,
) -> Result<Option<usize>, Refused> {
    // the path of an entry, as names from the root
    let path_of = |mut e: Option<usize>| {
        let mut p = Vec::new();
        while let Some(i) = e {
            p.push(Name::of(&names.name(&t.entries[i].name).os));
            e = t.entries[i].parent;
        }
        p.reverse();
        p
    };
    // the entries at a path, those of directories taken as one together
    let find = |path: &[Name]| -> Vec<usize> {
        let mut at: Vec<Option<usize>> = vec![None];
        for n in path {
            let mut next: Vec<Option<usize>> = Vec::new();
            for &d in &at {
                for j in n.keys().filter_map(|k| by_name.get(&(d, k))).flatten() {
                    if !next.contains(&Some(*j)) {
                        next.push(Some(*j));
                    }
                }
            }
            at = next;
            if at.is_empty() {
                break;
            }
        }
        at.into_iter().flatten().collect()
    };
    // (the tree's own links followed, as many as there are entries at most)
    let mut budget = t.entries.len() + 1;
    fn resolve(
        t: &Tree,
        names: &Names,
        dir: Vec<Name>,
        target: &[u8],
        budget: &mut usize,
        find: &dyn Fn(&[Name]) -> Vec<usize>,
        path_of: &dyn Fn(Option<usize>) -> Vec<Name>,
    ) -> Result<Vec<Name>, Refused> {
        let (bytes, raw) = name_bytes(target);
        if bytes.starts_with(b"/") || (cfg!(windows) && bytes.get(1) == Some(&b':')) {
            return Err(Refused::Out);
        }
        let mut at = dir;
        for part in bytes.split(|&b| b == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    at.pop().ok_or(Refused::Out)?;
                }
                name => {
                    let os = names.part(name, raw).os;
                    let n = Name::of(&os);
                    // (a short name of FAT or NTFS made, which could be any long name's of the
                    // directory)
                    let made = n.short.as_deref().is_some_and(made_short) || cfg!(windows) && name.contains(&b'~');
                    at.push(n);
                    let there = find(&at);
                    let unsure = || Refused::Unsure(String::from_utf8_lossy(name).into_owned());
                    let link = |&j: &usize| matches!(t.entries[j].node, EntryNode::Link(_));
                    if there.iter().any(link) {
                        let [j] = there[..] else { return Err(unsure()) };
                        if names.name(&t.entries[j].name).os != os {
                            return Err(unsure());
                        }
                        let EntryNode::Link(next) = &t.entries[j].node else { unreachable!() };
                        *budget = budget.checked_sub(1).ok_or(Refused::Loop)?;
                        at = resolve(t, names, path_of(t.entries[j].parent), next, budget, find, path_of)?;
                    } else if made && there.is_empty() {
                        return Err(unsure());
                    }
                }
            }
        }
        Ok(at)
    }
    let EntryNode::Link(target) = &t.entries[i].node else { return Ok(None) };
    let p = resolve(t, names, path_of(t.entries[i].parent), target, &mut budget, &find, &path_of)?;
    if let Some(w) = within {
        let within = p.len() >= w.len() && w.iter().zip(&p).all(|(n, m)| Name::of(&names.name(n).os).fold == m.fold);
        if !within {
            return Err(Refused::Out);
        }
    }
    Ok(find(&p).first().copied())
}

/// Makes a link to `target` at `at` (on Windows, to a directory where `dir`).
#[cfg(unix)]
fn symlink(target: &std::ffi::OsStr, at: &Path, _: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

/// (`target` of `\`s, [`link_target`])
#[cfg(windows)]
fn symlink(target: &std::ffi::OsStr, at: &Path, dir: bool) -> std::io::Result<()> {
    if dir { std::os::windows::fs::symlink_dir(target, at) } else { std::os::windows::fs::symlink_file(target, at) }
}

#[cfg(test)]
mod tests {
    use super::{made_short, short_valid, windows_wide};

    #[test]
    fn short_names() {
        for n in ["PAQ", "A", "ABCDEFGH", "ABCDEFGH.TXT", "A.B", "LONGFI~1.TXT"] {
            assert!(short_valid(n.as_bytes()), "{n}");
        }
        // (DBCS: a trail byte of `\` in code page 932)
        assert!(short_valid(b"\xb0\xa1\x95\x5c.TXT"));
        for n in ["", ".TXT", "ABCDEFGHI", "A.TXTX", "A.B.C", "A.", "P?Q", "A*"] {
            assert!(!short_valid(n.as_bytes()), "{n}");
        }
        assert!(made_short(b"LONGFI~1.TXT") && made_short(b"A~12") && !made_short(b"A~B") && !made_short(b"A"));
    }

    #[test]
    fn windows_names() {
        let w = |s: &str| windows_wide(&s.encode_utf16().collect::<Vec<_>>()).map(|o| String::from_utf16(&o).unwrap());
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
        // a lone surrogate kept, the rest as any other name's
        let lone = |a: &str, b: &str| [a.encode_utf16().collect(), vec![0xd800], b.encode_utf16().collect()].concat();
        assert_eq!(windows_wide(&lone("x", ":ads")), Some(lone("x", "_ads")));
        assert_eq!(windows_wide(&lone("", "\\..\\..\\x")), Some(lone("", "_.._.._x")));
        assert_eq!(windows_wide(&lone("x", "y")), None);
    }
}
