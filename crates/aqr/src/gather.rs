//! The inputs walked ([`gather`]): their entries found, followed or excluded, and read.

use super::*;

aqr_common::zopt! {
    /// The most threads that read what is found ([`gather`]): more only contend in the file system
    /// (on Windows, 402,844 files on an SSD: 11.0 s on 1, 4.2 s on 4, 4.5 s on 8, 4.4 s on 16 and
    /// 5.3 s on 32).
    static STAT_THREADS: Opt<usize> = Opt::value("stat-threads", 8, "the most threads that read the files found");
}

/// An entry found as the inputs are walked ([`walk`]): its path and name, the one found that
/// holds it (none: at the root), and whether it is a directory as it was listed; `plain`, a
/// directory on the way to an input's path in the archive, of no path and no metadata.
struct Found {
    path: PathBuf,
    name: OsString,
    parent: Option<usize>,
    dir: bool,
    plain: bool,
    /// Of a directory, what was read of it ([`meta::stat`]) before what it holds was listed:
    /// as listing it changes its access time (Linux's `relatime`, where it is a day old).
    stat: Option<std::io::Result<meta::Stat>>,
}

/// The names of the path in the archive of `input`: those `to` gives (`--rename`), exactly;
/// else those of a relative path as it is (`.` left out), and the last one alone of an absolute
/// path or one with `..` (as 7-Zip names them: the frontends give absolute paths).
pub(crate) fn archive_names(input: &Path, to: Option<&Path>) -> Result<Vec<OsString>, String> {
    use std::path::Component;
    if let Some(to) = to {
        let b = os_bytes(to.as_os_str());
        let names: Vec<&[u8]> = b.split(|&c| c == b'/').collect();
        if names.iter().any(|n| n.is_empty() || *n == b"." || *n == b"..") {
            return Err(format!("--rename {}: not a path in an archive", to.display()));
        }
        return Ok(names.into_iter().map(os_from).collect());
    }
    let last = || input.file_name().map(|n| vec![n.to_owned()]);
    let mut names = Vec::new();
    for c in input.components() {
        match c {
            Component::Normal(n) => names.push(n.to_owned()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                names = last().unwrap_or_default();
                break;
            }
        }
    }
    if names.is_empty() {
        return Err(format!("{}: no name to archive it by", input.display()));
    }
    Ok(names)
}

/// Gathers `inputs` into `g` as [`gather`] does, telling how far it is (a bar of no end known, and
/// how many files) for as long as it takes.
pub(crate) fn gather_listed(a: &Args, inputs: Inputs, g: &mut Gathered) -> Result<(), String> {
    let listing = view::Listing::new(bars());
    let gathered = ticking(a, &listing, || -> Result<(), String> {
        gather(inputs, g, &listing, a)?;
        listing.finish();
        Ok(())
    });
    listing.finish();
    gathered
}

