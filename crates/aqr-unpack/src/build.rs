//! A [`Tree`] built of the paths an archive gives its members: the directories on the way made
//! where the archive has none, a path given again taking the place of the one before (a later
//! member of a tar archive updating an earlier one), and what cannot be extracted safely left
//! out and told.

use std::collections::{HashMap, HashSet};

use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree, name_from};

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
