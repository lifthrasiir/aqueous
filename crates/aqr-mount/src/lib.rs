//! What the mounting programs share: a read-only [`Tree`] of nodes, which each program serves
//! through its system's interface (aqr-mount-win through WinFsp, aqr-mount-fuse, aqr-mac's).
//!
//! Nodes are named by a [`NodeId`], the root being [`ROOT`] (1, FUSE's root inode); names are
//! looked up one component at a time in a directory. That suits FUSE's inodes as it is and
//! WinFsp's paths walked component by component. Names are the archive's, UTF-8.
//!
//! Whether a lookup folds case is the caller's ([`Case`]): Windows asks for names in any case.
//! A tree whose names differ only in case resolves such a lookup to the one named exactly so,
//! else to the first in its order.
//!
//! [`ServedTree`] is the tree of an archive, as `aqr --serve` serves it; [`OneFile`] a tree of
//! one fixed file, for the tests.

use std::fmt;

mod served;

pub use served::ServedTree;

/// A node of a [`Tree`].
pub type NodeId = u64;

/// The root directory.
pub const ROOT: NodeId = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
}

/// What a node is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    pub kind: Kind,
    /// Bytes (0 for a directory).
    pub size: u64,
    /// The time of the last modification, in nanoseconds since 1970 (UTC).
    pub mtime_ns: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// Names as they are (POSIX).
    Sensitive,
    /// Names in any case (Windows), compared lowercased (Unicode's mapping).
    Insensitive,
}

#[derive(Debug)]
pub enum Error {
    NotFound,
    NotADirectory,
    IsADirectory,
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NotFound => f.write_str("no such file or directory"),
            Error::NotADirectory => f.write_str("not a directory"),
            Error::IsADirectory => f.write_str("is a directory"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One entry of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub node: NodeId,
}

/// A read-only tree. Called from many threads at once (the systems' dispatchers).
pub trait Tree: Send + Sync {
    fn attr(&self, node: NodeId) -> Result<Attr>;

    /// The node named `name` in the directory `dir`.
    fn lookup(&self, dir: NodeId, name: &str, case: Case) -> Result<NodeId>;

    /// The entries of `dir` in a stable order (neither `.` nor `..`).
    fn entries(&self, dir: NodeId) -> Result<Vec<Entry>>;

    /// Reads from `offset` into `buf`, all of it but at the end; 0 at and past the end.
    fn read(&self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// The name of the volume, where a system shows one.
    fn label(&self) -> String {
        "Aqueous".into()
    }
}

/// The node of a path of components split by `sep` (`\` on Windows), from the root.
pub fn lookup_path(tree: &dyn Tree, path: &str, sep: char, case: Case) -> Result<NodeId> {
    path.split(sep).filter(|c| !c.is_empty()).try_fold(ROOT, |dir, name| tree.lookup(dir, name, case))
}

/// Whether `a` names `b` under `case`.
pub fn same_name(a: &str, b: &str, case: Case) -> bool {
    match case {
        Case::Sensitive => a == b,
        Case::Insensitive => a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase)),
    }
}

/// A root holding one file of fixed contents: the tree the programs serve until archives do.
pub struct OneFile {
    pub name: String,
    pub data: Vec<u8>,
    pub mtime_ns: i64,
}

const FILE: NodeId = 2;

impl OneFile {
    /// `hello.txt` of a line, dated 2026-01-01 00:00 UTC.
    pub fn hello() -> OneFile {
        OneFile {
            name: "hello.txt".into(),
            data: b"Hello from Aqueous, through a mount.\n".to_vec(),
            mtime_ns: 1_767_225_600 * 1_000_000_000,
        }
    }
}

impl Tree for OneFile {
    fn attr(&self, node: NodeId) -> Result<Attr> {
        match node {
            ROOT => Ok(Attr { kind: Kind::Dir, size: 0, mtime_ns: self.mtime_ns }),
            FILE => Ok(Attr { kind: Kind::File, size: self.data.len() as u64, mtime_ns: self.mtime_ns }),
            _ => Err(Error::NotFound),
        }
    }

    fn lookup(&self, dir: NodeId, name: &str, case: Case) -> Result<NodeId> {
        match dir {
            ROOT if same_name(name, &self.name, case) => Ok(FILE),
            ROOT => Err(Error::NotFound),
            FILE => Err(Error::NotADirectory),
            _ => Err(Error::NotFound),
        }
    }

    fn entries(&self, dir: NodeId) -> Result<Vec<Entry>> {
        match dir {
            ROOT => Ok(vec![Entry { name: self.name.clone(), node: FILE }]),
            FILE => Err(Error::NotADirectory),
            _ => Err(Error::NotFound),
        }
    }

    fn read(&self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize> {
        match file {
            FILE => {
                let rest = usize::try_from(offset).ok().and_then(|o| self.data.get(o..)).unwrap_or_default();
                let n = rest.len().min(buf.len());
                buf[..n].copy_from_slice(&rest[..n]);
                Ok(n)
            }
            ROOT => Err(Error::IsADirectory),
            _ => Err(Error::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file() {
        let tree = OneFile::hello();
        let file = lookup_path(&tree, "\\HELLO.txt", '\\', Case::Insensitive).unwrap();
        assert!(matches!(lookup_path(&tree, "/HELLO.txt", '/', Case::Sensitive), Err(Error::NotFound)));
        assert_eq!(lookup_path(&tree, "/hello.txt", '/', Case::Sensitive).unwrap(), file);
        assert_eq!(lookup_path(&tree, "\\", '\\', Case::Insensitive).unwrap(), ROOT);
        assert!(matches!(lookup_path(&tree, "/hello.txt/x", '/', Case::Sensitive), Err(Error::NotADirectory)));
        assert_eq!(tree.entries(ROOT).unwrap(), vec![Entry { name: "hello.txt".into(), node: file }]);
        let mut buf = [0; 5];
        assert_eq!(tree.read(file, 6, &mut buf).unwrap(), 5);
        assert_eq!(&buf, b"from ");
        assert_eq!(tree.read(file, tree.attr(file).unwrap().size, &mut buf).unwrap(), 0);
        assert_eq!(tree.read(file, u64::MAX, &mut buf).unwrap(), 0);
    }

    #[test]
    fn folding() {
        assert!(!same_name("a.txt", "b.txt", Case::Insensitive));
        assert!(same_name("ÄBC", "äbc", Case::Insensitive));
        assert!(!same_name("ÄBC", "äbc", Case::Sensitive));
    }
}