/// Gathers `inputs` (a directory alone and not renamed: its entries at the root; else each at
/// the path [`archive_names`] gives it, as given, with the renames) into `g`: walked first (what the
/// directories hold, a call for each), then what is read of each by its path ([`meta::stat`], a
/// handle or so each, the most of the time) read on `a`'s threads, then made the entries in the
/// order they were found. The files of an inode archived already are of its content (and
/// forks), read once. Where `a` dereferences (`-L`), the links given and found are followed
/// ([`Follow`]); what `a` excludes (`-x`) is left out.
pub(crate) fn gather(inputs: Inputs, g: &mut Gathered, l: &view::Listing, a: &Args) -> Result<(), String> {
    let (threads, dereference, x) = (a.opt.threads, a.dereference, &a.exclude);
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let privileged = g.reader.privileged;
    let mut found = Vec::new();
    let mut follow = Follow { on: dereference, chain: Vec::new(), warnings: Vec::new() };
    // (each input as it is read: where followed, what it leads to)
    let read: Vec<(PathBuf, fs::FileType)> = inputs
        .paths
        .iter()
        .map(|input| {
            let m = fs::symlink_metadata(input).map_err(|e| err(input, e))?;
            Ok(match follow.target(input, &m)? {
                Some((to, m)) => (to, m.file_type()),
                None => (input.clone(), m.file_type()),
            })
        })
        .collect::<Result<_, String>>()?;
    match (&read[..], inputs.renames) {
        // (a directory alone, its entries at the root; a link alone, not followed, is archived
        // as a link at its name)
        ([(dir, t)], [None]) if t.is_dir() => {
            let s = meta::stat(dir, privileged).map_err(|e| err(dir, e))?;
            let (extra, forks) = g.reader.extra(dir, &s, &mut |w| g.warnings.push(w));
            g.tree.root = extra;
            g.atimes.note(dir, &s.m);
            if let Some(gone) = &mut g.gone {
                gone.push(Gone::of(dir, &s.m));
            }
            for f in forks {
                g.tree.root.forks.push((f.name, g.contents.len()));
                g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
                l.bytes(f.size);
            }
            walk(dir, None, &mut found, l, privileged, &mut follow, x)?;
        }
        _ => {
            // (by their names in the archive, the directories found among the inputs or on the way)
            let mut dirs: std::collections::HashMap<Vec<OsString>, usize> = Default::default();
            for (((input, t), given), to) in read.iter().zip(inputs.given).zip(inputs.renames) {
                let names = archive_names(given, to.as_deref())?;
                if x.excludes(&names) {
                    continue;
                }
                let mut parent = None;
                for k in 1..names.len() {
                    let at = dirs.entry(names[..k].to_vec()).or_insert_with(|| {
                        let name = names[k - 1].clone();
                        found.push(Found { path: PathBuf::new(), name, parent, dir: true, plain: true, stat: None });
                        found.len() - 1
                    });
                    parent = Some(*at);
                }
                // (a directory made on the way to an earlier input, given itself: that one)
                let at = match dirs.get(&names) {
                    Some(&i) if t.is_dir() && found[i].plain => {
                        (found[i].path, found[i].plain) = (input.clone(), false);
                        i
                    }
                    _ => {
                        let name = names.last().unwrap().clone();
                        found.push(Found {
                            path: input.clone(),
                            name,
                            parent,
                            dir: t.is_dir(),
                            plain: false,
                            stat: None,
                        });
                        found.len() - 1
                    }
                };
                if t.is_file() {
                    l.found();
                }
                if t.is_dir() {
                    dirs.entry(names).or_insert(at);
                    walk(input, Some(at), &mut found, l, privileged, &mut follow, x)?;
                }
            }
            taken_twice(&found)?;
        }
    }
    g.warnings.append(&mut follow.warnings);
    l.walked();
    let early: Vec<_> = found.iter_mut().map(|f| f.stat.take()).collect();
    let stats = aqr_common::pool::par_map(found.len(), threads.min(STAT_THREADS.get()), |i| {
        if found[i].plain || early[i].is_some() {
            return None;
        }
        let s = meta::stat(&found[i].path, privileged);
        if let Ok(s) = &s
            && s.m.is_file()
        {
            l.read(s.m.len());
        }
        Some(s)
    });
    if l.cancelled() {
        return Err(session::CANCELLED.into());
    }
    // (by the entries found, the entry made of each)
    let mut made: Vec<Option<usize>> = Vec::with_capacity(found.len());
    for ((f, s), e) in found.iter().zip(stats).zip(early) {
        let Some(s) = s.or(e) else {
            let parent = f.parent.map(|p| made[p].expect("a directory made"));
            let name = name_from(&os_bytes(&f.name));
            g.tree.entries.push(Entry { parent, name, node: EntryNode::Dir, extra: Default::default() });
            made.push(Some(g.tree.entries.len() - 1));
            continue;
        };
        let s = s.map_err(|e| err(&f.path, e))?;
        if s.m.is_dir() != f.dir {
            return Err(format!("{}: changed as it was listed", f.path.display()));
        }
        // (a directory never skipped)
        let parent = f.parent.map(|p| made[p].expect("a directory made"));
        let e = entry(&f.path, &f.name, &s, parent, g, l)?;
        g.atimes.note(&f.path, &s.m);
        if e.is_some()
            && let Some(gone) = &mut g.gone
        {
            gone.push(Gone::of(&f.path, &s.m));
        }
        made.push(e);
    }
    Ok(())
}

/// An error where two of `found` would be archived by the same path, telling the inputs (or
/// what was found under them) that are.
fn taken_twice(found: &[Found]) -> Result<(), String> {
    let mut seen: std::collections::HashMap<(Option<usize>, &OsStr), usize> = Default::default();
    for (i, f) in found.iter().enumerate() {
        if let Some(j) = seen.insert((f.parent, &f.name), i) {
            let mut names = vec![f.name.to_string_lossy()];
            let mut at = f.parent;
            while let Some(p) = at {
                names.push(found[p].name.to_string_lossy());
                at = found[p].parent;
            }
            names.reverse();
            let by = |k: usize| match found[k].plain {
                true => "a directory on the way to another input".to_owned(),
                false => found[k].path.display().to_string(),
            };
            return Err(format!("{} and {}: both archived as {}", by(j), by(i), names.join("/")));
        }
    }
    Ok(())
}

