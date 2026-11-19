//! Appending to an archive (`-a`; spec, "Appending to an archive"): what the inputs hold that the
//! archive does not, as a tree of segments to append ([`open`], [`Opened::against`]).
//!
//! The archive is opened to be written, locked against another writer, and located in its file
//! (after a stub, say). Bytes after it are those of an append not committed, cut off (even where
//! they end with a commit written whole: it was not committed); bytes after it that are not
//! blocks are not taken away, and the archive is not appended to.
//! An encrypted archive is not appended to (its nonces would be used again), nor one of mirror
//! blocks (what is appended would go unmirrored), nor one of a single file, nor one of a size
//! hint block (to be written over).
//!
//! The entries of the inputs are compared with the archive's tree, merged, by path: a file is
//! taken again where it is not there, is there of another kind, size or modification time (as
//! rsync's quick check has it); a link where its target differs; a directory where it is not
//! there. The directories on the way to those taken are taken too, without their metadata where
//! they are there already (which keeps it).
//!
//! Synchronizing (`-s`), what the archive holds and the inputs do not is taken away too (a
//! tombstone at the top of each such subtree), so that the archive holds what `-c` of the inputs
//! would, as the quick check sees it. Its file still grows: what is taken away is only marked so.

use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::Path;

use aqr_archive::{Entry, EntryNode, Extra, Tree};

use crate::Gathered;

/// The archive appended to, as it is in its file.
pub struct Prior {
    pub file: fs::File,
    /// Where it is in the file, and where its footer and last metadata block are (from its start).
    pub at: Range<u64>,
    pub footer: Range<usize>,
    pub metadata: usize,
    /// The least archive-wide link group of its tree (0: none).
    pub link_groups: i128,
    /// Whether its footer can be made a spacer block in place (else written over: the file cut
    /// there already).
    pub kill: bool,
}

/// The archive appended to, opened before the inputs are gathered (so that an archive not to be
/// appended to is told at once), with what it holds.
pub struct Opened {
    prior: Prior,
    old: Tree,
    sizes: Vec<u64>,
}

/// Archive `path` opened to be appended to: locked, located, the bytes after it of an append not
/// committed cut off ([module](self)).
pub fn open(path: &Path, quiet: bool) -> Result<Opened, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = fs::OpenOptions::new().read(true).write(true).open(path).map_err(|e| err(&e))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(err(&"being written by another")),
        Err(fs::TryLockError::Error(e)) => return Err(err(&e)),
    }
    // (the file cut where an append not committed is cut off: once it is no longer mapped, as
    // Windows does not cut a file mapped)
    let mut cut = None;
    let (at, footer, metadata, kill, old, sizes) = {
        // SAFETY: the archive is locked against other writers; where it changes all the same, its
        // checksums tell
        let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
        if aqr_archive::envelope::is_encrypted(&c) || aqr_archive::envelope::is_damaged_encrypted(&c) {
            return Err(err(&"an encrypted archive is not appended to (its nonces would be used again)"));
        }
        let at = aqr_archive::locate(&c).ok_or_else(|| err(&"no archive found in it (aqr -K tells more)"))?;
        if at.end < c.len() {
            cut = Some(tail(&c, at.clone(), path)?);
        }
        let a = &c[at.clone()];
        let (footer, metadata) = aqr_archive::footer_of(a).ok_or_else(|| err(&"no footer"))?;
        let metadata = metadata.ok_or_else(|| err(&"an archive of a single file, not of paths"))?;
        // (what is written after the mirror blocks would be unmirrored, or the footer's mirror kept stale)
        let m = aqr_archive::mirror::check(a);
        if !m.mirrors.is_empty() || m.broken > 0 {
            return Err(err(&"an archive of mirror blocks (-A) is not appended to nor edited in place"));
        }
        if aqr_archive::block_at(a, aqr_archive::HEADER_MAGIC.len()).is_some_and(|(ty, _)| ty == 4) {
            return Err(err(&"an archive of a size hint block is not appended to"));
        }
        let archive = aqr_archive::open(a, &aqr_archive::NoModules).map_err(|e| err(&e))?;
        let old = archive.tree().cloned().ok_or_else(|| err(&"an archive of a single file, not of paths"))?;
        let sizes: Vec<u64> = (0..archive.contents_len()).map(|c| archive.content_size(c).unwrap_or(0)).collect();
        let kill = !aqr_archive::crossing(at.start as u64 + footer.start as u64, aqr_archive::kill(footer.len()).len());
        ((at.start as u64)..(at.end as u64), footer, metadata, kill, old, sizes)
    };
    if let Some(n) = cut {
        file.set_len(at.end).map_err(|e| err(&e))?;
        crate::sync_file(&file).map_err(|e| err(&e))?;
        if !quiet {
            eprintln!("{}: an append not committed cut off ({n} bytes)", path.display());
        }
    }
    let link_groups = old.entries.iter().filter_map(|e| e.extra.link_group).min().unwrap_or(0).min(0);
    Ok(Opened { prior: Prior { file, at, footer, metadata, link_groups, kill }, old, sizes })
}

impl Opened {
    /// Of `g` (gathered of the inputs; `sync`: synchronizing): `g` left as what is to be appended
    /// ([module](self)), the archive, and whether anything is to be (else the archive is left as
    /// it is).
    pub fn against(self, g: &mut Gathered, sync: bool) -> (Prior, bool) {
        let any = diff(g, &self.old, &self.sizes, sync);
        (self.prior, any)
    }

    /// The archive's tree, and the bytes of each of its contents.
    pub fn tree(&self) -> &Tree {
        &self.old
    }

    pub fn sizes(&self) -> &[u64] {
        &self.sizes
    }

