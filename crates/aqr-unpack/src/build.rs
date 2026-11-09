//! A [`Tree`] built of the paths an archive gives its members: the directories on the way made
//! where the archive has none, a path given again taking the place of the one before (a later
//! member of a tar archive updating an earlier one), and what cannot be extracted safely left
//! out and told.

use std::collections::{HashMap, HashSet};

use aqr_foreign::charset::{Charset, Choice, Decision, decide};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree, name_bytes, name_from};

/// A path as an archive gives it: in an encoding it declares, as UTF-8, or in none, as it is
/// (`docs/names-design.md`).
#[derive(Clone, Debug)]
pub enum Named {
    Text(Vec<u8>),
    Bytes(Vec<u8>),
}

impl Named {
    /// Path `b`, declared UTF-8: of no declared encoding where it is not UTF-8 (declared wrongly).
    pub fn utf8(b: Vec<u8>) -> Named {
        match std::str::from_utf8(&b) {
            Ok(_) => Named::Text(b),
            Err(_) => Named::Bytes(b),
        }
    }

    /// Path `b`, declared of `cs`: of no declared encoding where it is not of it.
    pub fn of(cs: Charset, b: Vec<u8>) -> Named {
        match cs.decode(&b) {
            Some(s) => Named::Text(s.into_bytes()),
            None => Named::Bytes(b),
        }
    }

    /// The path where it is of no declared encoding.
    pub fn undeclared(&self) -> Option<&[u8]> {
        match self {
            Named::Text(_) => None,
            Named::Bytes(b) => Some(b),
        }
    }
}

/// How the paths of no declared encoding of an archive are taken ([`Decision`]), and how many
/// there are.
#[derive(Clone, Debug, Default)]
pub struct Undeclared {
    pub d: Decision,
    pub n: usize,
}

impl Undeclared {
    /// The paths of no declared encoding of an archive, `paths`, decided as `choice` asks:
    /// decoded where they can be, guessed where not given. Where they are not all UTF-8,
    /// `convention`, the set the format's archivers write (ALZip's CP949), is taken where it
    /// decodes them all.
    pub fn decide<'a>(
        choice: &Choice,
        paths: impl IntoIterator<Item = &'a [u8]>,
        convention: Option<Charset>,
    ) -> Undeclared {
        let paths: Vec<&[u8]> = paths.into_iter().collect();
        if paths.is_empty() {
            return Undeclared::default();
        }
        let utf8 = || paths.iter().all(|p| std::str::from_utf8(p).is_ok());
        let d = match (choice, convention) {
            (Choice::Auto, Some(cs)) if !utf8() && paths.iter().all(|p| cs.decode(p).is_some()) => Decision::of(cs),
            _ => decide(choice, &paths, true, Charset::system(true), convention.or_else(Charset::likely)),
        };
        Undeclared { d, n: paths.len() }
    }

    /// Path `p` with its names separated by `/` alone: decoded where it is of no declared
    /// encoding and that is decided (else as it is), and where `backslash`, its `\`s made `/`s
    /// as characters, not as bytes (5C is a byte of characters of Shift_JIS, GBK and Big5).
    pub fn path(&self, p: &Named, backslash: bool) -> Vec<u8> {
        let text = match p {
            Named::Text(t) => Some(t.clone()),
            Named::Bytes(b) => self.d.decode(b).map(String::into_bytes),
        };
        match (text, p) {
            (Some(mut t), _) => {
                if backslash {
                    t.iter_mut().filter(|c| **c == b'\\').for_each(|c| *c = b'/');
                }
                t
            }
            (None, Named::Text(_)) => unreachable!(),
            (None, Named::Bytes(b)) => {
                let mut out = b.clone();
                if backslash {
                    for (i, c) in out.iter_mut().enumerate() {
                        if *c == b'\\' && self.d.starts(b, i) {
                            *c = b'/';
                        }
                    }
                }
                out
            }
        }
    }

    /// Link target `t` of an entry, of the encoding of the entry's path (none where
    /// `undeclared`): decoded as [`Undeclared::path`] decodes, else as it is.
    pub fn target(&self, t: &[u8], undeclared: bool) -> Vec<u8> {
        match undeclared.then(|| self.d.decode(t)).flatten() {
            Some(s) => s.into_bytes(),
            None => t.to_vec(),
        }
    }

    /// What is told of the decision, where anything is (of archive format `what`).
    pub fn told(&self, what: &str) -> Option<String> {
        self.d.told(self.n).map(|t| format!("{what}: {t}"))
    }
}