/// Finds what directory `dir` (found as `parent`) holds, by name, and what those directories
/// hold in turn, each directory followed by what it holds. On Unix, each directory found is read
/// ([`meta::stat`]) before it is listed. Links are followed as `follow` has it.
fn walk(
    dir: &Path,
    parent: Option<usize>,
    found: &mut Vec<Found>,
    l: &view::Listing,
    privileged: bool,
    follow: &mut Follow,
    x: &Exclude,
) -> Result<(), String> {
    if l.cancelled() {
        return Err(session::CANCELLED.into());
    }
    // (not on Windows, where reading takes longer and listing a directory leaves its access
    // time, NTFS's, mostly as it was: there they are read with the rest, on threads)
    if cfg!(unix)
        && let Some(p) = parent
    {
        found[p].stat = Some(meta::stat(dir, privileged));
    }
    l.dir(dir);
    let err = |e: std::io::Error| format!("{}: {e}", dir.display());
    let mut names: Vec<(OsString, fs::FileType)> = fs::read_dir(dir)
        .map_err(err)?
        .map(|e| e.and_then(|e| Ok((e.file_name(), e.file_type()?))))
        .collect::<Result<_, _>>()
        .map_err(err)?;
    names.sort_by(|a, b| a.0.cmp(&b.0));
    if follow.on {
        follow.chain.push(fs::canonicalize(dir).map_err(err)?);
    }
    // (the path in the archive of what it holds, where paths are excluded)
    let mut at: Vec<OsString> = Vec::new();
    if !x.paths.is_empty() {
        let mut p = parent;
        while let Some(i) = p {
            at.push(found[i].name.clone());
            p = found[i].parent;
        }
        at.reverse();
    }
    for (name, t) in names {
        at.push(name.clone());
        let out = x.excludes(&at);
        at.pop();
        if out {
            continue;
        }
        let mut path = joined(dir, &name);
        let mut t = t;
        if follow.on && t.is_symlink() {
            let m = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some((to, m)) = follow.target(&path, &m)? {
                (path, t) = (to, m.file_type());
            }
        }
        found.push(Found { path, name, parent, dir: t.is_dir(), plain: false, stat: None });
        if t.is_file() {
            l.found();
        }
        if t.is_dir() {
            let at = found.len() - 1;
            let path = found[at].path.clone();
            walk(&path, Some(at), found, l, privileged, follow, x)?;
        }
    }
    follow.chain.pop();
    Ok(())
}

/// `dir` and `name` joined: where `name` is one Win32 reads as another (a trailing dot or space
/// taken off, a device's name such as `CON` or `NUL.txt`), the extended-length path (`\\?\`) it
/// takes as it is, as the entry could be listed but not opened else.
fn joined(dir: &Path, name: &OsStr) -> PathBuf {
    #[cfg(windows)]
    if win32_changes(name) {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let base = std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned());
        let wide: Vec<u16> = base.as_os_str().encode_wide().collect();
        let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
        let v: Vec<u16> = if wide.starts_with(&verbatim) {
            wide
        } else if wide.starts_with(&[b'\\' as u16, b'\\' as u16]) {
            [r"\\?\UNC\".encode_utf16().collect::<Vec<u16>>(), wide[2..].to_vec()].concat()
        } else {
            [verbatim, wide].concat()
        };
        return PathBuf::from(std::ffi::OsString::from_wide(&v)).join(name);
    }
    dir.join(name)
}

/// Whether Win32 takes `name` as another name than it is.
#[cfg(windows)]
fn win32_changes(name: &OsStr) -> bool {
    let n = name.to_string_lossy();
    if n.ends_with(['.', ' ']) && n != "." && n != ".." {
        return true;
    }
    let stem = n.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |p: &str| {
        stem.strip_prefix(p)
            .is_some_and(|d| matches!(d.chars().collect::<Vec<_>>()[..], ['1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}',]))
    };
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

/// What `-x` (`--exclude`) leaves out of the inputs, with what is under it: the entries of a
/// name (a pattern with no `/` but at its end), wherever they are, and of a path in the archive
/// (`/` between names; `.` and a `/` at its start or end left out). Given as they are: no
/// wildcards yet.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct Exclude {
    pub(crate) names: std::collections::HashSet<OsString>,
    pub(crate) paths: Vec<Vec<OsString>>,
}