    /// The archive as it is in its file.
    pub fn prior(&self) -> &Prior {
        &self.prior
    }

    /// The archive, to be appended to as it is.
    pub fn into_prior(self) -> Prior {
        self.prior
    }
}

/// The bytes after the archive `at` of file `c`, where some follow it: those of an append not
/// committed, to be cut off (which the caller does once `c` is not mapped), how many; where they
/// are not blocks, an error.
///
/// An append whose commit was written whole but for its footer before made a spacer block is cut
/// off too, not made: until then the file was the archive before (what a reader saw of it), and
/// what was not committed is not to be seen afterwards (spec, "Appending to an archive": the
/// trailing bytes removed).
fn tail(c: &[u8], at: Range<usize>, path: &Path) -> Result<usize, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    // (blocks after the footer, and its padding byte, as an append writes them)
    let mut e = at.end + (at.end - at.start) % 2;
    let ours = e >= c.len() || aqr_archive::block_at(c, e).is_some() || c.len() - e < 16;
    while let Some((_, end)) = aqr_archive::block_at(c, e) {
        e = end;
    }
    if !ours {
        return Err(err(&"bytes after the archive not of an append: not appended to"));
    }
    Ok(c.len() - at.end)
}

/// The path of each entry of `t`, by its names.
pub fn paths(t: &Tree) -> Vec<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let mut p = e.parent.map_or_else(Vec::new, |q| out[q].clone());
        p.push(e.name.clone());
        out.push(p);
    }
    out
}

/// `g` left as what is to be appended to the archive of tree `old` (its contents of `sizes`):
/// whether anything is ([module](self)).
fn diff(g: &mut Gathered, old: &Tree, sizes: &[u64], sync: bool) -> bool {
    let (new_paths, old_paths) = (paths(&g.tree), paths(old));
    let was: HashMap<&[Vec<u8>], usize> = old_paths.iter().enumerate().map(|(k, p)| (&p[..], k)).collect();
    let is: HashMap<&[Vec<u8>], usize> = new_paths.iter().enumerate().map(|(k, p)| (&p[..], k)).collect();
    let n = g.tree.entries.len();
    let mut taken = vec![false; n];
    for (k, e) in g.tree.entries.iter().enumerate() {
        let before = was.get(&new_paths[k][..]).map(|&o| &old.entries[o]);
        taken[k] = match (&e.node, before.map(|b| &b.node)) {
            (_, None) => true,
            (&EntryNode::File(c), Some(&EntryNode::File(o))) => {
                g.contents[c].2 != sizes.get(o).copied().unwrap_or(u64::MAX)
                    || e.extra.mtime != before.unwrap().extra.mtime
            }
            (EntryNode::Dir, Some(EntryNode::Dir)) => false,
            (a, Some(b)) => a != b,
        };
    }
    // the directories on the way, and whether each is taken for itself
    let own = taken.clone();
    for k in (0..n).rev() {
        if taken[k]
            && let Some(p) = g.tree.entries[k].parent
        {
            taken[p] = true;
        }
    }
    // synchronizing, what the archive holds and the inputs do not, at the top of each such
    // subtree (the directory it is in still a directory; else it is taken away or replaced)
    let mut gone: Vec<(Option<usize>, Vec<u8>)> = Vec::new();
    if sync {
        for (o, p) in old_paths.iter().enumerate() {
            if is.contains_key(&p[..]) {
                continue;
            }
            let up = match old.entries[o].parent {
                None => None,
                Some(q) => match is.get(&old_paths[q][..]) {
                    Some(&k) if matches!(g.tree.entries[k].node, EntryNode::Dir) => Some(k),
                    _ => continue,
                },
            };
            if let Some(k) = up {
                taken[k] = true;
                let mut q = g.tree.entries[k].parent;
                while let Some(j) = q {
                    taken[j] = true;
                    q = g.tree.entries[j].parent;
                }
            }
            gone.push((up, old.entries[o].name.clone()));
        }
    }
    if !taken.iter().any(|&t| t) && gone.is_empty() {
        return false;
    }
    // the tree of what is taken, its contents numbered anew
    let mut contents = Vec::new();
    let mut number: HashMap<usize, usize> = HashMap::new();
    let mut content = |c: usize, contents: &mut Vec<(std::path::PathBuf, Vec<u8>, u64)>| {
        *number.entry(c).or_insert_with(|| {
            contents.push(g.contents[c].clone());
            contents.len() - 1
        })
    };
    let mut at = vec![None; n];
    let mut entries: Vec<Entry> = Vec::new();
    for k in (0..n).filter(|&k| taken[k]) {
        let e = &g.tree.entries[k];
        // (a directory there already, taken on the way: its metadata kept)
        let keep = !own[k] && was.contains_key(&new_paths[k][..]);
        let mut extra = if keep { Extra::default() } else { e.extra.clone() };
        extra.forks.iter_mut().for_each(|f| f.1 = content(f.1, &mut contents));
        let node = match e.node {
            EntryNode::File(c) => EntryNode::File(content(c, &mut contents)),
            ref other => other.clone(),
        };
        at[k] = Some(entries.len());
        let parent = e.parent.map(|p| at[p].expect("a directory on the way taken"));
        entries.push(Entry { parent, name: e.name.clone(), node, extra });
    }
    for (up, name) in gone {
        let parent = up.map(|k| at[k].expect("a directory taken"));
        entries.push(Entry::tombstone(parent, name));
    }
    let mut root = g.tree.root.clone();
    root.forks.iter_mut().for_each(|f| f.1 = content(f.1, &mut contents));
    g.tree = Tree { root, entries };
    g.contents = contents;
    true
}