/// The names of `path` (separated by `/`, and `\` too where `backslash`): `None` where it has a
/// `..` (it could reach out of the directory) or a zero byte. A `/` or a drive (`C:`) at the
/// start, empty names and `.` are left out.
pub fn split(path: &[u8], backslash: bool) -> Option<Vec<&[u8]>> {
    let sep = |b: &u8| *b == b'/' || backslash && *b == b'\\';
    let mut names: Vec<&[u8]> = path.split(sep).filter(|n| !n.is_empty() && *n != b".").collect();
    if let Some(first) = names.first()
        && first.len() == 2
        && first[1] == b':'
        && first[0].is_ascii_alphabetic()
        && (path.len() == 2 || sep(&path[2]))
    {
        names.remove(0);
    }
    (!names.iter().any(|n| *n == b".." || n.contains(&0))).then_some(names)
}

/// The names of `path` as [`split`] has them, but taken as they are where it has a `..` or a
/// zero byte: what a note of an entry left out is of ([`Select::wants`]).
pub fn lax(path: &[u8], backslash: bool) -> Vec<Vec<u8>> {
    let sep = |b: &u8| *b == b'/' || backslash && *b == b'\\';
    match split(path, backslash) {
        Some(names) => names.into_iter().map(<[u8]>::to_vec).collect(),
        None => path.split(sep).filter(|n| !n.is_empty() && *n != b".").map(<[u8]>::to_vec).collect(),
    }
}

/// Paths of an archive's entries to extract alone, each with what is under it (as names, by
/// `split`); none: everything.
#[derive(Debug, Clone, Default)]
pub struct Select {
    paths: Vec<Vec<Vec<u8>>>,
}

impl Select {
    /// The selection of `paths` (each of one name at least).
    pub fn new(paths: Vec<Vec<Vec<u8>>>) -> Select {
        Select { paths }
    }

    /// Whether the entry of `names` is selected, or under a path selected. A note of no names
    /// (of the archive, not of an entry) is wanted.
    pub fn wants<N: AsRef<[u8]>>(&self, names: &[N]) -> bool {
        names.is_empty()
            || self
                .paths
                .iter()
                .any(|p| p.len() <= names.len() && p.iter().zip(names).all(|(a, b)| a[..] == *b.as_ref()))
    }

    /// By entry of `t`, whether it is selected or under a path selected.
    pub fn entries(&self, t: &Tree) -> Vec<bool> {
        let mut paths: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
        let mut out = Vec::with_capacity(t.entries.len());
        for e in &t.entries {
            let mut p = e.parent.map_or_else(Vec::new, |q| paths[q].clone());
            p.push(name_bytes(&e.name).0.to_vec());
            out.push(e.parent.is_some_and(|q| out[q]) || self.wants(&p));
            paths.push(p);
        }
        out
    }

    /// By content of `t` (of `n` contents at least), whether a file or a fork of an entry
    /// selected is of it.
    pub fn contents(&self, t: &Tree, n: usize) -> Vec<bool> {
        let mut out = vec![false; n];
        for (e, _) in t.entries.iter().zip(self.entries(t)).filter(|x| x.1) {
            let files = match e.node {
                EntryNode::File(c) => Some(c),
                _ => None,
            };
            for c in files.into_iter().chain(e.extra.forks.iter().map(|f| f.1)) {
                if out.len() <= c {
                    out.resize(c + 1, false);
                }
                out[c] = true;
            }
        }
        out
    }
}

/// Whether content `c` is wanted by `wanted` (by content; all where `None`).
pub fn wanted(wanted: Option<&[bool]>, c: usize) -> bool {
    wanted.is_none_or(|w| w.get(c) == Some(&true))
}

/// What is told of an entry left out: its names ([`lax`]; none: of the archive) and what.
pub type Note = (Vec<Vec<u8>>, String);

/// `names` joined for people.
pub fn shown(names: &[&[u8]]) -> String {
    String::from_utf8_lossy(&names.join(&b'/')).into_owned()
}

#[derive(Default)]
pub struct Builder {
    pub tree: Tree,
    by_name: HashMap<(Option<usize>, Vec<u8>), usize>,
    /// The contents numbered so far.
    pub contents: usize,
    /// The contents of files given again since (of no file now: not to be told).
    dead: HashSet<usize>,
    /// Entries made or changed since they were last taken ([`Builder::take`]).
    new: Vec<usize>,
}

impl Builder {
    /// Whether content `c` is of a file of the tree (not of one given again since).
    pub fn live(&self, c: usize) -> bool {
        !self.dead.contains(&c)
    }

    /// A new content's number.
    pub fn content(&mut self) -> usize {
        self.contents += 1;
        self.contents - 1
    }