impl Exclude {
    pub(crate) fn add(&mut self, p: &OsStr) -> Result<(), String> {
        let b = os_bytes(p);
        let b = b.strip_suffix(b"/").unwrap_or(&b);
        let names: Vec<OsString> =
            b.split(|&c| c == b'/').filter(|n| !n.is_empty() && *n != b".").map(os_from).collect();
        match names.len() {
            0 => return Err(format!("-x {}: no name", p.display())),
            1 if !b.contains(&b'/') => _ = self.names.insert(names.into_iter().next().unwrap()),
            _ => self.paths.push(names),
        }
        Ok(())
    }

    /// Whether the entry at path `names` in the archive is left out (what holds it not).
    pub(crate) fn excludes(&self, names: &[OsString]) -> bool {
        names.last().is_some_and(|n| self.names.contains(n)) || self.paths.iter().any(|p| p == names)
    }
}

/// Links followed (`-L`), where `on`: each archived as what it leads to, read by the path
/// that has no links in it ([`fs::canonicalize`]), so that what is read of it (metadata,
/// extended attributes, access times put back) is of that, not of the link. A link leading
/// nowhere, or to a directory it is in (which would be walked without end), is kept as a link
/// and told.
struct Follow {
    on: bool,
    /// The directories being walked, by such paths.
    chain: Vec<PathBuf>,
    warnings: Vec<String>,
}

impl Follow {
    /// What `path`, read (not followed) as `m`, leads to and its metadata, where it is a link
    /// to follow.
    pub(crate) fn target(&mut self, path: &Path, m: &fs::Metadata) -> Result<Option<(PathBuf, fs::Metadata)>, String> {
        if !self.on || !m.file_type().is_symlink() {
            return Ok(None);
        }
        let to = match fs::canonicalize(path).and_then(|to| Ok((fs::metadata(&to)?, to))) {
            Ok((m, to)) if m.is_dir() && self.chain.contains(&to) => {
                self.warnings.push(format!("{}: a link to a directory it is in, kept as a link", path.display()));
                return Ok(None);
            }
            Ok((m, to)) => (to, m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.warnings.push(format!("{}: a link leading nowhere, kept as a link", path.display()));
                return Ok(None);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Some(to))
    }
}

/// What input `path` leads to, where it is a link (`-L`): the path that has no links in it.
/// (A link kept is told by [`gather`], which takes it then.)
pub(crate) fn follow(path: &Path) -> Result<PathBuf, String> {
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut f = Follow { on: true, chain: Vec::new(), warnings: Vec::new() };
    Ok(f.target(path, &m)?.map_or_else(|| path.to_owned(), |(to, _)| to))
}

/// The entry `name` in `parent` (`None`: the root) of `path`, as `s` read it: its index, none
/// where it is skipped.
fn entry(
    path: &Path,
    name: &OsStr,
    s: &meta::Stat,
    parent: Option<usize>,
    g: &mut Gathered,
    l: &view::Listing,
) -> Result<Option<usize>, String> {
    let m = &s.m;
    if m.file_type().is_file()
        && let Some(itself) = &g.itself
        && itself.file_name() == Some(name)
        && fs::canonicalize(path).is_ok_and(|p| p == *itself)
    {
        g.skipped.push(format!("{} (the archive appended to)", path.display()));
        return Ok(None);
    }
    let name = name_from(&os_bytes(name));
    let (mut extra, forks) = g.reader.extra(path, s, &mut |w| g.warnings.push(w));
    let t = m.file_type();
    let inode = s.inode;
    let first = inode.and_then(|i| g.inodes.get(&i)).map(|f| f[0]);
    if let Some(first) = first {
        extra.forks = g.tree.entries[first].extra.forks.clone();
    } else {
        for f in forks {
            extra.forks.push((f.name, g.contents.len()));
            g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
            l.bytes(f.size);
        }
    }
    let node = if t.is_dir() {
        EntryNode::Dir
    } else if let Some(first) = first {
        g.tree.entries[first].node.clone()
    } else if t.is_file() {
        g.contents.push((path.to_owned(), os_bytes(path.as_os_str()), m.len()));
        EntryNode::File(g.contents.len() - 1)
    } else if let Some(node) = meta::special(m) {
        node
    } else if t.is_symlink() {
        let target = meta::link_target(fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let target = name_from(&os_bytes(target.as_os_str()));
        if name_bytes(&target).0.is_empty() {
            g.skipped.push(format!("{} (an empty link)", path.display()));
            return Ok(None);
        }
        EntryNode::Link(target)
    } else {
        g.skipped.push(format!("{} (a socket, or of a kind not known)", path.display()));
        return Ok(None);
    };
    if let Some(i) = inode {
        g.inodes.entry(i).or_default().push(g.tree.entries.len());
    }
    g.tree.entries.push(Entry { parent, name, node, extra });
    Ok(Some(g.tree.entries.len() - 1))
}
