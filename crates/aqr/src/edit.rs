//! An archive edited in place ([`edit`]):
//!
//! - `--delete ARCHIVE PATH...`: the paths given taken out, by a segment of tombstones appended
//!   and committed (spec, "Appending to an archive"), one at the top of each path, with the
//!   directories on the way (without their metadata, which keeps theirs). What was taken out is
//!   only marked so: the file grows by the segment.
//! - `--compact ARCHIVE` (alone, or with `--delete`, the paths then taken out of the tree): the
//!   archive written again of what its tree holds ([`aqr::rewrite`]), every block it still needs
//!   copied as it is, segment by segment; what an append not committed, tombstones and entries
//!   replaced left in the file taken out of it. It is written beside the archive
//!   (`ARCHIVE.PID.part`), opened again and its tree checked against the one meant, then put in
//!   its place by a rename: the archive as it was or as edited, whatever happens meanwhile.
//!   Bytes before the archive in the file (a stub) are kept.
//!
//! The archive is opened as [`append::open`] opens it (locked, the bytes of an append not
//! committed made or cut off); an encrypted archive, one of a single file and one of a size hint
//! block are not edited. A path not in the archive is an error, before anything is written.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aqr_formula::aqr::{self, Entry, EntryNode, Extra, SegmentWriter, Tree};

use crate::{Args, append, exit, stop};

/// How an archive is edited, besides `--compact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// `--delete`: the paths given taken out.
    Delete,
}

impl Edit {
    pub fn option(self) -> &'static str {
        match self {
            Edit::Delete => "--delete",
        }
    }
}

/// Edits the archive (the first input) as `a.edit` and `a.compact` tell ([module](self)).
pub fn edit(a: &Args) -> Result<(), String> {
    let path = a.inputs.first().ok_or("no archive given")?;
    let start = Instant::now();
    crate::begin(if a.compact { "compact" } else { "delete" }, std::slice::from_ref(path), Some(path));
    let opened = append::open(path, a.quiet)?;
    let gone = match &a.select {
        Some(sel) => {
            let at = sel.find(opened.tree());
            if let Some(k) = at.iter().position(Option::is_none) {
                return Err(format!("{}: no {} in it", path.display(), sel.shown(k)));
            }
            at.into_iter().map(Option::unwrap).collect()
        }
        None => Vec::new(),
    };
    let told = |what: String| {
        if !a.quiet {
            eprintln!("{}: {what}", path.display());
        }
    };
    let paths = |n: usize| format!("{n} path{} deleted", if n == 1 { "" } else { "s" });
    if !a.compact {
        let t = tombstones(opened.tree(), &gone);
        let added = commit(a, path, opened.into_prior(), &t)?;
        told(format!("{} (+{added} bytes; --compact takes them out of the file)", paths(gone.len())));
        crate::end(Some(path), 0, 0, added, start);
        return Ok(());
    }
    let t = without(opened.tree(), &gone);
    let (before, r) = compact(a, path, &opened, &t)?;
    let mut what = format!("{} -> {} bytes", before, r.len);
    if r.segments.0 != r.segments.1 {
        what += &format!(", {} -> {} segments", r.segments.1, r.segments.0);
    }
    if !gone.is_empty() {
        what = format!("{}, {what}", paths(gone.len()));
    }
    told(what);
    if a.verbose {
        told(format!(
            "{} leaf and {} non-leaf blocks left out, {} formula blocks made again",
            r.leaves_left, r.non_leaf_left, r.formulas_made
        ));
    }
    crate::end(Some(path), 0, before, r.len, start);
    Ok(())
}