    /// The entry of `names` (`None`: the root), its directories made where the tree has none
    /// (and an entry of `node` and `extra` at the end where `node` is given). A directory given
    /// again takes the later one's metadata (where it has any), another node given again takes
    /// the place of the one before; a node in place of a directory or the other way round is an
    /// error, as is a path through a node not a directory. Errors tell what is left out.
    pub fn add(&mut self, names: &[&[u8]], node: Option<EntryNode>, extra: Extra) -> Result<Option<usize>, String> {
        let mut at = None;
        let dirs = if node.is_some() { names.len().saturating_sub(1) } else { names.len() };
        for (k, name) in names[..dirs].iter().enumerate() {
            let key = (at, name_from(name));
            let i = match self.by_name.get(&key) {
                Some(&i) if self.tree.entries[i].node == EntryNode::Dir => i,
                Some(_) => return Err(format!("{}: in {}, not a directory", shown(names), shown(&names[..=k]))),
                None => self.push(key, EntryNode::Dir, Extra::default()),
            };
            at = Some(i);
        }
        let Some(node) = node else {
            // a directory given (the root, where its path has no names)
            if !extra.is_empty() {
                match at {
                    Some(i) => {
                        self.tree.entries[i].extra = extra;
                        self.new.push(i);
                    }
                    None => self.tree.root = extra,
                }
            }
            return Ok(at);
        };
        let Some(name) = names.last() else {
            return Err("an entry of no name".into());
        };
        let key = (at, name_from(name));
        match self.by_name.get(&key) {
            None => Ok(Some(self.push(key, node, extra))),
            Some(&i) => {
                let e = &mut self.tree.entries[i];
                if (e.node == EntryNode::Dir) != (node == EntryNode::Dir) {
                    return Err(format!("{}: a directory and a file of one name, the later left out", shown(names)));
                }
                if let EntryNode::File(c) = e.node
                    && node != EntryNode::File(c)
                {
                    self.dead.insert(c);
                }
                if node != EntryNode::Dir || !extra.is_empty() {
                    (e.node, e.extra) = (node, extra);
                    self.new.push(i);
                }
                Ok(Some(i))
            }
        }
    }

    fn push(&mut self, key: (Option<usize>, Vec<u8>), node: EntryNode, extra: Extra) -> usize {
        let i = self.tree.entries.len();
        self.tree.entries.push(Entry { parent: key.0, name: key.1.clone(), node, extra });
        self.by_name.insert(key, i);
        self.new.push(i);
        i
    }

    /// Entry `i` changed (by the caller).
    pub fn touch(&mut self, i: usize) {
        self.new.push(i);
    }

    /// The entries made or changed since they were last taken, in order (each after its parent).
    pub fn take(&mut self) -> Vec<usize> {
        let mut new = std::mem::take(&mut self.new);
        new.sort_unstable();
        new.dedup();
        new
    }

    /// The entry of a hard link to `names`, a file given before: its content (`None`: none).
    pub fn file(&self, names: &[&[u8]]) -> Option<(usize, usize)> {
        let mut at = None;
        for name in names {
            at = Some(*self.by_name.get(&(at, name_from(name)))?);
        }
        let i = at?;
        match self.tree.entries[i].node {
            EntryNode::File(c) => Some((i, c)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split() {
        let s = |p: &str, b| {
            split(p.as_bytes(), b).map(|v| v.iter().map(|n| String::from_utf8_lossy(n)).collect::<Vec<_>>().join("|"))
        };
        assert_eq!(s("a/b/c", false).as_deref(), Some("a|b|c"));
        assert_eq!(s("/a//./b/", false).as_deref(), Some("a|b"));
        assert_eq!(s("a\\b", false).as_deref(), Some("a\\b"));
        assert_eq!(s("a\\b", true).as_deref(), Some("a|b"));
        assert_eq!(s("C:\\x\\y", true).as_deref(), Some("x|y"));
        assert_eq!(s("C:", true).as_deref(), Some(""));
        assert_eq!(s("Cx:/y", false).as_deref(), Some("Cx:|y"));
        assert_eq!(s("a/../b", false), None);
        assert_eq!(s("..", false), None);
        assert_eq!(s("a/..b", false).as_deref(), Some("a|..b"));
    }

    #[test]
    fn duplicates() {
        let mut b = Builder::default();
        let c0 = b.content();
        b.add(&[b"d", b"f"], Some(EntryNode::File(c0)), Extra::default()).unwrap();
        assert_eq!(b.take(), vec![0, 1]);
        let c1 = b.content();
        assert_eq!(b.add(&[b"d", b"f"], Some(EntryNode::File(c1)), Extra::default()).unwrap(), Some(1));
        assert_eq!(b.tree.entries[1].node, EntryNode::File(1));
        assert_eq!(b.take(), vec![1]);
        assert!(b.add(&[b"d", b"f", b"g"], Some(EntryNode::Dir), Extra::default()).is_err());
        assert!(b.add(&[b"d"], Some(EntryNode::File(c1)), Extra::default()).is_err());
        assert_eq!(b.add(&[b"d"], Some(EntryNode::Dir), Extra::default()).unwrap(), Some(0));
        assert_eq!(b.take(), Vec::<usize>::new());
        assert_eq!(b.file(&[b"d", b"f"]), Some((1, 1)));
    }
}
