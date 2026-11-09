//! An archive's entries taken away in place (`--delete ARCHIVE PATH...`): a segment of
//! tombstones appended and committed (spec, "Appending to an archive"), one at the top of each
//! path given, with the directories on the way (without their metadata, which keeps theirs).
//! What was taken away is only marked so: the file grows by the segment.
//!
//! The archive is opened as [`append::open`] opens it (locked, the bytes of an append not
//! committed made or cut off); an encrypted archive, one of a single file and one of a size hint
//! block are not edited. A path not in the archive is an error, before anything is written.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use aqr_formula::aqr::{self, Entry, EntryNode, Extra, SegmentWriter, Tree};

use crate::{Args, append, exit};

/// How an archive is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// `--delete`: the paths given taken away.
    Delete,
}

impl Edit {
    pub fn option(self) -> &'static str {
        match self {
            Edit::Delete => "--delete",
        }
    }
}

/// Edits the archive (the first input) as `a.edit` tells ([module](self)).
pub fn edit(a: &Args) -> Result<(), String> {
    let path = a.inputs.first().ok_or("no archive given")?;
    let sel = a.select.as_ref().expect("the paths of the archive");
    let start = Instant::now();
    crate::begin("delete", std::slice::from_ref(path), Some(path));
    let opened = append::open(path, a.quiet)?;
    let at = sel.find(opened.tree());
    if let Some(k) = at.iter().position(Option::is_none) {
        return Err(format!("{}: no {} in it", path.display(), sel.shown(k)));
    }
    let t = tombstones(opened.tree(), &at.iter().map(|e| e.unwrap()).collect::<Vec<_>>());
    let added = commit(a, path, opened.into_prior(), &t)?;
    if !a.quiet {
        let n = at.len();
        eprintln!(
            "{}: {n} path{} deleted (+{added} bytes; --compact takes them out of the file)",
            path.display(),
            if n == 1 { "" } else { "s" },
        );
    }
    crate::end(Some(path), 0, 0, added, start);
    Ok(())
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