/// Archive `path`, opened as `o`, written again of tree `t` (its own, edited) and put in its
/// place ([module](self)): the bytes of the archive before, and what was written.
fn compact(a: &Args, path: &Path, o: &append::Opened, t: &Tree) -> Result<(u64, aqr::Rewritten), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let io = |e: std::io::Error| err(&exit::seen(e));
    // (a link to the archive: the archive written again where it is)
    let path = &fs::canonicalize(path).map_err(io)?;
    let p = o.prior();
    let mut part = path.file_name().unwrap_or_default().to_owned();
    part.push(format!(".{}.part", std::process::id()));
    let part = path.with_file_name(part);
    let (file, _guard) = stop::create(&part, || fs::OpenOptions::new().write(true).create_new(true).open(&part))
        .map_err(|e| format!("{}: {e}", part.display()))?;
    let r = (|| {
        let perr = |e: &dyn std::fmt::Display| format!("{}: {e}", part.display());
        // SAFETY: the archive is locked against other writers; where it changes all the same,
        // its checksums tell
        let map = unsafe { memmap2::Mmap::map(&p.file) }.map_err(io)?;
        let (lead, c) = (&map[..p.at.start as usize], &map[p.at.start as usize..p.at.end as usize]);
        let mut w = std::io::BufWriter::with_capacity(8 << 20, &file);
        w.write_all(lead).map_err(|e| perr(&e))?;
        let r = aqr::rewrite(c, t, a.opt.level, a.opt.threads, &mut w).map_err(|e| err(&e))?;
        w.flush().map_err(|e| perr(&e))?;
        drop(w);
        crate::sync_file(&file).map_err(|e| perr(&e))?;
        let lead = lead.len();
        drop(map);
        checked(&part, lead, t, o.sizes()).map_err(|e| perr(&e))?;
        let perms = p.file.metadata().map_err(io)?.permissions();
        fs::set_permissions(&part, perms).map_err(|e| perr(&e))?;
        Ok((p.at.end - p.at.start, r))
    })();
    drop(file);
    match r {
        Ok(r) => {
            crate::place(&part, path, true)?;
            Ok(r)
        }
        Err(e) => {
            _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Whether archive `part` (after `lead` bytes) holds tree `t` (its contents of `sizes`), as it
/// is opened: its paths, their kinds and metadata, and the bytes of each content.
fn checked(part: &PathBuf, lead: usize, t: &Tree, sizes: &[u64]) -> Result<(), String> {
    let file = fs::File::open(part).map_err(|e| e.to_string())?;
    // SAFETY: a file of this process, not written to any more
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
    let archive = aqr::open(&c[lead..], &aqr::NoModules).map_err(|e| e.to_string())?;
    let new = archive.tree().ok_or("no tree")?;
    let size = |k: usize| archive.content_size(k).unwrap_or(u64::MAX);
    if shape(new, &size) != shape(t, &|k| sizes.get(k).copied().unwrap_or(u64::MAX)) {
        return Err("the archive written again does not hold the tree meant (left as it was)".into());
    }
    Ok(())
}

/// Tree `t` as it is told apart: each entry by its path (sorted), its contents by their bytes.
fn shape(t: &Tree, size: &dyn Fn(usize) -> u64) -> Vec<(Vec<Vec<u8>>, EntryNode, Extra)> {
    let paths = append::paths(t);
    let mut out: Vec<_> = std::iter::once((Vec::new(), EntryNode::Dir, t.root.clone()))
        .chain(t.entries.iter().zip(paths).map(|(e, p)| (p, e.node.clone(), e.extra.clone())))
        .map(|(p, mut node, mut extra)| {
            if let EntryNode::File(k) = &mut node {
                *k = size(*k) as usize;
            }
            extra.forks.iter_mut().for_each(|f| f.1 = size(f.1) as usize);
            (p, node, extra)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Tree `t` without entries `gone` and what is under them.
fn without(t: &Tree, gone: &[usize]) -> Tree {
    let mut index: Vec<Option<usize>> = vec![None; t.entries.len()];
    let mut out = Tree { root: t.root.clone(), entries: Vec::new() };
    let mut dropped = vec![false; t.entries.len()];
    gone.iter().for_each(|&g| dropped[g] = true);
    for (k, e) in t.entries.iter().enumerate() {
        if dropped[k] || e.parent.is_some_and(|p| dropped[p]) {
            dropped[k] = true;
            continue;
        }
        index[k] = Some(out.entries.len());
        out.entries.push(Entry { parent: e.parent.map(|p| index[p].unwrap()), ..e.clone() });
    }
    out
}

/// The tree of a segment taking away entries `gone` of tree `t`: a tombstone at each, after the
/// directories on the way (of no extra, which keeps theirs).
fn tombstones(t: &Tree, gone: &[usize]) -> Tree {
    let mut entries: Vec<Entry> = Vec::new();
    // (by the entry of `t`, its directory in the segment)
    let mut dirs: HashMap<usize, usize> = HashMap::new();
    for &g in gone {
        let mut up = Vec::new();
        let mut p = t.entries[g].parent;
        while let Some(q) = p {
            up.push(q);
            p = t.entries[q].parent;
        }
        let mut parent = None;
        for &q in up.iter().rev() {
            let at = *dirs.entry(q).or_insert_with(|| {
                entries.push(Entry {
                    parent,
                    name: t.entries[q].name.clone(),
                    node: EntryNode::Dir,
                    extra: Extra::default(),
                });
                entries.len() - 1
            });
            parent = Some(at);
        }
        let name = t.entries[g].name.clone();
        entries.push(Entry { parent, name, node: EntryNode::Special(0), extra: Extra::default() });
    }
    Tree { root: Extra::default(), entries }
}

/// Appends a segment of tree `t` (of no contents) to archive `path` as `p` found it, and commits
/// it: the bytes the file grew by.
fn commit(a: &Args, path: &Path, p: append::Prior, t: &Tree) -> Result<u64, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let io = |e: std::io::Error| err(&exit::seen(e));
    let file = &p.file;
    let enc = aqr::Encoder::with_threads(a.opt.level, a.opt.threads);
    let mut w = match p.kill {
        true => SegmentWriter::appending(p.footer.clone(), p.metadata),
        false => {
            // (a footer across a sector of the file written over, from where it starts)
            file.set_len(p.at.start + p.footer.start as u64).map_err(io)?;
            SegmentWriter::replacing(p.footer.start, p.metadata)
        }
    };
    // (stopped before the commit is whole, the bytes written are cut off by the next append, or
    // the commit completed by it: nothing to undo)
    let mut out = w.pad().to_vec();
    out.extend(w.close_as(&enc, t, &[], aqr::Status::Complete).map_err(|e| err(&e))?);
    let (footer, before) = w.footer(p.at.start);
    out.extend(footer);
    let len = file.metadata().map_err(io)?.len();
    crate::write_at(file, len, &out).map_err(io)?;
    crate::sync_file(file).map_err(io)?;
    if let Some(f) = before {
        let k = aqr::kill(f.len());
        crate::write_at(file, p.at.start + f.start as u64, &k).map_err(io)?;
        crate::sync_file(file).map_err(io)?;
        crate::write_at(file, p.at.start + (f.start + k.len()) as u64, &vec![0; f.len() - k.len()]).map_err(io)?;
    }
    Ok(file.metadata().map_err(io)?.len() - p.at.end)
}
