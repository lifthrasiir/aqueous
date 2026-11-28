//! A [`Tree`] of an archive, as `aqr --serve` serves it (`crates/aqr/src/serve.rs`; the client
//! is aqr-ipc's): what aqr reads (Aqueous archives, and the other formats of a tree, the
//! compressed single files) is mounted as aqr-win's shell extension browses it, and what
//! decodes is in a process of its own (aqr's), not the mounting program.
//!
//! Directories are listed as they are first looked in, and their entries given node IDs then,
//! in the order listed; nothing is ever forgotten, the archive not changing. Links and the
//! entries of no kind known here are left out for now. Reads go to `read` in pieces of at most
//! [`CHUNK`]: the first of a file evaluates its leaf, which `aqr --serve` keeps for the others.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};

use aqr_ipc::serve::{CHUNK, Served};
use serde_json::{Value, json};

use crate::{Attr, Case, Entry, Error, Kind, NodeId, Result, Tree, same_name};

struct Node {
    /// From the root, joined with `/` (empty for the root).
    path: String,
    attr: Attr,
}

#[derive(Default)]
struct Nodes {
    /// By node ID less 1.
    all: Vec<Node>,
    /// The entries of the directories listed.
    listed: HashMap<NodeId, Vec<Entry>>,
}

/// An archive open in `aqr --serve`.
pub struct ServedTree {
    served: Served,
    handle: u64,
    label: String,
    nodes: RwLock<Nodes>,
    /// Held while a directory is listed, so that two lookups at once list it once.
    listing: Mutex<()>,
}

fn failed(e: aqr_ipc::serve::Error) -> Error {
    Error::Io(std::io::Error::other(e.to_string()))
}

/// What the entry `v` (of `list`) is, where it is of a kind served.
fn attr_of(v: &Value) -> Option<Attr> {
    let kind = match v["kind"].as_str()? {
        "dir" => Kind::Dir,
        "file" => Kind::File,
        _ => return None,
    };
    let mtime_ns = match &v["mtime"] {
        Value::Array(t) => t[0].as_i64().unwrap_or(0) * 1_000_000_000 + t[1].as_i64().unwrap_or(0),
        _ => 0,
    };
    Some(Attr { kind, size: v["size"].as_u64().unwrap_or(0), mtime_ns })
}

impl ServedTree {
    /// `archive` opened in an `aqr --serve` of the program at `aqr` (started here, ended as the
    /// tree is dropped). An encrypted archive is an error for now. The errors name the path
    /// (`aqr`'s, or the archive's) themselves.
    pub fn open(aqr: &Path, archive: &Path) -> Result<ServedTree> {
        let served = Served::start(aqr, &[])
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{}: {e}", aqr.display()))))?;
        let opened = served.send(json!({"open": archive})).map_err(failed)?.recv();
        let (v, _) = opened.map_err(|_| failed(aqr_ipc::serve::Error::Gone))?;
        if v["encrypted"] == true {
            return Err(Error::Io(std::io::Error::other(format!(
                "{}: encrypted (not mounted yet)",
                archive.display()
            ))));
        }
        if let Some(e) = v["error"].as_str() {
            return Err(Error::Io(std::io::Error::other(e.to_owned())));
        }
        let handle = v["archive"].as_u64().ok_or_else(|| Error::Io(std::io::Error::other("no handle opened")))?;
        // (the root has no time of its own: the archive's)
        let mtime_ns = std::fs::metadata(archive)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        let root = Node { path: String::new(), attr: Attr { kind: Kind::Dir, size: 0, mtime_ns } };
        let label = archive.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        Ok(ServedTree {
            served,
            handle,
            label,
            nodes: RwLock::new(Nodes { all: vec![root], ..Nodes::default() }),
            listing: Mutex::new(()),
        })
    }

    fn node<T>(&self, id: NodeId, f: impl FnOnce(&Node) -> T) -> Result<T> {
        let nodes = self.nodes.read().unwrap();
        let i = id.checked_sub(1).ok_or(Error::NotFound)? as usize;
        nodes.all.get(i).map(f).ok_or(Error::NotFound)
    }

    /// The entries of directory `dir`, listed where they are not yet.
    fn listed(&self, dir: NodeId) -> Result<Vec<Entry>> {
        if let Some(e) = self.nodes.read().unwrap().listed.get(&dir) {
            return Ok(e.clone());
        }
        let (path, kind) = self.node(dir, |n| (n.path.clone(), n.attr.kind))?;
        if kind != Kind::Dir {
            return Err(Error::NotADirectory);
        }
        let _listing = self.listing.lock().unwrap();
        // (listed by another while this one waited)
        if let Some(e) = self.nodes.read().unwrap().listed.get(&dir) {
            return Ok(e.clone());
        }
        let got = self.served.list(self.handle, &path).map_err(failed)?;
        let mut nodes = self.nodes.write().unwrap();
        let mut entries = Vec::with_capacity(got.len());
        for v in &got {
            let (Some(name), Some(attr)) = (v["name"].as_str(), attr_of(v)) else { continue };
            let path = if path.is_empty() { name.to_owned() } else { format!("{path}/{name}") };
            nodes.all.push(Node { path, attr });
            entries.push(Entry { name: name.to_owned(), node: nodes.all.len() as NodeId });
        }
        nodes.listed.insert(dir, entries.clone());
        Ok(entries)
    }
}

impl Tree for ServedTree {
    fn attr(&self, node: NodeId) -> Result<Attr> {
        self.node(node, |n| n.attr.clone())
    }

    fn lookup(&self, dir: NodeId, name: &str, case: Case) -> Result<NodeId> {
        let entries = self.listed(dir)?;
        // (the name as it is first: of two differing in case alone, the one named so)
        let exact = entries.iter().find(|e| e.name == name);
        exact.or_else(|| entries.iter().find(|e| same_name(&e.name, name, case))).map(|e| e.node).ok_or(Error::NotFound)
    }

    fn entries(&self, dir: NodeId) -> Result<Vec<Entry>> {
        self.listed(dir)
    }

    fn read(&self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let (path, attr) = self.node(file, |n| (n.path.clone(), n.attr.clone()))?;
        if attr.kind == Kind::Dir {
            return Err(Error::IsADirectory);
        }
        let mut have = 0;
        while have < buf.len() {
            let want = (buf.len() - have).min(CHUNK);
            let d = self.served.read(self.handle, &path, offset + have as u64, want).map_err(failed)?;
            if d.is_empty() {
                break;
            }
            let n = d.len().min(buf.len() - have);
            buf[have..have + n].copy_from_slice(&d[..n]);
            have += n;
        }
        Ok(have)
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

impl Drop for ServedTree {
    fn drop(&mut self) {
        _ = self.served.ask(json!({"close": self.handle}));
    }
}
