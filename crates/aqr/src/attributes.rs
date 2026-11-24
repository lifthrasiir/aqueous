//! The attributes of the files compressed (docs/hints-design.md §2.1, [`hints`]): the files of
//! `--attributes` (relative to the root of each input, their values naming files relative to
//! their own directories), then the `.aqr/attributes` at the root of each input that is a
//! directory, then the `.gitattributes` of each directory under one, as they are among what was
//! gathered (an input of a single file has neither). The hints they give the contents
//! ([`aqr_compress::Hints`]) are by the path of each in the archive.

use super::*;
use std::sync::Arc;

use aqr_compress::Hints;
use aqr_compress::attributes::{Attributes, Origin};
use aqr_formula::Kind;

/// What [`hints`] made: the hints, the kind of each content, and the warnings of the files.
pub(crate) struct Hinted {
    pub(crate) hints: Arc<Hints>,
    pub(crate) kinds: Vec<Kind>,
    pub(crate) warnings: Vec<String>,
}

/// The path of each entry of `t` from its root, of `/` separators (each entry after the one
/// holding it, as they are gathered).
fn entry_paths(t: &Tree) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let mut p = match e.parent.and_then(|i| out.get(i)) {
            Some(parent) => [&parent[..], b"/"].concat(),
            None => Vec::new(),
        };
        p.extend_from_slice(&e.name);
        out.push(p);
    }
    out
}

/// The roots of `inputs` in the archive (paths of `/` separators, empty for the archive's), each
/// with whether it is a directory's (else a file's, the root its parent): a directory alone
/// and not renamed is at the archive's root.
fn roots(inputs: Inputs) -> Result<Vec<(Vec<u8>, bool)>, String> {
    if let Some(input) = inputs.sole()
        && input.is_dir()
    {
        return Ok(vec![(Vec::new(), true)]);
    }
    let mut out = Vec::new();
    for ((input, given), to) in inputs.paths.iter().zip(inputs.given).zip(inputs.renames) {
        let names: Vec<Vec<u8>> =
            archive_names(given, to.as_deref())?.iter().map(|n| name_from(&os_bytes(n))).collect();
        let dir = input.is_dir();
        let names = if dir { &names[..] } else { &names[..names.len() - 1] };
        let root = (names.join(&b'/'), dir);
        if !out.contains(&root) {
            out.push(root);
        }
    }
    Ok(out)
}

/// `name` in directory `base` of the archive.
fn under(base: &[u8], name: &[u8]) -> Vec<u8> {
    match base.is_empty() {
        true => name.to_vec(),
        false => [base, b"/", name].concat(),
    }
}

fn read(p: &Path) -> Result<String, String> {
    let d = fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(String::from_utf8_lossy(&d).into_owned())
}

/// The hints of the contents of `g`, gathered of `inputs` as `a` tells, where any file of
/// attributes is given or found.
pub(crate) fn hints(a: &Args, inputs: Inputs, g: &Gathered) -> Result<Option<Hinted>, String> {
    let paths = entry_paths(&g.tree);
    // (each content by the path of its first file)
    let mut path_of: Vec<Option<usize>> = vec![None; g.contents.len()];
    let mut by_path = std::collections::HashMap::new();
    for (i, e) in g.tree.entries.iter().enumerate() {
        if let EntryNode::File(c) = e.node {
            path_of[c].get_or_insert(i);
            by_path.insert(&paths[i][..], c);
        }
    }
    let roots = roots(inputs)?;
    let mut attrs = Attributes::new();
    let mut warnings = Vec::new();
    for f in &a.attributes {
        let text = read(f)?;
        let dir = f.parent().map(Path::to_owned);
        for (base, _) in &roots {
            warnings.extend(attrs.add(Origin::Given, base, &text, &f.display().to_string(), dir.clone(), true));
        }
    }
    for (base, _) in roots.iter().filter(|r| r.1) {
        let at = under(base, b".aqr/attributes");
        if let Some(&c) = by_path.get(&at[..]) {
            let disk = &g.contents[c].0;
            let dir = disk.parent().and_then(Path::parent).map(Path::to_owned);
            let name = String::from_utf8_lossy(&at).into_owned();
            warnings.extend(attrs.add(Origin::Aqr, base, &read(disk)?, &name, dir, true));
        }
    }
    let mut git: Vec<(&[u8], usize)> = by_path
        .iter()
        .filter(|(p, _)| p.rsplit(|&b| b == b'/').next() == Some(b".gitattributes"))
        .map(|(p, &c)| (&p[..p.len().saturating_sub(b"/.gitattributes".len())], c))
        .map(|(dir, c)| (if dir == b".gitattributes" { &b""[..] } else { dir }, c))
        .collect();
    git.sort();
    for (dir, c) in git {
        let within = |base: &[u8]| base.is_empty() || dir == base || dir.starts_with(&[base, b"/"].concat());
        let Some((base, _)) = roots.iter().filter(|r| r.1).find(|r| within(&r.0)) else { continue };
        let disk = &g.contents[c].0;
        let name = String::from_utf8_lossy(&under(dir, b".gitattributes")).into_owned();
        let macros = dir == &base[..];
        warnings.extend(attrs.add(Origin::Git, dir, &read(disk)?, &name, disk.parent().map(Path::to_owned), macros));
    }
    if attrs.is_empty() {
        return Ok(None);
    }
    let hints = Hints::new(attrs);
    let kinds = (0..g.contents.len())
        .map(|c| {
            let by_name = kind_by_name(&g.contents[c].1);
            match path_of[c] {
                Some(e) => hints.kind(&paths[e], by_name),
                None => by_name,
            }
        })
        .collect();
    Ok(Some(Hinted { hints, kinds, warnings }))
}

/// The hints of a single file archived alone, by its name: of `--attributes` alone.
pub(crate) fn hints_alone(a: &Args, name: &[u8], by_name: Kind) -> Result<Option<(Arc<Hints>, Kind)>, String> {
    if a.attributes.is_empty() {
        return Ok(None);
    }
    let mut attrs = Attributes::new();
    for f in &a.attributes {
        let w =
            attrs.add(Origin::Given, b"", &read(f)?, &f.display().to_string(), f.parent().map(Path::to_owned), true);
        if !a.quiet {
            w.iter().for_each(|w| eprintln!("{w}"));
        }
    }
    let hints = Hints::new(attrs);
    let kind = hints.kind(name, by_name);
    Ok(Some((hints, kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_entries() {
        let e =
            |parent, name: &str| Entry { parent, name: name.into(), node: EntryNode::Dir, extra: Default::default() };
        let t = Tree {
            root: Default::default(),
            entries: vec![e(None, "a"), e(Some(0), "b"), e(Some(1), "c"), e(None, "d")],
        };
        assert_eq!(entry_paths(&t), [&b"a"[..], b"a/b", b"a/b/c", b"d"]);
    }
}
